//! Cache maintenance and low-level draw submission.
//!
//! This module manages group target cache contents, tile copy/update routines,
//! and submission of tile-instance runs during composite/view pass execution.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU32, Ordering};

use render_protocol::{
    BlendModePipelineStrategy, RenderNodeSnapshot, RenderTreeSnapshot, TransformMatrix4x4,
};
use tiles::{TILE_STRIDE, TileImage, TileKey};

use crate::{
    BlendMode, DrawPassContext, GroupTargetCacheEntry, Renderer, TileCompositeSpace, TileCoord,
    TileDrawInstance, ViewportMode, build_group_tile_draw_instances, tile_coord_from_draw_instance,
};

static ROOT_DRAW_LOG_COUNT: AtomicU32 = AtomicU32::new(0);

impl Renderer {
    fn collect_live_node_ids(
        node: &RenderNodeSnapshot,
        live_leaf_layers: &mut HashSet<u64>,
        live_group_ids: &mut HashSet<u64>,
    ) {
        match node {
            RenderNodeSnapshot::Leaf { layer_id, .. } => {
                live_leaf_layers.insert(*layer_id);
            }
            RenderNodeSnapshot::Group {
                group_id, children, ..
            } => {
                live_group_ids.insert(*group_id);
                for child in children.iter() {
                    Self::collect_live_node_ids(child, live_leaf_layers, live_group_ids);
                }
            }
        }
    }

    fn live_ids(snapshot: &RenderTreeSnapshot) -> (HashSet<u64>, HashSet<u64>) {
        let mut live_leaf_layers = HashSet::new();
        let mut live_group_ids = HashSet::new();
        Self::collect_live_node_ids(
            snapshot.root.as_ref(),
            &mut live_leaf_layers,
            &mut live_group_ids,
        );
        (live_leaf_layers, live_group_ids)
    }

