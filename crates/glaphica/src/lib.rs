pub mod engine_core;
pub mod engine_bridge;
pub mod sample_source;
pub mod phase4_test_utils;
pub mod app_core;
pub mod driver_bridge;
pub mod runtime;

use app_core::AppCore;
use app_core::AppCoreError;
use brush_execution::BrushExecutionMergeFeedback;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::{Arc, RwLock};
use std::time::Instant;

use document::{Document, DocumentMergeError, TileCoordinate};
use render_protocol::{
    BlendMode, BrushControlAck, BrushControlCommand, BrushRenderCommand, ImageHandle, ImageSource,
    ReceiptTerminalState, RenderOp, RenderStepSupportMatrix, RenderTreeSnapshot,
    StrokeExecutionReceiptId, Viewport,
};
use renderer::{
    BrushControlError, BrushRenderEnqueueError, FrameGpuTimingReport, MergeAckError,
    MergeCompletionNotice, MergeFinalizeError, MergePollError, MergeSubmitError,
    RenderDataResolver, Renderer, ViewOpSender,
};
use tiles::{
    BrushBufferTileRegistry, DirtySinceResult, GenericR32FloatTileAtlasStore,
    GenericTileAtlasConfig, MergeAuditRecord, MergePlanRequest, MergePlanTileOp, MergeTileStore,
    TileAddress, TileAtlasFormat, TileAtlasStore, TileAtlasUsage,
    TileImageApplyError, TileKey, TileMergeCompletionNoticeId, TileMergeEngine, TileMergeError,
    TilesBusinessResult, apply_tile_key_mappings,
};

use view::ViewTransform;
 // Used in tests
use winit::dpi::PhysicalSize;
use winit::window::Window;

const DEFAULT_DOCUMENT_WIDTH: u32 = 1280;
const DEFAULT_DOCUMENT_HEIGHT: u32 = 720;

struct DocumentRenderDataResolver {
    document: Arc<RwLock<Document>>,
    atlas_store: Arc<TileAtlasStore>,
    brush_buffer_store: Arc<GenericR32FloatTileAtlasStore>,
    brush_buffer_tile_keys: Arc<RwLock<BrushBufferTileRegistry>>,
}

impl RenderDataResolver for DocumentRenderDataResolver {
    fn document_size(&self) -> (u32, u32) {
        let document = self.document
            .read()
            .unwrap_or_else(|_| panic!("document read lock poisoned"));
        (document.size_x(), document.size_y())
    }

    fn visit_image_tiles(
        &self,
        image_handle: ImageHandle,
        visitor: &mut dyn FnMut(u32, u32, TileKey),
    ) {
        let document = self.document
            .read()
            .unwrap_or_else(|_| panic!("document read lock poisoned"));
        let Some(image) = document.image(image_handle) else {
            return;
        };

        #[cfg(debug_assertions)]
        let mut resolved_address_to_tile: HashMap<TileAddress, (TileKey, u32, u32)> =
            HashMap::new();
        #[cfg(debug_assertions)]
        let mut tile_coord_by_key: HashMap<TileKey, (u32, u32)> = HashMap::new();

        for (tile_x, tile_y, tile_key) in image.iter_tiles() {
            #[cfg(debug_assertions)]
            {
                if let Some((existing_tile_x, existing_tile_y)) =
                    tile_coord_by_key.get(&tile_key).copied()
                {
                    if (existing_tile_x, existing_tile_y) != (tile_x, tile_y) {
                        panic!(
                            "image uses duplicated tile key across coordinates: image_handle={:?} key={:?} first_tile=({}, {}) duplicate_tile=({}, {})",
                            image_handle,
                            tile_key,
                            existing_tile_x,
                            existing_tile_y,
                            tile_x,
                            tile_y
                        );
                    }
                } else {
                    tile_coord_by_key.insert(tile_key, (tile_x, tile_y));
                }
                let tile_address = self.atlas_store.resolve(tile_key).unwrap_or_else(|| {
                    panic!(
                        "image tile key unresolved in debug address uniqueness check: image_handle={:?} tile=({}, {}) key={:?}",
                        image_handle,
                        tile_x,
                        tile_y,
                        tile_key
                    )
                });
                if let Some((existing_key, existing_tile_x, existing_tile_y)) =
                    resolved_address_to_tile.get(&tile_address).copied()
                {
                    if existing_key != tile_key {
                        panic!(
                            "image tile keys resolved to duplicated atlas address: image_handle={:?} first_tile=({}, {}) first_key={:?} second_tile=({}, {}) second_key={:?} address={:?}",
                            image_handle,
                            existing_tile_x,
                            existing_tile_y,
                            existing_key,
                            tile_x,
                            tile_y,
                            tile_key,
                            tile_address
                        );
                    }
                } else {
                    resolved_address_to_tile.insert(tile_address, (tile_key, tile_x, tile_y));
                }
            }
            visitor(tile_x, tile_y, tile_key);
        }
    }

    fn visit_image_source_tiles(
        &self,
        image_source: ImageSource,
        visitor: &mut dyn FnMut(u32, u32, TileKey),
    ) {
        match image_source {
            ImageSource::LayerImage { image_handle } => {
                self.visit_image_tiles(image_handle, visitor)
            }
            ImageSource::BrushBuffer { stroke_session_id } => {
                let brush_buffer_tile_keys =
                    self.brush_buffer_tile_keys.read().unwrap_or_else(|_| {
                        panic!("brush buffer tile key registry read lock poisoned")
                    });
                #[cfg(debug_assertions)]
                let mut resolved_address_to_tile: HashMap<
                    TileAddress,
                    (TileKey, u32, u32),
                > = HashMap::new();
                #[cfg(debug_assertions)]
                let mut tile_coord_by_key: HashMap<TileKey, (u32, u32)> = HashMap::new();

                brush_buffer_tile_keys.visit_tiles(stroke_session_id, |tile_coordinate, tile_key| {
                    if tile_coordinate.tile_x < 0 || tile_coordinate.tile_y < 0 {
                        return;
                    }
                    let tile_x = u32::try_from(tile_coordinate.tile_x)
                        .expect("positive brush tile x must convert to u32");
                    let tile_y = u32::try_from(tile_coordinate.tile_y)
                        .expect("positive brush tile y must convert to u32");
                    #[cfg(debug_assertions)]
                    {
                        if let Some((existing_tile_x, existing_tile_y)) =
                            tile_coord_by_key.get(&tile_key).copied()
                        {
                            if (existing_tile_x, existing_tile_y) != (tile_x, tile_y) {
                                panic!(
                                    "brush buffer uses duplicated tile key across coordinates: stroke_session_id={} key={:?} first_tile=({}, {}) duplicate_tile=({}, {})",
                                    stroke_session_id,
                                    tile_key,
                                    existing_tile_x,
                                    existing_tile_y,
                                    tile_x,
                                    tile_y
                                );
                            }
                        } else {
                            tile_coord_by_key.insert(tile_key, (tile_x, tile_y));
                        }
                        let tile_address = self.brush_buffer_store.resolve(tile_key).unwrap_or_else(|| {
                            panic!(
                                "brush buffer tile key unresolved in debug address uniqueness check: stroke_session_id={} tile=({}, {}) key={:?}",
                                stroke_session_id,
                                tile_x,
                                tile_y,
                                tile_key
                            )
                        });
                        if let Some((existing_key, existing_tile_x, existing_tile_y)) =
                            resolved_address_to_tile.get(&tile_address).copied()
                        {
                            if existing_key != tile_key {
                                panic!(
                                    "brush buffer tile keys resolved to duplicated atlas address: stroke_session_id={} first_tile=({}, {}) first_key={:?} second_tile=({}, {}) second_key={:?} address={:?}",
                                    stroke_session_id,
                                    existing_tile_x,
                                    existing_tile_y,
                                    existing_key,
                                    tile_x,
                                    tile_y,
                                    tile_key,
                                    tile_address
                                );
                            }
                        } else {
                            resolved_address_to_tile.insert(tile_address, (tile_key, tile_x, tile_y));
                        }
                    }
                    visitor(tile_x, tile_y, tile_key);
                });
            }
        }
    }

