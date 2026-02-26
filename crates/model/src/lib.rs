// REFRACTORING:
// use 126 image + 2 gutter = 128 stride
// instead of 128 image + 2 gutter = 130 stride
pub const TILE_STRIDE: u32 = 128;
pub const TILE_GUTTER: u32 = 1;
pub const TILE_IMAGE: u32 = TILE_STRIDE - 2 * TILE_GUTTER;
pub const TILE_IMAGE_ORIGIN: u32 = TILE_GUTTER;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct ImageLayout {
    /// We split it into a single struct
    /// because all layers in a document share the same layout
    size_x: u32,
    size_y: u32,
    tiles_per_row: u32,
    tiles_per_column: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageLayoutError {
    TileIndexOutOfBounds,
}

impl ImageLayout {
    pub fn new(size_x: u32, size_y: u32) -> Self {
        let tiles_per_row = size_x.div_ceil(TILE_IMAGE);
        let tiles_per_column = size_y.div_ceil(TILE_IMAGE);
        Self {
            size_x,
            size_y,
            tiles_per_row,
            tiles_per_column,
        }
    }

    pub const fn max_tiles(self) -> u32 {
        self.tiles_per_row * self.tiles_per_column
    }

    fn tile_index(&self, tile_x: u32, tile_y: u32) -> Result<usize, ImageLayoutError> {
        if tile_x >= self.tiles_per_row || tile_y >= self.tiles_per_column {
            Err(ImageLayoutError::TileIndexOutOfBounds)
        } else {
            Ok((tile_y * self.tiles_per_row + tile_x) as usize)
        }
    }
}

pub trait EmptyKey: Copy + PartialEq {
    const EMPTY: Self;
    #[inline]
    fn is_empty(self) -> bool {
        self == Self::EMPTY
    }
}

pub struct TileImage<K> {
    layout: ImageLayout,
    tiles: Box<[K]>, //tiles.len() == layout.max_tiles()
}

pub enum TileImageError {
    Layout(ImageLayoutError),
}

impl From<ImageLayoutError> for TileImageError {
    fn from(err: ImageLayoutError) -> Self {
        TileImageError::Layout(err)
    }
}

impl<K: Copy + EmptyKey> TileImage<K> {
    pub fn new(layout: ImageLayout) -> Self {
        let max_tiles = layout.max_tiles();
        Self {
            layout,
            tiles: vec![K::EMPTY; max_tiles as usize].into_boxed_slice(),
        }
    }

    pub fn get_tile(&self, tile_x: u32, tile_y: u32) -> Result<&K, TileImageError> {
        let index = self.layout.tile_index(tile_x, tile_y)?;
        Ok(&self.tiles[index])
    }

    pub fn set_tile(
        &mut self,
        tile_x: u32,
        tile_y: u32,
        tile_key: K,
    ) -> Result<(), TileImageError> {
        let index = self.layout.tile_index(tile_x, tile_y)?;
        self.tiles[index] = tile_key;
        Ok(())
    }
}
