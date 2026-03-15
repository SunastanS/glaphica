use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Duration;
use std::{fmt::Display, path::Path};

use brushes::{
    BrushDrawInputLayout, BrushDrawKind, BrushGpuPipelineRegistry, BrushLayoutRegistry,
    BrushRegistryError, BrushSpec,
};
use document::{FlatRenderTree, SharedRenderTree, View};
use glaphica_core::{
    AtlasLayout, BackendId, BackendKind, BrushId, ImageDirtyTracker, NodeId, RenderTreeGeneration,
    TextureFormat, TileDirtyTracker, TileKey,
};
use gpu_runtime::{
    FrameBatch, FrameBatchContext, FrameBatchPerfStats, GpuContext, GpuContextInitDescriptor,
    RenderContext, RenderExecutor,
    atlas_runtime::AtlasStorageRuntime,
    brush_runtime::{BrushGpuRuntime, validate_draw_op_layout},
    surface_runtime::{SurfaceError, SurfaceRuntime},
    wgpu_brush_executor::WgpuBrushExecutorError,
};
use thread_protocol::{GpuCmdMsg, RenderTreeUpdatedMsg, TileSlotKeyUpdateMsg};

use crate::{
    config,
    layer_image_export::{LayerImageExportError, LayerImageExporter},
    layer_preview::{LayerPreviewBitmap, LayerPreviewRenderer, PreviewSource},
    screen_blitter::ScreenBlitter,
};

#[derive(Debug, Default, Clone, Copy)]
pub struct GpuSubmitPerfStats {
    pub frame_batch: FrameBatchPerfStats,
    pub dirty_tile_count: usize,
    pub dirty_rect_count: usize,
    pub dirty_bbox_tile_area: usize,
    pub dirty_node_count: usize,
}

#[derive(Default)]
struct DirtyNodeBounds {
    min_x: u32,
    min_y: u32,
    max_x: u32,
    max_y: u32,
    has_any: bool,
}

#[derive(Debug, Clone, Copy)]
struct GpuCmdTraceConfig {
    enabled: bool,
    max_commands: usize,
}

fn gpu_cmd_trace_config() -> GpuCmdTraceConfig {
    static CONFIG: OnceLock<GpuCmdTraceConfig> = OnceLock::new();
    *CONFIG.get_or_init(|| {
        let enabled = std::env::var("GLAPHICA_DEBUG_GPU_CMD_TRACE")
            .ok()
            .is_some_and(|value| value != "0");
        let max_commands = std::env::var("GLAPHICA_DEBUG_GPU_CMD_TRACE_MAX")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(64);
        GpuCmdTraceConfig {
            enabled,
            max_commands,
        }
    })
}

fn pipeline_trace_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("GLAPHICA_DEBUG_PIPELINE_TRACE")
            .ok()
            .is_some_and(|value| value != "0")
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct DrawLaneKey {
    node_id: NodeId,
    tile_index: usize,
}

fn draw_lane_key(cmd: &GpuCmdMsg) -> Option<DrawLaneKey> {
    match cmd {
        GpuCmdMsg::DrawOp(draw_op) => Some(DrawLaneKey {
            node_id: draw_op.node_id,
            tile_index: draw_op.tile_index,
        }),
        _ => None,
    }
}

fn validate_draw_lane_contract(commands: &[GpuCmdMsg]) {
    let mut lane_to_tile_key: HashMap<DrawLaneKey, TileKey> = HashMap::new();

    for cmd in commands {
        let GpuCmdMsg::DrawOp(draw_op) = cmd else {
            continue;
        };
        let lane = DrawLaneKey {
            node_id: draw_op.node_id,
            tile_index: draw_op.tile_index,
        };
        match lane_to_tile_key.get(&lane).copied() {
            Some(existing) if existing != draw_op.tile_key => {
                eprintln!(
                    "[BUG][gpu_cmd_lane] lane {:?} maps to multiple tile keys in one frame: {:?} then {:?}",
                    lane, existing, draw_op.tile_key
                );
                debug_assert_eq!(
                    existing, draw_op.tile_key,
                    "draw lane must map to a stable tile_key within one frame"
                );
            }
            Some(_) => {}
            None => {
                lane_to_tile_key.insert(lane, draw_op.tile_key);
            }
        }
    }
}

fn build_draw_lane_plan(commands: &[GpuCmdMsg]) -> Vec<Vec<usize>> {
    let mut lane_index_map: HashMap<DrawLaneKey, usize> = HashMap::new();
    let mut lanes: Vec<Vec<usize>> = Vec::new();
    for (cmd_index, cmd) in commands.iter().enumerate() {
        let Some(lane_key) = draw_lane_key(cmd) else {
            continue;
        };
        let lane = match lane_index_map.get(&lane_key).copied() {
            Some(existing) => existing,
            None => {
                let next = lanes.len();
                lanes.push(Vec::new());
                lane_index_map.insert(lane_key, next);
                next
            }
        };
        lanes[lane].push(cmd_index);
    }
    lanes
}

