use std::error::Error;
use std::fmt::{Display, Formatter};
use std::time::Duration;

use atlas::Backend;
use glaphica_core::{CanvasInput, RadianVec2, ScreenVec2};
use renderer::TileRenderer;

use crate::{
    ActiveTool, AppBrushRegistry, AppFrameScheduler, AppView, BrushThreadRuntime,
    BrushThreadRuntimeError, EditorRenderUpdate, EditorSession, EditorSessionError, ToolSet,
};

pub struct AppRuntime {
    session: EditorSession,
    brush_thread: BrushThreadRuntime,
    view: AppView,
    frame_scheduler: AppFrameScheduler,
}

#[derive(Debug)]
pub enum AppRuntimeError {
    Session(EditorSessionError),
    BrushThread(BrushThreadRuntimeError),
}

impl Display for AppRuntimeError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Session(error) => Display::fmt(error, f),
            Self::BrushThread(error) => Display::fmt(error, f),
        }
    }
}

impl Error for AppRuntimeError {}

impl From<EditorSessionError> for AppRuntimeError {
    fn from(error: EditorSessionError) -> Self {
        Self::Session(error)
    }
}

impl From<BrushThreadRuntimeError> for AppRuntimeError {
    fn from(error: BrushThreadRuntimeError) -> Self {
        Self::BrushThread(error)
    }
}

impl AppRuntime {
    pub fn new(session: EditorSession, brush_thread: BrushThreadRuntime, view: AppView) -> Self {
        Self {
            session,
            brush_thread,
            view,
            frame_scheduler: AppFrameScheduler::new(),
        }
    }

    pub fn spawn(
        doc: gla_document::GlaDoc,
        doc_renderer: gla_doc_renderer::GlaDocRenderer,
        session_brushes: AppBrushRegistry,
        worker_brushes: AppBrushRegistry,
        tool_set: ToolSet,
        active_tool: ActiveTool,
        view: AppView,
        canvas_input_capacity: usize,
        brush_input_capacity: usize,
        worker_batch_capacity: usize,
        worker_wait_timeout: Duration,
    ) -> Result<Self, AppRuntimeError> {
        let session = EditorSession::new(doc, doc_renderer, session_brushes);
        let brush_thread = BrushThreadRuntime::spawn(
            worker_brushes,
            tool_set,
            active_tool,
            canvas_input_capacity,
            brush_input_capacity,
            worker_batch_capacity,
            worker_wait_timeout,
        )?;
        Ok(Self::new(session, brush_thread, view))
    }

    pub fn session(&self) -> &EditorSession {
        &self.session
    }

    pub fn session_mut(&mut self) -> &mut EditorSession {
        &mut self.session
    }

    pub fn brush_thread(&self) -> &BrushThreadRuntime {
        &self.brush_thread
    }

    pub fn view(&self) -> &AppView {
        &self.view
    }

    pub fn view_mut(&mut self) -> &mut AppView {
        &mut self.view
    }

    pub fn tool_set(&self) -> &ToolSet {
        self.brush_thread.tool_set()
    }

    pub fn active_tool(&self) -> ActiveTool {
        self.brush_thread.active_tool()
    }

    pub fn set_active_tool(&self, active_tool: ActiveTool) -> Result<(), AppRuntimeError> {
        self.brush_thread.set_active_tool(active_tool)?;
        self.brush_thread.reset_active_stroke_processing();
        Ok(())
    }

    pub fn begin_active_tool_stroke(&mut self) -> Result<(), AppRuntimeError> {
        self.discard_pending_brush_inputs();
        self.brush_thread.reset_active_stroke_processing();
        match self.brush_thread.active_tool() {
            ActiveTool::Brush(brush_id) => self.session.begin_stroke(brush_id)?,
        }
        self.frame_scheduler.request_redraw();
        Ok(())
    }

    pub fn cancel_stroke(&mut self) {
        self.session.cancel_stroke();
        self.brush_thread.reset_active_stroke_processing();
        self.frame_scheduler.request_redraw();
    }