    fn visit_image_tiles_for_coords(
        &self,
        image_handle: ImageHandle,
        tile_coords: &[(u32, u32)],
        visitor: &mut dyn FnMut(u32, u32, TileKey),
    ) {
        let document = self.document
            .read()
            .unwrap_or_else(|_| panic!("document read lock poisoned"));
        let Some(image) = document.image(image_handle) else {
            return;
        };

        #[cfg(debug_assertions)]
        let mut resolved_address_to_tile: HashMap<TileAddress, (TileKey, u32, u32)> =
            HashMap::new();
        #[cfg(debug_assertions)]
        let mut tile_coord_by_key: HashMap<TileKey, (u32, u32)> = HashMap::new();

        for (tile_x, tile_y) in tile_coords {
            let tile_key = image
                .get_tile(*tile_x, *tile_y)
                .unwrap_or_else(|error| panic!("tile coordinate lookup failed: {error:?}"));
            let Some(tile_key) = tile_key else {
                continue;
            };
            #[cfg(debug_assertions)]
            {
                if let Some((existing_tile_x, existing_tile_y)) =
                    tile_coord_by_key.get(&tile_key).copied()
                {
                    if (existing_tile_x, existing_tile_y) != (*tile_x, *tile_y) {
                        panic!(
                            "image uses duplicated tile key across coordinates for coords query: image_handle={:?} key={:?} first_tile=({}, {}) duplicate_tile=({}, {})",
                            image_handle,
                            tile_key,
                            existing_tile_x,
                            existing_tile_y,
                            tile_x,
                            tile_y
                        );
                    }
                } else {
                    tile_coord_by_key.insert(tile_key, (*tile_x, *tile_y));
                }
                let tile_address = self.atlas_store.resolve(tile_key).unwrap_or_else(|| {
                    panic!(
                        "image tile key unresolved in debug address uniqueness check for coords: image_handle={:?} tile=({}, {}) key={:?}",
                        image_handle,
                        tile_x,
                        tile_y,
                        tile_key
                    )
                });
                if let Some((existing_key, existing_tile_x, existing_tile_y)) =
                    resolved_address_to_tile.get(&tile_address).copied()
                {
                    if existing_key != tile_key {
                        panic!(
                            "image tile keys resolved to duplicated atlas address for coords: image_handle={:?} first_tile=({}, {}) first_key={:?} second_tile=({}, {}) second_key={:?} address={:?}",
                            image_handle,
                            existing_tile_x,
                            existing_tile_y,
                            existing_key,
                            tile_x,
                            tile_y,
                            tile_key,
                            tile_address
                        );
                    }
                } else {
                    resolved_address_to_tile.insert(tile_address, (tile_key, *tile_x, *tile_y));
                }
            }
            visitor(*tile_x, *tile_y, tile_key);
        }
    }

    fn resolve_tile_address(&self, tile_key: TileKey) -> Option<TileAddress> {
        self.atlas_store.resolve(tile_key)
    }

    fn resolve_image_source_tile_address(
        &self,
        image_source: ImageSource,
        tile_key: TileKey,
    ) -> Option<TileAddress> {
        match image_source {
            ImageSource::LayerImage { .. } => self.atlas_store.resolve(tile_key),
            ImageSource::BrushBuffer { .. } => self.brush_buffer_store.resolve(tile_key),
        }
    }

    fn layer_dirty_since(&self, layer_id: u64, since_version: u64) -> Option<DirtySinceResult> {
        let document = self.document
            .read()
            .unwrap_or_else(|_| panic!("document read lock poisoned"));
        document.layer_dirty_since(layer_id, since_version)
    }

    fn layer_version(&self, layer_id: u64) -> Option<u64> {
        let document = self.document
            .read()
            .unwrap_or_else(|_| panic!("document read lock poisoned"));
        document.layer_version(layer_id)
    }
}

/// GpuState - main GPU state holder.
///
/// Phase 2.5: Delegates all business logic to AppCore.
/// GpuState is now a thin facade over AppCore.
pub struct GpuState {
    core: AppCore,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuSemanticStateDigest {
    pub document_revision: u64,
    pub render_tree_revision: u64,
    pub render_tree_semantic_hash: u64,
    pub pending_brush_command_count: u64,
    pub has_pending_merge_work: bool,
}

#[derive(Debug)]
struct StrokeTileMergePlan {
    layer_id: u64,
    tile_ops: Vec<MergePlanTileOp>,
}

#[derive(Debug)]
pub enum MergeBridgeError {
    RendererPoll(MergePollError),
    RendererAck(MergeAckError),
    RendererSubmit(MergeSubmitError),
    RendererFinalize(MergeFinalizeError),
    Tiles(TileMergeError),
    Document(DocumentMergeError),
    TileImageApply(TileImageApplyError),
    MissingRendererNotice {
        receipt_id: StrokeExecutionReceiptId,
        notice_id: TileMergeCompletionNoticeId,
    },
}

impl GpuState {
    fn required_device_features() -> wgpu::Features {
        wgpu::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES
    }

    fn perf_log_enabled() -> bool {
        static ENABLED: OnceLock<bool> = OnceLock::new();
        *ENABLED
            .get_or_init(|| std::env::var_os("GLAPHICA_PERF_LOG").is_some_and(|value| value != "0"))
    }

    fn brush_trace_enabled() -> bool {
        static ENABLED: OnceLock<bool> = OnceLock::new();
        *ENABLED.get_or_init(|| {
            std::env::var_os("GLAPHICA_BRUSH_TRACE").is_some_and(|value| value != "0")
        })
    }

    fn render_tree_trace_enabled() -> bool {
        static ENABLED: OnceLock<bool> = OnceLock::new();
        *ENABLED.get_or_init(|| {
            std::env::var_os("GLAPHICA_RENDER_TREE_TRACE").is_some_and(|value| value != "0")
        })
    }

    fn render_tree_invariants_enabled() -> bool {
        static ENABLED: OnceLock<bool> = OnceLock::new();
        *ENABLED.get_or_init(|| {
            std::env::var_os("GLAPHICA_RENDER_TREE_INVARIANTS").is_some_and(|value| value != "0")
        })
    }

    fn render_node_semantic_hash(node: &render_protocol::RenderNodeSnapshot) -> u64 {
        use std::hash::{Hash, Hasher};

        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        fn hash_node<H: Hasher>(node: &render_protocol::RenderNodeSnapshot, state: &mut H) {
            match node {
                render_protocol::RenderNodeSnapshot::Leaf {
                    layer_id,
                    blend,
                    image_source,
                } => {
                    0u8.hash(state);
                    layer_id.hash(state);
                    blend.hash(state);
                    match image_source {
                        render_protocol::ImageSource::LayerImage { .. } => {
                            0u8.hash(state);
                        }
                        render_protocol::ImageSource::BrushBuffer { stroke_session_id } => {
                            1u8.hash(state);
                            stroke_session_id.hash(state);
                        }
                    }
                }
                render_protocol::RenderNodeSnapshot::Group {
                    group_id,
                    blend,
                    children,
                } => {
                    1u8.hash(state);
                    group_id.hash(state);
                    blend.hash(state);
                    children.len().hash(state);
                    for child in children.iter() {
                        hash_node(child, state);
                    }
                }
            }
        }
        hash_node(node, &mut hasher);
        hasher.finish()
    }

    #[cfg(debug_assertions)]
    fn check_render_tree_semantics_invariants(
        reason: &'static str,
        last_bound: Option<(u64, u64)>,
        snapshot: &RenderTreeSnapshot,
        trace_enabled: bool,
        invariants_enabled: bool,
    ) -> (u64, u64) {
        let revision = snapshot.revision;
        let hash = Self::render_node_semantic_hash(snapshot.root.as_ref());
        if trace_enabled {
            eprintln!(
                "[render_tree] bind reason={} revision={} semantic_hash={:016x}",
                reason, revision, hash
            );
        }
        if invariants_enabled {
            if let Some((last_revision, last_hash)) = last_bound {
                if last_revision == revision && last_hash != hash {
                    panic!(
                        "render tree semantics changed without revision bump: reason={} revision={} last_hash={:016x} new_hash={:016x}",
                        reason, revision, last_hash, hash
                    );
                }
            }
        }
        (revision, hash)
    }

    fn note_bound_render_tree(&mut self, reason: &'static str, snapshot: &RenderTreeSnapshot) {
        #[cfg(debug_assertions)]
        {
            let trace_enabled = Self::render_tree_trace_enabled();
            let invariants_enabled = Self::render_tree_invariants_enabled();
            let next = Self::check_render_tree_semantics_invariants(
                reason,
                self.core.last_bound_render_tree(),
                snapshot,
                trace_enabled,
                invariants_enabled,
            );
            self.core.set_last_bound_render_tree(Some(next));
        }
        #[cfg(not(debug_assertions))]
        {
            let _ = reason;
            let _ = snapshot;
        }
    }