fn prevalidate_draw_layouts_parallel(
    commands: &[GpuCmdMsg],
    brush_layouts: &BrushLayoutRegistry,
    draw_lane_plan: &[Vec<usize>],
) -> Vec<Option<BrushDrawInputLayout>> {
    let mut prevalidated = vec![None; commands.len()];
    if draw_lane_plan.is_empty() {
        return prevalidated;
    }

    let max_workers = std::thread::available_parallelism()
        .map(|parallelism| parallelism.get())
        .unwrap_or(1);
    let worker_count = draw_lane_plan.len().min(max_workers).max(1);
    let chunk_size = draw_lane_plan.len().div_ceil(worker_count);

    let worker_results = std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for lane_chunk in draw_lane_plan.chunks(chunk_size) {
            handles.push(scope.spawn(move || {
                let mut layouts = Vec::new();
                let mut errors = Vec::new();
                for lane in lane_chunk {
                    for &index in lane {
                        let GpuCmdMsg::DrawOp(draw_op) = &commands[index] else {
                            continue;
                        };
                        match validate_draw_op_layout(draw_op, brush_layouts) {
                            Ok(layout) => layouts.push((index, layout)),
                            Err(error) => errors.push(format!("index {}: {}", index, error)),
                        }
                    }
                }
                (layouts, errors)
            }));
        }
        let mut results = Vec::new();
        for handle in handles {
            match handle.join() {
                Ok(result) => results.push(result),
                Err(_) => {
                    eprintln!(
                        "[BUG][gpu_cmd_lane] draw layout prevalidation worker thread panicked"
                    );
                    debug_assert!(false, "draw layout prevalidation worker thread panicked");
                }
            }
        }
        results
    });

    for (layouts, errors) in worker_results {
        for (index, layout) in layouts {
            prevalidated[index] = Some(layout);
        }
        for error in errors {
            eprintln!("GPU command processing failed: {error}");
        }
    }
    prevalidated
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct MergeableRoundDrawKey {
    node_id: NodeId,
    tile_index: usize,
    tile_key: TileKey,
    brush_id: BrushId,
    stroke_id: glaphica_core::StrokeId,
}

#[derive(Debug)]
struct PendingVisibleTileUpdateBatch {
    submission_index: wgpu::SubmissionIndex,
    updates: Vec<(NodeId, usize, TileKey)>,
}

fn compact_round_draws(
    commands: &[GpuCmdMsg],
    prevalidated_layouts: &[Option<BrushDrawInputLayout>],
) -> (Vec<GpuCmdMsg>, Vec<Option<BrushDrawInputLayout>>) {
    let mut compacted_commands = Vec::with_capacity(commands.len());
    let mut compacted_layouts = Vec::with_capacity(commands.len());
    let mut merged_indices: HashMap<MergeableRoundDrawKey, usize> = HashMap::new();

    for (cmd, layout) in commands.iter().zip(prevalidated_layouts.iter().copied()) {
        let can_merge = match (cmd, layout) {
            (GpuCmdMsg::DrawOp(draw_op), Some(layout))
                if layout.kind() == BrushDrawKind::Round
                    && draw_op.blend_mode == thread_protocol::DrawBlendMode::Additive
                    && draw_op.origin_tile == TileKey::EMPTY
                    && draw_op.ref_image.is_none() =>
            {
                // Round stroke-buffer draws only accumulate thickness into a transient tile.
                // Within one frame that makes same-tile dabs mergeable as a single packed draw:
                // no origin/ref sampling is involved and the final write happens later.
                Some(MergeableRoundDrawKey {
                    node_id: draw_op.node_id,
                    tile_index: draw_op.tile_index,
                    tile_key: draw_op.tile_key,
                    brush_id: draw_op.brush_id,
                    stroke_id: draw_op.stroke_id,
                })
            }
            _ => None,
        };

        if let Some(key) = can_merge {
            if let Some(existing_index) = merged_indices.get(&key).copied() {
                let GpuCmdMsg::DrawOp(existing_draw) = &mut compacted_commands[existing_index]
                else {
                    debug_assert!(false, "merged round draw index must reference draw op");
                    continue;
                };
                let GpuCmdMsg::DrawOp(draw_op) = cmd else {
                    debug_assert!(false, "mergeable round key must come from draw op");
                    continue;
                };
                existing_draw.input.extend_from_slice(&draw_op.input);
                continue;
            }
            merged_indices.insert(key, compacted_commands.len());
        }

        compacted_commands.push(cmd.clone());
        compacted_layouts.push(layout);
    }

    (compacted_commands, compacted_layouts)
}

pub struct MainThreadState {
    gpu_context: Arc<GpuContext>,
    atlas_storage: AtlasStorageRuntime,
    surface_runtime: Option<SurfaceRuntime>,
    screen_blitter: ScreenBlitter,
    render_executor: RenderExecutor,
    brush_runtime: BrushGpuRuntime<gpu_runtime::wgpu_brush_executor::WgpuBrushExecutor>,
    brush_layouts: BrushLayoutRegistry,
    brush_pipeline_registry: BrushGpuPipelineRegistry,
    shared_tree: Arc<SharedRenderTree>,
    view: View,
    image_dirty_tracker: ImageDirtyTracker,
    tile_dirty_tracker: TileDirtyTracker,
    pending_visible_tile_updates: VecDeque<PendingVisibleTileUpdateBatch>,
    layer_preview_renderer: LayerPreviewRenderer,
    layer_preview_updates: Vec<LayerPreviewBitmap>,
    pending_preview_nodes: Vec<NodeId>,
    blocked_preview_nodes: HashSet<NodeId>,
    next_brush_cache_backend_id: u8,
    layer_image_exporter: LayerImageExporter,
}

impl MainThreadState {
    pub async fn init() -> Result<Self, InitError> {
        let gpu_context = Arc::new(
            GpuContext::init(&GpuContextInitDescriptor::default())
                .await
                .map_err(InitError::GpuContext)?,
        );
        Self::init_with_gpu_context(gpu_context).await
    }

