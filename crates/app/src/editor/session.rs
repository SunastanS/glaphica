use std::error::Error;
use std::fmt::{Display, Formatter};

use atlas::{AtlasError, Backend};
use brush::{BrushId, BrushInput, BrushInputError, BrushStrokeError, BrushStrokeState};
use gla_doc_renderer::{GlaDocRenderer, GlaDocRendererError};
use gla_document::{
    DocumentUndoTileRecord, GlaDoc, GlaDocError, GlaDocUndoError, GlaDocUndoTileAction,
};
use renderer::{CopyTileCommand, RenderCommand, TileRenderer, TileRendererError};

use crate::AppBrushRegistry;

pub struct EditorSession {
    doc: GlaDoc,
    doc_renderer: GlaDocRenderer,
    brushes: AppBrushRegistry,
    active_stroke: Option<BrushStrokeState>,
    active_merge_payload: Option<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorRenderUpdate {
    tile_indices: Vec<usize>,
    prepared_active_plan: bool,
    rendered_active_tiles: bool,
}

#[derive(Debug)]
pub enum EditorSessionError {
    Document(GlaDocError),
    DocumentUndo(GlaDocUndoError),
    DocRenderer(GlaDocRendererError),
    Brush(BrushStrokeError),
    BrushInput(BrushInputError),
    TileRenderer(TileRendererError),
    Atlas(AtlasError),
    MissingActiveStroke,
    MissingActiveMergePayload,
    BrushNotRegistered(BrushId),
}

impl Display for EditorSessionError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Document(error) => Display::fmt(error, f),
            Self::DocumentUndo(error) => Display::fmt(error, f),
            Self::DocRenderer(error) => Display::fmt(error, f),
            Self::Brush(error) => Display::fmt(error, f),
            Self::BrushInput(error) => Display::fmt(error, f),
            Self::TileRenderer(error) => Display::fmt(error, f),
            Self::Atlas(error) => Display::fmt(error, f),
            Self::MissingActiveStroke => f.write_str("missing active stroke"),
            Self::MissingActiveMergePayload => f.write_str("missing active merge payload"),
            Self::BrushNotRegistered(brush_id) => {
                write!(f, "brush {} is not registered", brush_id.raw())
            }
        }
    }
}

impl Error for EditorSessionError {}

impl From<GlaDocError> for EditorSessionError {
    fn from(error: GlaDocError) -> Self {
        Self::Document(error)
    }
}

impl From<GlaDocUndoError> for EditorSessionError {
    fn from(error: GlaDocUndoError) -> Self {
        Self::DocumentUndo(error)
    }
}

impl From<GlaDocRendererError> for EditorSessionError {
    fn from(error: GlaDocRendererError) -> Self {
        Self::DocRenderer(error)
    }
}

impl From<BrushStrokeError> for EditorSessionError {
    fn from(error: BrushStrokeError) -> Self {
        Self::Brush(error)
    }
}

impl From<BrushInputError> for EditorSessionError {
    fn from(error: BrushInputError) -> Self {
        Self::BrushInput(error)
    }
}

impl From<TileRendererError> for EditorSessionError {
    fn from(error: TileRendererError) -> Self {
        Self::TileRenderer(error)
    }
}

impl From<AtlasError> for EditorSessionError {
    fn from(error: AtlasError) -> Self {
        Self::Atlas(error)
    }
}

impl EditorRenderUpdate {
    pub fn tile_indices(&self) -> &[usize] {
        &self.tile_indices
    }

    pub fn prepared_active_plan(&self) -> bool {
        self.prepared_active_plan
    }

    pub fn rendered_active_tiles(&self) -> bool {
        self.rendered_active_tiles
    }
}

impl EditorSession {
    pub fn new(doc: GlaDoc, doc_renderer: GlaDocRenderer, brushes: AppBrushRegistry) -> Self {
        Self {
            doc,
            doc_renderer,
            brushes,
            active_stroke: None,
            active_merge_payload: None,
        }
    }

