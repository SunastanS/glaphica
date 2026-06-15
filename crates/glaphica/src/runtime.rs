use std::error::Error;
use std::fmt::{Display, Formatter};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gla_core::{CanvasInput, ScreenCoordF};
use gla_ir::{DrawOnToolKind, RegistryPatch};
use gla_renderer::{GpuRenderer, GpuRendererError, PresentTarget};
use gla_session::{DrawCommit, DrawHistory, DrawRecordId};
use tile_key::NewAtlasError;
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
    ScreenBlitter, ScreenPresentCache, ScriptDrawSession, SurfaceError, SurfaceFrame,
    SurfaceRuntime, ToolSet, UiAction, UiLayerItem, UiTraceStatus, WorkspaceExportError,
    collect_ui_layers,
    egui_overlay::EguiRenderer,
    export_workspace_directory,
    frame::{AppFrameScheduler, ScreenUpdateRequest},
    import_workspace_directory,
    script::{
        ScriptCommand, ScriptCommandOutcome, ScriptCommandPlan, ScriptHost, ScriptHostError,
        script_command_plan_from_json_str,
    },
    stroke::{BrushThreadRuntime, BrushThreadRuntimeError, ReplaceCircleSampleCache},
    trace::{
        AppTraceBlendMode, AppTraceConfig, AppTraceError, AppTraceEvent, AppTraceState,
        AppTraceUiAction,
    },
    ui_overlay::{AppUi, UiPaintOutput},
    visible_layer_index,
};

const BRUSH_THREAD_COMMAND_CAPACITY: usize = 1024;
const ACTIVE_STROKE_COMMIT_FRAME_DAB_BUDGET: u32 = 512;

#[derive(Debug, Clone)]
pub struct AppRuntimeConfig {
    pub window_title: String,
    pub clear_color: wgpu::Color,
    pub canvas_width_px: u32,
    pub canvas_height_px: u32,
    pub workspace_path: Option<PathBuf>,
    pub startup_command_plan_path: Option<PathBuf>,
    pub exit_after_redraw_frames: Option<u64>,
    pub tool_set: ToolSet,
    pub active_tool: ActiveTool,
    pub draw_on_tools: Vec<DrawOnToolKind>,
    pub brush_settings: BrushSettings,
    pub trace_config: AppTraceConfig,
    pub trace_default_path: PathBuf,
    pub perf_trace_config: AppPerfTraceConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppPerfTraceConfig {
    pub stderr_enabled: bool,
    pub slow_threshold: Duration,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct AppFramePerf {
    process_preview: Duration,
    update_cache: Duration,
    acquire_frame: Duration,
    present_surface: Duration,
    dirty_tile_count: usize,
}

#[derive(Debug, Clone)]
struct ActiveStrokePreview {
    samples: ReplaceCircleSampleCache,
    dirty: bool,
}

impl ActiveStrokePreview {
    fn new(brush_settings: BrushSettings, input: CanvasInput) -> Self {
        let mut samples = ReplaceCircleSampleCache::new(brush_settings);
        samples.push_input(input);
        Self {
            samples,
            dirty: true,
        }
    }

    fn push_input(&mut self, input: CanvasInput) {
        self.samples.push_input(input);
        self.dirty = true;
    }

    fn needs_render(&self) -> bool {
        self.dirty
    }

    fn mark_rendered(&mut self) {
        self.dirty = false;
    }

    fn replace_circle_samples(&self) -> Vec<crate::ReplaceCircleStrokeSample> {
        self.samples.replace_circle_samples()
    }
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
            workspace_path: None,
            startup_command_plan_path: None,
            exit_after_redraw_frames: None,
            tool_set: ToolSet::default_brush(),
            active_tool: ActiveTool::Brush(BrushId::DEFAULT),
            draw_on_tools: vec![DrawOnToolKind::ReplaceCircle4D],
            brush_settings: BrushSettings::default(),
            trace_config: AppTraceConfig::default(),
            trace_default_path: PathBuf::from("target/glaphica-trace.json"),
            perf_trace_config: AppPerfTraceConfig::default(),
        }
    }
}

impl AppRuntimeConfig {
    pub fn with_round_brush_settings(mut self, settings: RoundBrushSettings) -> Self {
        self.brush_settings = BrushSettings::from_round_brush(settings);
        self
    }
}

impl Default for AppPerfTraceConfig {
    fn default() -> Self {
        Self {
            stderr_enabled: false,
            slow_threshold: Duration::from_millis(8),
        }
    }
}

impl AppPerfTraceConfig {
    pub fn from_env() -> Self {
        Self {
            stderr_enabled: env_flag("GLAPHICA_APP_PERF_TRACE_STDERR")
                || env_flag("GLAPHICA_PREVIEW_PERF_TRACE_STDERR"),
            slow_threshold: env_millis("GLAPHICA_APP_PERF_TRACE_SLOW_MS")
                .or_else(|| env_millis("GLAPHICA_PREVIEW_PERF_TRACE_SLOW_MS"))
                .map(Duration::from_millis)
                .unwrap_or(Duration::from_millis(8)),
        }
    }

    pub fn stderr(slow_threshold: Duration) -> Self {
        Self {
            stderr_enabled: true,
            slow_threshold,
        }
    }
}

#[derive(Debug)]
pub enum AppRunError {
    EventLoop(winit::error::EventLoopError),
    Trace(AppTraceError),
    BrushThread(BrushThreadRuntimeError),
}

impl Display for AppRunError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EventLoop(error) => write!(f, "app event loop failed: {error}"),
            Self::Trace(error) => write!(f, "app trace setup failed: {error}"),
            Self::BrushThread(error) => write!(f, "app brush thread setup failed: {error}"),
        }
    }
}

impl Error for AppRunError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::EventLoop(error) => Some(error),
            Self::Trace(error) => Some(error),
            Self::BrushThread(error) => Some(error),
        }
    }
}

pub fn run_app_window() -> Result<(), AppRunError> {
    let mut config = AppRuntimeConfig::default();
    config.perf_trace_config = AppPerfTraceConfig::from_env();
    run_app_window_with_config(config)
}

pub fn run_app_window_with_config(config: AppRuntimeConfig) -> Result<(), AppRunError> {
    let event_loop = EventLoop::new().map_err(AppRunError::EventLoop)?;
    let mut app = App::try_new(config)?;
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
    active_stroke_preview: Option<ActiveStrokePreview>,
    trace: AppTraceState,
    primary_down: bool,
    middle_down: bool,
    middle_pan_last_pos: Option<ScreenCoordF>,
    last_cursor_pos: Option<ScreenCoordF>,
    input_clock_start: Instant,
    last_input_time_ns: u64,
    rendered_frame_count: u64,
    perf_frame_seq: u64,
    modifiers: ModifiersState,
    window: Option<Arc<Window>>,
    gpu: Option<GpuCtx>,
    ui: Option<AppUi>,
}

impl App {
    fn new(config: AppRuntimeConfig) -> Self {
        Self::try_new(config).expect("app runtime configuration should initialize")
    }

