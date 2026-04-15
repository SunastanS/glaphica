use std::error::Error;
use std::fmt::{Display, Formatter};

use atlas::{AtlasError, Backend, BackendId, TileKey, TileOwner};
use glaphica_core::CanvasVec2;

use crate::{ImageId, ImageTileSlot, layout::GlaImageLayout};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlaImageCreateError {
    TooManyTiles,
}

impl Display for GlaImageCreateError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooManyTiles => write!(f, "image has too many tiles for this platform"),
        }
    }
}

impl Error for GlaImageCreateError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlaImageTileAccessError {
    OutOfBounds,
    WrongBackend {
        expected: BackendId,
        actual: BackendId,
    },
}

impl Display for GlaImageTileAccessError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OutOfBounds => write!(f, "tile index is out of bounds"),
            Self::WrongBackend { expected, actual } => write!(
                f,
                "tile owner belongs to backend {}, expected backend {}",
                actual.raw(),
                expected.raw()
            ),
        }
    }
}

impl Error for GlaImageTileAccessError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlaImageEnsureActiveTileError {
    Atlas(AtlasError),
    TileAccess(GlaImageTileAccessError),
}

impl Display for GlaImageEnsureActiveTileError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Atlas(error) => Display::fmt(error, f),
            Self::TileAccess(error) => Display::fmt(error, f),
        }
    }
}

impl Error for GlaImageEnsureActiveTileError {}

impl From<AtlasError> for GlaImageEnsureActiveTileError {
    fn from(error: AtlasError) -> Self {
        Self::Atlas(error)
    }
}

