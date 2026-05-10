use std::error::Error;
use std::fmt::{Display, Formatter};

use atlas::{AtlasError, Backend, CachedTileGroup, TileKey};

use crate::{
    GlaImage, GlaImageCreateError, GlaImageTileAccessError, TileGrid, layout::GlaImageLayout,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GlaCachedImageCreateError {
    WrongTileCount { expected: usize, actual: usize },
    WrongCachedTileCount { expected: usize, actual: usize },
    WrongTileKeys,
    DuplicateTileKeys,
    TileAccess(GlaImageTileAccessError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlaCachedImageActivateError {
    Atlas(AtlasError),
    ImageCreate(GlaImageCreateError),
    TileAccess(GlaImageTileAccessError),
    TileKeyNotFound(TileKey),
}

impl Display for GlaCachedImageCreateError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WrongTileCount { expected, actual } => write!(
                f,
                "cached image tile count mismatch: expected {expected} tile slots, got {actual}"
            ),
            Self::WrongCachedTileCount { expected, actual } => write!(
                f,
                "cached image non-empty tile count mismatch: expected {expected} cached tiles, got {actual}"
            ),
            Self::WrongTileKeys => write!(
                f,
                "cached image has non-empty tile keys that are not present in the cached group"
            ),
            Self::DuplicateTileKeys => write!(f, "cached image has duplicate non-empty tile keys"),
            Self::TileAccess(error) => Display::fmt(error, f),
        }
    }
}

impl Error for GlaCachedImageCreateError {}

impl From<GlaImageTileAccessError> for GlaCachedImageCreateError {
    fn from(error: GlaImageTileAccessError) -> Self {
        Self::TileAccess(error)
    }
}

impl Display for GlaCachedImageActivateError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Atlas(e) => Display::fmt(e, f),
            Self::ImageCreate(e) => Display::fmt(e, f),
            Self::TileAccess(e) => Display::fmt(e, f),
            Self::TileKeyNotFound(key) => {
                write!(
                    f,
                    "activated tile key {key:?} not found in cached image tile map"
                )
            }
        }
    }
}

impl Error for GlaCachedImageActivateError {}

impl From<AtlasError> for GlaCachedImageActivateError {
    fn from(e: AtlasError) -> Self {
        Self::Atlas(e)
    }
}

impl From<GlaImageCreateError> for GlaCachedImageActivateError {
    fn from(e: GlaImageCreateError) -> Self {
        Self::ImageCreate(e)
    }
}

