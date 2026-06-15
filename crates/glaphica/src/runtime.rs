use std::error::Error;
use std::fmt::{Display, Formatter};
use std::sync::Arc;

use gla_ir::DrawOnToolKind;
use gla_renderer::{GpuRenderer, GpuRendererError};
use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
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
    window: Option<Arc<Window>>,
    gpu: Option<GpuCtx>,
}

impl App {
    fn new(config: AppRuntimeConfig) -> Self {
        Self {
            config,
            workspace: None,
            window: None,
            gpu: None,
        }
    }

    fn window_attributes(&self) -> WindowAttributes {
        WindowAttributes::default().with_title(self.config.window_title.clone())
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

    fn render(&mut self) {
        let _ = self.renderer_mut().capabilities();
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
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("glaphica-clear-frame-encoder"),
            });

        {
            encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("glaphica-clear-frame-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(self.clear_color),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
        }

        self.queue.submit(Some(encoder.finish()));
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
        let Some(window) = self.window.as_ref() else {
            return;
        };

        if window.id() != window_id {
            return;
        }

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(gpu) = self.gpu.as_mut() {
                    gpu.resize(size.width, size.height);
                }
            }
            WindowEvent::RedrawRequested => {
                if let Some(gpu) = self.gpu.as_mut() {
                    gpu.render();
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
    }
}