impl From<GlaImageTileAccessError> for GlaImageEnsureActiveTileError {
    fn from(error: GlaImageTileAccessError) -> Self {
        Self::TileAccess(error)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GlaImageTileRecBounds {
    pub min_tile_x: u32,
    pub min_tile_y: u32,
    pub max_tile_x: u32,
    pub max_tile_y: u32,
}

#[derive(Debug)]
pub struct GlaImage {
    layout: GlaImageLayout,
    tile_owners: Box<[TileOwner]>,
    backend: BackendId,
}

impl GlaImage {
    pub fn new(layout: GlaImageLayout, backend: BackendId) -> Result<Self, GlaImageCreateError> {
        let total_tiles =
            usize::try_from(layout.total_tiles()).map_err(|_| GlaImageCreateError::TooManyTiles)?;
        let tile_owners = std::iter::repeat_with(TileOwner::empty)
            .take(total_tiles)
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Ok(Self {
            layout,
            tile_owners,
            backend,
        })
    }

    pub const fn backend(&self) -> BackendId {
        self.backend
    }

    pub const fn layout(&self) -> &GlaImageLayout {
        &self.layout
    }

    pub fn tile_count(&self) -> usize {
        self.tile_owners.len()
    }

    pub fn tile_key(&self, tile_index: usize) -> Option<TileKey> {
        Some(self.tile_owners.get(tile_index)?.tile_key())
    }

    pub fn tile_owner(&self, tile_index: usize) -> Option<&TileOwner> {
        self.tile_owners.get(tile_index)
    }

    pub fn tile_canvas_origin(&self, tile_index: usize) -> Option<CanvasVec2> {
        self.layout.tile_canvas_origin(tile_index)
    }

    pub fn replace_tile_owner(
        &mut self,
        tile_index: usize,
        tile_owner: TileOwner,
    ) -> Result<TileOwner, GlaImageTileAccessError> {
        let actual_backend = tile_owner.backend_id();
        if actual_backend != self.backend {
            return Err(GlaImageTileAccessError::WrongBackend {
                expected: self.backend,
                actual: actual_backend,
            });
        }

        let Some(slot) = self.tile_owners.get_mut(tile_index) else {
            return Err(GlaImageTileAccessError::OutOfBounds);
        };

        Ok(std::mem::replace(slot, tile_owner))
    }

    pub fn clear_tile(&mut self, tile_index: usize) -> Result<TileOwner, GlaImageTileAccessError> {
        let Some(slot) = self.tile_owners.get_mut(tile_index) else {
            return Err(GlaImageTileAccessError::OutOfBounds);
        };
        Ok(std::mem::replace(slot, TileOwner::empty()))
    }

    pub fn ensure_active_tile_key(
        &mut self,
        tile_index: usize,
        backend: &Backend,
    ) -> Result<TileKey, GlaImageEnsureActiveTileError> {
        let actual_backend = backend.backend_id()?;
        if actual_backend != self.backend {
            return Err(GlaImageTileAccessError::WrongBackend {
                expected: self.backend,
                actual: actual_backend,
            }
            .into());
        }

        let existing_tile_key = self
            .tile_key(tile_index)
            .ok_or(GlaImageTileAccessError::OutOfBounds)?;
        if existing_tile_key != TileKey::EMPTY {
            return Ok(existing_tile_key);
        }

        let tile_owner = backend.alloc_active()?;
        let tile_key = tile_owner.tile_key();
        self.replace_tile_owner(tile_index, tile_owner)?;
        Ok(tile_key)
    }

    pub fn resize_anchored_top_left(
        &mut self,
        new_layout: GlaImageLayout,
    ) -> Result<(), GlaImageCreateError> {
        if self.layout == new_layout {
            return Ok(());
        }

        let old_layout = self.layout;
        let new_total_tiles = usize::try_from(new_layout.total_tiles())
            .map_err(|_| GlaImageCreateError::TooManyTiles)?;
        let mut old_tile_owners = std::mem::replace(
            &mut self.tile_owners,
            std::iter::repeat_with(TileOwner::empty)
                .take(new_total_tiles)
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        );
        let overlap_tile_x = old_layout.tile_x().min(new_layout.tile_x()) as usize;
        let overlap_tile_y = old_layout.tile_y().min(new_layout.tile_y()) as usize;
        let old_stride = old_layout.tile_x() as usize;
        let new_stride = new_layout.tile_x() as usize;

        for tile_index in 0..old_tile_owners.len() {
            let tile_x = tile_index % old_stride;
            let tile_y = tile_index / old_stride;
            if tile_x >= overlap_tile_x || tile_y >= overlap_tile_y {
                continue;
            }

            let new_index = tile_y * new_stride + tile_x;
            self.tile_owners[new_index] =
                std::mem::replace(&mut old_tile_owners[tile_index], TileOwner::empty());
        }

        self.layout = new_layout;
        Ok(())
    }

    pub fn non_empty_tile_bounds(&self) -> Option<GlaImageTileRecBounds> {
        let tile_x = self.layout.tile_x() as usize;
        let mut bounds: Option<GlaImageTileRecBounds> = None;

        for (tile_index, tile_owner) in self.tile_owners.iter().enumerate() {
            if tile_owner.tile_key() == TileKey::EMPTY {
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
                    bounds = Some(GlaImageTileRecBounds {
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
                let tile_key = self
                    .tile_owners
                    .get(index)
                    .map(TileOwner::tile_key)
                    .unwrap_or(TileKey::EMPTY);
                visit(index, tile_key);
            });
    }

    pub fn collect_affected_tile_slots(
        &self,
        image_id: ImageId,
        center: CanvasVec2,
        max_affected_radius_px: u32,
        output: &mut Vec<ImageTileSlot>,
    ) {
        output.clear();
        self.layout
            .for_each_affected_tile_index(center, max_affected_radius_px, |tile_index| {
                output.push(ImageTileSlot::new(image_id, tile_index));
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
            output.push(tile_key);
        });
    }
}

#[cfg(test)]
mod tests {
    use atlas::{AtlasLayout, Backend, BackendId, TileKey};
    use glaphica_core::{CanvasVec2, IMAGE_TILE_SIZE};

    use crate::{ImageId, ImageTileSlot, layout::GlaImageLayout};

    use super::{GlaImage, GlaImageTileAccessError, GlaImageTileRecBounds};

    #[test]
    fn replace_and_get_tile_key_use_index_mapping() {
        let layout = GlaImageLayout::new(IMAGE_TILE_SIZE, IMAGE_TILE_SIZE);
        let mut image = GlaImage::new(layout, BackendId::new(1)).unwrap();
        let backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(1));
        let tile_owner = backend.alloc_active().unwrap();
        let key = tile_owner.tile_key();

        let replaced = image.replace_tile_owner(0, tile_owner);
        assert!(matches!(replaced, Ok(previous) if previous.tile_key() == TileKey::EMPTY));
        assert_eq!(image.tile_key(0), Some(key));
    }

    #[test]
    fn replace_tile_owner_rejects_out_of_bounds_index() {
        let layout = GlaImageLayout::new(IMAGE_TILE_SIZE, IMAGE_TILE_SIZE);
        let mut image = GlaImage::new(layout, BackendId::new(1)).unwrap();
        let backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(1));
        let tile_owner = backend.alloc_active().unwrap();

        let set = image.replace_tile_owner(9, tile_owner);
        assert!(matches!(set, Err(GlaImageTileAccessError::OutOfBounds)));
    }

    #[test]
    fn replace_tile_owner_rejects_wrong_backend() {
        let layout = GlaImageLayout::new(IMAGE_TILE_SIZE, IMAGE_TILE_SIZE);
        let mut image = GlaImage::new(layout, BackendId::new(1)).unwrap();
        let backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(2));
        let tile_owner = backend.alloc_active().unwrap();

        let set = image.replace_tile_owner(0, tile_owner);
        assert!(matches!(
            set,
            Err(GlaImageTileAccessError::WrongBackend {
                expected,
                actual,
            }) if expected == BackendId::new(1) && actual == BackendId::new(2)
        ));
    }

    #[test]
    fn collect_affected_tile_keys_uses_layout_addressing() {
        let layout = GlaImageLayout::new(IMAGE_TILE_SIZE * 2, IMAGE_TILE_SIZE);
        let mut image = GlaImage::new(layout, BackendId::new(1)).unwrap();
        let backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(1));
        let tile_owner = backend.alloc_active().unwrap();
        let expected = tile_owner.tile_key();
        assert!(image.replace_tile_owner(1, tile_owner).is_ok());

        let mut keys = Vec::new();
        image.collect_affected_tile_keys(
            CanvasVec2::new(IMAGE_TILE_SIZE as f32, 5.0),
            0,
            &mut keys,
        );

        assert_eq!(keys, vec![TileKey::EMPTY, expected]);
    }

