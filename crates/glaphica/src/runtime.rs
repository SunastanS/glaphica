use std::error::Error;
use std::fmt::{Display, Formatter};
use std::sync::Arc;
use std::time::Instant;

use gla_core::{CanvasInput, ScreenCoordF};
use gla_ir::DrawOnToolKind;
use gla_renderer::{GpuRenderer, GpuRendererError, PresentTarget};
use gla_session::{DrawCommit, DrawHistory, DrawRecordId};
use winit::{
    application::ApplicationHandler,
    event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoop},
    keyboard::{Key, ModifiersState, NamedKey},
    window::{Window, WindowAttributes, WindowId},
};

use crate::{
    ActiveTool, AppView, AppViewMatrixError, BrushId, BrushSettings, DEFAULT_CANVAS_HEIGHT_PX,
    DEFAULT_CANVAS_WIDTH_PX, DocumentWorkspace, DocumentWorkspaceInitError, RoundBrushSettings,
    ScreenBlitter, ScreenPresentCache, SurfaceError, SurfaceFrame, SurfaceRuntime, ToolSet,
    frame::{AppFrameScheduler, ScreenUpdateRequest},
    stroke::BrushWorker,
};

#[derive(Debug, Clone)]
pub struct AppRuntimeConfig {
    pub window_title: String,
    pub clear_color: wgpu::Color,
    pub canvas_width_px: u32,
    pub canvas_height_px: u32,
    pub tool_set: ToolSet,
    pub active_tool: ActiveTool,
    pub draw_on_tools: Vec<DrawOnToolKind>,
    pub brush_settings: BrushSettings,
}

impl Default for AppRuntimeConfig {
    fn default() -> Self {
        Self {
            window_title: "Glaphica".to_owned(),
            clear_color: wgpu::Color {
                r: 0.08,
                g: 0.09,
                b: 0.10,
                a: 1.0,
            },
            canvas_width_px: DEFAULT_CANVAS_WIDTH_PX,
            canvas_height_px: DEFAULT_CANVAS_HEIGHT_PX,
            tool_set: ToolSet::default_brush(),
            active_tool: ActiveTool::Brush(BrushId::DEFAULT),
            draw_on_tools: vec![DrawOnToolKind::ReplaceCircle4D],
            brush_settings: BrushSettings::default(),
        }
    }
}

impl AppRuntimeConfig {
    pub fn with_round_brush_settings(mut self, settings: RoundBrushSettings) -> Self {
        self.brush_settings = BrushSettings::from_round_brush(settings);
        self
    }
}

#[derive(Debug)]
pub enum AppRunError {
    EventLoop(winit::error::EventLoopError),
}

impl Display for AppRunError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EventLoop(error) => write!(f, "app event loop failed: {error}"),
        }
    }
}

impl Error for AppRunError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::EventLoop(error) => Some(error),
        }
    }
}

pub fn run_app_window() -> Result<(), AppRunError> {
    run_app_window_with_config(AppRuntimeConfig::default())
}

pub fn run_app_window_with_config(config: AppRuntimeConfig) -> Result<(), AppRunError> {
    let event_loop = EventLoop::new().map_err(AppRunError::EventLoop)?;
    let mut app = App::new(config);
    event_loop.run_app(&mut app).map_err(AppRunError::EventLoop)
}

struct App {
    config: AppRuntimeConfig,
    workspace: Option<DocumentWorkspace>,
    view: AppView,
    history: DrawHistory,
    undo_stack: Vec<DrawRecordId>,
    redo_stack: Vec<DrawRecordId>,
    frame_scheduler: AppFrameScheduler,
    brush_worker: BrushWorker,
    primary_down: bool,
    middle_down: bool,
    middle_pan_last_pos: Option<ScreenCoordF>,
    last_cursor_pos: Option<ScreenCoordF>,
    input_clock_start: Instant,
    last_input_time_ns: u64,
    modifiers: ModifiersState,
    window: Option<Arc<Window>>,
    gpu: Option<GpuCtx>,
}

