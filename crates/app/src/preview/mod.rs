mod app_bootstrap;
mod app_controller;
mod app_present_loop;

use std::error::Error;
use std::fmt::{Display, Formatter};
use std::sync::Arc;
use std::time::{Duration, Instant};

use atlas::Backend;
use renderer::{GpuContext, TileRenderer};
use winit::application::ApplicationHandler;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::ModifiersState;
use winit::window::{Window, WindowId};

use crate::display::{SurfaceError, SurfaceRuntime};
use crate::{
    AppPresentError, AppRuntime, AppRuntimeError, AppViewMatrixError, ScreenPresentCache,
    ScreenPresentCacheError,
};

const DEFAULT_DOCUMENT_WIDTH: u32 = 1024;
const DEFAULT_DOCUMENT_HEIGHT: u32 = 1024;
const INPUT_RING_CAPACITY: usize = 256;
const BRUSH_RING_CAPACITY: usize = 256;
const WORKER_BATCH_CAPACITY: usize = 64;
const WORKER_WAIT_TIMEOUT: Duration = Duration::from_millis(1);
const MAX_PENDING_BRUSH_INPUTS_PER_FRAME: usize = 64;

#[derive(Debug)]
pub enum AppPreviewError {
    EventLoop(winit::error::EventLoopError),
}

impl Display for AppPreviewError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EventLoop(error) => Display::fmt(error, f),
        }
    }
}

impl Error for AppPreviewError {}

pub fn run_preview_window() -> Result<(), AppPreviewError> {
    let event_loop = EventLoop::new().map_err(AppPreviewError::EventLoop)?;
    let mut app = PreviewApp::default();
    event_loop
        .run_app(&mut app)
        .map_err(AppPreviewError::EventLoop)
}

#[derive(Debug)]
enum PreviewInitError {
    CreateWindow(winit::error::OsError),
    CreateSurface(SurfaceError),
    Gpu(renderer::GpuContextInitError),
    Atlas(atlas::AtlasError),
    AtlasManager(atlas::AtlasManagerError),
    Document(gla_document::GlaDocError),
    Runtime(AppRuntimeError),
    ScreenPresentCache(ScreenPresentCacheError),
    TileRenderer(renderer::TileRendererError),
    View(AppViewMatrixError),
    PuffinHttpServer(String),
}

impl Display for PreviewInitError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CreateWindow(error) => Display::fmt(error, f),
            Self::CreateSurface(error) => Display::fmt(error, f),
            Self::Gpu(error) => Display::fmt(error, f),
            Self::Atlas(error) => Display::fmt(error, f),
            Self::AtlasManager(error) => write!(f, "atlas backend manager failed: {error:?}"),
            Self::Document(error) => Display::fmt(error, f),
            Self::Runtime(error) => Display::fmt(error, f),
            Self::ScreenPresentCache(error) => Display::fmt(error, f),
            Self::TileRenderer(error) => Display::fmt(error, f),
            Self::View(error) => Display::fmt(error, f),
            Self::PuffinHttpServer(error) => {
                write!(f, "failed to start puffin http server: {error}")
            }
        }
    }
}

impl Error for PreviewInitError {}

impl From<SurfaceError> for PreviewInitError {
    fn from(error: SurfaceError) -> Self {
        Self::CreateSurface(error)
    }
}

impl From<renderer::GpuContextInitError> for PreviewInitError {
    fn from(error: renderer::GpuContextInitError) -> Self {
        Self::Gpu(error)
    }
}

impl From<atlas::AtlasError> for PreviewInitError {
    fn from(error: atlas::AtlasError) -> Self {
        Self::Atlas(error)
    }
}

impl From<atlas::AtlasManagerError> for PreviewInitError {
    fn from(error: atlas::AtlasManagerError) -> Self {
        Self::AtlasManager(error)
    }
}

impl From<gla_document::GlaDocError> for PreviewInitError {
    fn from(error: gla_document::GlaDocError) -> Self {
        Self::Document(error)
    }
}