    pub async fn init_with_gpu_context(gpu_context: Arc<GpuContext>) -> Result<Self, InitError> {
        let mut atlas_storage =
            AtlasStorageRuntime::with_capacity(config::atlas_storage::INITIAL_BACKEND_CAPACITY);
        atlas_storage
            .create_backend(
                &gpu_context.device,
                0,
                BackendKind::Leaf,
                AtlasLayout::Small11,
                Default::default(),
            )
            .map_err(InitError::Atlas)?;
        atlas_storage
            .create_backend(
                &gpu_context.device,
                1,
                BackendKind::BranchCache,
                AtlasLayout::Small11,
                Default::default(),
            )
            .map_err(InitError::Atlas)?;

        Ok(Self {
            gpu_context,
            atlas_storage,
            surface_runtime: None,
            screen_blitter: ScreenBlitter::new(),
            render_executor: RenderExecutor::new(),
            brush_runtime: BrushGpuRuntime::new(
                gpu_runtime::wgpu_brush_executor::WgpuBrushExecutor::new(),
            ),
            brush_layouts: BrushLayoutRegistry::new(
                config::registry_capacities::BRUSH_LAYOUT_REGISTRY,
            ),
            brush_pipeline_registry: BrushGpuPipelineRegistry::new(
                config::registry_capacities::BRUSH_PIPELINE_REGISTRY,
            ),
            shared_tree: Arc::new(SharedRenderTree::new(FlatRenderTree {
                generation: RenderTreeGeneration(0),
                nodes: Arc::new(std::collections::HashMap::new()),
                root_id: None,
            })),
            view: View::default(),
            image_dirty_tracker: ImageDirtyTracker::default(),
            tile_dirty_tracker: TileDirtyTracker::default(),
            pending_visible_tile_updates: VecDeque::new(),
            layer_preview_renderer: LayerPreviewRenderer::new(),
            layer_preview_updates: Vec::new(),
            pending_preview_nodes: Vec::new(),
            blocked_preview_nodes: HashSet::new(),
            next_brush_cache_backend_id: 2,
            layer_image_exporter: LayerImageExporter::new(),
        })
    }

    pub fn register_brush<S: BrushSpec>(
        &mut self,
        brush_id: BrushId,
        brush: &S,
    ) -> Result<Option<BackendId>, BrushRegisterError> {
        let layout = brush.draw_input_layout();
        let spec = brush.gpu_pipeline_spec();

        self.brush_layouts
            .register_layout(brush_id, layout)
            .map_err(BrushRegisterError::Layout)?;
        self.brush_pipeline_registry
            .register_pipeline_spec(brush_id, spec)
            .map_err(BrushRegisterError::Pipeline)?;

        let cache_backend_id = match brush.cache_backend_kind() {
            Some(kind) => {
                let id = self.next_brush_cache_backend_id;
                let mut texture_config = gpu_runtime::atlas_runtime::AtlasTextureConfig::default();
                if let Some(format) = spec.cache_backend_format {
                    texture_config.format = to_wgpu_texture_format(format);
                }
                self.atlas_storage
                    .create_backend(
                        &self.gpu_context.device,
                        id,
                        kind,
                        AtlasLayout::Small11,
                        texture_config,
                    )
                    .map_err(|_| BrushRegisterError::CacheBackendAlloc { brush_id })?;
                self.next_brush_cache_backend_id += 1;
                Some(BackendId::new(id))
            }
            None => None,
        };

        self.brush_runtime
            .executor_mut()
            .configure_brush(brush_id, spec, cache_backend_id.map(|id| id.raw()))
            .map_err(BrushRegisterError::Executor)?;

        Ok(cache_backend_id)
    }

    pub fn set_shared_tree(&mut self, shared_tree: Arc<SharedRenderTree>) {
        self.shared_tree = shared_tree;
        let tree = self.shared_tree.read();
        self.enqueue_all_preview_nodes(&tree);
    }

    pub fn gpu_context(&self) -> &Arc<GpuContext> {
        &self.gpu_context
    }

    pub fn export_layer_image(
        &mut self,
        image: &images::Image,
    ) -> Result<images::StoredImage, LayerImageExportError> {
        self.layer_image_exporter.export(
            &self.gpu_context.device,
            &self.gpu_context.queue,
            &self.atlas_storage,
            image,
        )
    }

