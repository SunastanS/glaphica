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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NonEmptyTileBounds {
    pub min_tile_x: u32,
    pub min_tile_y: u32,
    pub max_tile_x: u32,
    pub max_tile_y: u32,
}

impl Display for ImageTileAccessError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OutOfBounds => write!(f, "tile index is out of bounds"),
        }
    }
}

impl Error for ImageTileAccessError {}

#[derive(Clone, PartialEq)]
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

    pub fn tile_count(&self) -> usize {
        self.tile_keys.len()
    }

    pub fn tile_key(&self, tile_index: usize) -> Option<TileKey> {
        self.tile_keys.get(tile_index).copied()
    }

    pub fn tile_canvas_origin(&self, tile_index: usize) -> Option<CanvasVec2> {
        self.layout.tile_canvas_origin(tile_index)
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

    pub fn resize_anchored_top_left(
        &mut self,
        new_layout: ImageLayout,
    ) -> Result<Vec<TileKey>, ImageCreateError> {
        if self.layout == new_layout {
            return Ok(Vec::new());
        }

        let old_layout = self.layout;
        let old_tile_keys = std::mem::replace(
            &mut self.tile_keys,
            vec![
                TileKey::EMPTY;
                usize::try_from(new_layout.total_tiles())
                    .map_err(|_| ImageCreateError::TooManyTiles)?
            ]
            .into_boxed_slice(),
        );
        let overlap_tile_x = old_layout.tile_x().min(new_layout.tile_x()) as usize;
        let overlap_tile_y = old_layout.tile_y().min(new_layout.tile_y()) as usize;
        let old_stride = old_layout.tile_x() as usize;
        let new_stride = new_layout.tile_x() as usize;

        let mut removed_tile_keys = Vec::new();
        for (tile_index, tile_key) in old_tile_keys.iter().copied().enumerate() {
            let tile_x = tile_index % old_stride;
            let tile_y = tile_index / old_stride;
            if tile_x < overlap_tile_x && tile_y < overlap_tile_y {
                let new_index = tile_y * new_stride + tile_x;
                self.tile_keys[new_index] = tile_key;
                continue;
            }

            if tile_key != TileKey::EMPTY {
                removed_tile_keys.push(tile_key);
            }
        }

        self.layout = new_layout;
        Ok(removed_tile_keys)
    }

    pub fn non_empty_tile_bounds(&self) -> Option<NonEmptyTileBounds> {
        let tile_x = self.layout.tile_x() as usize;
        let mut bounds: Option<NonEmptyTileBounds> = None;

        for (tile_index, tile_key) in self.tile_keys.iter().copied().enumerate() {
            if tile_key == TileKey::EMPTY {
                continue;
            }

            let tile_coord_x = (tile_index % tile_x) as u32;
            let tile_coord_y = (tile_index / tile_x) as u32;
            match &mut bounds {
                Some(bounds) => {
                    bounds.min_tile_x = bounds.min_tile_x.min(tile_coord_x);
                    bounds.min_tile_y = bounds.min_tile_y.min(tile_coord_y);
                    bounds.max_tile_x = bounds.max_tile_x.max(tile_coord_x);
                    bounds.max_tile_y = bounds.max_tile_y.max(tile_coord_y);
                }
                None => {
                    bounds = Some(NonEmptyTileBounds {
                        min_tile_x: tile_coord_x,
                        min_tile_y: tile_coord_y,
                        max_tile_x: tile_coord_x,
                        max_tile_y: tile_coord_y,
                    });
                }
            }
        }

        bounds
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

    use super::{Image, ImageTileAccessError, NonEmptyTileBounds};

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

    #[test]
    fn non_empty_tile_bounds_cover_non_empty_keys() {
        let layout = ImageLayout::new(IMAGE_TILE_SIZE * 3, IMAGE_TILE_SIZE * 2);
        let mut image = Image::new(layout, BackendId::new(1)).unwrap();
        assert!(
            image
                .set_tile_key(1, TileKey::from_parts(1, 2, 100))
                .is_ok()
        );
        assert!(
            image
                .set_tile_key(5, TileKey::from_parts(1, 2, 101))
                .is_ok()
        );

        assert_eq!(
            image.non_empty_tile_bounds(),
            Some(NonEmptyTileBounds {
                min_tile_x: 1,
                min_tile_y: 0,
                max_tile_x: 2,
                max_tile_y: 1,
            })
        );
    }

    #[test]
    fn resize_anchored_top_left_remaps_overlapping_tiles_and_reports_removed() {
        let old_layout = ImageLayout::new(IMAGE_TILE_SIZE * 2, IMAGE_TILE_SIZE * 2);
        let mut image = Image::new(old_layout, BackendId::new(1)).unwrap();
        let kept_top_left = TileKey::from_parts(1, 1, 10);
        let kept_bottom_left = TileKey::from_parts(1, 1, 11);
        let removed_top_right = TileKey::from_parts(1, 1, 12);
        let removed_bottom_right = TileKey::from_parts(1, 1, 13);
        image.set_tile_key(0, kept_top_left).unwrap();
        image.set_tile_key(1, removed_top_right).unwrap();
        image.set_tile_key(2, kept_bottom_left).unwrap();
        image.set_tile_key(3, removed_bottom_right).unwrap();

        let removed = image
            .resize_anchored_top_left(ImageLayout::new(IMAGE_TILE_SIZE, IMAGE_TILE_SIZE * 2))
            .unwrap();

        assert_eq!(
            *image.layout(),
            ImageLayout::new(IMAGE_TILE_SIZE, IMAGE_TILE_SIZE * 2)
        );
        assert_eq!(image.tile_key(0), Some(kept_top_left));
        assert_eq!(image.tile_key(1), Some(kept_bottom_left));
        assert_eq!(removed, vec![removed_top_right, removed_bottom_right]);
    }

    #[test]
    fn resize_anchored_top_left_moves_tile_keys_by_tile_coords() {
        let old_layout = ImageLayout::new(IMAGE_TILE_SIZE * 2, IMAGE_TILE_SIZE * 2);
        let new_layout = ImageLayout::new(IMAGE_TILE_SIZE * 3, IMAGE_TILE_SIZE * 2);
        let mut image = Image::new(old_layout, BackendId::new(1)).unwrap();
        let top_right = TileKey::from_parts(1, 2, 100);
        let bottom_left = TileKey::from_parts(1, 2, 101);
        let bottom_right = TileKey::from_parts(1, 2, 102);
        image.set_tile_key(1, top_right).unwrap();
        image.set_tile_key(2, bottom_left).unwrap();
        image.set_tile_key(3, bottom_right).unwrap();

        let removed = image.resize_anchored_top_left(new_layout).unwrap();

        assert!(removed.is_empty());
        assert_eq!(image.tile_key(1), Some(top_right));
        assert_eq!(image.tile_key(3), Some(bottom_left));
        assert_eq!(image.tile_key(4), Some(bottom_right));
    }
}
