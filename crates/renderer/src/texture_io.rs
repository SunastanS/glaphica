use std::error::Error;
use std::fmt::{Display, Formatter};
use std::sync::mpsc;

use atlas::{AtlasLayout, TileKey};
use glaphica_core::{
    ATLAS_TILE_SIZE, AlphaMode, ColorManagement, ColorManagementError, ColorProfile,
    CpuTransformOptions, GUTTER_SIZE, GpuColorTransformUniform, IMAGE_TILE_SIZE,
};

pub struct TextureColorRuntime {
    color_management: ColorManagement,
}

impl TextureColorRuntime {
    pub fn new(color_management: ColorManagement) -> Self {
        Self { color_management }
    }

    pub fn color_management(&self) -> &ColorManagement {
        &self.color_management
    }

    pub fn display_transform_uniform(
        &self,
        destination_profile: &ColorProfile,
    ) -> Result<GpuColorTransformUniform, TextureIoError> {
        self.color_management
            .display_transform(destination_profile)
            .map(|transform| transform.uniform())
            .map_err(TextureIoError::ColorManagement)
    }

    pub fn prepare_upload_rgba8(
        &self,
        pixels_rgba8: &[u8],
        source_profile: ColorProfile,
        alpha_mode: AlphaMode,
    ) -> Result<Vec<u8>, TextureIoError> {
        let mut working_pixels = pixels_rgba8.to_vec();
        let transform = self.color_management.import_transform(
            source_profile,
            CpuTransformOptions {
                alpha_mode,
                ..Default::default()
            },
        );
        transform
            .transform_in_place(&mut working_pixels)
            .map_err(TextureIoError::ColorManagement)?;
        Ok(working_pixels)
    }

    pub fn create_texture_from_rgba8(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        upload: &TextureUploadDescriptor<'_>,
    ) -> Result<RendererTexture, TextureIoError> {
        let texture = RendererTexture::new(device, &upload.texture)?;
        self.upload_rgba8(queue, &texture, upload)?;
        Ok(texture)
    }

