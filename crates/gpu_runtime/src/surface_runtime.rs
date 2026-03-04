use std::error::Error;
use std::fmt::{Display, Formatter};

#[derive(Debug)]
pub enum SurfaceError {
    CreateSurface(wgpu::CreateSurfaceError),
    UnsupportedFormat,
    AcquireFailed,
}

impl Display for SurfaceError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CreateSurface(e) => write!(f, "failed to create surface: {e}"),
            Self::UnsupportedFormat => write!(f, "no supported surface format"),
            Self::AcquireFailed => write!(f, "failed to acquire surface texture"),
        }
    }
}

impl Error for SurfaceError {}

pub struct SurfaceFrame {
    pub texture: wgpu::SurfaceTexture,
    pub view: wgpu::TextureView,
}

pub struct SurfaceRuntime {
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
}

impl SurfaceRuntime {
    pub fn new(
        surface: wgpu::Surface<'static>,
        adapter: &wgpu::Adapter,
        device: &wgpu::Device,
        width: u32,
        height: u32,
    ) -> Result<Self, SurfaceError> {
        let caps = surface.get_capabilities(adapter);

        let format = caps
            .formats
            .iter()
            .find(|f| {
                matches!(
                    f,
                    wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Rgba8Unorm
                )
            })
            .copied()
            .ok_or(SurfaceError::UnsupportedFormat)?;

        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width,
            height,
            present_mode: wgpu::PresentMode::AutoVsync,
            alpha_mode: wgpu::CompositeAlphaMode::Auto,
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };

        surface.configure(device, &config);

        Ok(Self { surface, config })
    }

    pub fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        if width > 0 && height > 0 && (self.config.width != width || self.config.height != height) {
            self.config.width = width;
            self.config.height = height;
            self.surface.configure(device, &self.config);
        }
    }

    pub fn format(&self) -> wgpu::TextureFormat {
        self.config.format
    }

    pub fn width(&self) -> u32 {
        self.config.width
    }

    pub fn height(&self) -> u32 {
        self.config.height
    }

    pub fn acquire_frame(&mut self) -> Result<SurfaceFrame, SurfaceError> {
        let texture = self
            .surface
            .get_current_texture()
            .map_err(|_| SurfaceError::AcquireFailed)?;

        let view = texture.texture.create_view(&wgpu::TextureViewDescriptor {
            format: Some(self.config.format),
            ..Default::default()
        });

        Ok(SurfaceFrame { texture, view })
    }

    pub fn present(frame: SurfaceFrame) {
        frame.texture.present();
    }
}
