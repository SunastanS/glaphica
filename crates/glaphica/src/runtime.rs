use std::error::Error;
use std::fmt::{Display, Formatter};
use std::sync::Arc;

use gla_color::PremultipliedRgbaF32;
use gla_core::ScreenCoordF;
use gla_ir::DrawOnToolKind;
use gla_renderer::{GpuRenderer, GpuRendererError, PresentTarget};
use gla_session::{DrawHistory, DrawRecordId};
use winit::{
    application::ApplicationHandler,
    event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoop},
    keyboard::{Key, ModifiersState},
    window::{Window, WindowAttributes, WindowId},
};

use crate::{
    AppView, AppViewMatrixError, DEFAULT_CANVAS_HEIGHT_PX, DEFAULT_CANVAS_WIDTH_PX,
    DocumentWorkspace, DocumentWorkspaceInitError, ReplaceCircleStrokeSample,
    frame::AppFrameScheduler,
};

#[derive(Debug, Clone)]
pub struct AppRuntimeConfig {
    pub window_title: String,
    pub clear_color: wgpu::Color,
    pub canvas_width_px: u32,
    pub canvas_height_px: u32,
    pub draw_on_tools: Vec<DrawOnToolKind>,
    pub brush_radius_px: f32,
    pub brush_color: PremultipliedRgbaF32,
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
            draw_on_tools: vec![DrawOnToolKind::ReplaceCircle4D],
            brush_radius_px: 10.0,
            brush_color: PremultipliedRgbaF32::new(0.95, 0.17, 0.10, 1.0),
        }
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
    active_stroke: Option<ActiveRootStroke>,
    primary_down: bool,
    middle_down: bool,
    middle_pan_last_pos: Option<ScreenCoordF>,
    last_cursor_pos: Option<ScreenCoordF>,
    modifiers: ModifiersState,
    window: Option<Arc<Window>>,
    gpu: Option<GpuCtx>,
}

impl App {
    fn new(config: AppRuntimeConfig) -> Self {
        Self {
            config,
            workspace: None,
            view: AppView::identity(),
            history: DrawHistory::new(),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            frame_scheduler: AppFrameScheduler::new(),
            active_stroke: None,
            primary_down: false,
            middle_down: false,
            middle_pan_last_pos: None,
            last_cursor_pos: None,
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

    fn begin_stroke_at_last_cursor(&mut self) {
        let Some(position) = self.last_cursor_pos else {
            return;
        };
        self.begin_stroke_at(position);
    }

    fn begin_stroke_at(&mut self, position: ScreenCoordF) {
        self.active_stroke = Some(ActiveRootStroke::default());
        self.push_stroke_sample(position);
    }

    fn continue_stroke_at(&mut self, position: ScreenCoordF) {
        if self.active_stroke.is_some() {
            self.push_stroke_sample(position);
        } else {
            self.begin_stroke_at(position);
        }
    }

    fn push_stroke_sample(&mut self, position: ScreenCoordF) {
        let sample = self.stroke_sample_from_screen(position);
        if let Some(stroke) = self.active_stroke.as_mut() {
            stroke.push(sample);
        }
    }

    fn stroke_sample_from_screen(&self, position: ScreenCoordF) -> ReplaceCircleStrokeSample {
        let canvas = self.view.screen_to_document_point(position);
        ReplaceCircleStrokeSample {
            center: canvas,
            radius_px: self.config.brush_radius_px,
            color: self.config.brush_color,
        }
    }

    fn commit_active_stroke(&mut self) -> bool {
        let Some(stroke) = self.active_stroke.take() else {
            return false;
        };
        if stroke.samples.is_empty() {
            return false;
        }
        let (Some(workspace), Some(gpu)) = (self.workspace.as_mut(), self.gpu.as_mut()) else {
            self.active_stroke = Some(stroke);
            return false;
        };
        let samples = stroke.samples;

        match workspace.replace_circle_stroke_on_root(
            &mut self.history,
            gpu.renderer_mut(),
            samples.iter().copied(),
        ) {
            Ok(Some(commit)) => {
                self.undo_stack.push(commit.record_id);
                self.redo_stack.clear();
                true
            }
            Ok(None) => false,
            Err(error) => {
                eprintln!("stroke failed: {error}");
                self.active_stroke = Some(ActiveRootStroke { samples });
                false
            }
        }
    }

    fn undo(&mut self) -> bool {
        let Some(record_id) = self.undo_stack.pop() else {
            return false;
        };
        match self.apply_history_record(record_id) {
            Some(redo_record) => {
                self.redo_stack.push(redo_record);
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
            Some(undo_record) => {
                self.undo_stack.push(undo_record);
                true
            }
            None => {
                self.redo_stack.push(record_id);
                false
            }
        }
    }

    fn apply_history_record(&mut self, record_id: DrawRecordId) -> Option<DrawRecordId> {
        let (Some(workspace), Some(gpu)) = (self.workspace.as_mut(), self.gpu.as_mut()) else {
            return None;
        };
        match workspace.apply_draw_record(&mut self.history, gpu.renderer_mut(), record_id) {
            Ok(next_record) => Some(next_record),
            Err(error) => {
                eprintln!("history apply failed: {error}");
                None
            }
        }
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

#[derive(Debug, Default)]
struct ActiveRootStroke {
    samples: Vec<ReplaceCircleStrokeSample>,
}

impl ActiveRootStroke {
    fn push(&mut self, sample: ReplaceCircleStrokeSample) {
        self.samples.push(sample);
    }
}

struct GpuCtx {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    clear_color: wgpu::Color,
    renderer: GpuRenderer,
}

#[derive(Debug)]
enum GpuInitError {
    CreateSurface(wgpu::CreateSurfaceError),
    Document(DocumentWorkspaceInitError<GpuRendererError>),
    Renderer(GpuRendererError),
    RequestAdapter(wgpu::RequestAdapterError),
    RequestDevice(wgpu::RequestDeviceError),
    UnsupportedSurfaceFormat,
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
            Self::UnsupportedSurfaceFormat => f.write_str("surface has no supported format"),
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
            Self::UnsupportedSurfaceFormat => None,
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

        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .find(|format| {
                matches!(
                    format,
                    wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Rgba8Unorm
                )
            })
            .copied()
            .or_else(|| caps.formats.first().copied())
            .ok_or(GpuInitError::UnsupportedSurfaceFormat)?;

        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::AutoVsync,
            alpha_mode: wgpu::CompositeAlphaMode::Auto,
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };

        surface.configure(&device, &config);

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
                config,
                clear_color: app_config.clear_color,
                renderer,
            },
            workspace,
        ))
    }