    pub fn upload_rgba8(
        &self,
        queue: &wgpu::Queue,
        texture: &RendererTexture,
        upload: &TextureUploadDescriptor<'_>,
    ) -> Result<(), TextureIoError> {
        validate_texture_extent(upload.width, upload.height)?;
        validate_rgba8_buffer(upload.pixels_rgba8.len(), upload.width, upload.height)?;
        texture.validate_layer(upload.layer)?;
        texture.validate_extent(upload.width, upload.height)?;
        texture.validate_rgba8_unorm()?;

        let prepared = self.prepare_upload_rgba8(
            upload.pixels_rgba8,
            upload.source_profile.clone(),
            upload.alpha_mode,
        )?;

        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture.texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: 0,
                    y: 0,
                    z: upload.layer,
                },
                aspect: wgpu::TextureAspect::All,
            },
            &prepared,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(upload.width * 4),
                rows_per_image: Some(upload.height),
            },
            wgpu::Extent3d {
                width: upload.width,
                height: upload.height,
                depth_or_array_layers: 1,
            },
        );

        Ok(())
    }

    pub fn readback_rgba8(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        texture: &RendererTexture,
        layer: u32,
    ) -> Result<TextureReadback, TextureIoError> {
        texture.validate_layer(layer)?;
        texture.validate_rgba8_unorm()?;
        let width = texture.width;
        let height = texture.height;
        validate_texture_extent(width, height)?;
        let bytes_per_row = width
            .checked_mul(4)
            .ok_or(TextureIoError::InvalidExtent { width, height })?;
        let padded_bytes_per_row = bytes_per_row.div_ceil(256) * 256;
        let buffer_size = u64::from(padded_bytes_per_row) * u64::from(height);

        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("glaphica-renderer-readback"),
            size: buffer_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("glaphica-renderer-readback-encoder"),
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &texture.texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: 0,
                    y: 0,
                    z: layer,
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_bytes_per_row),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(Some(encoder.finish()));

        let slice = buffer.slice(..);
        let (sender, receiver) = mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
        let _ = device.poll(wgpu::PollType::wait_indefinitely());
        receiver
            .recv()
            .map_err(TextureIoError::MapChannelRecv)?
            .map_err(TextureIoError::BufferMap)?;

        let mapped = slice.get_mapped_range();
        let mut pixels = vec![0u8; (width as usize) * (height as usize) * 4];
        let copy_bytes_per_row = bytes_per_row as usize;
        let padded_bytes_per_row = padded_bytes_per_row as usize;
        for row in 0..height as usize {
            let src_start = row * padded_bytes_per_row;
            let src_end = src_start + copy_bytes_per_row;
            let dst_start = row * copy_bytes_per_row;
            let dst_end = dst_start + copy_bytes_per_row;
            pixels[dst_start..dst_end].copy_from_slice(&mapped[src_start..src_end]);
        }
        drop(mapped);
        buffer.unmap();

        Ok(TextureReadback {
            width,
            height,
            layer,
            pixels_rgba8: pixels,
        })
    }

    pub fn export_texture_rgba8(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        texture: &RendererTexture,
        layer: u32,
        destination_profile: ColorProfile,
        alpha_mode: AlphaMode,
    ) -> Result<TextureReadback, TextureIoError> {
        let mut readback = self.readback_rgba8(device, queue, texture, layer)?;
        let transform = self.color_management.export_transform(
            destination_profile,
            CpuTransformOptions {
                alpha_mode,
                ..Default::default()
            },
        );
        transform
            .transform_in_place(&mut readback.pixels_rgba8)
            .map_err(TextureIoError::ColorManagement)?;
        Ok(readback)
    }

    pub fn build_tile_image_export_request(
        &self,
        atlas_layout: AtlasLayout,
        image_width: u32,
        image_height: u32,
        image_tiles: &[(usize, TileKey)],
    ) -> Result<TileImageExportRequest, TextureIoError> {
        let mut tiles = Vec::new();
        for &(tile_index, tile_key) in image_tiles {
            if tile_key == TileKey::EMPTY {
                continue;
            }

            let parts = tile_key.parts();
            let slot_address = atlas_layout.slot_address(parts.slot_index).ok_or(
                TextureIoError::AtlasSlotOutOfBounds {
                    slot_index: parts.slot_index,
                    total_slots: atlas_layout.total_slots(),
                },
            )?;
            tiles.push(TileImageExportTile {
                atlas_layer: slot_address.layer,
                atlas_tile_x: slot_address.tile_x,
                atlas_tile_y: slot_address.tile_y,
                image_tile_index: tile_index,
            });
        }

        Ok(TileImageExportRequest {
            image_width,
            image_height,
            tiles,
        })
    }

    pub fn readback_image_tiles_rgba8(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        atlas_texture: &RendererTexture,
        request: &TileImageExportRequest,
        destination_profile: ColorProfile,
        alpha_mode: AlphaMode,
    ) -> Result<Vec<ImageTileReadback>, TextureIoError> {
        let mut layer_readbacks = Vec::new();
        for layer in 0..atlas_texture.layers {
            layer_readbacks.push(self.export_texture_rgba8(
                device,
                queue,
                atlas_texture,
                layer,
                destination_profile.clone(),
                alpha_mode,
            )?);
        }
        extract_tile_readbacks_from_readbacks(request, &layer_readbacks)
    }
}

#[derive(Debug)]
pub struct RendererTexture {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub width: u32,
    pub height: u32,
    pub layers: u32,
    pub format: wgpu::TextureFormat,
}

