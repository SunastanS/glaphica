use gla_color::GlaFormat;
use gla_core::{Pool, PoolError};
pub use gla_core::IMAGE_TILE_SIZE;
use std::fmt::{Display, Formatter};
use tile_key::{TileKey, Tiles, TilesError};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct GlaImageKey(u64);

impl GlaImageKey {
    const INDEX_BITS: u32 = 32;
    const INDEX_MASK: u64 = (1 << Self::INDEX_BITS) - 1;
    const GENERATION_SHIFT: u32 = Self::INDEX_BITS;

    #[inline]
    pub fn new(index: u32, generation: u32) -> Self {
        debug_assert!(index as u64 <= Self::INDEX_MASK);
        Self(((generation as u64) << Self::GENERATION_SHIFT) | index as u64)
    }

    #[inline]
    pub fn index(self) -> u32 {
        (self.0 & Self::INDEX_MASK) as u32
    }

    #[inline]
    pub fn generation(self) -> u32 {
        (self.0 >> Self::GENERATION_SHIFT) as u32
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GlaImageLayout {
    pub width_px: u32,
    pub height_px: u32,
} // line-first mapping index to tile logical arrangement

impl GlaImageLayout {
    pub fn new(width_px: u32, height_px: u32) -> Self {
        Self {
            width_px,
            height_px,
        }
    }

    pub fn tile_count_x(&self) -> u32 {
        self.width_px.div_ceil(IMAGE_TILE_SIZE)
    }

    pub fn tile_count_y(&self) -> u32 {
        self.height_px.div_ceil(IMAGE_TILE_SIZE)
    }

    pub fn tile_count(&self) -> u32 {
        self.tile_count_x() * self.tile_count_y()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TileSet {
    Full,
    Tiles(Vec<u32>),
}

impl TileSet {
    pub fn tiles<I>(tiles: I) -> Self
    where
        I: IntoIterator<Item = u32>,
    {
        let mut tiles: Vec<u32> = tiles.into_iter().collect();
        tiles.sort_unstable();
        tiles.dedup();
        Self::Tiles(tiles)
    }

    pub fn single(tile: u32) -> Self {
        Self::Tiles(vec![tile])
    }

    pub fn insert(&mut self, tile: u32) {
        match self {
            Self::Full => {}
            Self::Tiles(tiles) => match tiles.binary_search(&tile) {
                Ok(_) => {}
                Err(index) => tiles.insert(index, tile),
            },
        }
    }

    pub fn is_empty(&self) -> bool {
        matches!(self, Self::Tiles(tiles) if tiles.is_empty())
    }

    pub fn clear(&mut self) {
        match self {
            Self::Full => *self = Self::default(),
            Self::Tiles(tiles) => tiles.clear(),
        }
    }

    pub fn union_assign(&mut self, other: &Self) {
        match other {
            Self::Full => *self = Self::Full,
            Self::Tiles(right) if right.is_empty() => {}
            Self::Tiles(right) => match self {
                Self::Full => {}
                Self::Tiles(left) => {
                    left.extend(right.iter().copied());
                    left.sort_unstable();
                    left.dedup();
                }
            },
        }
    }
}

impl Default for TileSet {
    fn default() -> Self {
        Self::Tiles(Vec::new())
    }
}

#[derive(Clone, Debug)]
pub struct GlaImage {
    pub format: GlaFormat,
    pub layout: GlaImageLayout,
    pub tiles: Box<[TileKey]>, // tiles.len() == layout.tile_count()
}

impl GlaImage {
    pub fn new(
        format: GlaFormat,
        layout: GlaImageLayout,
        tiles: Box<[TileKey]>,
    ) -> Result<Self, GlaImagesError> {
        let expected = layout.tile_count();
        let actual = tiles.len();
        if actual != expected as usize {
            return Err(GlaImagesError::TileCountMisMatch { expected, actual });
        }

        Ok(Self {
            format,
            layout,
            tiles,
        })
    }
}

#[derive(Debug)]
pub enum GlaImagesError {
    KeyPoolFull,
    InvalidKey {
        key: GlaImageKey,
    },
    KeyGenMisMatch {
        key: GlaImageKey,
    },
    TileCountMisMatch {
        expected: u32,
        actual: usize,
    },
    TileIndexOutOfBounds {
        key: GlaImageKey,
        tile_index: u32,
        tile_count: u32,
    },
    TileAllocFailed {
        source: TilesError,
    },
}

impl Display for GlaImagesError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::KeyPoolFull => f.write_str("image key pool is full"),
            Self::InvalidKey { key } => write!(f, "invalid image key {key:?}"),
            Self::KeyGenMisMatch { key } => {
                write!(f, "image key generation mismatch for {key:?}")
            }
            Self::TileCountMisMatch { expected, actual } => {
                write!(f, "image expected {expected} tiles, got {actual}")
            }
            Self::TileIndexOutOfBounds {
                key,
                tile_index,
                tile_count,
            } => write!(
                f,
                "tile index {tile_index} out of bounds for image {key:?} with {tile_count} tiles"
            ),
            Self::TileAllocFailed { source } => {
                write!(f, "tile allocation failed while creating image: {source}")
            }
        }
    }
}

pub struct GlaImages {
    images: Vec<Option<GlaImage>>,
    pool: Pool,
}

impl GlaImages {
    pub fn new() -> Self {
        Self {
            images: Vec::new(),
            pool: Pool::new(u32::MAX),
        }
    }