impl App {
    fn new(config: AppRuntimeConfig) -> Self {
        let brush_worker = BrushWorker::new(
            config.tool_set.clone(),
            config.active_tool,
            config.brush_settings,
        );
        Self {
            config,
            workspace: None,
            view: AppView::identity(),
            history: DrawHistory::new(),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            frame_scheduler: AppFrameScheduler::new(),
            brush_worker,
            primary_down: false,
            middle_down: false,
            middle_pan_last_pos: None,
            last_cursor_pos: None,
            input_clock_start: Instant::now(),
            last_input_time_ns: 0,
            modifiers: ModifiersState::default(),
            window: None,
            gpu: None,
        }
    }

    fn window_attributes(&self) -> WindowAttributes {
        WindowAttributes::default().with_title(self.config.window_title.clone())
    }

    fn request_redraw(&mut self) {
        self.frame_scheduler.request_redraw();
    }

    fn request_full_screen_update(&mut self) {
        self.frame_scheduler.schedule_full_update();
    }

    fn begin_stroke_at_last_cursor(&mut self) {
        let Some(position) = self.last_cursor_pos else {
            return;
        };
        self.begin_stroke_at(position);
    }

    fn begin_stroke_at(&mut self, position: ScreenCoordF) {
        if !self.brush_worker.begin_active_stroke() {
            return;
        }
        self.push_stroke_input(position);
    }

    fn continue_stroke_at(&mut self, position: ScreenCoordF) {
        if self.brush_worker.has_active_stroke() {
            self.push_stroke_input(position);
        } else {
            self.begin_stroke_at(position);
        }
    }

    fn push_stroke_input(&mut self, position: ScreenCoordF) {
        let input = self.stroke_input_from_screen(position);
        self.brush_worker.push_canvas_input(input);
    }

    fn active_brush_id(&self) -> Option<BrushId> {
        self.brush_worker.active_brush_id()
    }

    fn stroke_input_from_screen(&mut self, position: ScreenCoordF) -> CanvasInput {
        let canvas = self.view.screen_to_document_point(position);
        CanvasInput {
            time_ns: self.next_input_time_ns(),
            position: canvas,
            pressure: 1.0,
            tilt: (0.0, 0.0),
            twist: 0.0,
        }
    }

    fn next_input_time_ns(&mut self) -> u64 {
        let elapsed_ns = self.input_clock_start.elapsed().as_nanos();
        let elapsed_ns = u64::try_from(elapsed_ns).unwrap_or(u64::MAX);
        let time_ns = elapsed_ns.max(self.last_input_time_ns.saturating_add(1));
        self.last_input_time_ns = time_ns;
        time_ns
    }

    fn commit_active_stroke(&mut self) -> bool {
        let Some(stroke) = self.brush_worker.finish_active_stroke() else {
            return false;
        };
        let (Some(workspace), Some(gpu)) = (self.workspace.as_mut(), self.gpu.as_mut()) else {
            self.brush_worker.restore_active_stroke(stroke);
            return false;
        };
        let samples = stroke.replace_circle_samples();

        let commit = match workspace.replace_circle_stroke_on_root(
            &mut self.history,
            gpu.renderer_mut(),
            samples.iter().copied(),
        ) {
            Ok(Some(commit)) => commit,
            Ok(None) => return false,
            Err(error) => {
                eprintln!("stroke failed: {error}");
                self.brush_worker.restore_active_stroke(stroke);
                return false;
            }
        };
        let dirty_tiles = workspace.root_dirty_tile_indices(&commit);
        self.undo_stack.push(commit.record_id);
        self.redo_stack.clear();
        self.frame_scheduler.schedule_tile_indices(&dirty_tiles);
        true
    }

    fn cancel_active_stroke(&mut self) -> bool {
        self.brush_worker.cancel_active_stroke()
    }

    fn undo(&mut self) -> bool {
        let Some(record_id) = self.undo_stack.pop() else {
            return false;
        };
        match self.apply_history_record(record_id) {
            Some(redo_commit) => {
                self.redo_stack.push(redo_commit.record_id);
                self.schedule_commit_dirty(&redo_commit);
                true
            }
            None => {
                self.undo_stack.push(record_id);
                false
            }
        }
    }

    fn redo(&mut self) -> bool {
        let Some(record_id) = self.redo_stack.pop() else {
            return false;
        };
        match self.apply_history_record(record_id) {
            Some(undo_commit) => {
                self.undo_stack.push(undo_commit.record_id);
                self.schedule_commit_dirty(&undo_commit);
                true
            }
            None => {
                self.redo_stack.push(record_id);
                false
            }
        }
    }

