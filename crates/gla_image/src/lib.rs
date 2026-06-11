use gla_color::GlaFormat;
pub use gla_core::IMAGE_TILE_SIZE;
use std::error::Error;
use std::fmt::{Display, Formatter};
use tile_key::{Tile, Tiles, TilesError};

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

    pub fn checked_tile_count(&self) -> Result<u32, ImageLayoutError> {
        if self.width_px == 0 || self.height_px == 0 {
            return Err(ImageLayoutError::Empty);
        }
        self.tile_count_x()
            .checked_mul(self.tile_count_y())
            .ok_or(ImageLayoutError::TooLarge)
    }

    pub fn tile_count(&self) -> u32 {
        match self.checked_tile_count() {
            Ok(tile_count) => tile_count,
            Err(ImageLayoutError::Empty) => 0,
            Err(ImageLayoutError::TooLarge) => {
                panic!("image layout tile count must fit in u32")
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageLayoutError {
    Empty,
    TooLarge,
}

impl Display for ImageLayoutError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => f.write_str("image layout must contain at least one tile"),
            Self::TooLarge => f.write_str("image layout tile count overflows u32"),
        }
    }
}

impl Error for ImageLayoutError {}

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

#[derive(Debug)]
pub enum ImageError {
    InvalidLayout { source: ImageLayoutError },
    TileIndexOutOfBounds { tile_index: u32, tile_count: u32 },
    TileAllocFailed { source: TilesError },
}

impl Display for ImageError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidLayout { source } => write!(f, "invalid image layout: {source}"),
            Self::TileIndexOutOfBounds {
                tile_index,
                tile_count,
            } => write!(
                f,
                "tile index {tile_index} out of bounds for image with {tile_count} tiles"
            ),
            Self::TileAllocFailed { source } => {
                write!(f, "tile allocation failed while creating image: {source}")
            }
        }
    }
}

impl Error for ImageError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidLayout { source } => Some(source),
            Self::TileAllocFailed { source } => Some(source),
            _ => None,
        }
    }
}

impl From<TilesError> for ImageError {
    fn from(source: TilesError) -> Self {
        ImageError::TileAllocFailed { source }
    }
}

#[derive(Debug)]
pub struct TileReplaceError {
    kind: ImageError,
    tile: Tile,
}

impl TileReplaceError {
    fn new(kind: ImageError, tile: Tile) -> Self {
        Self { kind, tile }
    }

    pub fn kind(&self) -> &ImageError {
        &self.kind
    }

    pub fn into_tile(self) -> Tile {
        self.tile
    }

    pub fn into_parts(self) -> (ImageError, Tile) {
        (self.kind, self.tile)
    }
}

impl Display for TileReplaceError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        Display::fmt(&self.kind, f)
    }
}

impl Error for TileReplaceError {}

#[derive(Debug)]
pub struct DenseImage {
    format: GlaFormat,
    layout: GlaImageLayout,
    tiles: Box<[Tile]>,
}

impl DenseImage {
    pub fn allocate(
        format: GlaFormat,
        layout: GlaImageLayout,
        tiles: &mut Tiles,
    ) -> Result<Self, ImageError> {
        let tile_count = checked_tile_count(layout)?;
        let image_tiles = tiles.reserve_batch_for_format(format, tile_count)?;
        Ok(Self {
            format,
            layout,
            tiles: image_tiles.into_boxed_slice(),
        })
    }

    pub fn format(&self) -> GlaFormat {
        self.format
    }

    pub fn layout(&self) -> GlaImageLayout {
        self.layout
    }

    pub fn tile_count(&self) -> u32 {
        self.tiles
            .len()
            .try_into()
            .expect("dense image tile count must fit in u32")
    }

    pub fn tile(&self, tile_index: u32) -> Result<&Tile, ImageError> {
        let tile_count = self.tile_count();
        self.tiles
            .get(tile_index as usize)
            .ok_or(ImageError::TileIndexOutOfBounds {
                tile_index,
                tile_count,
            })
    }