    pub fn remaining(&self) -> u32 {
        self.pool.remaining()
    }

    pub fn alloc(
        &mut self,
        format: GlaFormat,
        layout: GlaImageLayout,
        tiles: &mut Tiles,
        atlas_id: u8,
    ) -> Result<GlaImageKey, GlaImagesError> {
        if self.pool.remaining() == 0 {
            return Err(GlaImagesError::KeyPoolFull);
        }

        let tile_keys = tiles
            .alloc_batch_from(atlas_id, layout.tile_count())
            .map_err(|source| GlaImagesError::TileAllocFailed { source })?;
        self.insert(format, layout, tile_keys.into_boxed_slice())
    }

    pub fn insert(
        &mut self,
        format: GlaFormat,
        layout: GlaImageLayout,
        tiles: Box<[TileKey]>,
    ) -> Result<GlaImageKey, GlaImagesError> {
        let image = GlaImage::new(format, layout, tiles)?;
        self.insert_image(image)
    }

    fn insert_image(&mut self, image: GlaImage) -> Result<GlaImageKey, GlaImagesError> {
        let (index, generation) = self.pool.alloc()?;
        let key = GlaImageKey::new(index, generation);
        self.bind_image(key, image);
        Ok(key)
    }

    pub fn insert_invalid(
        &mut self,
        format: GlaFormat,
        layout: GlaImageLayout,
    ) -> Result<GlaImageKey, GlaImagesError> {
        let tiles = vec![TileKey::INVALID; layout.tile_count() as usize].into_boxed_slice();
        self.insert(format, layout, tiles)
    }

    pub fn free(&mut self, key: GlaImageKey) -> Result<(), GlaImagesError> {
        self.ensure_valid(key)?;
        self.images[key.index() as usize] = None;
        self.pool.free(key.index());
        Ok(())
    }

    pub fn ensure_valid(&self, key: GlaImageKey) -> Result<(), GlaImagesError> {
        self.pool
            .check(key.index(), key.generation())
            .then_some(())
            .ok_or(GlaImagesError::KeyGenMisMatch { key })?;

        self.images
            .get(key.index() as usize)
            .and_then(Option::as_ref)
            .ok_or(GlaImagesError::InvalidKey { key })?;

        Ok(())
    }

    pub fn get(&self, key: GlaImageKey) -> Result<&GlaImage, GlaImagesError> {
        self.ensure_valid(key)?;
        self.images[key.index() as usize]
            .as_ref()
            .ok_or(GlaImagesError::InvalidKey { key })
    }