    fn build_stroke_tile_merge_plan(
        &self,
        stroke_session_id: u64,
        layer_id: u64,
    ) -> Option<StrokeTileMergePlan> {
        let document = self.core.document()
            .read()
            .unwrap_or_else(|_| panic!("document read lock poisoned"));
        let layer_image_handle = document
            .leaf_image_handle(layer_id, stroke_session_id)
            .unwrap_or_else(|error| {
                panic!(
                    "resolve leaf image handle for merge plan failed: layer_id={} stroke_session_id={} error={error:?}",
                    layer_id, stroke_session_id
                )
            });
        let layer_image = document.image(layer_image_handle).unwrap_or_else(|| {
            panic!(
                "layer image handle missing while building merge plan: layer_id={} stroke_session_id={} image_handle={:?}",
                layer_id, stroke_session_id, layer_image_handle
            )
        });
        let layer_tiles_per_row = layer_image.tiles_per_row();
        let layer_tiles_per_column = layer_image.tiles_per_column();
        let mut tile_ops = Vec::new();
        let mut op_trace_id = 0u64;
        let mut seen_output_tiles = HashSet::new();
        let mut stroke_tile_by_key = HashMap::new();
        let brush_buffer_tile_keys = self.core.brush_buffer_tile_keys()
            .read()
            .unwrap_or_else(|_| panic!("brush buffer tile key registry read lock poisoned"));
        brush_buffer_tile_keys.visit_tiles(stroke_session_id, |tile_coordinate, stroke_buffer_key| {
                if tile_coordinate.tile_x < 0 || tile_coordinate.tile_y < 0 {
                    return;
                }
                let tile_x = u32::try_from(tile_coordinate.tile_x)
                    .expect("positive brush tile x must convert to u32");
                let tile_y = u32::try_from(tile_coordinate.tile_y)
                    .expect("positive brush tile y must convert to u32");
                if tile_x >= layer_tiles_per_row || tile_y >= layer_tiles_per_column {
                    return;
                }
                if !seen_output_tiles.insert((tile_x, tile_y)) {
                    panic!(
                        "duplicate output tile in stroke merge plan: stroke_session_id={} layer_id={} tile=({}, {})",
                        stroke_session_id, layer_id, tile_x, tile_y
                    );
                }
                if let Some((previous_tile_x, previous_tile_y)) =
                    stroke_tile_by_key.insert(stroke_buffer_key, (tile_x, tile_y))
                {
                    if (previous_tile_x, previous_tile_y) != (tile_x, tile_y) {
                        panic!(
                            "duplicate stroke buffer key in merge plan: stroke_session_id={} layer_id={} key={:?} first_tile=({}, {}) duplicate_tile=({}, {})",
                            stroke_session_id,
                            layer_id,
                            stroke_buffer_key,
                            previous_tile_x,
                            previous_tile_y,
                            tile_x,
                            tile_y
                        );
                    }
                }
                let existing_layer_key = document.leaf_tile_key_at(layer_id, tile_x, tile_y);
                tile_ops.push(MergePlanTileOp {
                    tile_x,
                    tile_y,
                    existing_layer_key,
                    stroke_buffer_key,
                    blend_mode: BlendMode::Normal,
                    opacity: 1.0,
                    op_trace_id,
                });
                op_trace_id = op_trace_id
                    .checked_add(1)
                    .expect("merge op index exceeds u64");
            });
        if tile_ops.is_empty() {
            return None;
        }
        Some(StrokeTileMergePlan { layer_id, tile_ops })
    }

    fn build_merge_plan_request_from_plan(
        &self,
        stroke_session_id: u64,
        tx_token: u64,
        merge_plan: StrokeTileMergePlan,
    ) -> MergePlanRequest {
        MergePlanRequest {
            stroke_session_id,
            tx_token,
            program_revision: None,
            layer_id: merge_plan.layer_id,
            tile_ops: merge_plan.tile_ops,
        }
    }

    fn enqueue_stroke_merge_submission(
        &mut self,
        stroke_session_id: u64,
        tx_token: u64,
        layer_id: u64,
    ) {
        let Some(merge_plan) = self.build_stroke_tile_merge_plan(stroke_session_id, layer_id)
        else {
            self.core.brush_buffer_tile_keys()
                .write()
                .unwrap_or_else(|_| panic!("brush buffer tile key registry write lock poisoned"))
                .release_stroke_on_merge_failed(stroke_session_id, self.core.brush_buffer_store().as_ref());
            self.clear_preview_buffer_and_rebind(stroke_session_id);
            self.core.brush_execution_feedback_queue_mut()
                .push_back(BrushExecutionMergeFeedback::MergeApplied { stroke_session_id });
            return;
        };
        let request =
            self.build_merge_plan_request_from_plan(stroke_session_id, tx_token, merge_plan);
        let submission = self.core.tile_merge_engine_mut()
            .submit_merge_plan(request)
            .unwrap_or_else(|error| panic!("submit merge plan failed: {error:?}"));
        self.core.runtime_mut().renderer_mut()
            .enqueue_planned_merge(
                submission.renderer_submit_payload.receipt,
                submission.renderer_submit_payload.gpu_merge_ops,
                submission.renderer_submit_payload.meta,
            )
            .unwrap_or_else(|error| panic!("enqueue planned merge failed: {error:?}"));
    }

    fn set_preview_buffer_and_rebind(&mut self, layer_id: u64, stroke_session_id: u64) {
        let render_tree = {
            let mut document = self.core.document()
                .write()
                .unwrap_or_else(|_| panic!("document write lock poisoned"));
            document
                .set_active_preview_buffer(layer_id, stroke_session_id)
                .unwrap_or_else(|error| {
                    panic!(
                        "set active preview buffer failed: layer_id={} stroke_session_id={} error={error:?}",
                        layer_id, stroke_session_id
                    )
                });
            if !document.take_render_tree_cache_dirty() {
                return;
            }
            document.render_tree_snapshot()
        };
        self.note_bound_render_tree("preview_set", &render_tree);
        self.core.runtime().view_sender()
            .send(RenderOp::BindRenderTree(render_tree))
            .expect("send updated render tree after preview set");
    }

    fn clear_preview_buffer_and_rebind(&mut self, stroke_session_id: u64) {
        let render_tree = {
            let mut document = self.core.document()
                .write()
                .unwrap_or_else(|_| panic!("document write lock poisoned"));
            let _ = document.clear_active_preview_buffer(stroke_session_id);
            if !document.take_render_tree_cache_dirty() {
                return;
            }
            document.render_tree_snapshot()
        };
        self.note_bound_render_tree("preview_clear", &render_tree);
        self.core.runtime().view_sender()
            .send(RenderOp::BindRenderTree(render_tree))
            .expect("send updated render tree after preview clear");
    }

    pub async fn new(window: Arc<Window>, startup_image_path: Option<PathBuf>) -> Self {
        eprintln!(
            "[startup] begin app init: startup_image_path={}",
            startup_image_path
                .as_deref()
                .map_or("<none>".to_string(), |path| path.display().to_string())
        );
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..Default::default()
        });

        let surface = instance
            .create_surface(window.clone())
            .expect("create wgpu surface");