impl From<AppRuntimeError> for PreviewInitError {
    fn from(error: AppRuntimeError) -> Self {
        Self::Runtime(error)
    }
}

impl From<ScreenPresentCacheError> for PreviewInitError {
    fn from(error: ScreenPresentCacheError) -> Self {
        Self::ScreenPresentCache(error)
    }
}

impl From<renderer::TileRendererError> for PreviewInitError {
    fn from(error: renderer::TileRendererError) -> Self {
        Self::TileRenderer(error)
    }
}

impl From<AppViewMatrixError> for PreviewInitError {
    fn from(error: AppViewMatrixError) -> Self {
        Self::View(error)
    }
}

impl From<winit::error::OsError> for PreviewInitError {
    fn from(error: winit::error::OsError) -> Self {
        Self::CreateWindow(error)
    }
}

#[derive(Debug)]
enum PreviewRuntimeError {
    Runtime(AppRuntimeError),
    Present(AppPresentError),
    ScreenPresentCache(ScreenPresentCacheError),
    View(AppViewMatrixError),
}

impl Display for PreviewRuntimeError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Runtime(error) => Display::fmt(error, f),
            Self::Present(error) => Display::fmt(error, f),
            Self::ScreenPresentCache(error) => Display::fmt(error, f),
            Self::View(error) => Display::fmt(error, f),
        }
    }
}

impl Error for PreviewRuntimeError {}

impl From<AppRuntimeError> for PreviewRuntimeError {
    fn from(error: AppRuntimeError) -> Self {
        Self::Runtime(error)
    }
}

impl From<AppPresentError> for PreviewRuntimeError {
    fn from(error: AppPresentError) -> Self {
        Self::Present(error)
    }
}

impl From<AppViewMatrixError> for PreviewRuntimeError {
    fn from(error: AppViewMatrixError) -> Self {
        Self::View(error)
    }
}

impl From<ScreenPresentCacheError> for PreviewRuntimeError {
    fn from(error: ScreenPresentCacheError) -> Self {
        Self::ScreenPresentCache(error)
    }
}

#[derive(Default)]
struct PreviewApp {
    state: Option<PreviewState>,
}

struct PreviewState {
    window: Arc<Window>,
    gpu: GpuContext,
    surface: SurfaceRuntime,
    screen_cache: ScreenPresentCache,
    runtime: Option<AppRuntime>,
    tile_renderer: TileRenderer,
    image_backend: Backend,
    full_tile_indices: Vec<usize>,
    started_at: Instant,
    cursor_position: Option<glaphica_core::ScreenVec2>,
    modifiers: ModifiersState,
    stroke_active: bool,
    perf_trace: app_present_loop::PreviewPerfTraceConfig,
    perf_frame_seq: u64,
    _puffin_server: Option<puffin_http::Server>,
}

impl ApplicationHandler for PreviewApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_some() {
            return;
        }

        match PreviewState::new(event_loop) {
            Ok(state) => {
                state.window.request_redraw();
                self.state = Some(state);
            }
            Err(error) => {
                eprintln!("preview init failed: {error}");
                event_loop.exit();
            }
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: winit::event::WindowEvent,
    ) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        if state.window.id() != window_id {
            return;
        }

        match state.handle_window_event(event) {
            Ok(PreviewEventAction::None) => {}
            Ok(PreviewEventAction::RequestRedraw) => state.window.request_redraw(),
            Ok(PreviewEventAction::Shutdown) => {
                state.shutdown();
                event_loop.exit();
            }
            Err(error) => {
                eprintln!("preview runtime failed: {error}");
                state.shutdown();
                event_loop.exit();
            }
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(state) = &self.state {
            let scheduled_redraw = state
                .runtime
                .as_ref()
                .map(|runtime| runtime.frame_scheduler().has_requested_redraw())
                .unwrap_or(false);
            if state.stroke_active || scheduled_redraw {
                state.window.request_redraw();
            }
        }
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(state) = self.state.as_mut() {
            state.shutdown();
        }
    }
}

enum PreviewEventAction {
    None,
    RequestRedraw,
    Shutdown,
}
