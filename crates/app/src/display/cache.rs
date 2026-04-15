use std::error::Error;
use std::fmt::{Display, Formatter};

use renderer::{RendererTexture, RendererTextureDescriptor, TextureIoError};

pub struct ScreenPresentCache {
    texture: RendererTexture,
}

#[derive(Debug)]
pub enum ScreenPresentCacheError {
    TextureIo(TextureIoError),
}

impl Display for ScreenPresentCacheError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TextureIo(error) => Display::fmt(error, f),
        }
    }
}

impl Error for ScreenPresentCacheError {}

impl From<TextureIoError> for ScreenPresentCacheError {
    fn from(error: TextureIoError) -> Self {
        Self::TextureIo(error)
    }
}

impl ScreenPresentCache {
    pub fn new(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        width: u32,
        height: u32,
    ) -> Result<Self, ScreenPresentCacheError> {
        Ok(Self {
            texture: RendererTexture::new(
                device,
                &RendererTextureDescriptor {
                    label: Some("glaphica-screen-present-cache"),
                    width,
                    height,
                    layers: 1,
                    format,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING
                        | wgpu::TextureUsages::COPY_SRC
                        | wgpu::TextureUsages::COPY_DST
                        | wgpu::TextureUsages::RENDER_ATTACHMENT,
                },
            )?,
        })
    }

    pub fn resize(
        &mut self,
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        width: u32,
        height: u32,
    ) -> Result<(), ScreenPresentCacheError> {
        if self.texture.width == width && self.texture.height == height && self.texture.format == format {
            return Ok(());
        }
        *self = Self::new(device, format, width, height)?;
        Ok(())
    }

    pub fn texture(&self) -> &RendererTexture {
        &self.texture
    }
}
