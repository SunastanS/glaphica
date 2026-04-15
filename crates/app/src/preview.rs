use std::error::Error;
use std::env;
use std::fmt::{Display, Formatter};
use std::sync::Arc;
use std::time::{Duration, Instant};

use atlas::{AtlasLayout, Backend, BackendManager};
use brush::round::ROUND_BRUSH_ID;
use gla_doc_renderer::GlaDocRenderer;
use gla_document::{GlaDoc, GlaDocError, GlaImageCreateError, GlaImageLayout};
use gla_image::GlaImageTileAccessError;
use glaphica_core::{ATLAS_TILE_SIZE, RadianVec2, ScreenVec2};
use renderer::{GpuContext, GpuContextInitDescriptor, RenderTarget2d, TileRenderer};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::{Key, ModifiersState};
use winit::window::{Window, WindowAttributes, WindowId};

use crate::display::{SurfaceError, SurfaceRuntime};
use crate::{
    ActiveTool, AppBrushRegistry, AppPresentError, AppRuntime, AppRuntimeError, AppView,
    AppViewMatrixError, ScreenPresentCache, ScreenPresentCacheError, Tool, ToolSet,
    present_root_tiles,
};

const DEFAULT_DOCUMENT_WIDTH: u32 = 1024;
const DEFAULT_DOCUMENT_HEIGHT: u32 = 1024;
const DEFAULT_IMAGE_ATLAS_LAYOUT: AtlasLayout = AtlasLayout::Small11;
const DEFAULT_RENDER_ATLAS_LAYOUT: AtlasLayout = AtlasLayout::Small11;
const DEFAULT_BACKUP_ATLAS_LAYOUT: AtlasLayout = AtlasLayout::Small11;
const DEFAULT_BRUSH_ATLAS_LAYOUT: AtlasLayout = AtlasLayout::Small11;
const INPUT_RING_CAPACITY: usize = 256;
const BRUSH_RING_CAPACITY: usize = 256;
const WORKER_BATCH_CAPACITY: usize = 64;
const WORKER_WAIT_TIMEOUT: Duration = Duration::from_millis(1);
const MAX_PENDING_BRUSH_INPUTS_PER_FRAME: usize = 64;
const DEFAULT_BACKGROUND_COLOR: wgpu::Color = wgpu::Color {
    r: 0.12,
    g: 0.12,
    b: 0.12,
    a: 1.0,
};

#[derive(Debug, Clone, Copy)]
struct PreviewPerfTraceConfig {
    enabled: bool,
    slow_threshold: Duration,
}

impl PreviewPerfTraceConfig {
    fn from_env() -> Self {
        let enabled = env_flag("GLAPHICA_PREVIEW_PERF_TRACE");
        let slow_threshold = env_millis("GLAPHICA_PREVIEW_PERF_TRACE_SLOW_MS")
            .map(Duration::from_millis)
            .unwrap_or(Duration::from_millis(8));
        Self {
            enabled,
            slow_threshold,
        }
    }
}

#[derive(Debug, Default, Clone, Copy)]
struct PreviewFramePerf {
    process_inputs: Duration,
    update_cache: Duration,
    acquire_frame: Duration,
    present_surface: Duration,
    dirty_tile_count: usize,
}

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
    Document(GlaDocError),
    Runtime(AppRuntimeError),
    ScreenPresentCache(ScreenPresentCacheError),
    TileRenderer(renderer::TileRendererError),
    View(AppViewMatrixError),
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