    pub fn doc(&self) -> &GlaDoc {
        &self.doc
    }

    pub fn doc_mut(&mut self) -> &mut GlaDoc {
        &mut self.doc
    }

    pub fn doc_renderer(&self) -> &GlaDocRenderer {
        &self.doc_renderer
    }

    pub fn doc_renderer_mut(&mut self) -> &mut GlaDocRenderer {
        &mut self.doc_renderer
    }

    pub fn brushes(&self) -> &AppBrushRegistry {
        &self.brushes
    }

    pub fn brushes_mut(&mut self) -> &mut AppBrushRegistry {
        &mut self.brushes
    }

    pub fn active_stroke(&self) -> Option<&BrushStrokeState> {
        self.active_stroke.as_ref()
    }

    pub fn begin_stroke(&mut self, brush_id: BrushId) -> Result<(), EditorSessionError> {
        let stroke = self
            .brushes
            .begin_stroke(brush_id)
            .ok_or(EditorSessionError::BrushNotRegistered(brush_id))?;
        self.active_stroke = Some(stroke);
        self.active_merge_payload = None;
        Ok(())
    }

    pub fn cancel_stroke(&mut self) {
        self.active_stroke = None;
        self.active_merge_payload = None;
        self.doc_renderer.clear_brush_preview_image();
    }