    pub fn prepare_document_gpu(
        &mut self,
        tile_renderer: &mut TileRenderer,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> Result<EditorRenderUpdate, AppRuntimeError> {
        let update = self
            .session
            .prepare_document_gpu(tile_renderer, device, queue)?;
        self.frame_scheduler.schedule_render_update(&update);
        Ok(update)
    }

    pub fn push_canvas_input(&self, input: CanvasInput) {
        self.brush_thread.canvas_input_producer().push(input);
    }

    pub fn push_screen_input(
        &self,
        time_ns: u64,
        position: ScreenVec2,
        pressure: f32,
        tilt: RadianVec2,
        twist: f32,
    ) {
        self.push_canvas_input(CanvasInput {
            time_ns,
            position: self.view.screen_to_document_point(position),
            pressure,
            tilt,
            twist,
        });
    }

    pub fn process_pending_brush_input_gpu(
        &mut self,
        image_backend: &Backend,
        tile_renderer: &mut TileRenderer,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        max_inputs: usize,
    ) -> Result<Option<EditorRenderUpdate>, AppRuntimeError> {
        self.frame_scheduler.drain_brush_inputs(
            self.brush_thread.brush_input_consumer(),
            max_inputs,
            Duration::ZERO,
        );
        if self.session.active_stroke().is_none() {
            self.frame_scheduler.clear_pending_brush_inputs();
            return Ok(None);
        }
        let update = self.session.process_brush_inputs_gpu(
            image_backend,
            tile_renderer,
            device,
            queue,
            self.frame_scheduler.pending_brush_inputs(),
        )?;
        if let Some(update) = update.as_ref() {
            self.frame_scheduler.schedule_render_update(update);
        }
        self.frame_scheduler.finish_brush_inputs();
        Ok(update)
    }

    pub fn end_active_tool_stroke_gpu(
        &mut self,
        image_backend: &Backend,
        tile_renderer: &mut TileRenderer,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        max_pending_inputs: usize,
    ) -> Result<Option<EditorRenderUpdate>, AppRuntimeError> {
        self.brush_thread.finish_active_stroke_processing()?;
        self.process_pending_brush_input_gpu(
            image_backend,
            tile_renderer,
            device,
            queue,
            max_pending_inputs,
        )?;
        let update =
            self.session
                .commit_active_stroke(image_backend, tile_renderer, device, queue)?;
        self.brush_thread.reset_active_stroke_processing();
        if let Some(update) = update.as_ref() {
            self.frame_scheduler.schedule_render_update(update);
        }
        Ok(update)
    }

    pub fn undo_last_stroke_gpu(
        &mut self,
        image_backend: &Backend,
        tile_renderer: &mut TileRenderer,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> Result<Option<EditorRenderUpdate>, AppRuntimeError> {
        let update = self
            .session
            .undo_last_stroke(image_backend, tile_renderer, device, queue)?;
        if let Some(update) = update.as_ref() {
            self.frame_scheduler.schedule_render_update(update);
        }
        Ok(update)
    }

    pub fn frame_scheduler(&self) -> &AppFrameScheduler {
        &self.frame_scheduler
    }

    pub fn frame_scheduler_mut(&mut self) -> &mut AppFrameScheduler {
        &mut self.frame_scheduler
    }

    pub fn shutdown(self) -> Result<(), AppRuntimeError> {
        self.brush_thread.shutdown()?;
        Ok(())
    }

    fn discard_pending_brush_inputs(&mut self) {
        const DISCARD_BATCH_SIZE: usize = 64;
        self.frame_scheduler.clear_pending_brush_inputs();
        loop {
            self.frame_scheduler.drain_brush_inputs(
                self.brush_thread.brush_input_consumer(),
                DISCARD_BATCH_SIZE,
                Duration::ZERO,
            );
            if self.frame_scheduler.pending_brush_inputs().is_empty() {
                break;
            }
            self.frame_scheduler.clear_pending_brush_inputs();
        }
        self.frame_scheduler.clear_pending_brush_inputs();
    }
}
