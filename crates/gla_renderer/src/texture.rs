use gla_color::{ChannelCount, ChannelType, GlaFormat};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TextureFormatRuntime {
    pub(crate) texture_format: wgpu::TextureFormat,
    pub(crate) bytes_per_pixel: u32,
}

#[derive(Debug)]
pub(crate) enum TextureResourceError {
    UnsupportedFormat(GlaFormat),
    InvalidExtent { width: u32, height: u32 },
    InvalidLayerCount(u32),
    LayerOutOfBounds { layer: u32, layers: u32 },
}

#[derive(Debug)]
pub(crate) struct RendererTexture {
    pub(crate) texture: wgpu::Texture,
    pub(crate) view: wgpu::TextureView,
    pub(crate) layers: u32,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct RendererTextureDescriptor<'a> {
    pub(crate) label: Option<&'a str>,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) layers: u32,
    pub(crate) format: wgpu::TextureFormat,
    pub(crate) usage: wgpu::TextureUsages,
}

impl RendererTexture {
    pub(crate) fn new(
        device: &wgpu::Device,
        descriptor: &RendererTextureDescriptor<'_>,
    ) -> Result<Self, TextureResourceError> {
        validate_texture_extent(descriptor.width, descriptor.height)?;
        if descriptor.layers == 0 {
            return Err(TextureResourceError::InvalidLayerCount(0));
        }
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: descriptor.label,
            size: wgpu::Extent3d {
                width: descriptor.width,
                height: descriptor.height,
                depth_or_array_layers: descriptor.layers,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: descriptor.format,
            usage: descriptor.usage,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        Ok(Self {
            texture,
            view,
            layers: descriptor.layers,
        })
    }

    pub(crate) fn create_layer_view(
        &self,
        layer: u32,
    ) -> Result<wgpu::TextureView, TextureResourceError> {
        self.validate_layer(layer)?;
        Ok(self.texture.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2),
            base_array_layer: layer,
            array_layer_count: Some(1),
            ..Default::default()
        }))
    }

    pub(crate) fn validate_layer(&self, layer: u32) -> Result<(), TextureResourceError> {
        if layer < self.layers {
            Ok(())
        } else {
            Err(TextureResourceError::LayerOutOfBounds {
                layer,
                layers: self.layers,
            })
        }
    }
}

pub(crate) fn runtime_format(
    format: GlaFormat,
) -> Result<TextureFormatRuntime, TextureResourceError> {
    let (texture_format, bytes_per_pixel) = match (format.channel_count, format.channel_type) {
        (ChannelCount::D1, ChannelType::U8) => (wgpu::TextureFormat::R8Unorm, 1),
        (ChannelCount::D2, ChannelType::U8) => (wgpu::TextureFormat::Rg8Unorm, 2),
        (ChannelCount::D4, ChannelType::U8) => (wgpu::TextureFormat::Rgba8Unorm, 4),
        (ChannelCount::D1, ChannelType::U32) => (wgpu::TextureFormat::R32Uint, 4),
        (ChannelCount::D1, ChannelType::F32) => (wgpu::TextureFormat::R32Float, 4),
        (ChannelCount::D2, ChannelType::F32) => (wgpu::TextureFormat::Rg32Float, 8),
        (ChannelCount::D4, ChannelType::F32) => (wgpu::TextureFormat::Rgba32Float, 16),
        _ => return Err(TextureResourceError::UnsupportedFormat(format)),
    };

    Ok(TextureFormatRuntime {
        texture_format,
        bytes_per_pixel,
    })
}

fn validate_texture_extent(width: u32, height: u32) -> Result<(), TextureResourceError> {
    if width == 0 || height == 0 {
        Err(TextureResourceError::InvalidExtent { width, height })
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::runtime_format;
    use gla_color::{ChannelCount, ChannelType, GlaFormat};

    #[test]
    fn maps_rgba8_gla_format_to_wgpu_format() {
        let runtime = runtime_format(GlaFormat {
            channel_count: ChannelCount::D4,
            channel_type: ChannelType::U8,
        })
        .unwrap();

        assert_eq!(runtime.texture_format, wgpu::TextureFormat::Rgba8Unorm);
        assert_eq!(runtime.bytes_per_pixel, 4);
    }
}
