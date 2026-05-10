use atlas::{Backend, TileCredential, TileManager};
use brush::{BrushInput, BrushRegistry, BrushStrokeState};
use glaphica_core::CanvasVec2;
use gla_doc_renderer::GlaDocRenderer;
use gla_document::{GlaDoc, GlaDocError};
use renderer::{ApplyDabCommand, BrushTileFormat, MergeTileCommand, RenderCommand, TileRenderer};

use crate::editor::session::EditorSessionError;



struct ImageBrushOp {
    input_index: usize,
    block_index: usize,
    center: CanvasVec2,
    max_affected_radius: u32,
}

struct PreviewBuildResult {
    commands: Vec<RenderCommand>,
    dirty_tile_indices: Vec<usize>,
    merge_payload: Vec<u8>,
}

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
        let image_tile_manager = TileManager::from(image_backend.clone());
        let brush_backend = brushes.backend(self.stroke.brush_id()).ok_or(
            EditorSessionError::BrushNotRegistered(self.stroke.brush_id()),
        )?;
        let brush_tile_manager = TileManager::from(brush_backend.brush_backend().clone());
        let render_tile_manager = TileManager::from(doc_renderer.render_backend().clone());

        let preview = self.build_preview_commands(
            doc,
            doc_renderer,
            brushes,
            &brush_tile_manager,
            &image_tile_manager,
            &render_tile_manager,
            inputs,
        )?;

        let Some(preview) = preview else {
            return Ok(None);
        };

        self.merge_payload = Some(preview.merge_payload);

        self.execute_preview_commands_gpu(
            doc_renderer,
            brushes,
            image_backend,
            tile_renderer,
            device,
            queue,
            &preview.commands,
        )?;

        Ok(Some(preview.dirty_tile_indices))
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
        let brush_tile_manager = TileManager::from(brush_backend.clone());

        let active_layer_id = doc.active_layer_id();
        let image_undo = doc.image_undo().clone();
        let plan = self.stroke.build_commit_plan(merge_payload);

        let source_credentials: Vec<(usize, TileCredential)> = {
            let image = doc.active_layer_image()?;
            plan.entries
                .iter()
                .map(|e| Ok((e.tile_index, image.source_tile_credential(e.tile_index)?)))
                .collect::<Result<Vec<_>, gla_image::GlaImageTileAccessError>>()?
        };
        let backup_result = image_undo.execute_backup(&source_credentials)?;

        let image = doc.active_layer_image_mut()?;

        let mut merge_commands = Vec::with_capacity(plan.entries.len());
        for (i, entry) in plan.entries.iter().enumerate() {
            let brush_tile_key =
                brush_tile_manager.resolve_destination_key(entry.brush_credential)?;
            let origin_tile_key = backup_result
                .merge_origin_tile_key(i)
                .ok_or(atlas::AtlasError::InvalidState)?;
            let destination_tile_key = image
                .tile_manager()
                .resolve_destination_key(image.source_tile_credential(entry.tile_index)?)?;
            merge_commands.push(RenderCommand::MergeTile(MergeTileCommand {
                brush_id: plan.brush_id,
                brush_tile_key,
                origin_tile_key,
                destination_tile_key,
                brush_payload: plan.brush_payload.clone(),
            }));
        }

        let mut commands: Vec<RenderCommand> = backup_result.commands;
        commands.extend(merge_commands);

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

        doc.push_undo_entry(
            active_layer_id,
            backup_result.backup_group,
            backup_result.tile_records,
        )?;

        let brush_backend = brushes
            .backend_mut(brush_id)
            .ok_or(EditorSessionError::BrushNotRegistered(brush_id))?;
        let _cached_group = brush_backend.archive_stroke(self.stroke)?;
        doc_renderer.clear_brush_preview_image();

        Ok(Some(tile_indices))
    }

    fn build_preview_commands(
        &mut self,
        doc: &GlaDoc,
        doc_renderer: &mut GlaDocRenderer,
        brushes: &BrushRegistry,
        brush_tile_manager: &TileManager,
        image_tile_manager: &TileManager,
        render_tile_manager: &TileManager,
        inputs: &[BrushInput],
    ) -> Result<Option<PreviewBuildResult>, EditorSessionError> {
        let active_image = doc.active_layer_image()?;
        let mut dirty_tile_indices = Vec::new();
        let mut commands = Vec::new();

        let image_ops = self.build_image_brush_ops(brushes, inputs)?;
        self.push_apply_dab_commands(
            brushes,
            brush_tile_manager,
            active_image,
            inputs,
            &image_ops,
            &mut commands,
            &mut dirty_tile_indices,
        )?;

        if dirty_tile_indices.is_empty() {
            return Ok(None);
        }

        dirty_tile_indices.sort_unstable();
        dirty_tile_indices.dedup();

        let merge_payload = brushes
            .merge_payload(self.stroke.brush_id())
            .ok_or(EditorSessionError::BrushNotRegistered(self.stroke.brush_id()))?;
        self.push_preview_merge_commands(
            doc,
            doc_renderer,
            brush_tile_manager,
            image_tile_manager,
            render_tile_manager,
            &dirty_tile_indices,
            &merge_payload,
            &mut commands,
        )?;

        Ok(Some(PreviewBuildResult {
            commands,
            dirty_tile_indices,
            merge_payload,
        }))
    }

    fn build_image_brush_ops(
        &self,
        brushes: &BrushRegistry,
        inputs: &[BrushInput],
    ) -> Result<Vec<ImageBrushOp>, EditorSessionError> {
        let mut ops = Vec::new();
        for (input_index, input) in inputs.iter().enumerate() {
            let max_affected_radius = brushes
                .max_affected_radius_px(input.brush_id)
                .ok_or(EditorSessionError::BrushNotRegistered(input.brush_id))?;
            for (block_index, _) in input.blocks.blocks().iter().enumerate() {
                let center = brushes.block_center(input, block_index)?;
                ops.push(ImageBrushOp {
                    input_index,
                    block_index,
                    center,
                    max_affected_radius,
                });
            }
        }
        Ok(ops)
    }

    fn push_apply_dab_commands(
        &mut self,
        brushes: &BrushRegistry,
        brush_tile_manager: &TileManager,
        active_image: &gla_image::GlaImage,
        inputs: &[BrushInput],
        image_ops: &[ImageBrushOp],
        commands: &mut Vec<RenderCommand>,
        dirty_tile_indices: &mut Vec<usize>,
    ) -> Result<(), EditorSessionError> {
        for image_op in image_ops {
            let mut affected_tiles = Vec::new();
            active_image.layout().collect_affected_tile_indices(
                image_op.center,
                image_op.max_affected_radius,
                &mut affected_tiles,
            );
            for tile_index in affected_tiles {
                let tile_origin = active_image.tile_canvas_origin(tile_index).ok_or(
                    EditorSessionError::Document(GlaDocError::InvalidSlotIndex {
                        slot_index: tile_index,
                        slot_count: active_image.slot_count(),
                    }),
                )?;
                let source_tile_key = self
                    .stroke
                    .brush_tiles()
                    .credential(tile_index)
                    .map(|credential| brush_tile_manager.resolve_destination_key(credential))
                    .transpose()?;
                let destination_credential = self.stroke.push_apply_dab(tile_index)?;
                let destination_tile_key =
                    brush_tile_manager.resolve_destination_key(destination_credential)?;
                let input = &inputs[image_op.input_index];
                let apply_payload =
                    brushes.encode_apply_dab_payload(input, image_op.block_index, tile_origin)?;
                commands.push(RenderCommand::ApplyDab(ApplyDabCommand {
                    brush_id: input.brush_id,
                    destination_tile_key,
                    source_tile_key,
                    brush_payload: apply_payload,
                }));
                dirty_tile_indices.push(tile_index);
            }
        }
        Ok(())
    }

    fn push_preview_merge_commands(
        &self,
        doc: &GlaDoc,
        doc_renderer: &mut GlaDocRenderer,
        brush_tile_manager: &TileManager,
        image_tile_manager: &TileManager,
        render_tile_manager: &TileManager,
        dirty_tile_indices: &[usize],
        merge_payload: &[u8],
        commands: &mut Vec<RenderCommand>,
    ) -> Result<(), EditorSessionError> {
        for &tile_index in dirty_tile_indices {
            let (origin_credential, preview_credential) =
                doc_renderer.ensure_brush_preview_merge_target(doc, tile_index)?;
            let origin_tile_key = image_tile_manager.resolve_source_key(origin_credential)?;
            let preview_tile_key = render_tile_manager.resolve_destination_key(preview_credential)?;
            if let Some(brush_credential) = self.stroke.preview_brush_tile_credential(tile_index) {
                let brush_tile_key = brush_tile_manager.resolve_destination_key(brush_credential)?;
                commands.push(RenderCommand::MergeTile(MergeTileCommand {
                    brush_id: self.stroke.brush_id(),
                    origin_tile_key,
                    brush_tile_key,
                    destination_tile_key: preview_tile_key,
                    brush_payload: merge_payload.to_vec(),
                }));
            }
        }
        Ok(())
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
