use std::sync::mpsc;

use glaphica_core::{GUTTER_SIZE, IMAGE_TILE_SIZE, TileKey};
use gpu_runtime::atlas_runtime::AtlasStorageRuntime;
use images::{Image, StoredImage};

#[derive(Debug)]
pub enum LayerImageExportError {
    MissingTileAddress { tile_key: TileKey },
    InvalidOutputSize,
    BufferMap(wgpu::BufferAsyncError),
    MapChannelRecv(mpsc::RecvError),
    StoredImage(images::StoredImageError),
}

impl From<images::StoredImageError> for LayerImageExportError {
    fn from(error: images::StoredImageError) -> Self {
        Self::StoredImage(error)
    }
}

pub struct LayerImageExporter;

impl LayerImageExporter {
    pub fn new() -> Self {
        Self
    }

    pub fn export(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        atlas_storage: &AtlasStorageRuntime,
        image: &Image,
    ) -> Result<StoredImage, LayerImageExportError> {
        let width = image.layout().size_x();
        let height = image.layout().size_y();
        if width == 0 || height == 0 {
            return Err(LayerImageExportError::InvalidOutputSize);
        }

        let bytes_per_pixel = 4usize;
        let width_usize =
            usize::try_from(width).map_err(|_| LayerImageExportError::InvalidOutputSize)?;
        let height_usize =
            usize::try_from(height).map_err(|_| LayerImageExportError::InvalidOutputSize)?;
        let mut pixels = vec![0; width_usize * height_usize * bytes_per_pixel];
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("glaphica-layer-image-export-encoder"),
        });
        let mut readbacks = Vec::new();

        for tile_index in 0..image.tile_count() {
            let Some(tile_key) = image.tile_key(tile_index) else {
                continue;
            };
            if tile_key == TileKey::EMPTY {
                continue;
            }
            let Some(tile_origin) = image.layout().tile_canvas_origin(tile_index) else {
                continue;
            };
            let tile_origin_x = tile_origin.x as u32;
            let tile_origin_y = tile_origin.y as u32;
            let sample_width = (width.saturating_sub(tile_origin_x)).min(IMAGE_TILE_SIZE);
            let sample_height = (height.saturating_sub(tile_origin_y)).min(IMAGE_TILE_SIZE);
            if sample_width == 0 || sample_height == 0 {
                continue;
            }

            let Some(resolved) = atlas_storage.resolve(tile_key) else {
                return Err(LayerImageExportError::MissingTileAddress { tile_key });
            };
            let bytes_per_row = sample_width.saturating_mul(4);
            let padded_bytes_per_row = bytes_per_row.div_ceil(256).saturating_mul(256);
            let buffer_size = u64::from(padded_bytes_per_row) * u64::from(sample_height);
            let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("glaphica-layer-image-export-readback"),
                size: buffer_size,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });

            encoder.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo {
                    texture: resolved.texture2d_array,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: resolved.address.texel_offset.0 + GUTTER_SIZE,
                        y: resolved.address.texel_offset.1 + GUTTER_SIZE,
                        z: resolved.address.layer,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyBufferInfo {
                    buffer: &buffer,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(padded_bytes_per_row),
                        rows_per_image: Some(sample_height),
                    },
                },
                wgpu::Extent3d {
                    width: sample_width,
                    height: sample_height,
                    depth_or_array_layers: 1,
                },
            );

            readbacks.push(TileReadback {
                tile_origin_x,
                tile_origin_y,
                sample_width,
                sample_height,
                padded_bytes_per_row: usize::try_from(padded_bytes_per_row)
                    .map_err(|_| LayerImageExportError::InvalidOutputSize)?,
                buffer,
            });
        }

        queue.submit(Some(encoder.finish()));

        for readback in readbacks {
            let buffer_slice = readback.buffer.slice(..);
            let (sender, receiver) = mpsc::channel();
            buffer_slice.map_async(wgpu::MapMode::Read, move |result| {
                if let Err(send_error) = sender.send(result) {
                    eprintln!("layer image export map callback send failed: {send_error}");
                }
            });
            let _ = device.poll(wgpu::PollType::wait_indefinitely());
            let map_result = receiver
                .recv()
                .map_err(LayerImageExportError::MapChannelRecv)?;
            map_result.map_err(LayerImageExportError::BufferMap)?;

            let mapped = buffer_slice.get_mapped_range();
            scatter_tile_readback(&mut pixels, width_usize, &mapped, &readback)?;
            drop(mapped);
            readback.buffer.unmap();
        }

        StoredImage::new_rgba8(width, height, pixels).map_err(Into::into)
    }
}

impl Default for LayerImageExporter {
    fn default() -> Self {
        Self::new()
    }
}

struct TileReadback {
    tile_origin_x: u32,
    tile_origin_y: u32,
    sample_width: u32,
    sample_height: u32,
    padded_bytes_per_row: usize,
    buffer: wgpu::Buffer,
}

fn scatter_tile_readback(
    dst_pixels: &mut [u8],
    image_width: usize,
    mapped: &[u8],
    readback: &TileReadback,
) -> Result<(), LayerImageExportError> {
    let tile_origin_x = usize::try_from(readback.tile_origin_x)
        .map_err(|_| LayerImageExportError::InvalidOutputSize)?;
    let tile_origin_y = usize::try_from(readback.tile_origin_y)
        .map_err(|_| LayerImageExportError::InvalidOutputSize)?;
    let sample_width = usize::try_from(readback.sample_width)
        .map_err(|_| LayerImageExportError::InvalidOutputSize)?;
    let sample_height = usize::try_from(readback.sample_height)
        .map_err(|_| LayerImageExportError::InvalidOutputSize)?;
    let bytes_per_row = sample_width * 4;

    for row in 0..sample_height {
        let src_start = row * readback.padded_bytes_per_row;
        let src_end = src_start + bytes_per_row;
        let dst_start = ((tile_origin_y + row) * image_width + tile_origin_x) * 4;
        let dst_end = dst_start + bytes_per_row;
        dst_pixels[dst_start..dst_end].copy_from_slice(&mapped[src_start..src_end]);
    }

    Ok(())
}