    pub fn tile_mut(&mut self, tile_index: u32) -> Result<&mut Tile, ImageError> {
        let tile_count = self.tile_count();
        self.tiles
            .get_mut(tile_index as usize)
            .ok_or(ImageError::TileIndexOutOfBounds {
                tile_index,
                tile_count,
            })
    }

    pub fn replace_tile(
        &mut self,
        tile_index: u32,
        new_tile: Tile,
    ) -> Result<Tile, TileReplaceError> {
        let tile_count = self.tile_count();
        let Some(tile) = self.tiles.get_mut(tile_index as usize) else {
            return Err(TileReplaceError::new(
                ImageError::TileIndexOutOfBounds {
                    tile_index,
                    tile_count,
                },
                new_tile,
            ));
        };
        Ok(std::mem::replace(tile, new_tile))
    }

    pub fn into_tiles(self) -> Box<[Tile]> {
        self.tiles
    }

    pub fn release_tiles(self, tiles: &mut Tiles) {
        for tile in self.tiles.into_vec() {
            tiles.release(tile);
        }
    }
}

#[derive(Debug)]
pub struct CacheImage {
    format: GlaFormat,
    layout: GlaImageLayout,
    tiles: Box<[Option<Tile>]>,
}

impl CacheImage {
    pub fn new_invalid(format: GlaFormat, layout: GlaImageLayout) -> Result<Self, ImageError> {
        let tile_count = checked_tile_count(layout)?;
        let tiles = (0..tile_count)
            .map(|_| None)
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Ok(Self {
            format,
            layout,
            tiles,
        })
    }

    pub fn format(&self) -> GlaFormat {
        self.format
    }

    pub fn layout(&self) -> GlaImageLayout {
        self.layout
    }

    pub fn tile_count(&self) -> u32 {
        self.tiles
            .len()
            .try_into()
            .expect("cache image tile count must fit in u32")
    }

    pub fn tile(&self, tile_index: u32) -> Result<Option<&Tile>, ImageError> {
        let tile_count = self.tile_count();
        self.tiles
            .get(tile_index as usize)
            .map(Option::as_ref)
            .ok_or(ImageError::TileIndexOutOfBounds {
                tile_index,
                tile_count,
            })
    }

    pub fn tile_mut(&mut self, tile_index: u32) -> Result<Option<&mut Tile>, ImageError> {
        let tile_count = self.tile_count();
        self.tiles
            .get_mut(tile_index as usize)
            .map(Option::as_mut)
            .ok_or(ImageError::TileIndexOutOfBounds {
                tile_index,
                tile_count,
            })
    }

    pub fn replace_tile(
        &mut self,
        tile_index: u32,
        new_tile: Tile,
    ) -> Result<Option<Tile>, TileReplaceError> {
        let tile_count = self.tile_count();
        let Some(slot) = self.tiles.get_mut(tile_index as usize) else {
            return Err(TileReplaceError::new(
                ImageError::TileIndexOutOfBounds {
                    tile_index,
                    tile_count,
                },
                new_tile,
            ));
        };
        Ok(slot.replace(new_tile))
    }

    pub fn take_tile(&mut self, tile_index: u32) -> Result<Option<Tile>, ImageError> {
        let tile_count = self.tile_count();
        let slot =
            self.tiles
                .get_mut(tile_index as usize)
                .ok_or(ImageError::TileIndexOutOfBounds {
                    tile_index,
                    tile_count,
                })?;
        Ok(slot.take())
    }

    pub fn into_tiles(self) -> Box<[Option<Tile>]> {
        self.tiles
    }

    pub fn release_tiles(self, tiles: &mut Tiles) {
        for tile in self.tiles.into_vec() {
            tiles.release_optional(tile);
        }
    }
}