impl RendererTexture {
    pub fn new(
        device: &wgpu::Device,
        descriptor: &RendererTextureDescriptor<'_>,
    ) -> Result<Self, TextureIoError> {
        validate_texture_extent(descriptor.width, descriptor.height)?;
        if descriptor.layers == 0 {
            return Err(TextureIoError::InvalidLayerCount(0));
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
            width: descriptor.width,
            height: descriptor.height,
            layers: descriptor.layers,
            format: descriptor.format,
        })
    }

    pub fn create_layer_view(&self, layer: u32) -> Result<wgpu::TextureView, TextureIoError> {
        self.validate_layer(layer)?;
        Ok(self.texture.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2),
            base_array_layer: layer,
            array_layer_count: Some(1),
            ..Default::default()
        }))
    }

    fn validate_layer(&self, layer: u32) -> Result<(), TextureIoError> {
        if layer < self.layers {
            Ok(())
        } else {
            Err(TextureIoError::LayerOutOfBounds {
                layer,
                layers: self.layers,
            })
        }
    }

    fn validate_extent(&self, width: u32, height: u32) -> Result<(), TextureIoError> {
        if self.width == width && self.height == height {
            Ok(())
        } else {
            Err(TextureIoError::ExtentMismatch {
                expected_width: self.width,
                expected_height: self.height,
                actual_width: width,
                actual_height: height,
            })
        }
    }

    fn validate_rgba8_unorm(&self) -> Result<(), TextureIoError> {
        if self.format == wgpu::TextureFormat::Rgba8Unorm {
            Ok(())
        } else {
            Err(TextureIoError::UnsupportedTextureFormat(self.format))
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct RendererTextureDescriptor<'a> {
    pub label: Option<&'a str>,
    pub width: u32,
    pub height: u32,
    pub layers: u32,
    pub format: wgpu::TextureFormat,
    pub usage: wgpu::TextureUsages,
}

impl<'a> RendererTextureDescriptor<'a> {
    pub fn atlas_rgba8_unorm(label: Option<&'a str>, width: u32, height: u32, layers: u32) -> Self {
        Self {
            label,
            width,
            height,
            layers,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::RENDER_ATTACHMENT,
        }
    }
}

#[derive(Clone, Debug)]
pub struct TextureUploadDescriptor<'a> {
    pub texture: RendererTextureDescriptor<'a>,
    pub pixels_rgba8: &'a [u8],
    pub width: u32,
    pub height: u32,
    pub layer: u32,
    pub source_profile: ColorProfile,
    pub alpha_mode: AlphaMode,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextureReadback {
    pub width: u32,
    pub height: u32,
    pub layer: u32,
    pub pixels_rgba8: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TileImageExportTile {
    pub atlas_layer: u32,
    pub atlas_tile_x: u32,
    pub atlas_tile_y: u32,
    pub image_tile_index: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TileImageExportRequest {
    pub image_width: u32,
    pub image_height: u32,
    pub tiles: Vec<TileImageExportTile>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageTileReadback {
    pub image_tile_index: usize,
    pub pixels_rgba8: Vec<u8>,
}

#[derive(Debug)]
pub enum TextureIoError {
    InvalidExtent {
        width: u32,
        height: u32,
    },
    InvalidLayerCount(u32),
    LayerOutOfBounds {
        layer: u32,
        layers: u32,
    },
    ExtentMismatch {
        expected_width: u32,
        expected_height: u32,
        actual_width: u32,
        actual_height: u32,
    },
    PixelBufferLengthMismatch {
        expected: usize,
        actual: usize,
    },
    AtlasSlotOutOfBounds {
        slot_index: u32,
        total_slots: u32,
    },
    AtlasLayerReadbackMissing {
        layer: u32,
        available_layers: usize,
    },
    AtlasReadbackExtentMismatch {
        expected_width: u32,
        expected_height: u32,
        actual_width: u32,
        actual_height: u32,
    },
    UnsupportedTextureFormat(wgpu::TextureFormat),
    BufferMap(wgpu::BufferAsyncError),
    MapChannelRecv(mpsc::RecvError),
    ColorManagement(ColorManagementError),
}

impl Display for TextureIoError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidExtent { width, height } => {
                write!(f, "invalid texture extent {width}x{height}")
            }
            Self::InvalidLayerCount(layers) => {
                write!(f, "invalid texture layer count {layers}")
            }
            Self::LayerOutOfBounds { layer, layers } => {
                write!(
                    f,
                    "texture layer {layer} is out of bounds for {layers} layers"
                )
            }
            Self::ExtentMismatch {
                expected_width,
                expected_height,
                actual_width,
                actual_height,
            } => write!(
                f,
                "texture extent mismatch: expected {}x{}, got {}x{}",
                expected_width, expected_height, actual_width, actual_height
            ),
            Self::PixelBufferLengthMismatch { expected, actual } => {
                write!(
                    f,
                    "pixel buffer length mismatch: expected {expected} bytes, got {actual}"
                )
            }
            Self::AtlasSlotOutOfBounds {
                slot_index,
                total_slots,
            } => write!(
                f,
                "atlas slot {slot_index} is out of bounds for layout with {total_slots} slots"
            ),
            Self::AtlasLayerReadbackMissing {
                layer,
                available_layers,
            } => write!(
                f,
                "atlas layer {layer} is unavailable in readback set with {available_layers} layers"
            ),
            Self::AtlasReadbackExtentMismatch {
                expected_width,
                expected_height,
                actual_width,
                actual_height,
            } => write!(
                f,
                "atlas readback extent mismatch: expected {}x{}, got {}x{}",
                expected_width, expected_height, actual_width, actual_height
            ),
            Self::UnsupportedTextureFormat(format) => {
                write!(f, "unsupported texture format for RGBA8 IO: {format:?}")
            }
            Self::BufferMap(error) => write!(f, "buffer map failed: {error}"),
            Self::MapChannelRecv(error) => write!(f, "buffer map channel receive failed: {error}"),
            Self::ColorManagement(error) => write!(f, "{error}"),
        }
    }
}

impl Error for TextureIoError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::BufferMap(error) => Some(error),
            Self::MapChannelRecv(error) => Some(error),
            Self::ColorManagement(error) => Some(error),
            Self::InvalidExtent { .. }
            | Self::InvalidLayerCount(_)
            | Self::LayerOutOfBounds { .. }
            | Self::ExtentMismatch { .. }
            | Self::PixelBufferLengthMismatch { .. }
            | Self::AtlasSlotOutOfBounds { .. }
            | Self::AtlasLayerReadbackMissing { .. }
            | Self::AtlasReadbackExtentMismatch { .. }
            | Self::UnsupportedTextureFormat(_) => None,
        }
    }
}

