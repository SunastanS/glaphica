use std::error::Error;
use std::fmt::{Display, Formatter};

use atlas::{AtlasError, Backend, BackendId, TileCredential, TileKey, TileManager, TileOwner};
use glaphica_core::CanvasVec2;

use crate::{AtlasTileMap, ImageId, ImageTileSlot, TileGrid, layout::GlaImageLayout};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlaImageCreateError {
    TooManyTiles,
    Backend(AtlasError),
}

impl Display for GlaImageCreateError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooManyTiles => write!(f, "image has too many tiles for this platform"),
            Self::Backend(error) => Display::fmt(error, f),
        }
    }
}

impl Error for GlaImageCreateError {}

impl From<AtlasError> for GlaImageCreateError {
    fn from(error: AtlasError) -> Self {
        Self::Backend(error)
    }
}

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
pub enum GlaImageCacheTileError {
    Atlas(AtlasError),
    TileAccess(GlaImageTileAccessError),
}

impl Display for GlaImageCacheTileError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Atlas(error) => Display::fmt(error, f),
            Self::TileAccess(error) => Display::fmt(error, f),
        }
    }
}

impl Error for GlaImageCacheTileError {}

impl From<AtlasError> for GlaImageCacheTileError {
    fn from(error: AtlasError) -> Self {
        Self::Atlas(error)
    }
}