    fn apply_history_record(&mut self, record_id: DrawRecordId) -> Option<DrawCommit> {
        let (Some(workspace), Some(gpu)) = (self.workspace.as_mut(), self.gpu.as_mut()) else {
            return None;
        };
        match workspace.apply_draw_record(&mut self.history, gpu.renderer_mut(), record_id) {
            Ok(commit) => Some(commit),
            Err(error) => {
                eprintln!("history apply failed: {error}");
                None
            }
        }
    }

    fn schedule_commit_dirty(&mut self, commit: &DrawCommit) {
        let Some(workspace) = self.workspace.as_ref() else {
            return;
        };
        let dirty_tiles = workspace.root_dirty_tile_indices(commit);
        self.frame_scheduler.schedule_tile_indices(&dirty_tiles);
    }

    fn fit_view_to_workspace(&mut self) -> Result<(), AppViewMatrixError> {
        let (Some(workspace), Some(gpu)) = (self.workspace.as_ref(), self.gpu.as_ref()) else {
            return Ok(());
        };
        self.view =
            AppView::fit_canvas_in_surface(workspace.canvas_size_px(), gpu.surface_size_px())?;
        Ok(())
    }

    fn pan_view_to(&mut self, position: ScreenCoordF) {
        let Some(last_position) = self.middle_pan_last_pos else {
            self.middle_pan_last_pos = Some(position);
            return;
        };
        let dx = position.x - last_position.x;
        let dy = position.y - last_position.y;
        self.middle_pan_last_pos = Some(position);
        if dx.abs() <= f32::EPSILON && dy.abs() <= f32::EPSILON {
            return;
        }
        if let Err(error) = self.view.translate_screen(dx, dy) {
            eprintln!("pan failed: {error}");
        }
    }

    fn zoom_view_at_cursor(&mut self, delta: &MouseScrollDelta) -> bool {
        let scroll_lines = scroll_delta_lines(delta);
        if scroll_lines.abs() <= f32::EPSILON {
            return false;
        }
        let center = self.last_cursor_pos.unwrap_or_else(|| {
            let (width, height) = self
                .gpu
                .as_ref()
                .map(GpuCtx::surface_size_px)
                .unwrap_or((1, 1));
            ScreenCoordF::new(width as f32 * 0.5, height as f32 * 0.5)
        });
        let scale = (scroll_lines * 0.12).exp().clamp(0.05, 20.0);
        if let Err(error) = self.view.scale_about_screen_point(scale, center) {
            eprintln!("zoom failed: {error}");
            return false;
        }
        true
    }
}

struct GpuCtx {
    surface: SurfaceRuntime,
    device: wgpu::Device,
    queue: wgpu::Queue,
    clear_color: wgpu::Color,
    renderer: GpuRenderer,
    screen_cache: ScreenPresentCache,
    screen_blitter: ScreenBlitter,
}

#[derive(Debug)]
enum GpuInitError {
    CreateSurface(wgpu::CreateSurfaceError),
    Document(DocumentWorkspaceInitError<GpuRendererError>),
    Renderer(GpuRendererError),
    RequestAdapter(wgpu::RequestAdapterError),
    RequestDevice(wgpu::RequestDeviceError),
    Surface(SurfaceError),
}

impl Display for GpuInitError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CreateSurface(error) => write!(f, "failed to create surface: {error}"),
            Self::Document(error) => {
                write!(f, "failed to create GPU-backed document workspace: {error}")
            }
            Self::Renderer(error) => write!(f, "failed to create GPU renderer: {error}"),
            Self::RequestAdapter(error) => write!(f, "failed to request adapter: {error}"),
            Self::RequestDevice(error) => write!(f, "failed to request device: {error}"),
            Self::Surface(error) => write!(f, "surface initialization failed: {error}"),
        }
    }
}

impl Error for GpuInitError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::CreateSurface(error) => Some(error),
            Self::Document(error) => Some(error),
            Self::Renderer(error) => Some(error),
            Self::RequestAdapter(error) => Some(error),
            Self::RequestDevice(error) => Some(error),
            Self::Surface(error) => Some(error),
        }
    }
}

