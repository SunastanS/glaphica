use atlas::Backend;
use brush::{BrushInput, BrushStrokeState};
use gla_doc_renderer::GlaDocRenderer;
use gla_document::{GlaDoc, GlaDocError};
use renderer::{MergeTileCommand, RenderCommand, TileRenderer};

use crate::AppBrushRegistry;
use crate::editor::session::EditorSessionError;

pub struct StrokeTransaction {
    stroke: BrushStrokeState,
    merge_payload: Option<Vec<u8>>,
}

impl StrokeTransaction {
    pub fn new(stroke: BrushStrokeState) -> Self {
        Self {
            stroke,
            merge_payload: None,
        }
    }

    pub fn stroke(&self) -> &BrushStrokeState {
        &self.stroke
    }

    pub fn process_inputs_gpu(
        &mut self,
        doc: &GlaDoc,
        doc_renderer: &mut GlaDocRenderer,
        brushes: &AppBrushRegistry,
        tile_renderer: &mut TileRenderer,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        inputs: &[BrushInput],
    ) -> Result<Option<Vec<usize>>, EditorSessionError> {
        if inputs.is_empty() {
            return Ok(None);
        }

        let [image_backend, _] = doc.image_undo().backends();

        let active_image = doc.active_layer_image()?;
        let mut dirty_tile_indices = Vec::new();
        let mut commands = Vec::new();

        for input in inputs {
            let max_affected_radius = brushes
                .max_affected_radius_px(input.brush_id)
                .ok_or(EditorSessionError::BrushNotRegistered(input.brush_id))?;
            for (block_index, _) in input.blocks.blocks().iter().enumerate() {
                let center = brushes.block_center(input, block_index)?;
                let mut affected_tiles = Vec::new();
                active_image.layout().collect_affected_tile_indices(
                    center,
                    max_affected_radius,
                    &mut affected_tiles,
                );
                for tile_index in affected_tiles {
                    let tile_origin = active_image.tile_canvas_origin(tile_index).ok_or(
                        EditorSessionError::Document(GlaDocError::InvalidSlotIndex {
                            slot_index: tile_index,
                            slot_count: active_image.slot_count(),
                        }),
                    )?;
                    let source_tile_key = self.stroke.brush_tiles().tile_key(tile_index);
                    let apply_payload =
                        brushes.encode_apply_dab_payload(input, block_index, tile_origin)?;
                    self.stroke.push_apply_dab(
                        tile_index,
                        source_tile_key,
                        apply_payload,
                        &mut commands,
                    )?;
                    dirty_tile_indices.push(tile_index);
                }
            }
        }

        if dirty_tile_indices.is_empty() {
            return Ok(None);
        }

        dirty_tile_indices.sort_unstable();
        dirty_tile_indices.dedup();

        let merge_payload = brushes.merge_payload(self.stroke.brush_id()).ok_or(
            EditorSessionError::BrushNotRegistered(self.stroke.brush_id()),
        )?;
        self.merge_payload = Some(merge_payload.clone());
        for &tile_index in &dirty_tile_indices {
            let (origin_tile_key, preview_tile_key) =
                doc_renderer.ensure_brush_preview_merge_target(doc, tile_index)?;
            self.stroke.push_preview_merge(
                tile_index,
                origin_tile_key,
                preview_tile_key,
                merge_payload.clone(),
                &mut commands,
            );
        }

        self.execute_preview_commands_gpu(
            doc_renderer,
            brushes,
            image_backend,
            tile_renderer,
            device,
            queue,
            &commands,
        )?;

        Ok(Some(dirty_tile_indices))
    }