impl From<GlaImageTileAccessError> for GlaCachedImageActivateError {
    fn from(e: GlaImageTileAccessError) -> Self {
        Self::TileAccess(e)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlaCachedImage {
    layout: GlaImageLayout,
    cache_group: CachedTileGroup,
    cached_tile_indices: Box<[usize]>,
    tile_keys: Box<[Option<TileKey>]>,
}

impl GlaCachedImage {
    pub fn from_active_image(
        image: &GlaImage,
        cache_group: CachedTileGroup,
    ) -> Result<Self, GlaCachedImageCreateError> {
        let mut tile_keys = Vec::with_capacity(image.slot_count());
        for tile_index in 0..image.slot_count() {
            tile_keys.push(image.physical_tile_key(tile_index)?);
        }
        Self::new(*image.layout(), cache_group, tile_keys)
    }

    pub fn new(
        layout: GlaImageLayout,
        cache_group: CachedTileGroup,
        tile_keys: Vec<Option<TileKey>>,
    ) -> Result<Self, GlaCachedImageCreateError> {
        let expected_tiles = layout.total_slots() as usize;
        if tile_keys.len() != expected_tiles {
            return Err(GlaCachedImageCreateError::WrongTileCount {
                expected: expected_tiles,
                actual: tile_keys.len(),
            });
        }

        let mut cached_tile_indices = vec![None; cache_group.len()];
        let mut non_empty_count = 0usize;
        for (tile_index, key) in tile_keys.iter().copied().enumerate() {
            let Some(key) = key else {
                continue;
            };

            non_empty_count += 1;
            let Some(group_index) = cache_group
                .physical_keys()
                .position(|cached_key| cached_key == key)
            else {
                return Err(GlaCachedImageCreateError::WrongTileKeys);
            };
            if cached_tile_indices[group_index]
                .replace(tile_index)
                .is_some()
            {
                return Err(GlaCachedImageCreateError::DuplicateTileKeys);
            }
        }

        if non_empty_count != cache_group.len() {
            return Err(GlaCachedImageCreateError::WrongCachedTileCount {
                expected: non_empty_count,
                actual: cache_group.len(),
            });
        }

        let mut cached_tile_indices_ordered = Vec::with_capacity(cache_group.len());
        for tile_index in cached_tile_indices {
            let Some(tile_index) = tile_index else {
                return Err(GlaCachedImageCreateError::WrongTileKeys);
            };
            cached_tile_indices_ordered.push(tile_index);
        }

        Ok(Self {
            layout,
            cache_group,
            cached_tile_indices: cached_tile_indices_ordered.into_boxed_slice(),
            tile_keys: tile_keys.into_boxed_slice(),
        })
    }

    pub const fn layout(&self) -> GlaImageLayout {
        self.layout
    }

    pub fn cache_group(&self) -> &CachedTileGroup {
        &self.cache_group
    }

    pub fn slot_count(&self) -> usize {
        self.tile_keys.len()
    }

    pub fn physical_tile_key(&self, tile_index: usize) -> Option<TileKey> {
        self.tile_keys.get(tile_index).copied().flatten()
    }

    pub fn collect_non_empty_slot_indices(&self, output: &mut Vec<usize>) {
        output.clear();
        for (tile_index, tile_key) in self.tile_keys.iter().enumerate() {
            if tile_key.is_some() {
                output.push(tile_index);
            }
        }
    }

    pub fn activate(self, backend: &Backend) -> Result<GlaImage, GlaCachedImageActivateError> {
        let mut image = GlaImage::new(self.layout, backend.clone())?;
        let activated = backend.activate_cached_group(&self.cache_group)?;
        for (tile_owner, &tile_index) in activated.into_iter().zip(self.cached_tile_indices.iter())
        {
            image.replace_tile_owner(tile_index, tile_owner)?;
        }
        Ok(image)
    }
}

impl TileGrid for GlaCachedImage {
    fn layout(&self) -> GlaImageLayout {
        GlaCachedImage::layout(self)
    }

    fn slot_count(&self) -> usize {
        GlaCachedImage::slot_count(self)
    }
}

#[cfg(test)]
mod tests {
    use atlas::{AtlasLayout, Backend, BackendId};
    use glaphica_core::IMAGE_TILE_SIZE;

    use crate::{GlaCachedImage, GlaCachedImageCreateError, GlaImageLayout};

    #[test]
    fn cached_image_requires_full_logical_tile_map() {
        let backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(1));
        let cached = backend
            .alloc_cached(1)
            .expect("cached tiles should allocate");

        let image = GlaCachedImage::new(
            GlaImageLayout::new(IMAGE_TILE_SIZE * 2, IMAGE_TILE_SIZE),
            cached,
            vec![None],
        );

        assert_eq!(
            image,
            Err(GlaCachedImageCreateError::WrongTileCount {
                expected: 2,
                actual: 1,
            })
        );
    }

    #[test]
    fn cached_image_requires_group_to_match_non_empty_slots() {
        let backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(1));
        let cached = backend
            .alloc_cached(1)
            .expect("cached tiles should allocate");

        let image = GlaCachedImage::new(
            GlaImageLayout::new(IMAGE_TILE_SIZE * 2, IMAGE_TILE_SIZE),
            cached,
            vec![None, None],
        );

        assert_eq!(
            image,
            Err(GlaCachedImageCreateError::WrongCachedTileCount {
                expected: 0,
                actual: 1,
            })
        );
    }

    #[test]
    fn cached_image_rejects_keys_not_in_group() {
        let backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(1));
        let group_a = backend.alloc_cached(1).expect("group a should allocate");
        let group_b = backend.alloc_cached(1).expect("group b should allocate");
        let key_from_b = group_b
            .physical_key(0)
            .expect("cached group should contain one key");

        let image = GlaCachedImage::new(
            GlaImageLayout::new(IMAGE_TILE_SIZE, IMAGE_TILE_SIZE),
            group_a,
            vec![Some(key_from_b)],
        );