fn checked_tile_count(layout: GlaImageLayout) -> Result<u32, ImageError> {
    layout
        .checked_tile_count()
        .map_err(|source| ImageError::InvalidLayout { source })
}

#[cfg(test)]
mod tests {
    use super::{
        CacheImage, DenseImage, GlaImageLayout, IMAGE_TILE_SIZE, ImageError, ImageLayoutError,
        TileSet,
    };
    use atlas::{AtlasLayout, NoAtlasTextures};
    use gla_color::{ChannelCount, ChannelType, GlaFormat};
    use tile_key::{TileReadRef, Tiles};

    fn format() -> GlaFormat {
        GlaFormat {
            channel_count: ChannelCount::D4,
            channel_type: ChannelType::U8,
        }
    }

    fn new_test_atlas(tiles: &mut Tiles) -> u8 {
        let mut textures = NoAtlasTextures;
        tiles
            .new_atlas(AtlasLayout::TINY8, format(), &mut textures)
            .unwrap()
    }

    #[test]
    fn layout_tile_count_covers_partial_edge_tiles() {
        assert_eq!(GlaImageLayout::new(0, 0).tile_count(), 0);
        assert!(matches!(
            GlaImageLayout::new(0, 0).checked_tile_count(),
            Err(ImageLayoutError::Empty)
        ));
        assert_eq!(GlaImageLayout::new(1, 1).tile_count(), 1);
        assert_eq!(
            GlaImageLayout::new(IMAGE_TILE_SIZE + 1, IMAGE_TILE_SIZE).tile_count(),
            2
        );
    }

    #[test]
    fn checked_tile_count_rejects_overflowing_layout() {
        assert!(matches!(
            GlaImageLayout::new(u32::MAX, u32::MAX).checked_tile_count(),
            Err(ImageLayoutError::TooLarge)
        ));
    }

    #[test]
    fn tile_set_sorts_and_deduplicates_tiles() {
        assert_eq!(TileSet::tiles([3, 1, 3, 2]), TileSet::Tiles(vec![1, 2, 3]));
    }

    #[test]
    fn dense_allocate_reserves_full_valid_zero_tiles() {
        let mut tiles = Tiles::new();
        let atlas_id = new_test_atlas(&mut tiles);
        let image = DenseImage::allocate(
            format(),
            GlaImageLayout::new(IMAGE_TILE_SIZE + 1, IMAGE_TILE_SIZE),
            &mut tiles,
        )
        .unwrap();

        assert_eq!(image.tile_count(), 2);
        assert_eq!(tiles.atlas(atlas_id).unwrap().remaining(), 256);
        assert_eq!(
            tiles.read_ref(image.tile(0).unwrap()).unwrap(),
            TileReadRef::Zero
        );
        assert_eq!(
            tiles.read_ref(image.tile(1).unwrap()).unwrap(),
            TileReadRef::Zero
        );
    }

    #[test]
    fn dense_replace_tile_moves_previous_owner_out() {
        let mut tiles = Tiles::new();
        let atlas_id = new_test_atlas(&mut tiles);
        let mut image =
            DenseImage::allocate(format(), GlaImageLayout::new(1, 1), &mut tiles).unwrap();
        let replacement = tiles.reserve(atlas_id).unwrap();

        let old = image.replace_tile(0, replacement).unwrap();

        tiles.release(old);
        assert_eq!(
            tiles.read_ref(image.tile(0).unwrap()).unwrap(),
            TileReadRef::Zero
        );
    }

    #[test]
    fn dense_replace_tile_returns_new_owner_on_out_of_bounds() {
        let mut tiles = Tiles::new();
        let atlas_id = new_test_atlas(&mut tiles);
        let mut image =
            DenseImage::allocate(format(), GlaImageLayout::new(1, 1), &mut tiles).unwrap();
        let replacement = tiles.reserve(atlas_id).unwrap();

        let err = image.replace_tile(1, replacement).unwrap_err();

        assert!(matches!(
            err.kind(),
            ImageError::TileIndexOutOfBounds {
                tile_index: 1,
                tile_count: 1
            }
        ));
        tiles.release(err.into_tile());
    }

