use std::collections::{HashMap, HashSet};

use render_protocol::{
    BlendModePipelineStrategy, RenderStepEntry, RenderStepSnapshot, TransformMatrix4x4,
};
use tiles::{TileKey, VirtualImage, TILE_STRIDE};

use crate::{
    build_group_tile_draw_instances, tile_coord_from_draw_instance, BlendMode, DrawPassContext,
    GroupTargetCacheEntry, Renderer, TileCompositeSpace, TileCoord, TileDrawInstance, ViewportMode,
};

impl Renderer {
    pub(super) fn draw_root_group_to_surface(
        &mut self,
        target_view: &wgpu::TextureView,
        encoder: &mut wgpu::CommandEncoder,
    ) {
        const ROOT_GROUP_ID: u64 = 0;
        let group_target = self
            .group_target_cache
            .remove(&ROOT_GROUP_ID)
            .expect("root group cache must exist before view pass");
        let group_atlas_bind_group = if self.should_use_nearest_sampling_for_view() {
            self.group_atlas_bind_group_nearest.clone()
        } else {
            self.group_atlas_bind_group_linear.clone()
        };
        let draw_context = DrawPassContext {
            target_view,
            atlas_bind_group: &group_atlas_bind_group,
            visible_tiles: None,
            viewport_mode: ViewportMode::Apply,
            composite_space: TileCompositeSpace::Content,
        };
        self.draw_tile_instances_to_target_with_bind_group(
            &group_target.draw_instances,
            encoder,
            &draw_context,
        );
        self.group_target_cache.insert(ROOT_GROUP_ID, group_target);
    }

    pub(super) fn should_use_nearest_sampling_for_view(&self) -> bool {
        let Some(viewport) = self.viewport else {
            return false;
        };

        let half_width = (viewport.width as f32) * 0.5;
        let half_height = (viewport.height as f32) * 0.5;
        let x_basis_scale = f32::hypot(
            self.view_matrix[0] * half_width,
            self.view_matrix[1] * half_height,
        );
        let y_basis_scale = f32::hypot(
            self.view_matrix[4] * half_width,
            self.view_matrix[5] * half_height,
        );
        let max_scale = x_basis_scale.max(y_basis_scale);
        max_scale > 1.0 + 1e-4
    }

    pub(super) fn retain_live_leaf_caches(&mut self, snapshot: &RenderStepSnapshot) {
        let live_leaf_layers: HashSet<u64> = snapshot
            .steps
            .iter()
            .filter_map(|step| match step {
                RenderStepEntry::Leaf { layer_id, .. } => Some(*layer_id),
                RenderStepEntry::Group { .. } => None,
            })
            .collect();
        self.leaf_draw_cache
            .retain(|layer_id, _| live_leaf_layers.contains(layer_id));
        self.dirty_state_store.retain_layers(&live_leaf_layers);
    }

    pub(super) fn retain_live_group_targets(&mut self, snapshot: &RenderStepSnapshot) {
        let live_group_ids: HashSet<u64> = snapshot
            .steps
            .iter()
            .filter_map(|step| match step {
                RenderStepEntry::Group { group_id, .. } => Some(*group_id),
                RenderStepEntry::Leaf { .. } => None,
            })
            .collect();
        let mut retained_cache = HashMap::new();
        let previous_cache = std::mem::take(&mut self.group_target_cache);
        for (group_id, entry) in previous_cache {
            if live_group_ids.contains(&group_id) {
                retained_cache.insert(group_id, entry);
            } else {
                self.release_group_cache_entry(entry);
            }
        }
        self.group_target_cache = retained_cache;
    }

    pub(super) fn clear_group_target_cache(&mut self) {
        let previous_cache = std::mem::take(&mut self.group_target_cache);
        for (_, entry) in previous_cache {
            self.release_group_cache_entry(entry);
        }
    }

    pub(super) fn release_group_cache_entry(&self, entry: GroupTargetCacheEntry) {
        for (_, _, tile_key) in entry.image.iter_tiles() {
            let released = self.group_tile_store.release(*tile_key);
            assert!(
                released,
                "group cache tile key must be allocated before release"
            );
        }
    }

    pub(super) fn group_cache_extent(&self) -> wgpu::Extent3d {
        let (document_width, document_height) = self.render_data_resolver.document_size();
        crate::group_cache_extent_from_document_size(document_width, document_height)
    }

    pub(super) fn group_cache_slot_extent(&self) -> wgpu::Extent3d {
        let (document_width, document_height) = self.render_data_resolver.document_size();
        crate::group_cache_slot_extent_from_document_size(document_width, document_height)
    }