    fn try_new(config: AppRuntimeConfig) -> Result<Self, AppRunError> {
        let trace = AppTraceState::from_config(&config.trace_config).map_err(AppRunError::Trace)?;
        let brush_thread = BrushThreadRuntime::spawn(
            config.tool_set.clone(),
            config.active_tool,
            config.brush_settings.clone(),
            BRUSH_THREAD_COMMAND_CAPACITY,
        )
        .map_err(AppRunError::BrushThread)?;
        Ok(Self {
            config,
            workspace: None,
            view: AppView::identity(),
            history: DrawHistory::new(),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            frame_scheduler: AppFrameScheduler::new(),
            brush_thread,
            active_stroke_preview: None,
            trace,
            primary_down: false,
            middle_down: false,
            middle_pan_last_pos: None,
            last_cursor_pos: None,
            input_clock_start: Instant::now(),
            last_input_time_ns: 0,
            rendered_frame_count: 0,
            perf_frame_seq: 0,
            modifiers: ModifiersState::default(),
            window: None,
            gpu: None,
            ui: None,
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
        if self
            .workspace
            .as_ref()
            .is_some_and(|workspace| workspace.active_paint_image().is_none())
        {
            return Err(ScriptHostError::InvalidCommand {
                reason: "active document node is not paintable".to_owned(),
            });
        }
        self.brush_thread
            .begin_active_stroke_processing()
            .map_err(|error| ScriptHostError::Runtime {
                reason: error.to_string(),
            })?;
        self.brush_thread.push_canvas_input(input);
        self.active_stroke_preview = Some(ActiveStrokePreview::new(
            self.config.brush_settings.clone(),
            input,
        ));
        self.request_redraw();
        Ok(())
    }

    fn push_canvas_input_from_script(&mut self, input: CanvasInput) -> Result<(), ScriptHostError> {
        if !self.brush_thread.has_active_stroke() {
            return Err(ScriptHostError::InvalidCommand {
                reason: "cannot push stroke input before beginning a stroke".to_owned(),
            });
        }
        self.brush_thread.push_canvas_input(input);
        if let Some(preview) = self.active_stroke_preview.as_mut() {
            preview.push_input(input);
        } else {
            self.active_stroke_preview = Some(ActiveStrokePreview::new(
                self.config.brush_settings.clone(),
                input,
            ));
        }
        self.request_redraw();
        Ok(())
    }

    fn process_pending_active_stroke_preview(&mut self) {
        let samples = match self.active_stroke_preview.as_ref() {
            Some(preview) if preview.needs_render() => preview.replace_circle_samples(),
            _ => return,
        };
        if samples.is_empty() {
            if let Some(preview) = self.active_stroke_preview.as_mut() {
                preview.mark_rendered();
            }
            return;
        }
        self.refresh_layer_composite_if_needed();
        let (Some(workspace), Some(gpu)) = (self.workspace.as_mut(), self.gpu.as_mut()) else {
            return;
        };
        match workspace.render_stroke_preview(gpu.renderer_mut(), samples) {
            Ok(dirty_tiles) if !dirty_tiles.is_empty() => {
                if let Some(preview) = self.active_stroke_preview.as_mut() {
                    preview.mark_rendered();
                }
                self.frame_scheduler.schedule_tile_indices(&dirty_tiles);
            }
            Ok(_) => {
                if let Some(preview) = self.active_stroke_preview.as_mut() {
                    preview.mark_rendered();
                }
            }
            Err(error) => eprintln!("stroke preview render failed: {error}"),
        }
    }

    fn clear_active_stroke_preview_cache(&mut self) {
        self.active_stroke_preview = None;
        let (Some(workspace), Some(gpu)) = (self.workspace.as_mut(), self.gpu.as_mut()) else {
            return;
        };
        match workspace.clear_stroke_preview(gpu.renderer_mut()) {
            Ok(dirty_tiles) if !dirty_tiles.is_empty() => {
                self.frame_scheduler.schedule_tile_indices(&dirty_tiles);
            }
            Ok(_) => {}
            Err(error) => eprintln!("stroke preview clear failed: {error}"),
        }
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
        self.brush_thread
            .set_active_tool(active_tool)
            .map_err(|error| ScriptHostError::Runtime {
                reason: error.to_string(),
            })?;
        self.brush_thread
            .reset_active_stroke_processing()
            .map_err(|error| ScriptHostError::Runtime {
                reason: error.to_string(),
            })?;
        self.config.active_tool = active_tool;
        self.primary_down = false;
        self.clear_active_stroke_preview_cache();
        Ok(())
    }

    fn set_round_brush_settings_from_script(&mut self, settings: RoundBrushSettings) {
        let brush_settings = BrushSettings::from_round_brush(settings.clone());
        self.config.brush_settings = brush_settings.clone();
        self.brush_thread.update_brush_settings(brush_settings);
        if let Some(ui) = self.ui.as_mut() {
            ui.set_round_brush_settings(settings);
        }
        self.clear_active_stroke_preview_cache();
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

    fn run_draw_session_from_script(
        &mut self,
        request: ScriptDrawSession,
    ) -> Result<ScriptCommandOutcome, ScriptHostError> {
        if self.brush_thread.has_active_stroke() {
            return Err(ScriptHostError::InvalidCommand {
                reason: "cannot run a draw session while a stroke is active".to_owned(),
            });
        }
        let (Some(workspace), Some(gpu)) = (self.workspace.as_mut(), self.gpu.as_mut()) else {
            return Err(ScriptHostError::InvalidCommand {
                reason: "cannot run a draw session before the document workspace and GPU exist"
                    .to_owned(),
            });
        };
        let commit = workspace
            .run_script_draw_session(&mut self.history, gpu.renderer_mut(), &request)
            .map_err(|error| ScriptHostError::Runtime {
                reason: error.to_string(),
            })?;
        let Some(commit) = commit else {
            return Ok(ScriptCommandOutcome::None);
        };
        self.undo_stack.push(commit.record_id);
        self.redo_stack.clear();
        self.request_full_screen_update();
        Ok(ScriptCommandOutcome::DocumentVersion(commit.version))
    }

    fn open_workspace_directory_from_script(
        &mut self,
        path: PathBuf,
    ) -> Result<ScriptCommandOutcome, ScriptHostError> {
        if self.brush_thread.has_active_stroke() {
            return Err(ScriptHostError::InvalidCommand {
                reason: "cannot open a workspace while a stroke is active".to_owned(),
            });
        }
        let Some(gpu) = self.gpu.as_mut() else {
            return Err(ScriptHostError::InvalidCommand {
                reason: "cannot open a workspace before the GPU exists".to_owned(),
            });
        };
        let workspace = import_workspace_directory(gpu.renderer_mut(), path).map_err(|error| {
            ScriptHostError::Runtime {
                reason: error.to_string(),
            }
        })?;
        let version = workspace.version();
        self.workspace = Some(workspace);
        self.history = DrawHistory::new();
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.primary_down = false;
        if let Err(error) = self.fit_view_to_workspace() {
            return Err(ScriptHostError::Runtime {
                reason: error.to_string(),
            });
        }
        self.request_full_screen_update();
        Ok(ScriptCommandOutcome::DocumentVersion(version))
    }

    fn export_workspace_directory_from_script(
        &mut self,
        path: PathBuf,
    ) -> Result<ScriptCommandOutcome, ScriptHostError> {
        if self.brush_thread.has_active_stroke() {
            return Err(ScriptHostError::InvalidCommand {
                reason: "cannot export a workspace while a stroke is active".to_owned(),
            });
        }
        let (Some(workspace), Some(gpu)) = (self.workspace.as_ref(), self.gpu.as_ref()) else {
            return Err(ScriptHostError::InvalidCommand {
                reason: "cannot export a workspace before the document workspace and GPU exist"
                    .to_owned(),
            });
        };
        let version = workspace.version();
        export_workspace_directory(workspace, &gpu.renderer, path).map_err(|error| {
            ScriptHostError::Runtime {
                reason: error.to_string(),
            }
        })?;
        Ok(ScriptCommandOutcome::DocumentVersion(version))
    }

    fn run_layer_command(
        &mut self,
        clear_history: bool,
        request_full_update: bool,
        command: impl FnOnce(&mut DocumentWorkspace) -> Result<ScriptCommandOutcome, String>,
    ) -> Result<ScriptCommandOutcome, ScriptHostError> {
        if self.brush_thread.has_active_stroke() {
            return Err(ScriptHostError::InvalidCommand {
                reason: "cannot update document layers while a stroke is active".to_owned(),
            });
        }
        let Some(workspace) = self.workspace.as_mut() else {
            return Err(ScriptHostError::InvalidCommand {
                reason: "cannot update document layers before the workspace exists".to_owned(),
            });
        };
        let outcome = command(workspace).map_err(|reason| ScriptHostError::Runtime { reason })?;
        if clear_history {
            self.history = DrawHistory::new();
            self.undo_stack.clear();
            self.redo_stack.clear();
        }
        if request_full_update {
            self.request_full_screen_update();
        }
        Ok(outcome)
    }

    fn execute_ui_action(
        &mut self,
        action: UiAction,
    ) -> Result<ScriptCommandOutcome, ScriptHostError> {
        match action {
            UiAction::StartRecordingRequested => {
                self.trace.start_recording(&self.config.trace_default_path);
                Ok(ScriptCommandOutcome::None)
            }
            UiAction::StopRecordingRequested => self
                .trace
                .stop_recording()
                .map(|_| ScriptCommandOutcome::None)
                .map_err(|error| ScriptHostError::Runtime {
                    reason: error.to_string(),
                }),
            UiAction::ReplayRequested => self
                .trace
                .load_replay(&self.config.trace_default_path)
                .map(|()| ScriptCommandOutcome::None)
                .map_err(|error| ScriptHostError::Runtime {
                    reason: error.to_string(),
                }),
            UiAction::UndoRequested => {
                if self.undo() {
                    self.request_redraw();
                    Ok(ScriptCommandOutcome::RedrawRequested)
                } else {
                    Ok(ScriptCommandOutcome::None)
                }
            }
            UiAction::CreateLayerRequested => {
                self.execute_script_command(ScriptCommand::CreateLayerAboveActive)
            }
            UiAction::CreateGroupRequested => {
                self.execute_script_command(ScriptCommand::CreateGroupAboveActive)
            }
            UiAction::DeleteActiveNodeRequested => {
                self.execute_script_command(ScriptCommand::DeleteActiveNode)
            }
            UiAction::ActiveNodeChanged(node_id) => {
                self.execute_script_command(ScriptCommand::SetActiveNode(node_id))
            }
            UiAction::NodeOpacityChanged(node_id, opacity) => {
                self.execute_script_command(ScriptCommand::SetNodeOpacity { node_id, opacity })
            }
            UiAction::NodeBlendModeChanged(node_id, blend_mode) => {
                self.execute_script_command(ScriptCommand::SetNodeBlendMode {
                    node_id,
                    blend_mode,
                })
            }
            UiAction::RoundBrushSettingsChanged(settings) => {
                self.execute_script_command(ScriptCommand::SetRoundBrushSettings(settings))
            }
        }
    }

    fn execute_ui_action_from_window(&mut self, action: UiAction) {
        let trace_action = self
            .ui_layers()
            .ok()
            .and_then(|layers| trace_action_for_ui_action(&layers, &action));
        match self.execute_ui_action(action) {
            Ok(_) => {
                if let Some(trace_action) = trace_action {
                    self.trace.record(AppTraceEvent::Ui(trace_action));
                }
            }
            Err(error) => {
                eprintln!("ui action failed: {error}");
            }
        }
    }

    fn paint_ui_overlay(&mut self, window: &Window) -> Option<UiPaintOutput> {
        let document_size = self
            .workspace
            .as_ref()
            .map(|workspace| {
                let (width, height) = workspace.canvas_size_px();
                [width, height]
            })
            .unwrap_or([self.config.canvas_width_px, self.config.canvas_height_px]);
        let layers = self
            .workspace
            .as_ref()
            .and_then(|workspace| collect_ui_layers(workspace).ok())
            .unwrap_or_default();
        let stroke_active = self.brush_thread.has_active_stroke();
        let trace_status = UiTraceStatus::from(self.trace.status());

        self.ui
            .as_mut()
            .map(|ui| ui.paint(window, document_size, &layers, stroke_active, &trace_status))
    }

    fn process_ui_overlay_actions(&mut self, output: &mut UiPaintOutput) {
        let actions = std::mem::take(&mut output.actions);
        for action in actions {
            self.execute_ui_action_from_window(action);
        }
    }

    fn select_visible_layer_from_window(&mut self, delta: isize) {
        let layers = match self.ui_layers() {
            Ok(layers) if !layers.is_empty() => layers,
            Ok(_) => return,
            Err(error) => {
                eprintln!("ui layer collection failed: {error}");
                return;
            }
        };
        let active_index = layers
            .iter()
            .position(|layer| layer.active)
            .unwrap_or_default();
        let next_index = active_index
            .saturating_add_signed(delta)
            .min(layers.len().saturating_sub(1));
        let next_id = layers[next_index].id;
        self.execute_ui_action_from_window(UiAction::ActiveNodeChanged(next_id));
    }

    fn ui_layers(&self) -> Result<Vec<UiLayerItem>, ScriptHostError> {
        let Some(workspace) = self.workspace.as_ref() else {
            return Ok(Vec::new());
        };
        collect_ui_layers(workspace).map_err(|error| ScriptHostError::Runtime {
            reason: error.to_string(),
        })
    }

    fn delete_active_node_from_script(&mut self) -> Result<ScriptCommandOutcome, ScriptHostError> {
        if self.brush_thread.has_active_stroke() {
            return Err(ScriptHostError::InvalidCommand {
                reason: "cannot update document layers while a stroke is active".to_owned(),
            });
        }
        let Some(workspace) = self.workspace.as_mut() else {
            return Err(ScriptHostError::InvalidCommand {
                reason: "cannot update document layers before the workspace exists".to_owned(),
            });
        };
        let deleted = workspace
            .delete_active_node()
            .map_err(|error| ScriptHostError::Runtime {
                reason: error.to_string(),
            })?;
        if !deleted {
            return Ok(ScriptCommandOutcome::None);
        }
        self.history = DrawHistory::new();
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.request_full_screen_update();
        Ok(ScriptCommandOutcome::RedrawRequested)
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
        let stroke = match self.brush_thread.finish_active_stroke_processing() {
            Ok(Some(stroke)) => stroke,
            Ok(None) => return None,
            Err(error) => {
                eprintln!("stroke finish failed: {error}");
                return None;
            }
        };
        let (Some(workspace), Some(gpu)) = (self.workspace.as_mut(), self.gpu.as_mut()) else {
            self.brush_thread.restore_active_stroke(stroke);
            return None;
        };
        self.active_stroke_preview = None;
        match workspace.clear_stroke_preview(gpu.renderer_mut()) {
            Ok(dirty_tiles) if !dirty_tiles.is_empty() => {
                self.frame_scheduler.schedule_tile_indices(&dirty_tiles);
            }
            Ok(_) => {}
            Err(error) => eprintln!("stroke preview clear failed: {error}"),
        }
        let commit = match workspace
            .replace_circle_brush_input_on_active_paint_target_with_frame_budget(
                &mut self.history,
                gpu.renderer_mut(),
                stroke.brush_input(),
                ACTIVE_STROKE_COMMIT_FRAME_DAB_BUDGET,
            ) {
            Ok(Some(commit)) => commit,
            Ok(None) => return None,
            Err(error) => {
                eprintln!("stroke failed: {error}");
                self.brush_thread.restore_active_stroke(stroke);
                return None;
            }
        };
        let dirty_tiles = workspace.dirty_tile_indices(&commit);
        self.undo_stack.push(commit.record_id);
        self.redo_stack.clear();
        self.frame_scheduler.schedule_tile_indices(&dirty_tiles);
        Some(dirty_tiles)
    }

    fn cancel_active_stroke(&mut self) -> bool {
        let canceled = self.brush_thread.cancel_active_stroke();
        if canceled {
            self.clear_active_stroke_preview_cache();
        }
        canceled
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
            AppTraceEvent::Ui(action) => {
                self.execute_trace_ui_action(action)?;
            }
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

    fn execute_trace_ui_action(&mut self, action: AppTraceUiAction) -> Result<(), ScriptHostError> {
        let layers = self.ui_layers()?;
        let Some(action) = ui_action_for_trace_action(&layers, action) else {
            return Ok(());
        };
        self.execute_ui_action(action)?;
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
        let dirty_tiles = workspace.dirty_tile_indices(commit);
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

    fn trace_frame_perf(&mut self, total: Duration, perf: &AppFramePerf) {
        let config = self.config.perf_trace_config;
        if !config.stderr_enabled || total < config.slow_threshold {
            return;
        }
        self.perf_frame_seq = self.perf_frame_seq.saturating_add(1);
        eprintln!("{}", format_app_perf_line(self.perf_frame_seq, total, perf));
    }

    fn refresh_layer_composite_if_needed(&mut self) -> bool {
        let (Some(workspace), Some(gpu)) = (self.workspace.as_mut(), self.gpu.as_mut()) else {
            return false;
        };
        if !workspace.layer_composite_needs_render() {
            return false;
        }
        match workspace.render_layer_tree_full(gpu.renderer_mut()) {
            Ok(dirty) => {
                if dirty.is_empty() {
                    false
                } else {
                    self.frame_scheduler.schedule_full_update();
                    true
                }
            }
            Err(error) => {
                eprintln!("document layer composite render failed: {error}");
                false
            }
        }
    }

    fn execute_startup_command_plan(&mut self) -> Result<(), ScriptHostError> {
        let Some(path) = self.config.startup_command_plan_path.clone() else {
            return Ok(());
        };
        let source = std::fs::read_to_string(&path).map_err(|error| ScriptHostError::Runtime {
            reason: format!(
                "failed to read startup command plan {}: {error}",
                path.display()
            ),
        })?;
        let plan = script_command_plan_from_json_str(&source).map_err(|error| {
            ScriptHostError::InvalidCommand {
                reason: format!(
                    "failed to parse startup command plan {}: {error}",
                    path.display()
                ),
            }
        })?;
        self.execute_script_command_plan(&plan)?;
        self.request_full_screen_update();
        Ok(())
    }

    fn execute_script_command_plan(
        &mut self,
        plan: &ScriptCommandPlan,
    ) -> Result<Vec<ScriptCommandOutcome>, ScriptHostError> {
        plan.execute_on(self)
    }
}

impl Drop for App {
    fn drop(&mut self) {
        if let Err(error) = self.trace.stop_recording() {
            eprintln!("trace save failed: {error}");
        }
        // Surface-backed resources must go away while the Arc<Window> used to create the surface
        // is still alive.
        self.ui = None;
        self.gpu = None;
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
            ScriptCommand::OpenWorkspaceDirectory(path) => {
                self.open_workspace_directory_from_script(path)
            }
            ScriptCommand::ExportWorkspaceDirectory(path) => {
                self.export_workspace_directory_from_script(path)
            }
            ScriptCommand::AppendLayer { parent } => {
                self.run_layer_command(true, true, |workspace| {
                    workspace
                        .append_layer(parent)
                        .map(ScriptCommandOutcome::DocumentNode)
                        .map_err(|error| error.to_string())
                })
            }
            ScriptCommand::AppendGroup { parent } => {
                self.run_layer_command(true, true, |workspace| {
                    workspace
                        .append_group(parent)
                        .map(ScriptCommandOutcome::DocumentNode)
                        .map_err(|error| error.to_string())
                })
            }
            ScriptCommand::CreateLayerAboveActive => {
                self.run_layer_command(true, true, |workspace| {
                    workspace
                        .insert_layer_above_active()
                        .map(ScriptCommandOutcome::DocumentNode)
                        .map_err(|error| error.to_string())
                })
            }
            ScriptCommand::CreateGroupAboveActive => {
                self.run_layer_command(true, true, |workspace| {
                    workspace
                        .insert_group_above_active()
                        .map(ScriptCommandOutcome::DocumentNode)
                        .map_err(|error| error.to_string())
                })
            }
            ScriptCommand::DeleteNode(node_id) => self.run_layer_command(true, true, |workspace| {
                workspace
                    .delete_node(node_id)
                    .map(|()| ScriptCommandOutcome::RedrawRequested)
                    .map_err(|error| error.to_string())
            }),
            ScriptCommand::DeleteActiveNode => self.delete_active_node_from_script(),
            ScriptCommand::MoveNode {
                node_id,
                new_parent,
                new_index,
            } => self.run_layer_command(true, true, |workspace| {
                workspace
                    .move_node(node_id, new_parent, new_index)
                    .map(|()| ScriptCommandOutcome::RedrawRequested)
                    .map_err(|error| error.to_string())
            }),
            ScriptCommand::SetActiveNode(node_id) => {
                self.run_layer_command(false, false, |workspace| {
                    workspace
                        .set_active_node(node_id)
                        .map(|()| ScriptCommandOutcome::None)
                        .map_err(|error| error.to_string())
                })
            }
            ScriptCommand::SetNodeOpacity { node_id, opacity } => {
                self.run_layer_command(false, true, |workspace| {
                    workspace
                        .set_node_opacity(node_id, opacity)
                        .map(|()| ScriptCommandOutcome::RedrawRequested)
                        .map_err(|error| error.to_string())
                })
            }
            ScriptCommand::SetNodeBlendMode {
                node_id,
                blend_mode,
            } => self.run_layer_command(false, true, |workspace| {
                workspace
                    .set_node_blend_mode(node_id, blend_mode)
                    .map(|()| ScriptCommandOutcome::RedrawRequested)
                    .map_err(|error| error.to_string())
            }),
            ScriptCommand::RunDrawSession(request) => self.run_draw_session_from_script(request),
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

fn trace_action_for_ui_action(
    layers: &[UiLayerItem],
    action: &UiAction,
) -> Option<AppTraceUiAction> {
    match action {
        UiAction::UndoRequested => Some(AppTraceUiAction::Undo),
        UiAction::CreateLayerRequested => Some(AppTraceUiAction::CreateLayer),
        UiAction::CreateGroupRequested => Some(AppTraceUiAction::CreateGroup),
        UiAction::DeleteActiveNodeRequested => Some(AppTraceUiAction::DeleteActiveNode),
        UiAction::ActiveNodeChanged(node_id) => visible_layer_index(layers, *node_id)
            .map(|visible_index| AppTraceUiAction::SelectLayer { visible_index }),
        UiAction::NodeOpacityChanged(node_id, opacity) => visible_layer_index(layers, *node_id)
            .map(|visible_index| AppTraceUiAction::SetLayerOpacity {
                visible_index,
                opacity: *opacity,
            }),
        UiAction::NodeBlendModeChanged(node_id, blend_mode) => {
            visible_layer_index(layers, *node_id).map(|visible_index| {
                AppTraceUiAction::SetLayerBlendMode {
                    visible_index,
                    blend_mode: AppTraceBlendMode::from(*blend_mode),
                }
            })
        }
        UiAction::RoundBrushSettingsChanged(settings) => Some(
            AppTraceUiAction::SetRoundBrushSettings(settings.clone().into()),
        ),
        UiAction::StartRecordingRequested
        | UiAction::StopRecordingRequested
        | UiAction::ReplayRequested => None,
    }
}

fn ui_action_for_trace_action(
    layers: &[UiLayerItem],
    action: AppTraceUiAction,
) -> Option<UiAction> {
    match action {
        AppTraceUiAction::Undo => Some(UiAction::UndoRequested),
        AppTraceUiAction::CreateLayer => Some(UiAction::CreateLayerRequested),
        AppTraceUiAction::CreateGroup => Some(UiAction::CreateGroupRequested),
        AppTraceUiAction::DeleteActiveNode => Some(UiAction::DeleteActiveNodeRequested),
        AppTraceUiAction::SelectLayer { visible_index } => layers
            .get(visible_index)
            .map(|layer| UiAction::ActiveNodeChanged(layer.id)),
        AppTraceUiAction::SetLayerOpacity {
            visible_index,
            opacity,
        } => layers
            .get(visible_index)
            .map(|layer| UiAction::NodeOpacityChanged(layer.id, opacity)),
        AppTraceUiAction::SetLayerBlendMode {
            visible_index,
            blend_mode,
        } => layers
            .get(visible_index)
            .map(|layer| UiAction::NodeBlendModeChanged(layer.id, blend_mode.into())),
        AppTraceUiAction::SetRoundBrushSettings(settings) => {
            Some(UiAction::RoundBrushSettingsChanged(settings.into()))
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
    ui_renderer: EguiRenderer,
}

#[derive(Debug)]
enum GpuInitError {
    CreateSurface(wgpu::CreateSurfaceError),
    Document(DocumentWorkspaceInitError<GpuRendererError>),
    DrawOnAtlas(NewAtlasError<GpuRendererError>),
    WorkspaceImport(WorkspaceExportError),
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
            Self::DrawOnAtlas(error) => {
                write!(f, "failed to allocate draw-on workspace atlas: {error}")
            }
            Self::WorkspaceImport(error) => write!(f, "failed to import workspace: {error}"),
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
            Self::DrawOnAtlas(error) => Some(error),
            Self::WorkspaceImport(error) => Some(error),
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
        let ui_renderer = EguiRenderer::new(&device, surface.format());

        let mut renderer = GpuRenderer::with_draw_on_tools(
            &adapter,
            device.clone(),
            queue.clone(),
            app_config.draw_on_tools.iter().copied(),
        )
        .map_err(GpuInitError::Renderer)?;
        let mut workspace = match &app_config.workspace_path {
            Some(path) => import_workspace_directory(&mut renderer, path)
                .map_err(GpuInitError::WorkspaceImport)?,
            None => DocumentWorkspace::white_with_textures(
                app_config.canvas_width_px,
                app_config.canvas_height_px,
                &mut renderer,
            )
            .map_err(GpuInitError::Document)?,
        };
        workspace
            .ensure_draw_on_tool_atlases(app_config.draw_on_tools.iter().copied(), &mut renderer)
            .map_err(GpuInitError::DrawOnAtlas)?;

        Ok((
            Self {
                surface,
                device,
                queue,
                clear_color: app_config.clear_color,
                renderer,
                screen_cache,
                screen_blitter,
                ui_renderer,
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
        ui_output: Option<&UiPaintOutput>,
    ) -> Option<AppFramePerf> {
        let dirty_tile_count = match &update_request {
            ScreenUpdateRequest::Tiles(tile_indices) => tile_indices.len(),
            ScreenUpdateRequest::Full | ScreenUpdateRequest::None => 0,
        };
        let mut perf = AppFramePerf {
            dirty_tile_count,
            ..AppFramePerf::default()
        };

        let update_cache_started = Instant::now();
        let cache_ready = self.update_screen_cache(workspace, view, update_request);
        perf.update_cache = update_cache_started.elapsed();

        let acquire_frame_started = Instant::now();
        let frame = match self.surface.acquire_frame(&self.device) {
            Ok(frame) => frame,
            Err(error) => {
                eprintln!("surface acquire failed: {error}");
                return None;
            }
        };
        perf.acquire_frame = acquire_frame_started.elapsed();

        let present_started = Instant::now();
        {
            if let Some(ui_output) = ui_output {
                self.ui_renderer.upload_textures(
                    &self.device,
                    &self.queue,
                    &ui_output.textures_delta,
                );
                self.ui_renderer.upload_meshes(
                    &self.device,
                    &self.queue,
                    &ui_output.clipped_primitives,
                );
            }
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
            if let Some(ui_output) = ui_output {
                self.render_ui_overlay_to_frame(&frame, ui_output);
            }
            SurfaceRuntime::present(frame);
        }
        perf.present_surface = present_started.elapsed();
        Some(perf)
    }

    fn render_ui_overlay_to_frame(&self, frame: &SurfaceFrame, ui_output: &UiPaintOutput) {
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("glaphica-egui-overlay-encoder"),
            });
        self.ui_renderer.render(
            &self.queue,
            &mut encoder,
            &frame.view,
            [self.surface.width(), self.surface.height()],
            ui_output.pixels_per_point,
        );
        self.queue.submit(Some(encoder.finish()));
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

        self.ui = Some(AppUi::new(
            event_loop,
            &window,
            round_brush_settings_from_brush_settings(self.config.brush_settings.clone()),
        ));
        self.window = Some(window);
        self.workspace = Some(workspace);
        self.gpu = Some(gpu);
        if let Err(error) = self.fit_view_to_workspace() {
            eprintln!("view initialization failed: {error}");
            event_loop.exit();
            return;
        }
        if let Err(error) = self.execute_startup_command_plan() {
            eprintln!("startup command plan failed: {error}");
            event_loop.exit();
            return;
        }
        if let Err(error) = self.fit_view_to_workspace() {
            eprintln!("view initialization after startup command plan failed: {error}");
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

        let ui_response = self
            .ui
            .as_mut()
            .map(|ui| ui.on_window_event(&window, &event));
        let ui_consumed = ui_response
            .as_ref()
            .is_some_and(|response| response.consumed);
        if ui_response
            .as_ref()
            .is_some_and(|response| response.repaint)
        {
            self.request_redraw();
        }
        if ui_consumed && ui_consumes_app_input(&event) {
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
                        Key::Named(NamedKey::Delete) if self.modifiers.control_key() => {
                            self.execute_ui_action_from_window(UiAction::DeleteActiveNodeRequested);
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
                                && self.modifiers.shift_key()
                                && value.eq_ignore_ascii_case("l")
                            {
                                self.execute_ui_action_from_window(UiAction::CreateGroupRequested);
                            } else if self.modifiers.control_key() && value == "[" {
                                self.select_visible_layer_from_window(-1);
                            } else if self.modifiers.control_key() && value == "]" {
                                self.select_visible_layer_from_window(1);
                            } else if self.modifiers.control_key()
                                && value.eq_ignore_ascii_case("z")
                            {
                                if self.undo_from_window() {
                                    self.request_redraw();
                                }
                            } else if self.modifiers.control_key()
                                && value.eq_ignore_ascii_case("l")
                            {
                                self.execute_ui_action_from_window(UiAction::CreateLayerRequested);
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
                let frame_started = Instant::now();
                let process_preview_started = Instant::now();
                let mut ui_output = self.paint_ui_overlay(&window);
                if let Some(output) = ui_output.as_mut() {
                    self.process_ui_overlay_actions(output);
                }
                self.refresh_layer_composite_if_needed();
                self.process_pending_active_stroke_preview();
                let process_preview = process_preview_started.elapsed();
                let update_request = self.frame_scheduler.take_screen_update_request();
                let mut perf = self.gpu.as_mut().and_then(|gpu| {
                    gpu.render(
                        self.workspace.as_ref(),
                        &self.view,
                        update_request,
                        ui_output.as_ref(),
                    )
                });
                if let Some(perf) = perf.as_mut() {
                    perf.process_preview = process_preview;
                    self.trace_frame_perf(frame_started.elapsed(), perf);
                    self.rendered_frame_count = self.rendered_frame_count.saturating_add(1);
                }
                self.frame_scheduler.reset_redraw_request();
                if let Some(limit) = self.config.exit_after_redraw_frames {
                    if self.rendered_frame_count >= limit {
                        event_loop.exit();
                    } else {
                        self.request_redraw();
                    }
                }
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

fn round_brush_settings_from_brush_settings(settings: BrushSettings) -> RoundBrushSettings {
    RoundBrushSettings {
        base_radius_px: settings.radius_px,
        spacing_ratio: settings.spacing_ratio,
        base_hardness: settings.hardness,
        base_flow: settings.flow,
        base_opacity: settings.opacity,
        tint: [settings.color.r, settings.color.g, settings.color.b],
        modulations: settings.modulations,
    }
}

fn ui_consumes_app_input(event: &WindowEvent) -> bool {
    matches!(
        event,
        WindowEvent::CursorMoved { .. }
            | WindowEvent::MouseInput { .. }
            | WindowEvent::MouseWheel { .. }
            | WindowEvent::KeyboardInput { .. }
    )
}

fn scroll_delta_lines(delta: &MouseScrollDelta) -> f32 {
    match delta {
        MouseScrollDelta::LineDelta(_, y) => *y,
        MouseScrollDelta::PixelDelta(position) => finite_f64_to_f32(position.y) / 40.0,
    }
}

fn env_flag(name: &str) -> bool {
    std::env::var_os(name)
        .and_then(|value| value.into_string().ok())
        .is_some_and(|value| {
            let value = value.trim();
            !value.is_empty() && value != "0" && !value.eq_ignore_ascii_case("false")
        })
}

fn env_millis(name: &str) -> Option<u64> {
    std::env::var_os(name)
        .and_then(|value| value.into_string().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
}

fn duration_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

fn format_app_perf_line(frame_seq: u64, total: Duration, perf: &AppFramePerf) -> String {
    let stages = [
        ("process_preview", perf.process_preview),
        ("update_cache", perf.update_cache),
        ("acquire_frame", perf.acquire_frame),
        ("present_surface", perf.present_surface),
    ];
    let (bottleneck, bottleneck_duration) = stages
        .iter()
        .max_by_key(|(_, duration)| *duration)
        .copied()
        .expect("app frame perf should always have stages");
    format!(
        "[PERF][app][frame={frame_seq}] total_ms={:.3} bottleneck={bottleneck} ({:.3}ms) dirty_tiles={} stages_ms={{process_preview:{:.3}, update_cache:{:.3}, acquire_frame:{:.3}, present_surface:{:.3}}}",
        duration_ms(total),
        duration_ms(bottleneck_duration),
        perf.dirty_tile_count,
        duration_ms(perf.process_preview),
        duration_ms(perf.update_cache),
        duration_ms(perf.acquire_frame),
        duration_ms(perf.present_surface),
    )
}

#[cfg(test)]
mod tests {
    use super::{App, AppFramePerf, AppPerfTraceConfig, AppRunError, AppRuntimeConfig};
    use crate::{
        ActiveTool, AppTraceCanvasInput, AppTraceConfig, AppTraceEvent, AppTraceUiAction, AppView,
        BrushId, BrushSettings, BrushThreadRuntimeError, DEFAULT_CANVAS_HEIGHT_PX,
        DEFAULT_CANVAS_WIDTH_PX, DocumentBlendMode, DocumentNodeId, DocumentWorkspace,
        RoundBrushSettings, ScriptCommand, ScriptCommandOutcome, ScriptCommandPlan,
        ScriptDrawSession, ScriptHost, ScriptHostError, Tool, ToolSet, UiAction, load_trace_file,
        save_trace_file, script_command_plan_to_json_string_pretty,
    };
    use gla_core::{CanvasCoordF, CanvasInput, ScreenCoordF};
    use gla_ir::{
        DocImageUse, DocumentVersionId, DrawOnToolKind, DrawSessionIR, ImageId, ImageLayoutSpec,
        ImageRole, RegistryPatch, RegistryPatchOp,
    };
    use std::path::PathBuf;
    use std::time::Duration;

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
        assert_eq!(config.workspace_path, None);
        assert_eq!(config.startup_command_plan_path, None);
        assert_eq!(config.exit_after_redraw_frames, None);
        assert_eq!(config.tool_set.tools(), &[Tool::Brush(BrushId::DEFAULT)]);
        assert_eq!(config.active_tool, ActiveTool::Brush(BrushId::DEFAULT));
        assert_eq!(config.draw_on_tools, vec![DrawOnToolKind::ReplaceCircle4D]);
        assert_eq!(config.brush_settings, BrushSettings::default());
        assert_eq!(config.trace_config, AppTraceConfig::Disabled);
        assert_eq!(
            config.trace_default_path,
            PathBuf::from("target/glaphica-trace.json")
        );
        assert_eq!(config.perf_trace_config, AppPerfTraceConfig::default());
    }

    #[test]
    fn active_brush_requires_registered_tool() {
        let missing_brush = BrushId::new(99);
        let mut config = AppRuntimeConfig::default();
        config.active_tool = ActiveTool::Brush(missing_brush);
        let error = match App::try_new(config) {
            Ok(_) => panic!("app should reject an unregistered active brush"),
            Err(error) => error,
        };

        assert!(matches!(
            error,
            AppRunError::BrushThread(BrushThreadRuntimeError::ActiveToolUnavailable(
                ActiveTool::Brush(brush_id)
            )) if brush_id == missing_brush
        ));
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
            ..RoundBrushSettings::default()
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
    fn app_perf_line_reports_stage_breakdown_and_bottleneck() {
        let perf = AppFramePerf {
            process_preview: Duration::from_micros(400),
            update_cache: Duration::from_micros(500),
            acquire_frame: Duration::from_micros(250),
            present_surface: Duration::from_micros(1_250),
            dirty_tile_count: 3,
        };

        let line = super::format_app_perf_line(7, Duration::from_micros(2_000), &perf);

        assert!(line.contains("[PERF][app][frame=7]"));
        assert!(line.contains("total_ms=2.000"));
        assert!(line.contains("bottleneck=present_surface (1.250ms)"));
        assert!(line.contains("dirty_tiles=3"));
        assert!(line.contains("process_preview:0.400"));
        assert!(line.contains("update_cache:0.500"));
    }

    #[test]
    fn active_stroke_preview_tracks_deferred_render_work() {
        let mut preview = super::ActiveStrokePreview::new(
            BrushSettings::default(),
            canvas_input(0, 1.0, 2.0, 1.0),
        );

        assert!(preview.needs_render());
        preview.mark_rendered();
        assert!(!preview.needs_render());

        preview.push_input(canvas_input(1, 3.0, 4.0, 1.0));

        assert!(preview.needs_render());
        assert_eq!(preview.replace_circle_samples().len(), 2);
    }

    #[test]
    fn active_stroke_records_canvas_inputs_for_current_view() {
        let mut app = App::new(AppRuntimeConfig::default());
        app.view = AppView::new([2.0, 0.0, 0.0, 2.0, 10.0, 20.0]).unwrap();

        app.begin_stroke_at(ScreenCoordF::new(12.0, 24.0));
        app.continue_stroke_at(ScreenCoordF::new(14.0, 28.0));

        let preview_samples = app
            .active_stroke_preview
            .as_ref()
            .unwrap()
            .replace_circle_samples();
        let stroke = app.brush_thread.finish_active_stroke().unwrap();
        assert_eq!(stroke.brush_id(), BrushId::DEFAULT);
        assert_eq!(stroke.inputs().len(), 2);
        assert_eq!(stroke.inputs()[0].position, CanvasCoordF::new(1.0, 2.0));
        assert_eq!(stroke.inputs()[1].position, CanvasCoordF::new(2.0, 4.0));
        assert_eq!(stroke.inputs()[0].pressure, 1.0);
        assert!(stroke.inputs()[1].time_ns > stroke.inputs()[0].time_ns);

        let samples = stroke.replace_circle_samples();
        assert!(app.frame_scheduler.has_requested_redraw());
        assert_eq!(preview_samples, samples);
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
        assert!(app.active_stroke_preview.is_some());
        assert!(app.cancel_active_stroke());
        assert!(!app.brush_thread.has_active_stroke());
        assert!(app.active_stroke_preview.is_none());
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
                ..RoundBrushSettings::default()
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
        assert!(app.active_stroke_preview.is_none());
        app.execute_script_command(ScriptCommand::BeginStroke(canvas_input(1, 1.0, 2.0, 1.0)))
            .unwrap();
        let finished = app.brush_thread.finish_active_stroke().unwrap();

        assert_eq!(outcome, ScriptCommandOutcome::None);
        assert_eq!(app.config.active_tool, ActiveTool::Brush(second_brush));
        assert!(app.active_stroke_preview.is_some());
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
    fn startup_command_plan_file_executes_after_workspace_exists() {
        let path = trace_path("startup-command-plan");
        let plan = ScriptCommandPlan::new(vec![
            ScriptCommand::CreateLayerAboveActive,
            ScriptCommand::SetActiveNode(DocumentNodeId::new(2)),
            ScriptCommand::RequestRedraw,
        ]);
        std::fs::write(
            &path,
            script_command_plan_to_json_string_pretty(&plan).unwrap(),
        )
        .unwrap();
        let mut config = AppRuntimeConfig::default();
        config.startup_command_plan_path = Some(path.clone());
        let mut app = App::new(config);
        app.workspace = Some(DocumentWorkspace::blank(128, 96).unwrap());

        app.execute_startup_command_plan().unwrap();

        let workspace = app.workspace.as_ref().unwrap();
        assert_eq!(workspace.layer_tree().len(), 2);
        assert_eq!(
            workspace.layer_tree().active_node_id(),
            DocumentNodeId::new(2)
        );
        assert!(app.frame_scheduler.has_requested_redraw());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn script_host_rejects_draw_session_before_gpu_workspace_exists() {
        let mut app = App::new(AppRuntimeConfig::default());
        let ir = DrawSessionIR {
            expected_document_version: DocumentVersionId::new(7),
            doc_images: vec![DocImageUse::read(ImageId::new(1))],
            session_images: Vec::new(),
            draw_on: Vec::new(),
            derive: Vec::new(),
        };

        let error = app
            .execute_script_command(ScriptCommand::RunDrawSession(ScriptDrawSession::new(ir)))
            .unwrap_err();

        assert!(matches!(
            error,
            ScriptHostError::InvalidCommand { reason }
                if reason.contains("before the document workspace and GPU exist")
        ));
    }

    #[test]
    fn script_host_rejects_open_workspace_before_gpu_exists() {
        let mut app = App::new(AppRuntimeConfig::default());

        let error = app
            .execute_script_command(ScriptCommand::OpenWorkspaceDirectory(
                "target/workspace-export".into(),
            ))
            .unwrap_err();

        assert!(matches!(
            error,
            ScriptHostError::InvalidCommand { reason }
                if reason.contains("before the GPU exists")
        ));
        assert!(app.workspace.is_none());
        assert!(app.undo_stack.is_empty());
        assert!(app.redo_stack.is_empty());
    }

    #[test]
    fn script_host_rejects_export_workspace_before_gpu_exists() {
        let mut app = App::new(AppRuntimeConfig::default());
        app.workspace = Some(DocumentWorkspace::blank(320, 240).unwrap());

        let error = app
            .execute_script_command(ScriptCommand::ExportWorkspaceDirectory(
                "target/workspace-export".into(),
            ))
            .unwrap_err();

        assert!(matches!(
            error,
            ScriptHostError::InvalidCommand { reason }
                if reason.contains("before the document workspace and GPU exist")
        ));
        assert!(app.workspace.is_some());
        assert!(app.undo_stack.is_empty());
        assert!(app.redo_stack.is_empty());
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
    fn script_host_appends_and_deletes_document_layer_nodes() {
        let mut app = App::new(AppRuntimeConfig::default());
        app.workspace = Some(DocumentWorkspace::blank(320, 240).unwrap());
        let root_node = app.workspace.as_ref().unwrap().layer_tree().root_id();

        let ScriptCommandOutcome::DocumentNode(group) = app
            .execute_script_command(ScriptCommand::AppendGroup { parent: root_node })
            .unwrap()
        else {
            panic!("append group should return the new node id");
        };
        let ScriptCommandOutcome::DocumentNode(layer) = app
            .execute_script_command(ScriptCommand::AppendLayer { parent: group })
            .unwrap()
        else {
            panic!("append layer should return the new node id");
        };
        let group_image = app
            .workspace
            .as_ref()
            .unwrap()
            .layer_tree()
            .node(group)
            .unwrap()
            .image();
        let layer_image = app
            .workspace
            .as_ref()
            .unwrap()
            .layer_tree()
            .node(layer)
            .unwrap()
            .image();

        let deleted = app
            .execute_script_command(ScriptCommand::DeleteNode(group))
            .unwrap();

        let workspace = app.workspace.as_ref().unwrap();
        assert_eq!(deleted, ScriptCommandOutcome::RedrawRequested);
        assert!(!workspace.layer_tree().contains_node(group));
        assert!(!workspace.layer_tree().contains_node(layer));
        assert!(workspace.storage().image(group_image).is_none());
        assert!(workspace.storage().image(layer_image).is_none());
        assert!(app.undo_stack.is_empty());
        assert!(app.redo_stack.is_empty());
        assert!(app.frame_scheduler.has_requested_redraw());
    }

    #[test]
    fn script_host_creates_nodes_above_active_node_and_deletes_active_node() {
        let mut app = App::new(AppRuntimeConfig::default());
        app.workspace = Some(DocumentWorkspace::blank(320, 240).unwrap());
        let root_node = app.workspace.as_ref().unwrap().layer_tree().root_id();

        let no_op = app
            .execute_script_command(ScriptCommand::DeleteActiveNode)
            .unwrap();
        let ScriptCommandOutcome::DocumentNode(first) = app
            .execute_script_command(ScriptCommand::CreateLayerAboveActive)
            .unwrap()
        else {
            panic!("create layer should return the new node id");
        };
        let ScriptCommandOutcome::DocumentNode(second) = app
            .execute_script_command(ScriptCommand::CreateLayerAboveActive)
            .unwrap()
        else {
            panic!("create layer should return the new node id");
        };
        app.execute_script_command(ScriptCommand::SetActiveNode(first))
            .unwrap();
        let ScriptCommandOutcome::DocumentNode(group) = app
            .execute_script_command(ScriptCommand::CreateGroupAboveActive)
            .unwrap()
        else {
            panic!("create group should return the new node id");
        };
        let deleted = app
            .execute_script_command(ScriptCommand::DeleteActiveNode)
            .unwrap();

        let workspace = app.workspace.as_ref().unwrap();
        assert_eq!(no_op, ScriptCommandOutcome::None);
        assert_eq!(deleted, ScriptCommandOutcome::RedrawRequested);
        assert_eq!(
            workspace.layer_tree().child_ids(root_node).unwrap(),
            &[first, second]
        );
        assert_eq!(workspace.layer_tree().active_node_id(), root_node);
        assert!(!workspace.layer_tree().contains_node(group));
        assert!(app.undo_stack.is_empty());
        assert!(app.redo_stack.is_empty());
        assert!(app.frame_scheduler.has_requested_redraw());
    }

    #[test]
    fn ui_actions_drive_document_layer_runtime_commands() {
        let mut app = App::new(AppRuntimeConfig::default());
        app.workspace = Some(DocumentWorkspace::blank(320, 240).unwrap());
        let root_node = app.workspace.as_ref().unwrap().layer_tree().root_id();

        let ScriptCommandOutcome::DocumentNode(layer) = app
            .execute_ui_action(UiAction::CreateLayerRequested)
            .unwrap()
        else {
            panic!("create layer action should return the new node id");
        };
        let ScriptCommandOutcome::DocumentNode(group) = app
            .execute_ui_action(UiAction::CreateGroupRequested)
            .unwrap()
        else {
            panic!("create group action should return the new node id");
        };
        let active = app
            .execute_ui_action(UiAction::ActiveNodeChanged(layer))
            .unwrap();
        let opacity = app
            .execute_ui_action(UiAction::NodeOpacityChanged(layer, 0.25))
            .unwrap();
        let blend = app
            .execute_ui_action(UiAction::NodeBlendModeChanged(
                layer,
                DocumentBlendMode::Overlay,
            ))
            .unwrap();
        let deleted = app
            .execute_ui_action(UiAction::DeleteActiveNodeRequested)
            .unwrap();

        let workspace = app.workspace.as_ref().unwrap();
        assert_eq!(active, ScriptCommandOutcome::None);
        assert_eq!(opacity, ScriptCommandOutcome::RedrawRequested);
        assert_eq!(blend, ScriptCommandOutcome::RedrawRequested);
        assert_eq!(deleted, ScriptCommandOutcome::RedrawRequested);
        assert_eq!(workspace.layer_tree().active_node_id(), root_node);
        assert!(!workspace.layer_tree().contains_node(layer));
        assert!(workspace.layer_tree().contains_node(group));
        assert!(app.frame_scheduler.has_requested_redraw());
    }

    #[test]
    fn ui_action_can_select_group_but_stroke_still_requires_paintable_node() {
        let mut app = App::new(AppRuntimeConfig::default());
        app.workspace = Some(DocumentWorkspace::blank(320, 240).unwrap());
        let root_node = app.workspace.as_ref().unwrap().layer_tree().root_id();
        let ScriptCommandOutcome::DocumentNode(layer) = app
            .execute_script_command(ScriptCommand::AppendLayer { parent: root_node })
            .unwrap()
        else {
            panic!("append layer should return the new node id");
        };
        let ScriptCommandOutcome::DocumentNode(group) = app
            .execute_script_command(ScriptCommand::AppendGroup { parent: root_node })
            .unwrap()
        else {
            panic!("append group should return the new node id");
        };
        app.execute_script_command(ScriptCommand::SetActiveNode(layer))
            .unwrap();

        let active = app
            .execute_ui_action(UiAction::ActiveNodeChanged(group))
            .unwrap();
        let stroke =
            app.execute_script_command(ScriptCommand::BeginStroke(canvas_input(0, 1.0, 2.0, 1.0)));

        assert_eq!(active, ScriptCommandOutcome::None);
        assert_eq!(
            app.workspace
                .as_ref()
                .unwrap()
                .layer_tree()
                .active_node_id(),
            group
        );
        assert!(matches!(
            stroke,
            Err(ScriptHostError::InvalidCommand { reason })
                if reason.contains("not paintable")
        ));
        assert!(!app.brush_thread.has_active_stroke());
    }

    #[test]
    fn window_ui_actions_record_visible_layer_trace_actions() {
        let path = trace_path("window-ui-action");
        let mut config = AppRuntimeConfig::default();
        config.trace_config = AppTraceConfig::record(path.clone());
        let mut app = App::try_new(config).unwrap();
        app.workspace = Some(DocumentWorkspace::blank(320, 240).unwrap());

        app.execute_ui_action_from_window(UiAction::CreateLayerRequested);
        app.trace.stop_recording().unwrap().unwrap();

        assert_eq!(
            load_trace_file(&path).unwrap(),
            vec![AppTraceEvent::Ui(AppTraceUiAction::CreateLayer)]
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn ui_trace_control_actions_start_stop_and_replay_default_path() {
        let path = trace_path("ui-trace-control");
        let mut config = AppRuntimeConfig::default();
        config.trace_default_path = path.clone();
        let mut app = App::try_new(config).unwrap();

        app.execute_ui_action_from_window(UiAction::StartRecordingRequested);
        app.execute_ui_action_from_window(UiAction::UndoRequested);
        app.execute_ui_action_from_window(UiAction::StopRecordingRequested);

        let events = load_trace_file(&path).unwrap();
        assert_eq!(events, vec![AppTraceEvent::Ui(AppTraceUiAction::Undo)]);

        app.execute_ui_action(UiAction::ReplayRequested).unwrap();
        assert!(app.process_next_trace_replay_event());
        assert!(!app.process_next_trace_replay_event());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn trace_ui_actions_resolve_visible_layer_indices_during_replay() {
        let mut app = App::new(AppRuntimeConfig::default());
        app.workspace = Some(DocumentWorkspace::blank(320, 240).unwrap());
        app.execute_trace_ui_action(AppTraceUiAction::CreateLayer)
            .unwrap();
        let layer = app
            .workspace
            .as_ref()
            .unwrap()
            .layer_tree()
            .active_node_id();

        app.execute_trace_ui_action(AppTraceUiAction::SetLayerOpacity {
            visible_index: 1,
            opacity: 0.25,
        })
        .unwrap();

        let workspace = app.workspace.as_ref().unwrap();
        assert_eq!(workspace.layer_tree().node(layer).unwrap().opacity(), 0.25);
    }

    #[test]
    fn window_layer_selection_steps_through_visible_layers() {
        let path = trace_path("window-layer-selection");
        let mut config = AppRuntimeConfig::default();
        config.trace_config = AppTraceConfig::record(path.clone());
        let mut app = App::try_new(config).unwrap();
        let mut workspace = DocumentWorkspace::blank(320, 240).unwrap();
        let root = workspace.layer_tree().root_id();
        let first = workspace.append_layer(root).unwrap();
        let second = workspace.append_layer(root).unwrap();
        app.workspace = Some(workspace);

        app.select_visible_layer_from_window(1);
        let active_after_next = app
            .workspace
            .as_ref()
            .unwrap()
            .layer_tree()
            .active_node_id();
        app.select_visible_layer_from_window(-1);
        let active_after_prev = app
            .workspace
            .as_ref()
            .unwrap()
            .layer_tree()
            .active_node_id();
        app.trace.stop_recording().unwrap().unwrap();

        assert_eq!(active_after_next, first);
        assert_eq!(active_after_prev, second);
        assert_eq!(
            load_trace_file(&path).unwrap(),
            vec![
                AppTraceEvent::Ui(AppTraceUiAction::SelectLayer { visible_index: 2 }),
                AppTraceEvent::Ui(AppTraceUiAction::SelectLayer { visible_index: 1 }),
            ]
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn script_host_updates_document_layer_metadata() {
        let mut app = App::new(AppRuntimeConfig::default());
        app.workspace = Some(DocumentWorkspace::blank(320, 240).unwrap());
        let root_node = app.workspace.as_ref().unwrap().layer_tree().root_id();
        let ScriptCommandOutcome::DocumentNode(layer) = app
            .execute_script_command(ScriptCommand::AppendLayer { parent: root_node })
            .unwrap()
        else {
            panic!("append layer should return the new node id");
        };

        let active = app
            .execute_script_command(ScriptCommand::SetActiveNode(layer))
            .unwrap();
        let opacity = app
            .execute_script_command(ScriptCommand::SetNodeOpacity {
                node_id: layer,
                opacity: 0.4,
            })
            .unwrap();
        let blend = app
            .execute_script_command(ScriptCommand::SetNodeBlendMode {
                node_id: layer,
                blend_mode: DocumentBlendMode::Multiply,
            })
            .unwrap();

        let workspace = app.workspace.as_ref().unwrap();
        let node = workspace.layer_tree().node(layer).unwrap();
        assert_eq!(active, ScriptCommandOutcome::None);
        assert_eq!(opacity, ScriptCommandOutcome::RedrawRequested);
        assert_eq!(blend, ScriptCommandOutcome::RedrawRequested);
        assert_eq!(workspace.layer_tree().active_node_id(), layer);
        assert_eq!(node.opacity(), 0.4);
        assert_eq!(node.blend_mode(), DocumentBlendMode::Multiply);
    }

    #[test]
    fn script_host_rejects_stroke_when_active_node_is_group() {
        let mut app = App::new(AppRuntimeConfig::default());
        app.workspace = Some(DocumentWorkspace::blank(320, 240).unwrap());
        let root_node = app.workspace.as_ref().unwrap().layer_tree().root_id();
        let ScriptCommandOutcome::DocumentNode(group) = app
            .execute_script_command(ScriptCommand::AppendGroup { parent: root_node })
            .unwrap()
        else {
            panic!("append group should return the new node id");
        };

        let error = app
            .execute_script_command(ScriptCommand::BeginStroke(canvas_input(0, 1.0, 2.0, 1.0)))
            .unwrap_err();

        assert_eq!(
            app.workspace
                .as_ref()
                .unwrap()
                .layer_tree()
                .active_node_id(),
            group
        );
        assert!(matches!(
            error,
            ScriptHostError::InvalidCommand { reason }
                if reason.contains("not paintable")
        ));
        assert!(!app.brush_thread.has_active_stroke());
    }

    #[test]
    fn script_host_rejects_layer_updates_during_active_stroke() {
        let mut app = App::new(AppRuntimeConfig::default());
        app.workspace = Some(DocumentWorkspace::blank(320, 240).unwrap());
        app.execute_script_command(ScriptCommand::BeginStroke(canvas_input(1, 1.0, 2.0, 1.0)))
            .unwrap();

        let error = app
            .execute_script_command(ScriptCommand::AppendLayer {
                parent: DocumentNodeId::new(1),
            })
            .unwrap_err();

        assert!(matches!(
            error,
            ScriptHostError::InvalidCommand { reason }
                if reason.contains("while a stroke is active")
        ));
        assert!(app.brush_thread.has_active_stroke());
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
