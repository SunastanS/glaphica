//! Renderer crate root.
//!
//! This module defines the public API (`Renderer`, `ViewOpSender`, `RenderDataResolver`)
//! and wires internal modules around state compartments used by the frame pipeline.
//!
//! Internal architecture overview:
//! - `renderer_init`: constructs GPU resources and initial state.
//! - `renderer_view_ops`: ingests `RenderOp` and mutates view/frame state.
//! - `renderer_frame`: builds `FramePlan` and orchestrates pass execution.
//! - `renderer_composite`: builds and executes `CompositeNodePlan` trees.
//! - `renderer_cache_draw`: maintains group cache and submits draw runs.
//! - `dirty`/`planning`/`render_tree`/`geometry`: domain logic shared by orchestration modules.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::mpsc;

use render_protocol::{
    BlendMode, BrushId, BrushProgramKey, BrushRenderCommand, ImageHandle, LayerId, ProgramRevision,
    ReferenceLayerSelection, ReferenceSetId, RenderOp, RenderTreeSnapshot, TransformMatrix4x4,
    Viewport,
};
#[cfg(test)]
use tiles::TILE_STRIDE;
use tiles::{
    GroupTileAtlasGpuArray, GroupTileAtlasStore, TILE_SIZE, TileAddress, TileAtlasGpuArray,
    TileAtlasLayout, TileGpuDrainError, TileKey, VirtualImage,
};

#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct TileInstanceGpu {
    pub document_x: f32,
    pub document_y: f32,
    pub atlas_layer: f32,
    pub tile_index: u32,
    pub _padding0: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct TileTextureManagerGpu {
    atlas_width: f32,
    atlas_height: f32,
    tiles_per_row: u32,
    _padding0: u32,
}