impl GpuCtx {
    async fn new(
        window: Arc<Window>,
        app_config: &AppRuntimeConfig,
    ) -> Result<(Self, DocumentWorkspace), GpuInitError> {
        let size = window.inner_size();
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        let surface = instance
            .create_surface(window)
            .map_err(GpuInitError::CreateSurface)?;

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                compatible_surface: Some(&surface),
                ..Default::default()
            })
            .await
            .map_err(GpuInitError::RequestAdapter)?;

        let required_features = required_draw_on_features(&app_config.draw_on_tools);
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("glaphica-device"),
                required_features,
                required_limits: wgpu::Limits::default(),
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
                memory_hints: wgpu::MemoryHints::default(),
                trace: wgpu::Trace::default(),
            })
            .await
            .map_err(GpuInitError::RequestDevice)?;

        let surface = SurfaceRuntime::new(surface, &adapter, &device, size.width, size.height)
            .map_err(GpuInitError::Surface)?;
        let screen_cache =
            ScreenPresentCache::new(&device, surface.format(), surface.width(), surface.height());
        let screen_blitter = ScreenBlitter::new(&device);

        let mut renderer = GpuRenderer::with_draw_on_tools(
            &adapter,
            device.clone(),
            queue.clone(),
            app_config.draw_on_tools.iter().copied(),
        )
        .map_err(GpuInitError::Renderer)?;
        let workspace = DocumentWorkspace::white_with_textures(
            app_config.canvas_width_px,
            app_config.canvas_height_px,
            &mut renderer,
        )
        .map_err(GpuInitError::Document)?;

        Ok((
            Self {
                surface,
                device,
                queue,
                clear_color: app_config.clear_color,
                renderer,
                screen_cache,
                screen_blitter,
            },
            workspace,
        ))
    }

    fn renderer_mut(&mut self) -> &mut GpuRenderer {
        &mut self.renderer
    }

    fn surface_size_px(&self) -> (u32, u32) {
        self.surface.size_px()
    }

    fn resize(&mut self, width: u32, height: u32) {
        self.surface.resize(&self.device, width, height);
        self.screen_cache.resize(
            &self.device,
            self.surface.format(),
            self.surface.width(),
            self.surface.height(),
        );
        self.screen_cache.invalidate();
    }

    fn render(
        &mut self,
        workspace: Option<&DocumentWorkspace>,
        view: &AppView,
        update_request: ScreenUpdateRequest,
    ) {
        let cache_ready = self.update_screen_cache(workspace, view, update_request);
        let frame = match self.surface.acquire_frame(&self.device) {
            Ok(frame) => frame,
            Err(error) => {
                eprintln!("surface acquire failed: {error}");
                return;
            }
        };
        if cache_ready {
            self.screen_blitter.present_cache(
                &self.device,
                &self.queue,
                &self.screen_cache,
                &frame,
                self.surface.format(),
            );
        } else {
            self.render_direct_to_frame(workspace, view, &frame);
        }
        SurfaceRuntime::present(frame);
    }

    fn update_screen_cache(
        &mut self,
        workspace: Option<&DocumentWorkspace>,
        view: &AppView,
        update_request: ScreenUpdateRequest,
    ) -> bool {
        let full_update =
            matches!(update_request, ScreenUpdateRequest::Full) || !self.screen_cache.is_valid();

        if full_update {
            return self.update_screen_cache_full(workspace, view);
        }

        let ScreenUpdateRequest::Tiles(tile_indices) = update_request else {
            return self.screen_cache.is_valid();
        };
        if tile_indices.is_empty() {
            return self.screen_cache.is_valid();
        }
        let Some(workspace) = workspace else {
            return self.update_screen_cache_full(None, view);
        };

        let tiles = match workspace.root_present_tiles_for_view_tile_indices(view, &tile_indices) {
            Ok(tiles) if tiles.len() == tile_indices.len() => tiles,
            Ok(_) => {
                return self.update_screen_cache_full(Some(workspace), view);
            }
            Err(error) => {
                eprintln!("document present cache update failed: {error}");
                self.screen_cache.invalidate();
                return false;
            }
        };
        match self.renderer.present_tiles_incremental(
            &tiles,
            PresentTarget {
                view: self.screen_cache.view(),
                format: self.screen_cache.format(),
                width: self.screen_cache.width(),
                height: self.screen_cache.height(),
                clear_color: self.clear_color,
            },
        ) {
            Ok(()) => true,
            Err(error) => {
                eprintln!("screen present cache update failed: {error}");
                self.screen_cache.invalidate();
                false
            }
        }
    }

    fn update_screen_cache_full(
        &mut self,
        workspace: Option<&DocumentWorkspace>,
        view: &AppView,
    ) -> bool {
        let tiles = match workspace {
            Some(workspace) => match workspace.root_present_tiles_for_view(view) {
                Ok(tiles) => tiles,
                Err(error) => {
                    eprintln!("document present failed: {error}");
                    Vec::new()
                }
            },
            None => Vec::new(),
        };
        if let Err(error) = self.renderer.present_tiles(
            &tiles,
            PresentTarget {
                view: self.screen_cache.view(),
                format: self.screen_cache.format(),
                width: self.screen_cache.width(),
                height: self.screen_cache.height(),
                clear_color: self.clear_color,
            },
        ) {
            eprintln!("screen present cache rebuild failed: {error}");
            self.screen_cache.invalidate();
            return false;
        }
        self.screen_cache.mark_valid();
        true
    }

    fn render_direct_to_frame(
        &mut self,
        workspace: Option<&DocumentWorkspace>,
        view: &AppView,
        frame: &SurfaceFrame,
    ) {
        let tiles = match workspace {
            Some(workspace) => match workspace.root_present_tiles_for_view(view) {
                Ok(tiles) => tiles,
                Err(error) => {
                    eprintln!("document direct present failed: {error}");
                    Vec::new()
                }
            },
            None => Vec::new(),
        };
        let target = PresentTarget {
            view: &frame.view,
            format: self.surface.format(),
            width: self.surface.width(),
            height: self.surface.height(),
            clear_color: self.clear_color,
        };
        if let Err(error) = self.renderer.present_tiles(&tiles, target) {
            eprintln!("surface direct present failed: {error}");
            if let Err(clear_error) = self.renderer.present_tiles(&[], target) {
                eprintln!("surface clear failed: {clear_error}");
            }
        }
    }
}