        let required_features = Self::required_device_features();
        let brush_trace_enabled = Self::brush_trace_enabled();
        let mut adapter = None;
        let mut adapter_rejection_reasons = Vec::new();
        for candidate in instance.enumerate_adapters(wgpu::Backends::all()).await {
            let adapter_info = candidate.get_info();
            if !candidate.is_surface_supported(&surface) {
                if brush_trace_enabled {
                    adapter_rejection_reasons.push(format!(
                        "{} ({:?}): surface not supported",
                        adapter_info.name, adapter_info.backend
                    ));
                }
                continue;
            }
            if !candidate.features().contains(required_features) {
                if brush_trace_enabled {
                    adapter_rejection_reasons.push(format!(
                        "{} ({:?}): missing required features {:?}",
                        adapter_info.name, adapter_info.backend, required_features
                    ));
                }
                continue;
            }
            let r32float_format_features =
                candidate.get_texture_format_features(wgpu::TextureFormat::R32Float);
            let has_storage_binding = r32float_format_features
                .allowed_usages
                .contains(wgpu::TextureUsages::STORAGE_BINDING);
            let has_storage_write_only = r32float_format_features
                .flags
                .contains(wgpu::TextureFormatFeatureFlags::STORAGE_WRITE_ONLY);
            if !has_storage_binding || !has_storage_write_only {
                if brush_trace_enabled {
                    adapter_rejection_reasons.push(format!(
                        "{} ({:?}): R32Float storage write unsupported: has_storage_binding={} has_storage_write_only={} allowed_usages={:?} flags={:?}",
                        adapter_info.name,
                        adapter_info.backend,
                        has_storage_binding,
                        has_storage_write_only,
                        r32float_format_features.allowed_usages,
                        r32float_format_features.flags
                    ));
                }
                continue;
            }
            adapter = Some(candidate);
            break;
        }
        if brush_trace_enabled && !adapter_rejection_reasons.is_empty() {
            eprintln!(
                "[brush_trace] adapter_rejections_for_r32float_storage:\n{}",
                adapter_rejection_reasons.join("\n")
            );
        }
        let adapter = adapter.expect(
            "no compatible adapter supports R32Float storage binding + STORAGE_WRITE_ONLY for brush execution",
        );
        if brush_trace_enabled {
            let r32float_features =
                adapter.get_texture_format_features(wgpu::TextureFormat::R32Float);
            let adapter_info = adapter.get_info();
            eprintln!(
                "[brush_trace] selected_adapter={} backend={:?} r32float.allowed_usages={:?} r32float.flags={:?}",
                adapter_info.name,
                adapter_info.backend,
                r32float_features.allowed_usages,
                r32float_features.flags
            );
        }

