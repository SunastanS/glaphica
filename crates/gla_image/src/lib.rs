use gla_color::GlaFormat;
pub use gla_core::IMAGE_TILE_SIZE;
use gla_core::{PixelRect, TileGrid, TileGridError, tile_rect, tiles_covering_rect};
use std::error::Error;
use std::fmt::{Display, Formatter};
use tile_key::{Tile, Tiles, TilesError};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GlaImageLayout {
    width_px: u32,
    height_px: u32,
    tile_count: u32,
} // line-first mapping index to tile logical arrangement

impl GlaImageLayout {
    pub fn new(width_px: u32, height_px: u32) -> Result<Self, ImageLayoutError> {
        if width_px == 0 || height_px == 0 {
            return Err(ImageLayoutError::Empty);
        }
        let tile_count = width_px
            .div_ceil(IMAGE_TILE_SIZE)
            .checked_mul(height_px.div_ceil(IMAGE_TILE_SIZE))
            .ok_or(ImageLayoutError::TooLarge)?;
        Ok(Self {
            width_px,
            height_px,
            tile_count,
        })
    }

    pub fn width_px(&self) -> u32 {
        self.width_px
    }

    pub fn height_px(&self) -> u32 {
        self.height_px
    }

    pub fn tile_count_x(&self) -> u32 {
        self.width_px.div_ceil(IMAGE_TILE_SIZE)
    }

    pub fn tile_count_y(&self) -> u32 {
        self.height_px.div_ceil(IMAGE_TILE_SIZE)
    }

    pub fn checked_tile_count(&self) -> Result<u32, ImageLayoutError> {
        Ok(self.tile_count)
    }

    pub fn tile_count(&self) -> u32 {
        self.tile_count
    }