    pub fn upload_tile_rgba8(&self, tile_key: TileKey, rgba8: &[u8]) -> bool {
        let Some(resolved) = self.atlas_storage.resolve(tile_key) else {
            return false;
        };
        let expected_len =
            (glaphica_core::IMAGE_TILE_SIZE * glaphica_core::IMAGE_TILE_SIZE * 4) as usize;
        if rgba8.len() != expected_len {
            return false;
        }
        self.gpu_context.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: resolved.texture2d_array,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: resolved.address.texel_offset.0 + glaphica_core::GUTTER_SIZE,
                    y: resolved.address.texel_offset.1 + glaphica_core::GUTTER_SIZE,
                    z: resolved.address.layer,
                },
                aspect: wgpu::TextureAspect::All,
            },
            rgba8,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(glaphica_core::IMAGE_TILE_SIZE * 4),
                rows_per_image: Some(glaphica_core::IMAGE_TILE_SIZE),
            },
            wgpu::Extent3d {
                width: glaphica_core::IMAGE_TILE_SIZE,
                height: glaphica_core::IMAGE_TILE_SIZE,
                depth_or_array_layers: 1,
            },
        );
        true
    }

    pub fn set_surface(&mut self, surface_runtime: SurfaceRuntime) {
        self.surface_runtime = Some(surface_runtime);
    }

    pub fn view(&self) -> &View {
        &self.view
    }

    pub fn view_mut(&mut self) -> &mut View {
        &mut self.view
    }

    pub fn resize_surface(&mut self, width: u32, height: u32) {
        if let Some(surface) = &mut self.surface_runtime {
            surface.resize(&self.gpu_context.device, width, height);
        }
    }

    pub fn present_to_screen(&mut self) -> Result<(), PresentError> {
        self.present_to_screen_with_overlay(|_, _, _, _, _, _, _| {})
    }

    pub fn present_to_screen_with_overlay<F>(&mut self, mut overlay: F) -> Result<(), PresentError>
    where
        F: FnMut(
            &wgpu::Device,
            &wgpu::Queue,
            &mut wgpu::CommandEncoder,
            &wgpu::TextureView,
            wgpu::TextureFormat,
            u32,
            u32,
        ),
    {
        self.promote_completed_tile_updates();
        let surface = self
            .surface_runtime
            .as_mut()
            .ok_or(PresentError::NoSurface)?;
        let frame = surface.acquire_frame().map_err(PresentError::Surface)?;
        let tree = self.shared_tree.read();
        let width = surface.width();
        let height = surface.height();
        let format = surface.format();
        let mut encoder =
            self.gpu_context
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("main-present-encoder"),
                });

        self.screen_blitter.blit_into_encoder(
            &self.gpu_context.device,
            &self.gpu_context.queue,
            &self.atlas_storage,
            &tree,
            &self.view,
            &frame.view,
            format,
            width,
            height,
            &mut encoder,
        );
        overlay(
            &self.gpu_context.device,
            &self.gpu_context.queue,
            &mut encoder,
            &frame.view,
            format,
            width,
            height,
        );
        self.gpu_context.queue.submit(Some(encoder.finish()));

        SurfaceRuntime::present(frame);
        Ok(())
    }

    pub fn process_render(&mut self) -> bool {
        self.promote_completed_tile_updates();
        let tree = self.shared_tree.read();
        let cmds = tree.build_render_cmds(&self.image_dirty_tracker);
        let mut has_work = false;

        if !cmds.is_empty() {
            let mut context = RenderContext {
                gpu_context: &self.gpu_context,
                atlas_storage: &self.atlas_storage,
            };

            if let Err(e) = self.render_executor.execute(&mut context, &cmds) {
                eprintln!("Render execution failed: {e}");
                return false;
            }

            self.image_dirty_tracker.clear();
            has_work = true;
        }

        if self.process_layer_previews(&tree) {
            has_work = true;
        }

        has_work
    }

    pub fn clear_dirty_markers(&mut self) {
        self.image_dirty_tracker.clear();
        self.tile_dirty_tracker.clear();
    }

    pub fn reset_document_runtime_state(&mut self) {
        self.pending_visible_tile_updates.clear();
        self.clear_dirty_markers();
        self.pending_preview_nodes.clear();
        self.blocked_preview_nodes.clear();
        self.layer_preview_updates.clear();
    }

    pub fn take_layer_preview_updates(&mut self) -> Vec<LayerPreviewBitmap> {
        std::mem::take(&mut self.layer_preview_updates)
    }

    pub fn begin_preview_stroke(&mut self, node_id: NodeId) {
        let tree = self.shared_tree.read();
        let mut current = Some(node_id);
        while let Some(blocked_id) = current {
            if preview_source_image(&tree, blocked_id).is_some() {
                self.blocked_preview_nodes.insert(blocked_id);
            }
            current = tree.nodes.get(&blocked_id).and_then(|node| node.parent_id);
        }
    }

    pub fn end_preview_stroke(&mut self, node_id: NodeId) {
        let tree = self.shared_tree.read();
        let mut current = Some(node_id);
        while let Some(unblocked_id) = current {
            self.blocked_preview_nodes.remove(&unblocked_id);
            current = tree
                .nodes
                .get(&unblocked_id)
                .and_then(|node| node.parent_id);
        }
    }

    pub fn process_gpu_commands(&mut self, commands: &[GpuCmdMsg]) -> GpuSubmitPerfStats {
        self.promote_completed_tile_updates();
        let mut submit_perf = GpuSubmitPerfStats::default();
        let trace_config = gpu_cmd_trace_config();
        if trace_config.enabled {
            trace_gpu_commands(commands, trace_config.max_commands);
        }
        validate_draw_lane_contract(commands);
        let draw_lane_plan = build_draw_lane_plan(commands);
        let prevalidated_draw_layouts =
            prevalidate_draw_layouts_parallel(commands, &self.brush_layouts, &draw_lane_plan);
        let original_draw_count = commands
            .iter()
            .filter(|cmd| matches!(cmd, GpuCmdMsg::DrawOp(_)))
            .count();
        let (commands, prevalidated_draw_layouts) =
            compact_round_draws(commands, &prevalidated_draw_layouts);
        if pipeline_trace_enabled() {
            let compacted_draw_count = commands
                .iter()
                .filter(|cmd| matches!(cmd, GpuCmdMsg::DrawOp(_)))
                .count();
            if compacted_draw_count != original_draw_count {
                eprintln!(
                    "[PERF][gpu_draw_compact] before={} after={}",
                    original_draw_count, compacted_draw_count
                );
            }
        }
        let mut frame_batch = FrameBatch::new(&self.gpu_context);
        let mut deferred_visible_tile_updates = Vec::new();
        let mut index = 0usize;
        while index < commands.len() {
            let cmd = &commands[index];
            match cmd {
                GpuCmdMsg::RenderTreeUpdated(msg) => {
                    self.apply_render_tree_updated(msg);
                    index += 1;
                }
                GpuCmdMsg::TileSlotKeyUpdate(op) => {
                    self.defer_tile_slot_key_update(op, &mut deferred_visible_tile_updates);
                    index += 1;
                }
                GpuCmdMsg::DrawOp(_) if prevalidated_draw_layouts[index].is_some() => {
                    let mut draw_ops: Vec<&thread_protocol::DrawOp> = Vec::new();
                    let mut layouts: Vec<BrushDrawInputLayout> = Vec::new();
                    let mut end = index;
                    while end < commands.len() {
                        let GpuCmdMsg::DrawOp(draw_op) = &commands[end] else {
                            break;
                        };
                        let Some(layout) = prevalidated_draw_layouts[end] else {
                            break;
                        };
                        draw_ops.push(draw_op);
                        layouts.push(layout);
                        end += 1;
                    }
                    if !draw_ops.is_empty() {
                        let mut context = FrameBatchContext {
                            gpu_context: &self.gpu_context,
                            atlas_storage: &self.atlas_storage,
                            render_executor: &mut self.render_executor,
                            brush_runtime: &mut self.brush_runtime,
                            brush_layouts: &self.brush_layouts,
                            shared_tree: &self.shared_tree,
                            image_dirty_tracker: &mut self.image_dirty_tracker,
                            tile_dirty_tracker: &mut self.tile_dirty_tracker,
                        };
                        if let Err(error) =
                            frame_batch.push_draw_batch(&draw_ops, &layouts, &mut context)
                        {
                            eprintln!("GPU command processing failed: {error:?}");
                        }
                        index = end;
                    } else {
                        index += 1;
                    }
                }
                GpuCmdMsg::WriteOp(_) => {
                    let mut write_ops: Vec<&thread_protocol::WriteOp> = Vec::new();
                    let mut end = index;
                    while end < commands.len() {
                        let GpuCmdMsg::WriteOp(write_op) = &commands[end] else {
                            break;
                        };
                        write_ops.push(write_op);
                        end += 1;
                    }
                    let mut context = FrameBatchContext {
                        gpu_context: &self.gpu_context,
                        atlas_storage: &self.atlas_storage,
                        render_executor: &mut self.render_executor,
                        brush_runtime: &mut self.brush_runtime,
                        brush_layouts: &self.brush_layouts,
                        shared_tree: &self.shared_tree,
                        image_dirty_tracker: &mut self.image_dirty_tracker,
                        tile_dirty_tracker: &mut self.tile_dirty_tracker,
                    };
                    if let Err(error) = frame_batch.push_write_batch(&write_ops, &mut context) {
                        eprintln!("GPU command processing failed: {error:?}");
                    }
                    index = end;
                }
                _ => {
                    let mut context = FrameBatchContext {
                        gpu_context: &self.gpu_context,
                        atlas_storage: &self.atlas_storage,
                        render_executor: &mut self.render_executor,
                        brush_runtime: &mut self.brush_runtime,
                        brush_layouts: &self.brush_layouts,
                        shared_tree: &self.shared_tree,
                        image_dirty_tracker: &mut self.image_dirty_tracker,
                        tile_dirty_tracker: &mut self.tile_dirty_tracker,
                    };
                    if let Err(error) = frame_batch.push_command_with_layout(
                        cmd,
                        &mut context,
                        prevalidated_draw_layouts[index],
                    ) {
                        eprintln!("GPU command processing failed: {error:?}");
                    }
                    index += 1;
                }
            }
        }

        let dirty_summary =
            summarize_dirty_tracker(&self.shared_tree.read(), &self.image_dirty_tracker);
        submit_perf.dirty_tile_count = dirty_summary.0;
        submit_perf.dirty_rect_count = dirty_summary.1;
        submit_perf.dirty_bbox_tile_area = dirty_summary.2;
        submit_perf.dirty_node_count = dirty_summary.3;
        let mut context = FrameBatchContext {
            gpu_context: &self.gpu_context,
            atlas_storage: &self.atlas_storage,
            render_executor: &mut self.render_executor,
            brush_runtime: &mut self.brush_runtime,
            brush_layouts: &self.brush_layouts,
            shared_tree: &self.shared_tree,
            image_dirty_tracker: &mut self.image_dirty_tracker,
            tile_dirty_tracker: &mut self.tile_dirty_tracker,
        };
        let submission_index = match frame_batch.finish(&mut context) {
            Ok((submission_index, frame_batch_perf)) => {
                submit_perf.frame_batch = frame_batch_perf;
                submission_index
            }
            Err(error) => {
                eprintln!("GPU command processing failed during frame finalize: {error:?}");
                None
            }
        };

        if let Some(submission_index) = submission_index {
            if !deferred_visible_tile_updates.is_empty() {
                self.pending_visible_tile_updates
                    .push_back(PendingVisibleTileUpdateBatch {
                        submission_index,
                        updates: deferred_visible_tile_updates,
                    });
            }
        } else if !deferred_visible_tile_updates.is_empty() {
            self.apply_visible_tile_updates(&deferred_visible_tile_updates);
        }
        self.brush_runtime
            .executor_mut()
            .clear_transient_draw_resources();
        let dirty_node_ids = self
            .image_dirty_tracker
            .iter()
            .map(|key| key.node_id)
            .collect::<Vec<_>>();
        let tree = self.shared_tree.read();
        self.enqueue_dirty_preview_nodes(&tree, &dirty_node_ids);
        submit_perf
    }

    fn apply_render_tree_updated(&mut self, msg: &RenderTreeUpdatedMsg) {
        let tree = self.shared_tree.read();
        for node_id in &msg.dirty_render_caches {
            if let Some(node) = tree.nodes.get(node_id) {
                let Some(render_cache) = node.kind.render_cache() else {
                    continue;
                };
                for tile_index in 0..render_cache.tile_count() {
                    self.image_dirty_tracker.mark(*node_id, tile_index);
                }
            }
        }
        for node_id in &msg.dirty_render_caches {
            self.enqueue_preview_node(*node_id);
        }
    }

    fn defer_tile_slot_key_update(
        &mut self,
        op: &TileSlotKeyUpdateMsg,
        deferred_updates: &mut Vec<(NodeId, usize, TileKey)>,
    ) {
        deferred_updates.extend(op.updates.iter().copied());
    }

    fn promote_completed_tile_updates(&mut self) {
        while let Some(batch) = self.pending_visible_tile_updates.front() {
            let poll = self.gpu_context.device.poll(wgpu::PollType::Wait {
                submission_index: Some(batch.submission_index.clone()),
                timeout: Some(Duration::ZERO),
            });
            match poll {
                Ok(status) if status.wait_finished() => {}
                Ok(_) | Err(wgpu::PollError::Timeout) => break,
                Err(error) => {
                    eprintln!("GPU poll failed while promoting tile updates: {error}");
                    break;
                }
            }

            if let Some(batch) = self.pending_visible_tile_updates.pop_front() {
                self.apply_visible_tile_updates(&batch.updates);
            }
        }
    }

    fn apply_visible_tile_updates(&mut self, updates: &[(NodeId, usize, TileKey)]) {
        if updates.is_empty() {
            return;
        }

        let tree = self.shared_tree.read();
        let mut new_nodes = (*tree.nodes).clone();

        for (node_id, tile_index, tile_key) in updates {
            if let Some(node) = new_nodes.get_mut(node_id) {
                let Some(image) = node.kind.render_image_mut() else {
                    continue;
                };
                if image.set_tile_key(*tile_index, *tile_key).is_ok() {
                    self.image_dirty_tracker.mark(*node_id, *tile_index);
                    self.tile_dirty_tracker.mark(*tile_key);
                }
            }
        }

        self.shared_tree.update(FlatRenderTree {
            generation: RenderTreeGeneration(tree.generation.0 + 1),
            nodes: Arc::new(new_nodes),
            root_id: tree.root_id,
        });

        let updated_tree = self.shared_tree.read();
        for (node_id, _, _) in updates {
            let mut current = Some(*node_id);
            while let Some(ancestor_id) = current {
                if preview_source_image(&updated_tree, ancestor_id).is_some() {
                    self.enqueue_preview_node(ancestor_id);
                }
                current = updated_tree
                    .nodes
                    .get(&ancestor_id)
                    .and_then(|node| node.parent_id);
            }
        }
    }

    fn enqueue_all_preview_nodes(&mut self, tree: &FlatRenderTree) {
        for node_id in tree.nodes.keys().copied() {
            if preview_source_image(tree, node_id).is_some() {
                self.enqueue_preview_node(node_id);
            }
        }
    }

    fn enqueue_dirty_preview_nodes(&mut self, tree: &FlatRenderTree, dirty_node_ids: &[NodeId]) {
        for &dirty_node_id in dirty_node_ids {
            let mut current = Some(dirty_node_id);
            while let Some(node_id) = current {
                if preview_source_image(tree, node_id).is_some() {
                    self.enqueue_preview_node(node_id);
                }
                current = tree.nodes.get(&node_id).and_then(|node| node.parent_id);
            }
        }
    }

    fn enqueue_preview_node(&mut self, node_id: NodeId) {
        if self.pending_preview_nodes.contains(&node_id) {
            return;
        }
        self.pending_preview_nodes.push(node_id);
    }

    fn process_layer_previews(&mut self, tree: &FlatRenderTree) -> bool {
        if self.pending_preview_nodes.is_empty() {
            return false;
        }

        let pending = std::mem::take(&mut self.pending_preview_nodes);
        let mut blocked = Vec::new();
        let mut produced = false;
        for node_id in pending {
            if self.blocked_preview_nodes.contains(&node_id) {
                blocked.push(node_id);
                continue;
            }
            let Some(image) = preview_source_image(tree, node_id) else {
                continue;
            };
            match self.layer_preview_renderer.render(
                &self.gpu_context.device,
                &self.gpu_context.queue,
                &self.atlas_storage,
                node_id,
                PreviewSource { image },
            ) {
                Ok(Some(bitmap)) => {
                    self.layer_preview_updates.push(bitmap);
                    produced = true;
                }
                Ok(None) => {}
                Err(error) => {
                    eprintln!(
                        "Layer preview render failed for node {}: {error:?}",
                        node_id.0
                    );
                }
            }
        }
        self.pending_preview_nodes = blocked;
        produced
    }

    pub fn save_screenshot(
        &mut self,
        output_path: &Path,
        width: u32,
        height: u32,
    ) -> Result<(), ScreenshotError> {
        self.promote_completed_tile_updates();
        if width == 0 || height == 0 {
            return Err(ScreenshotError::InvalidSize);
        }
        if let Some(parent_dir) = output_path.parent() {
            if !parent_dir.as_os_str().is_empty() {
                std::fs::create_dir_all(parent_dir).map_err(ScreenshotError::Io)?;
            }
        }

        let screenshot_texture = self
            .gpu_context
            .device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("glaphica-e2e-screenshot-texture"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
        let screenshot_view =
            screenshot_texture.create_view(&wgpu::TextureViewDescriptor::default());

        let tree = self.shared_tree.read();
        self.screen_blitter.blit(
            &self.gpu_context.device,
            &self.gpu_context.queue,
            &self.atlas_storage,
            &tree,
            &self.view,
            &screenshot_view,
            wgpu::TextureFormat::Rgba8Unorm,
            width,
            height,
        );

        let bytes_per_pixel = 4u32;
        let unpadded_bytes_per_row = width.saturating_mul(bytes_per_pixel);
        let padded_bytes_per_row = unpadded_bytes_per_row.div_ceil(256).saturating_mul(256);
        let output_buffer_size = u64::from(padded_bytes_per_row) * u64::from(height);

        let output_buffer = self
            .gpu_context
            .device
            .create_buffer(&wgpu::BufferDescriptor {
                label: Some("glaphica-e2e-screenshot-readback-buffer"),
                size: output_buffer_size,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });

        let mut encoder =
            self.gpu_context
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("glaphica-e2e-screenshot-readback-encoder"),
                });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &screenshot_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &output_buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_bytes_per_row),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        self.gpu_context.queue.submit(Some(encoder.finish()));

        let buffer_slice = output_buffer.slice(..);
        let (result_sender, result_receiver) = std::sync::mpsc::channel();
        buffer_slice.map_async(wgpu::MapMode::Read, move |result| {
            if let Err(send_error) = result_sender.send(result) {
                eprintln!("screenshot map callback send failed: {send_error}");
            }
        });
        let _ = self
            .gpu_context
            .device
            .poll(wgpu::PollType::wait_indefinitely());
        let map_result = result_receiver
            .recv()
            .map_err(ScreenshotError::MapChannel)?;
        map_result.map_err(ScreenshotError::Map)?;

        let mapped_range = buffer_slice.get_mapped_range();
        let unpadded_row_len =
            usize::try_from(unpadded_bytes_per_row).map_err(|_| ScreenshotError::InvalidSize)?;
        let padded_row_len =
            usize::try_from(padded_bytes_per_row).map_err(|_| ScreenshotError::InvalidSize)?;
        let height_usize = usize::try_from(height).map_err(|_| ScreenshotError::InvalidSize)?;
        let mut pixels = vec![0u8; unpadded_row_len.saturating_mul(height_usize)];
        for row_index in 0..height_usize {
            let source_start = row_index * padded_row_len;
            let source_end = source_start + unpadded_row_len;
            let destination_start = row_index * unpadded_row_len;
            let destination_end = destination_start + unpadded_row_len;
            pixels[destination_start..destination_end]
                .copy_from_slice(&mapped_range[source_start..source_end]);
        }
        drop(mapped_range);
        output_buffer.unmap();

        let file = std::fs::File::create(output_path).map_err(ScreenshotError::Io)?;
        let mut encoder = png::Encoder::new(file, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(ScreenshotError::Png)?;
        writer
            .write_image_data(&pixels)
            .map_err(ScreenshotError::Png)?;
        Ok(())
    }
}