        let limits = adapter.limits();
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: None,
                required_features,
                required_limits: limits,
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
                memory_hints: wgpu::MemoryHints::Performance,
                trace: wgpu::Trace::Off,
            })
            .await
            .expect("request wgpu device");

        let caps = surface.get_capabilities(&adapter);
        let surface_format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(caps.formats[0]);
        eprintln!(
            "[startup] surface capabilities: selected_format={:?} present_modes={:?} alpha_modes={:?}",
            surface_format, caps.present_modes, caps.alpha_modes
        );

        let mut size = window.inner_size();
        size.width = size.width.max(1);
        size.height = size.height.max(1);
        eprintln!(
            "[startup] window size for surface config: {}x{}",
            size.width, size.height
        );

        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: surface_format,
            width: size.width,
            height: size.height,
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };

        let atlas_format = surface_format_to_default_atlas_format(surface_format);
        eprintln!(
            "[startup] selected default atlas format from surface: surface={:?} atlas={:?}",
            surface_format, atlas_format
        );
        let (atlas_store, tile_atlas) = Renderer::create_default_tile_atlas(&device, atlas_format)
            .expect("create tile atlas store");
        eprintln!(
            "[startup] tile atlas created: format={:?} layout={:?}",
            tile_atlas.format(),
            tile_atlas.layout()
        );

        let document = create_startup_document(&atlas_store, startup_image_path.as_deref());
        eprintln!(
            "[startup] document created: size={}x{} revision={}",
            document.size_x(),
            document.size_y(),
            document.revision()
        );
        let initial_snapshot = document.render_tree_snapshot();
        initial_snapshot
            .validate_executable(&RenderStepSupportMatrix::current_executable_semantics())
            .unwrap_or_else(|error| {
                panic!(
                    "initial render steps include unsupported feature at step {}: {:?}",
                    error.step_index, error.reason
                )
            });
        let document = Arc::new(RwLock::new(document));

        let (brush_buffer_store_raw, brush_buffer_atlas) =
            GenericR32FloatTileAtlasStore::with_config(
                &device,
                GenericTileAtlasConfig {
                    max_layers: GenericTileAtlasConfig::default().max_layers,
                    tiles_per_row: GenericTileAtlasConfig::default().tiles_per_row,
                    tiles_per_column: GenericTileAtlasConfig::default().tiles_per_column,
                    usage: TileAtlasUsage::TEXTURE_BINDING
                        | TileAtlasUsage::STORAGE_BINDING
                        | TileAtlasUsage::COPY_DST
                        | TileAtlasUsage::COPY_SRC,
                },
            )
            .expect("create brush buffer atlas store");
        let brush_buffer_store = Arc::new(brush_buffer_store_raw);
        let brush_buffer_tile_keys = Arc::new(RwLock::new(BrushBufferTileRegistry::default()));

        let render_data_resolver = Box::new(DocumentRenderDataResolver {
            document: Arc::clone(&document),
            atlas_store: Arc::clone(&atlas_store),
            brush_buffer_store: Arc::clone(&brush_buffer_store),
            brush_buffer_tile_keys: Arc::clone(&brush_buffer_tile_keys),
        });

        let tile_merge_engine = TileMergeEngine::new(crate::app_core::MergeStores {
            layer_store: Arc::clone(&atlas_store),
            stroke_store: Arc::clone(&brush_buffer_store),
        });

        let (renderer, view_sender) = Renderer::new(
            device,
            queue,
            surface,
            config,
            tile_atlas,
            Arc::clone(&brush_buffer_store),
            brush_buffer_atlas,
            render_data_resolver,
        );
        eprintln!("[startup] renderer initialized");
        let disable_merge_for_debug =
            std::env::var_os("GLAPHICA_DISABLE_MERGE").is_some_and(|value| value != "0");
        if disable_merge_for_debug {
            eprintln!("[startup] GLAPHICA_DISABLE_MERGE enabled: skipping merge submission");
        }
        let perf_log_enabled = Self::perf_log_enabled();
        if perf_log_enabled {
            eprintln!("[startup] GLAPHICA_PERF_LOG enabled: verbose merge/render perf logs");
        }

        let view_transform = ViewTransform::default();
        push_view_state(&view_sender, &view_transform, size);
        eprintln!("[startup] initial viewport and view transform pushed");
        let initial_snapshot_for_trace = initial_snapshot.clone();
        view_sender
            .send(RenderOp::BindRenderTree(initial_snapshot))
            .expect("send initial render steps");
        
        // Create GpuRuntime with GPU resources
        let runtime = crate::runtime::GpuRuntime::new(
            renderer,
            view_sender,
            atlas_store,
            brush_buffer_store,
            size,
            0, // next_frame_id
        );
        
        // Create AppCore with runtime and business components
        let core = AppCore::new(
            document,
            tile_merge_engine,
            brush_buffer_tile_keys,
            view_transform,
            runtime,
            disable_merge_for_debug,
            perf_log_enabled,
            0, // next_frame_id
        );
        
        let mut state = Self {
            core,
        };
        state.note_bound_render_tree("startup", &initial_snapshot_for_trace);
        eprintln!("[startup] initial render tree bound");

        state
    }

    /// Resize the surface.
    ///
    /// Phase 2.5-B: Now returns Result for error propagation.
    pub fn resize(&mut self, new_size: PhysicalSize<u32>) -> Result<(), AppCoreError> {
        self.core.resize(new_size)
    }

    /// Render a frame.
    ///
    /// Phase 2.5-B: Now delegates to AppCore with unified error handling.
    pub fn render(&mut self) -> Result<(), AppCoreError> {
        self.core.render()
    }

    pub fn set_brush_command_quota(&self, max_commands: u32) {
        self.core.runtime().view_sender()
            .send(RenderOp::SetBrushCommandQuota { max_commands })
            .expect("send brush command quota");
    }

    pub fn take_latest_gpu_timing_report(&mut self) -> Option<FrameGpuTimingReport> {
        self.core.runtime_mut().renderer_mut().take_latest_gpu_timing_report()
    }

    pub fn apply_brush_control_command(
        &mut self,
        command: BrushControlCommand,
    ) -> Result<BrushControlAck, BrushControlError> {
        self.core.runtime_mut().renderer_mut().apply_brush_control_command(command)
    }

    pub fn enqueue_brush_render_command(
        &mut self,
        command: BrushRenderCommand,
    ) -> Result<(), BrushRenderEnqueueError> {
        match command {
            BrushRenderCommand::BeginStroke(begin) => {
                self.core.runtime_mut().renderer_mut()
                    .enqueue_brush_render_command(BrushRenderCommand::BeginStroke(begin))?;
                self.set_preview_buffer_and_rebind(begin.target_layer_id, begin.stroke_session_id);
                Ok(())
            }
            BrushRenderCommand::AllocateBufferTiles(allocate) => {
                self.core.brush_buffer_tile_keys()
                    .write()
                    .unwrap_or_else(|_| {
                        panic!("brush buffer tile key registry write lock poisoned")
                    })
                    .allocate_tiles(
                        allocate.stroke_session_id,
                        allocate.tiles.clone(),
                        self.core.brush_buffer_store().as_ref(),
                    )
                    .unwrap_or_else(|error| {
                        panic!(
                            "failed to allocate brush buffer tiles for stroke {}: {error}",
                            allocate.stroke_session_id
                        )
                    });
                let tile_bindings = self.core.brush_buffer_tile_keys()
                    .read()
                    .unwrap_or_else(|_| panic!("brush buffer tile key registry read lock poisoned"))
                    .tile_bindings_for_stroke(allocate.stroke_session_id);
                self.core.runtime_mut().renderer_mut()
                    .bind_brush_buffer_tiles(allocate.stroke_session_id, tile_bindings);
                self.drain_tile_gc_evictions();
                self.core.runtime_mut().renderer_mut()
                    .enqueue_brush_render_command(BrushRenderCommand::AllocateBufferTiles(allocate))
            }
            BrushRenderCommand::MergeBuffer(merge) => {
                if self.core.disable_merge_for_debug() {
                    self.core.brush_buffer_tile_keys()
                        .write()
                        .unwrap_or_else(|_| {
                            panic!("brush buffer tile key registry write lock poisoned")
                        })
                        .release_stroke_on_merge_failed(
                            merge.stroke_session_id,
                            self.core.brush_buffer_store().as_ref(),
                        );
                    self.clear_preview_buffer_and_rebind(merge.stroke_session_id);
                    self.core.brush_execution_feedback_queue_mut().push_back(
                        BrushExecutionMergeFeedback::MergeApplied {
                            stroke_session_id: merge.stroke_session_id,
                        },
                    );
                } else {
                    self.enqueue_stroke_merge_submission(
                        merge.stroke_session_id,
                        merge.tx_token,
                        merge.target_layer_id,
                    );
                }
                self.core.runtime_mut().renderer_mut()
                    .enqueue_brush_render_command(BrushRenderCommand::MergeBuffer(merge))
            }
            other => self.core.runtime_mut().renderer_mut().enqueue_brush_render_command(other),
        }
    }

    pub fn pending_brush_dab_count(&self) -> u64 {
        self.core.runtime().renderer().pending_brush_dab_count()
    }

    pub fn pending_brush_command_count(&self) -> u64 {
        self.core.runtime().renderer().pending_brush_command_count()
    }

    pub fn semantic_state_digest(&self) -> GpuSemanticStateDigest {
        let (document_revision, render_tree_revision, render_tree_semantic_hash) = {
            let document = self.core.document()
                .read()
                .unwrap_or_else(|_| panic!("document read lock poisoned"));
            let document_revision = document.revision();
            let snapshot = document.render_tree_snapshot();
            let render_tree_revision = snapshot.revision;
            let render_tree_semantic_hash = Self::render_node_semantic_hash(snapshot.root.as_ref());
            (
                document_revision,
                render_tree_revision,
                render_tree_semantic_hash,
            )
        };
        GpuSemanticStateDigest {
            document_revision,
            render_tree_revision,
            render_tree_semantic_hash,
            pending_brush_command_count: self.core.runtime().renderer().pending_brush_command_count(),
            has_pending_merge_work: self.core.tile_merge_engine().has_pending_work(),
        }
    }

    pub fn has_pending_merge_work(&self) -> bool {
        self.core.tile_merge_engine().has_pending_work()
    }

    pub fn process_renderer_merge_completions(
        &mut self,
        frame_id: u64,
    ) -> Result<(), MergeBridgeError> {
        let perf_started = Instant::now();
        let submission_report = self.core.runtime_mut().renderer_mut()
            .submit_pending_merges(frame_id, u32::MAX)
            .map_err(MergeBridgeError::RendererSubmit)?;
        let renderer_notices = self.core.runtime_mut().renderer_mut()
            .poll_completion_notices(frame_id)
            .map_err(MergeBridgeError::RendererPoll)?;
        if self.core.perf_log_enabled() {
            eprintln!(
                "[merge_bridge_perf] frame_id={} submitted_receipts={} renderer_submission_id={:?} renderer_notices={}",
                frame_id,
                submission_report.receipt_ids.len(),
                submission_report.renderer_submission_id.map(|id| id.0),
                renderer_notices.len(),
            );
        }

        let mut renderer_notice_by_key = HashMap::new();
        for renderer_notice in renderer_notices {
            let notice_id = notice_id_from_renderer(&renderer_notice);
            let notice_key = (notice_id, renderer_notice.receipt_id);
            self.core.tile_merge_engine_mut()
                .on_renderer_completion_signal(
                    renderer_notice.receipt_id,
                    renderer_notice.audit_meta,
                    renderer_notice.result.clone(),
                )
                .map_err(MergeBridgeError::Tiles)?;
            let previous = renderer_notice_by_key.insert(notice_key, renderer_notice);
            assert!(
                previous.is_none(),
                "renderer poll yielded duplicate merge notice key"
            );
        }

        let completion_notices = self.core.tile_merge_engine_mut().poll_submission_results();
        if self.core.perf_log_enabled() && !completion_notices.is_empty() {
            eprintln!(
                "[merge_bridge_perf] frame_id={} tile_engine_completion_notices={}",
                frame_id,
                completion_notices.len(),
            );
        }
        for notice in completion_notices {
            let notice_key = (notice.notice_id, notice.receipt_id);
            let renderer_notice = renderer_notice_by_key.remove(&notice_key).ok_or(
                MergeBridgeError::MissingRendererNotice {
                    receipt_id: notice.receipt_id,
                    notice_id: notice.notice_id,
                },
            )?;

            self.core.runtime_mut().renderer_mut()
                .ack_merge_result(renderer_notice)
                .map_err(MergeBridgeError::RendererAck)?;
            self.core.tile_merge_engine_mut()
                .ack_merge_result(notice.receipt_id, notice.notice_id)
                .map_err(MergeBridgeError::Tiles)?;
        }

        let business_results = self.core.tile_merge_engine_mut().drain_business_results();
        if self.core.perf_log_enabled() && !business_results.is_empty() {
            let finalize_count = business_results
                .iter()
                .filter(|result| matches!(result, TilesBusinessResult::CanFinalize { .. }))
                .count();
            let abort_count = business_results.len().saturating_sub(finalize_count);
            let total_dirty_tiles: usize = business_results
                .iter()
                .map(|result| match result {
                    TilesBusinessResult::CanFinalize { dirty_tiles, .. } => dirty_tiles.len(),
                    TilesBusinessResult::RequiresAbort { .. } => 0,
                })
                .sum();
            eprintln!(
                "[merge_bridge_perf] frame_id={} business_results={} finalize={} abort={} dirty_tiles={}",
                frame_id,
                business_results.len(),
                finalize_count,
                abort_count,
                total_dirty_tiles,
            );
        }
        self.apply_tiles_business_results(&business_results)?;
        self.drain_tile_gc_evictions();
        if self.core.perf_log_enabled() {
            eprintln!(
                "[merge_bridge_perf] frame_id={} process_merge_completions_cpu_ms={:.3}",
                frame_id,
                perf_started.elapsed().as_secs_f64() * 1_000.0,
            );
        }
        Ok(())
    }

    pub fn drain_brush_execution_merge_feedbacks(&mut self) -> Vec<BrushExecutionMergeFeedback> {
        self.core.brush_execution_feedback_queue_mut().drain(..).collect()
    }

    pub fn finalize_merge_receipt(
        &mut self,
        receipt_id: StrokeExecutionReceiptId,
    ) -> Result<(), MergeBridgeError> {
        self.core.runtime_mut().renderer_mut()
            .ack_receipt_terminal_state(receipt_id, ReceiptTerminalState::Finalized)
            .map_err(MergeBridgeError::RendererFinalize)?;
        self.core.tile_merge_engine_mut()
            .finalize_receipt(receipt_id)
            .map_err(MergeBridgeError::Tiles)
    }

    pub fn abort_merge_receipt(
        &mut self,
        receipt_id: StrokeExecutionReceiptId,
    ) -> Result<(), MergeBridgeError> {
        self.core.runtime_mut().renderer_mut()
            .ack_receipt_terminal_state(receipt_id, ReceiptTerminalState::Aborted)
            .map_err(MergeBridgeError::RendererFinalize)?;
        self.core.tile_merge_engine_mut()
            .abort_receipt(receipt_id)
            .map_err(MergeBridgeError::Tiles)
    }

    pub fn query_merge_audit_record(
        &self,
        receipt_id: StrokeExecutionReceiptId,
    ) -> Result<MergeAuditRecord, MergeBridgeError> {
        self.core.tile_merge_engine()
            .query_merge_audit_record(receipt_id)
            .map_err(MergeBridgeError::Tiles)
    }

    pub fn pan_canvas(&mut self, delta_x: f32, delta_y: f32) {
        self.core.view_transform_mut()
            .pan_by(delta_x, delta_y)
            .unwrap_or_else(|error| panic!("pan canvas failed: {error:?}"));
        push_view_state(&self.core.runtime().view_sender(), &self.core.view_transform(), self.core.runtime().surface_size());
    }

    pub fn rotate_canvas(&mut self, delta_radians: f32) {
        self.core.view_transform_mut()
            .rotate_by(delta_radians)
            .unwrap_or_else(|error| panic!("rotate canvas failed: {error:?}"));
        push_view_state(&self.core.runtime().view_sender(), &self.core.view_transform(), self.core.runtime().surface_size());
    }

    pub fn zoom_canvas_about_viewport_point(
        &mut self,
        zoom_factor: f32,
        viewport_x: f32,
        viewport_y: f32,
    ) {
        self.core.view_transform_mut()
            .zoom_about_point(zoom_factor, viewport_x, viewport_y)
            .unwrap_or_else(|error| panic!("zoom canvas failed: {error:?}"));
        push_view_state(&self.core.runtime().view_sender(), &self.core.view_transform(), self.core.runtime().surface_size());
    }

    pub fn screen_to_canvas_point(&self, screen_x: f32, screen_y: f32) -> (f32, f32) {
        self.core.view_transform()
            .screen_to_canvas_point(screen_x, screen_y)
            .unwrap_or_else(|error| panic!("screen to canvas conversion failed: {error:?}"))
    }

    fn apply_tiles_business_results(
        &mut self,
        business_results: &[TilesBusinessResult],
    ) -> Result<(), MergeBridgeError> {
        for result in business_results {
            match result {
                TilesBusinessResult::CanFinalize {
                    receipt_id,
                    stroke_session_id,
                    layer_id,
                    new_key_mappings,
                    dirty_tiles,
                    ..
                } => {
                    let apply_started = Instant::now();
                    #[cfg(debug_assertions)]
                    {
                        let mut mapping_coords = HashSet::with_capacity(new_key_mappings.len());
                        for mapping in new_key_mappings {
                            if !mapping_coords.insert((mapping.tile_x, mapping.tile_y)) {
                                panic!(
                                    "duplicate tile coordinate in new_key_mappings: receipt_id={} stroke_session_id={} layer_id={} tile=({}, {})",
                                    receipt_id.0,
                                    stroke_session_id,
                                    layer_id,
                                    mapping.tile_x,
                                    mapping.tile_y
                                );
                            }
                        }
                        let mut dirty_coords = HashSet::with_capacity(dirty_tiles.len());
                        for (tile_x, tile_y) in dirty_tiles {
                            if !dirty_coords.insert((*tile_x, *tile_y)) {
                                panic!(
                                    "duplicate tile coordinate in dirty_tiles: receipt_id={} stroke_session_id={} layer_id={} tile=({}, {})",
                                    receipt_id.0, stroke_session_id, layer_id, tile_x, tile_y
                                );
                            }
                        }
                        if mapping_coords != dirty_coords {
                            panic!(
                                "dirty tile set does not match mapping tile set: receipt_id={} stroke_session_id={} layer_id={} mapping_count={} dirty_count={}",
                                receipt_id.0,
                                stroke_session_id,
                                layer_id,
                                mapping_coords.len(),
                                dirty_coords.len()
                            );
                        }
                    }
                    if Self::brush_trace_enabled() {
                        eprintln!(
                            "[brush_trace] merge_finalize_prepare receipt_id={} stroke_session_id={} layer_id={} mappings={} dirty_tiles={}",
                            receipt_id.0,
                            stroke_session_id,
                            layer_id,
                            new_key_mappings.len(),
                            dirty_tiles.len(),
                        );
                    }
                    let atlas_store = Arc::clone(&self.core.runtime().atlas_store());
                    let document_apply_result: Result<
                        Option<RenderTreeSnapshot>,
                        MergeBridgeError,
                    > = (|| {
                        let mut document = self.core.document()
                            .write()
                            .unwrap_or_else(|_| panic!("document write lock poisoned"));
                        let expected_revision = document.revision();
                        document
                            .begin_merge(*layer_id, *stroke_session_id, expected_revision)
                            .map_err(MergeBridgeError::Document)?;
                        let image_handle = document
                            .leaf_image_handle(*layer_id, *stroke_session_id)
                            .map_err(MergeBridgeError::Document)?;
                        let existing_image =
                            document
                                .image(image_handle)
                                .ok_or(MergeBridgeError::Document(
                                    DocumentMergeError::LayerNotFoundInStrokeSession {
                                        layer_id: *layer_id,
                                        stroke_session_id: *stroke_session_id,
                                    },
                                ))?;
                        let mut updated_image = (*existing_image).clone();
                        apply_tile_key_mappings(&mut updated_image, new_key_mappings)
                            .map_err(MergeBridgeError::TileImageApply)?;
                        let layer_dirty_tiles: Vec<TileCoordinate> = dirty_tiles
                            .iter()
                            .map(|(tile_x, tile_y)| TileCoordinate {
                                tile_x: *tile_x,
                                tile_y: *tile_y,
                            })
                            .collect();
                        document
                            .apply_merge_image(
                                *layer_id,
                                *stroke_session_id,
                                updated_image,
                                &layer_dirty_tiles,
                                false,
                            )
                            .map_err(MergeBridgeError::Document)?;
                        let committed_image_handle = document
                            .leaf_image_handle(*layer_id, *stroke_session_id)
                            .map_err(MergeBridgeError::Document)?;
                        let committed_image = document.image(committed_image_handle).ok_or(
                            MergeBridgeError::Document(
                                DocumentMergeError::LayerNotFoundInStrokeSession {
                                    layer_id: *layer_id,
                                    stroke_session_id: *stroke_session_id,
                                },
                            ),
                        )?;
                        for (tile_x, tile_y, tile_key) in committed_image.iter_tiles() {
                            if atlas_store.resolve(tile_key).is_none() {
                                panic!(
                                    "merge committed unresolved layer tile key: layer_id={} stroke_session_id={} image_handle={:?} tile=({}, {}) key={:?}",
                                    layer_id,
                                    stroke_session_id,
                                    committed_image_handle,
                                    tile_x,
                                    tile_y,
                                    tile_key
                                );
                            }
                        }
                        Ok(if document.take_render_tree_cache_dirty() {
                            Some(document.render_tree_snapshot())
                        } else {
                            None
                        })
                    })();
                    let render_tree = match document_apply_result {
                        Ok(render_tree) => render_tree,
                        Err(error) => {
                            self.core.brush_execution_feedback_queue_mut().push_back(
                                BrushExecutionMergeFeedback::MergeFailed {
                                    stroke_session_id: *stroke_session_id,
                                    message: format!("document merge apply failed: {error:?}"),
                                },
                            );
                            return Err(error);
                        }
                    };
                    if let Some(render_tree) = render_tree {
                        self.note_bound_render_tree("merge_apply", &render_tree);
                        self.core.runtime().view_sender()
                            .send(RenderOp::BindRenderTree(render_tree))
                            .expect("send updated render tree after merge");
                    }
                    if self.core.perf_log_enabled() {
                        eprintln!(
                            "[merge_bridge_perf] merge_finalize receipt_id={} stroke_session_id={} layer_id={} dirty_tiles={} cpu_apply_ms={:.3}",
                            receipt_id.0,
                            stroke_session_id,
                            layer_id,
                            dirty_tiles.len(),
                            apply_started.elapsed().as_secs_f64() * 1_000.0,
                        );
                    }
                    if let Err(error) = self.finalize_merge_receipt(*receipt_id) {
                        self.core.brush_execution_feedback_queue_mut().push_back(
                            BrushExecutionMergeFeedback::MergeFailed {
                                stroke_session_id: *stroke_session_id,
                                message: format!("finalize merge receipt failed: {error:?}"),
                            },
                        );
                        return Err(error);
                    }
                    self.core.brush_buffer_tile_keys()
                        .write()
                        .unwrap_or_else(|_| {
                            panic!("brush buffer tile key registry write lock poisoned")
                        })
                        .retain_stroke_tiles(*stroke_session_id, self.core.brush_buffer_store().as_ref());
                    self.core.brush_execution_feedback_queue_mut().push_back(
                        BrushExecutionMergeFeedback::MergeApplied {
                            stroke_session_id: *stroke_session_id,
                        },
                    );
                }
                TilesBusinessResult::RequiresAbort {
                    receipt_id,
                    stroke_session_id,
                    layer_id,
                    message,
                    ..
                } => {
                    let document_abort_result: Result<
                        Option<RenderTreeSnapshot>,
                        MergeBridgeError,
                    > = (|| {
                        let mut document = self.core.document()
                            .write()
                            .unwrap_or_else(|_| panic!("document write lock poisoned"));
                        if document.has_active_merge(*layer_id, *stroke_session_id) {
                            document
                                .abort_merge(*layer_id, *stroke_session_id)
                                .map_err(MergeBridgeError::Document)?;
                        }
                        Ok(if document.take_render_tree_cache_dirty() {
                            Some(document.render_tree_snapshot())
                        } else {
                            None
                        })
                    })();
                    let render_tree = match document_abort_result {
                        Ok(render_tree) => render_tree,
                        Err(error) => {
                            self.core.brush_execution_feedback_queue_mut().push_back(
                                BrushExecutionMergeFeedback::MergeFailed {
                                    stroke_session_id: *stroke_session_id,
                                    message: format!("document merge abort failed: {error:?}"),
                                },
                            );
                            return Err(error);
                        }
                    };
                    if let Some(render_tree) = render_tree {
                        self.note_bound_render_tree("merge_abort", &render_tree);
                        self.core.runtime().view_sender()
                            .send(RenderOp::BindRenderTree(render_tree))
                            .expect("send updated render tree after merge abort");
                    }
                    if let Err(error) = self.abort_merge_receipt(*receipt_id) {
                        self.core.brush_execution_feedback_queue_mut().push_back(
                            BrushExecutionMergeFeedback::MergeFailed {
                                stroke_session_id: *stroke_session_id,
                                message: format!("abort merge receipt failed: {error:?}"),
                            },
                        );
                        return Err(error);
                    }
                    self.core.brush_buffer_tile_keys()
                        .write()
                        .unwrap_or_else(|_| {
                            panic!("brush buffer tile key registry write lock poisoned")
                        })
                        .release_stroke_on_merge_failed(
                            *stroke_session_id,
                            self.core.brush_buffer_store().as_ref(),
                        );
                    self.core.brush_execution_feedback_queue_mut().push_back(
                        BrushExecutionMergeFeedback::MergeFailed {
                            stroke_session_id: *stroke_session_id,
                            message: format!("merge requires abort: {message}"),
                        },
                    );
                }
            }
        }
        Ok(())
    }

    fn drain_tile_gc_evictions(&mut self) {
        let evicted_batches = self.core.brush_buffer_store().drain_evicted_retain_batches();
        for evicted_batch in evicted_batches {
            self.core.brush_buffer_tile_keys()
                .write()
                .unwrap_or_else(|_| panic!("brush buffer tile key registry write lock poisoned"))
                .apply_retained_eviction(evicted_batch.retain_id, &evicted_batch.keys);
            self.apply_gc_evicted_batch(evicted_batch.retain_id, evicted_batch.keys.len());
        }
    }

    fn apply_gc_evicted_batch(&mut self, retain_id: u64, key_count: usize) {
        apply_gc_evicted_batch_state(
            &mut self.core.gc_evicted_batches_total(),
            &mut self.core.gc_evicted_keys_total(),
            retain_id,
            key_count,
        );
        eprintln!(
            "tiles gc evicted retain batch: retain_id={} key_count={} total_batches={} total_keys={}",
            retain_id, key_count, self.core.gc_evicted_batches_total(), self.core.gc_evicted_keys_total()
        );
    }
}