    pub fn commit_gpu(
        mut self,
        doc: &mut GlaDoc,
        doc_renderer: &mut GlaDocRenderer,
        brushes: &mut AppBrushRegistry,
        tile_renderer: &mut TileRenderer,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> Result<Option<Vec<usize>>, EditorSessionError> {
        let merge_payload = self
            .merge_payload
            .take()
            .ok_or(EditorSessionError::MissingActiveMergePayload)?;

        let brush_id = self.stroke.brush_id();
        let tile_indices = self
            .stroke
            .touched_tiles()
            .iter()
            .map(|record| record.tile_index)
            .collect::<Vec<_>>();
        if tile_indices.is_empty() {
            doc_renderer.clear_brush_preview_image();
            return Ok(None);
        }

        let brush_backend = brushes
            .brush_backend(brush_id)
            .ok_or(EditorSessionError::BrushNotRegistered(brush_id))?;
        let brush_tiles_format = brush_backend.brush_tile_format();
        let brush_backend = brush_backend.brush_backend().clone();

        let active_layer_id = doc.active_layer_id();
        let image_undo = doc.image_undo().clone();
        let plan = self.stroke.build_commit_plan(merge_payload);

        let (backup_group, undo_tile_records, commands) = {
            let image = doc.active_layer_image_mut()?;
            let backup = image_undo
                .backup_tiles(image, &tile_indices)?;
            let (backup_group, undo_tile_records, copy_commands) = backup.into_parts();
            let mut commands: Vec<RenderCommand> = copy_commands
                .into_iter()
                .map(RenderCommand::CopyTile)
                .collect();

            for (entry, record) in plan.entries.iter().zip(self.stroke.touched_tiles()) {
                let destination_tile_key = image
                    .ensure_active_tile_key(entry.tile_index)?;
                let origin_tile_key = undo_tile_records
                    .iter()
                    .find(|r| r.tile_index() == entry.tile_index)
                    .map(|r| r.backup_tile_key())
                    .ok_or(atlas::AtlasError::InvalidState)
                    .map_err(EditorSessionError::Atlas)?;
                commands.push(RenderCommand::MergeTile(MergeTileCommand {
                    brush_id: plan.brush_id,
                    origin_tile_key,
                    brush_tile_key: record.brush_tile_key,
                    destination_tile_key,
                    brush_payload: plan.brush_payload.clone(),
                }));
            }
            (backup_group, undo_tile_records, commands)
        };

        let [image_backend, backup_backend] = image_undo.backends();
        let render_backend = doc_renderer.render_backend();
        tile_renderer.ensure_backend(device, image_backend)?;
        tile_renderer.ensure_backend_with_format(device, &brush_backend, brush_tiles_format)?;
        tile_renderer.ensure_backend(device, backup_backend)?;
        tile_renderer.ensure_backend(device, render_backend)?;

        let mut clear_batches = image_backend.take_pending_clear_batches()?;
        clear_batches.extend(brush_backend.take_pending_clear_batches()?);
        clear_batches.extend(render_backend.take_pending_clear_batches()?);
        clear_batches.extend(backup_backend.take_pending_clear_batches()?);
        tile_renderer.execute_commands_with_shader_provider(
            device,
            queue,
            &[
                image_backend,
                &brush_backend,
                render_backend,
                backup_backend,
            ],
            &clear_batches,
            &commands,
            None,
            brushes,
        )?;

        doc.push_undo_entry(active_layer_id, backup_group, undo_tile_records)?;

        let brush_backend = brushes
            .brush_backend_mut(brush_id)
            .ok_or(EditorSessionError::BrushNotRegistered(brush_id))?;
        let _cached_group = brush_backend.archive_stroke(self.stroke)?;
        doc_renderer.clear_brush_preview_image();

        Ok(Some(tile_indices))
    }

    pub fn cancel(self, doc_renderer: &mut GlaDocRenderer) {
        doc_renderer.clear_brush_preview_image();
    }

    fn execute_preview_commands_gpu(
        &self,
        doc_renderer: &GlaDocRenderer,
        brushes: &AppBrushRegistry,
        image_backend: &Backend,
        tile_renderer: &mut TileRenderer,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        commands: &[RenderCommand],
    ) -> Result<(), EditorSessionError> {
        let brush_backend = brushes.brush_backend(self.stroke.brush_id()).ok_or(
            EditorSessionError::BrushNotRegistered(self.stroke.brush_id()),
        )?;
        let brush_tile_fmt = brush_backend.brush_tile_format();
        let brush_backend = brush_backend.brush_backend();
        let render_backend = doc_renderer.render_backend();

        tile_renderer.ensure_backend(device, image_backend)?;
        tile_renderer.ensure_backend_with_format(device, brush_backend, brush_tile_fmt)?;
        tile_renderer.ensure_backend(device, render_backend)?;

        let mut clear_batches = brush_backend.take_pending_clear_batches()?;
        clear_batches.extend(render_backend.take_pending_clear_batches()?);
        tile_renderer.execute_commands_with_shader_provider(
            device,
            queue,
            &[image_backend, brush_backend, render_backend],
            &clear_batches,
            commands,
            None,
            brushes,
        )?;

        Ok(())
    }
}