fn to_wgpu_texture_format(format: TextureFormat) -> wgpu::TextureFormat {
    match format {
        TextureFormat::Rgba8Unorm => wgpu::TextureFormat::Rgba8Unorm,
        TextureFormat::Rgba16Float => wgpu::TextureFormat::Rgba16Float,
        TextureFormat::Bgra8Unorm => wgpu::TextureFormat::Bgra8Unorm,
        TextureFormat::R8Unorm => wgpu::TextureFormat::R8Unorm,
        TextureFormat::Rg8Unorm => wgpu::TextureFormat::Rg8Unorm,
    }
}

fn summarize_dirty_tracker(
    tree: &FlatRenderTree,
    dirty: &ImageDirtyTracker,
) -> (usize, usize, usize, usize) {
    let mut by_node = HashMap::<NodeId, DirtyNodeBounds>::new();
    let mut dirty_tile_count = 0usize;

    for key in dirty.iter() {
        let Some(node) = tree.nodes.get(&key.node_id) else {
            continue;
        };
        let Some(image) = node.kind.render_image() else {
            continue;
        };
        let layout = image.layout();
        let tile_x = layout.tile_x();
        let tile_index = key.tile_index as u32;
        let tile_coord_x = tile_index % tile_x;
        let tile_coord_y = tile_index / tile_x;
        let entry = by_node.entry(key.node_id).or_default();
        if entry.has_any {
            entry.min_x = entry.min_x.min(tile_coord_x);
            entry.min_y = entry.min_y.min(tile_coord_y);
            entry.max_x = entry.max_x.max(tile_coord_x);
            entry.max_y = entry.max_y.max(tile_coord_y);
        } else {
            entry.min_x = tile_coord_x;
            entry.min_y = tile_coord_y;
            entry.max_x = tile_coord_x;
            entry.max_y = tile_coord_y;
            entry.has_any = true;
        }
        dirty_tile_count += 1;
    }

    let dirty_rect_count = by_node.len();
    let dirty_bbox_tile_area = by_node
        .values()
        .map(|bounds| {
            let width = bounds.max_x - bounds.min_x + 1;
            let height = bounds.max_y - bounds.min_y + 1;
            (width as usize) * (height as usize)
        })
        .sum();
    (
        dirty_tile_count,
        dirty_rect_count,
        dirty_bbox_tile_area,
        by_node.len(),
    )
}

