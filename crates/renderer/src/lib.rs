use std::collections::{HashMap, HashSet};
use std::sync::mpsc;

use render_protocol::{
    BlendMode, ImageHandle, RenderOp, RenderStepSnapshot, TransformMatrix4x4, Viewport,
};
#[cfg(test)]
use render_protocol::RenderStepEntry;
use tiles::{
    GroupTileAtlasGpuArray, GroupTileAtlasStore, TILE_SIZE, TileAddress, TileAtlasGpuArray,
    TileGpuDrainError, TileKey, VirtualImage,
};
#[cfg(test)]
use tiles::TILE_STRIDE;

#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct TileInstanceGpu {
    pub document_x: f32,
    pub document_y: f32,
    pub atlas_layer: f32,
    pub atlas_u: f32,
    pub atlas_v: f32,
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
use render_tree::{RenderTreeNode, build_render_tree_from_snapshot, collect_node_dirty_rects};

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

pub struct Renderer {
    view_receiver: mpsc::Receiver<RenderOp>,

    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    surface_config: wgpu::SurfaceConfiguration,

    render_data_resolver: Box<dyn RenderDataResolver>,

    view_matrix: TransformMatrix4x4,
    view_matrix_dirty: bool,
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
    group_target_cache: HashMap<u64, GroupTargetCacheEntry>,
    tile_instance_buffer: wgpu::Buffer,
    tile_instance_capacity: usize,
    tile_instance_gpu_staging: Vec<TileInstanceGpu>,
    atlas_bind_group_linear: wgpu::BindGroup,
    tile_atlas: TileAtlasGpuArray,

    viewport: Option<Viewport>,
    bound_steps: Option<RenderStepSnapshot>,
    cached_render_tree: Option<RenderTreeNode>,
    render_tree_dirty: bool,
    leaf_draw_cache: HashMap<u64, CachedLeafDraw>,
    dirty_state_store: DirtyStateStore,
    frame_budget_micros: u32,
    drop_before_revision: u64,
    present_requested: bool,
    frame_sync: FrameSync,
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

impl Renderer {
    pub fn present_frame(&mut self, frame_id: u64) -> Result<(), PresentError> {
        self.tile_atlas
            .drain_and_execute(&self.queue)
            .map_err(PresentError::TileDrain)?;

        let frame_plan = self.build_frame_plan(frame_id);

        let frame = self
            .surface
            .get_current_texture()
            .map_err(PresentError::Surface)?;
        let frame_view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        {
            let mut clear_encoder =
                self.device
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                        label: Some("renderer.frame.clear"),
                    });
            {
                let _clear_pass = clear_encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("renderer.clear"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &frame_view,
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color {
                                r: 0.07,
                                g: 0.08,
                                b: 0.09,
                                a: 1.0,
                            }),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
            }
            self.queue.submit(Some(clear_encoder.finish()));
        }

        let frame_result = self.execute_frame_plan(frame_plan, &frame_view);