    pub fn get_mut(&mut self, key: GlaImageKey) -> Result<&mut GlaImage, GlaImagesError> {
        self.ensure_valid(key)?;
        self.images[key.index() as usize]
            .as_mut()
            .ok_or(GlaImagesError::InvalidKey { key })
    }

    pub fn tile(&self, key: GlaImageKey, tile_index: u32) -> Result<TileKey, GlaImagesError> {
        let image = self.get(key)?;
        image
            .tiles
            .get(tile_index as usize)
            .copied()
            .ok_or(GlaImagesError::TileIndexOutOfBounds {
                key,
                tile_index,
                tile_count: image.layout.tile_count(),
            })
    }

    pub fn set_tile(
        &mut self,
        key: GlaImageKey,
        tile_index: u32,
        tile_key: TileKey,
    ) -> Result<(), GlaImagesError> {
        let image = self.get_mut(key)?;
        let tile_count = image.layout.tile_count();
        let tile = image.tiles.get_mut(tile_index as usize).ok_or(
            GlaImagesError::TileIndexOutOfBounds {
                key,
                tile_index,
                tile_count,
            },
        )?;
        *tile = tile_key;
        Ok(())
    }

    fn bind_image(&mut self, key: GlaImageKey, image: GlaImage) {
        let index = key.index() as usize;
        if self.images.len() <= index {
            self.images.resize_with(index + 1, || None);
        }

        debug_assert!(
            self.images[index].is_none(),
            "binding image key {key:?} over an occupied slot"
        );
        self.images[index] = Some(image);
    }
}

impl Default for GlaImages {
    fn default() -> Self {
        Self::new()
    }
}

impl From<PoolError> for GlaImagesError {
    fn from(error: PoolError) -> Self {
        match error {
            PoolError::Full => GlaImagesError::KeyPoolFull,
        }
    }
}

impl From<TilesError> for GlaImagesError {
    fn from(source: TilesError) -> Self {
        GlaImagesError::TileAllocFailed { source }
    }
}

#[cfg(test)]
mod tests {
    use super::{GlaImageLayout, GlaImages, GlaImagesError, IMAGE_TILE_SIZE};
    use gla_color::{ChannelCount, ChannelType, GlaFormat};
    use tile_key::TileKey;

    fn format() -> GlaFormat {
        GlaFormat {
            channel_count: ChannelCount::D4,
            channel_type: ChannelType::U8,
        }
    }

    #[test]
    fn layout_tile_count_covers_partial_edge_tiles() {
        assert_eq!(GlaImageLayout::new(0, 0).tile_count(), 0);
        assert_eq!(GlaImageLayout::new(1, 1).tile_count(), 1);
        assert_eq!(
            GlaImageLayout::new(IMAGE_TILE_SIZE + 1, IMAGE_TILE_SIZE).tile_count(),
            2
        );
    }

    #[test]
    fn insert_validates_tile_count() {
        let mut images = GlaImages::new();
        let err = images
            .insert(
                format(),
                GlaImageLayout::new(1, 1),
                Vec::new().into_boxed_slice(),
            )
            .unwrap_err();

        assert!(matches!(
            err,
            GlaImagesError::TileCountMisMatch {
                expected: 1,
                actual: 0
            }
        ));
    }

    #[test]
    fn free_reuses_image_slot_with_new_generation() {
        let mut images = GlaImages::new();
        let layout = GlaImageLayout::new(1, 1);
        let first_tile = TileKey::new(7, 0);
        let second_tile = TileKey::new(9, 0);

        let first = images
            .insert(format(), layout, vec![first_tile].into_boxed_slice())
            .unwrap();
        assert_eq!(images.tile(first, 0).unwrap(), first_tile);

        images.free(first).unwrap();
        assert!(matches!(
            images.get(first),
            Err(GlaImagesError::KeyGenMisMatch { .. })
        ));

        let second = images
            .insert(format(), layout, vec![second_tile].into_boxed_slice())
            .unwrap();
        assert_eq!(second.index(), first.index());
        assert_eq!(second.generation(), first.generation() + 1);
        assert_eq!(images.tile(second, 0).unwrap(), second_tile);
    }

}