fn validate_texture_extent(width: u32, height: u32) -> Result<(), TextureIoError> {
    if width == 0 || height == 0 {
        Err(TextureIoError::InvalidExtent { width, height })
    } else {
        Ok(())
    }
}

fn validate_rgba8_buffer(len: usize, width: u32, height: u32) -> Result<(), TextureIoError> {
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or(TextureIoError::InvalidExtent { width, height })?;
    if len == expected {
        Ok(())
    } else {
        Err(TextureIoError::PixelBufferLengthMismatch {
            expected,
            actual: len,
        })
    }
}

fn extract_tile_readbacks_from_readbacks(
    request: &TileImageExportRequest,
    layer_readbacks: &[TextureReadback],
) -> Result<Vec<ImageTileReadback>, TextureIoError> {
    let mut tiles = Vec::with_capacity(request.tiles.len());

    for tile in &request.tiles {
        let layer_readback = layer_readbacks.get(tile.atlas_layer as usize).ok_or(
            TextureIoError::AtlasLayerReadbackMissing {
                layer: tile.atlas_layer,
                available_layers: layer_readbacks.len(),
            },
        )?;
        if layer_readback.width == 0 || layer_readback.height == 0 {
            return Err(TextureIoError::AtlasReadbackExtentMismatch {
                expected_width: ATLAS_TILE_SIZE,
                expected_height: ATLAS_TILE_SIZE,
                actual_width: layer_readback.width,
                actual_height: layer_readback.height,
            });
        }
        let mut pixels_rgba8 = vec![0; (IMAGE_TILE_SIZE * IMAGE_TILE_SIZE * 4) as usize];
        let copy_width = tile_copy_width(request, tile.image_tile_index);
        let copy_height = tile_copy_height(request, tile.image_tile_index);

        for row in 0..copy_height {
            let src_y = (tile.atlas_tile_y * ATLAS_TILE_SIZE + GUTTER_SIZE) as usize + row;
            let src_x = (tile.atlas_tile_x * ATLAS_TILE_SIZE + GUTTER_SIZE) as usize;
            let src_start = (src_y * layer_readback.width as usize + src_x) * 4;
            let src_end = src_start + copy_width * 4;
            let dst_start = row * IMAGE_TILE_SIZE as usize * 4;
            let dst_end = dst_start + copy_width * 4;
            pixels_rgba8[dst_start..dst_end]
                .copy_from_slice(&layer_readback.pixels_rgba8[src_start..src_end]);
        }

        tiles.push(ImageTileReadback {
            image_tile_index: tile.image_tile_index,
            pixels_rgba8,
        });
    }

    Ok(tiles)
}

