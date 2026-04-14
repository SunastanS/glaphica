use std::error::Error;
use std::fmt::{Display, Formatter};

use atlas::{CachedTileGroup, TileKey};

use crate::{GlaImage, layout::GlaImageLayout};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GlaCachedImageCreateError {
    WrongTileCount { expected: usize, actual: usize },
    WrongCachedTileCount { expected: usize, actual: usize },
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
        }
    }
}

impl Error for GlaCachedImageCreateError {}

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
        let mut tile_keys = Vec::with_capacity(image.tile_count());
        for tile_index in 0..image.tile_count() {
            tile_keys.push(image.tile_key(tile_index).unwrap_or(TileKey::EMPTY));
        }
        Self::new(*image.layout(), cache_group, tile_keys)
    }

    pub fn new(
        layout: GlaImageLayout,
        cache_group: CachedTileGroup,
        tile_keys: Vec<TileKey>,
    ) -> Result<Self, GlaCachedImageCreateError> {
        let expected_tiles = layout.total_tiles() as usize;
        if tile_keys.len() != expected_tiles {
            return Err(GlaCachedImageCreateError::WrongTileCount {
                expected: expected_tiles,
                actual: tile_keys.len(),
            });
        }

        let non_empty_tiles = tile_keys
            .iter()
            .copied()
            .filter(|key| *key != TileKey::EMPTY)
            .count();
        let cached_tiles = cache_group.keys().len();
        if non_empty_tiles != cached_tiles {
            return Err(GlaCachedImageCreateError::WrongCachedTileCount {
                expected: non_empty_tiles,
                actual: cached_tiles,
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

    pub fn tile_count(&self) -> usize {
        self.tile_keys.len()
    }

    pub fn tile_key(&self, tile_index: usize) -> Option<TileKey> {
        self.tile_keys.get(tile_index).copied()
    }

    pub fn tile_keys(&self) -> &[TileKey] {
        &self.tile_keys
    }

    pub fn collect_non_empty_tile_indices(&self, output: &mut Vec<usize>) {
        output.clear();
        for (tile_index, &tile_key) in self.tile_keys.iter().enumerate() {
            if tile_key != TileKey::EMPTY {
                output.push(tile_index);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use atlas::{AtlasLayout, Backend, BackendId, TileKey};
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
            vec![TileKey::EMPTY],
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
    fn cached_image_requires_group_to_match_non_empty_tiles() {
        let backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(1));
        let cached = backend
            .alloc_cached(1)
            .expect("cached tiles should allocate");

        let image = GlaCachedImage::new(
            GlaImageLayout::new(IMAGE_TILE_SIZE * 2, IMAGE_TILE_SIZE),
            cached,
            vec![TileKey::EMPTY, TileKey::EMPTY],
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
            vec![first, TileKey::EMPTY, second, TileKey::EMPTY],
        );
        let image = image.expect("cached image should build");

        assert_eq!(image.tile_key(0), Some(first));
        assert_eq!(image.tile_key(1), Some(TileKey::EMPTY));
        assert_eq!(image.tile_key(2), Some(second));
        assert_eq!(image.tile_key(3), Some(TileKey::EMPTY));
    }
}