impl From<GlaImageTileAccessError> for GlaImageCacheTileError {
    fn from(error: GlaImageTileAccessError) -> Self {
        Self::TileAccess(error)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GlaImageSlotRecBounds {
    pub min_slot_x: u32,
    pub min_slot_y: u32,
    pub max_slot_x: u32,
    pub max_slot_y: u32,
}

#[derive(Debug)]
pub struct GlaImage {
    layout: GlaImageLayout,
    tile_owners: Box<[TileOwner]>,
    tile_manager: TileManager,
    backend_id: BackendId,
}

impl GlaImage {
    pub fn new(
        layout: GlaImageLayout,
        tile_manager: impl Into<TileManager>,
    ) -> Result<Self, GlaImageCreateError> {
        let total_tiles =
            usize::try_from(layout.total_slots()).map_err(|_| GlaImageCreateError::TooManyTiles)?;
        let tile_manager = tile_manager.into();
        let backend_id = tile_manager.backend_id();
        let tile_owners = std::iter::repeat_with(|| tile_manager.empty_owner())
            .take(total_tiles)
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Ok(Self {
            layout,
            tile_owners,
            tile_manager,
            backend_id,
        })
    }

    pub const fn backend_id(&self) -> BackendId {
        self.backend_id
    }

    pub fn backend(&self) -> &Backend {
        self.tile_manager.backend()
    }

    pub fn tile_manager(&self) -> &TileManager {
        &self.tile_manager
    }

    pub const fn layout(&self) -> &GlaImageLayout {
        &self.layout
    }

    pub fn slot_count(&self) -> usize {
        self.tile_owners.len()
    }

    pub fn tile_key(&self, tile_index: usize) -> Result<TileKey, GlaImageTileAccessError> {
        let Some(tile_owner) = self.tile_owners.get(tile_index) else {
            return Err(GlaImageTileAccessError::OutOfBounds);
        };
        Ok(tile_owner.tile_key())
    }

    pub fn physical_tile_key(
        &self,
        tile_index: usize,
    ) -> Result<Option<TileKey>, GlaImageTileAccessError> {
        let Some(tile_owner) = self.tile_owners.get(tile_index) else {
            return Err(GlaImageTileAccessError::OutOfBounds);
        };
        Ok(tile_owner.physical_tile_key())
    }

    pub fn tile_credential(
        &self,
        tile_index: usize,
    ) -> Result<TileCredential, GlaImageTileAccessError> {
        let Some(tile_owner) = self.tile_owners.get(tile_index) else {
            return Err(GlaImageTileAccessError::OutOfBounds);
        };
        Ok(tile_owner.credential())
    }

    pub fn tile_owner(&self, tile_index: usize) -> Option<&TileOwner> {
        self.tile_owners.get(tile_index)
    }

    pub fn is_tile_empty(&self, tile_index: usize) -> Result<bool, GlaImageTileAccessError> {
        let Some(tile_owner) = self.tile_owners.get(tile_index) else {
            return Err(GlaImageTileAccessError::OutOfBounds);
        };
        Ok(tile_owner.is_empty())
    }

    pub fn into_tile_owners(self) -> (GlaImageLayout, Box<[TileOwner]>) {
        (self.layout, self.tile_owners)
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
        if actual_backend != self.backend_id {
            return Err(GlaImageTileAccessError::WrongBackend {
                expected: self.backend_id,
                actual: actual_backend,
            });
        }

        let Some(slot) = self.tile_owners.get_mut(tile_index) else {
            return Err(GlaImageTileAccessError::OutOfBounds);
        };

        Ok(std::mem::replace(slot, tile_owner))
    }

    pub fn clear_tile(&mut self, tile_index: usize) -> Result<(), GlaImageTileAccessError> {
        let Some(slot) = self.tile_owners.get_mut(tile_index) else {
            return Err(GlaImageTileAccessError::OutOfBounds);
        };
        let vacant = self.tile_manager.empty_owner();
        let old = std::mem::replace(slot, vacant);
        drop(old);
        Ok(())
    }

    pub fn cache_tile(&mut self, tile_index: usize) -> Result<(), GlaImageCacheTileError> {
        let Some(slot) = self.tile_owners.get_mut(tile_index) else {
            return Err(GlaImageTileAccessError::OutOfBounds.into());
        };
        if slot.physical_tile_key().is_none() {
            return Ok(());
        }

        let tile_owner = std::mem::replace(slot, self.tile_manager.empty_owner());
        let _cached_group = self.tile_manager.cache_active_owners([tile_owner])?;
        Ok(())
    }

    pub fn ensure_active_tile_key(
        &mut self,
        tile_index: usize,
    ) -> Result<TileKey, GlaImageEnsureActiveTileError> {
        // TODO: we should make activation of empty key automatically step by step
        let Some(tile_owner) = self.tile_owners.get_mut(tile_index) else {
            return Err(GlaImageTileAccessError::OutOfBounds.into());
        };
        Ok(self.tile_manager.ensure_active_tile(tile_owner)?)
    }

    pub fn resize_anchored_top_left(
        &mut self,
        new_layout: GlaImageLayout,
    ) -> Result<(), GlaImageCreateError> {
        if self.layout == new_layout {
            return Ok(());
        }

        let old_layout = self.layout;
        let new_total_tiles = usize::try_from(new_layout.total_slots())
            .map_err(|_| GlaImageCreateError::TooManyTiles)?;
        let mut old_tile_owners = std::mem::replace(
            &mut self.tile_owners,
            std::iter::repeat_with(|| self.tile_manager.empty_owner())
                .take(new_total_tiles)
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        );
        let overlap_tile_x = old_layout.slot_x().min(new_layout.slot_x()) as usize;
        let overlap_tile_y = old_layout.slot_y().min(new_layout.slot_y()) as usize;
        let old_stride = old_layout.slot_x() as usize;
        let new_stride = new_layout.slot_x() as usize;

        for tile_index in 0..old_tile_owners.len() {
            let tile_x = tile_index % old_stride;
            let tile_y = tile_index / old_stride;
            if tile_x >= overlap_tile_x || tile_y >= overlap_tile_y {
                continue;
            }

            let new_index = tile_y * new_stride + tile_x;
            self.tile_owners[new_index] =
                std::mem::replace(&mut old_tile_owners[tile_index], self.tile_manager.empty_owner());
        }

        self.layout = new_layout;
        Ok(())
    }

    pub fn non_empty_slot_bounds(&self) -> Option<GlaImageSlotRecBounds> {
        let tile_x = self.layout.slot_x() as usize;
        let mut bounds: Option<GlaImageSlotRecBounds> = None;

        for (tile_index, tile_owner) in self.tile_owners.iter().enumerate() {
            if tile_owner.physical_tile_key().is_none() {
                continue;
            }

            let tile_coord_x = (tile_index % tile_x) as u32;
            let tile_coord_y = (tile_index / tile_x) as u32;
            match &mut bounds {
                Some(bounds) => {
                    bounds.min_slot_x = bounds.min_slot_x.min(tile_coord_x);
                    bounds.min_slot_y = bounds.min_slot_y.min(tile_coord_y);
                    bounds.max_slot_x = bounds.max_slot_x.max(tile_coord_x);
                    bounds.max_slot_y = bounds.max_slot_y.max(tile_coord_y);
                }
                None => {
                    bounds = Some(GlaImageSlotRecBounds {
                        min_slot_x: tile_coord_x,
                        min_slot_y: tile_coord_y,
                        max_slot_x: tile_coord_x,
                        max_slot_y: tile_coord_y,
                    });
                }
            }
        }

        bounds
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
}

impl TileGrid for GlaImage {
    fn layout(&self) -> GlaImageLayout {
        *GlaImage::layout(self)
    }

    fn slot_count(&self) -> usize {
        GlaImage::slot_count(self)
    }
}

impl AtlasTileMap for GlaImage {
    fn physical_tile_key(&self, tile_index: usize) -> Option<TileKey> {
        GlaImage::physical_tile_key(self, tile_index).ok().flatten()
    }

    fn tile_key(&self, tile_index: usize) -> Option<TileKey> {
        GlaImage::tile_key(self, tile_index).ok()
    }
}

#[cfg(test)]
mod tests {
    use atlas::{AtlasLayout, Backend, BackendId};
    use glaphica_core::{CanvasVec2, IMAGE_TILE_SIZE};

    use crate::{ImageId, ImageTileSlot, layout::GlaImageLayout};

    use super::{GlaImage, GlaImageSlotRecBounds, GlaImageTileAccessError};

    #[test]
    fn replace_and_get_tile_key_use_index_mapping() {
        let layout = GlaImageLayout::new(IMAGE_TILE_SIZE, IMAGE_TILE_SIZE);
        let mut image =
            GlaImage::new(layout, Backend::new(AtlasLayout::Tiny8, BackendId::new(1))).unwrap();
        let backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(1));
        let tile_owner = backend.alloc_active().unwrap();
        let key = tile_owner.tile_key();

        let replaced = image.replace_tile_owner(0, tile_owner);
        assert!(matches!(replaced, Ok(previous) if previous.tile_key().is_empty()));
        assert_eq!(image.tile_key(0), Ok(key));
    }

    #[test]
    fn replace_tile_owner_rejects_out_of_bounds_index() {
        let layout = GlaImageLayout::new(IMAGE_TILE_SIZE, IMAGE_TILE_SIZE);
        let mut image =
            GlaImage::new(layout, Backend::new(AtlasLayout::Tiny8, BackendId::new(1))).unwrap();
        let backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(1));
        let tile_owner = backend.alloc_active().unwrap();

        let set = image.replace_tile_owner(9, tile_owner);
        assert!(matches!(set, Err(GlaImageTileAccessError::OutOfBounds)));
    }

    #[test]
    fn replace_tile_owner_rejects_wrong_backend() {
        let layout = GlaImageLayout::new(IMAGE_TILE_SIZE, IMAGE_TILE_SIZE);
        let mut image =
            GlaImage::new(layout, Backend::new(AtlasLayout::Tiny8, BackendId::new(1))).unwrap();
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
    fn collect_affected_tile_slots_returns_logical_slots() {
        let layout = GlaImageLayout::new(IMAGE_TILE_SIZE * 2, IMAGE_TILE_SIZE);
        let image =
            GlaImage::new(layout, Backend::new(AtlasLayout::Tiny8, BackendId::new(1))).unwrap();

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
    fn new_image_assigns_distinct_credentials_to_empty_slots() {
        let layout = GlaImageLayout::new(IMAGE_TILE_SIZE * 2, IMAGE_TILE_SIZE);
        let image =
            GlaImage::new(layout, Backend::new(AtlasLayout::Tiny8, BackendId::new(1))).unwrap();

        let left = image.tile_credential(0).expect("left slot should exist");
        let right = image.tile_credential(1).expect("right slot should exist");

        assert_ne!(left, right);
        assert_eq!(image.physical_tile_key(0), Ok(None));
        assert_eq!(image.physical_tile_key(1), Ok(None));
        assert!(image.tile_key(0).is_ok_and(|key| key.is_empty()));
        assert!(image.tile_key(1).is_ok_and(|key| key.is_empty()));
    }

    #[test]
    fn ensure_active_tile_binds_existing_slot_credential() {
        let backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(1));
        let mut image =
            GlaImage::new(GlaImageLayout::new(IMAGE_TILE_SIZE, IMAGE_TILE_SIZE), backend).unwrap();
        let credential = image
            .tile_credential(0)
            .expect("slot credential should exist");

        let tile_key = image
            .ensure_active_tile_key(0)
            .expect("tile should allocate");

        assert_eq!(image.physical_tile_key(0), Ok(Some(tile_key)));
        assert_eq!(image.tile_manager().resolve(credential), Ok(Some(tile_key)));
    }

    #[test]
    fn non_empty_slot_bounds_cover_non_empty_keys() {
        let layout = GlaImageLayout::new(IMAGE_TILE_SIZE * 3, IMAGE_TILE_SIZE * 2);
        let mut image =
            GlaImage::new(layout, Backend::new(AtlasLayout::Tiny8, BackendId::new(1))).unwrap();
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
            image.non_empty_slot_bounds(),
            Some(GlaImageSlotRecBounds {
                min_slot_x: 1,
                min_slot_y: 0,
                max_slot_x: 2,
                max_slot_y: 1,
            })
        );
    }

    #[test]
    fn resize_anchored_top_left_drops_removed_tile_owners() {
        let backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(1));
        let old_layout = GlaImageLayout::new(IMAGE_TILE_SIZE * 2, IMAGE_TILE_SIZE * 2);
        let mut image = GlaImage::new(
            old_layout,
            Backend::new(AtlasLayout::Tiny8, BackendId::new(1)),
        )
        .unwrap();
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

        assert_eq!(image.tile_key(0), Ok(kept_top_left_key));
        assert_eq!(image.tile_key(1), Ok(kept_bottom_left_key));
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
    fn clear_tile_releases_owner_immediately() {
        let backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(1));
        let mut image = GlaImage::new(
            GlaImageLayout::new(IMAGE_TILE_SIZE, IMAGE_TILE_SIZE),
            backend.clone(),
        )
        .unwrap();
        let tile_owner = backend.alloc_active().unwrap();
        let key = tile_owner.tile_key();
        assert!(image.replace_tile_owner(0, tile_owner).is_ok());

        let removed = image.clear_tile(0);
        assert!(removed.is_ok());

        assert_eq!(
            backend.tile_state(key),
            Err(atlas::AtlasError::GenerationMismatch)
        );
    }

    #[test]
    fn cache_tile_moves_owner_to_backend_cache() {
        let backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(1));
        let mut image = GlaImage::new(
            GlaImageLayout::new(IMAGE_TILE_SIZE, IMAGE_TILE_SIZE),
            backend.clone(),
        )
        .unwrap();
        let tile_owner = backend.alloc_active().unwrap();
        let key = tile_owner.tile_key();
        assert!(image.replace_tile_owner(0, tile_owner).is_ok());

        assert!(image.cache_tile(0).is_ok());

        assert!(image.tile_key(0).is_ok_and(|key| key.is_empty()));
        assert_eq!(backend.tile_state(key), Ok(atlas::TileState::Cached));
    }
}