fn preview_source_image(tree: &FlatRenderTree, node_id: NodeId) -> Option<&images::Image> {
    let node = tree.nodes.get(&node_id)?;
    match &node.kind {
        document::FlatNodeKind::Branch { render_cache, .. } => Some(render_cache),
        document::FlatNodeKind::Leaf { content } => match content {
            document::FlatLeafContent::Raster { image } => Some(image),
            document::FlatLeafContent::Parametric { .. } => None,
        },
    }
}

fn trace_gpu_commands(commands: &[GpuCmdMsg], max_commands: usize) {
    eprintln!("[PERF][gpu_cmd_trace] frame_cmd_count={}", commands.len());
    for (index, cmd) in commands.iter().take(max_commands).enumerate() {
        match cmd {
            GpuCmdMsg::DrawOp(op) => {
                eprintln!(
                    "[PERF][gpu_cmd_trace][{}] DrawOp node={} tile_index={} tile_key={:?} origin_tile={:?} ref_tile={:?} input_len={}",
                    index,
                    op.node_id.0,
                    op.tile_index,
                    op.tile_key,
                    op.origin_tile,
                    op.ref_image.map(|ref_image| ref_image.tile_key),
                    op.input.len()
                );
            }
            GpuCmdMsg::CopyOp(op) => {
                eprintln!(
                    "[PERF][gpu_cmd_trace][{}] CopyOp src={:?} dst={:?}",
                    index, op.src_tile_key, op.dst_tile_key
                );
            }
            GpuCmdMsg::WriteOp(op) => {
                eprintln!(
                    "[PERF][gpu_cmd_trace][{}] WriteOp src={:?} dst={:?}",
                    index, op.src_tile_key, op.dst_tile_key
                );
            }
            GpuCmdMsg::CompositeOp(op) => {
                eprintln!(
                    "[PERF][gpu_cmd_trace][{}] CompositeOp base={:?} overlay={:?} dst={:?} opacity={:.3}",
                    index, op.base_tile_key, op.overlay_tile_key, op.dst_tile_key, op.opacity
                );
            }
            GpuCmdMsg::ClearOp(op) => {
                eprintln!(
                    "[PERF][gpu_cmd_trace][{}] ClearOp tile={:?}",
                    index, op.tile_key
                );
            }
            GpuCmdMsg::RenderTreeUpdated(op) => {
                eprintln!(
                    "[PERF][gpu_cmd_trace][{}] RenderTreeUpdated generation={} dirty_render_caches={}",
                    index,
                    op.generation.0,
                    op.dirty_render_caches.len()
                );
            }
            GpuCmdMsg::TileSlotKeyUpdate(op) => {
                eprintln!(
                    "[PERF][gpu_cmd_trace][{}] TileSlotKeyUpdate updates={}",
                    index,
                    op.updates.len()
                );
            }
        }
    }
    if commands.len() > max_commands {
        eprintln!(
            "[PERF][gpu_cmd_trace] omitted={} (increase GLAPHICA_DEBUG_GPU_CMD_TRACE_MAX to show more)",
            commands.len() - max_commands
        );
    }
}