    #[test]
    fn collect_affected_tile_slots_returns_logical_slots() {
        let layout = GlaImageLayout::new(IMAGE_TILE_SIZE * 2, IMAGE_TILE_SIZE);
        let image = GlaImage::new(layout, BackendId::new(1)).unwrap();

        let mut slots = Vec::new();
        image.collect_affected_tile_slots(
            ImageId(9),
            CanvasVec2::new(IMAGE_TILE_SIZE as f32, 5.0),
            0,
            &mut slots,
        );

        assert_eq!(
            slots,
            vec![
                ImageTileSlot::new(ImageId(9), 0),
                ImageTileSlot::new(ImageId(9), 1)
            ]
        );
    }

    #[test]
    fn non_empty_tile_bounds_cover_non_empty_keys() {
        let layout = GlaImageLayout::new(IMAGE_TILE_SIZE * 3, IMAGE_TILE_SIZE * 2);
        let mut image = GlaImage::new(layout, BackendId::new(1)).unwrap();
        let backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(1));
        assert!(
            image
                .replace_tile_owner(1, backend.alloc_active().unwrap())
                .is_ok()
        );
        assert!(
            image
                .replace_tile_owner(5, backend.alloc_active().unwrap())
                .is_ok()
        );

        assert_eq!(
            image.non_empty_tile_bounds(),
            Some(GlaImageTileRecBounds {
                min_tile_x: 1,
                min_tile_y: 0,
                max_tile_x: 2,
                max_tile_y: 1,
            })
        );
    }

    #[test]
    fn resize_anchored_top_left_drops_removed_tile_owners() {
        let backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(1));
        let old_layout = GlaImageLayout::new(IMAGE_TILE_SIZE * 2, IMAGE_TILE_SIZE * 2);
        let mut image = GlaImage::new(old_layout, BackendId::new(1)).unwrap();
        let kept_top_left = backend.alloc_active().unwrap();
        let removed_top_right = backend.alloc_active().unwrap();
        let kept_bottom_left = backend.alloc_active().unwrap();
        let removed_bottom_right = backend.alloc_active().unwrap();
        let kept_top_left_key = kept_top_left.tile_key();
        let kept_bottom_left_key = kept_bottom_left.tile_key();
        let removed_top_right_key = removed_top_right.tile_key();
        let removed_bottom_right_key = removed_bottom_right.tile_key();

        assert!(image.replace_tile_owner(0, kept_top_left).is_ok());
        assert!(image.replace_tile_owner(1, removed_top_right).is_ok());
        assert!(image.replace_tile_owner(2, kept_bottom_left).is_ok());
        assert!(image.replace_tile_owner(3, removed_bottom_right).is_ok());

        assert!(
            image
                .resize_anchored_top_left(GlaImageLayout::new(IMAGE_TILE_SIZE, IMAGE_TILE_SIZE * 2))
                .is_ok()
        );

        assert_eq!(image.tile_key(0), Some(kept_top_left_key));
        assert_eq!(image.tile_key(1), Some(kept_bottom_left_key));
        assert_eq!(
            backend.tile_state(removed_top_right_key),
            Err(atlas::AtlasError::GenerationMismatch)
        );
        assert_eq!(
            backend.tile_state(removed_bottom_right_key),
            Err(atlas::AtlasError::GenerationMismatch)
        );
    }

    #[test]
    fn clear_tile_releases_owner_when_dropped() {
        let backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(1));
        let mut image = GlaImage::new(
            GlaImageLayout::new(IMAGE_TILE_SIZE, IMAGE_TILE_SIZE),
            BackendId::new(1),
        )
        .unwrap();
        let tile_owner = backend.alloc_active().unwrap();
        let key = tile_owner.tile_key();
        assert!(image.replace_tile_owner(0, tile_owner).is_ok());

        let removed = image.clear_tile(0);
        assert!(removed.is_ok());
        drop(removed.unwrap());

        assert_eq!(
            backend.tile_state(key),
            Err(atlas::AtlasError::GenerationMismatch)
        );
    }
}