        frame.present();
        self.commit_frame_result(frame_result);
        Ok(())
    }

    fn snapshot_revision(&self) -> u64 {
        self.bound_steps
            .as_ref()
            .map_or(0, |snapshot| snapshot.revision)
    }

    fn build_frame_plan(&mut self, frame_id: u64) -> FramePlan {
        if self.render_tree_dirty {
            self.cached_render_tree = self
                .bound_steps
                .as_ref()
                .map(build_render_tree_from_snapshot);
            self.render_tree_dirty = false;
        }

        let render_tree = self.cached_render_tree.take();
        let layer_dirty_rect_masks = self
            .dirty_state_store
            .resolve_layer_dirty_rect_masks(self.render_data_resolver.as_ref());
        let force_group_rerender = self.dirty_state_store.is_document_composite_dirty();
        let mut dirty_leaf_tiles = HashMap::new();
        let mut dirty_group_tiles = HashMap::new();
        if let Some(render_tree) = render_tree.as_ref() {
            for (layer_id, dirty_rect_mask) in &layer_dirty_rect_masks {
                if matches!(dirty_rect_mask, DirtyRectMask::Full) {
                    dirty_leaf_tiles.insert(*layer_id, DirtyTileMask::Full);
                }
            }
            if !force_group_rerender {
                let (tiles_per_row, tiles_per_column) = self.group_tile_grid();
                let group_tile_count = usize::try_from(
                    tiles_per_row
                        .checked_mul(tiles_per_column)
                        .expect("group tile count overflow"),
                )
                .expect("group tile count exceeds usize");
                let propagation_engine = DirtyPropagationEngine::new(group_tile_count);
                let node_dirty_tiles = propagation_engine
                    .collect_node_tile_masks(render_tree, &layer_dirty_rect_masks);
                for (node_key, dirty_tile_mask) in node_dirty_tiles {
                    match node_key {
                        RenderNodeKey::Leaf(layer_id) => {
                            dirty_leaf_tiles.insert(layer_id, dirty_tile_mask);
                        }
                        RenderNodeKey::Group(group_id) => {
                            dirty_group_tiles.insert(group_id, dirty_tile_mask);
                        }
                    }
                }
            }
        }

        let dirty_plan = DirtyExecutionPlan {
            force_group_rerender,
            dirty_leaf_tiles,
            dirty_group_tiles,
        };
        let composite_plan = render_tree
            .as_ref()
            .map(|render_tree| self.build_composite_node_plan(render_tree, &dirty_plan, None));

        FramePlan {
            version: self.frame_sync.version(frame_id, self.snapshot_revision()),
            render_tree,
            composite_plan,
            composite_matrix: self.group_cache_slot_matrix(),
        }
    }

    fn execute_frame_plan(
        &mut self,
        frame_plan: FramePlan,
        frame_view: &wgpu::TextureView,
    ) -> FrameExecutionResult {
        if let Some(composite_plan) = frame_plan.composite_plan.as_ref() {
            self.queue.write_buffer(
                &self.view_uniform_buffer,
                0,
                bytemuck::bytes_of(&frame_plan.composite_matrix),
            );

            let mut composite_encoder =
                self.device
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                        label: Some("renderer.frame.composite"),
                    });
            let composite_context = CompositePassContext {
                target_view: frame_view,
                emission: CompositeEmission::CacheOnly,
                viewport_mode: ViewportMode::Ignore,
            };
            self.render_composite_node_plan(
                composite_plan,
                &composite_context,
                &mut composite_encoder,
            );
            self.queue.submit(Some(composite_encoder.finish()));

            self.queue.write_buffer(
                &self.view_uniform_buffer,
                0,
                bytemuck::bytes_of(&self.view_matrix),
            );
            self.view_matrix_dirty = false;

            let mut view_encoder =
                self.device
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                        label: Some("renderer.frame.view"),
                    });
            self.draw_root_group_to_surface(frame_view, &mut view_encoder);
            self.queue.submit(Some(view_encoder.finish()));
        }

        FrameExecutionResult {
            version: frame_plan.version,
            render_tree: frame_plan.render_tree,
        }
    }

    fn build_composite_node_plan(
        &self,
        node: &RenderTreeNode,
        dirty_plan: &DirtyExecutionPlan,
        active_tiles: Option<&HashSet<TileCoord>>,
    ) -> CompositeNodePlan {
        match node {
            RenderTreeNode::Leaf {
                layer_id,
                blend,
                image_handle,
            } => {
                let leaf_dirty_tiles = dirty_plan.dirty_leaf_tiles.get(layer_id);
                let should_rebuild = leaf_should_rebuild(
                    leaf_dirty_tiles,
                    self.leaf_draw_cache.get(layer_id),
                    *blend,
                    *image_handle,
                );
                CompositeNodePlan::Leaf {
                    layer_id: *layer_id,
                    blend: *blend,
                    image_handle: *image_handle,
                    should_rebuild,
                    dirty_tiles: leaf_dirty_tiles.cloned(),
                    visible_tiles: active_tiles.cloned(),
                }
            }
            RenderTreeNode::Group {
                group_id,
                blend,
                children,
            } => {
                let cache_missing = !self.group_target_cache.contains_key(group_id);
                let group_dirty = dirty_plan.dirty_group_tiles.get(group_id);
                let decision = GroupDecisionEngine::default().decide(
                    dirty_plan.force_group_rerender,
                    cache_missing,
                    group_dirty,
                    active_tiles,
                );
                let child_plans = if matches!(decision.mode, GroupRerenderMode::Rerender) {
                    children
                        .iter()
                        .map(|child| {
                            self.build_composite_node_plan(
                                child,
                                dirty_plan,
                                decision.rerender_tiles.as_ref(),
                            )
                        })
                        .collect()
                } else {
                    Vec::new()
                };
                let emit_tiles = if matches!(decision.mode, GroupRerenderMode::UseCache) {
                    active_tiles.cloned()
                } else {
                    decision.rerender_tiles.clone()
                };
                CompositeNodePlan::Group {
                    group_id: *group_id,
                    blend: *blend,
                    decision,
                    emit_tiles,
                    children: child_plans,
                }
            }
        }
    }

    fn render_composite_node_plan(
        &mut self,
        node_plan: &CompositeNodePlan,
        context: &CompositePassContext<'_>,
        encoder: &mut wgpu::CommandEncoder,
    ) {
        match node_plan {
            CompositeNodePlan::Leaf {
                layer_id,
                blend,
                image_handle,
                should_rebuild,
                dirty_tiles,
                visible_tiles,
            } => {
                let mut cached_leaf =
                    self.leaf_draw_cache
                        .remove(layer_id)
                        .unwrap_or_else(|| CachedLeafDraw {
                            blend: *blend,
                            image_handle: *image_handle,
                            draw_instances: Vec::new(),
                            tile_instance_index: HashMap::new(),
                        });
                if *should_rebuild {
                    let partial_tiles = match dirty_tiles {
                        Some(DirtyTileMask::Partial(tiles))
                            if cached_leaf.blend == *blend
                                && cached_leaf.image_handle == *image_handle
                                && !cached_leaf.draw_instances.is_empty() =>
                        {
                            Some(tiles)
                        }
                        _ => None,
                    };

                    if let Some(partial_tiles) = partial_tiles {
                        cached_leaf.replace_partial_tiles(partial_tiles);
                        let partial_instances = build_leaf_tile_draw_instances_for_tiles(
                            *blend,
                            *image_handle,
                            self.render_data_resolver.as_ref(),
                            partial_tiles,
                        );
                        cached_leaf.append_instances(partial_instances);
                    } else {
                        let full_instances = build_leaf_tile_draw_instances(
                            *blend,
                            *image_handle,
                            self.render_data_resolver.as_ref(),
                        );
                        cached_leaf.replace_all_instances(*blend, *image_handle, full_instances);
                    }
                }

                let atlas_bind_group = self.atlas_bind_group_linear.clone();
                let draw_context = DrawPassContext {
                    target_view: context.target_view,
                    atlas_bind_group: &atlas_bind_group,
                    visible_tiles: visible_tiles.as_ref(),
                    viewport_mode: context.viewport_mode,
                    composite_space: TileCompositeSpace::Slot,
                };
                self.draw_tile_instances_to_target(
                    &cached_leaf.draw_instances,
                    encoder,
                    &draw_context,
                );
                self.leaf_draw_cache.insert(*layer_id, cached_leaf);
            }
            CompositeNodePlan::Group {
                group_id,
                blend,
                decision,
                emit_tiles,
                children,
            } => {
                if matches!(decision.mode, GroupRerenderMode::UseCache) {
                    let mut group_target = self
                        .group_target_cache
                        .remove(group_id)
                        .expect("group target cache must contain clean group");
                    if group_target.blend != *blend {
                        group_target.blend = *blend;
                        group_target.draw_instances = build_group_tile_draw_instances(
                            &group_target.image,
                            *blend,
                            &self.group_tile_store,
                        );
                    }
                    if matches!(context.emission, CompositeEmission::EmitToTarget) {
                        let group_atlas_bind_group = self.group_atlas_bind_group_linear.clone();
                        let draw_context = DrawPassContext {
                            target_view: context.target_view,
                            atlas_bind_group: &group_atlas_bind_group,
                            visible_tiles: emit_tiles.as_ref(),
                            viewport_mode: context.viewport_mode,
                            composite_space: TileCompositeSpace::Slot,
                        };
                        self.draw_tile_instances_to_target_with_bind_group(
                            &group_target.draw_instances,
                            encoder,
                            &draw_context,
                        );
                    }
                    self.group_target_cache.insert(*group_id, group_target);
                    return;
                }

                if decision
                    .rerender_tiles
                    .as_ref()
                    .is_some_and(|tiles| tiles.is_empty())
                {
                    let group_target = self
                        .group_target_cache
                        .remove(group_id)
                        .expect("group cache must exist for empty rerender tile set");
                    if matches!(context.emission, CompositeEmission::EmitToTarget) {
                        let group_atlas_bind_group = self.group_atlas_bind_group_linear.clone();
                        let draw_context = DrawPassContext {
                            target_view: context.target_view,
                            atlas_bind_group: &group_atlas_bind_group,
                            visible_tiles: emit_tiles.as_ref(),
                            viewport_mode: context.viewport_mode,
                            composite_space: TileCompositeSpace::Slot,
                        };
                        self.draw_tile_instances_to_target_with_bind_group(
                            &group_target.draw_instances,
                            encoder,
                            &draw_context,
                        );
                    }
                    self.group_target_cache.insert(*group_id, group_target);
                    return;
                }

                let (group_target_texture, group_target_view) = self.create_group_target_scratch();

                {
                    let _clear_group_pass =
                        encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                            label: Some("renderer.group_clear"),
                            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                                view: &group_target_view,
                                resolve_target: None,
                                depth_slice: None,
                                ops: wgpu::Operations {
                                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                                    store: wgpu::StoreOp::Store,
                                },
                            })],
                            depth_stencil_attachment: None,
                            timestamp_writes: None,
                            occlusion_query_set: None,
                            multiview_mask: None,
                        });
                }

                for child in children {
                    let child_context = CompositePassContext {
                        target_view: &group_target_view,
                        emission: CompositeEmission::EmitToTarget,
                        viewport_mode: ViewportMode::Ignore,
                    };
                    self.render_composite_node_plan(child, &child_context, encoder);
                }

                self.update_group_cache_from_texture(
                    *group_id,
                    &group_target_texture,
                    encoder,
                    decision.rerender_tiles.as_ref(),
                );
                let mut group_target = self
                    .group_target_cache
                    .remove(group_id)
                    .expect("group cache must contain rerendered group");
                if group_target.blend != *blend {
                    group_target.blend = *blend;
                    group_target.draw_instances = build_group_tile_draw_instances(
                        &group_target.image,
                        *blend,
                        &self.group_tile_store,
                    );
                }
                if matches!(context.emission, CompositeEmission::EmitToTarget) {
                    let group_atlas_bind_group = self.group_atlas_bind_group_linear.clone();
                    let draw_context = DrawPassContext {
                        target_view: context.target_view,
                        atlas_bind_group: &group_atlas_bind_group,
                        visible_tiles: emit_tiles.as_ref(),
                        viewport_mode: context.viewport_mode,
                        composite_space: TileCompositeSpace::Slot,
                    };
                    self.draw_tile_instances_to_target_with_bind_group(
                        &group_target.draw_instances,
                        encoder,
                        &draw_context,
                    );
                }
                self.group_target_cache.insert(*group_id, group_target);
            }
        }
    }

    fn commit_frame_result(&mut self, frame_result: FrameExecutionResult) {
        assert!(
            self.frame_sync
                .can_commit(frame_result.version, self.snapshot_revision()),
            "frame result must match current renderer epoch and snapshot"
        );
        self.cached_render_tree = frame_result.render_tree;
        self.dirty_state_store.clear_layer_dirty_masks();
        self.dirty_state_store.clear_document_composite_dirty();
        self.frame_sync
            .commit(frame_result.version, self.snapshot_revision());
    }

}