#[derive(Debug)]
pub enum InitError {
    GpuContext(gpu_runtime::GpuContextInitError),
    Atlas(gpu_runtime::atlas_runtime::AtlasStorageRuntimeRegisterError),
    Document(document::ImageCreateError),
    BackendManager(atlas::AtlasBackendManagerError),
}

#[derive(Debug)]
pub enum BrushRegisterError {
    Engine(BrushRegistryError),
    Layout(BrushRegistryError),
    Pipeline(BrushRegistryError),
    Executor(WgpuBrushExecutorError),
    CacheBackendAlloc { brush_id: BrushId },
}

#[derive(Debug)]
pub enum PresentError {
    NoSurface,
    Surface(SurfaceError),
}

#[derive(Debug)]
pub enum ScreenshotError {
    InvalidSize,
    Io(std::io::Error),
    Map(wgpu::BufferAsyncError),
    MapChannel(std::sync::mpsc::RecvError),
    Png(png::EncodingError),
}

impl Display for ScreenshotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidSize => write!(f, "invalid screenshot size"),
            Self::Io(error) => write!(f, "screenshot io error: {error}"),
            Self::Map(error) => write!(f, "screenshot map error: {error}"),
            Self::MapChannel(error) => write!(f, "screenshot map channel error: {error}"),
            Self::Png(error) => write!(f, "screenshot png error: {error}"),
        }
    }
}