impl TileTextureManagerGpu {
    fn from_layout(layout: TileAtlasLayout) -> Self {
        Self {
            atlas_width: layout.atlas_width as f32,
            atlas_height: layout.atlas_height as f32,
            tiles_per_row: layout.tiles_per_row,
            _padding0: 0,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct TileDrawInstance {
    pub blend_mode: BlendMode,
    pub tile: TileInstanceGpu,
}

use dirty::{
    DirtyPropagationEngine, DirtyRectMask, DirtyStateStore, DirtyTileMask, RenderNodeKey, TileCoord,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirtyRect {
    pub min_x: i32,
    pub min_y: i32,
    pub max_x: i32,
    pub max_y: i32,
}

use geometry::{
    document_clip_matrix_from_size, group_cache_extent_from_document_size,
    group_cache_slot_extent_from_document_size, group_tile_grid_from_document_size,
};
use render_tree::{RenderTreeNode, collect_node_dirty_rects};
use renderer_draw_builders::{
    build_group_tile_draw_instances, build_leaf_tile_draw_instances,
    build_leaf_tile_draw_instances_for_tiles, leaf_should_rebuild, tile_coord_from_draw_instance,
};
use renderer_pipeline::{create_composite_pipeline, multiply_blend_state};

#[derive(Debug, Clone)]
struct CachedLeafDraw {
    blend: BlendMode,
    image_handle: ImageHandle,
    draw_instances: Vec<TileDrawInstance>,
    tile_instance_index: HashMap<TileCoord, usize>,
}

impl CachedLeafDraw {
    fn rebuild_tile_index(&mut self) {
        self.tile_instance_index.clear();
        for (index, instance) in self.draw_instances.iter().enumerate() {
            self.tile_instance_index
                .insert(tile_coord_from_draw_instance(instance), index);
        }
    }

    fn ensure_tile_index_consistent(&mut self) {
        if self.tile_instance_index.len() != self.draw_instances.len() {
            self.rebuild_tile_index();
        }
    }

    fn replace_all_instances(
        &mut self,
        blend: BlendMode,
        image_handle: ImageHandle,
        draw_instances: Vec<TileDrawInstance>,
    ) {
        self.blend = blend;
        self.image_handle = image_handle;
        self.draw_instances = draw_instances;
        self.rebuild_tile_index();
    }

    fn replace_partial_tiles(&mut self, partial_tiles: &HashSet<TileCoord>) {
        self.ensure_tile_index_consistent();
        for tile_coord in partial_tiles {
            let Some(remove_index) = self.tile_instance_index.remove(tile_coord) else {
                continue;
            };
            self.draw_instances.swap_remove(remove_index);
            if remove_index < self.draw_instances.len() {
                let moved_tile_coord =
                    tile_coord_from_draw_instance(&self.draw_instances[remove_index]);
                self.tile_instance_index
                    .insert(moved_tile_coord, remove_index);
            }
        }
    }

    fn append_instances(&mut self, new_instances: Vec<TileDrawInstance>) {
        if new_instances.is_empty() {
            return;
        }
        self.ensure_tile_index_consistent();
        for instance in new_instances {
            let tile_coord = tile_coord_from_draw_instance(&instance);
            let index = self.draw_instances.len();
            self.draw_instances.push(instance);
            self.tile_instance_index.insert(tile_coord, index);
        }
    }
}

#[derive(Debug)]
struct GroupTargetCacheEntry {
    image: VirtualImage<TileKey>,
    draw_instances: Vec<TileDrawInstance>,
    blend: BlendMode,
}

#[cfg(test)]
use planning::rerender_tiles_for_group;
use planning::{
    CompositeNodePlan, DirtyExecutionPlan, FrameExecutionResult, FramePlan, FrameSync,
    GroupDecisionEngine, GroupRerenderMode,
};

pub trait RenderDataResolver {
    fn document_size(&self) -> (u32, u32);

    fn visit_image_tiles(
        &self,
        image_handle: ImageHandle,
        visitor: &mut dyn FnMut(u32, u32, TileKey),
    );

    fn visit_image_tiles_for_coords(
        &self,
        image_handle: ImageHandle,
        tile_coords: &[(u32, u32)],
        visitor: &mut dyn FnMut(u32, u32, TileKey),
    ) {
        let requested_tiles: HashSet<(u32, u32)> = tile_coords.iter().copied().collect();
        if requested_tiles.is_empty() {
            return;
        }

        let mut filtered = |tile_x: u32, tile_y: u32, tile_key: TileKey| {
            if requested_tiles.contains(&(tile_x, tile_y)) {
                visitor(tile_x, tile_y, tile_key);
            }
        };
        self.visit_image_tiles(image_handle, &mut filtered);
    }

    // Reserved hook for future special node kinds (for example filter-driven layers that
    // expand dirty regions and may not map to a direct image handle). The final propagation
    // model is still being designed, so renderer currently uses this default identity behavior.
    fn propagate_layer_dirty_rects(
        &self,
        _layer_id: u64,
        incoming_rects: &[DirtyRect],
    ) -> Vec<DirtyRect> {
        incoming_rects.to_vec()
    }

    fn resolve_tile_address(&self, tile_key: TileKey) -> Option<TileAddress>;
}

pub struct ViewOpSender(mpsc::Sender<RenderOp>);

impl ViewOpSender {
    pub fn send(&self, operation: RenderOp) -> Result<(), mpsc::SendError<RenderOp>> {
        self.0.send(operation)
    }
}

struct ViewState {
    view_matrix: TransformMatrix4x4,
    view_matrix_dirty: bool,
    viewport: Option<Viewport>,
    brush_command_quota: u32,
    drop_before_revision: u64,
    present_requested: bool,
}

struct FrameState {
    bound_tree: Option<RenderTreeSnapshot>,
    cached_render_tree: Option<RenderTreeNode>,
    render_tree_dirty: bool,
    dirty_state_store: DirtyStateStore,
    frame_sync: FrameSync,
}

struct CacheState {
    group_target_cache: HashMap<u64, GroupTargetCacheEntry>,
    leaf_draw_cache: HashMap<u64, CachedLeafDraw>,
}

struct BrushWorkState {
    pending_commands: VecDeque<BrushRenderCommand>,
    pending_dab_count: u64,
    carry_credit_dabs: u8,
    prepared_programs: HashMap<BrushProgramKey, PreparedBrushProgram>,
    active_program_by_brush: HashMap<BrushId, ProgramRevision>,
    active_strokes: HashMap<u64, BrushProgramKey>,
    executing_strokes: HashMap<u64, BrushProgramKey>,
    reference_sets: HashMap<ReferenceSetId, ReferenceSetState>,
    stroke_reference_set: HashMap<u64, ReferenceSetId>,
    stroke_target_layer: HashMap<u64, LayerId>,
    ended_strokes_pending_merge: HashMap<u64, LayerId>,
}

struct PreparedBrushProgram {
    payload_hash: u64,
    _wgsl_source: std::sync::Arc<str>,
    compute_pipeline: wgpu::ComputePipeline,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ReferenceSetState {
    selection: ReferenceLayerSelection,
}

struct GpuState {
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    surface_config: wgpu::SurfaceConfiguration,
    view_uniform_buffer: wgpu::Buffer,
    alpha_composite_pipeline: wgpu::RenderPipeline,
    multiply_composite_pipeline: wgpu::RenderPipeline,
    alpha_composite_slot_pipeline: wgpu::RenderPipeline,
    multiply_composite_slot_pipeline: wgpu::RenderPipeline,
    per_frame_bind_group_layout: wgpu::BindGroupLayout,
    per_frame_bind_group: wgpu::BindGroup,
    group_tile_store: GroupTileAtlasStore,
    group_tile_atlas: GroupTileAtlasGpuArray,
    group_atlas_bind_group_linear: wgpu::BindGroup,
    group_atlas_bind_group_nearest: wgpu::BindGroup,
    _group_texture_manager_buffer: wgpu::Buffer,
    tile_instance_buffer: wgpu::Buffer,
    tile_instance_capacity: usize,
    tile_instance_gpu_staging: Vec<TileInstanceGpu>,
    atlas_bind_group_linear: wgpu::BindGroup,
    _tile_texture_manager_buffer: wgpu::Buffer,
    tile_atlas: TileAtlasGpuArray,
    gpu_timing: GpuFrameTimingState,
    brush_pipeline_layout: wgpu::PipelineLayout,
}

struct DataState {
    render_data_resolver: Box<dyn RenderDataResolver>,
}

struct InputState {
    view_receiver: mpsc::Receiver<RenderOp>,
}

pub struct Renderer {
    input_state: InputState,
    data_state: DataState,
    gpu_state: GpuState,
    view_state: ViewState,
    cache_state: CacheState,
    brush_work_state: BrushWorkState,

    frame_state: FrameState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrushControlError {
    ProgramNotPrepared { key: BrushProgramKey },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrushRenderEnqueueError {
    ProgramNotPrepared {
        key: BrushProgramKey,
    },
    ProgramNotActivated {
        key: BrushProgramKey,
    },
    StrokeProgramMismatch {
        stroke_session_id: u64,
        expected: BrushProgramKey,
        received: BrushProgramKey,
    },
    UnknownStroke {
        stroke_session_id: u64,
    },
    ReferenceSetMissing {
        reference_set_id: ReferenceSetId,
    },
    MergeBeforeStrokeEnd {
        stroke_session_id: u64,
    },
    MergeTargetLayerMismatch {
        stroke_session_id: u64,
        expected_layer_id: LayerId,
        received_layer_id: LayerId,
    },
    BeginWithPendingMerge,
}

const IDENTITY_MATRIX: TransformMatrix4x4 = [
    1.0, 0.0, 0.0, 0.0, // col0
    0.0, 1.0, 0.0, 0.0, // col1
    0.0, 0.0, 1.0, 0.0, // col2
    0.0, 0.0, 0.0, 1.0, // col3
];

const INITIAL_TILE_INSTANCE_CAPACITY: usize = 256;
const GROUP_FULL_DIRTY_RATIO_THRESHOLD: f32 = 0.4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TileCompositeSpace {
    Content,
    Slot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ViewportMode {
    Apply,
    Ignore,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompositeEmission {
    EmitToTarget,
    CacheOnly,
}

struct CompositePassContext<'a> {
    target_view: &'a wgpu::TextureView,
    emission: CompositeEmission,
    viewport_mode: ViewportMode,
}

struct DrawPassContext<'a> {
    target_view: &'a wgpu::TextureView,
    atlas_bind_group: &'a wgpu::BindGroup,
    visible_tiles: Option<&'a HashSet<TileCoord>>,
    viewport_mode: ViewportMode,
    composite_space: TileCompositeSpace,
}

#[derive(Debug)]
pub enum PresentError {
    Surface(wgpu::SurfaceError),
    TileDrain(TileGpuDrainError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameGpuTimingReport {
    pub frame_id: u64,
    pub gpu_time_micros: u64,
}

const GPU_TIMING_SLOTS: usize = 4;

struct GpuFrameTimingState {
    query_set: Option<wgpu::QuerySet>,
    timestamp_period_ns: f64,
    slots: Vec<GpuFrameTimingSlot>,
    latest_report: Option<FrameGpuTimingReport>,
}

struct GpuFrameTimingSlot {
    resolve_buffer: wgpu::Buffer,
    readback_buffer: wgpu::Buffer,
    state: GpuFrameTimingSlotState,
}

enum GpuFrameTimingSlotState {
    Idle,
    Submitted {
        frame_id: u64,
    },
    Mapping {
        frame_id: u64,
        receiver: mpsc::Receiver<Result<(), wgpu::BufferAsyncError>>,
    },
}

fn dirty_rect_to_tile_coords(dirty_rect: DirtyRect) -> HashSet<TileCoord> {
    if dirty_rect.min_x >= dirty_rect.max_x || dirty_rect.min_y >= dirty_rect.max_y {
        return HashSet::new();
    }

    let min_x = dirty_rect.min_x.max(0) as u32;
    let min_y = dirty_rect.min_y.max(0) as u32;
    let max_x = dirty_rect.max_x.max(0) as u32;
    let max_y = dirty_rect.max_y.max(0) as u32;
    if min_x >= max_x || min_y >= max_y {
        return HashSet::new();
    }

    let start_tile_x = min_x / TILE_SIZE;
    let start_tile_y = min_y / TILE_SIZE;
    let end_tile_x = max_x.saturating_sub(1) / TILE_SIZE;
    let end_tile_y = max_y.saturating_sub(1) / TILE_SIZE;

    let mut tiles = HashSet::new();
    for tile_y in start_tile_y..=end_tile_y {
        for tile_x in start_tile_x..=end_tile_x {
            tiles.insert(TileCoord { tile_x, tile_y });
        }
    }
    tiles
}

mod dirty;

mod planning;

mod render_tree;

mod geometry;

mod renderer_cache_draw;

mod renderer_init;

mod renderer_frame;

mod renderer_composite;

mod renderer_draw_builders;

mod renderer_pipeline;

mod renderer_view_ops;

#[cfg(test)]
mod tests;
