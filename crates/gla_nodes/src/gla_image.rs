use crate::{GlaImageLayout, gla_layout::GlaLayoutError};
use tile_key::{TileKey, TilesError, TilesSession};

#[derive(Debug, Clone)]
pub struct GlaImage {
    pub layout: GlaImageLayout,
    pub atlas_id: u8,
    pub tiles: Box<[TileKey]>,
}

pub enum GlaImageError {
    InvalidTileIndex { index: usize },
    RuntimeError { source: TilesError },
}

impl GlaImage {
    pub fn new_empty(
        tile_session: &mut TilesSession,
        layout: GlaImageLayout,
        atlas_id: u8,
    ) -> Result<Self, GlaImageError> {
        let tiles = tile_session.alloc_batch_from(atlas_id, layout.total_tiles() as u32)?;
        Ok(Self {
            layout,
            atlas_id,
            tiles: tiles.into_boxed_slice(),
        })
    }

    pub fn get_key(&self, index: usize) -> Result<TileKey, GlaImageError> {
        if index >= self.tiles.len() {
            return Err(GlaImageError::InvalidTileIndex { index });
        }
        Ok(self.tiles[index])
    }

    pub fn set_key(&mut self, index: usize, key: TileKey) -> Result<(), GlaImageError> {
        if index >= self.tiles.len() {
            return Err(GlaImageError::InvalidTileIndex { index });
        }
        self.tiles[index] = key;
        Ok(())
    }
}

impl From<GlaLayoutError> for GlaImageError {
    fn from(error: GlaLayoutError) -> Self {
        match error {
            GlaLayoutError::InvalidTileIndex { index } => GlaImageError::InvalidTileIndex { index },
        }
    }
}

impl From<TilesError> for GlaImageError {
    fn from(error: TilesError) -> Self {
        GlaImageError::RuntimeError { source: error }
    }
}
