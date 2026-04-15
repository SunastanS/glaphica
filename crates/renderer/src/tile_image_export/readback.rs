use glaphica_core::{ATLAS_TILE_SIZE, AlphaMode, ColorProfile, GUTTER_SIZE, IMAGE_TILE_SIZE};

use crate::texture_io::{RendererTexture, TextureColorRuntime, TextureIoError, TextureReadback};
use crate::tile_image_export::plan::TileImageExportRequest;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageTileReadback {
    pub image_tile_index: usize,
    pub pixels_rgba8: Vec<u8>,
}

pub fn readback_image_tiles_rgba8(
    runtime: &TextureColorRuntime,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    atlas_texture: &RendererTexture,
    request: &TileImageExportRequest,
    destination_profile: ColorProfile,
    alpha_mode: AlphaMode,
) -> Result<Vec<ImageTileReadback>, TextureIoError> {
    let mut layer_readbacks = Vec::new();
    for layer in 0..atlas_texture.layers {
        layer_readbacks.push(runtime.export_texture_rgba8(
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
    use glaphica_core::IMAGE_TILE_SIZE;

    use super::*;
    use crate::tile_image_export::plan::TileImageExportTile;

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
