use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt::{Display, Formatter};

use atlas::{AtlasError, Backend, CachedTileGroup, TileKey};

use crate::{
    AtlasTileMap, GlaImage, GlaImageCreateError, GlaImageTileAccessError, TileGrid,
    layout::GlaImageLayout,
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
    tile_keys: Box<[TileKey]>,
}

impl GlaCachedImage {
    pub fn from_active_image(
        image: &GlaImage,
        cache_group: CachedTileGroup,
    ) -> Result<Self, GlaCachedImageCreateError> {
        let mut tile_keys = Vec::with_capacity(image.slot_count());
        for tile_index in 0..image.slot_count() {
            tile_keys.push(image.tile_key(tile_index)?);
        }
        Self::new(*image.layout(), cache_group, tile_keys)
    }

    pub fn new(
        layout: GlaImageLayout,
        cache_group: CachedTileGroup,
        tile_keys: Vec<TileKey>,
    ) -> Result<Self, GlaCachedImageCreateError> {
        let expected_tiles = layout.total_slots() as usize;
        if tile_keys.len() != expected_tiles {
            return Err(GlaCachedImageCreateError::WrongTileCount {
                expected: expected_tiles,
                actual: tile_keys.len(),
            });
        }

        let cached_set: HashSet<TileKey> = cache_group.keys().iter().copied().collect();
        let mut non_empty_set = HashSet::with_capacity(cached_set.len());
        for &key in &tile_keys {
            if key.is_empty() {
                continue;
            }
            if !cached_set.contains(&key) {
                return Err(GlaCachedImageCreateError::WrongTileKeys);
            }
            if !non_empty_set.insert(key) {
                return Err(GlaCachedImageCreateError::DuplicateTileKeys);
            }
        }

        if non_empty_set != cached_set {
            return Err(GlaCachedImageCreateError::WrongCachedTileCount {
                expected: non_empty_set.len(),
                actual: cached_set.len(),
            });
        }

        Ok(Self {
            layout,
            cache_group,
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

    pub fn tile_key(&self, tile_index: usize) -> Option<TileKey> {
        self.tile_keys.get(tile_index).copied()
    }

    pub fn tile_keys(&self) -> &[TileKey] {
        &self.tile_keys
    }

    pub fn collect_non_empty_slot_indices(&self, output: &mut Vec<usize>) {
        output.clear();
        for (tile_index, &tile_key) in self.tile_keys.iter().enumerate() {
            if !tile_key.is_empty() {
                output.push(tile_index);
            }
        }
    }

    pub fn activate(self, backend: &Backend) -> Result<GlaImage, GlaCachedImageActivateError> {
        let key_to_index: HashMap<TileKey, usize> = self
            .tile_keys
            .iter()
            .enumerate()
            .filter(|&(_, k)| !k.is_empty())
            .map(|(i, k)| (*k, i))
            .collect();
        let mut image = GlaImage::new(self.layout, backend.clone())?;
        let activated = backend.activate_cached_group(&self.cache_group)?;
        for tile_owner in activated {
            let tile_key = tile_owner.tile_key();
            let &tile_index = key_to_index
                .get(&tile_key)
                .ok_or(GlaCachedImageActivateError::TileKeyNotFound(tile_key))?;
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

impl AtlasTileMap for GlaCachedImage {
    fn tile_key(&self, tile_index: usize) -> Option<TileKey> {
        GlaCachedImage::tile_key(self, tile_index)
    }
}

#[cfg(test)]
mod tests {
    use atlas::{AtlasLayout, Backend, BackendId};
    use glaphica_core::IMAGE_TILE_SIZE;

    use crate::{GlaCachedImage, GlaCachedImageCreateError, GlaImageLayout};

    fn empty_key_with_backend(backend: &Backend) -> atlas::TileKey {
        backend.empty_tile_key()
    }

    #[test]
    fn cached_image_requires_full_logical_tile_map() {
        let backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(1));
        let cached = backend
            .alloc_cached(1)
            .expect("cached tiles should allocate");

        let image = GlaCachedImage::new(
            GlaImageLayout::new(IMAGE_TILE_SIZE * 2, IMAGE_TILE_SIZE),
            cached,
            vec![empty_key_with_backend(&backend)],
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
            vec![
                empty_key_with_backend(&backend),
                empty_key_with_backend(&backend),
            ],
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
        let key_from_b = group_b.keys()[0];

        let image = GlaCachedImage::new(
            GlaImageLayout::new(IMAGE_TILE_SIZE, IMAGE_TILE_SIZE),
            group_a,
            vec![key_from_b],
        );

        assert_eq!(image, Err(GlaCachedImageCreateError::WrongTileKeys));
    }

    #[test]
    fn cached_image_rejects_duplicate_non_empty_keys() {
        let backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(1));
        let cached = backend
            .alloc_cached(2)
            .expect("cached tiles should allocate");
        let key_a = cached.keys()[0];
        let key_b = cached.keys()[1];

        let image = GlaCachedImage::new(
            GlaImageLayout::new(IMAGE_TILE_SIZE * 2, IMAGE_TILE_SIZE * 2),
            cached,
            vec![key_a, key_b, key_a, empty_key_with_backend(&backend)],
        );

        assert_eq!(image, Err(GlaCachedImageCreateError::DuplicateTileKeys));
    }

    #[test]
    fn cached_image_rejects_missing_cached_group_key() {
        let backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(1));
        let cached = backend
            .alloc_cached(2)
            .expect("cached tiles should allocate");
        let key_a = cached.keys()[0];

        let image = GlaCachedImage::new(
            GlaImageLayout::new(IMAGE_TILE_SIZE, IMAGE_TILE_SIZE),
            cached,
            vec![key_a],
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
        let first = cached.keys()[0];
        let second = cached.keys()[1];
        let cached_image = GlaCachedImage::new(
            layout,
            cached,
            vec![
                first,
                empty_key_with_backend(&backend),
                second,
                empty_key_with_backend(&backend),
            ],
        )
        .expect("cached image should build");

        let active = cached_image
            .activate(&backend)
            .expect("activate should succeed");

        assert_eq!(active.tile_key(0), Ok(first));
        assert_eq!(active.tile_key(1), Ok(empty_key_with_backend(&backend)));
        assert_eq!(active.tile_key(2), Ok(second));
        assert_eq!(active.tile_key(3), Ok(empty_key_with_backend(&backend)));
        assert_eq!(backend.tile_state(first), Ok(atlas::TileState::Active));
        assert_eq!(backend.tile_state(second), Ok(atlas::TileState::Active));
    }

    #[test]
    fn cached_image_keeps_tile_keys_in_logical_order() {
        let backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(1));
        let cached = backend
            .alloc_cached(2)
            .expect("cached tiles should allocate");
        let first = cached.keys()[0];
        let second = cached.keys()[1];
        let image = GlaCachedImage::new(
            GlaImageLayout::new(IMAGE_TILE_SIZE * 2, IMAGE_TILE_SIZE * 2),
            cached,
            vec![
                first,
                empty_key_with_backend(&backend),
                second,
                empty_key_with_backend(&backend),
            ],
        );
        let image = image.expect("cached image should build");

        assert_eq!(image.tile_key(0), Some(first));
        assert_eq!(image.tile_key(1), Some(empty_key_with_backend(&backend)));
        assert_eq!(image.tile_key(2), Some(second));
        assert_eq!(image.tile_key(3), Some(empty_key_with_backend(&backend)));
    }
}