    fn renderer_mut(&mut self) -> &mut GpuRenderer {
        &mut self.renderer
    }

    fn surface_size_px(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
    }

    fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        if self.config.width == width && self.config.height == height {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
    }

    fn render(&mut self, workspace: Option<&DocumentWorkspace>, view: &AppView) {
        let frame = match self.surface.get_current_texture() {
            Ok(frame) => frame,
            Err(error) => {
                if matches!(
                    error,
                    wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated
                ) {
                    self.surface.configure(&self.device, &self.config);
                }
                return;
            }
        };

        let surface_view = frame.texture.create_view(&wgpu::TextureViewDescriptor {
            format: Some(self.config.format),
            ..Default::default()
        });

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
                view: &surface_view,
                format: self.config.format,
                width: self.config.width,
                height: self.config.height,
                clear_color: self.clear_color,
            },
        ) {
            eprintln!("surface present failed: {error}");
        }
        frame.present();
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
        self.request_redraw();
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
                    self.request_redraw();
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
                    self.request_redraw();
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state == ElementState::Pressed
                    && !event.repeat
                    && let Key::Character(value) = &event.logical_key
                {
                    if self.modifiers.control_key()
                        && self.modifiers.shift_key()
                        && value.eq_ignore_ascii_case("z")
                    {
                        if self.redo() {
                            self.request_redraw();
                        }
                    } else if self.modifiers.control_key() && value.eq_ignore_ascii_case("z") {
                        if self.undo() {
                            self.request_redraw();
                        }
                    } else if self.modifiers.control_key() && value.eq_ignore_ascii_case("y") {
                        if self.redo() {
                            self.request_redraw();
                        }
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
                self.request_redraw();
            }
            WindowEvent::RedrawRequested => {
                if let Some(gpu) = self.gpu.as_mut() {
                    gpu.render(self.workspace.as_ref(), &self.view);
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
    use super::AppRuntimeConfig;
    use crate::{DEFAULT_CANVAS_HEIGHT_PX, DEFAULT_CANVAS_WIDTH_PX};
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
        assert_eq!(config.draw_on_tools, vec![DrawOnToolKind::ReplaceCircle4D]);
        assert_eq!(config.brush_radius_px, 10.0);
        assert_eq!(
            config.brush_color,
            gla_color::PremultipliedRgbaF32::new(0.95, 0.17, 0.10, 1.0)
        );
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