        assert_eq!(image, Err(GlaCachedImageCreateError::WrongTileKeys));
    }

    #[test]
    fn cached_image_rejects_duplicate_non_empty_keys() {
        let backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(1));
        let cached = backend
            .alloc_cached(2)
            .expect("cached tiles should allocate");
        let key_a = cached
            .physical_key(0)
            .expect("cached group should contain first key");
        let key_b = cached
            .physical_key(1)
            .expect("cached group should contain second key");

        let image = GlaCachedImage::new(
            GlaImageLayout::new(IMAGE_TILE_SIZE * 2, IMAGE_TILE_SIZE * 2),
            cached,
            vec![Some(key_a), Some(key_b), Some(key_a), None],
        );

        assert_eq!(image, Err(GlaCachedImageCreateError::DuplicateTileKeys));
    }

    #[test]
    fn cached_image_rejects_missing_cached_group_key() {
        let backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(1));
        let cached = backend
            .alloc_cached(2)
            .expect("cached tiles should allocate");
        let key_a = cached
            .physical_key(0)
            .expect("cached group should contain first key");

        let image = GlaCachedImage::new(
            GlaImageLayout::new(IMAGE_TILE_SIZE, IMAGE_TILE_SIZE),
            cached,
            vec![Some(key_a)],
        );

        assert!(image.is_err());
    }

    #[test]
    fn cached_image_activate_round_trip() {
        let backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(1));
        let layout = GlaImageLayout::new(IMAGE_TILE_SIZE * 2, IMAGE_TILE_SIZE * 2);
        let cached = backend
            .alloc_cached(2)
            .expect("cached tiles should allocate");
        let first = cached
            .physical_key(0)
            .expect("cached group should contain first key");
        let second = cached
            .physical_key(1)
            .expect("cached group should contain second key");
        let cached_image =
            GlaCachedImage::new(layout, cached, vec![Some(first), None, Some(second), None])
                .expect("cached image should build");

        let active = cached_image
            .activate(&backend)
            .expect("activate should succeed");

        assert_eq!(active.physical_tile_key(0), Ok(Some(first)));
        assert_eq!(active.physical_tile_key(1), Ok(None));
        assert_eq!(active.physical_tile_key(2), Ok(Some(second)));
        assert_eq!(active.physical_tile_key(3), Ok(None));
        assert_eq!(backend.tile_state(first), Ok(atlas::TileState::Active));
        assert_eq!(backend.tile_state(second), Ok(atlas::TileState::Active));
    }

    #[test]
    fn cached_image_activate_uses_cached_group_order_index_mapping() {
        let backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(1));
        let layout = GlaImageLayout::new(IMAGE_TILE_SIZE * 2, IMAGE_TILE_SIZE);
        let cached = backend
            .alloc_cached(2)
            .expect("cached tiles should allocate");
        let first = cached
            .physical_key(0)
            .expect("cached group should contain first key");
        let second = cached
            .physical_key(1)
            .expect("cached group should contain second key");
        let cached_image = GlaCachedImage::new(layout, cached, vec![Some(second), Some(first)])
            .expect("cached image should build");

        let active = cached_image
            .activate(&backend)
            .expect("activate should succeed");

        assert_eq!(active.physical_tile_key(0), Ok(Some(second)));
        assert_eq!(active.physical_tile_key(1), Ok(Some(first)));
    }

    #[test]
    fn cached_image_keeps_tile_keys_in_logical_order() {
        let backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(1));
        let cached = backend
            .alloc_cached(2)
            .expect("cached tiles should allocate");
        let first = cached
            .physical_key(0)
            .expect("cached group should contain first key");
        let second = cached
            .physical_key(1)
            .expect("cached group should contain second key");
        let image = GlaCachedImage::new(
            GlaImageLayout::new(IMAGE_TILE_SIZE * 2, IMAGE_TILE_SIZE * 2),
            cached,
            vec![Some(first), None, Some(second), None],
        );
        let image = image.expect("cached image should build");

        assert_eq!(image.physical_tile_key(0), Some(first));
        assert_eq!(image.physical_tile_key(1), None);
        assert_eq!(image.physical_tile_key(2), Some(second));
        assert_eq!(image.physical_tile_key(3), None);
    }
}