    pub fn tile_grid(&self) -> TileGrid {
        TileGrid::new(self.width_px, self.height_px, IMAGE_TILE_SIZE)
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct ImageTileIndex(u32);

impl ImageTileIndex {
    pub(crate) const fn new_unchecked(value: u32) -> Self {
        Self(value)
    }

    pub const fn value(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum TileSetCoverage {
    Full,
    Tiles(Vec<ImageTileIndex>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TileSet {
    layout: GlaImageLayout,
    coverage: TileSetCoverage,
}

impl TileSet {
    pub fn empty(layout: GlaImageLayout) -> Self {
        Self {
            layout,
            coverage: TileSetCoverage::Tiles(Vec::new()),
        }
    }

    pub fn full(layout: GlaImageLayout) -> Self {
        Self {
            layout,
            coverage: TileSetCoverage::Full,
        }
    }

    pub fn from_indices<I>(layout: GlaImageLayout, tiles: I) -> Result<Self, ImageError>
    where
        I: IntoIterator<Item = u32>,
    {
        let mut tiles = tiles
            .into_iter()
            .map(|tile| layout.tile_index(tile))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self::from_tile_indices(layout, &mut tiles))
    }

    pub fn single(layout: GlaImageLayout, tile: u32) -> Result<Self, ImageError> {
        Self::from_indices(layout, [tile])
    }

    pub fn layout(&self) -> GlaImageLayout {
        self.layout
    }

    pub fn is_full(&self) -> bool {
        matches!(self.coverage, TileSetCoverage::Full)
    }

    pub fn tile_indices(&self) -> Option<&[ImageTileIndex]> {
        match &self.coverage {
            TileSetCoverage::Full => None,
            TileSetCoverage::Tiles(tiles) => Some(tiles),
        }
    }

    pub fn insert(&mut self, tile: u32) -> Result<(), ImageError> {
        let tile = self.layout.tile_index(tile)?;
        self.insert_tile_index(tile);
        Ok(())
    }

    fn insert_tile_index(&mut self, tile: ImageTileIndex) {
        match &mut self.coverage {
            TileSetCoverage::Full => {}
            TileSetCoverage::Tiles(tiles) => match tiles.binary_search(&tile) {
                Ok(_) => {}
                Err(index) => tiles.insert(index, tile),
            },
        }
    }

    pub fn is_empty(&self) -> bool {
        matches!(&self.coverage, TileSetCoverage::Tiles(tiles) if tiles.is_empty())
    }

    pub fn clear(&mut self) {
        match &mut self.coverage {
            TileSetCoverage::Full => self.coverage = TileSetCoverage::Tiles(Vec::new()),
            TileSetCoverage::Tiles(tiles) => tiles.clear(),
        }
    }

    pub fn union_assign(&mut self, other: &Self) {
        assert_eq!(
            self.layout, other.layout,
            "cannot union tile sets from different image layouts"
        );
        match &other.coverage {
            TileSetCoverage::Full => self.coverage = TileSetCoverage::Full,
            TileSetCoverage::Tiles(right) if right.is_empty() => {}
            TileSetCoverage::Tiles(right) => match &mut self.coverage {
                TileSetCoverage::Full => {}
                TileSetCoverage::Tiles(left) => {
                    left.extend(right.iter().copied());
                    left.sort_unstable();
                    left.dedup();
                }
            },
        }
    }

    fn from_tile_indices(layout: GlaImageLayout, tiles: &mut Vec<ImageTileIndex>) -> Self {
        tiles.sort_unstable();
        tiles.dedup();
        Self {
            layout,
            coverage: TileSetCoverage::Tiles(std::mem::take(tiles)),
        }
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

impl GlaImageLayout {
    pub fn tile_index(&self, tile_index: u32) -> Result<ImageTileIndex, ImageError> {
        let tile_count = self
            .checked_tile_count()
            .map_err(|source| ImageError::InvalidLayout { source })?;
        if tile_index >= tile_count {
            return Err(ImageError::TileIndexOutOfBounds {
                tile_index,
                tile_count,
            });
        }
        Ok(ImageTileIndex::new_unchecked(tile_index))
    }

    pub fn tile_rect(&self, tile_index: ImageTileIndex) -> Result<PixelRect, TileGridError> {
        tile_rect(self.tile_grid(), tile_index.value())
    }

    pub fn tile_set_covering_rect(&self, rect: PixelRect) -> Result<TileSet, TileGridError> {
        let mut tiles = tiles_covering_rect(self.tile_grid(), rect)?
            .into_iter()
            .map(ImageTileIndex::new_unchecked)
            .collect::<Vec<_>>();
        Ok(TileSet::from_tile_indices(*self, &mut tiles))
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

    fn layout(width_px: u32, height_px: u32) -> GlaImageLayout {
        GlaImageLayout::new(width_px, height_px).unwrap()
    }

    #[test]
    fn layout_tile_count_covers_partial_edge_tiles() {
        assert!(matches!(
            GlaImageLayout::new(0, 0),
            Err(ImageLayoutError::Empty)
        ));
        assert_eq!(layout(1, 1).tile_count(), 1);
        assert_eq!(layout(IMAGE_TILE_SIZE + 1, IMAGE_TILE_SIZE).tile_count(), 2);
    }

    #[test]
    fn checked_tile_count_rejects_overflowing_layout() {
        assert!(matches!(
            GlaImageLayout::new(u32::MAX, u32::MAX),
            Err(ImageLayoutError::TooLarge)
        ));
    }

    #[test]
    fn tile_set_sorts_and_deduplicates_tiles() {
        let set = TileSet::from_indices(layout(IMAGE_TILE_SIZE * 4, 1), [3, 1, 3, 2]).unwrap();
        let tiles = set
            .tile_indices()
            .unwrap()
            .iter()
            .map(|tile| tile.value())
            .collect::<Vec<_>>();

        assert_eq!(tiles, vec![1, 2, 3]);
        assert_eq!(set.layout(), layout(IMAGE_TILE_SIZE * 4, 1));
    }

    #[test]
    fn tile_set_rejects_indices_outside_layout() {
        let err = TileSet::single(layout(1, 1), 1).unwrap_err();

        assert!(matches!(
            err,
            ImageError::TileIndexOutOfBounds {
                tile_index: 1,
                tile_count: 1
            }
        ));
    }

    #[test]
    fn tile_set_covering_rect_preserves_layout() {
        let layout = layout(IMAGE_TILE_SIZE * 2, IMAGE_TILE_SIZE);
        let set = layout
            .tile_set_covering_rect(gla_core::PixelRect::new(
                IMAGE_TILE_SIZE - 1,
                0,
                IMAGE_TILE_SIZE + 1,
                1,
            ))
            .unwrap();
        let tiles = set
            .tile_indices()
            .unwrap()
            .iter()
            .map(|tile| tile.value())
            .collect::<Vec<_>>();

        assert_eq!(set.layout(), layout);
        assert_eq!(tiles, vec![0, 1]);
    }

    #[test]
    fn dense_allocate_reserves_full_valid_zero_tiles() {
        let mut tiles = Tiles::new();
        let atlas_id = new_test_atlas(&mut tiles);
        let image = DenseImage::allocate(
            format(),
            layout(IMAGE_TILE_SIZE + 1, IMAGE_TILE_SIZE),
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
        let mut image = DenseImage::allocate(format(), layout(1, 1), &mut tiles).unwrap();
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
        let mut image = DenseImage::allocate(format(), layout(1, 1), &mut tiles).unwrap();
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
        let image = CacheImage::new_invalid(format(), layout(1, 1)).unwrap();

        assert_eq!(image.tile_count(), 1);
        assert!(image.tile(0).unwrap().is_none());
    }

    #[test]
    fn cache_replace_tile_returns_previous_optional_owner() {
        let mut tiles = Tiles::new();
        let atlas_id = new_test_atlas(&mut tiles);
        let mut image = CacheImage::new_invalid(format(), layout(1, 1)).unwrap();
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
        let mut image = CacheImage::new_invalid(format(), layout(1, 1)).unwrap();
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
        let mut image = CacheImage::new_invalid(format(), layout(1, 1)).unwrap();
        let tile = tiles.reserve(atlas_id).unwrap();
        image.replace_tile(0, tile).unwrap();

        let taken = image.take_tile(0).unwrap().unwrap();

        assert!(image.tile(0).unwrap().is_none());
        tiles.release(taken);
    }
}
