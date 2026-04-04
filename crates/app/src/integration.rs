use std::collections::{HashMap, VecDeque};
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use brushes::builtin_brushes::{pixel_rect::PixelRectBrush, round::RoundBrush};
use brushes::{BrushConfigValue, BrushResamplerDistance, BrushResamplerDistancePolicy, BrushSpec};
use document::{
    Document, DocumentStorageError, DocumentStorageManifest, FlatRenderTree, LayerMoveTarget,
    NewLayerKind, SharedRenderTree, UiBlendMode, UiLayerTreeItem,
};
use flate2::{Compression, read::GzDecoder, write::GzEncoder};
use glaphica_core::{AtlasLayout, BrushId, NodeId, StrokeId};
use gpu_runtime::surface_runtime::SurfaceRuntime;
use images::StoredImage;
use images::layout::ImageLayout;
use serde::{Deserialize, Serialize};
use thread_protocol::{
    DrawFrameMergePolicy, GpuCmdFrameMergeTag, GpuCmdMsg, GpuFeedbackFrame, InputControlEvent,
    InputControlOp, InputRingSample, MergeItem, MergeVecIndex, TileKey,
};
use threads::{EngineThreadChannels, MainThreadChannels, create_thread_channels};

use crate::trace::{
    TraceAppControl, TraceDrawCompaction, TraceInputFrame, TraceIoError, TraceRecorder,
    TraceRuntimeTileEvent, TraceRuntimeTileStage,
};
use crate::{
    BrushRegisterError, EngineThreadState, ExportImageError, LayerImageExportError,
    LayerPreviewBitmap, MainThreadState, config,
};

#[derive(Debug)]
pub enum DocumentPackageError {
    Io(std::io::Error),
    Json(serde_json::Error),
    PngDecode(png::DecodingError),
    PngEncode(png::EncodingError),
    Storage(DocumentStorageError),
    LayerExport(LayerImageExportError),
    MissingRasterNode { node_id: NodeId },
    TileAlloc { node_id: NodeId, tile_index: usize },
    TileUpload { node_id: NodeId, tile_index: usize },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct PackedDocumentFile {
    manifest: DocumentStorageManifest,
    layers: Vec<PackedLayerAsset>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PackedLayerAsset {
    node_id: u64,
    file_name: String,
    png_bytes: Vec<u8>,
}

impl From<std::io::Error> for DocumentPackageError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for DocumentPackageError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

impl From<png::DecodingError> for DocumentPackageError {
    fn from(error: png::DecodingError) -> Self {
        Self::PngDecode(error)
    }
}

impl From<png::EncodingError> for DocumentPackageError {
    fn from(error: png::EncodingError) -> Self {
        Self::PngEncode(error)
    }
}

impl From<DocumentStorageError> for DocumentPackageError {
    fn from(error: DocumentStorageError) -> Self {
        Self::Storage(error)
    }
}

impl From<LayerImageExportError> for DocumentPackageError {
    fn from(error: LayerImageExportError) -> Self {
        Self::LayerExport(error)
    }
}

impl Display for DocumentPackageError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "document package io error: {error}"),
            Self::Json(error) => write!(f, "document package json error: {error}"),
            Self::PngDecode(error) => write!(f, "document package png decode error: {error}"),
            Self::PngEncode(error) => write!(f, "document package png encode error: {error}"),
            Self::Storage(error) => write!(f, "document package storage error: {error:?}"),
            Self::LayerExport(error) => write!(f, "document package layer export error: {error:?}"),
            Self::MissingRasterNode { node_id } => {
                write!(f, "document package missing raster node {}", node_id.0)
            }
            Self::TileAlloc {
                node_id,
                tile_index,
            } => write!(
                f,
                "document package tile allocation failed for node {} tile {}",
                node_id.0, tile_index
            ),
            Self::TileUpload {
                node_id,
                tile_index,
            } => write!(
                f,
                "document package tile upload failed for node {} tile {}",
                node_id.0, tile_index
            ),
        }
    }
}

impl Error for DocumentPackageError {}

#[derive(Debug, Clone, PartialEq)]
pub enum AppControl {
    StrokeBoundary {
        node_id: NodeId,
        begin: bool,
    },
    SelectNode {
        node_id: NodeId,
    },
    CreateLayerAboveActive {
        kind: NewLayerKind,
    },
    CreateGroupAboveActive,
    DeleteNode {
        node_id: NodeId,
    },
    MoveNode {
        node_id: NodeId,
        target: LayerMoveTarget,
    },
    SetNodeVisibility {
        node_id: NodeId,
        visible: bool,
    },
    SetNodeOpacity {
        node_id: NodeId,
        opacity: f32,
    },
    SetNodeBlendMode {
        node_id: NodeId,
        blend_mode: UiBlendMode,
    },
    SetActiveBrush {
        brush_id: BrushId,
    },
    SetActiveBrushColorRgb {
        rgb: [f32; 3],
    },
    SetActiveBrushErase {
        erase: bool,
    },
    UpdateBrushConfig {
        brush_id: BrushId,
        values: Vec<BrushConfigValue>,
    },
    MoveActiveNodeUp,
    MoveActiveNodeDown,
}

impl InputControlOp for AppControl {
    type Target = Option<NodeId>;
    type Serialized = TraceAppControl;

    fn apply(&self, target: &mut Self::Target) {
        if let Self::StrokeBoundary { node_id, begin } = self {
            if *begin {
                *target = Some(*node_id);
            } else {
                *target = None;
            }
        }
    }

    fn undo(&self, target: &mut Self::Target) {
        if let Self::StrokeBoundary { node_id, begin } = self {
            if *begin {
                *target = None;
            } else {
                *target = Some(*node_id);
            }
        }
    }

    fn to_serialized(&self) -> Option<Self::Serialized> {
        Some(TraceAppControl::from(self.clone()))
    }