    pub(super) fn group_cache_slot_matrix(&self) -> TransformMatrix4x4 {
        let slot_extent = self.group_cache_slot_extent();
        crate::document_clip_matrix_from_size(slot_extent.width, slot_extent.height)
    }

    pub(super) fn group_tile_grid(&self) -> (u32, u32) {
        let (document_width, document_height) = self.render_data_resolver.document_size();
        crate::group_tile_grid_from_document_size(document_width, document_height)
    }

    pub(super) fn create_group_target_scratch(&self) -> (wgpu::Texture, wgpu::TextureView) {
        let extent = self.group_cache_slot_extent();
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("renderer.group_target.scratch"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.surface_config.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        (texture, view)
    }

    pub(super) fn update_group_cache_from_texture(
        &mut self,
        group_id: u64,
        source_texture: &wgpu::Texture,
        encoder: &mut wgpu::CommandEncoder,
        dirty_tiles: Option<&HashSet<TileCoord>>,
    ) {
        let cache_extent = self.group_cache_extent();
        let mut rebuild_draw_instances = false;
        let mut entry = self
            .group_target_cache
            .remove(&group_id)
            .unwrap_or_else(|| {
                let image = VirtualImage::new(cache_extent.width, cache_extent.height)
                    .unwrap_or_else(|error| panic!("create group virtual image: {error:?}"));
                GroupTargetCacheEntry {
                    image,
                    draw_instances: Vec::new(),
                    blend: BlendMode::Normal,
                }
            });

        if entry.image.size_x() != cache_extent.width || entry.image.size_y() != cache_extent.height
        {
            self.release_group_cache_entry(entry);
            let image = VirtualImage::new(cache_extent.width, cache_extent.height)
                .unwrap_or_else(|error| panic!("resize group virtual image: {error:?}"));
            entry = GroupTargetCacheEntry {
                image,
                draw_instances: Vec::new(),
                blend: BlendMode::Normal,
            };
            rebuild_draw_instances = true;
        }

        let dirty_tiles = if entry.draw_instances.is_empty() {
            None
        } else {
            dirty_tiles
        };

        match dirty_tiles {
            Some(dirty_tiles) => {
                for tile_coord in dirty_tiles {
                    if tile_coord.tile_x >= entry.image.tiles_per_row()
                        || tile_coord.tile_y >= entry.image.tiles_per_column()
                    {
                        continue;
                    }
                    let tile_key = self.group_cache_tile_key_for_coord(&mut entry, *tile_coord);
                    self.copy_group_tile_from_texture(
                        source_texture,
                        encoder,
                        tile_coord.tile_x,
                        tile_coord.tile_y,
                        tile_key,
                    );
                }
            }
            None => {
                for tile_y in 0..entry.image.tiles_per_column() {
                    for tile_x in 0..entry.image.tiles_per_row() {
                        let tile_coord = TileCoord { tile_x, tile_y };
                        let tile_key = self.group_cache_tile_key_for_coord(&mut entry, tile_coord);
                        self.copy_group_tile_from_texture(
                            source_texture,
                            encoder,
                            tile_x,
                            tile_y,
                            tile_key,
                        );
                    }
                }
                rebuild_draw_instances = true;
            }
        }

        if rebuild_draw_instances || entry.draw_instances.is_empty() {
            entry.draw_instances =
                build_group_tile_draw_instances(&entry.image, entry.blend, &self.group_tile_store);
        }
        self.group_target_cache.insert(group_id, entry);
    }

    pub(super) fn group_cache_tile_key_for_coord(
        &self,
        entry: &mut GroupTargetCacheEntry,
        tile_coord: TileCoord,
    ) -> TileKey {
        if let Some(existing_key) = entry
            .image
            .get_tile(tile_coord.tile_x, tile_coord.tile_y)
            .unwrap_or_else(|error| panic!("get group tile key: {error:?}"))
        {
            *existing_key
        } else {
            let allocated_key = self
                .group_tile_store
                .allocate()
                .unwrap_or_else(|error| panic!("allocate group tile: {error}"));
            entry
                .image
                .set_tile(tile_coord.tile_x, tile_coord.tile_y, allocated_key)
                .unwrap_or_else(|error| panic!("set group tile key: {error:?}"));
            allocated_key
        }
    }

    pub(super) fn copy_group_tile_from_texture(
        &self,
        source_texture: &wgpu::Texture,
        encoder: &mut wgpu::CommandEncoder,
        tile_x: u32,
        tile_y: u32,
        tile_key: TileKey,
    ) {
        let tile_address = self
            .group_tile_store
            .resolve(tile_key)
            .expect("group tile key must resolve to atlas address");
        let source_x = tile_x
            .checked_mul(TILE_STRIDE)
            .expect("source group tile x overflow");
        let source_y = tile_y
            .checked_mul(TILE_STRIDE)
            .expect("source group tile y overflow");
        let (destination_slot_x, destination_slot_y) = tile_address.atlas_slot_origin_pixels();
        let destination_layer = tile_address.atlas_layer;
        encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: source_texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: source_x,
                    y: source_y,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture: self.group_tile_atlas.texture(),
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: destination_slot_x,
                    y: destination_slot_y,
                    z: destination_layer,
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width: TILE_STRIDE,
                height: TILE_STRIDE,
                depth_or_array_layers: 1,
            },
        );
    }

    pub(super) fn draw_tile_instances_to_target(
        &mut self,
        draw_instances: &[TileDrawInstance],
        encoder: &mut wgpu::CommandEncoder,
        context: &DrawPassContext<'_>,
    ) {
        self.draw_tile_instances_to_target_with_bind_group(draw_instances, encoder, context);
    }

    pub(super) fn draw_tile_instances_to_target_with_bind_group(
        &mut self,
        draw_instances: &[TileDrawInstance],
        encoder: &mut wgpu::CommandEncoder,
        context: &DrawPassContext<'_>,
    ) {
        if draw_instances.is_empty() {
            return;
        }

        let mut filtered_instances = Vec::new();
        let effective_instances: &[TileDrawInstance] =
            if let Some(visible_tiles) = context.visible_tiles {
                filtered_instances.extend(draw_instances.iter().copied().filter(|instance| {
                    visible_tiles.contains(&tile_coord_from_draw_instance(instance))
                }));
                &filtered_instances
            } else {
                draw_instances
            };

        if effective_instances.is_empty() {
            return;
        }

        self.tile_instance_gpu_staging.clear();
        self.tile_instance_gpu_staging.extend(
            effective_instances
                .iter()
                .map(|draw_instance| draw_instance.tile),
        );
        self.ensure_tile_instance_capacity(self.tile_instance_gpu_staging.len());
        let instance_bytes: &[u8] = bytemuck::cast_slice(&self.tile_instance_gpu_staging);
        self.queue
            .write_buffer(&self.tile_instance_buffer, 0, instance_bytes);

        let support_matrix =
            render_protocol::RenderStepSupportMatrix::current_executable_semantics();
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("renderer.layer_composite"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: context.target_view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });

        if matches!(context.viewport_mode, ViewportMode::Apply) {
            let viewport = self
                .viewport
                .as_ref()
                .expect("view pass requires viewport before drawing");
            pass.set_viewport(
                viewport.origin_x as f32,
                viewport.origin_y as f32,
                viewport.width as f32,
                viewport.height as f32,
                0.0,
                1.0,
            );
        }

        pass.set_bind_group(0, &self.per_frame_bind_group, &[]);
        pass.set_bind_group(1, context.atlas_bind_group, &[]);

        let mut run_start = 0usize;
        while run_start < effective_instances.len() {
            let blend_mode = effective_instances[run_start].blend_mode;
            let mut run_end = run_start + 1;
            while run_end < effective_instances.len()
                && effective_instances[run_end].blend_mode == blend_mode
            {
                run_end += 1;
            }

            let pipeline = match (
                support_matrix.blend_strategy(blend_mode),
                context.composite_space,
            ) {
                (BlendModePipelineStrategy::SurfaceAlphaBlend, TileCompositeSpace::Content) => {
                    &self.alpha_composite_pipeline
                }
                (BlendModePipelineStrategy::SurfaceMultiplyBlend, TileCompositeSpace::Content) => {
                    &self.multiply_composite_pipeline
                }
                (BlendModePipelineStrategy::SurfaceAlphaBlend, TileCompositeSpace::Slot) => {
                    &self.alpha_composite_slot_pipeline
                }
                (BlendModePipelineStrategy::SurfaceMultiplyBlend, TileCompositeSpace::Slot) => {
                    &self.multiply_composite_slot_pipeline
                }
                (BlendModePipelineStrategy::Unsupported, _) => {
                    panic!("unsupported blend mode in draw list: {blend_mode:?}")
                }
            };
            pass.set_pipeline(pipeline);

            let start_instance =
                u32::try_from(run_start).expect("tile instance range start exceeds u32");
            let end_instance = u32::try_from(run_end).expect("tile instance range end exceeds u32");
            pass.draw(0..6, start_instance..end_instance);
            run_start = run_end;
        }
    }
}