    pub fn prepare_document_gpu(
        &mut self,
        tile_renderer: &mut TileRenderer,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> Result<EditorRenderUpdate, EditorSessionError> {
        let refresh = self.doc.build_full_render_refresh()?;
        self.doc_renderer
            .prepare_active_plan_gpu(&self.doc, device, queue, tile_renderer)?;
        self.doc_renderer.render_active_tiles_gpu(
            &self.doc,
            device,
            queue,
            tile_renderer,
            &refresh.tile_indices,
        )?;
        Ok(EditorRenderUpdate {
            tile_indices: refresh.tile_indices,
            prepared_active_plan: true,
            rendered_active_tiles: true,
        })
    }

    pub fn refresh_active_tiles_gpu(
        &mut self,
        tile_renderer: &mut TileRenderer,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        dirty_tile_indices: &[usize],
    ) -> Result<EditorRenderUpdate, EditorSessionError> {
        let tile_indices = self.normalized_dirty_tile_indices(dirty_tile_indices)?;
        if tile_indices.is_empty() {
            return Ok(EditorRenderUpdate {
                tile_indices,
                prepared_active_plan: false,
                rendered_active_tiles: false,
            });
        }

        let mut prepared_active_plan = false;
        if self.doc_renderer.active_plan().is_none() {
            self.doc_renderer
                .prepare_active_plan_gpu(&self.doc, device, queue, tile_renderer)?;
            prepared_active_plan = true;
        }
        self.doc_renderer.render_active_tiles_gpu(
            &self.doc,
            device,
            queue,
            tile_renderer,
            &tile_indices,
        )?;
        Ok(EditorRenderUpdate {
            tile_indices,
            prepared_active_plan,
            rendered_active_tiles: true,
        })
    }

    pub fn execute_preview_commands_gpu(
        &mut self,
        image_backend: &Backend,
        tile_renderer: &mut TileRenderer,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        dirty_tile_indices: &[usize],
        commands: &[RenderCommand],
    ) -> Result<EditorRenderUpdate, EditorSessionError> {
        let active_stroke = self
            .active_stroke
            .as_ref()
            .ok_or(EditorSessionError::MissingActiveStroke)?;
        let brush_backend = self
            .brushes
            .brush_backend(active_stroke.brush_id())
            .ok_or(EditorSessionError::BrushNotRegistered(active_stroke.brush_id()))?;
        let intermediate_backend = brush_backend.intermediate_backend();
        let render_backend = self.doc_renderer.render_backend();

        tile_renderer.ensure_backend(device, image_backend)?;
        tile_renderer.ensure_backend(device, intermediate_backend)?;
        tile_renderer.ensure_backend(device, render_backend)?;

        let mut clear_batches = intermediate_backend.take_pending_clear_batches()?;
        clear_batches.extend(render_backend.take_pending_clear_batches()?);
        tile_renderer.execute_commands_with_shader_provider(
            device,
            queue,
            &[image_backend, intermediate_backend, render_backend],
            &clear_batches,
            commands,
            None,
            &self.brushes,
        )?;

        self.refresh_active_tiles_gpu(tile_renderer, device, queue, dirty_tile_indices)
    }

    pub fn process_brush_input_gpu(
        &mut self,
        image_backend: &Backend,
        tile_renderer: &mut TileRenderer,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        input: &BrushInput,
    ) -> Result<EditorRenderUpdate, EditorSessionError> {
        let max_affected_radius = self
            .brushes
            .max_affected_radius_px(input.brush_id)
            .ok_or(EditorSessionError::BrushNotRegistered(input.brush_id))?;
        let merge_payload = self.brushes.encode_merge_payload(input)?;
        let mut dirty_tile_indices = Vec::new();
        let mut commands = Vec::new();

        {
            let active_image = self.doc.active_layer_image()?;
            let stroke = self
                .active_stroke
                .as_mut()
                .ok_or(EditorSessionError::MissingActiveStroke)?;

            for (block_index, _) in input.blocks.blocks().iter().enumerate() {
                let center = self.brushes.block_center(input, block_index)?;
                let mut affected_tiles = Vec::new();
                active_image.layout().collect_affected_tile_indices(
                    center,
                    max_affected_radius,
                    &mut affected_tiles,
                );
                for tile_index in affected_tiles {
                    let tile_origin = active_image
                        .tile_canvas_origin(tile_index)
                        .ok_or(EditorSessionError::Document(GlaDocError::InvalidTileIndex {
                            tile_index,
                            tile_count: active_image.tile_count(),
                        }))?;
                    let source_tile_key = stroke.intermediate().tile_key(tile_index);
                    let apply_payload = self
                        .brushes
                        .encode_apply_dab_payload(input, block_index, tile_origin)?;
                    stroke.push_apply_dab(
                        tile_index,
                        source_tile_key,
                        apply_payload,
                        &mut commands,
                    )?;
                    dirty_tile_indices.push(tile_index);
                }
            }
        }

        dirty_tile_indices.sort_unstable();
        dirty_tile_indices.dedup();
        self.active_merge_payload = Some(merge_payload.clone());
        {
            let stroke = self
                .active_stroke
                .as_ref()
                .ok_or(EditorSessionError::MissingActiveStroke)?;
            for &tile_index in &dirty_tile_indices {
                let (origin_tile_key, preview_tile_key) = self
                    .doc_renderer
                    .ensure_brush_preview_merge_target(&self.doc, tile_index)?;
                stroke.push_preview_merge(
                    tile_index,
                    origin_tile_key,
                    preview_tile_key,
                    merge_payload.clone(),
                    &mut commands,
                );
            }
        }

        self.execute_preview_commands_gpu(
            image_backend,
            tile_renderer,
            device,
            queue,
            &dirty_tile_indices,
            &commands,
        )
    }

    pub fn commit_active_stroke(
        &mut self,
        image_backend: &Backend,
        tile_renderer: &mut TileRenderer,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        tile_indices: &[usize],
    ) -> Result<Option<EditorRenderUpdate>, EditorSessionError> {
        let Some(mut stroke) = self.active_stroke.take() else {
            return Ok(None);
        };
        let merge_payload = self
            .active_merge_payload
            .take()
            .ok_or(EditorSessionError::MissingActiveMergePayload)?;

        let brush_id = stroke.brush_id();
        let intermediate_backend = self
            .brushes
            .brush_backend(brush_id)
            .ok_or(EditorSessionError::BrushNotRegistered(brush_id))?
            .intermediate_backend()
            .clone();
        let active_layer_id = self.doc.active_layer_id();
        let batch = {
            let (image, backup_store) = self.doc.active_layer_image_and_backup_store_mut()?;
            stroke.build_commit_batch(
                image,
                image_backend,
                backup_store,
                tile_indices,
                merge_payload,
            )?
        };

        let backup_backend = self.doc.undo_stack().backup_store().backend();
        let render_backend = self.doc_renderer.render_backend();
        tile_renderer.ensure_backend(device, image_backend)?;
        tile_renderer.ensure_backend(device, &intermediate_backend)?;
        tile_renderer.ensure_backend(device, backup_backend)?;
        tile_renderer.ensure_backend(device, render_backend)?;

        let mut clear_batches = image_backend.take_pending_clear_batches()?;
        clear_batches.extend(intermediate_backend.take_pending_clear_batches()?);
        clear_batches.extend(render_backend.take_pending_clear_batches()?);
        clear_batches.extend(backup_backend.take_pending_clear_batches()?);
        tile_renderer.execute_commands_with_shader_provider(
            device,
            queue,
            &[image_backend, &intermediate_backend, render_backend, backup_backend],
            &clear_batches,
            &batch.commands,
            None,
            &self.brushes,
        )?;

        self.doc.push_undo_entry(
            active_layer_id,
            batch.backup_group,
            batch.tile_records
                .into_iter()
                .map(|record| DocumentUndoTileRecord::new(record.tile_index, record.backup_tile_key))
                .collect(),
        )?;

        let brush_backend = self
            .brushes
            .brush_backend_mut(brush_id)
            .ok_or(EditorSessionError::BrushNotRegistered(brush_id))?;
        brush_backend.archive_stroke(stroke)?;
        self.doc_renderer.clear_brush_preview_image();

        Ok(Some(self.refresh_active_tiles_gpu(
            tile_renderer,
            device,
            queue,
            tile_indices,
        )?))
    }

    pub fn undo_last_stroke(
        &mut self,
        image_backend: &Backend,
        tile_renderer: &mut TileRenderer,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> Result<Option<EditorRenderUpdate>, EditorSessionError> {
        let Some(restore) = self.doc.restore_last_undo(image_backend)? else {
            return Ok(None);
        };

        let backup_backend = self.doc.undo_stack().backup_store().backend();
        tile_renderer.ensure_backend(device, image_backend)?;
        tile_renderer.ensure_backend(device, backup_backend)?;

        let commands = restore
            .tile_actions()
            .iter()
            .filter_map(|action| match action {
                GlaDocUndoTileAction::RestoreFromBackup {
                    source_tile_key,
                    destination_tile_key,
                    ..
                } => Some(RenderCommand::CopyTile(CopyTileCommand {
                    source_tile_key: *source_tile_key,
                    destination_tile_key: *destination_tile_key,
                })),
                GlaDocUndoTileAction::Clear { .. } => None,
            })
            .collect::<Vec<_>>();
        let dirty_tile_indices = restore
            .tile_actions()
            .iter()
            .map(|action| match action {
                GlaDocUndoTileAction::RestoreFromBackup { tile_index, .. }
                | GlaDocUndoTileAction::Clear { tile_index } => *tile_index,
            })
            .collect::<Vec<_>>();

        let mut clear_batches = image_backend.take_pending_clear_batches()?;
        clear_batches.extend(backup_backend.take_pending_clear_batches()?);
        tile_renderer.execute_commands(
            device,
            queue,
            &[image_backend, backup_backend],
            &clear_batches,
            &commands,
            None,
        )?;

        Ok(Some(self.refresh_active_tiles_gpu(
            tile_renderer,
            device,
            queue,
            &dirty_tile_indices,
        )?))
    }

    fn normalized_dirty_tile_indices(
        &self,
        dirty_tile_indices: &[usize],
    ) -> Result<Vec<usize>, EditorSessionError> {
        let Some(refresh) = self
            .doc
            .build_active_layer_incremental_refresh(dirty_tile_indices)?
        else {
            return Ok(Vec::new());
        };
        Ok(refresh.tile_indices)
    }
}