fn required_draw_on_features(tools: &[DrawOnToolKind]) -> wgpu::Features {
    if tools.contains(&DrawOnToolKind::RadialKernel1D) {
        wgpu::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES
    } else {
        wgpu::Features::empty()
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }

        let window = match event_loop.create_window(self.window_attributes()) {
            Ok(window) => Arc::new(window),
            Err(error) => {
                eprintln!("window creation failed: {error}");
                event_loop.exit();
                return;
            }
        };

        let (gpu, workspace) = match pollster::block_on(GpuCtx::new(window.clone(), &self.config)) {
            Ok(parts) => parts,
            Err(error) => {
                eprintln!("gpu initialization failed: {error}");
                event_loop.exit();
                return;
            }
        };

        self.window = Some(window);
        self.workspace = Some(workspace);
        self.gpu = Some(gpu);
        if let Err(error) = self.fit_view_to_workspace() {
            eprintln!("view initialization failed: {error}");
            event_loop.exit();
            return;
        }
        self.request_full_screen_update();
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(window) = self.window.as_ref().cloned() else {
            return;
        };

        if window.id() != window_id {
            return;
        }

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::ModifiersChanged(modifiers) => {
                self.modifiers = modifiers.state();
            }
            WindowEvent::CursorMoved { position, .. } => {
                let pos =
                    ScreenCoordF::new(finite_f64_to_f32(position.x), finite_f64_to_f32(position.y));
                self.last_cursor_pos = Some(pos);
                if self.primary_down {
                    self.continue_stroke_at(pos);
                }
                if self.middle_down {
                    self.pan_view_to(pos);
                    self.request_full_screen_update();
                }
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => {
                let was_primary_down = self.primary_down;
                self.primary_down = state == ElementState::Pressed;
                if self.primary_down && !was_primary_down {
                    self.begin_stroke_at_last_cursor();
                } else if was_primary_down && self.commit_active_stroke() {
                    self.request_redraw();
                }
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Middle,
                ..
            } => {
                self.middle_down = state == ElementState::Pressed;
                self.middle_pan_last_pos = if self.middle_down {
                    self.last_cursor_pos
                } else {
                    None
                };
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if self.zoom_view_at_cursor(&delta) {
                    self.request_full_screen_update();
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state == ElementState::Pressed && !event.repeat {
                    match &event.logical_key {
                        Key::Named(NamedKey::Escape) => {
                            if self.cancel_active_stroke() {
                                self.primary_down = false;
                            }
                        }
                        Key::Character(value) => {
                            if self.modifiers.control_key()
                                && self.modifiers.shift_key()
                                && value.eq_ignore_ascii_case("z")
                            {
                                if self.redo() {
                                    self.request_redraw();
                                }
                            } else if self.modifiers.control_key()
                                && value.eq_ignore_ascii_case("z")
                            {
                                if self.undo() {
                                    self.request_redraw();
                                }
                            } else if self.modifiers.control_key()
                                && value.eq_ignore_ascii_case("y")
                            {
                                if self.redo() {
                                    self.request_redraw();
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            WindowEvent::Resized(size) => {
                if let Some(gpu) = self.gpu.as_mut() {
                    gpu.resize(size.width, size.height);
                }
                if let Err(error) = self.fit_view_to_workspace() {
                    eprintln!("view resize failed: {error}");
                }
                self.request_full_screen_update();
            }
            WindowEvent::RedrawRequested => {
                let update_request = self.frame_scheduler.take_screen_update_request();
                if let Some(gpu) = self.gpu.as_mut() {
                    gpu.render(self.workspace.as_ref(), &self.view, update_request);
                }
                self.frame_scheduler.reset_redraw_request();
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if self.frame_scheduler.has_requested_redraw()
            && let Some(window) = self.window.as_ref()
        {
            window.request_redraw();
        }
    }
}

fn finite_f64_to_f32(value: f64) -> f32 {
    if value.is_finite() { value as f32 } else { 0.0 }
}

fn scroll_delta_lines(delta: &MouseScrollDelta) -> f32 {
    match delta {
        MouseScrollDelta::LineDelta(_, y) => *y,
        MouseScrollDelta::PixelDelta(position) => finite_f64_to_f32(position.y) / 40.0,
    }
}

#[cfg(test)]
mod tests {
    use super::{App, AppRuntimeConfig};
    use crate::{
        ActiveTool, AppView, BrushId, BrushSettings, DEFAULT_CANVAS_HEIGHT_PX,
        DEFAULT_CANVAS_WIDTH_PX, RoundBrushSettings, Tool,
    };
    use gla_core::{CanvasCoordF, ScreenCoordF};
    use gla_ir::DrawOnToolKind;

    #[test]
    fn default_runtime_config_keeps_window_contract_stable() {
        let config = AppRuntimeConfig::default();

        assert_eq!(config.window_title, "Glaphica");
        assert_eq!(config.clear_color.r, 0.08);
        assert_eq!(config.clear_color.g, 0.09);
        assert_eq!(config.clear_color.b, 0.10);
        assert_eq!(config.clear_color.a, 1.0);
        assert_eq!(config.canvas_width_px, DEFAULT_CANVAS_WIDTH_PX);
        assert_eq!(config.canvas_height_px, DEFAULT_CANVAS_HEIGHT_PX);
        assert_eq!(config.tool_set.tools(), &[Tool::Brush(BrushId::DEFAULT)]);
        assert_eq!(config.active_tool, ActiveTool::Brush(BrushId::DEFAULT));
        assert_eq!(config.draw_on_tools, vec![DrawOnToolKind::ReplaceCircle4D]);
        assert_eq!(config.brush_settings, BrushSettings::default());
    }

    #[test]
    fn active_brush_requires_registered_tool() {
        let mut config = AppRuntimeConfig::default();
        config.active_tool = ActiveTool::Brush(BrushId::new(99));
        let app = App::new(config);

        assert_eq!(app.active_brush_id(), None);
    }

    #[test]
    fn runtime_config_accepts_round_brush_settings() {
        let config = AppRuntimeConfig::default().with_round_brush_settings(RoundBrushSettings {
            base_radius_px: 18.0,
            spacing_ratio: 0.25,
            base_hardness: 0.4,
            base_flow: 0.8,
            base_opacity: 0.6,
            tint: [0.2, 0.3, 0.4],
        });

        assert_eq!(config.brush_settings.radius_px, 18.0);
        assert_eq!(config.brush_settings.spacing_ratio, 0.25);
        assert_eq!(config.brush_settings.hardness, 0.4);
        assert_eq!(config.brush_settings.flow, 0.8);
        assert_eq!(config.brush_settings.opacity, 0.6);
        assert_eq!(
            config.brush_settings.color,
            gla_color::PremultipliedRgbaF32::new(0.2, 0.3, 0.4, 1.0)
        );
    }

    #[test]
    fn active_stroke_records_canvas_inputs_for_current_view() {
        let mut app = App::new(AppRuntimeConfig::default());
        app.view = AppView::new([2.0, 0.0, 0.0, 2.0, 10.0, 20.0]).unwrap();

        app.begin_stroke_at(ScreenCoordF::new(12.0, 24.0));
        app.continue_stroke_at(ScreenCoordF::new(14.0, 28.0));

        let stroke = app.brush_worker.active_stroke().unwrap();
        assert_eq!(stroke.brush_id(), BrushId::DEFAULT);
        assert_eq!(stroke.inputs().len(), 2);
        assert_eq!(stroke.inputs()[0].position, CanvasCoordF::new(1.0, 2.0));
        assert_eq!(stroke.inputs()[1].position, CanvasCoordF::new(2.0, 4.0));
        assert_eq!(stroke.inputs()[0].pressure, 1.0);
        assert!(stroke.inputs()[1].time_ns > stroke.inputs()[0].time_ns);

        let samples = app
            .brush_worker
            .finish_active_stroke()
            .unwrap()
            .replace_circle_samples();
        assert_eq!(samples[0].center, CanvasCoordF::new(1.0, 2.0));
        assert_eq!(samples[1].center, CanvasCoordF::new(2.0, 4.0));
        assert_eq!(samples[0].radius_px, BrushSettings::default().radius_px);
    }

    #[test]
    fn active_stroke_snapshots_brush_settings_at_begin() {
        let mut config = AppRuntimeConfig::default();
        config.brush_settings.radius_px = 7.0;
        config.brush_settings.color = gla_color::PremultipliedRgbaF32::new(0.1, 0.2, 0.3, 1.0);
        let mut app = App::new(config);

        app.begin_stroke_at(ScreenCoordF::new(10.0, 20.0));
        app.config.brush_settings.radius_px = 99.0;
        app.config.brush_settings.color = gla_color::PremultipliedRgbaF32::new(0.8, 0.7, 0.6, 1.0);
        app.continue_stroke_at(ScreenCoordF::new(11.0, 21.0));

        let samples = app
            .brush_worker
            .finish_active_stroke()
            .unwrap()
            .replace_circle_samples();
        assert_eq!(samples.len(), 2);
        assert!(samples.iter().all(|sample| sample.radius_px == 7.0));
        assert!(
            samples
                .iter()
                .all(|sample| sample.color
                    == gla_color::PremultipliedRgbaF32::new(0.1, 0.2, 0.3, 1.0))
        );
    }

    #[test]
    fn cancel_active_stroke_drops_transaction() {
        let mut app = App::new(AppRuntimeConfig::default());

        app.begin_stroke_at(ScreenCoordF::new(10.0, 20.0));

        assert!(app.brush_worker.has_active_stroke());
        assert!(app.cancel_active_stroke());
        assert!(!app.brush_worker.has_active_stroke());
        assert!(!app.cancel_active_stroke());
    }

    #[test]
    fn non_finite_cursor_coordinate_maps_to_zero() {
        assert_eq!(super::finite_f64_to_f32(f64::NAN), 0.0);
        assert_eq!(super::finite_f64_to_f32(f64::INFINITY), 0.0);
        assert_eq!(super::finite_f64_to_f32(42.25), 42.25);
    }

    #[test]
    fn scroll_delta_pixels_are_normalized_to_line_units() {
        assert_eq!(
            super::scroll_delta_lines(&winit::event::MouseScrollDelta::LineDelta(0.0, 2.0)),
            2.0
        );
        assert_eq!(
            super::scroll_delta_lines(&winit::event::MouseScrollDelta::PixelDelta(
                winit::dpi::PhysicalPosition::new(0.0, 80.0)
            )),
            2.0
        );
    }
}