fn build_leaf_tile_draw_instances(
    blend: BlendMode,
    image_handle: ImageHandle,
    render_data_resolver: &dyn RenderDataResolver,
) -> Vec<TileDrawInstance> {
    let mut draw_instances = Vec::new();
    let mut collect_tile = |tile_x: u32, tile_y: u32, tile_key: TileKey| {
        let Some(address) = render_data_resolver.resolve_tile_address(tile_key) else {
            return;
        };
        let (atlas_u, atlas_v) = address.atlas_uv_origin();
        let document_x = tile_x
            .checked_mul(TILE_SIZE)
            .expect("tile x position overflow") as f32;
        let document_y = tile_y
            .checked_mul(TILE_SIZE)
            .expect("tile y position overflow") as f32;

        draw_instances.push(TileDrawInstance {
            blend_mode: blend,
            tile: TileInstanceGpu {
                document_x,
                document_y,
                atlas_layer: address.atlas_layer as f32,
                atlas_u,
                atlas_v,
            },
        });
    };
    render_data_resolver.visit_image_tiles(image_handle, &mut collect_tile);
    draw_instances
}

fn build_leaf_tile_draw_instances_for_tiles(
    blend: BlendMode,
    image_handle: ImageHandle,
    render_data_resolver: &dyn RenderDataResolver,
    tiles: &HashSet<TileCoord>,
) -> Vec<TileDrawInstance> {
    if tiles.is_empty() {
        return Vec::new();
    }

    let requested_coords: Vec<(u32, u32)> = tiles
        .iter()
        .map(|coord| (coord.tile_x, coord.tile_y))
        .collect();
    let mut draw_instances = Vec::new();
    let mut collect_tile = |tile_x: u32, tile_y: u32, tile_key: TileKey| {
        let Some(address) = render_data_resolver.resolve_tile_address(tile_key) else {
            return;
        };
        let (atlas_u, atlas_v) = address.atlas_uv_origin();
        let document_x = tile_x
            .checked_mul(TILE_SIZE)
            .expect("tile x position overflow") as f32;
        let document_y = tile_y
            .checked_mul(TILE_SIZE)
            .expect("tile y position overflow") as f32;

        draw_instances.push(TileDrawInstance {
            blend_mode: blend,
            tile: TileInstanceGpu {
                document_x,
                document_y,
                atlas_layer: address.atlas_layer as f32,
                atlas_u,
                atlas_v,
            },
        });
    };
    render_data_resolver.visit_image_tiles_for_coords(
        image_handle,
        &requested_coords,
        &mut collect_tile,
    );
    draw_instances
}