fn request_width_in_tiles(request: &TileImageExportRequest) -> usize {
    request.image_width.div_ceil(IMAGE_TILE_SIZE) as usize
}

fn tile_copy_width(request: &TileImageExportRequest, image_tile_index: usize) -> usize {
    let tile_origin_x =
        (image_tile_index % request_width_in_tiles(request)) * IMAGE_TILE_SIZE as usize;
    request
        .image_width
        .saturating_sub(tile_origin_x as u32)
        .min(IMAGE_TILE_SIZE) as usize
}

fn tile_copy_height(request: &TileImageExportRequest, image_tile_index: usize) -> usize {
    let tile_origin_y =
        (image_tile_index / request_width_in_tiles(request)) * IMAGE_TILE_SIZE as usize;
    request
        .image_height
        .saturating_sub(tile_origin_y as u32)
        .min(IMAGE_TILE_SIZE) as usize
}

#[cfg(test)]
mod tests {
    use super::*;
    use atlas::{AtlasLayout, Backend, BackendId};
    use glaphica_core::{ColorProfile, IMAGE_TILE_SIZE};

    #[test]
    fn prepare_upload_applies_import_transform() {
        let runtime = TextureColorRuntime::new(ColorManagement::new(ColorProfile::linear_srgb()));
        let prepared = runtime
            .prepare_upload_rgba8(
                &[128, 128, 128, 255],
                ColorProfile::srgb(),
                AlphaMode::Straight,
            )
            .unwrap();
        assert!(prepared[0] < 80);
        assert_eq!(prepared[3], 255);
    }

    #[test]
    fn rgba8_descriptor_uses_expected_defaults() {
        let descriptor = RendererTextureDescriptor::atlas_rgba8_unorm(Some("tex"), 64, 32, 7);
        assert_eq!(descriptor.format, wgpu::TextureFormat::Rgba8Unorm);
        assert_eq!(descriptor.layers, 7);
        assert!(descriptor.usage.contains(wgpu::TextureUsages::COPY_DST));
        assert!(descriptor.usage.contains(wgpu::TextureUsages::COPY_SRC));
        assert!(
            descriptor
                .usage
                .contains(wgpu::TextureUsages::TEXTURE_BINDING)
        );
        assert!(
            descriptor
                .usage
                .contains(wgpu::TextureUsages::RENDER_ATTACHMENT)
        );
    }

