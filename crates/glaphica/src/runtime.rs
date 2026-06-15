use std::error::Error;
use std::fmt::{Display, Formatter};
use std::sync::Arc;
use std::time::Instant;

use gla_core::{CanvasInput, ScreenCoordF};
use gla_ir::{DrawOnToolKind, RegistryPatch};
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
    script::{ScriptCommand, ScriptCommandOutcome, ScriptHost, ScriptHostError},
    stroke::BrushThreadRuntime,
    trace::{AppTraceConfig, AppTraceError, AppTraceEvent, AppTraceState},
};

const BRUSH_THREAD_COMMAND_CAPACITY: usize = 1024;

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
    pub trace_config: AppTraceConfig,
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
            trace_config: AppTraceConfig::default(),
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
    Trace(AppTraceError),
}

impl Display for AppRunError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EventLoop(error) => write!(f, "app event loop failed: {error}"),
            Self::Trace(error) => write!(f, "app trace setup failed: {error}"),
        }
    }
}

impl Error for AppRunError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::EventLoop(error) => Some(error),
            Self::Trace(error) => Some(error),
        }
    }
}

pub fn run_app_window() -> Result<(), AppRunError> {
    run_app_window_with_config(AppRuntimeConfig::default())
}

pub fn run_app_window_with_config(config: AppRuntimeConfig) -> Result<(), AppRunError> {
    let event_loop = EventLoop::new().map_err(AppRunError::EventLoop)?;
    let mut app = App::try_new(config).map_err(AppRunError::Trace)?;
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
    brush_thread: BrushThreadRuntime,
    trace: AppTraceState,
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
        Self::try_new(config).expect("default app trace configuration should initialize")
    }

    fn try_new(config: AppRuntimeConfig) -> Result<Self, AppTraceError> {
        let trace = AppTraceState::from_config(&config.trace_config)?;
        let brush_thread = BrushThreadRuntime::spawn(
            config.tool_set.clone(),
            config.active_tool,
            config.brush_settings,
            BRUSH_THREAD_COMMAND_CAPACITY,
        );
        Ok(Self {
            config,
            workspace: None,
            view: AppView::identity(),
            history: DrawHistory::new(),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            frame_scheduler: AppFrameScheduler::new(),
            brush_thread,
            trace,
            primary_down: false,
            middle_down: false,
            middle_pan_last_pos: None,
            last_cursor_pos: None,
            input_clock_start: Instant::now(),
            last_input_time_ns: 0,
            modifiers: ModifiersState::default(),
            window: None,
            gpu: None,
        })
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
        let input = self.stroke_input_from_screen(position);
        if self.begin_stroke_with_canvas_input(input).is_ok() {
            self.trace.record(AppTraceEvent::BeginStroke(input.into()));
        }
    }

    fn continue_stroke_at(&mut self, position: ScreenCoordF) {
        if self.brush_thread.has_active_stroke() {
            self.push_stroke_input(position);
        } else {
            self.begin_stroke_at(position);
        }
    }

    fn push_stroke_input(&mut self, position: ScreenCoordF) {
        let input = self.stroke_input_from_screen(position);
        if self.push_canvas_input_from_script(input).is_ok() {
            self.trace.record(AppTraceEvent::StrokeSample(input.into()));
        }
    }

    fn begin_stroke_with_canvas_input(
        &mut self,
        input: CanvasInput,
    ) -> Result<(), ScriptHostError> {
        if self.active_brush_id().is_none() {
            return Err(ScriptHostError::InvalidCommand {
                reason: "active tool is not a registered brush".to_owned(),
            });
        }
        if !self.brush_thread.begin_active_stroke() {
            return Err(ScriptHostError::Runtime {
                reason: "brush thread did not start a stroke".to_owned(),
            });
        }
        self.brush_thread.push_canvas_input(input);
        Ok(())
    }

    fn push_canvas_input_from_script(&mut self, input: CanvasInput) -> Result<(), ScriptHostError> {
        if !self.brush_thread.has_active_stroke() {
            return Err(ScriptHostError::InvalidCommand {
                reason: "cannot push stroke input before beginning a stroke".to_owned(),
            });
        }
        self.brush_thread.push_canvas_input(input);
        Ok(())
    }

    fn set_active_tool_from_script(
        &mut self,
        active_tool: ActiveTool,
    ) -> Result<(), ScriptHostError> {
        if !self.config.tool_set.contains(active_tool.as_tool()) {
            return Err(ScriptHostError::InvalidCommand {
                reason: format!("active tool {active_tool:?} is not registered"),
            });
        }
        if !self.brush_thread.set_active_tool(active_tool) {
            return Err(ScriptHostError::Runtime {
                reason: "brush thread did not accept active tool update".to_owned(),
            });
        }
        self.config.active_tool = active_tool;
        self.primary_down = false;
        Ok(())
    }

    fn set_round_brush_settings_from_script(&mut self, settings: RoundBrushSettings) {
        let brush_settings = BrushSettings::from_round_brush(settings);
        self.config.brush_settings = brush_settings;
        self.brush_thread.update_brush_settings(brush_settings);
    }

    fn apply_registry_patch_from_script(
        &mut self,
        patch: RegistryPatch,
    ) -> Result<ScriptCommandOutcome, ScriptHostError> {
        if self.brush_thread.has_active_stroke() {
            return Err(ScriptHostError::InvalidCommand {
                reason: "cannot apply a registry patch while a stroke is active".to_owned(),
            });
        }
        let Some(workspace) = self.workspace.as_mut() else {
            return Err(ScriptHostError::InvalidCommand {
                reason: "cannot apply a registry patch before the document workspace exists"
                    .to_owned(),
            });
        };
        let version =
            workspace
                .apply_registry_patch(patch)
                .map_err(|error| ScriptHostError::Runtime {
                    reason: error.to_string(),
                })?;
        self.undo_stack.clear();
        self.redo_stack.clear();
        if let Err(error) = self.fit_view_to_workspace() {
            return Err(ScriptHostError::Runtime {
                reason: error.to_string(),
            });
        }
        self.request_full_screen_update();
        Ok(ScriptCommandOutcome::DocumentVersion(version))
    }

    fn active_brush_id(&self) -> Option<BrushId> {
        self.brush_thread.active_brush_id()
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
        self.commit_active_stroke_dirty_tiles().is_some()
    }

    fn finish_stroke_from_window(&mut self) -> bool {
        let committed = self.commit_active_stroke();
        self.trace.record(AppTraceEvent::FinishStroke);
        committed
    }

    fn commit_active_stroke_dirty_tiles(&mut self) -> Option<Vec<u32>> {
        let Some(stroke) = self.brush_thread.finish_active_stroke() else {
            return None;
        };
        let (Some(workspace), Some(gpu)) = (self.workspace.as_mut(), self.gpu.as_mut()) else {
            self.brush_thread.restore_active_stroke(stroke);
            return None;
        };
        let samples = stroke.replace_circle_samples();

        let commit = match workspace.replace_circle_stroke_on_root(
            &mut self.history,
            gpu.renderer_mut(),
            samples.iter().copied(),
        ) {
            Ok(Some(commit)) => commit,
            Ok(None) => return None,
            Err(error) => {
                eprintln!("stroke failed: {error}");
                self.brush_thread.restore_active_stroke(stroke);
                return None;
            }
        };
        let dirty_tiles = workspace.root_dirty_tile_indices(&commit);
        self.undo_stack.push(commit.record_id);
        self.redo_stack.clear();
        self.frame_scheduler.schedule_tile_indices(&dirty_tiles);
        Some(dirty_tiles)
    }

    fn cancel_active_stroke(&mut self) -> bool {
        self.brush_thread.cancel_active_stroke()
    }

    fn cancel_stroke_from_window(&mut self) -> bool {
        let canceled = self.cancel_active_stroke();
        if canceled {
            self.trace.record(AppTraceEvent::CancelStroke);
        }
        canceled
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

    fn undo_from_window(&mut self) -> bool {
        let undone = self.undo();
        if undone {
            self.trace.record(AppTraceEvent::Undo);
        }
        undone
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

    fn redo_from_window(&mut self) -> bool {
        let redone = self.redo();
        if redone {
            self.trace.record(AppTraceEvent::Redo);
        }
        redone
    }

    fn process_next_trace_replay_event(&mut self) -> bool {
        let Some(event) = self.trace.next_replay_event() else {
            return false;
        };
        if let Err(error) = self.execute_trace_replay_event(event) {
            eprintln!("trace replay event failed: {error}");
        }
        true
    }

    fn execute_trace_replay_event(&mut self, event: AppTraceEvent) -> Result<(), ScriptHostError> {
        match event {
            AppTraceEvent::BeginStroke(input) => {
                self.execute_script_command(ScriptCommand::BeginStroke(input.into()))?;
            }
            AppTraceEvent::StrokeSample(input) => {
                self.execute_script_command(ScriptCommand::PushStrokeInput(input.into()))?;
            }
            AppTraceEvent::FinishStroke => {
                self.execute_script_command(ScriptCommand::FinishStroke)?;
            }
            AppTraceEvent::CancelStroke => {
                self.execute_script_command(ScriptCommand::CancelStroke)?;
            }
            AppTraceEvent::Undo => {
                self.execute_script_command(ScriptCommand::Undo)?;
            }
            AppTraceEvent::Redo => {
                self.execute_script_command(ScriptCommand::Redo)?;
            }
        }
        Ok(())
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

impl Drop for App {
    fn drop(&mut self) {
        if let Err(error) = self.trace.stop_recording() {
            eprintln!("trace save failed: {error}");
        }
    }
}

impl ScriptHost for App {
    fn execute_script_command(
        &mut self,
        command: ScriptCommand,
    ) -> Result<ScriptCommandOutcome, ScriptHostError> {
        match command {
            ScriptCommand::ApplyRegistryPatch(patch) => {
                self.apply_registry_patch_from_script(patch)
            }
            ScriptCommand::RunDrawSession(_) => Err(ScriptHostError::UnsupportedCommand {
                command: "RunDrawSession",
            }),
            ScriptCommand::SetActiveTool(active_tool) => {
                self.set_active_tool_from_script(active_tool)?;
                Ok(ScriptCommandOutcome::None)
            }
            ScriptCommand::SetRoundBrushSettings(settings) => {
                self.set_round_brush_settings_from_script(settings);
                Ok(ScriptCommandOutcome::None)
            }
            ScriptCommand::BeginStroke(input) => {
                self.begin_stroke_with_canvas_input(input)?;
                Ok(ScriptCommandOutcome::None)
            }
            ScriptCommand::PushStrokeInput(input) => {
                self.push_canvas_input_from_script(input)?;
                Ok(ScriptCommandOutcome::None)
            }
            ScriptCommand::FinishStroke => {
                let Some(dirty_tiles) = self.commit_active_stroke_dirty_tiles() else {
                    return Ok(ScriptCommandOutcome::None);
                };
                self.request_redraw();
                Ok(ScriptCommandOutcome::DirtyRootTiles(dirty_tiles))
            }
            ScriptCommand::CancelStroke => {
                if self.cancel_active_stroke() {
                    self.primary_down = false;
                }
                Ok(ScriptCommandOutcome::None)
            }
            ScriptCommand::Undo => {
                if self.undo() {
                    self.request_redraw();
                    Ok(ScriptCommandOutcome::RedrawRequested)
                } else {
                    Ok(ScriptCommandOutcome::None)
                }
            }
            ScriptCommand::Redo => {
                if self.redo() {
                    self.request_redraw();
                    Ok(ScriptCommandOutcome::RedrawRequested)
                } else {
                    Ok(ScriptCommandOutcome::None)
                }
            }
            ScriptCommand::RequestRedraw => {
                self.request_redraw();
                Ok(ScriptCommandOutcome::RedrawRequested)
            }
        }
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
                if self.trace.is_replaying() {
                    return;
                }
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
                if self.trace.is_replaying() {
                    return;
                }
                let was_primary_down = self.primary_down;
                self.primary_down = state == ElementState::Pressed;
                if self.primary_down && !was_primary_down {
                    self.begin_stroke_at_last_cursor();
                } else if was_primary_down && self.finish_stroke_from_window() {
                    self.request_redraw();
                }
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Middle,
                ..
            } => {
                if self.trace.is_replaying() {
                    return;
                }
                self.middle_down = state == ElementState::Pressed;
                self.middle_pan_last_pos = if self.middle_down {
                    self.last_cursor_pos
                } else {
                    None
                };
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if self.trace.is_replaying() {
                    return;
                }
                if self.zoom_view_at_cursor(&delta) {
                    self.request_full_screen_update();
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if self.trace.is_replaying() {
                    return;
                }
                if event.state == ElementState::Pressed && !event.repeat {
                    match &event.logical_key {
                        Key::Named(NamedKey::Escape) => {
                            if self.cancel_stroke_from_window() {
                                self.primary_down = false;
                            }
                        }
                        Key::Character(value) => {
                            if self.modifiers.control_key()
                                && self.modifiers.shift_key()
                                && value.eq_ignore_ascii_case("z")
                            {
                                if self.redo_from_window() {
                                    self.request_redraw();
                                }
                            } else if self.modifiers.control_key()
                                && value.eq_ignore_ascii_case("z")
                            {
                                if self.undo_from_window() {
                                    self.request_redraw();
                                }
                            } else if self.modifiers.control_key()
                                && value.eq_ignore_ascii_case("y")
                            {
                                if self.redo_from_window() {
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
        if self.trace.is_replaying() && self.process_next_trace_replay_event() {
            self.request_redraw();
        }
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
        ActiveTool, AppTraceCanvasInput, AppTraceConfig, AppTraceEvent, AppView, BrushId,
        BrushSettings, DEFAULT_CANVAS_HEIGHT_PX, DEFAULT_CANVAS_WIDTH_PX, DocumentWorkspace,
        RoundBrushSettings, ScriptCommand, ScriptCommandOutcome, ScriptHost, ScriptHostError, Tool,
        ToolSet, load_trace_file, save_trace_file,
    };
    use gla_core::{CanvasCoordF, CanvasInput, ScreenCoordF};
    use gla_ir::{
        DocImageUse, DocumentVersionId, DrawOnToolKind, DrawSessionIR, ImageId, ImageLayoutSpec,
        ImageRole, RegistryPatch, RegistryPatchOp,
    };

    fn canvas_input(time_ns: u64, x: f32, y: f32, pressure: f32) -> CanvasInput {
        CanvasInput {
            time_ns,
            position: CanvasCoordF::new(x, y),
            pressure,
            tilt: (0.0, 0.0),
            twist: 0.0,
        }
    }

    fn trace_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "glaphica-runtime-trace-{name}-{}.json",
            std::process::id()
        ))
    }

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
        assert_eq!(config.trace_config, AppTraceConfig::Disabled);
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

        let stroke = app.brush_thread.finish_active_stroke().unwrap();
        assert_eq!(stroke.brush_id(), BrushId::DEFAULT);
        assert_eq!(stroke.inputs().len(), 2);
        assert_eq!(stroke.inputs()[0].position, CanvasCoordF::new(1.0, 2.0));
        assert_eq!(stroke.inputs()[1].position, CanvasCoordF::new(2.0, 4.0));
        assert_eq!(stroke.inputs()[0].pressure, 1.0);
        assert!(stroke.inputs()[1].time_ns > stroke.inputs()[0].time_ns);

        let samples = stroke.replace_circle_samples();
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
            .brush_thread
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

        assert!(app.brush_thread.has_active_stroke());
        assert!(app.cancel_active_stroke());
        assert!(!app.brush_thread.has_active_stroke());
        assert!(!app.cancel_active_stroke());
    }

    #[test]
    fn script_host_updates_round_brush_settings_for_next_stroke() {
        let mut app = App::new(AppRuntimeConfig::default());

        let outcome = app
            .execute_script_command(ScriptCommand::SetRoundBrushSettings(RoundBrushSettings {
                base_radius_px: 3.0,
                spacing_ratio: 1.0,
                base_hardness: 0.4,
                base_flow: 1.0,
                base_opacity: 1.0,
                tint: [0.2, 0.4, 0.6],
            }))
            .unwrap();
        app.execute_script_command(ScriptCommand::BeginStroke(canvas_input(0, 1.0, 2.0, 1.0)))
            .unwrap();

        let samples = app
            .brush_thread
            .finish_active_stroke()
            .unwrap()
            .replace_circle_samples();

        assert_eq!(outcome, ScriptCommandOutcome::None);
        assert_eq!(app.config.brush_settings.radius_px, 3.0);
        assert_eq!(samples[0].radius_px, 3.0);
        assert_eq!(
            samples[0].color,
            gla_color::PremultipliedRgbaF32::new(0.2, 0.4, 0.6, 1.0)
        );
    }

    #[test]
    fn script_host_sets_registered_active_tool() {
        let second_brush = BrushId::new(2);
        let mut config = AppRuntimeConfig::default();
        config.tool_set = ToolSet::new(vec![
            Tool::Brush(BrushId::DEFAULT),
            Tool::Brush(second_brush),
        ]);
        let mut app = App::new(config);

        app.execute_script_command(ScriptCommand::BeginStroke(canvas_input(0, 0.0, 0.0, 1.0)))
            .unwrap();
        let outcome = app
            .execute_script_command(ScriptCommand::SetActiveTool(ActiveTool::Brush(
                second_brush,
            )))
            .unwrap();
        app.execute_script_command(ScriptCommand::BeginStroke(canvas_input(1, 1.0, 2.0, 1.0)))
            .unwrap();
        let finished = app.brush_thread.finish_active_stroke().unwrap();

        assert_eq!(outcome, ScriptCommandOutcome::None);
        assert_eq!(app.config.active_tool, ActiveTool::Brush(second_brush));
        assert_eq!(finished.brush_id(), second_brush);
    }

    #[test]
    fn script_host_rejects_unregistered_active_tool() {
        let mut app = App::new(AppRuntimeConfig::default());

        let error = app
            .execute_script_command(ScriptCommand::SetActiveTool(ActiveTool::Brush(
                BrushId::new(99),
            )))
            .unwrap_err();

        assert!(matches!(
            error,
            ScriptHostError::InvalidCommand { reason }
                if reason.contains("is not registered")
        ));
        assert_eq!(app.config.active_tool, ActiveTool::Brush(BrushId::DEFAULT));
    }

    #[test]
    fn script_host_requires_begin_before_push() {
        let mut app = App::new(AppRuntimeConfig::default());

        let error = app
            .execute_script_command(ScriptCommand::PushStrokeInput(canvas_input(
                0, 1.0, 2.0, 1.0,
            )))
            .unwrap_err();

        assert!(matches!(
            error,
            ScriptHostError::InvalidCommand { reason }
                if reason.contains("before beginning a stroke")
        ));
    }

    #[test]
    fn script_host_request_redraw_latches_scheduler() {
        let mut app = App::new(AppRuntimeConfig::default());

        let outcome = app
            .execute_script_command(ScriptCommand::RequestRedraw)
            .unwrap();

        assert_eq!(outcome, ScriptCommandOutcome::RedrawRequested);
        assert!(app.frame_scheduler.has_requested_redraw());
    }

    #[test]
    fn script_host_reports_unimplemented_document_commands() {
        let mut app = App::new(AppRuntimeConfig::default());
        let ir = DrawSessionIR {
            expected_document_version: DocumentVersionId::new(7),
            doc_images: vec![DocImageUse::read(ImageId::new(1))],
            session_images: Vec::new(),
            draw_on: Vec::new(),
            derive: Vec::new(),
        };

        let error = app
            .execute_script_command(ScriptCommand::RunDrawSession(ir))
            .unwrap_err();

        assert_eq!(
            error,
            ScriptHostError::UnsupportedCommand {
                command: "RunDrawSession"
            }
        );
    }

    #[test]
    fn script_host_applies_registry_patch_to_workspace() {
        let mut app = App::new(AppRuntimeConfig::default());
        app.workspace = Some(DocumentWorkspace::blank(320, 240).unwrap());

        let outcome = app
            .execute_script_command(ScriptCommand::ApplyRegistryPatch(RegistryPatch::new(vec![
                RegistryPatchOp::NewImage {
                    id: ImageId::new(2),
                    format: gla_color::GlaFormat {
                        channel_count: gla_color::ChannelCount::D4,
                        channel_type: gla_color::ChannelType::F32,
                    },
                    layout: ImageLayoutSpec::new(64, 32),
                    role: ImageRole::Primitive,
                },
                RegistryPatchOp::SetRoot(ImageId::new(2)),
            ])))
            .unwrap();

        let workspace = app.workspace.as_ref().unwrap();
        assert_eq!(
            outcome,
            ScriptCommandOutcome::DocumentVersion(DocumentVersionId::new(2))
        );
        assert_eq!(workspace.root(), ImageId::new(2));
        assert_eq!(workspace.canvas_size_px(), (64, 32));
        assert!(app.undo_stack.is_empty());
        assert!(app.redo_stack.is_empty());
        assert!(app.frame_scheduler.has_requested_redraw());
    }

    #[test]
    fn script_host_rejects_registry_patch_during_active_stroke() {
        let mut app = App::new(AppRuntimeConfig::default());
        app.workspace = Some(DocumentWorkspace::blank(320, 240).unwrap());
        app.execute_script_command(ScriptCommand::BeginStroke(canvas_input(1, 1.0, 2.0, 1.0)))
            .unwrap();

        let error = app
            .execute_script_command(ScriptCommand::ApplyRegistryPatch(RegistryPatch::new(vec![
                RegistryPatchOp::SetRoot(ImageId::new(1)),
            ])))
            .unwrap_err();

        assert!(matches!(
            error,
            ScriptHostError::InvalidCommand { reason }
                if reason.contains("while a stroke is active")
        ));
        assert!(app.brush_thread.has_active_stroke());
    }

    #[test]
    fn window_stroke_methods_record_replayable_trace() {
        let path = trace_path("record");
        {
            let mut config = AppRuntimeConfig::default();
            config.trace_config = AppTraceConfig::record(path.clone());
            let mut app = App::try_new(config).unwrap();

            app.begin_stroke_at(ScreenCoordF::new(10.0, 20.0));
            app.continue_stroke_at(ScreenCoordF::new(12.0, 24.0));
            app.finish_stroke_from_window();
        }

        let events = load_trace_file(&path).unwrap();

        assert_eq!(events.len(), 3);
        assert!(matches!(&events[0], AppTraceEvent::BeginStroke(_)));
        assert!(matches!(&events[1], AppTraceEvent::StrokeSample(_)));
        assert!(matches!(&events[2], AppTraceEvent::FinishStroke));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn trace_replay_events_drive_script_host_stroke_commands() {
        let path = trace_path("replay");
        save_trace_file(
            &path,
            vec![
                AppTraceEvent::BeginStroke(AppTraceCanvasInput::from(canvas_input(
                    1, 2.0, 3.0, 0.5,
                ))),
                AppTraceEvent::StrokeSample(AppTraceCanvasInput::from(canvas_input(
                    2, 5.0, 7.0, 0.75,
                ))),
            ],
        )
        .unwrap();
        let mut config = AppRuntimeConfig::default();
        config.trace_config = AppTraceConfig::replay(path.clone());
        let mut app = App::try_new(config).unwrap();

        assert!(app.process_next_trace_replay_event());
        assert!(app.process_next_trace_replay_event());
        assert!(!app.process_next_trace_replay_event());
        let stroke = app.brush_thread.finish_active_stroke().unwrap();

        assert_eq!(stroke.inputs().len(), 2);
        assert_eq!(stroke.inputs()[0].position, CanvasCoordF::new(2.0, 3.0));
        assert_eq!(stroke.inputs()[1].position, CanvasCoordF::new(5.0, 7.0));
        assert_eq!(stroke.inputs()[0].pressure, 0.5);
        assert_eq!(stroke.inputs()[1].pressure, 0.75);
        let _ = std::fs::remove_file(path);
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
