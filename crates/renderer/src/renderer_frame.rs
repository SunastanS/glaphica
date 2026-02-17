//! Frame-level orchestration.
//!
//! This module builds `FramePlan`, executes composite/view passes, and commits
//! frame results after synchronization checks.

use std::collections::HashMap;

use crate::{
    CompositeEmission, CompositePassContext, DirtyExecutionPlan, DirtyPropagationEngine,
    DirtyRectMask, DirtyTileMask, FrameExecutionResult, FramePlan, FrameState, PresentError,
    RenderNodeKey, RenderTreeNode, Renderer, ViewportMode, build_render_tree_from_snapshot,
};

fn refresh_cached_render_tree_if_dirty(frame_state: &mut FrameState) {
    if !frame_state.render_tree_dirty {
        return;
    }
    frame_state.cached_render_tree = frame_state
        .bound_steps
        .as_ref()
        .map(build_render_tree_from_snapshot);
    frame_state.render_tree_dirty = false;
}

fn collect_full_leaf_dirty_tiles(
    layer_dirty_rect_masks: &HashMap<u64, DirtyRectMask>,
) -> HashMap<u64, DirtyTileMask> {
    let mut dirty_leaf_tiles = HashMap::new();
    for (layer_id, dirty_rect_mask) in layer_dirty_rect_masks {
        if matches!(dirty_rect_mask, DirtyRectMask::Full) {
            dirty_leaf_tiles.insert(*layer_id, DirtyTileMask::Full);
        }
    }
    dirty_leaf_tiles
}

fn split_node_dirty_tiles(
    node_dirty_tiles: HashMap<RenderNodeKey, DirtyTileMask>,
    dirty_leaf_tiles: &mut HashMap<u64, DirtyTileMask>,
    dirty_group_tiles: &mut HashMap<u64, DirtyTileMask>,
) {
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

fn group_tile_count(tiles_per_row: u32, tiles_per_column: u32) -> usize {
    usize::try_from(
        tiles_per_row
            .checked_mul(tiles_per_column)
            .expect("group tile count overflow"),
    )
    .expect("group tile count exceeds usize")
}

fn build_dirty_execution_plan(
    render_tree: Option<&RenderTreeNode>,
    layer_dirty_rect_masks: &HashMap<u64, DirtyRectMask>,
    force_group_rerender: bool,
    group_tile_count: usize,
) -> DirtyExecutionPlan {
    let mut dirty_leaf_tiles = collect_full_leaf_dirty_tiles(layer_dirty_rect_masks);
    let mut dirty_group_tiles = HashMap::new();
    let Some(render_tree) = render_tree else {
        return DirtyExecutionPlan {
            force_group_rerender,
            dirty_leaf_tiles,
            dirty_group_tiles,
        };
    };
    if !force_group_rerender {
        let propagation_engine = DirtyPropagationEngine::new(group_tile_count);
        let node_dirty_tiles =
            propagation_engine.collect_node_tile_masks(render_tree, layer_dirty_rect_masks);
        split_node_dirty_tiles(
            node_dirty_tiles,
            &mut dirty_leaf_tiles,
            &mut dirty_group_tiles,
        );
    }
    DirtyExecutionPlan {
        force_group_rerender,
        dirty_leaf_tiles,
        dirty_group_tiles,
    }
}

impl Renderer {
    pub fn present_frame(&mut self, frame_id: u64) -> Result<(), PresentError> {
        self.gpu_state
            .tile_atlas
            .drain_and_execute(&self.gpu_state.queue)
            .map_err(PresentError::TileDrain)?;

        let frame_plan = self.build_frame_plan(frame_id);

        let frame = self
            .gpu_state
            .surface
            .get_current_texture()
            .map_err(PresentError::Surface)?;
        let frame_view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        {
            let mut clear_encoder =
                self.gpu_state
                    .device
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
            self.gpu_state.queue.submit(Some(clear_encoder.finish()));
        }

        let frame_result = self.execute_frame_plan(frame_plan, &frame_view);

        frame.present();
        self.commit_frame_result(frame_result);
        Ok(())
    }

    fn snapshot_revision(&self) -> u64 {
        self.frame_state
            .bound_steps
            .as_ref()
            .map_or(0, |snapshot| snapshot.revision)
    }

    fn build_frame_plan(&mut self, frame_id: u64) -> FramePlan {
        refresh_cached_render_tree_if_dirty(&mut self.frame_state);

        let render_tree = self.frame_state.cached_render_tree.take();
        let layer_dirty_rect_masks = self
            .frame_state
            .dirty_state_store
            .resolve_layer_dirty_rect_masks(self.data_state.render_data_resolver.as_ref());
        let force_group_rerender = self
            .frame_state
            .dirty_state_store
            .is_document_composite_dirty();
        let (tiles_per_row, tiles_per_column) = self.group_tile_grid();
        let dirty_plan = build_dirty_execution_plan(
            render_tree.as_ref(),
            &layer_dirty_rect_masks,
            force_group_rerender,
            group_tile_count(tiles_per_row, tiles_per_column),
        );
        let composite_plan = render_tree
            .as_ref()
            .map(|render_tree| self.build_composite_node_plan(render_tree, &dirty_plan, None));

        FramePlan {
            version: self
                .frame_state
                .frame_sync
                .version(frame_id, self.snapshot_revision()),
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
            self.gpu_state.queue.write_buffer(
                &self.gpu_state.view_uniform_buffer,
                0,
                bytemuck::bytes_of(&frame_plan.composite_matrix),
            );

            let mut composite_encoder =
                self.gpu_state
                    .device
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
            self.gpu_state
                .queue
                .submit(Some(composite_encoder.finish()));

            self.gpu_state.queue.write_buffer(
                &self.gpu_state.view_uniform_buffer,
                0,
                bytemuck::bytes_of(&self.view_state.view_matrix),
            );
            self.view_state.view_matrix_dirty = false;

            let mut view_encoder =
                self.gpu_state
                    .device
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                        label: Some("renderer.frame.view"),
                    });
            self.draw_root_group_to_surface(frame_view, &mut view_encoder);
            self.gpu_state.queue.submit(Some(view_encoder.finish()));
        }

        FrameExecutionResult {
            version: frame_plan.version,
            render_tree: frame_plan.render_tree,
        }
    }

    fn commit_frame_result(&mut self, frame_result: FrameExecutionResult) {
        assert!(
            self.frame_state
                .frame_sync
                .can_commit(frame_result.version, self.snapshot_revision()),
            "frame result must match current renderer epoch and snapshot"
        );
        self.frame_state.cached_render_tree = frame_result.render_tree;
        self.frame_state.dirty_state_store.clear_layer_dirty_masks();
        self.frame_state
            .dirty_state_store
            .clear_document_composite_dirty();
        self.frame_state
            .frame_sync
            .commit(frame_result.version, self.snapshot_revision());
    }
}