fn create_startup_document(
    atlas_store: &TileAtlasStore,
    startup_image_path: Option<&Path>,
) -> Document {
    let Some(startup_image_path) = startup_image_path else {
        eprintln!(
            "[startup] no startup image provided, using default empty document {}x{}",
            DEFAULT_DOCUMENT_WIDTH, DEFAULT_DOCUMENT_HEIGHT
        );
        return Document::new(DEFAULT_DOCUMENT_WIDTH, DEFAULT_DOCUMENT_HEIGHT);
    };
    eprintln!(
        "[startup] loading startup image from {}",
        startup_image_path.display()
    );

    let decoded = image::ImageReader::open(startup_image_path)
        .unwrap_or_else(|error| {
            panic!(
                "failed to open startup image at {}: {error}",
                startup_image_path.display()
            )
        })
        .decode()
        .unwrap_or_else(|error| {
            panic!(
                "failed to decode startup image at {}: {error}",
                startup_image_path.display()
            )
        })
        .to_rgba8();

    let size_x = decoded.width();
    let size_y = decoded.height();
    let image_bytes = decoded.into_raw();
    eprintln!(
        "[startup] startup image decoded: {}x{} ({} bytes)",
        size_x,
        size_y,
        image_bytes.len()
    );

    let image = atlas_store
        .ingest_image_rgba8_strided(size_x, size_y, &image_bytes, size_x * 4)
        .unwrap_or_else(|error| {
            panic!(
                "failed to ingest startup image into tile atlas at {}: {error:?}",
                startup_image_path.display()
            )
        });
    let tile_count = image.iter_tiles().count();
    eprintln!(
        "[startup] startup image ingested into tile atlas: non_empty_tiles={}",
        tile_count
    );

    let mut document = Document::new(size_x, size_y);
    let _layer_id = document.new_layer_root_with_image(image, BlendMode::Normal);
    eprintln!("[startup] startup layer inserted into document root");
    document
}

