use atlas::{AtlasLayout, TileKey};

use crate::texture_io::TextureIoError;

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

impl TileImageExportRequest {
    pub fn from_image_tiles(
        atlas_layout: AtlasLayout,
        image_width: u32,
        image_height: u32,
        image_tiles: &[(usize, TileKey)],
    ) -> Result<Self, TextureIoError> {
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

        Ok(Self {
            image_width,
            image_height,
            tiles,
        })
    }
}

#[cfg(test)]
mod tests {
    use atlas::{AtlasLayout, Backend, BackendId};
    use glaphica_core::IMAGE_TILE_SIZE;

    use super::*;

    #[test]
    fn atlas_readback_request_converts_tile_keys_into_offsets() {
        let atlas_layout = AtlasLayout::Tiny8;
        let backend = Backend::new(atlas_layout, BackendId::new(0));
        let first_owner = backend.alloc_active().expect("first tile should allocate");
        let second_owner = backend.alloc_active().expect("second tile should allocate");
        let image_tiles = vec![
            (0usize, first_owner.tile_key()),
            (1usize, second_owner.tile_key()),
        ];

        let request = TileImageExportRequest::from_image_tiles(
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
}
