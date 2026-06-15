use std::error::Error;
use std::fmt::{Display, Formatter};
use std::sync::Arc;

use gla_color::PremultipliedRgbaF32;
use gla_ir::DrawOnToolKind;
use gla_renderer::{GpuRenderer, GpuRendererError, PresentTarget};
use gla_session::DrawHistory;
use winit::{
    application::ApplicationHandler,
    event::{ElementState, MouseButton, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoop},
    window::{Window, WindowAttributes, WindowId},
};

use crate::{
    DEFAULT_CANVAS_HEIGHT_PX, DEFAULT_CANVAS_WIDTH_PX, DocumentWorkspace,
    DocumentWorkspaceBuildError,
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
    history: DrawHistory,
    primary_down: bool,
    last_cursor_pos: Option<(f32, f32)>,
    window: Option<Arc<Window>>,
    gpu: Option<GpuCtx>,
}

impl App {
    fn new(config: AppRuntimeConfig) -> Self {
        Self {
            config,
            workspace: None,
            history: DrawHistory::new(),
            primary_down: false,
            last_cursor_pos: None,
            window: None,
            gpu: None,
        }
    }

    fn window_attributes(&self) -> WindowAttributes {
        WindowAttributes::default().with_title(self.config.window_title.clone())
    }

    fn paint_at_last_cursor(&mut self) {
        let Some((x, y)) = self.last_cursor_pos else {
            return;
        };
        self.paint_at(x, y);
    }

    fn paint_at(&mut self, x: f32, y: f32) {
        let (Some(workspace), Some(gpu)) = (self.workspace.as_mut(), self.gpu.as_mut()) else {
            return;
        };

        if let Err(error) = workspace.replace_circle_on_root(
            &mut self.history,
            gpu.renderer_mut(),
            x,
            y,
            self.config.brush_radius_px,
            self.config.brush_color,
        ) {
            eprintln!("stroke failed: {error}");
        }
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
    Document(DocumentWorkspaceBuildError<GpuRendererError>),
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
        let workspace = DocumentWorkspace::blank_with_textures(
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

    fn render(&mut self, workspace: Option<&DocumentWorkspace>) {
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

        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor {
            format: Some(self.config.format),
            ..Default::default()
        });

        let tiles = match workspace {
            Some(workspace) => match workspace.root_present_tiles() {
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
                view: &view,
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
            WindowEvent::CursorMoved { position, .. } => {
                let pos = (finite_f64_to_f32(position.x), finite_f64_to_f32(position.y));
                self.last_cursor_pos = Some(pos);
                if self.primary_down {
                    self.paint_at(pos.0, pos.1);
                    window.request_redraw();
                }
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => {
                self.primary_down = state == ElementState::Pressed;
                if self.primary_down {
                    self.paint_at_last_cursor();
                    window.request_redraw();
                }
            }
            WindowEvent::Resized(size) => {
                if let Some(gpu) = self.gpu.as_mut() {
                    gpu.resize(size.width, size.height);
                }
            }
            WindowEvent::RedrawRequested => {
                if let Some(gpu) = self.gpu.as_mut() {
                    gpu.render(self.workspace.as_ref());
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }
}

fn finite_f64_to_f32(value: f64) -> f32 {
    if value.is_finite() { value as f32 } else { 0.0 }
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
}