    #[test]
    fn atlas_readback_request_converts_tile_keys_into_offsets() {
        let runtime = TextureColorRuntime::new(ColorManagement::new(ColorProfile::linear_srgb()));
        let atlas_layout = AtlasLayout::Tiny8;
        let backend = Backend::new(atlas_layout, BackendId::new(0));
        let first_owner = backend.alloc_active().expect("first tile should allocate");
        let second_owner = backend.alloc_active().expect("second tile should allocate");
        let image_tiles = vec![
            (0usize, first_owner.tile_key()),
            (1usize, second_owner.tile_key()),
        ];

        let request = runtime
            .build_tile_image_export_request(
                atlas_layout,
                IMAGE_TILE_SIZE * 2,
                IMAGE_TILE_SIZE,
                &image_tiles,
            )
            .expect("request should build");

        assert_eq!(request.image_width, IMAGE_TILE_SIZE * 2);
        assert_eq!(request.image_height, IMAGE_TILE_SIZE);
        assert_eq!(request.tiles.len(), 2);
        assert_eq!(
            request.tiles[0],
            TileImageExportTile {
                atlas_layer: 0,
                atlas_tile_x: 0,
                atlas_tile_y: 0,
                image_tile_index: 0,
            }
        );
        assert_eq!(
            request.tiles[1],
            TileImageExportTile {
                atlas_layer: 0,
                atlas_tile_x: 1,
                atlas_tile_y: 0,
                image_tile_index: 1,
            }
        );
    }

    #[test]
    fn extract_tile_readbacks_returns_raw_tile_pixels() {
        let request = TileImageExportRequest {
            image_width: IMAGE_TILE_SIZE + 1,
            image_height: IMAGE_TILE_SIZE,
            tiles: vec![
                TileImageExportTile {
                    atlas_layer: 0,
                    atlas_tile_x: 0,
                    atlas_tile_y: 0,
                    image_tile_index: 0,
                },
                TileImageExportTile {
                    atlas_layer: 1,
                    atlas_tile_x: 0,
                    atlas_tile_y: 0,
                    image_tile_index: 1,
                },
            ],
        };
        let atlas_width = ATLAS_TILE_SIZE * 2;
        let atlas_height = ATLAS_TILE_SIZE;
        let mut first_layer = vec![0; (atlas_width * atlas_height * 4) as usize];
        let mut second_layer = vec![0; (atlas_width * atlas_height * 4) as usize];
        write_test_pixel(
            &mut first_layer,
            atlas_width,
            GUTTER_SIZE,
            GUTTER_SIZE,
            [1, 2, 3, 4],
        );
        write_test_pixel(
            &mut second_layer,
            atlas_width,
            GUTTER_SIZE,
            GUTTER_SIZE,
            [9, 8, 7, 6],
        );

        let tiles = extract_tile_readbacks_from_readbacks(
            &request,
            &[
                TextureReadback {
                    width: atlas_width,
                    height: atlas_height,
                    layer: 0,
                    pixels_rgba8: first_layer,
                },
                TextureReadback {
                    width: atlas_width,
                    height: atlas_height,
                    layer: 1,
                    pixels_rgba8: second_layer,
                },
            ],
        )
        .expect("tile readbacks should extract");

        assert_eq!(tiles.len(), 2);
        assert_eq!(tiles[0].image_tile_index, 0);
        assert_eq!(&tiles[0].pixels_rgba8[..4], &[1, 2, 3, 4]);
        assert_eq!(tiles[1].image_tile_index, 1);
        assert_eq!(&tiles[1].pixels_rgba8[..4], &[9, 8, 7, 6]);
        assert!(tiles[1].pixels_rgba8[4..].iter().all(|value| *value == 0));
    }

    fn write_test_pixel(pixels: &mut [u8], width: u32, x: u32, y: u32, rgba: [u8; 4]) {
        let offset = ((y * width + x) * 4) as usize;
        pixels[offset..offset + 4].copy_from_slice(&rgba);
    }
}
