use std::error::Error;
use std::fmt::{Display, Formatter};

use glaphica_core::{BackendId, CanvasVec2, TileKey};

use crate::layout::ImageLayout;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageCreateError {
    TooManyTiles,
}

impl Display for ImageCreateError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooManyTiles => write!(f, "image has too many tiles for this platform"),
        }
    }
}

impl Error for ImageCreateError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageTileAccessError {
    OutOfBounds,
}

impl Display for ImageTileAccessError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OutOfBounds => write!(f, "tile index is out of bounds"),
        }
    }
}

impl Error for ImageTileAccessError {}

pub struct Image {
    layout: ImageLayout,
    tile_keys: Box<[TileKey]>,
    backend: BackendId,
}

impl Image {
    pub fn new(layout: ImageLayout, backend: BackendId) -> Result<Self, ImageCreateError> {
        let total_tiles =
            usize::try_from(layout.total_tiles()).map_err(|_| ImageCreateError::TooManyTiles)?;
        let tile_keys = vec![TileKey::EMPTY; total_tiles].into_boxed_slice();
        Ok(Self {
            layout,
            tile_keys,
            backend,
        })
    }

    pub fn backend(&self) -> BackendId {
        self.backend
    }

    pub fn layout(&self) -> &ImageLayout {
        &self.layout
    }

    pub fn tile_keys(&self) -> &[TileKey] {
        &self.tile_keys
    }

    pub fn tile_key(&self, tile_index: usize) -> Option<TileKey> {
        self.tile_keys.get(tile_index).copied()
    }

    pub fn set_tile_key(
        &mut self,
        tile_index: usize,
        tile_key: TileKey,
    ) -> Result<(), ImageTileAccessError> {
        let Some(slot) = self.tile_keys.get_mut(tile_index) else {
            return Err(ImageTileAccessError::OutOfBounds);
        };
        *slot = tile_key;
        Ok(())
    }

    pub fn for_each_affected_tile_key<F>(
        &self,
        center: CanvasVec2,
        max_affected_radius_px: u32,
        mut visit: F,
    ) where
        F: FnMut(usize, TileKey),
    {
        self.layout
            .for_each_affected_tile_index(center, max_affected_radius_px, |index| {
                if let Some(tile_key) = self.tile_keys.get(index).copied() {
                    visit(index, tile_key);
                }
            });
    }

    pub fn collect_affected_tile_keys(
        &self,
        center: CanvasVec2,
        max_affected_radius_px: u32,
        output: &mut Vec<TileKey>,
    ) {
        output.clear();
        self.for_each_affected_tile_key(center, max_affected_radius_px, |_index, tile_key| {
            output.push(tile_key)
        });
    }
}

#[cfg(test)]
mod tests {
    use glaphica_core::{BackendId, CanvasVec2, IMAGE_TILE_SIZE, TileKey};

    use crate::layout::ImageLayout;

    use super::{Image, ImageTileAccessError};

    #[test]
    fn set_and_get_tile_key_use_index_mapping() {
        let layout = ImageLayout::new(IMAGE_TILE_SIZE, IMAGE_TILE_SIZE);
        let image_result = Image::new(layout, BackendId::new(1));
        assert!(image_result.is_ok());
        let mut image = match image_result {
            Ok(image) => image,
            Err(_) => return,
        };
        let key = TileKey::from_parts(1, 2, 3);

        let set = image.set_tile_key(0, key);
        assert!(set.is_ok());
        assert_eq!(image.tile_key(0), Some(key));
    }

    #[test]
    fn set_tile_key_rejects_out_of_bounds_index() {
        let layout = ImageLayout::new(IMAGE_TILE_SIZE, IMAGE_TILE_SIZE);
        let image_result = Image::new(layout, BackendId::new(1));
        assert!(image_result.is_ok());
        let mut image = match image_result {
            Ok(image) => image,
            Err(_) => return,
        };
        let set = image.set_tile_key(9, TileKey::from_parts(1, 2, 3));
        assert_eq!(set, Err(ImageTileAccessError::OutOfBounds));
    }

    #[test]
    fn collect_affected_tile_keys_uses_layout_addressing() {
        let layout = ImageLayout::new(IMAGE_TILE_SIZE * 2, IMAGE_TILE_SIZE);
        let image_result = Image::new(layout, BackendId::new(1));
        assert!(image_result.is_ok());
        let mut image = match image_result {
            Ok(image) => image,
            Err(_) => return,
        };
        assert!(
            image
                .set_tile_key(0, TileKey::from_parts(1, 2, 100))
                .is_ok()
        );
        assert!(
            image
                .set_tile_key(1, TileKey::from_parts(1, 2, 101))
                .is_ok()
        );

        let mut keys = Vec::new();
        image.collect_affected_tile_keys(
            CanvasVec2::new(IMAGE_TILE_SIZE as f32, 5.0),
            0,
            &mut keys,
        );

        assert_eq!(
            keys,
            vec![
                TileKey::from_parts(1, 2, 100),
                TileKey::from_parts(1, 2, 101)
            ]
        );
    }
}
