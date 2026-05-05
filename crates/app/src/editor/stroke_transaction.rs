use atlas::{Backend, TileCredential};
use brush::{BrushInput, BrushRegistry, BrushStrokeState, build_merge_command};
use gla_doc_renderer::GlaDocRenderer;
use gla_document::{GlaDoc, GlaDocError, GlaImageUndoTileRecord};
use renderer::{ApplyDabCommand, BrushTileFormat, MergeTileCommand, RenderCommand, TileRenderer};

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
        brushes: &BrushRegistry,
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
                    let destination_tile_key = self.stroke.push_apply_dab(tile_index)?;
                    commands.push(RenderCommand::ApplyDab(ApplyDabCommand {
                        brush_id: self.stroke.brush_id(),
                        destination_tile_key,
                        source_tile_key,
                        brush_payload: apply_payload,
                    }));
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
            if let Some(brush_tile_key) = self.stroke.preview_brush_tile_key(tile_index) {
                commands.push(RenderCommand::MergeTile(MergeTileCommand {
                    brush_id: self.stroke.brush_id(),
                    origin_tile_key,
                    brush_tile_key,
                    destination_tile_key: preview_tile_key,
                    brush_payload: merge_payload.clone(),
                }));
            }
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
        brushes: &mut BrushRegistry,
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
            .backend(brush_id)
            .ok_or(EditorSessionError::BrushNotRegistered(brush_id))?;
        let brush_tiles_format = brush_backend.brush_tile_format();
        let brush_backend = brush_backend.brush_backend().clone();

        let active_layer_id = doc.active_layer_id();
        let image_undo = doc.image_undo().clone();
        let plan = self.stroke.build_commit_plan(merge_payload);

        let source_credentials: Vec<(usize, TileCredential)> = {
            let image = doc.active_layer_image()?;
            plan.entries
                .iter()
                .map(|e| Ok((e.tile_index, image.tile_credential(e.tile_index)?)))
                .collect::<Result<Vec<_>, gla_image::GlaImageTileAccessError>>()?
        };
        let backup_result = image_undo.execute_backup(&source_credentials)?;

        let mut commands: Vec<RenderCommand> = backup_result.commands;
        let image = doc.active_layer_image_mut()?;
        let destination_tile_keys: Vec<_> = plan
            .entries
            .iter()
            .map(|e| image.ensure_active_tile_key(e.tile_index))
            .collect::<Result<_, _>>()?;

        let touched_tiles = self.stroke.touched_tiles();
        for i in 0..plan.entries.len() {
            let record = &touched_tiles[i];
            let (_, origin_tile_key) = backup_result.origin_keys[i];
            commands.push(RenderCommand::MergeTile(build_merge_command(
                plan.brush_id,
                &plan.brush_payload,
                record.brush_tile_key,
                origin_tile_key,
                destination_tile_keys[i],
            )));
        }

        let [image_backend, backup_backend] = image_undo.backends();
        let render_backend = doc_renderer.render_backend();
        execute_stroke_commands(
            tile_renderer,
            device,
            queue,
            image_backend,
            &brush_backend,
            brush_tiles_format,
            render_backend,
            Some(backup_backend),
            &commands,
            brushes,
        )?;

        let tile_records: Vec<GlaImageUndoTileRecord> = backup_result
            .origin_keys
            .into_iter()
            .map(|(tile_index, backup_tile_key)| {
                GlaImageUndoTileRecord::new(tile_index, backup_tile_key)
            })
            .collect();
        doc.push_undo_entry(active_layer_id, backup_result.backup_group, tile_records)?;

        let brush_backend = brushes
            .backend_mut(brush_id)
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
        brushes: &BrushRegistry,
        image_backend: &Backend,
        tile_renderer: &mut TileRenderer,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        commands: &[RenderCommand],
    ) -> Result<(), EditorSessionError> {
        let brush_backend = brushes.backend(self.stroke.brush_id()).ok_or(
            EditorSessionError::BrushNotRegistered(self.stroke.brush_id()),
        )?;
        let brush_tile_fmt = brush_backend.brush_tile_format();
        let render_backend = doc_renderer.render_backend();
        execute_stroke_commands(
            tile_renderer,
            device,
            queue,
            image_backend,
            brush_backend.brush_backend(),
            brush_tile_fmt,
            render_backend,
            None,
            commands,
            brushes,
        )
    }
}

fn execute_stroke_commands(
    tile_renderer: &mut TileRenderer,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    image_backend: &Backend,
    brush_backend: &Backend,
    brush_tile_format: BrushTileFormat,
    render_backend: &Backend,
    backup_backend: Option<&Backend>,
    commands: &[RenderCommand],
    brushes: &BrushRegistry,
) -> Result<(), EditorSessionError> {
    tile_renderer.ensure_backend(device, image_backend)?;
    tile_renderer.ensure_backend_with_format(device, brush_backend, brush_tile_format)?;
    tile_renderer.ensure_backend(device, render_backend)?;
    if let Some(backup) = backup_backend {
        tile_renderer.ensure_backend(device, backup)?;
    }

    let mut clear_batches = image_backend.take_pending_clear_batches()?;
    clear_batches.extend(brush_backend.take_pending_clear_batches()?);
    clear_batches.extend(render_backend.take_pending_clear_batches()?);
    if let Some(backup) = backup_backend {
        clear_batches.extend(backup.take_pending_clear_batches()?);
    }

    let backends = if let Some(backup) = backup_backend {
        vec![image_backend, brush_backend, render_backend, backup]
    } else {
        vec![image_backend, brush_backend, render_backend]
    };

    tile_renderer.execute_commands_with_shader_provider(
        device,
        queue,
        &backends,
        &clear_batches,
        commands,
        None,
        brushes,
    )?;

    Ok(())
}
