use std::error::Error;
use std::fmt::{Display, Formatter};

#[derive(Debug)]
pub enum SurfaceError {
    UnsupportedFormat,
    Acquire(wgpu::SurfaceError),
}

impl Display for SurfaceError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedFormat => f.write_str("surface has no supported presentation format"),
            Self::Acquire(error) => write!(f, "failed to acquire surface frame: {error}"),
        }
    }
}

impl Error for SurfaceError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::UnsupportedFormat => None,
            Self::Acquire(error) => Some(error),
        }
    }
}

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
            .find(|format| {
                matches!(
                    format,
                    wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Rgba8Unorm
                )
            })
            .copied()
            .or_else(|| caps.formats.first().copied())
            .ok_or(SurfaceError::UnsupportedFormat)?;

        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: width.max(1),
            height: height.max(1),
            present_mode: wgpu::PresentMode::AutoVsync,
            alpha_mode: wgpu::CompositeAlphaMode::Auto,
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(device, &config);
        Ok(Self { surface, config })
    }

    pub fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        if self.config.width == width && self.config.height == height {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(device, &self.config);
    }

    pub fn size_px(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
    }

    pub fn width(&self) -> u32 {
        self.config.width
    }

    pub fn height(&self) -> u32 {
        self.config.height
    }

    pub fn format(&self) -> wgpu::TextureFormat {
        self.config.format
    }

    pub fn acquire_frame(&mut self, device: &wgpu::Device) -> Result<SurfaceFrame, SurfaceError> {
        let texture = match self.surface.get_current_texture() {
            Ok(texture) => texture,
            Err(error) => {
                if matches!(
                    error,
                    wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated
                ) {
                    self.surface.configure(device, &self.config);
                }
                return Err(SurfaceError::Acquire(error));
            }
        };
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

#[cfg(test)]
mod tests {
    use super::SurfaceError;

    #[test]
    fn unsupported_surface_format_error_is_human_readable() {
        assert_eq!(
            SurfaceError::UnsupportedFormat.to_string(),
            "surface has no supported presentation format"
        );
    }
}