impl std::error::Error for ScreenshotError {}

#[cfg(test)]
mod tests {
    use super::compact_round_draws;
    use brushes::builtin_brushes::round::ROUND_DRAW_LAYOUT;
    use glaphica_core::{BrushId, NodeId, StrokeId, TileKey};
    use thread_protocol::{DrawBlendMode, DrawFrameMergePolicy, DrawOp, GpuCmdMsg};

    #[test]
    fn compact_round_draws_merges_same_tile_inputs() {
        let tile_key = TileKey::from_parts(2, 0, 9);
        let draw = |input: Vec<f32>| {
            GpuCmdMsg::DrawOp(DrawOp {
                node_id: NodeId(1),
                tile_index: 3,
                tile_key,
                blend_mode: DrawBlendMode::Additive,
                frame_merge: DrawFrameMergePolicy::None,
                origin_tile: TileKey::EMPTY,
                ref_image: None,
                input,
                rgb: [1.0, 0.0, 0.0],
                erase: false,
                brush_id: BrushId(2),
                stroke_id: StrokeId(4),
            })
        };

        let commands = vec![draw(vec![1.0; 6]), draw(vec![2.0; 6])];
        let layouts = vec![Some(ROUND_DRAW_LAYOUT), Some(ROUND_DRAW_LAYOUT)];
        let (commands, layouts) = compact_round_draws(&commands, &layouts);

        assert_eq!(commands.len(), 1);
        assert_eq!(layouts.len(), 1);
        let GpuCmdMsg::DrawOp(draw_op) = &commands[0] else {
            panic!("expected merged draw op");
        };
        assert_eq!(draw_op.input.len(), 12);
        assert_eq!(&draw_op.input[..6], &[1.0; 6]);
        assert_eq!(&draw_op.input[6..], &[2.0; 6]);
    }
}
