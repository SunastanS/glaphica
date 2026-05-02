use std::error::Error;
use std::fmt::{Display, Formatter};

use atlas::AtlasError;
use brush::{BrushId, BrushInput, BrushInputError, BrushStrokeError};
use gla_doc_renderer::{GlaDocRenderer, GlaDocRendererError};
use gla_document::{GlaDoc, GlaDocError, GlaDocUndoError, GlaImageUndoTileAction};
use renderer::{RenderCommand, TileRenderer, TileRendererError};

use crate::AppBrushRegistry;
use crate::editor::stroke_transaction::StrokeTransaction;

pub struct EditorSession {
    doc: GlaDoc,
    doc_renderer: GlaDocRenderer,
    brushes: AppBrushRegistry,
    active_stroke_transaction: Option<StrokeTransaction>,
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

    pub fn merge(&mut self, other: &EditorRenderUpdate) {
        self.tile_indices.extend_from_slice(other.tile_indices());
        self.prepared_active_plan |= other.prepared_active_plan();
        self.rendered_active_tiles |= other.rendered_active_tiles();
    }

    pub fn normalize(&mut self) {
        self.tile_indices.sort_unstable();
        self.tile_indices.dedup();
    }
}

impl EditorSession {
    pub fn new(doc: GlaDoc, doc_renderer: GlaDocRenderer, brushes: AppBrushRegistry) -> Self {
        Self {
            doc,
            doc_renderer,
            brushes,
            active_stroke_transaction: None,
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

    pub fn active_stroke(&self) -> Option<&brush::BrushStrokeState> {
        self.active_stroke_transaction
            .as_ref()
            .map(StrokeTransaction::stroke)
    }

    pub fn begin_stroke(&mut self, brush_id: BrushId) -> Result<(), EditorSessionError> {
        let stroke = self
            .brushes
            .begin_stroke(brush_id)
            .ok_or(EditorSessionError::BrushNotRegistered(brush_id))?;
        self.active_stroke_transaction = Some(StrokeTransaction::new(stroke));
        Ok(())
    }

    pub fn cancel_stroke(&mut self) {
        if let Some(transaction) = self.active_stroke_transaction.take() {
            transaction.cancel(&mut self.doc_renderer);
            return;
        }
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

    pub fn process_brush_input_gpu(
        &mut self,
        tile_renderer: &mut TileRenderer,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        input: &BrushInput,
    ) -> Result<EditorRenderUpdate, EditorSessionError> {
        Ok(self
            .process_brush_inputs_gpu(tile_renderer, device, queue, std::slice::from_ref(input))?
            .unwrap_or(EditorRenderUpdate {
                tile_indices: Vec::new(),
                prepared_active_plan: false,
                rendered_active_tiles: false,
            }))
    }

    pub fn process_brush_inputs_gpu(
        &mut self,
        tile_renderer: &mut TileRenderer,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        inputs: &[BrushInput],
    ) -> Result<Option<EditorRenderUpdate>, EditorSessionError> {
        if inputs.is_empty() {
            return Ok(None);
        }

        let dirty_tile_indices = {
            let transaction = self
                .active_stroke_transaction
                .as_mut()
                .ok_or(EditorSessionError::MissingActiveStroke)?;
            transaction.process_inputs_gpu(
                &self.doc,
                &mut self.doc_renderer,
                &self.brushes,
                tile_renderer,
                device,
                queue,
                inputs,
            )?
        };
        let Some(dirty_tile_indices) = dirty_tile_indices else {
            return Ok(None);
        };

        Ok(Some(self.refresh_active_tiles_gpu(
            tile_renderer,
            device,
            queue,
            &dirty_tile_indices,
        )?))
    }

    pub fn commit_active_stroke(
        &mut self,
        tile_renderer: &mut TileRenderer,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> Result<Option<EditorRenderUpdate>, EditorSessionError> {
        let Some(transaction) = self.active_stroke_transaction.take() else {
            return Ok(None);
        };
        let tile_indices = transaction.commit_gpu(
            &mut self.doc,
            &mut self.doc_renderer,
            &mut self.brushes,
            tile_renderer,
            device,
            queue,
        )?;
        let Some(tile_indices) = tile_indices else {
            return Ok(None);
        };

        Ok(Some(self.refresh_active_tiles_gpu(
            tile_renderer,
            device,
            queue,
            &tile_indices,
        )?))
    }

    pub fn undo_last_stroke(
        &mut self,
        tile_renderer: &mut TileRenderer,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> Result<Option<EditorRenderUpdate>, EditorSessionError> {
        let Some(restore) = self.doc.restore_last_undo()? else {
            return Ok(None);
        };

        let backends = self.doc.image_undo().backends();

        let commands = restore
            .image_restore()
            .tile_actions()
            .iter()
            .filter_map(|action| match action {
                GlaImageUndoTileAction::RestoreFromBackup { copy_command, .. } => {
                    Some(RenderCommand::CopyTile(*copy_command))
                }
                GlaImageUndoTileAction::Clear { .. } => None,
            })
            .collect::<Vec<_>>();
        let dirty_tile_indices = restore
            .image_restore()
            .tile_actions()
            .iter()
            .map(|action| match action {
                GlaImageUndoTileAction::RestoreFromBackup { tile_index, .. }
                | GlaImageUndoTileAction::Clear { tile_index } => *tile_index,
            })
            .collect::<Vec<_>>();

        let mut clear_batches = Vec::new();
        for backend in backends {
            clear_batches.extend(backend.take_pending_clear_batches()?);
        }
        tile_renderer.execute_commands(
            device,
            queue,
            &backends,
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