impl From<GlaDocError> for PreviewInitError {
    fn from(error: GlaDocError) -> Self {
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
    cursor_position: Option<ScreenVec2>,
    modifiers: ModifiersState,
    stroke_active: bool,
    perf_trace: PreviewPerfTraceConfig,
    perf_frame_seq: u64,
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
        event: WindowEvent,
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

impl PreviewState {
    fn new(event_loop: &ActiveEventLoop) -> Result<Self, PreviewInitError> {
        let window = Arc::new(
            event_loop
                .create_window(WindowAttributes::default().with_title("glaphica-dev preview"))?,
        );
        let size = window.inner_size();
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        let surface = instance
            .create_surface(window.clone())
            .map_err(SurfaceError::CreateSurface)?;
        let gpu = pollster::block_on(GpuContext::init_with_instance_and_surface(
            &GpuContextInitDescriptor::default(),
            instance,
            Some(&surface),
        ))?;
        let surface = SurfaceRuntime::new(
            surface,
            &gpu.adapter,
            &gpu.device,
            size.width.max(1),
            size.height.max(1),
        )?;

        let mut backend_manager = BackendManager::new();
        let image_backend_id = backend_manager.add_backend(DEFAULT_IMAGE_ATLAS_LAYOUT)?;
        let render_backend_id = backend_manager.add_backend(DEFAULT_RENDER_ATLAS_LAYOUT)?;
        let backup_backend_id = backend_manager.add_backend(DEFAULT_BACKUP_ATLAS_LAYOUT)?;
        let brush_backend_id = backend_manager.add_backend(DEFAULT_BRUSH_ATLAS_LAYOUT)?;
        let image_backend = backend_manager
            .backend(image_backend_id)
            .ok_or(atlas::AtlasError::WrongBackend)?
            .clone();
        let render_backend = backend_manager
            .backend(render_backend_id)
            .ok_or(atlas::AtlasError::WrongBackend)?
            .clone();
        let backup_backend = backend_manager
            .backend(backup_backend_id)
            .ok_or(atlas::AtlasError::WrongBackend)?
            .clone();
        let brush_backend = backend_manager
            .backend(brush_backend_id)
            .ok_or(atlas::AtlasError::WrongBackend)?
            .clone();

        let mut tile_renderer = TileRenderer::new(&gpu.device)?;
        let screen_cache = ScreenPresentCache::new(
            &gpu.device,
            surface.format(),
            surface.width(),
            surface.height(),
        )?;
        tile_renderer.ensure_backend(&gpu.device, &image_backend)?;
        tile_renderer.ensure_backend(&gpu.device, &render_backend)?;
        tile_renderer.ensure_backend(&gpu.device, &backup_backend)?;
        tile_renderer.ensure_backend(&gpu.device, &brush_backend)?;

        let mut doc = GlaDoc::new(
            GlaImageLayout::new(DEFAULT_DOCUMENT_WIDTH, DEFAULT_DOCUMENT_HEIGHT),
            image_backend_id,
            render_backend_id,
            backup_backend,
        )?;
        let active_layer = doc.append_layer(doc.root_id())?;
        doc.set_active_layer(active_layer)?;
        initialize_default_canvas_white(
            &mut doc,
            &image_backend,
            &mut tile_renderer,
            &gpu.device,
            &gpu.queue,
        )?;

        let session_brushes = AppBrushRegistry::with_builtin_round(brush_backend.clone());
        let worker_brushes = AppBrushRegistry::with_builtin_round(brush_backend);
        let tool_set = ToolSet::new(vec![Tool::Brush(ROUND_BRUSH_ID)]);
        let active_tool = ActiveTool::Brush(ROUND_BRUSH_ID);
        let view = fitted_view(DEFAULT_DOCUMENT_WIDTH, DEFAULT_DOCUMENT_HEIGHT, size.width, size.height)?;
        let doc_renderer = GlaDocRenderer::new(render_backend);
        let mut runtime = AppRuntime::spawn(
            doc,
            doc_renderer,
            session_brushes,
            worker_brushes,
            tool_set,
            active_tool,
            view,
            INPUT_RING_CAPACITY,
            BRUSH_RING_CAPACITY,
            WORKER_BATCH_CAPACITY,
            WORKER_WAIT_TIMEOUT,
        )?;
        runtime.prepare_document_gpu(&mut tile_renderer, &gpu.device, &gpu.queue)?;

        let full_tile_count = usize::try_from(runtime.session().doc().layout().total_tiles())
            .map_err(|_| GlaDocError::ImageCreate(GlaImageCreateError::TooManyTiles))?;
        let full_tile_indices = (0..full_tile_count).collect::<Vec<_>>();

        Ok(Self {
            window,
            gpu,
            surface,
            screen_cache,
            runtime: Some(runtime),
            tile_renderer,
            image_backend,
            full_tile_indices,
            started_at: Instant::now(),
            cursor_position: None,
            modifiers: ModifiersState::default(),
            stroke_active: false,
            perf_trace: PreviewPerfTraceConfig::from_env(),
            perf_frame_seq: 0,
        })
    }

    fn handle_window_event(
        &mut self,
        event: WindowEvent,
    ) -> Result<PreviewEventAction, PreviewRuntimeError> {
        match event {
            WindowEvent::CloseRequested => Ok(PreviewEventAction::Shutdown),
            WindowEvent::Resized(size) => {
                self.surface
                    .resize(&self.gpu.device, size.width.max(1), size.height.max(1));
                self.screen_cache.resize(
                    &self.gpu.device,
                    self.surface.format(),
                    self.surface.width(),
                    self.surface.height(),
                )?;
                self.update_view(self.surface.width(), self.surface.height())?;
                if let Some(runtime) = self.runtime.as_mut() {
                    runtime
                        .frame_scheduler_mut()
                        .schedule_tile_indices(&self.full_tile_indices);
                }
                Ok(PreviewEventAction::RequestRedraw)
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                self.modifiers = modifiers.state();
                Ok(PreviewEventAction::None)
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor_position = Some(ScreenVec2::new(position.x as f32, position.y as f32));
                if self.stroke_active {
                    self.push_cursor_input();
                    return Ok(PreviewEventAction::RequestRedraw);
                }
                Ok(PreviewEventAction::None)
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => match state {
                ElementState::Pressed => {
                    if !self.stroke_active {
                        if let Some(runtime) = self.runtime.as_mut() {
                            runtime.begin_active_tool_stroke()?;
                            self.stroke_active = true;
                            self.push_cursor_input();
                        }
                    }
                    Ok(PreviewEventAction::RequestRedraw)
                }
                ElementState::Released => {
                    if self.stroke_active {
                        if let Some(runtime) = self.runtime.as_mut() {
                            runtime.end_active_tool_stroke_gpu(
                                &self.image_backend,
                                &mut self.tile_renderer,
                                &self.gpu.device,
                                &self.gpu.queue,
                                MAX_PENDING_BRUSH_INPUTS_PER_FRAME,
                            )?;
                        }
                        self.stroke_active = false;
                    }
                    Ok(PreviewEventAction::RequestRedraw)
                }
            },
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        state,
                        logical_key,
                        repeat,
                        ..
                    },
                ..
            } => {
                if state == ElementState::Pressed
                    && !repeat
                    && self.modifiers.control_key()
                    && let Key::Character(value) = logical_key
                    && value.eq_ignore_ascii_case("z")
                {
                    if let Some(runtime) = self.runtime.as_mut() {
                        runtime.undo_last_stroke_gpu(
                            &self.image_backend,
                            &mut self.tile_renderer,
                            &self.gpu.device,
                            &self.gpu.queue,
                        )?;
                    }
                    return Ok(PreviewEventAction::RequestRedraw);
                }
                Ok(PreviewEventAction::None)
            }
            WindowEvent::RedrawRequested => {
                self.redraw()?;
                Ok(PreviewEventAction::None)
            }
            _ => Ok(PreviewEventAction::None),
        }
    }

    fn redraw(&mut self) -> Result<(), PreviewRuntimeError> {
        let frame_started = Instant::now();
        let mut perf = PreviewFramePerf::default();
        let Some(runtime) = self.runtime.as_mut() else {
            return Ok(());
        };
        let process_inputs_started = Instant::now();
        runtime.process_pending_brush_input_gpu(
            &self.image_backend,
            &mut self.tile_renderer,
            &self.gpu.device,
            &self.gpu.queue,
            MAX_PENDING_BRUSH_INPUTS_PER_FRAME,
        )?;
        perf.process_inputs = process_inputs_started.elapsed();
        let dirty_tile_indices = runtime.frame_scheduler_mut().take_scheduled_tile_indices();
        perf.dirty_tile_count = dirty_tile_indices.len();
        if !dirty_tile_indices.is_empty() {
            let update_cache_started = Instant::now();
            let screen_cache_view = self
                .screen_cache
                .texture()
                .create_layer_view(0)
                .map_err(ScreenPresentCacheError::from)?;
            let screen_cache_target = RenderTarget2d {
                view: &screen_cache_view,
                format: self.screen_cache.texture().format,
                width: self.screen_cache.texture().width,
                height: self.screen_cache.texture().height,
            };
            if dirty_tile_indices.len() == self.full_tile_indices.len() {
                self.tile_renderer.clear_render_target(
                    &self.gpu.device,
                    &self.gpu.queue,
                    screen_cache_target,
                    DEFAULT_BACKGROUND_COLOR,
                );
            }
            present_root_tiles(
                runtime.session().doc(),
                runtime.session().doc_renderer(),
                &mut self.tile_renderer,
                &self.gpu.device,
                &self.gpu.queue,
                runtime.view(),
                screen_cache_target,
                &dirty_tile_indices,
            )?;
            perf.update_cache = update_cache_started.elapsed();
        }

        let acquire_frame_started = Instant::now();
        let frame = self.surface.acquire_frame().map_err(|error| {
            AppPresentError::DocRenderer(gla_doc_renderer::GlaDocRendererError::RenderExecution(
                gla_doc_renderer::RenderExecutionError::new(error.to_string()),
            ))
        })?;
        perf.acquire_frame = acquire_frame_started.elapsed();
        let result = {
            let target = RenderTarget2d {
                view: &frame.view,
                format: self.surface.format(),
                width: self.surface.width(),
                height: self.surface.height(),
            };
            let present_surface_started = Instant::now();
            let result = self.tile_renderer.present_texture_2d(
                &self.gpu.device,
                &self.gpu.queue,
                self.screen_cache.texture(),
                target,
            );
            perf.present_surface = present_surface_started.elapsed();
            result.map_err(AppPresentError::from)
        };

        match result {
            Ok(()) => {
                runtime.frame_scheduler_mut().reset_redraw_request();
                SurfaceRuntime::present(frame);
                self.trace_frame_perf(frame_started.elapsed(), &perf);
                Ok(())
            }
            Err(error) => {
                drop(frame);
                Err(error.into())
            }
        }
    }

    fn update_view(&mut self, width: u32, height: u32) -> Result<(), PreviewRuntimeError> {
        let Some(runtime) = self.runtime.as_mut() else {
            return Ok(());
        };
        let layout = runtime.session().doc().layout();
        *runtime.view_mut() = fitted_view(layout.size_x(), layout.size_y(), width, height)?;
        Ok(())
    }

    fn push_cursor_input(&mut self) {
        let Some(position) = self.cursor_position else {
            return;
        };
        let Some(runtime) = self.runtime.as_ref() else {
            return;
        };
        runtime.push_screen_input(
            self.elapsed_time_ns(),
            position,
            1.0,
            RadianVec2::new(0.0, 0.0),
            0.0,
        );
    }

    fn elapsed_time_ns(&self) -> u64 {
        let nanos = self.started_at.elapsed().as_nanos();
        nanos.min(u128::from(u64::MAX)) as u64
    }

    fn shutdown(&mut self) {
        self.stroke_active = false;
        if let Some(runtime) = self.runtime.take()
            && let Err(error) = runtime.shutdown()
        {
            eprintln!("preview shutdown failed: {error}");
        }
    }

    fn trace_frame_perf(&mut self, total: Duration, perf: &PreviewFramePerf) {
        if !self.perf_trace.enabled || total < self.perf_trace.slow_threshold {
            return;
        }
        let stages = [
            ("process_inputs", perf.process_inputs),
            ("update_cache", perf.update_cache),
            ("acquire_frame", perf.acquire_frame),
            ("present_surface", perf.present_surface),
        ];
        let Some((bottleneck, bottleneck_duration)) =
            stages.iter().max_by_key(|(_, duration)| *duration)
        else {
            return;
        };
        self.perf_frame_seq += 1;
        eprintln!(
            "[PERF][preview][frame={}] total_ms={:.3} bottleneck={} ({:.3}ms) dirty_tiles={} stages_ms={{process_inputs:{:.3}, update_cache:{:.3}, acquire_frame:{:.3}, present_surface:{:.3}}}",
            self.perf_frame_seq,
            duration_ms(total),
            bottleneck,
            duration_ms(*bottleneck_duration),
            perf.dirty_tile_count,
            duration_ms(perf.process_inputs),
            duration_ms(perf.update_cache),
            duration_ms(perf.acquire_frame),
            duration_ms(perf.present_surface),
        );
    }
}

fn initialize_default_canvas_white(
    doc: &mut GlaDoc,
    image_backend: &Backend,
    tile_renderer: &mut TileRenderer,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
) -> Result<(), PreviewInitError> {
    tile_renderer.ensure_backend(device, image_backend)?;

    let tile_count = doc.active_layer_image()?.tile_count();
    let white_tile = vec![255; (ATLAS_TILE_SIZE * ATLAS_TILE_SIZE * 4) as usize];
    for tile_index in 0..tile_count {
        let tile_owner = image_backend.alloc_active()?;
        let tile_key = tile_owner.tile_key();
        doc.active_layer_image_mut()?
            .replace_tile_owner(tile_index, tile_owner)
            .map_err(|error| match error {
                GlaImageTileAccessError::OutOfBounds => {
                    PreviewInitError::Document(GlaDocError::InvalidTileIndex {
                        tile_index,
                        tile_count,
                    })
                }
                GlaImageTileAccessError::WrongBackend { .. } => {
                    PreviewInitError::Atlas(atlas::AtlasError::WrongBackend)
                }
            })?;
        tile_renderer.upload_rgba8_tile(device, queue, image_backend, tile_key, &white_tile)?;
    }

    Ok(())
}

fn fitted_view(
    doc_width: u32,
    doc_height: u32,
    width: u32,
    height: u32,
) -> Result<AppView, AppViewMatrixError> {
    let doc_width = doc_width as f32;
    let doc_height = doc_height as f32;
    let scale = (width as f32 / doc_width)
        .min(height as f32 / doc_height)
        .max(0.01);
    let translate_x = (width as f32 - doc_width * scale) * 0.5;
    let translate_y = (height as f32 - doc_height * scale) * 0.5;
    AppView::from_scale_rotation_translation(scale, scale, 0.0, translate_x, translate_y)
}

fn env_flag(name: &str) -> bool {
    env::var(name)
        .ok()
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(false)
}

fn env_millis(name: &str) -> Option<u64> {
    env::var(name).ok()?.parse::<u64>().ok()
}

fn duration_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}