fn surface_format_to_default_atlas_format(surface_format: wgpu::TextureFormat) -> TileAtlasFormat {
    match surface_format {
        wgpu::TextureFormat::Rgba8Unorm => TileAtlasFormat::Rgba8Unorm,
        wgpu::TextureFormat::Rgba8UnormSrgb => TileAtlasFormat::Rgba8UnormSrgb,
        wgpu::TextureFormat::Bgra8Unorm => TileAtlasFormat::Bgra8Unorm,
        wgpu::TextureFormat::Bgra8UnormSrgb => TileAtlasFormat::Bgra8UnormSrgb,
        _ => panic!(
            "unsupported surface format for default tile atlas format: {:?}",
            surface_format
        ),
    }
}

pub(crate) fn push_view_state(
    view_sender: &ViewOpSender,
    view_transform: &ViewTransform,
    size: PhysicalSize<u32>,
) {
    view_sender
        .send(RenderOp::SetViewport(Viewport {
            origin_x: 0,
            origin_y: 0,
            width: size.width,
            height: size.height,
        }))
        .expect("send viewport");

    let matrix = view_transform
        .to_clip_matrix4x4(size.width as f32, size.height as f32)
        .expect("build clip matrix");
    view_sender
        .send(RenderOp::SetViewTransform { matrix })
        .expect("send view transform");
}

pub(crate) fn notice_id_from_renderer(notice: &MergeCompletionNotice) -> TileMergeCompletionNoticeId {
    TileMergeCompletionNoticeId {
        renderer_submission_id: notice.audit_meta.renderer_submission_id,
        frame_id: notice.audit_meta.frame_id,
        op_trace_id: notice.audit_meta.op_trace_id,
    }
}