    #[test]
    fn cache_invalid_image_starts_with_cache_misses() {
        let image = CacheImage::new_invalid(format(), GlaImageLayout::new(1, 1)).unwrap();

        assert_eq!(image.tile_count(), 1);
        assert!(image.tile(0).unwrap().is_none());
    }

    #[test]
    fn cache_replace_tile_returns_previous_optional_owner() {
        let mut tiles = Tiles::new();
        let atlas_id = new_test_atlas(&mut tiles);
        let mut image = CacheImage::new_invalid(format(), GlaImageLayout::new(1, 1)).unwrap();
        let first = tiles.reserve(atlas_id).unwrap();
        let second = tiles.reserve(atlas_id).unwrap();

        assert!(image.replace_tile(0, first).unwrap().is_none());
        let old = image.replace_tile(0, second).unwrap().unwrap();

        tiles.release(old);
        assert_eq!(
            tiles.read_ref(image.tile(0).unwrap().unwrap()).unwrap(),
            TileReadRef::Zero
        );
    }

    #[test]
    fn cache_replace_tile_returns_new_owner_on_out_of_bounds() {
        let mut tiles = Tiles::new();
        let atlas_id = new_test_atlas(&mut tiles);
        let mut image = CacheImage::new_invalid(format(), GlaImageLayout::new(1, 1)).unwrap();
        let replacement = tiles.reserve(atlas_id).unwrap();

        let err = image.replace_tile(1, replacement).unwrap_err();

        assert!(matches!(
            err.kind(),
            ImageError::TileIndexOutOfBounds {
                tile_index: 1,
                tile_count: 1
            }
        ));
        tiles.release(err.into_tile());
    }

    #[test]
    fn cache_take_tile_clears_slot() {
        let mut tiles = Tiles::new();
        let atlas_id = new_test_atlas(&mut tiles);
        let mut image = CacheImage::new_invalid(format(), GlaImageLayout::new(1, 1)).unwrap();
        let tile = tiles.reserve(atlas_id).unwrap();
        image.replace_tile(0, tile).unwrap();

        let taken = image.take_tile(0).unwrap().unwrap();

        assert!(image.tile(0).unwrap().is_none());
        tiles.release(taken);
    }

    #[test]
    fn image_constructors_reject_zero_size_layouts() {
        let mut tiles = Tiles::new();
        let _atlas_id = new_test_atlas(&mut tiles);
        let dense = DenseImage::allocate(
            format(),
            GlaImageLayout::new(0, IMAGE_TILE_SIZE),
            &mut tiles,
        )
        .unwrap_err();
        let cache =
            CacheImage::new_invalid(format(), GlaImageLayout::new(IMAGE_TILE_SIZE, 0)).unwrap_err();

        assert!(matches!(
            dense,
            ImageError::InvalidLayout {
                source: ImageLayoutError::Empty
            }
        ));
        assert!(matches!(
            cache,
            ImageError::InvalidLayout {
                source: ImageLayoutError::Empty
            }
        ));
    }

    #[test]
    fn image_constructors_reject_overflowing_layouts_before_allocation() {
        let mut tiles = Tiles::new();
        let layout = GlaImageLayout::new(u32::MAX, u32::MAX);
        let dense = DenseImage::allocate(format(), layout, &mut tiles).unwrap_err();
        let cache = CacheImage::new_invalid(format(), layout).unwrap_err();

        assert!(matches!(
            dense,
            ImageError::InvalidLayout {
                source: ImageLayoutError::TooLarge
            }
        ));
        assert!(matches!(
            cache,
            ImageError::InvalidLayout {
                source: ImageLayoutError::TooLarge
            }
        ));
    }
}