    fn from_serialized(value: Self::Serialized) -> Option<Self> {
        Some(Self::from(value))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TileAllocReceipt {
    pub old_tile_key: TileKey,
    pub new_tile_key: TileKey,
}

impl MergeItem for TileAllocReceipt {
    type MergeKey = TileKey;

    fn merge_key(&self) -> Self::MergeKey {
        self.old_tile_key
    }

    fn merge_duplicate(existing: &mut Self, incoming: Self) {
        *existing = incoming;
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GpuError {
    pub key: u64,
    pub message: String,
}

impl MergeItem for GpuError {
    type MergeKey = u64;

    fn merge_key(&self) -> Self::MergeKey {
        self.key
    }

    fn merge_duplicate(_existing: &mut Self, _incoming: Self) {}
}

type AppMainThreadChannels = MainThreadChannels<AppControl, TileAllocReceipt, GpuError>;
type AppEngineThreadChannels = EngineThreadChannels<AppControl, TileAllocReceipt, GpuError>;
type AppGpuFeedbackFrame = GpuFeedbackFrame<TileAllocReceipt, GpuError>;

struct AppGpuFeedbackMergeState {
    receipt_index: MergeVecIndex<TileKey>,
    error_index: MergeVecIndex<u64>,
}

impl Default for AppGpuFeedbackMergeState {
    fn default() -> Self {
        Self {
            receipt_index: MergeVecIndex::default(),
            error_index: MergeVecIndex::default(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct PerfTraceConfig {
    enabled: bool,
    slow_threshold: Duration,
}

impl PerfTraceConfig {
    fn from_env() -> Self {
        let enabled = std::env::var("GLAPHICA_DEBUG_PIPELINE_TRACE")
            .ok()
            .is_some_and(|value| value != "0");
        let slow_threshold_ms = std::env::var("GLAPHICA_DEBUG_PIPELINE_SLOW_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(4);
        Self {
            enabled,
            slow_threshold: Duration::from_millis(slow_threshold_ms),
        }
    }
}

#[derive(Default)]
struct EngineFramePerf {
    input_sample: Duration,
    smooth_and_resample: Duration,
    brush_handling: Duration,
    send_to_app_thread: Duration,
    send_inline_submit: Duration,
    accept_by_app_thread: Duration,
    submit_to_gpu: Duration,
    submit_collect_render_tree: Duration,
    submit_materialize_parametric: Duration,
    submit_composite_render_tree: Duration,
    submit_queue: Duration,
    sample_count: usize,
    brush_input_count: usize,
    generated_gpu_command_count: usize,
    inline_submitted_gpu_command_count: usize,
    inline_submit_batches: usize,
    pending_send_gpu_command_count: usize,
    gpu_command_count: usize,
    submit_parametric_cmd_count: usize,
    submit_parametric_tile_count: usize,
    submit_render_cmd_count: usize,
    submit_render_tile_count: usize,
    submit_render_source_count: usize,
    submit_dirty_tile_count: usize,
    submit_dirty_rect_count: usize,
    submit_dirty_bbox_tile_area: usize,
    submit_dirty_node_count: usize,
}

pub struct AppThreadIntegration {
    main_state: MainThreadState,
    engine_state: EngineThreadState,
    main_channels: AppMainThreadChannels,
    engine_channels: AppEngineThreadChannels,
    input_controls: Vec<InputControlEvent<AppControl>>,
    input_samples: Vec<InputRingSample>,
    brush_inputs: Vec<glaphica_core::BrushInput>,
    gpu_commands: Vec<thread_protocol::GpuCmdMsg>,
    pending_send_gpu_commands: VecDeque<thread_protocol::GpuCmdMsg>,
    feedback_merge_state: AppGpuFeedbackMergeState,
    trace_recorder: Option<TraceRecorder>,
    active_stroke_node: Option<NodeId>,
    current_brush_id: Option<BrushId>,
    active_stroke_brush_id: Option<BrushId>,
    streaming_compact_stroke_id: Option<StrokeId>,
    current_brush_color_rgb: [f32; 3],
    current_brush_erase: bool,
    active_stroke_color_rgb: [f32; 3],
    active_stroke_erase: bool,
    brush_resampler_distances: Vec<Option<BrushResamplerDistance>>,
    next_stroke_id: u64,
    perf_trace: PerfTraceConfig,
    perf_frame_seq: u64,
    document_layout: ImageLayout,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppStats {
    pub backend_tiles: Vec<atlas::BackendTileStats>,
    pub undo_action_count: usize,
}

impl AppThreadIntegration {
    fn collect_unique_draw_tile_indices(commands: &[GpuCmdMsg]) -> Vec<usize> {
        let mut indices = commands
            .iter()
            .filter_map(|cmd| match cmd {
                GpuCmdMsg::DrawOp(draw_op) => Some(draw_op.tile_index),
                _ => None,
            })
            .collect::<Vec<_>>();
        indices.sort_unstable();
        indices.dedup();
        indices
    }

    fn should_merge_draw_in_frame(draw_op: &thread_protocol::DrawOp) -> bool {
        draw_op.stroke_ctx.is_some_and(|ctx| {
            ctx.frame_merge == DrawFrameMergePolicy::KeepLastInFrameByNodeTileBrush
        })
    }

    fn should_keep_first_copy_in_frame(copy_op: &thread_protocol::CopyOp) -> bool {
        copy_op.frame_merge == GpuCmdFrameMergeTag::KeepFirstInFrameByDstTile
    }

    fn should_keep_last_write_in_frame(write_op: &thread_protocol::WriteOp) -> bool {
        write_op.frame_merge == GpuCmdFrameMergeTag::KeepLastInFrameByDstTile
    }

    fn compact_frame_mergeable_draws(commands: &mut Vec<GpuCmdMsg>) {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        struct CompositeKey {
            node_id: NodeId,
            tile_index: usize,
            tile_key: TileKey,
            brush_id: BrushId,
        }

        let mut latest_composite_indices: HashMap<CompositeKey, usize> = HashMap::new();
        for (index, cmd) in commands.iter().enumerate() {
            let GpuCmdMsg::DrawOp(draw_op) = cmd else {
                continue;
            };
            if !Self::should_merge_draw_in_frame(draw_op) {
                continue;
            }
            let Some(ctx) = draw_op.stroke_ctx else {
                debug_assert!(false, "draw op must carry stroke ctx before compaction");
                continue;
            };
            latest_composite_indices.insert(
                CompositeKey {
                    node_id: ctx.node_id,
                    tile_index: draw_op.tile_index,
                    tile_key: draw_op.tile_key,
                    brush_id: ctx.brush_id,
                },
                index,
            );
        }

        if latest_composite_indices.is_empty() {
            return;
        }

        let mut compacted = Vec::with_capacity(commands.len());
        for (index, cmd) in commands.drain(..).enumerate() {
            let keep = match &cmd {
                GpuCmdMsg::DrawOp(draw_op) if Self::should_merge_draw_in_frame(draw_op) => {
                    match draw_op.stroke_ctx {
                        Some(ctx) => {
                            latest_composite_indices
                                .get(&CompositeKey {
                                    node_id: ctx.node_id,
                                    tile_index: draw_op.tile_index,
                                    tile_key: draw_op.tile_key,
                                    brush_id: ctx.brush_id,
                                })
                                .copied()
                                == Some(index)
                        }
                        None => {
                            debug_assert!(false, "draw op must carry stroke ctx before compaction");
                            false
                        }
                    }
                }
                _ => true,
            };
            if keep {
                compacted.push(cmd);
            }
        }
        *commands = compacted;
    }

    fn compact_frame_mergeable_copy_write(commands: &mut Vec<GpuCmdMsg>) {
        let mut first_copy_indices: HashMap<TileKey, usize> = HashMap::new();
        let mut last_write_indices: HashMap<TileKey, usize> = HashMap::new();

        for (index, cmd) in commands.iter().enumerate() {
            match cmd {
                GpuCmdMsg::CopyOp(copy_op) if Self::should_keep_first_copy_in_frame(copy_op) => {
                    first_copy_indices
                        .entry(copy_op.dst_tile_key)
                        .or_insert(index);
                }
                GpuCmdMsg::WriteOp(write_op) if Self::should_keep_last_write_in_frame(write_op) => {
                    last_write_indices.insert(write_op.dst_tile_key, index);
                }
                _ => {}
            }
        }

        if first_copy_indices.is_empty() && last_write_indices.is_empty() {
            return;
        }

        let mut compacted = Vec::with_capacity(commands.len());
        for (index, cmd) in commands.drain(..).enumerate() {
            let keep = match &cmd {
                GpuCmdMsg::CopyOp(copy_op) if Self::should_keep_first_copy_in_frame(copy_op) => {
                    first_copy_indices.get(&copy_op.dst_tile_key).copied() == Some(index)
                }
                GpuCmdMsg::WriteOp(write_op) if Self::should_keep_last_write_in_frame(write_op) => {
                    last_write_indices.get(&write_op.dst_tile_key).copied() == Some(index)
                }
                _ => true,
            };
            if keep {
                compacted.push(cmd);
            }
        }
        *commands = compacted;
    }

    fn move_mergeable_writes_to_end(commands: &mut Vec<GpuCmdMsg>) {
        let mut non_writes = Vec::with_capacity(commands.len());
        let mut deferred_writes = Vec::new();

        for cmd in commands.drain(..) {
            match &cmd {
                // After frame compaction only the final write to each destination remains.
                // Delaying these writes preserves the final image while keeping the buffered
                // stroke draw phase contiguous enough for GPU batching.
                GpuCmdMsg::WriteOp(write_op) if Self::should_keep_last_write_in_frame(write_op) => {
                    deferred_writes.push(cmd);
                }
                _ => non_writes.push(cmd),
            }
        }

        if deferred_writes.is_empty() {
            *commands = non_writes;
            return;
        }

        non_writes.extend(deferred_writes);
        *commands = non_writes;
    }

    fn move_setup_ops_before_draws(commands: &mut Vec<GpuCmdMsg>) {
        let mut setup_ops = Vec::with_capacity(commands.len());
        let mut draw_ops = Vec::new();
        let mut other_ops = Vec::new();

        for cmd in commands.drain(..) {
            match &cmd {
                // Buffered stroke setup must finish before the batched draw phase starts, otherwise
                // buffer clears and origin copies would race with packed round draws.
                GpuCmdMsg::ClearOp(_) => setup_ops.push(cmd),
                GpuCmdMsg::CopyOp(copy_op) if Self::should_keep_first_copy_in_frame(copy_op) => {
                    setup_ops.push(cmd);
                }
                GpuCmdMsg::DrawOp(_) => draw_ops.push(cmd),
                _ => other_ops.push(cmd),
            }
        }

        if draw_ops.is_empty() {
            setup_ops.extend(other_ops);
            *commands = setup_ops;
            return;
        }

        setup_ops.extend(draw_ops);
        setup_ops.extend(other_ops);
        *commands = setup_ops;
    }

    fn move_metadata_updates_to_end(commands: &mut Vec<GpuCmdMsg>) {
        let mut gpu_commands = Vec::with_capacity(commands.len());
        let mut deferred_updates = Vec::new();

        for cmd in commands.drain(..) {
            match &cmd {
                GpuCmdMsg::TileSlotKeyUpdate(_) | GpuCmdMsg::RenderTreeUpdated(_) => {
                    deferred_updates.push(cmd);
                }
                _ => gpu_commands.push(cmd),
            }
        }

        if deferred_updates.is_empty() {
            *commands = gpu_commands;
            return;
        }

        gpu_commands.extend(deferred_updates);
        *commands = gpu_commands;
    }

    fn compact_draw_ops_by_stroke_ctx(
        commands: &mut Vec<GpuCmdMsg>,
        streaming_compact_stroke_id: &mut Option<StrokeId>,
    ) {
        for cmd in commands.iter_mut() {
            let GpuCmdMsg::DrawOp(draw_op) = cmd else {
                continue;
            };
            if *streaming_compact_stroke_id != Some(draw_op.stroke_id) {
                *streaming_compact_stroke_id = Some(draw_op.stroke_id);
                continue;
            }
            draw_op.stroke_ctx = None;
        }
    }

    pub async fn new(document_name: String, layout: ImageLayout) -> Result<Self, crate::InitError> {
        let gpu_context = Arc::new(
            gpu_runtime::GpuContext::init(&gpu_runtime::GpuContextInitDescriptor::default())
                .await
                .map_err(crate::InitError::GpuContext)?,
        );
        Self::new_with_gpu_context(document_name, layout, gpu_context).await
    }

    pub async fn new_with_gpu_context(
        document_name: String,
        layout: ImageLayout,
        gpu_context: Arc<gpu_runtime::GpuContext>,
    ) -> Result<Self, crate::InitError> {
        let mut main_state = MainThreadState::init_with_gpu_context(gpu_context).await?;

        let document = Document::new(
            document_name,
            layout,
            glaphica_core::BackendId::new(0),
            glaphica_core::BackendId::new(1),
        )
        .map_err(crate::InitError::Document)?;

        let shared_tree = Arc::new(SharedRenderTree::new(FlatRenderTree {
            generation: glaphica_core::RenderTreeGeneration(0),
            nodes: Arc::new(HashMap::new()),
            root_id: None,
        }));

        main_state.set_shared_tree(shared_tree.clone());

        let mut engine_state = EngineThreadState::new(
            document,
            shared_tree,
            crate::config::brush_processing::MAX_BRUSHES,
        );

        // Add backends to the engine thread's backend manager to match main thread
        engine_state
            .backend_manager_mut()
            .add_backend(AtlasLayout::Small11)
            .expect("failed to add leaf backend to engine");
        engine_state
            .backend_manager_mut()
            .add_backend(AtlasLayout::Small11)
            .expect("failed to add render cache backend to engine");
        let initial_render_tree = engine_state
            .rebuild_render_tree()
            .map_err(crate::InitError::Document)?;
        let _ = main_state.process_gpu_commands(&[thread_protocol::GpuCmdMsg::RenderTreeUpdated(
            initial_render_tree,
        )]);

        let (main_channels, engine_channels) = create_thread_channels(
            config::thread_channels::MAIN_TO_ENGINE_INPUT_RING,
            config::thread_channels::ENGINE_TO_MAIN_INPUT_CONTROL,
            config::thread_channels::ENGINE_TO_MAIN_GPU_COMMAND,
            config::thread_channels::MAIN_TO_ENGINE_FEEDBACK,
        );

        Ok(Self {
            main_state,
            engine_state,
            main_channels,
            engine_channels,
            input_controls: Vec::with_capacity(config::batch_capacities::INPUT_SAMPLES),
            input_samples: Vec::with_capacity(config::batch_capacities::INPUT_SAMPLES),
            brush_inputs: Vec::with_capacity(config::batch_capacities::BRUSH_INPUTS),
            gpu_commands: Vec::with_capacity(config::batch_capacities::GPU_COMMANDS),
            pending_send_gpu_commands: VecDeque::with_capacity(
                config::batch_capacities::GPU_COMMANDS,
            ),
            feedback_merge_state: AppGpuFeedbackMergeState::default(),
            trace_recorder: None,
            active_stroke_node: None,
            current_brush_id: None,
            active_stroke_brush_id: None,
            streaming_compact_stroke_id: None,
            current_brush_color_rgb: [1.0, 0.0, 0.0],
            current_brush_erase: false,
            active_stroke_color_rgb: [1.0, 0.0, 0.0],
            active_stroke_erase: false,
            brush_resampler_distances: vec![None; config::brush_processing::MAX_BRUSHES],
            next_stroke_id: 1,
            perf_trace: PerfTraceConfig::from_env(),
            perf_frame_seq: 0,
            document_layout: layout,
        })
    }

    pub fn document_size(&self) -> (u32, u32) {
        (self.document_layout.size_x(), self.document_layout.size_y())
    }

    pub fn main_state(&self) -> &MainThreadState {
        &self.main_state
    }

    pub fn main_state_mut(&mut self) -> &mut MainThreadState {
        &mut self.main_state
    }

    pub fn engine_state(&self) -> &EngineThreadState {
        &self.engine_state
    }

    pub fn engine_state_mut(&mut self) -> &mut EngineThreadState {
        &mut self.engine_state
    }

    pub fn push_input_sample(&self, sample: InputRingSample) {
        self.main_channels.input_ring_producer.push(sample);
    }

    pub fn map_screen_to_document(&self, screen_x: f32, screen_y: f32) -> (f32, f32) {
        self.main_state
            .view()
            .screen_to_document(screen_x, screen_y)
    }

    pub fn map_document_to_screen(&self, doc_x: f32, doc_y: f32) -> (f32, f32) {
        self.main_state.view().document_to_screen(doc_x, doc_y)
    }

    pub fn pan_view(&mut self, dx: f32, dy: f32) {
        self.main_state.view_mut().pan(dx, dy);
    }

    pub fn zoom_view(&mut self, factor: f32, center_x: f32, center_y: f32) {
        self.main_state.view_mut().zoom(factor, center_x, center_y);
    }

    pub fn rotate_view(&mut self, delta_radians: f32, center_x: f32, center_y: f32) {
        self.main_state
            .view_mut()
            .rotate(delta_radians, center_x, center_y);
    }

    pub fn begin_stroke(&mut self, node_id: NodeId) {
        let control = AppControl::StrokeBoundary {
            node_id,
            begin: true,
        };
        self.main_channels
            .input_control_queue
            .blocking_push(InputControlEvent::Control(control));
        self.active_stroke_node = Some(node_id);
        self.active_stroke_brush_id = self.current_brush_id;
        self.streaming_compact_stroke_id = None;
        self.active_stroke_color_rgb = self.current_brush_color_rgb;
        self.active_stroke_erase = self.current_brush_erase;
    }

    pub fn active_document_node(&self) -> Option<NodeId> {
        self.engine_state.document().selected_node()
    }

    pub fn active_paint_node(&self) -> Option<NodeId> {
        self.engine_state.document().active_paint_node()
    }

    pub fn layer_tree_items(&self) -> Vec<UiLayerTreeItem> {
        self.engine_state.document().layer_tree_items()
    }

    pub fn take_layer_preview_updates(&mut self) -> Vec<LayerPreviewBitmap> {
        self.main_state.take_layer_preview_updates()
    }

    pub fn select_document_node(&mut self, node_id: NodeId) -> bool {
        if !self.engine_state.document().can_select_node(node_id) {
            return false;
        }
        self.main_channels
            .input_control_queue
            .blocking_push(InputControlEvent::Control(AppControl::SelectNode {
                node_id,
            }));
        true
    }

    pub fn create_layer_above_active(
        &mut self,
        kind: NewLayerKind,
    ) -> Result<(), document::LayerEditError> {
        if self.engine_state.document().selected_node().is_none() {
            return Err(document::LayerEditError::NoActiveNode);
        }
        self.main_channels
            .input_control_queue
            .blocking_push(InputControlEvent::Control(
                AppControl::CreateLayerAboveActive { kind },
            ));
        Ok(())
    }

    pub fn create_group_above_active(&mut self) -> Result<(), document::LayerEditError> {
        if self.engine_state.document().selected_node().is_none() {
            return Err(document::LayerEditError::NoActiveNode);
        }
        self.main_channels
            .input_control_queue
            .blocking_push(InputControlEvent::Control(
                AppControl::CreateGroupAboveActive,
            ));
        Ok(())
    }

    pub fn move_document_node(
        &mut self,
        node_id: NodeId,
        target: LayerMoveTarget,
    ) -> Result<(), document::LayerEditError> {
        if !self
            .engine_state
            .document()
            .layer_tree()
            .contains_node(node_id)
        {
            return Err(document::LayerEditError::InvalidNode);
        }
        self.main_channels
            .input_control_queue
            .blocking_push(InputControlEvent::Control(AppControl::MoveNode {
                node_id,
                target,
            }));
        Ok(())
    }

    pub fn delete_document_node(
        &mut self,
        node_id: NodeId,
    ) -> Result<(), document::LayerEditError> {
        if !self
            .engine_state
            .document()
            .layer_tree()
            .contains_node(node_id)
        {
            return Err(document::LayerEditError::InvalidNode);
        }
        self.main_channels
            .input_control_queue
            .blocking_push(InputControlEvent::Control(AppControl::DeleteNode {
                node_id,
            }));
        Ok(())
    }

    pub fn set_document_node_visibility(
        &mut self,
        node_id: NodeId,
        visible: bool,
    ) -> Result<(), document::LayerEditError> {
        if !self
            .engine_state
            .document()
            .layer_tree()
            .contains_node(node_id)
        {
            return Err(document::LayerEditError::InvalidNode);
        }
        self.main_channels
            .input_control_queue
            .blocking_push(InputControlEvent::Control(AppControl::SetNodeVisibility {
                node_id,
                visible,
            }));
        Ok(())
    }

    pub fn set_document_node_opacity(
        &mut self,
        node_id: NodeId,
        opacity: f32,
    ) -> Result<(), document::LayerEditError> {
        if !self
            .engine_state
            .document()
            .layer_tree()
            .contains_node(node_id)
        {
            return Err(document::LayerEditError::InvalidNode);
        }
        self.main_channels
            .input_control_queue
            .blocking_push(InputControlEvent::Control(AppControl::SetNodeOpacity {
                node_id,
                opacity,
            }));
        Ok(())
    }

    pub fn set_document_node_blend_mode(
        &mut self,
        node_id: NodeId,
        blend_mode: UiBlendMode,
    ) -> Result<(), document::LayerEditError> {
        if !self
            .engine_state
            .document()
            .layer_tree()
            .contains_node(node_id)
        {
            return Err(document::LayerEditError::InvalidNode);
        }
        self.main_channels
            .input_control_queue
            .blocking_push(InputControlEvent::Control(AppControl::SetNodeBlendMode {
                node_id,
                blend_mode,
            }));
        Ok(())
    }

    pub fn move_active_node_up(&mut self) -> Result<(), document::LayerEditError> {
        if self.engine_state.document().selected_node().is_none() {
            return Err(document::LayerEditError::NoActiveNode);
        }
        self.main_channels
            .input_control_queue
            .blocking_push(InputControlEvent::Control(AppControl::MoveActiveNodeUp));
        Ok(())
    }

    pub fn move_active_node_down(&mut self) -> Result<(), document::LayerEditError> {
        if self.engine_state.document().selected_node().is_none() {
            return Err(document::LayerEditError::NoActiveNode);
        }
        self.main_channels
            .input_control_queue
            .blocking_push(InputControlEvent::Control(AppControl::MoveActiveNodeDown));
        Ok(())
    }

    fn set_active_brush_state(&mut self, brush_id: BrushId) {
        self.current_brush_id = Some(brush_id);
        let Some(brush_index) = usize::try_from(brush_id.0).ok() else {
            return;
        };
        let Some(Some(distance)) = self.brush_resampler_distances.get(brush_index).copied() else {
            return;
        };
        self.engine_state.set_resampler_distance(distance);
    }

    pub fn set_active_brush(&mut self, brush_id: BrushId) {
        if self.current_brush_id == Some(brush_id) {
            return;
        }
        self.set_active_brush_state(brush_id);
        self.main_channels
            .input_control_queue
            .blocking_push(InputControlEvent::Control(AppControl::SetActiveBrush {
                brush_id,
            }));
    }

    pub fn active_brush_id(&self) -> Option<BrushId> {
        self.current_brush_id
    }

    pub fn stats(&self) -> AppStats {
        let engine_stats = self.engine_state.stats();
        AppStats {
            backend_tiles: engine_stats.backend_tiles,
            undo_action_count: engine_stats.undo_action_count,
        }
    }

    pub fn set_active_brush_color_rgb(&mut self, rgb: [f32; 3]) {
        if self.current_brush_color_rgb == rgb {
            return;
        }
        self.current_brush_color_rgb = rgb;
        self.main_channels
            .input_control_queue
            .blocking_push(InputControlEvent::Control(
                AppControl::SetActiveBrushColorRgb { rgb },
            ));
    }

    pub fn set_active_brush_erase(&mut self, erase: bool) {
        if self.current_brush_erase == erase {
            return;
        }
        self.current_brush_erase = erase;
        self.main_channels
            .input_control_queue
            .blocking_push(InputControlEvent::Control(
                AppControl::SetActiveBrushErase { erase },
            ));
    }

    pub fn update_brush_from_config_values(
        &mut self,
        brush_id: BrushId,
        values: &[BrushConfigValue],
    ) -> Result<(), String> {
        if let Ok(round_brush) = RoundBrush::from_config_values(values) {
            return self
                .update_brush(brush_id, round_brush)
                .map_err(|error| format!("{error:?}"));
        }
        if let Ok(pixel_rect_brush) = PixelRectBrush::from_config_values(values) {
            return self
                .update_brush(brush_id, pixel_rect_brush)
                .map_err(|error| format!("{error:?}"));
        }
        Err("unsupported brush config values for registered brush types".to_string())
    }

    pub fn update_brush_from_config_values_and_record(
        &mut self,
        brush_id: BrushId,
        values: Vec<BrushConfigValue>,
    ) -> Result<(), String> {
        self.update_brush_from_config_values(brush_id, &values)?;
        self.main_channels
            .input_control_queue
            .blocking_push(InputControlEvent::Control(AppControl::UpdateBrushConfig {
                brush_id,
                values,
            }));
        Ok(())
    }

    pub fn end_stroke(&mut self) {
        if let Some(node_id) = self.active_stroke_node {
            let control = AppControl::StrokeBoundary {
                node_id,
                begin: false,
            };
            self.main_channels
                .input_control_queue
                .blocking_push(InputControlEvent::Control(control));
        }
        self.active_stroke_node = None;
        self.active_stroke_brush_id = None;
        self.streaming_compact_stroke_id = None;
    }

    pub fn undo_action(&mut self) -> bool {
        if self.active_stroke_node.is_some() {
            return false;
        }
        let Some(output) = self.engine_state.undo_action() else {
            return false;
        };
        self.pending_send_gpu_commands
            .extend(self.engine_state.take_pending_gpu_commands());
        if let Some(update) = output.tile_updates {
            self.pending_send_gpu_commands
                .push_back(GpuCmdMsg::TileSlotKeyUpdate(update));
        }
        if let Some(update) = output.render_tree_update {
            self.pending_send_gpu_commands
                .push_back(GpuCmdMsg::RenderTreeUpdated(update));
        }
        true
    }

    pub fn redo_action(&mut self) -> bool {
        if self.active_stroke_node.is_some() {
            return false;
        }
        let Some(output) = self.engine_state.redo_action() else {
            return false;
        };
        self.pending_send_gpu_commands
            .extend(self.engine_state.take_pending_gpu_commands());
        if let Some(update) = output.tile_updates {
            self.pending_send_gpu_commands
                .push_back(GpuCmdMsg::TileSlotKeyUpdate(update));
        }
        if let Some(update) = output.render_tree_update {
            self.pending_send_gpu_commands
                .push_back(GpuCmdMsg::RenderTreeUpdated(update));
        }
        true
    }

    pub fn process_engine_frame(&mut self, wait_timeout: std::time::Duration) -> bool {
        let mut perf = EngineFramePerf::default();
        self.input_controls.clear();
        while let Ok(event) = self.engine_channels.input_control_queue.pop() {
            self.input_controls.push(event);
        }
        let drained_controls = self.input_controls.clone();
        for event in &drained_controls {
            self.apply_input_control_event(event);
        }
        self.input_samples.clear();
        let input_sample_started = Instant::now();
        self.engine_channels
            .input_ring_consumer
            .drain_batch_with_wait(
                &mut self.input_samples,
                config::brush_processing::MAX_INPUT_BATCH_SIZE,
                wait_timeout,
            );
        self.normalize_input_sample_timestamps();
        perf.input_sample = input_sample_started.elapsed();
        perf.sample_count = self.input_samples.len();
        if let Some(trace_recorder) = &mut self.trace_recorder {
            trace_recorder.record_input_frame(&self.input_controls, &self.input_samples);
        }
        self.process_engine_frame_from_samples(Some(&mut perf))
    }

    pub fn process_replay_input_frame(&mut self, input_frame: &TraceInputFrame) -> bool {
        let mut perf = EngineFramePerf::default();
        let (controls, samples) = input_frame.to_runtime();
        self.input_controls = controls;
        self.input_samples = samples;
        perf.sample_count = self.input_samples.len();

        let replay_controls = self.input_controls.clone();
        for event in &replay_controls {
            self.apply_input_control_event(event);
        }
        self.process_engine_frame_from_samples(Some(&mut perf))
    }

    fn apply_input_control_event(&mut self, event: &InputControlEvent<AppControl>) {
        let InputControlEvent::Control(control) = event;
        match control {
            AppControl::StrokeBoundary { node_id, begin } => {
                if *begin {
                    self.streaming_compact_stroke_id = None;
                    self.engine_state.invalidate_redo_actions();
                    let stroke_id = StrokeId(self.next_stroke_id);
                    self.next_stroke_id += 1;
                    self.active_stroke_node = Some(*node_id);
                    self.active_stroke_brush_id = self.current_brush_id;
                    self.active_stroke_color_rgb = self.current_brush_color_rgb;
                    self.active_stroke_erase = self.current_brush_erase;
                    self.main_state.begin_preview_stroke(*node_id);
                    self.engine_state.begin_stroke(stroke_id);
                } else {
                    self.streaming_compact_stroke_id = None;
                    self.active_stroke_node = None;
                    self.active_stroke_brush_id = None;
                    self.engine_state.end_stroke();
                    self.main_state.end_preview_stroke(*node_id);
                }
            }
            AppControl::SelectNode { node_id } => {
                let _ = self.engine_state.document_mut().set_active_node(*node_id);
            }
            AppControl::CreateLayerAboveActive { kind } => {
                self.engine_state.invalidate_redo_actions();
                match self
                    .engine_state
                    .document_mut()
                    .create_layer_above_active(*kind)
                {
                    Ok(_) => self.enqueue_render_tree_update(),
                    Err(error) => eprintln!("create layer control failed: {error:?}"),
                }
            }
            AppControl::CreateGroupAboveActive => {
                self.engine_state.invalidate_redo_actions();
                match self.engine_state.document_mut().create_group_above_active() {
                    Ok(_) => self.enqueue_render_tree_update(),
                    Err(error) => eprintln!("create group control failed: {error:?}"),
                }
            }
            AppControl::DeleteNode { node_id } => {
                self.engine_state.invalidate_redo_actions();
                match self.engine_state.delete_node(*node_id) {
                    Ok(update) => self.enqueue_render_tree_msg(update),
                    Err(error) => eprintln!("delete node control failed: {error:?}"),
                }
            }
            AppControl::MoveNode { node_id, target } => {
                self.engine_state.invalidate_redo_actions();
                match self
                    .engine_state
                    .document_mut()
                    .move_node_to(*node_id, *target)
                {
                    Ok(()) => self.enqueue_render_tree_update(),
                    Err(error) => eprintln!("move node control failed: {error:?}"),
                }
            }
            AppControl::SetNodeVisibility { node_id, visible } => {
                self.engine_state.invalidate_redo_actions();
                match self
                    .engine_state
                    .document_mut()
                    .set_node_visibility(*node_id, *visible)
                {
                    Ok(_) => self.enqueue_render_tree_update(),
                    Err(error) => eprintln!("set node visibility control failed: {error:?}"),
                }
            }
            AppControl::SetNodeOpacity { node_id, opacity } => {
                self.engine_state.invalidate_redo_actions();
                match self
                    .engine_state
                    .document_mut()
                    .set_node_opacity(*node_id, *opacity)
                {
                    Ok(()) => self.enqueue_render_tree_update(),
                    Err(error) => eprintln!("set node opacity control failed: {error:?}"),
                }
            }
            AppControl::SetNodeBlendMode {
                node_id,
                blend_mode,
            } => {
                self.engine_state.invalidate_redo_actions();
                match self
                    .engine_state
                    .document_mut()
                    .set_node_blend_mode(*node_id, *blend_mode)
                {
                    Ok(()) => self.enqueue_render_tree_update(),
                    Err(error) => eprintln!("set node blend mode control failed: {error:?}"),
                }
            }
            AppControl::SetActiveBrush { brush_id } => {
                self.set_active_brush_state(*brush_id);
            }
            AppControl::SetActiveBrushColorRgb { rgb } => {
                self.current_brush_color_rgb = *rgb;
            }
            AppControl::SetActiveBrushErase { erase } => {
                self.current_brush_erase = *erase;
            }
            AppControl::UpdateBrushConfig { brush_id, values } => {
                if let Err(error) = self.update_brush_from_config_values(*brush_id, values) {
                    eprintln!("update brush config control failed: {error}");
                }
            }
            AppControl::MoveActiveNodeUp => {
                self.engine_state.invalidate_redo_actions();
                match self.engine_state.document_mut().move_active_node_up() {
                    Ok(()) => self.enqueue_render_tree_update(),
                    Err(error) => eprintln!("move layer up control failed: {error:?}"),
                }
            }
            AppControl::MoveActiveNodeDown => {
                self.engine_state.invalidate_redo_actions();
                match self.engine_state.document_mut().move_active_node_down() {
                    Ok(()) => self.enqueue_render_tree_update(),
                    Err(error) => eprintln!("move layer down control failed: {error:?}"),
                }
            }
        }
    }

    fn enqueue_render_tree_update(&mut self) {
        match self.engine_state.rebuild_render_tree() {
            Ok(msg) => self.enqueue_render_tree_msg(msg),
            Err(error) => eprintln!("render tree rebuild failed after control event: {error}"),
        }
    }

    fn enqueue_render_tree_msg(&mut self, msg: thread_protocol::RenderTreeUpdatedMsg) {
        self.pending_send_gpu_commands
            .extend(self.engine_state.take_pending_gpu_commands());
        self.pending_send_gpu_commands
            .push_back(thread_protocol::GpuCmdMsg::RenderTreeUpdated(msg));
    }

    fn process_engine_frame_from_samples(
        &mut self,
        mut perf: Option<&mut EngineFramePerf>,
    ) -> bool {
        let mut generated_gpu_command_count = 0usize;
        let mut draw_compaction = None;
        if !self.input_samples.is_empty()
            && let Some(node_id) = self.active_stroke_node
        {
            if let Some(brush_id) = self.active_stroke_brush_id {
                self.brush_inputs.clear();
                self.gpu_commands.clear();

                let smooth_and_resample_started = Instant::now();
                for sample in &self.input_samples {
                    let new_inputs = self
                        .engine_state
                        .process_raw_input(sample.cursor, sample.time_ns);
                    self.brush_inputs.extend(new_inputs);
                }
                if let Some(perf) = perf.as_deref_mut() {
                    perf.smooth_and_resample = smooth_and_resample_started.elapsed();
                    perf.brush_input_count = self.brush_inputs.len();
                }

                let brush_inputs = self.brush_inputs.clone();
                let stroke_rgb = self.active_stroke_color_rgb;
                let stroke_erase = self.active_stroke_erase;
                let brush_handling_started = Instant::now();
                for brush_input in &brush_inputs {
                    match self.engine_state.process_stroke_input(
                        brush_id,
                        brush_input,
                        stroke_rgb,
                        stroke_erase,
                        node_id,
                        None,
                    ) {
                        Ok(cmds) => {
                            self.gpu_commands.extend(cmds);
                        }
                        Err(e) => {
                            eprintln!("Stroke processing failed: {e:?}");
                        }
                    }
                }
                if let Some(perf) = perf.as_deref_mut() {
                    perf.brush_handling = brush_handling_started.elapsed();
                }

                let pre_compact_draw_tile_indices =
                    Self::collect_unique_draw_tile_indices(&self.gpu_commands);
                Self::compact_frame_mergeable_draws(&mut self.gpu_commands);
                Self::compact_frame_mergeable_copy_write(&mut self.gpu_commands);
                Self::move_setup_ops_before_draws(&mut self.gpu_commands);
                Self::move_metadata_updates_to_end(&mut self.gpu_commands);
                Self::compact_draw_ops_by_stroke_ctx(
                    &mut self.gpu_commands,
                    &mut self.streaming_compact_stroke_id,
                );
                let post_compact_draw_tile_indices =
                    Self::collect_unique_draw_tile_indices(&self.gpu_commands);
                draw_compaction = Some(TraceDrawCompaction {
                    pre_compact_draw_tile_indices,
                    post_compact_draw_tile_indices,
                });

                let pending_gpu_cmds = std::mem::take(&mut self.gpu_commands);
                generated_gpu_command_count = pending_gpu_cmds.len();
                self.pending_send_gpu_commands.extend(pending_gpu_cmds);
            }
        }

        let send_to_app_thread_started = Instant::now();
        while self.engine_channels.gpu_command_sender.slots() > 0 {
            let Some(cmd) = self.pending_send_gpu_commands.front().cloned() else {
                break;
            };
            if let Err(e) = self.engine_channels.gpu_command_sender.push(cmd) {
                eprintln!("GPU command send failed: {e:?}");
                break;
            }
            self.pending_send_gpu_commands.pop_front();
        }
        if let Some(perf) = perf.as_deref_mut() {
            perf.send_to_app_thread = send_to_app_thread_started.elapsed();
            perf.send_inline_submit = Duration::ZERO;
            perf.generated_gpu_command_count = generated_gpu_command_count;
            perf.inline_submitted_gpu_command_count = 0;
            perf.inline_submit_batches = 0;
            perf.pending_send_gpu_command_count = self.pending_send_gpu_commands.len();
        }

        self.gpu_commands.clear();
        let accept_by_app_thread_started = Instant::now();
        while let Ok(cmd) = self.main_channels.gpu_command_receiver.pop() {
            self.gpu_commands.push(cmd);
        }
        if let Some(perf) = perf.as_deref_mut() {
            perf.accept_by_app_thread = accept_by_app_thread_started.elapsed();
            perf.gpu_command_count = self.gpu_commands.len();
        }

        let has_commands = !self.gpu_commands.is_empty();
        let mut frame_batch_stats = None;
        if has_commands {
            let submit_to_gpu_started = Instant::now();
            let submit_stats = self.main_state.process_gpu_commands(&self.gpu_commands);
            frame_batch_stats = Some(submit_stats.frame_batch.clone());
            if let Some(perf) = perf.as_deref_mut() {
                perf.submit_to_gpu = submit_to_gpu_started.elapsed();
                perf.submit_collect_render_tree = submit_stats.frame_batch.render_tree_collect;
                perf.submit_materialize_parametric =
                    submit_stats.frame_batch.parametric_materialize;
                perf.submit_composite_render_tree = submit_stats.frame_batch.render_tree_composite;
                perf.submit_queue = submit_stats.frame_batch.queue_submit;
                perf.submit_parametric_cmd_count = submit_stats.frame_batch.parametric_cmd_count;
                perf.submit_parametric_tile_count =
                    submit_stats.frame_batch.parametric_dst_tile_count;
                perf.submit_render_cmd_count = submit_stats.frame_batch.render_cmd_count;
                perf.submit_render_tile_count = submit_stats.frame_batch.render_dst_tile_count;
                perf.submit_render_source_count = submit_stats.frame_batch.render_source_count;
                perf.submit_dirty_tile_count = submit_stats.dirty_tile_count;
                perf.submit_dirty_rect_count = submit_stats.dirty_rect_count;
                perf.submit_dirty_bbox_tile_area = submit_stats.dirty_bbox_tile_area;
                perf.submit_dirty_node_count = submit_stats.dirty_node_count;
            }
        }
        if let Some(trace_recorder) = &mut self.trace_recorder {
            let runtime_tile_events = self
                .main_state
                .take_tile_runtime_events()
                .into_iter()
                .map(|event| TraceRuntimeTileEvent {
                    stage: match event.stage {
                        crate::main_thread::TileRuntimeStage::ApplyVisibleUpdates => {
                            TraceRuntimeTileStage::ApplyVisibleUpdates
                        }
                        crate::main_thread::TileRuntimeStage::ProcessRenderComposite => {
                            TraceRuntimeTileStage::ProcessRenderComposite
                        }
                    },
                    tile_indices: event.tile_indices,
                })
                .collect::<Vec<_>>();
            trace_recorder.record_output_frame(
                &self.gpu_commands,
                frame_batch_stats.as_ref(),
                runtime_tile_events,
                draw_compaction,
            );
        }

        if let Some(perf) = perf {
            self.trace_engine_frame_perf(perf);
        }

        has_commands
    }

    fn trace_engine_frame_perf(&mut self, perf: &EngineFramePerf) {
        if !self.perf_trace.enabled {
            return;
        }
        let stages = [
            ("input_sample", perf.input_sample),
            ("smooth_and_resample", perf.smooth_and_resample),
            ("brush_handling", perf.brush_handling),
            ("send_to_app_thread", perf.send_to_app_thread),
            ("accept_by_app_thread", perf.accept_by_app_thread),
            ("submit_to_gpu", perf.submit_to_gpu),
        ];
        let total = stages
            .iter()
            .map(|(_, duration)| *duration)
            .fold(Duration::ZERO, |acc, item| acc + item);
        if total < self.perf_trace.slow_threshold {
            return;
        }
        let Some((bottleneck, bottleneck_duration)) =
            stages.iter().max_by_key(|(_, duration)| *duration)
        else {
            return;
        };
        self.perf_frame_seq += 1;
        eprintln!(
            "[PERF][pipeline][engine_frame={}] total_ms={:.3} bottleneck={} ({:.3}ms) samples={} brush_inputs={} gpu_cmds={} generated_gpu_cmds={} pending_send_gpu_cmds={} inline_submit_cmds={} inline_submit_batches={} inline_submit_ms={:.3} stages_ms={{input:{:.3}, smooth_resample:{:.3}, brush:{:.3}, send:{:.3}, accept:{:.3}, submit:{:.3}}} submit_ms={{collect:{:.3}, materialize:{:.3}, composite:{:.3}, queue:{:.3}}} dirty={{tiles:{}, rects:{}, bbox_tiles:{}, nodes:{}}} render_work={{parametric_cmds:{}, parametric_tiles:{}, render_cmds:{}, render_tiles:{}, render_sources:{}}}",
            self.perf_frame_seq,
            duration_ms(total),
            bottleneck,
            duration_ms(*bottleneck_duration),
            perf.sample_count,
            perf.brush_input_count,
            perf.gpu_command_count,
            perf.generated_gpu_command_count,
            perf.pending_send_gpu_command_count,
            perf.inline_submitted_gpu_command_count,
            perf.inline_submit_batches,
            duration_ms(perf.send_inline_submit),
            duration_ms(perf.input_sample),
            duration_ms(perf.smooth_and_resample),
            duration_ms(perf.brush_handling),
            duration_ms(perf.send_to_app_thread),
            duration_ms(perf.accept_by_app_thread),
            duration_ms(perf.submit_to_gpu),
            duration_ms(perf.submit_collect_render_tree),
            duration_ms(perf.submit_materialize_parametric),
            duration_ms(perf.submit_composite_render_tree),
            duration_ms(perf.submit_queue),
            perf.submit_dirty_tile_count,
            perf.submit_dirty_rect_count,
            perf.submit_dirty_bbox_tile_area,
            perf.submit_dirty_node_count,
            perf.submit_parametric_cmd_count,
            perf.submit_parametric_tile_count,
            perf.submit_render_cmd_count,
            perf.submit_render_tile_count,
            perf.submit_render_source_count,
        );
    }

    fn normalize_input_sample_timestamps(&mut self) {
        for sample in &mut self.input_samples {
            if sample.time_ns == 0 {
                sample.time_ns = current_time_ns();
            }
        }
    }

    pub fn enable_trace_recording(&mut self) {
        self.trace_recorder = Some(TraceRecorder::default());
    }

    pub fn save_trace_files(
        &self,
        input_path: Option<&std::path::Path>,
        output_path: Option<&std::path::Path>,
    ) -> Result<(), TraceIoError> {
        match &self.trace_recorder {
            Some(trace_recorder) => {
                if let Some(input_path) = input_path {
                    trace_recorder.save_input_file(input_path)?;
                }
                if let Some(output_path) = output_path {
                    trace_recorder.save_output_file(output_path)?;
                }
                Ok(())
            }
            None => Ok(()),
        }
    }

    pub fn process_main_render(&mut self) -> bool {
        if !self.perf_trace.enabled {
            return self.main_state.process_render();
        }
        let started = Instant::now();
        let has_work = self.main_state.process_render();
        let elapsed = started.elapsed();
        if elapsed >= self.perf_trace.slow_threshold {
            eprintln!(
                "[PERF][pipeline][submit_to_gpu_render] elapsed_ms={:.3} has_work={}",
                duration_ms(elapsed),
                has_work
            );
        }
        has_work
    }

    pub fn set_surface(&mut self, surface: SurfaceRuntime) {
        self.main_state.set_surface(surface);
    }

    pub fn resize_surface(&mut self, width: u32, height: u32) {
        self.main_state.resize_surface(width, height);
    }

    pub fn present_to_screen(&mut self) {
        let started = if self.perf_trace.enabled {
            Some(Instant::now())
        } else {
            None
        };
        if let Err(e) = self.main_state.present_to_screen() {
            eprintln!("Screen present failed: {e:?}");
        }
        if let Some(started) = started {
            let elapsed = started.elapsed();
            if elapsed >= self.perf_trace.slow_threshold {
                eprintln!(
                    "[PERF][pipeline][show_on_screen] elapsed_ms={:.3}",
                    duration_ms(elapsed)
                );
            }
        }
    }

    pub fn present_to_screen_with_overlay<F>(&mut self, overlay: F)
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
        if let Err(e) = self.main_state.present_to_screen_with_overlay(overlay) {
            eprintln!("Screen present failed: {e:?}");
        }
    }

    pub fn has_pending_visible_tile_updates(&self) -> bool {
        self.main_state.has_pending_visible_tile_updates()
    }

    pub fn has_pending_render_work(&self) -> bool {
        self.main_state.has_pending_render_work()
    }

    pub fn has_pending_engine_gpu_commands(&self) -> bool {
        !self.pending_send_gpu_commands.is_empty()
    }

    pub fn flush_visible_tile_updates(&mut self) {
        self.main_state.flush_visible_tile_updates();
    }

    pub fn save_screenshot(
        &mut self,
        output_path: &std::path::Path,
        width: u32,
        height: u32,
    ) -> Result<(), crate::ScreenshotError> {
        self.main_state.save_screenshot(output_path, width, height)
    }

    pub fn export_document_jpeg(&mut self, output_path: &Path) -> Result<(), ExportImageError> {
        self.main_state.export_jpeg_image(output_path)
    }

    pub fn export_frontend_jpeg(&mut self, output_path: &Path) -> Result<(), ExportImageError> {
        self.main_state.export_frontend_jpeg(output_path)
    }

    pub fn rebuild_render_tree(&mut self) -> Result<(), document::ImageCreateError> {
        let msg = self.engine_state.rebuild_render_tree()?;
        let mut commands = self.engine_state.take_pending_gpu_commands();
        commands.push(thread_protocol::GpuCmdMsg::RenderTreeUpdated(msg));
        let _ = self.main_state.process_gpu_commands(&commands);
        Ok(())
    }

    pub fn resize_document_canvas_anchored_top_left(
        &mut self,
        layout: ImageLayout,
    ) -> Result<(), document::ImageCreateError> {
        let msg = self
            .engine_state
            .resize_document_canvas_anchored_top_left(layout)?;
        self.document_layout = layout;
        let mut commands = self.engine_state.take_pending_gpu_commands();
        commands.push(thread_protocol::GpuCmdMsg::RenderTreeUpdated(msg));
        let _ = self.main_state.process_gpu_commands(&commands);
        Ok(())
    }

    pub fn save_document_package(
        &mut self,
        package_dir: &Path,
    ) -> Result<(), DocumentPackageError> {
        let package = self.build_packed_document_file()?;
        std::fs::create_dir_all(package_dir)?;
        std::fs::create_dir_all(package_dir.join("layers"))?;

        for layer in &package.layers {
            save_png_bytes(&package_dir.join(&layer.file_name), &layer.png_bytes)?;
        }

        let manifest_file = File::create(package_dir.join("manifest.json"))?;
        serde_json::to_writer_pretty(BufWriter::new(manifest_file), &package.manifest)?;
        Ok(())
    }

    pub fn save_document_bundle(&mut self, bundle_path: &Path) -> Result<(), DocumentPackageError> {
        if let Some(parent) = bundle_path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        let package = self.build_packed_document_file()?;
        let file = File::create(bundle_path)?;
        let writer = BufWriter::new(file);
        let mut encoder = GzEncoder::new(writer, Compression::default());
        serde_json::to_writer(&mut encoder, &package)?;
        encoder.finish()?;
        Ok(())
    }

    pub fn load_document_package(
        &mut self,
        package_dir: &Path,
    ) -> Result<(), DocumentPackageError> {
        let manifest_file = File::open(package_dir.join("manifest.json"))?;
        let manifest: DocumentStorageManifest =
            serde_json::from_reader(BufReader::new(manifest_file))?;
        let raster_requests = collect_manifest_raster_assets(&manifest.root);
        let mut layers = Vec::with_capacity(raster_requests.len());
        for (node_id, file_name) in raster_requests {
            layers.push(PackedLayerAsset {
                node_id,
                file_name: file_name.to_string(),
                png_bytes: std::fs::read(package_dir.join(file_name))?,
            });
        }
        self.load_packed_document_file(PackedDocumentFile { manifest, layers })
    }

    pub fn load_document_bundle(&mut self, bundle_path: &Path) -> Result<(), DocumentPackageError> {
        let file = File::open(bundle_path)?;
        let reader = BufReader::new(file);
        let decoder = GzDecoder::new(reader);
        let package: PackedDocumentFile = serde_json::from_reader(decoder)?;
        self.load_packed_document_file(package)
    }

    fn build_packed_document_file(&mut self) -> Result<PackedDocumentFile, DocumentPackageError> {
        let manifest = self.engine_state.document().storage_manifest();
        let requests = self.engine_state.document().raster_layer_export_requests();
        let mut layers = Vec::with_capacity(requests.len());
        for request in requests {
            let image = self
                .engine_state
                .document()
                .get_leaf_image(request.node_id)
                .ok_or(DocumentPackageError::MissingRasterNode {
                    node_id: request.node_id,
                })?;
            let stored = self.main_state.export_layer_image(image)?;
            layers.push(PackedLayerAsset {
                node_id: request.node_id.0,
                file_name: request.file_name,
                png_bytes: encode_png_rgba8(&stored)?,
            });
        }
        Ok(PackedDocumentFile { manifest, layers })
    }

    fn load_packed_document_file(
        &mut self,
        package: PackedDocumentFile,
    ) -> Result<(), DocumentPackageError> {
        let manifest = package.manifest;
        let mut raster_images = Vec::with_capacity(package.layers.len());
        for layer in package.layers {
            let image = decode_png_rgba8(&layer.png_bytes)?;
            raster_images.push((NodeId(layer.node_id), image));
        }

        let mut document = Document::from_storage_manifest(
            manifest,
            glaphica_core::BackendId::new(0),
            glaphica_core::BackendId::new(1),
        )?;

        for (node_id, image) in raster_images {
            let mut tile_indices = Vec::new();
            image.collect_non_empty_tile_indices(&mut tile_indices);
            let Some(layer) = document.get_leaf_image_mut(node_id) else {
                return Err(DocumentPackageError::MissingRasterNode { node_id });
            };
            let mut tile_pixels = Vec::new();
            for tile_index in tile_indices {
                let tile_key = self
                    .engine_state
                    .allocate_leaf_tile(layer.backend())
                    .ok_or(DocumentPackageError::TileAlloc {
                        node_id,
                        tile_index,
                    })?;
                layer.set_tile_key(tile_index, tile_key).map_err(|_| {
                    DocumentPackageError::TileAlloc {
                        node_id,
                        tile_index,
                    }
                })?;
                image
                    .copy_tile_rgba8(tile_index, &mut tile_pixels)
                    .map_err(|_| DocumentPackageError::TileUpload {
                        node_id,
                        tile_index,
                    })?;
                if !self.main_state.upload_tile_rgba8(tile_key, &tile_pixels) {
                    return Err(DocumentPackageError::TileUpload {
                        node_id,
                        tile_index,
                    });
                }
            }
        }

        self.engine_state.replace_document(document);
        let mut msg = self.engine_state.rebuild_render_tree().map_err(|error| {
            DocumentPackageError::Storage(DocumentStorageError::ImageCreate(error))
        })?;
        msg.dirty_render_caches =
            collect_all_render_cache_node_ids(&self.engine_state.shared_tree().read());
        let _ = self
            .main_state
            .process_gpu_commands(&[thread_protocol::GpuCmdMsg::RenderTreeUpdated(msg)]);
        Ok(())
    }

    pub fn register_brush<S: BrushSpec + BrushResamplerDistancePolicy>(
        &mut self,
        brush_id: BrushId,
        brush: S,
    ) -> Result<(), BrushRegisterError> {
        let resampler_distance = brush.resampler_distance();
        let max_affected_radius_px = brush.max_affected_radius_px();

        let cache_backend_id = self.main_state.register_brush(brush_id, &brush)?;
        if let Some(cache_backend_id) = cache_backend_id {
            while self
                .engine_state
                .backend_manager()
                .backend(cache_backend_id)
                .is_none()
            {
                if self
                    .engine_state
                    .backend_manager_mut()
                    .add_backend(AtlasLayout::Small11)
                    .is_err()
                {
                    break;
                }
            }
        }

        self.engine_state
            .brush_runtime_mut()
            .register_pipeline_with_stroke_buffer_backend(
                brush_id,
                max_affected_radius_px,
                cache_backend_id,
                brush,
            )
            .map_err(BrushRegisterError::Engine)?;

        let Some(brush_index) = usize::try_from(brush_id.0).ok() else {
            return Ok(());
        };
        if let Some(slot) = self.brush_resampler_distances.get_mut(brush_index) {
            *slot = Some(resampler_distance);
        }

        Ok(())
    }

    pub fn update_brush<S: BrushSpec + BrushResamplerDistancePolicy>(
        &mut self,
        brush_id: BrushId,
        brush: S,
    ) -> Result<(), BrushRegisterError> {
        let resampler_distance = brush.resampler_distance();
        let max_affected_radius_px = brush.max_affected_radius_px();
        self.engine_state
            .brush_runtime_mut()
            .update_pipeline(brush_id, max_affected_radius_px, brush)
            .map_err(BrushRegisterError::Engine)?;

        let Some(brush_index) = usize::try_from(brush_id.0).ok() else {
            return Ok(());
        };
        if let Some(slot) = self.brush_resampler_distances.get_mut(brush_index) {
            *slot = Some(resampler_distance);
        }
        Ok(())
    }
}

fn duration_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

fn collect_manifest_raster_assets(root: &document::StoredLayerNode) -> Vec<(u64, &str)> {
    let mut output = Vec::new();
    collect_manifest_raster_assets_from_node(root, &mut output);
    output
}

fn collect_all_render_cache_node_ids(tree: &FlatRenderTree) -> Vec<NodeId> {
    tree.nodes
        .iter()
        .filter_map(|(node_id, node)| node.kind.render_cache().map(|_| *node_id))
        .collect()
}

fn collect_manifest_raster_assets_from_node<'a>(
    node: &'a document::StoredLayerNode,
    output: &mut Vec<(u64, &'a str)>,
) {
    match node {
        document::StoredLayerNode::Branch { children, .. } => {
            for child in children {
                collect_manifest_raster_assets_from_node(child, output);
            }
        }
        document::StoredLayerNode::RasterLayer { image, .. } => {
            output.push((image.node_id, &image.file_name));
        }
        document::StoredLayerNode::SolidColorLayer { .. } => {}
    }
}

fn save_png_rgba8(path: &Path, image: &StoredImage) -> Result<(), DocumentPackageError> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    save_png_bytes(path, &encode_png_rgba8(image)?)?;
    Ok(())
}

fn load_png_rgba8(path: &Path) -> Result<StoredImage, DocumentPackageError> {
    decode_png_rgba8(&std::fs::read(path)?)
}

fn save_png_bytes(path: &Path, png_bytes: &[u8]) -> Result<(), DocumentPackageError> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, png_bytes)?;
    Ok(())
}

fn encode_png_rgba8(image: &StoredImage) -> Result<Vec<u8>, DocumentPackageError> {
    let mut bytes = Vec::new();
    let mut encoder = png::Encoder::new(&mut bytes, image.width(), image.height());
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(image.pixels_rgba8())?;
    drop(writer);
    Ok(bytes)
}

fn decode_png_rgba8(png_bytes: &[u8]) -> Result<StoredImage, DocumentPackageError> {
    let decoder = png::Decoder::new(std::io::Cursor::new(png_bytes));
    let mut reader = decoder.read_info()?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf)?;
    let bytes = &buf[..info.buffer_size()];
    let pixels = match info.color_type {
        png::ColorType::Rgba => bytes.to_vec(),
        png::ColorType::Rgb => {
            let mut rgba = Vec::with_capacity((info.width * info.height * 4) as usize);
            for chunk in bytes.chunks_exact(3) {
                rgba.extend_from_slice(&[chunk[0], chunk[1], chunk[2], 255]);
            }
            rgba
        }
        png::ColorType::GrayscaleAlpha => {
            let mut rgba = Vec::with_capacity((info.width * info.height * 4) as usize);
            for chunk in bytes.chunks_exact(2) {
                rgba.extend_from_slice(&[chunk[0], chunk[0], chunk[0], chunk[1]]);
            }
            rgba
        }
        png::ColorType::Grayscale => {
            let mut rgba = Vec::with_capacity((info.width * info.height * 4) as usize);
            for value in bytes {
                rgba.extend_from_slice(&[*value, *value, *value, 255]);
            }
            rgba
        }
        png::ColorType::Indexed => {
            return Err(DocumentPackageError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "indexed color png is not supported for document package",
            )));
        }
    };
    StoredImage::new_rgba8(info.width, info.height, pixels).map_err(|error| {
        DocumentPackageError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, error))
    })
}

fn current_time_ns() -> u64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_nanos() as u64,
        Err(_) => 0,
    }
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::path::{Path, PathBuf};
    use std::sync::{Mutex, MutexGuard, OnceLock};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use brushes::builtin_brushes::{pixel_rect::PixelRectBrush, round::RoundBrush};
    use document::StoredLayerNode;
    use flate2::{Compression, read::GzDecoder, write::GzEncoder};
    use glaphica_core::{BlendMode, BrushId, NodeId, StrokeId, TileKey};
    use images::StoredImage;
    use images::layout::ImageLayout;
    use thread_protocol::{
        CopyOp, DrawFrameMergePolicy, DrawOp, DrawStrokeCtx, GpuCmdFrameMergeTag, GpuCmdMsg,
        WriteKind, WriteOp,
    };

    use super::{
        AppThreadIntegration, PackedDocumentFile, PackedLayerAsset, collect_manifest_raster_assets,
        decode_png_rgba8, encode_png_rgba8, load_png_rgba8, save_png_rgba8,
    };
    use crate::trace::{TraceOutputFile, TraceRecorder};

    fn issue10_replay_input_path() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test/records/tile_update_omitted.json")
    }

    fn issue10_trace_output_path() -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("glaphica-issue10-trace-{unique}.json"))
    }

    fn issue10_missing_tile_indices() -> Vec<usize> {
        vec![59, 60, 61, 76, 77, 78, 93, 94, 95, 110, 111, 112]
    }

    fn issue10_updated_tile_indices() -> Vec<usize> {
        vec![
            59, 60, 61, 62, 63, 64, 65, 76, 77, 78, 79, 80, 81, 82, 93, 94, 95, 96, 97, 98, 99,
            110, 111, 112, 113, 114, 115, 116,
        ]
    }

    fn issue10_non_missing_updated_tile_indices() -> Vec<usize> {
        vec![
            62, 63, 64, 65, 79, 80, 81, 82, 96, 97, 98, 99, 113, 114, 115, 116,
        ]
    }

    fn issue10_gpu_test_guard() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .expect("issue10 GPU test lock poisoned")
    }

    fn register_default_test_brushes(app: &mut AppThreadIntegration) {
        let round_brush = RoundBrush::with_default_curves(3.0, 0.8).unwrap();
        let pixel_rect_brush = PixelRectBrush::new(8);
        app.register_brush(BrushId(0), round_brush).unwrap();
        app.register_brush(BrushId(1), pixel_rect_brush).unwrap();
        app.set_active_brush(BrushId(0));
    }

    fn drive_issue10_replay_to_idle(app: &mut AppThreadIntegration) {
        let replay = TraceRecorder::load_input_file(&issue10_replay_input_path()).unwrap();
        let mut replay_index = 0usize;
        let mut replay_finished = false;
        let mut iterations = 0usize;
        loop {
            iterations += 1;
            assert!(iterations < 4096, "headless replay loop did not quiesce");

            let has_work = if !replay_finished {
                if let Some(frame) = replay.frames.get(replay_index) {
                    replay_index += 1;
                    app.process_replay_input_frame(frame)
                } else {
                    replay_finished = true;
                    app.process_engine_frame(Duration::from_millis(0))
                }
            } else {
                app.process_engine_frame(Duration::from_millis(0))
            };

            let has_pending_engine_gpu_commands = app.has_pending_engine_gpu_commands();
            let has_pending_visible_tile_updates = app.has_pending_visible_tile_updates();
            let has_pending_render_work = app.has_pending_render_work();

            if has_work
                || has_pending_engine_gpu_commands
                || has_pending_visible_tile_updates
                || has_pending_render_work
            {
                let _ = app.process_main_render();
            }

            if replay_finished
                && !has_work
                && !has_pending_engine_gpu_commands
                && !has_pending_visible_tile_updates
                && !has_pending_render_work
            {
                break;
            }
        }
    }

    fn run_issue10_headless_replay() -> TraceOutputFile {
        let Ok(mut app) = pollster::block_on(AppThreadIntegration::new(
            "repro".to_string(),
            ImageLayout::new(1024, 1024),
        )) else {
            panic!("failed to initialize app integration");
        };
        register_default_test_brushes(&mut app);
        app.enable_trace_recording();
        drive_issue10_replay_to_idle(&mut app);

        let output_path = issue10_trace_output_path();
        app.save_trace_files(None, Some(&output_path)).unwrap();
        let output: TraceOutputFile =
            serde_json::from_reader(File::open(&output_path).unwrap()).unwrap();
        let _ = std::fs::remove_file(&output_path);
        output
    }

    fn run_issue10_headless_replay_and_export_images() -> (StoredImage, Vec<(NodeId, StoredImage)>)
    {
        let Ok(mut app) = pollster::block_on(AppThreadIntegration::new(
            "repro".to_string(),
            ImageLayout::new(1024, 1024),
        )) else {
            panic!("failed to initialize app integration");
        };
        register_default_test_brushes(&mut app);
        drive_issue10_replay_to_idle(&mut app);

        let tree = app.engine_state.shared_tree().read();
        let root_id = tree.root_id.expect("root render node");
        let root_image = tree
            .nodes
            .get(&root_id)
            .and_then(|node| node.kind.render_image())
            .expect("root render image");
        let exported_root = app.main_state.export_layer_image(root_image).unwrap();
        let exported_non_root = tree
            .nodes
            .iter()
            .filter_map(|(node_id, node)| {
                if *node_id == root_id {
                    None
                } else {
                    node.kind
                        .render_image()
                        .map(|image| (*node_id, app.main_state.export_layer_image(image).unwrap()))
                }
            })
            .collect();

        (exported_root, exported_non_root)
    }

    fn export_current_root_image(app: &mut AppThreadIntegration) -> StoredImage {
        let tree = app.engine_state.shared_tree().read();
        let root_id = tree.root_id.expect("root render node");
        let root_image = tree
            .nodes
            .get(&root_id)
            .and_then(|node| node.kind.render_image())
            .expect("root render image");
        app.main_state.export_layer_image(root_image).unwrap()
    }

    fn current_tree_tile_keys(
        app: &AppThreadIntegration,
        tile_indices: &[usize],
    ) -> Vec<(NodeId, Vec<TileKey>)> {
        let tree = app.engine_state.shared_tree().read();
        let root_id = tree.root_id;
        let mut entries = tree
            .nodes
            .iter()
            .filter_map(|(node_id, node)| {
                let image = node.kind.render_image()?;
                Some((
                    *node_id,
                    tile_indices
                        .iter()
                        .map(|&tile_index| image.tile_key(tile_index).unwrap_or(TileKey::EMPTY))
                        .collect::<Vec<_>>(),
                ))
            })
            .collect::<Vec<_>>();
        entries.sort_by_key(|(node_id, _)| {
            (if Some(*node_id) == root_id { 0u8 } else { 1u8 }, node_id.0)
        });
        entries
    }

    fn run_issue10_headless_replay_and_capture_frontend_image() -> (StoredImage, StoredImage) {
        let Ok(mut app) = pollster::block_on(AppThreadIntegration::new(
            "repro".to_string(),
            ImageLayout::new(1024, 1024),
        )) else {
            panic!("failed to initialize app integration");
        };
        register_default_test_brushes(&mut app);
        drive_issue10_replay_to_idle(&mut app);

        let exported_root = export_current_root_image(&mut app);
        let frontend_image = app
            .main_state
            .test_read_final_image_rgba8(1024, 1024)
            .unwrap();

        (exported_root, frontend_image)
    }

    fn stored_image_tile_alpha_sum(image: &StoredImage, tile_index: usize) -> u64 {
        stored_image_tile_metric(image, tile_index, |rgba| u64::from(rgba[3]))
    }

    fn stored_image_tile_non_white_sum(image: &StoredImage, tile_index: usize) -> u64 {
        stored_image_tile_metric(image, tile_index, |rgba| {
            u64::from(255u8.saturating_sub(rgba[0]))
                + u64::from(255u8.saturating_sub(rgba[1]))
                + u64::from(255u8.saturating_sub(rgba[2]))
        })
    }

    fn stored_image_tile_metric<F>(
        image: &StoredImage,
        tile_index: usize,
        mut pixel_metric: F,
    ) -> u64
    where
        F: FnMut([u8; 4]) -> u64,
    {
        let tile_size = glaphica_core::IMAGE_TILE_SIZE as usize;
        let width = image.width() as usize;
        let tiles_per_row = width / tile_size;
        let tile_x = tile_index % tiles_per_row;
        let tile_y = tile_index / tiles_per_row;
        let origin_x = tile_x * tile_size;
        let origin_y = tile_y * tile_size;
        let pixels = image.pixels_rgba8();
        let mut sum = 0u64;
        for y in 0..tile_size {
            for x in 0..tile_size {
                let pixel_index = ((origin_y + y) * width + origin_x + x) * 4;
                sum += pixel_metric([
                    pixels[pixel_index],
                    pixels[pixel_index + 1],
                    pixels[pixel_index + 2],
                    pixels[pixel_index + 3],
                ]);
            }
        }
        sum
    }

    #[test]
    fn compact_copy_and_write_keeps_first_copy_and_last_write() {
        let dst_tile = TileKey::from_parts(0, 0, 1);
        let buffer_tile = TileKey::from_parts(2, 0, 9);
        let copy = GpuCmdMsg::CopyOp(CopyOp {
            src_tile_key: TileKey::from_parts(0, 0, 7),
            dst_tile_key: dst_tile,
            frame_merge: GpuCmdFrameMergeTag::KeepFirstInFrameByDstTile,
        });
        let draw = |value| {
            GpuCmdMsg::DrawOp(DrawOp {
                stroke_ctx: Some(DrawStrokeCtx {
                    node_id: NodeId(1),
                    blend_mode: BlendMode::Alpha,
                    frame_merge: DrawFrameMergePolicy::None,
                    rgb: [1.0, 0.0, 0.0],
                    brush_id: BrushId(2),
                }),
                tile_index: 3,
                tile_key: buffer_tile,
                origin_tile: TileKey::EMPTY,
                ref_image: None,
                input: vec![value],
                stroke_id: StrokeId(4),
            })
        };
        let write = |opacity| {
            GpuCmdMsg::WriteOp(WriteOp {
                src_tile_key: buffer_tile,
                node_id: NodeId(1),
                tile_index: 3,
                dst_tile_key: dst_tile,
                blend_mode: BlendMode::Normal,
                kind: WriteKind::Paint,
                opacity,
                rgb: Some([1.0, 0.0, 0.0]),
                frame_merge: GpuCmdFrameMergeTag::KeepLastInFrameByDstTile,
            })
        };

        let mut commands = vec![copy, draw(1.0), write(0.2), draw(2.0), write(0.8)];
        AppThreadIntegration::compact_frame_mergeable_copy_write(&mut commands);

        assert_eq!(commands.len(), 4);
        assert!(matches!(commands[0], GpuCmdMsg::CopyOp(_)));
        assert!(matches!(commands[1], GpuCmdMsg::DrawOp(_)));
        assert!(matches!(commands[2], GpuCmdMsg::DrawOp(_)));
        let GpuCmdMsg::WriteOp(write_op) = &commands[3] else {
            panic!("expected final write op");
        };
        assert_eq!(write_op.opacity, 0.8);
    }

    #[test]
    fn move_mergeable_writes_and_updates_to_end_preserves_draw_phase() {
        let dst_tile = TileKey::from_parts(0, 0, 1);
        let buffer_tile = TileKey::from_parts(2, 0, 9);
        let mut commands = vec![
            GpuCmdMsg::CopyOp(CopyOp {
                src_tile_key: TileKey::from_parts(0, 0, 7),
                dst_tile_key: dst_tile,
                frame_merge: GpuCmdFrameMergeTag::KeepFirstInFrameByDstTile,
            }),
            GpuCmdMsg::DrawOp(DrawOp {
                stroke_ctx: Some(DrawStrokeCtx {
                    node_id: NodeId(1),
                    blend_mode: BlendMode::Alpha,
                    frame_merge: DrawFrameMergePolicy::None,
                    rgb: [1.0, 0.0, 0.0],
                    brush_id: BrushId(2),
                }),
                tile_index: 3,
                tile_key: buffer_tile,
                origin_tile: TileKey::EMPTY,
                ref_image: None,
                input: vec![1.0],
                stroke_id: StrokeId(4),
            }),
            GpuCmdMsg::TileSlotKeyUpdate(thread_protocol::TileSlotKeyUpdateMsg {
                updates: vec![(NodeId(1), 3, dst_tile)],
            }),
            GpuCmdMsg::DrawOp(DrawOp {
                stroke_ctx: Some(DrawStrokeCtx {
                    node_id: NodeId(2),
                    blend_mode: BlendMode::Alpha,
                    frame_merge: DrawFrameMergePolicy::None,
                    rgb: [1.0, 0.0, 0.0],
                    brush_id: BrushId(2),
                }),
                tile_index: 4,
                tile_key: buffer_tile,
                origin_tile: TileKey::EMPTY,
                ref_image: None,
                input: vec![2.0],
                stroke_id: StrokeId(4),
            }),
            GpuCmdMsg::WriteOp(WriteOp {
                src_tile_key: buffer_tile,
                node_id: NodeId(1),
                tile_index: 3,
                dst_tile_key: dst_tile,
                blend_mode: BlendMode::Normal,
                kind: WriteKind::Paint,
                opacity: 0.8,
                rgb: Some([1.0, 0.0, 0.0]),
                frame_merge: GpuCmdFrameMergeTag::KeepLastInFrameByDstTile,
            }),
        ];

        AppThreadIntegration::move_mergeable_writes_to_end(&mut commands);
        AppThreadIntegration::move_metadata_updates_to_end(&mut commands);

        assert!(matches!(commands[0], GpuCmdMsg::CopyOp(_)));
        assert!(matches!(commands[1], GpuCmdMsg::DrawOp(_)));
        assert!(matches!(commands[2], GpuCmdMsg::DrawOp(_)));
        assert!(matches!(commands[3], GpuCmdMsg::WriteOp(_)));
        assert!(matches!(commands[4], GpuCmdMsg::TileSlotKeyUpdate(_)));
    }

    #[test]
    fn move_setup_ops_before_draws_builds_setup_draw_write_update_phases() {
        let dst_tile = TileKey::from_parts(0, 0, 1);
        let buffer_tile = TileKey::from_parts(2, 0, 9);
        let mut commands = vec![
            GpuCmdMsg::DrawOp(DrawOp {
                stroke_ctx: Some(DrawStrokeCtx {
                    node_id: NodeId(1),
                    blend_mode: BlendMode::Alpha,
                    frame_merge: DrawFrameMergePolicy::None,
                    rgb: [1.0, 0.0, 0.0],
                    brush_id: BrushId(2),
                }),
                tile_index: 3,
                tile_key: buffer_tile,
                origin_tile: TileKey::EMPTY,
                ref_image: None,
                input: vec![1.0],
                stroke_id: StrokeId(4),
            }),
            GpuCmdMsg::CopyOp(CopyOp {
                src_tile_key: TileKey::from_parts(0, 0, 7),
                dst_tile_key: dst_tile,
                frame_merge: GpuCmdFrameMergeTag::KeepFirstInFrameByDstTile,
            }),
            GpuCmdMsg::ClearOp(thread_protocol::ClearOp {
                tile_key: buffer_tile,
            }),
            GpuCmdMsg::WriteOp(WriteOp {
                src_tile_key: buffer_tile,
                node_id: NodeId(1),
                tile_index: 3,
                dst_tile_key: dst_tile,
                blend_mode: BlendMode::Normal,
                kind: WriteKind::Paint,
                opacity: 0.8,
                rgb: Some([1.0, 0.0, 0.0]),
                frame_merge: GpuCmdFrameMergeTag::KeepLastInFrameByDstTile,
            }),
            GpuCmdMsg::TileSlotKeyUpdate(thread_protocol::TileSlotKeyUpdateMsg {
                updates: vec![(NodeId(1), 3, dst_tile)],
            }),
        ];

        AppThreadIntegration::move_setup_ops_before_draws(&mut commands);
        AppThreadIntegration::move_mergeable_writes_to_end(&mut commands);
        AppThreadIntegration::move_metadata_updates_to_end(&mut commands);

        assert!(matches!(commands[0], GpuCmdMsg::CopyOp(_)));
        assert!(matches!(commands[1], GpuCmdMsg::ClearOp(_)));
        assert!(matches!(commands[2], GpuCmdMsg::DrawOp(_)));
        assert!(matches!(commands[3], GpuCmdMsg::WriteOp(_)));
        assert!(matches!(commands[4], GpuCmdMsg::TileSlotKeyUpdate(_)));
    }

    #[test]
    fn compact_draws_does_not_merge_different_tile_keys_for_same_tile_index() {
        let buffer_tile_a = TileKey::from_parts(2, 0, 10);
        let buffer_tile_b = TileKey::from_parts(2, 0, 11);
        let draw = |tile_key: TileKey, value: f32| {
            GpuCmdMsg::DrawOp(DrawOp {
                stroke_ctx: Some(DrawStrokeCtx {
                    node_id: NodeId(1),
                    blend_mode: BlendMode::Additive,
                    frame_merge: DrawFrameMergePolicy::KeepLastInFrameByNodeTileBrush,
                    rgb: [1.0, 0.0, 0.0],
                    brush_id: BrushId(2),
                }),
                tile_index: 3,
                tile_key,
                origin_tile: TileKey::EMPTY,
                ref_image: None,
                input: vec![value],
                stroke_id: StrokeId(5),
            })
        };
        let write = |src: TileKey, dst: TileKey| {
            GpuCmdMsg::WriteOp(WriteOp {
                src_tile_key: src,
                node_id: NodeId(1),
                tile_index: 3,
                dst_tile_key: dst,
                blend_mode: BlendMode::Normal,
                kind: WriteKind::Paint,
                opacity: 1.0,
                rgb: Some([1.0, 0.0, 0.0]),
                frame_merge: GpuCmdFrameMergeTag::KeepLastInFrameByDstTile,
            })
        };

        let dst_a = TileKey::from_parts(0, 0, 1);
        let dst_b = TileKey::from_parts(0, 0, 2);
        let mut commands = vec![
            draw(buffer_tile_a, 1.0),
            write(buffer_tile_a, dst_a),
            draw(buffer_tile_b, 2.0),
            write(buffer_tile_b, dst_b),
        ];
        AppThreadIntegration::compact_frame_mergeable_draws(&mut commands);

        assert_eq!(commands.len(), 4);
        let GpuCmdMsg::DrawOp(first) = &commands[0] else {
            panic!("expected first draw op");
        };
        assert_eq!(first.tile_key, buffer_tile_a);
        assert_eq!(first.input, vec![1.0]);
        let GpuCmdMsg::WriteOp(_) = &commands[1] else {
            panic!("expected first write op");
        };
        let GpuCmdMsg::DrawOp(second) = &commands[2] else {
            panic!("expected second draw op (must not be merged away)");
        };
        assert_eq!(second.tile_key, buffer_tile_b);
        assert_eq!(second.input, vec![2.0]);
        let GpuCmdMsg::WriteOp(_) = &commands[3] else {
            panic!("expected second write op");
        };
    }

    #[test]
    fn collect_manifest_raster_assets_walks_nested_tree() {
        let root = StoredLayerNode::Branch {
            id: 2,
            label: "Root".to_string(),
            visible: true,
            opacity: 1.0,
            blend_mode: document::StoredBranchBlendMode::Base(
                document::StoredLeafBlendMode::Normal,
            ),
            children: vec![
                StoredLayerNode::SolidColorLayer {
                    id: 3,
                    label: "bg".to_string(),
                    visible: true,
                    opacity: 1.0,
                    blend_mode: document::StoredLeafBlendMode::Normal,
                    color: [1.0; 4],
                },
                StoredLayerNode::Branch {
                    id: 4,
                    label: "group".to_string(),
                    visible: true,
                    opacity: 1.0,
                    blend_mode: document::StoredBranchBlendMode::Penetrate,
                    children: vec![StoredLayerNode::RasterLayer {
                        id: 9,
                        label: "paint".to_string(),
                        visible: true,
                        opacity: 1.0,
                        blend_mode: document::StoredLeafBlendMode::Multiply,
                        image: document::RasterLayerAssetMetadata {
                            node_id: 9,
                            file_name: "layers/9.png".to_string(),
                            width: 8,
                            height: 4,
                        },
                    }],
                },
            ],
        };

        assert_eq!(
            collect_manifest_raster_assets(&root),
            vec![(9, "layers/9.png")]
        );
    }

    #[test]
    fn save_and_load_png_rgba8_round_trip() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("glaphica-pkg-test-{unique}"));
        let path = dir.join("layers/test.png");
        let image = StoredImage::new_rgba8(
            2,
            2,
            vec![
                255, 0, 0, 255, 0, 255, 0, 128, 0, 0, 255, 64, 255, 255, 255, 0,
            ],
        )
        .unwrap();

        save_png_rgba8(&path, &image).unwrap();
        let loaded = load_png_rgba8(&path).unwrap();

        assert_eq!(loaded, image);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn packed_document_file_round_trip_through_gzip_json() {
        let package = PackedDocumentFile {
            manifest: document::DocumentStorageManifest {
                version: 1,
                name: "demo".to_string(),
                canvas_width: 2,
                canvas_height: 2,
                root: StoredLayerNode::RasterLayer {
                    id: 9,
                    label: "paint".to_string(),
                    visible: true,
                    opacity: 1.0,
                    blend_mode: document::StoredLeafBlendMode::Normal,
                    image: document::RasterLayerAssetMetadata {
                        node_id: 9,
                        file_name: "layers/9.png".to_string(),
                        width: 2,
                        height: 2,
                    },
                },
                active_node_id: Some(9),
                next_node_id: 10,
                next_layer_label_index: 2,
                next_group_label_index: 1,
            },
            layers: vec![PackedLayerAsset {
                node_id: 9,
                file_name: "layers/9.png".to_string(),
                png_bytes: encode_png_rgba8(
                    &StoredImage::new_rgba8(
                        2,
                        2,
                        vec![
                            1, 2, 3, 4, 10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120,
                        ],
                    )
                    .unwrap(),
                )
                .unwrap(),
            }],
        };

        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        serde_json::to_writer(&mut encoder, &package).unwrap();
        let compressed = encoder.finish().unwrap();
        let decoded: PackedDocumentFile =
            serde_json::from_reader(GzDecoder::new(std::io::Cursor::new(compressed))).unwrap();

        assert_eq!(decoded.manifest.name, "demo");
        assert_eq!(
            decode_png_rgba8(&decoded.layers[0].png_bytes).unwrap(),
            StoredImage::new_rgba8(
                2,
                2,
                vec![
                    1, 2, 3, 4, 10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120,
                ],
            )
            .unwrap()
        );
    }

    #[test]
    fn solid_white_document_root_image_fills_canvas() {
        let Ok(mut app) = pollster::block_on(AppThreadIntegration::new(
            "repro".to_string(),
            ImageLayout::new(1024, 1024),
        )) else {
            return;
        };
        let tree = app.engine_state.shared_tree().read();
        let root_id = tree.root_id.unwrap();
        let root_image = tree
            .nodes
            .get(&root_id)
            .unwrap()
            .kind
            .render_image()
            .unwrap();
        let image = app.main_state.export_layer_image(root_image).unwrap();

        assert_eq!(image.width(), 1024);
        assert_eq!(image.height(), 1024);
        assert_eq!(&image.pixels_rgba8()[..4], &[255, 255, 255, 255]);

        let top_right = ((1024 - 1) * 4) as usize;
        assert_eq!(
            &image.pixels_rgba8()[top_right..top_right + 4],
            &[255, 255, 255, 255]
        );

        let bottom_left = (((1024 - 1) * 1024) * 4) as usize;
        assert_eq!(
            &image.pixels_rgba8()[bottom_left..bottom_left + 4],
            &[255, 255, 255, 255]
        );

        let bottom_right = (((1024 * 1024) - 1) * 4) as usize;
        assert_eq!(
            &image.pixels_rgba8()[bottom_right..bottom_right + 4],
            &[255, 255, 255, 255]
        );
    }

    #[test]
    fn issue10_replay_preserves_full_updated_tile_metadata_without_present() {
        let _guard = issue10_gpu_test_guard();
        let output = run_issue10_headless_replay();

        let expected_updated = issue10_updated_tile_indices();
        let updated_frame = output
            .frames
            .iter()
            .find(|frame| {
                frame
                    .tile_timeline
                    .as_ref()
                    .is_some_and(|timeline| timeline.updated_tile_indices == expected_updated)
            })
            .expect("updated replay frame");

        let tile_update_tiles = updated_frame
            .commands
            .iter()
            .filter_map(|command| match command {
                crate::trace::TraceGpuCmd::TileSlotKeyUpdate(update) => Some(
                    update
                        .updates
                        .iter()
                        .map(|(_, tile_index, _)| *tile_index)
                        .collect::<Vec<_>>(),
                ),
                _ => None,
            })
            .collect::<Vec<_>>();
        let mut flattened_updated_tiles = tile_update_tiles
            .iter()
            .flat_map(|tiles| tiles.iter().copied())
            .collect::<Vec<_>>();
        flattened_updated_tiles.sort_unstable();

        assert_eq!(flattened_updated_tiles, expected_updated);
        assert_eq!(tile_update_tiles.len(), 7);
        assert!(tile_update_tiles.iter().all(|tiles| tiles.len() == 4));
        assert!(updated_frame.submit_render.is_some());
    }

    #[test]
    fn issue10_replay_should_not_drop_any_updated_tiles() {
        let _guard = issue10_gpu_test_guard();
        let Ok(mut app) = pollster::block_on(AppThreadIntegration::new(
            "repro".to_string(),
            ImageLayout::new(1024, 1024),
        )) else {
            panic!("failed to initialize app integration");
        };
        register_default_test_brushes(&mut app);

        let replay = TraceRecorder::load_input_file(&issue10_replay_input_path()).unwrap();
        let expected_updated = issue10_updated_tile_indices();
        let expected_drawn = issue10_non_missing_updated_tile_indices();
        let expected_missing = issue10_missing_tile_indices();
        let mut replay_index = 0usize;
        let mut replay_finished = false;
        let mut iterations = 0usize;

        loop {
            iterations += 1;
            assert!(iterations < 4096, "headless replay loop did not quiesce");

            let has_work = if !replay_finished {
                if let Some(frame) = replay.frames.get(replay_index) {
                    replay_index += 1;
                    app.process_replay_input_frame(frame)
                } else {
                    replay_finished = true;
                    app.process_engine_frame(Duration::from_millis(0))
                }
            } else {
                app.process_engine_frame(Duration::from_millis(0))
            };

            let has_pending_engine_gpu_commands = app.has_pending_engine_gpu_commands();
            let has_pending_visible_tile_updates = app.has_pending_visible_tile_updates();
            let has_pending_render_work = app.has_pending_render_work();

            if has_work
                || has_pending_engine_gpu_commands
                || has_pending_visible_tile_updates
                || has_pending_render_work
            {
                let mut updated_tiles_in_accepted_commands = app
                    .gpu_commands
                    .iter()
                    .filter_map(|cmd| match cmd {
                        GpuCmdMsg::TileSlotKeyUpdate(update) => {
                            Some(update.updates.iter().map(|(_, tile_index, _)| *tile_index))
                        }
                        _ => None,
                    })
                    .flatten()
                    .collect::<Vec<_>>();
                updated_tiles_in_accepted_commands.sort_unstable();
                updated_tiles_in_accepted_commands.dedup();
                let mut drawn_tiles_in_accepted_commands = app
                    .gpu_commands
                    .iter()
                    .filter_map(|cmd| match cmd {
                        GpuCmdMsg::DrawOp(draw_op) => Some(draw_op.tile_index),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                drawn_tiles_in_accepted_commands.sort_unstable();
                drawn_tiles_in_accepted_commands.dedup();

                let _ = app.process_main_render();

                if updated_tiles_in_accepted_commands == expected_updated
                    && drawn_tiles_in_accepted_commands == expected_drawn
                {
                    let root_image = export_current_root_image(&mut app);
                    let missing_non_white = expected_missing
                        .iter()
                        .map(|&tile_index| stored_image_tile_non_white_sum(&root_image, tile_index))
                        .collect::<Vec<_>>();

                    assert!(
                        missing_non_white.iter().any(|&value| value > 0),
                        "updated tiles should already be visible in root image, missing={missing_non_white:?}"
                    );
                    return;
                }
            }
        }
    }

    #[test]
    fn issue10_updated_tiles_are_visible_in_root_image_during_failing_command_iteration() {
        let _guard = issue10_gpu_test_guard();
        let Ok(mut app) = pollster::block_on(AppThreadIntegration::new(
            "repro".to_string(),
            ImageLayout::new(1024, 1024),
        )) else {
            panic!("failed to initialize app integration");
        };
        register_default_test_brushes(&mut app);

        let replay = TraceRecorder::load_input_file(&issue10_replay_input_path()).unwrap();
        let expected_updated = issue10_updated_tile_indices();
        let expected_drawn = issue10_non_missing_updated_tile_indices();
        let expected_missing = issue10_missing_tile_indices();
        let mut replay_index = 0usize;
        let mut replay_finished = false;
        let mut iterations = 0usize;

        loop {
            iterations += 1;
            assert!(iterations < 4096, "headless replay loop did not quiesce");

            let has_work = if !replay_finished {
                if let Some(frame) = replay.frames.get(replay_index) {
                    replay_index += 1;
                    app.process_replay_input_frame(frame)
                } else {
                    replay_finished = true;
                    app.process_engine_frame(Duration::from_millis(0))
                }
            } else {
                app.process_engine_frame(Duration::from_millis(0))
            };

            let has_pending_engine_gpu_commands = app.has_pending_engine_gpu_commands();
            let has_pending_visible_tile_updates = app.has_pending_visible_tile_updates();
            let has_pending_render_work = app.has_pending_render_work();

            if has_work
                || has_pending_engine_gpu_commands
                || has_pending_visible_tile_updates
                || has_pending_render_work
            {
                let mut updated_tiles_in_accepted_commands = app
                    .gpu_commands
                    .iter()
                    .filter_map(|cmd| match cmd {
                        GpuCmdMsg::TileSlotKeyUpdate(update) => {
                            Some(update.updates.iter().map(|(_, tile_index, _)| *tile_index))
                        }
                        _ => None,
                    })
                    .flatten()
                    .collect::<Vec<_>>();
                updated_tiles_in_accepted_commands.sort_unstable();
                updated_tiles_in_accepted_commands.dedup();
                let mut drawn_tiles_in_accepted_commands = app
                    .gpu_commands
                    .iter()
                    .filter_map(|cmd| match cmd {
                        GpuCmdMsg::DrawOp(draw_op) => Some(draw_op.tile_index),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                drawn_tiles_in_accepted_commands.sort_unstable();
                drawn_tiles_in_accepted_commands.dedup();

                let _ = app.process_main_render();

                if updated_tiles_in_accepted_commands == expected_updated
                    && drawn_tiles_in_accepted_commands == expected_drawn
                {
                    let root_image = export_current_root_image(&mut app);
                    let missing_non_white = expected_missing
                        .iter()
                        .map(|&tile_index| stored_image_tile_non_white_sum(&root_image, tile_index))
                        .collect::<Vec<_>>();

                    assert!(missing_non_white.iter().any(|&value| value > 0));
                    return;
                }
            }
        }
    }

    #[test]
    fn move_metadata_updates_to_end_can_push_tile_updates_into_later_chunk() {
        let dst_tiles = (0..7)
            .map(|index| TileKey::from_parts(0, 0, index + 1))
            .collect::<Vec<_>>();
        let buffer_tiles = (0..7)
            .map(|index| TileKey::from_parts(2, 0, index + 11))
            .collect::<Vec<_>>();
        let mut commands = Vec::new();

        for (group_index, (&dst_tile, &buffer_tile)) in
            dst_tiles.iter().zip(&buffer_tiles).enumerate()
        {
            commands.push(GpuCmdMsg::DrawOp(DrawOp {
                stroke_ctx: Some(DrawStrokeCtx {
                    node_id: NodeId(1),
                    blend_mode: BlendMode::Additive,
                    frame_merge: DrawFrameMergePolicy::None,
                    rgb: [1.0, 0.0, 0.0],
                    brush_id: BrushId(2),
                }),
                tile_index: group_index,
                tile_key: buffer_tile,
                origin_tile: TileKey::EMPTY,
                ref_image: None,
                input: vec![group_index as f32],
                stroke_id: StrokeId(5),
            }));
            commands.push(GpuCmdMsg::WriteOp(WriteOp {
                src_tile_key: buffer_tile,
                node_id: NodeId(1),
                tile_index: group_index,
                dst_tile_key: dst_tile,
                blend_mode: BlendMode::Normal,
                kind: WriteKind::Paint,
                opacity: 1.0,
                rgb: Some([1.0, 0.0, 0.0]),
                frame_merge: GpuCmdFrameMergeTag::KeepLastInFrameByDstTile,
            }));
            commands.push(GpuCmdMsg::TileSlotKeyUpdate(
                thread_protocol::TileSlotKeyUpdateMsg {
                    updates: vec![(NodeId(1), group_index, dst_tile)],
                },
            ));
        }

        AppThreadIntegration::move_mergeable_writes_to_end(&mut commands);
        AppThreadIntegration::move_metadata_updates_to_end(&mut commands);

        let split_at = 14usize;
        let first_chunk = &commands[..split_at];
        let second_chunk = &commands[split_at..];
        let first_chunk_update_count = first_chunk
            .iter()
            .filter(|command| matches!(command, GpuCmdMsg::TileSlotKeyUpdate(_)))
            .count();
        let second_chunk_update_count = second_chunk
            .iter()
            .filter(|command| matches!(command, GpuCmdMsg::TileSlotKeyUpdate(_)))
            .count();

        assert_eq!(first_chunk_update_count, 0);
        assert!(second_chunk_update_count > 0);
    }

    #[test]
    fn issue10_headless_root_image_contains_ink_on_updated_tiles() {
        let _guard = issue10_gpu_test_guard();
        let (root_image, non_root_images) = run_issue10_headless_replay_and_export_images();
        let expected_updated = issue10_updated_tile_indices();
        let root_updated_non_white = expected_updated
            .iter()
            .map(|&tile_index| stored_image_tile_non_white_sum(&root_image, tile_index))
            .collect::<Vec<_>>();

        for tile_index in expected_updated {
            assert!(stored_image_tile_alpha_sum(&root_image, tile_index) > 0);
        }

        assert!(root_updated_non_white.iter().any(|&value| value > 0));
        assert!(non_root_images.iter().any(|(_, image)| {
            issue10_updated_tile_indices()
                .iter()
                .any(|&tile_index| stored_image_tile_non_white_sum(image, tile_index) > 0)
        }));
    }

    #[test]
    fn issue10_frontend_readback_matches_root_export_after_replay() {
        let _guard = issue10_gpu_test_guard();
        let (root_image, frontend_image) = run_issue10_headless_replay_and_capture_frontend_image();
        let missing_tiles = issue10_missing_tile_indices();
        let drawn_tiles = vec![
            62usize, 63, 64, 65, 79, 80, 81, 82, 96, 97, 98, 99, 113, 114, 115, 116,
        ];
        let root_missing_non_white = missing_tiles
            .iter()
            .map(|&tile_index| stored_image_tile_non_white_sum(&root_image, tile_index))
            .collect::<Vec<_>>();
        let frontend_missing_non_white = missing_tiles
            .iter()
            .map(|&tile_index| stored_image_tile_non_white_sum(&frontend_image, tile_index))
            .collect::<Vec<_>>();
        let frontend_drawn_non_white = drawn_tiles
            .iter()
            .map(|&tile_index| stored_image_tile_non_white_sum(&frontend_image, tile_index))
            .collect::<Vec<_>>();

        assert_eq!(frontend_missing_non_white, root_missing_non_white);
        assert!(frontend_drawn_non_white.iter().any(|&value| value > 0));
    }
}