fn apply_gc_evicted_batch_state(
    gc_evicted_batches_total: &mut u64,
    gc_evicted_keys_total: &mut u64,
    _retain_id: u64,
    key_count: usize,
) {
    *gc_evicted_batches_total = gc_evicted_batches_total
        .checked_add(1)
        .expect("gc evicted batch counter overflow");
    *gc_evicted_keys_total = gc_evicted_keys_total
        .checked_add(u64::try_from(key_count).expect("gc key count exceeds u64"))
        .expect("gc evicted key counter overflow");
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::TILE_IMAGE;

    fn snapshot_with_source(revision: u64, source: ImageSource) -> RenderTreeSnapshot {
        RenderTreeSnapshot {
            revision,
            root: Arc::new(render_protocol::RenderNodeSnapshot::Group {
                group_id: 0,
                blend: BlendMode::Normal,
                children: Arc::from([render_protocol::RenderNodeSnapshot::Leaf {
                    layer_id: 1,
                    blend: BlendMode::Normal,
                    image_source: source,
                }]),
            }),
        }
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "render tree semantics changed without revision bump")]
    fn render_tree_invariants_panics_on_semantics_change_without_revision_bump() {
        let base = snapshot_with_source(
            0,
            ImageSource::LayerImage {
                image_handle: ImageHandle::default(),
            },
        );
        let preview = snapshot_with_source(
            0,
            ImageSource::BrushBuffer {
                stroke_session_id: 42,
            },
        );

        let trace_enabled = false;
        let invariants_enabled = true;
        let last = GpuState::check_render_tree_semantics_invariants(
            "startup",
            None,
            &base,
            trace_enabled,
            invariants_enabled,
        );
        let _ = GpuState::check_render_tree_semantics_invariants(
            "preview_set",
            Some(last),
            &preview,
            trace_enabled,
            invariants_enabled,
        );
    }

    #[test]
    fn required_device_features_include_brush_storage_texture_support() {
        let required = GpuState::required_device_features();
        assert!(
            required.contains(wgpu::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES),
            "brush dab write uses R32Float storage texture; required device features must include TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES"
        );
    }

    #[test]
    fn surface_format_mapping_preserves_rgba_bgra_variants() {
        assert_eq!(
            surface_format_to_default_atlas_format(wgpu::TextureFormat::Rgba8Unorm),
            TileAtlasFormat::Rgba8Unorm
        );
        assert_eq!(
            surface_format_to_default_atlas_format(wgpu::TextureFormat::Rgba8UnormSrgb),
            TileAtlasFormat::Rgba8UnormSrgb
        );
        assert_eq!(
            surface_format_to_default_atlas_format(wgpu::TextureFormat::Bgra8Unorm),
            TileAtlasFormat::Bgra8Unorm
        );
        assert_eq!(
            surface_format_to_default_atlas_format(wgpu::TextureFormat::Bgra8UnormSrgb),
            TileAtlasFormat::Bgra8UnormSrgb
        );
    }

    fn create_device_queue() -> (wgpu::Device, wgpu::Queue) {
        pollster::block_on(async {
            let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
                backends: wgpu::Backends::all(),
                ..Default::default()
            });
            let adapter = instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::LowPower,
                    compatible_surface: None,
                    force_fallback_adapter: true,
                })
                .await
                .expect("request test adapter");
            adapter
                .request_device(&wgpu::DeviceDescriptor {
                    label: Some("glaphica tests"),
                    required_features: wgpu::Features::empty(),
                    required_limits: adapter.limits(),
                    experimental_features: wgpu::ExperimentalFeatures::disabled(),
                    memory_hints: wgpu::MemoryHints::Performance,
                    trace: wgpu::Trace::Off,
                })
                .await
                .expect("request test device")
        })
    }

    fn read_tile_rgba8(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        texture: &wgpu::Texture,
        atlas_layout: tiles::TileAtlasLayout,
        address: TileAddress,
    ) -> Vec<u8> {
        let row_bytes = (TILE_IMAGE * 4) as usize;
        let padded_row_bytes = row_bytes.next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize);
        let buffer_size = (padded_row_bytes as u64) * (TILE_IMAGE as u64);
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("glaphica.tests.readback"),
            size: buffer_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("glaphica.tests.readback"),
        });
        let (origin_x, origin_y) = address.atlas_content_origin_pixels_in(atlas_layout);
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: origin_x,
                    y: origin_y,
                    z: address.atlas_layer,
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_row_bytes as u32),
                    rows_per_image: Some(TILE_IMAGE),
                },
            },
            wgpu::Extent3d {
                width: TILE_IMAGE,
                height: TILE_IMAGE,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(Some(encoder.finish()));

        let slice = buffer.slice(..);
        let (sender, receiver) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            sender.send(result).expect("map callback send");
        });
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("device poll");
        receiver
            .recv()
            .expect("map callback recv")
            .expect("map tile readback");
        let tile = slice.get_mapped_range().to_vec();
        buffer.unmap();
        
        // Remove padding from each row to get actual pixel data
        let mut result = Vec::with_capacity(row_bytes * TILE_IMAGE as usize);
        for row in 0..TILE_IMAGE as usize {
            let row_start = row * padded_row_bytes;
            let row_end = row_start + row_bytes;
            result.extend_from_slice(&tile[row_start..row_end]);
        }
        result
    }

    #[test]
    fn image_from_tests_resources_round_trips_through_document_and_gpu_atlas() {
        let image_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/resources/document_import_e2e.png");
        let decoded = image::ImageReader::open(&image_path)
            .expect("open e2e source image")
            .decode()
            .expect("decode e2e source image")
            .to_rgba8();
        let size_x = decoded.width();
        let size_y = decoded.height();
        let source_bytes = decoded.into_raw();

        let (device, queue) = create_device_queue();
        let (atlas_store, atlas_gpu) =
            Renderer::create_default_tile_atlas(&device, TileAtlasFormat::Rgba8Unorm)
                .expect("create tile atlas store");

        let virtual_image = atlas_store
            .ingest_image_rgba8_strided(size_x, size_y, &source_bytes, size_x * 4)
            .expect("ingest source image into tile atlas");
        atlas_gpu
            .drain_and_execute(&queue)
            .expect("flush tile uploads to gpu atlas");
        let atlas_layout = atlas_gpu.layout();

        let mut document = Document::new(size_x, size_y);
        let _layer_id = document.new_layer_root_with_image(virtual_image, BlendMode::Normal);
        let snapshot = document.render_tree_snapshot();
        let image_handle = find_first_leaf_image_handle(snapshot.root.as_ref())
            .expect("snapshot should contain a leaf image");
        let document_image = document
            .image(image_handle)
            .expect("snapshot leaf image handle should resolve");

        let rendered_bytes = document_image
            .export_rgba8(|tile_key| {
                let address = atlas_store.resolve(tile_key)?;
                Some(read_tile_rgba8(
                    &device,
                    &queue,
                    atlas_gpu.texture(),
                    atlas_layout,
                    address,
                ))
            })
            .expect("export rendered image from document tiles");

        assert_eq!(rendered_bytes, source_bytes);
    }

    #[test]
    fn apply_gc_evicted_batch_state_updates_counters() {
        let mut gc_evicted_batches_total = 0u64;
        let mut gc_evicted_keys_total = 0u64;

        apply_gc_evicted_batch_state(
            &mut gc_evicted_batches_total,
            &mut gc_evicted_keys_total,
            42,
            3,
        );
        apply_gc_evicted_batch_state(
            &mut gc_evicted_batches_total,
            &mut gc_evicted_keys_total,
            42,
            2,
        );

        assert_eq!(gc_evicted_batches_total, 2);
        assert_eq!(gc_evicted_keys_total, 5);
    }

    #[test]
    fn apply_gc_evicted_batch_state_keeps_empty_batch_accounting_only() {
        let mut gc_evicted_batches_total = 0u64;
        let mut gc_evicted_keys_total = 0u64;

        apply_gc_evicted_batch_state(
            &mut gc_evicted_batches_total,
            &mut gc_evicted_keys_total,
            100,
            0,
        );

        assert_eq!(gc_evicted_batches_total, 1);
        assert_eq!(gc_evicted_keys_total, 0);
    }

    fn find_first_leaf_image_handle(
        node: &render_protocol::RenderNodeSnapshot,
    ) -> Option<ImageHandle> {
        match node {
            render_protocol::RenderNodeSnapshot::Leaf { image_source, .. } => match image_source {
                render_protocol::ImageSource::LayerImage { image_handle } => Some(*image_handle),
                render_protocol::ImageSource::BrushBuffer { .. } => None,
            },
            render_protocol::RenderNodeSnapshot::Group { children, .. } => {
                children.iter().find_map(find_first_leaf_image_handle)
            }
        }
    }
}