    pub(super) fn draw_root_group_to_surface(
        &mut self,
        target_view: &wgpu::TextureView,
        encoder: &mut wgpu::CommandEncoder,
    ) {
        const ROOT_GROUP_ID: u64 = 0;
        let group_target = self
            .cache_state
            .group_target_cache
            .remove(&ROOT_GROUP_ID)
            .expect("root group cache must exist before view pass");
        if ROOT_DRAW_LOG_COUNT.fetch_add(1, Ordering::Relaxed) < 8 {
            eprintln!(
                "[renderer] draw_root_group_to_surface draw_instances={} group_cache_entries={}",
                group_target.draw_instances.len(),
                self.cache_state.group_target_cache.len() + 1
            );
        }
        let group_atlas_bind_group = if self.should_use_nearest_sampling_for_view() {
            self.gpu_state.group_atlas_bind_group_nearest.clone()
        } else {
            self.gpu_state.group_atlas_bind_group_linear.clone()
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
        self.cache_state
            .group_target_cache
            .insert(ROOT_GROUP_ID, group_target);
    }

    pub(super) fn should_use_nearest_sampling_for_view(&self) -> bool {
        let Some(viewport) = self.view_state.viewport else {
            return false;
        };

        let half_width = (viewport.width as f32) * 0.5;
        let half_height = (viewport.height as f32) * 0.5;
        let x_basis_scale = f32::hypot(
            self.view_state.view_matrix[0] * half_width,
            self.view_state.view_matrix[1] * half_height,
        );
        let y_basis_scale = f32::hypot(
            self.view_state.view_matrix[4] * half_width,
            self.view_state.view_matrix[5] * half_height,
        );
        let max_scale = x_basis_scale.max(y_basis_scale);
        max_scale > 1.0 + 1e-4
    }

    pub(super) fn retain_live_leaf_caches(&mut self, snapshot: &RenderTreeSnapshot) {
        let (live_leaf_layers, _) = Self::live_ids(snapshot);
        self.cache_state
            .leaf_draw_cache
            .retain(|layer_id, _| live_leaf_layers.contains(layer_id));
        self.frame_state
            .dirty_state_store
            .retain_layers(&live_leaf_layers);
    }

    pub(super) fn retain_live_group_targets(&mut self, snapshot: &RenderTreeSnapshot) {
        let (_, live_group_ids) = Self::live_ids(snapshot);
        let mut retained_cache = HashMap::new();
        let previous_cache = std::mem::take(&mut self.cache_state.group_target_cache);
        for (group_id, entry) in previous_cache {
            if live_group_ids.contains(&group_id) {
                retained_cache.insert(group_id, entry);
            } else {
                self.release_group_cache_entry(entry);
            }
        }
        self.cache_state.group_target_cache = retained_cache;
    }

    pub(super) fn clear_group_target_cache(&mut self) {
        let previous_cache = std::mem::take(&mut self.cache_state.group_target_cache);
        for (_, entry) in previous_cache {
            self.release_group_cache_entry(entry);
        }
    }

    pub(super) fn release_group_cache_entry(&self, entry: GroupTargetCacheEntry) {
        let mut tile_key_iterator = entry.image.iter_tiles().map(|(_, _, tile_key)| tile_key);
        if tile_key_iterator.next().is_none() {
            return;
        }
        let set = self
            .gpu_state
            .group_tile_store
            .adopt_tile_set(entry.image.iter_tiles().map(|(_, _, tile_key)| tile_key))
            .unwrap_or_else(|error| panic!("adopt group cache tile set for release: {error}"));
        self.gpu_state
            .group_tile_store
            .release_tile_set(set)
            .unwrap_or_else(|error| panic!("release group cache tile set: {error}"));
    }

    pub(super) fn group_cache_extent(&self) -> wgpu::Extent3d {
        let (document_width, document_height) =
            self.data_state.render_data_resolver.document_size();
        crate::group_cache_extent_from_document_size(document_width, document_height)
    }

    pub(super) fn group_cache_slot_extent(&self) -> wgpu::Extent3d {
        let (document_width, document_height) =
            self.data_state.render_data_resolver.document_size();
        crate::group_cache_slot_extent_from_document_size(document_width, document_height)
    }

    pub(super) fn group_cache_slot_matrix(&self) -> TransformMatrix4x4 {
        let slot_extent = self.group_cache_slot_extent();
        crate::document_clip_matrix_from_size(slot_extent.width, slot_extent.height)
    }

    pub(super) fn group_tile_grid(&self) -> (u32, u32) {
        let (document_width, document_height) =
            self.data_state.render_data_resolver.document_size();
        crate::group_tile_grid_from_document_size(document_width, document_height)
    }

    pub(super) fn create_group_target_scratch(&self) -> (wgpu::Texture, wgpu::TextureView) {
        let extent = self.group_cache_slot_extent();
        let texture = self
            .gpu_state
            .device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("renderer.group_target.scratch"),
                size: extent,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: self.gpu_state.surface_config.format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        (texture, view)
    }

    fn create_group_target_cache_entry(
        &self,
        cache_extent: wgpu::Extent3d,
    ) -> GroupTargetCacheEntry {
        let image = TileImage::new(cache_extent.width, cache_extent.height)
            .unwrap_or_else(|error| panic!("create group virtual image: {error:?}"));
        GroupTargetCacheEntry {
            image,
            draw_instances: Vec::new(),
            blend: BlendMode::Normal,
        }
    }

    fn resize_group_cache_entry_if_needed(
        &self,
        entry: GroupTargetCacheEntry,
        cache_extent: wgpu::Extent3d,
    ) -> (GroupTargetCacheEntry, bool) {
        if entry.image.size_x() == cache_extent.width && entry.image.size_y() == cache_extent.height
        {
            return (entry, false);
        }
        self.release_group_cache_entry(entry);
        (
            GroupTargetCacheEntry {
                image: TileImage::new(cache_extent.width, cache_extent.height)
                    .unwrap_or_else(|error| panic!("resize group virtual image: {error:?}")),
                draw_instances: Vec::new(),
                blend: BlendMode::Normal,
            },
            true,
        )
    }

    fn copy_group_cache_dirty_tiles(
        &self,
        entry: &mut GroupTargetCacheEntry,
        source_texture: &wgpu::Texture,
        encoder: &mut wgpu::CommandEncoder,
        dirty_tiles: &HashSet<TileCoord>,
    ) {
        for tile_coord in dirty_tiles {
            if tile_coord.tile_x >= entry.image.tiles_per_row()
                || tile_coord.tile_y >= entry.image.tiles_per_column()
            {
                continue;
            }
            let tile_key = self.group_cache_tile_key_for_coord(entry, *tile_coord);
            self.copy_group_tile_from_texture(
                source_texture,
                encoder,
                tile_coord.tile_x,
                tile_coord.tile_y,
                tile_key,
            );
        }
    }

    fn copy_group_cache_all_tiles(
        &self,
        entry: &mut GroupTargetCacheEntry,
        source_texture: &wgpu::Texture,
        encoder: &mut wgpu::CommandEncoder,
    ) {
        for tile_y in 0..entry.image.tiles_per_column() {
            for tile_x in 0..entry.image.tiles_per_row() {
                let tile_coord = TileCoord { tile_x, tile_y };
                let tile_key = self.group_cache_tile_key_for_coord(entry, tile_coord);
                self.copy_group_tile_from_texture(
                    source_texture,
                    encoder,
                    tile_x,
                    tile_y,
                    tile_key,
                );
            }
        }
    }

    pub(super) fn update_group_cache_from_texture(
        &mut self,
        group_id: u64,
        source_texture: &wgpu::Texture,
        encoder: &mut wgpu::CommandEncoder,
        dirty_tiles: Option<&HashSet<TileCoord>>,
    ) {
        let cache_extent = self.group_cache_extent();
        let entry = self
            .cache_state
            .group_target_cache
            .remove(&group_id)
            .unwrap_or_else(|| self.create_group_target_cache_entry(cache_extent));

        let (mut entry, mut rebuild_draw_instances) =
            self.resize_group_cache_entry_if_needed(entry, cache_extent);

        let dirty_tiles = if entry.draw_instances.is_empty() {
            None
        } else {
            dirty_tiles
        };
        let copied_tile_count = match dirty_tiles {
            Some(dirty_tiles) => dirty_tiles.len(),
            None => usize::try_from(
                entry
                    .image
                    .tiles_per_row()
                    .checked_mul(entry.image.tiles_per_column())
                    .expect("group cache tile count overflow"),
            )
            .expect("group cache tile count exceeds usize"),
        };
        let cache_mode = if dirty_tiles.is_some() {
            "partial"
        } else {
            "full"
        };

        match dirty_tiles {
            Some(dirty_tiles) => {
                self.copy_group_cache_dirty_tiles(&mut entry, source_texture, encoder, dirty_tiles);
            }
            None => {
                self.copy_group_cache_all_tiles(&mut entry, source_texture, encoder);
                rebuild_draw_instances = true;
            }
        }

        if rebuild_draw_instances || entry.draw_instances.is_empty() {
            entry.draw_instances = build_group_tile_draw_instances(
                &entry.image,
                entry.blend,
                &self.gpu_state.group_tile_store,
            );
        }
        if crate::renderer_perf_log_enabled() {
            eprintln!(
                "[renderer_perf] group_cache_update group_id={} mode={} copied_tiles={} draw_instances={} rebuild_draw_instances={}",
                group_id,
                cache_mode,
                copied_tile_count,
                entry.draw_instances.len(),
                rebuild_draw_instances,
            );
        }
        if crate::renderer_perf_jsonl_enabled() {
            crate::renderer_perf_jsonl_write(&format!(
                "{{\"event\":\"group_cache_update\",\"group_id\":{},\"mode\":\"{}\",\"copied_tiles\":{},\"draw_instances\":{},\"rebuild_draw_instances\":{}}}",
                group_id,
                cache_mode,
                copied_tile_count,
                entry.draw_instances.len(),
                rebuild_draw_instances,
            ));
        }
        self.cache_state.group_target_cache.insert(group_id, entry);
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
            existing_key
        } else {
            let allocated_key = self
                .gpu_state
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
            .gpu_state
            .group_tile_store
            .resolve(tile_key)
            .expect("group tile key must resolve to atlas address");
        let source_x = tile_x
            .checked_mul(TILE_STRIDE)
            .expect("source group tile x overflow");
        let source_y = tile_y
            .checked_mul(TILE_STRIDE)
            .expect("source group tile y overflow");
        let group_atlas_layout = self.gpu_state.group_tile_atlas.layout();
        let (destination_slot_x, destination_slot_y) =
            tile_address.atlas_slot_origin_pixels_in(group_atlas_layout);
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
                texture: self.gpu_state.group_tile_atlas.texture(),
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
        let effective_instances = self.resolve_effective_draw_instances(
            draw_instances,
            context.visible_tiles,
            &mut filtered_instances,
        );

        if effective_instances.is_empty() {
            return;
        }

        #[cfg(debug_assertions)]
        self.assert_unique_effective_tile_coords(effective_instances);

        self.upload_effective_instances(effective_instances);

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
                .view_state
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

        pass.set_bind_group(0, &self.gpu_state.per_frame_bind_group, &[]);
        pass.set_bind_group(1, context.atlas_bind_group, &[]);

        self.draw_instance_runs(
            &mut pass,
            effective_instances,
            context.composite_space,
            support_matrix,
        );
    }

    #[cfg(debug_assertions)]
    fn assert_unique_effective_tile_coords(&self, effective_instances: &[TileDrawInstance]) {
        let mut seen = HashSet::with_capacity(effective_instances.len());
        for (index, instance) in effective_instances.iter().enumerate() {
            let tile_coord = tile_coord_from_draw_instance(instance);
            if !seen.insert(tile_coord) {
                panic!(
                    "effective draw instances contain duplicate tile coord {:?} at index {}",
                    tile_coord, index
                );
            }
        }
    }

    fn resolve_effective_draw_instances<'a>(
        &self,
        draw_instances: &'a [TileDrawInstance],
        visible_tiles: Option<&HashSet<TileCoord>>,
        filtered_instances: &'a mut Vec<TileDrawInstance>,
    ) -> &'a [TileDrawInstance] {
        if let Some(visible_tiles) = visible_tiles {
            filtered_instances.extend(draw_instances.iter().copied().filter(|instance| {
                visible_tiles.contains(&tile_coord_from_draw_instance(instance))
            }));
            filtered_instances
        } else {
            draw_instances
        }
    }

    fn upload_effective_instances(&mut self, effective_instances: &[TileDrawInstance]) {
        self.gpu_state.tile_instance_gpu_staging.clear();
        self.gpu_state.tile_instance_gpu_staging.extend(
            effective_instances
                .iter()
                .map(|draw_instance| draw_instance.tile),
        );
        self.ensure_tile_instance_capacity(self.gpu_state.tile_instance_gpu_staging.len());
        let instance_bytes: &[u8] = bytemuck::cast_slice(&self.gpu_state.tile_instance_gpu_staging);
        self.gpu_state
            .queue
            .write_buffer(&self.gpu_state.tile_instance_buffer, 0, instance_bytes);
    }

    fn draw_instance_runs(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        effective_instances: &[TileDrawInstance],
        composite_space: TileCompositeSpace,
        support_matrix: render_protocol::RenderStepSupportMatrix,
    ) {
        let mut run_start = 0usize;
        while run_start < effective_instances.len() {
            let blend_mode = effective_instances[run_start].blend_mode;
            let mut run_end = run_start + 1;
            while run_end < effective_instances.len()
                && effective_instances[run_end].blend_mode == blend_mode
            {
                run_end += 1;
            }

            let pipeline = match (support_matrix.blend_strategy(blend_mode), composite_space) {
                (BlendModePipelineStrategy::SurfaceAlphaBlend, TileCompositeSpace::Content) => {
                    &self.gpu_state.alpha_composite_pipeline
                }
                (BlendModePipelineStrategy::SurfaceMultiplyBlend, TileCompositeSpace::Content) => {
                    &self.gpu_state.multiply_composite_pipeline
                }
                (BlendModePipelineStrategy::SurfaceAlphaBlend, TileCompositeSpace::Slot) => {
                    &self.gpu_state.alpha_composite_slot_pipeline
                }
                (BlendModePipelineStrategy::SurfaceMultiplyBlend, TileCompositeSpace::Slot) => {
                    &self.gpu_state.multiply_composite_slot_pipeline
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