fn build_group_tile_draw_instances(
    image: &VirtualImage<TileKey>,
    blend: BlendMode,
    tile_store: &GroupTileAtlasStore,
) -> Vec<TileDrawInstance> {
    image
        .iter_tiles()
        .map(|(tile_x, tile_y, tile_key)| {
            let tile_address = tile_store
                .resolve(*tile_key)
                .expect("group tile key must resolve to atlas address");
            let (atlas_u, atlas_v) = tile_address.atlas_uv_origin();
            let document_x = tile_x
                .checked_mul(TILE_SIZE)
                .expect("group tile x position overflow") as f32;
            let document_y = tile_y
                .checked_mul(TILE_SIZE)
                .expect("group tile y position overflow") as f32;
            TileDrawInstance {
                blend_mode: blend,
                tile: TileInstanceGpu {
                    document_x,
                    document_y,
                    atlas_layer: tile_address.atlas_layer as f32,
                    atlas_u,
                    atlas_v,
                },
            }
        })
        .collect()
}

fn tile_coord_from_draw_instance(instance: &TileDrawInstance) -> TileCoord {
    TileCoord {
        tile_x: (instance.tile.document_x as u32) / TILE_SIZE,
        tile_y: (instance.tile.document_y as u32) / TILE_SIZE,
    }
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

fn leaf_should_rebuild(
    dirty_tiles: Option<&DirtyTileMask>,
    cached_leaf: Option<&CachedLeafDraw>,
    blend: BlendMode,
    image_handle: ImageHandle,
) -> bool {
    if dirty_tiles.is_some() {
        return true;
    }
    let Some(cached_leaf) = cached_leaf else {
        return true;
    };
    cached_leaf.blend != blend
        || cached_leaf.image_handle != image_handle
        || cached_leaf.draw_instances.is_empty()
}

fn create_composite_pipeline(
    device: &wgpu::Device,
    pipeline_layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    surface_format: wgpu::TextureFormat,
    blend_state: wgpu::BlendState,
    label: &str,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(pipeline_layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: surface_format,
                blend: Some(blend_state),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

fn multiply_blend_state() -> wgpu::BlendState {
    wgpu::BlendState {
        color: wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::Dst,
            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
            operation: wgpu::BlendOperation::Add,
        },
        alpha: wgpu::BlendComponent::OVER,
    }
}

mod dirty;

mod planning;

mod render_tree;

mod geometry;

mod renderer_cache_draw;

mod renderer_init;

mod renderer_view_ops;

#[cfg(test)]
mod tests;
