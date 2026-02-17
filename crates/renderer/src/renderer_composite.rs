//! Composite-tree planning and execution.
//!
//! This module translates render-tree nodes into `CompositeNodePlan` and executes
//! leaf/group rendering paths against cache targets and final outputs.

use std::collections::{HashMap, HashSet};

use crate::RenderTreeNode;
use crate::planning::GroupRenderDecision;
use crate::{
    CachedLeafDraw, CompositeEmission, CompositeNodePlan, CompositePassContext, DirtyExecutionPlan,
    DirtyTileMask, DrawPassContext, GroupDecisionEngine, GroupRerenderMode, Renderer,
    TileCompositeSpace, TileCoord, ViewportMode, build_group_tile_draw_instances,
    build_leaf_tile_draw_instances, build_leaf_tile_draw_instances_for_tiles, leaf_should_rebuild,
};

impl Renderer {
    fn emit_group_target_if_needed(
        &mut self,
        group_draw_instances: &[crate::TileDrawInstance],
        emit_tiles: &Option<HashSet<TileCoord>>,
        context: &CompositePassContext<'_>,
        encoder: &mut wgpu::CommandEncoder,
    ) {
        if !matches!(context.emission, CompositeEmission::EmitToTarget) {
            return;
        }
        let group_atlas_bind_group = self.gpu_state.group_atlas_bind_group_linear.clone();
        let draw_context = DrawPassContext {
            target_view: context.target_view,
            atlas_bind_group: &group_atlas_bind_group,
            visible_tiles: emit_tiles.as_ref(),
            viewport_mode: context.viewport_mode,
            composite_space: TileCompositeSpace::Slot,
        };
        self.draw_tile_instances_to_target_with_bind_group(
            group_draw_instances,
            encoder,
            &draw_context,
        );
    }

    fn render_leaf_node_plan(
        &mut self,
        layer_id: u64,
        blend: crate::BlendMode,
        image_handle: crate::ImageHandle,
        should_rebuild: bool,
        dirty_tiles: Option<&DirtyTileMask>,
        visible_tiles: &Option<HashSet<TileCoord>>,
        context: &CompositePassContext<'_>,
        encoder: &mut wgpu::CommandEncoder,
    ) {
        let mut cached_leaf = self
            .cache_state
            .leaf_draw_cache
            .remove(&layer_id)
            .unwrap_or_else(|| CachedLeafDraw {
                blend,
                image_handle,
                draw_instances: Vec::new(),
                tile_instance_index: HashMap::new(),
            });

        if should_rebuild {
            let partial_tiles = match dirty_tiles {
                Some(DirtyTileMask::Partial(tiles))
                    if cached_leaf.blend == blend
                        && cached_leaf.image_handle == image_handle
                        && !cached_leaf.draw_instances.is_empty() =>
                {
                    Some(tiles)
                }
                _ => None,
            };

            if let Some(partial_tiles) = partial_tiles {
                cached_leaf.replace_partial_tiles(partial_tiles);
                let partial_instances = build_leaf_tile_draw_instances_for_tiles(
                    blend,
                    image_handle,
                    self.data_state.render_data_resolver.as_ref(),
                    partial_tiles,
                );
                cached_leaf.append_instances(partial_instances);
            } else {
                let full_instances = build_leaf_tile_draw_instances(
                    blend,
                    image_handle,
                    self.data_state.render_data_resolver.as_ref(),
                );
                cached_leaf.replace_all_instances(blend, image_handle, full_instances);
            }
        }

        let atlas_bind_group = self.gpu_state.atlas_bind_group_linear.clone();
        let draw_context = DrawPassContext {
            target_view: context.target_view,
            atlas_bind_group: &atlas_bind_group,
            visible_tiles: visible_tiles.as_ref(),
            viewport_mode: context.viewport_mode,
            composite_space: TileCompositeSpace::Slot,
        };
        self.draw_tile_instances_to_target(&cached_leaf.draw_instances, encoder, &draw_context);
        self.cache_state
            .leaf_draw_cache
            .insert(layer_id, cached_leaf);
    }

    fn render_group_node_plan(
        &mut self,
        group_id: u64,
        blend: crate::BlendMode,
        decision: &GroupRenderDecision,
        emit_tiles: &Option<HashSet<TileCoord>>,
        children: &[CompositeNodePlan],
        context: &CompositePassContext<'_>,
        encoder: &mut wgpu::CommandEncoder,
    ) {
        if matches!(decision.mode, GroupRerenderMode::UseCache) {
            let mut group_target = self
                .cache_state
                .group_target_cache
                .remove(&group_id)
                .expect("group target cache must contain clean group");
            if group_target.blend != blend {
                group_target.blend = blend;
                group_target.draw_instances = build_group_tile_draw_instances(
                    &group_target.image,
                    blend,
                    &self.gpu_state.group_tile_store,
                );
            }
            self.emit_group_target_if_needed(
                &group_target.draw_instances,
                emit_tiles,
                context,
                encoder,
            );
            self.cache_state
                .group_target_cache
                .insert(group_id, group_target);
            return;
        }

        if decision
            .rerender_tiles
            .as_ref()
            .is_some_and(|tiles| tiles.is_empty())
        {
            let group_target = self
                .cache_state
                .group_target_cache
                .remove(&group_id)
                .expect("group cache must exist for empty rerender tile set");
            self.emit_group_target_if_needed(
                &group_target.draw_instances,
                emit_tiles,
                context,
                encoder,
            );
            self.cache_state
                .group_target_cache
                .insert(group_id, group_target);
            return;
        }

        let (group_target_texture, group_target_view) = self.create_group_target_scratch();
        {
            let _clear_group_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
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
            group_id,
            &group_target_texture,
            encoder,
            decision.rerender_tiles.as_ref(),
        );
        let mut group_target = self
            .cache_state
            .group_target_cache
            .remove(&group_id)
            .expect("group cache must contain rerendered group");
        if group_target.blend != blend {
            group_target.blend = blend;
            group_target.draw_instances = build_group_tile_draw_instances(
                &group_target.image,
                blend,
                &self.gpu_state.group_tile_store,
            );
        }
        self.emit_group_target_if_needed(
            &group_target.draw_instances,
            emit_tiles,
            context,
            encoder,
        );
        self.cache_state
            .group_target_cache
            .insert(group_id, group_target);
    }

    pub(super) fn build_composite_node_plan(
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
                    self.cache_state.leaf_draw_cache.get(layer_id),
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
                let cache_missing = !self.cache_state.group_target_cache.contains_key(group_id);
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

    pub(super) fn render_composite_node_plan(
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
                self.render_leaf_node_plan(
                    *layer_id,
                    *blend,
                    *image_handle,
                    *should_rebuild,
                    dirty_tiles.as_ref(),
                    visible_tiles,
                    context,
                    encoder,
                );
            }
            CompositeNodePlan::Group {
                group_id,
                blend,
                decision,
                emit_tiles,
                children,
            } => {
                self.render_group_node_plan(
                    *group_id, *blend, decision, emit_tiles, children, context, encoder,
                );
            }
        }
    }
}
