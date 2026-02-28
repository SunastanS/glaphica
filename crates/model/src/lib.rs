use bitvec::prelude::{BitVec, Lsb0};
use std::fmt;
// REFRACTORING:
// use 126 image + 2 gutter = 128 stride
// instead of 128 image + 2 gutter = 130 stride
pub const TILE_STRIDE: u32 = 128;
pub const TILE_GUTTER: u32 = 1;
pub const TILE_IMAGE: u32 = TILE_STRIDE - 2 * TILE_GUTTER;
pub const TILE_IMAGE_ORIGIN: u32 = TILE_GUTTER;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct TilePos {
    pub x: u32,
    pub y: u32,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct ImageLayout {
    /// We split it into a single struct
    /// because all layers in a document share the same layout
    size: TilePos,
    tiles_per_row: u32,
    tiles_per_column: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageLayoutError {
    TileIndexOutOfBounds,
    LayoutMismatch,
}

impl ImageLayout {
    pub fn new(size_x: u32, size_y: u32) -> Self {
        let tiles_per_row = size_x.div_ceil(TILE_IMAGE);
        let tiles_per_column = size_y.div_ceil(TILE_IMAGE);
        Self {
            size: TilePos {
                x: size_x,
                y: size_y,
            },
            tiles_per_row,
            tiles_per_column,
        }
    }

    pub const fn max_tiles(self) -> usize {
        self.tiles_per_row as usize * self.tiles_per_column as usize
    }

    pub fn tile_index(&self, tile: TilePos) -> Result<usize, ImageLayoutError> {
        if tile.x >= self.tiles_per_row || tile.y >= self.tiles_per_column {
            Err(ImageLayoutError::TileIndexOutOfBounds)
        } else {
            Ok((tile.y * self.tiles_per_row + tile.x) as usize)
        }
    }

    pub fn tile_pos(&self, index: usize) -> Result<TilePos, ImageLayoutError> {
        if index >= self.max_tiles() as usize {
            Err(ImageLayoutError::TileIndexOutOfBounds)
        } else {
            let x = index % self.tiles_per_row as usize;
            let y = index / self.tiles_per_row as usize;
            Ok(TilePos {
                x: x as u32,
                y: y as u32,
            })
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

/// Tile image structure with dirty bit tracking.
///
/// Replaces the old TileImageOld + VirtualImage architecture.
/// Uses BitVec for efficient dirty tile tracking instead of version numbers.
#[derive(Clone)]
pub struct TileImage<K> {
    layout: ImageLayout,
    tiles: Box<[K]>,
    dirty_bits: BitVec<usize, Lsb0>,
    dirty_count: usize,
}

impl<K: std::fmt::Debug> std::fmt::Debug for TileImage<K> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TileImage")
            .field("layout", &self.layout)
            .field("tiles", &self.tiles)
            .field("dirty_count", &self.dirty_count)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
            dirty_bits: BitVec::repeat(false, max_tiles as usize),
            dirty_count: 0,
        }
    }

    pub fn get_tile(&self, pos: TilePos) -> Result<&K, TileImageError> {
        let index = self.layout.tile_index(pos)?;
        Ok(&self.tiles[index])
    }

    pub fn set_tile(&mut self, tile: TilePos, tile_key: K) -> Result<(), TileImageError> {
        let index = self.layout.tile_index(tile)?;
        self.tiles[index] = tile_key;
        let was_dirty = self.dirty_bits[index];
        self.dirty_bits.set(index, true);
        self.dirty_count += !was_dirty as usize;
        Ok(())
    }

    pub fn iter_dirty_tile_keys(&self) -> impl Iterator<Item = K> {
        self.dirty_bits.iter_ones().map(|index| self.tiles[index])
    }

    /// Create TileImage from pixel dimensions (compatibility API).
    pub fn from_pixel_size(size_x: u32, size_y: u32) -> Self
    where
        K: EmptyKey,
    {
        let layout = ImageLayout::new(size_x, size_y);
        Self::new(layout)
    }

    /// Get tile at specific tile coordinates (compatibility API).
    pub fn get_tile_at(&self, tile_x: u32, tile_y: u32) -> Result<&K, TileImageError> {
        let pos = TilePos {
            x: tile_x,
            y: tile_y,
        };
        self.get_tile(pos)
    }

    /// Set tile at specific tile coordinates (compatibility API).
    pub fn set_tile_at(&mut self, tile_x: u32, tile_y: u32, key: K) -> Result<(), TileImageError> {
        let pos = TilePos {
            x: tile_x,
            y: tile_y,
        };
        self.set_tile(pos, key)
    }

    /// Iterate over all tiles with their coordinates.
    pub fn iter_all_tiles(&self) -> impl Iterator<Item = (u32, u32, K)> + '_
    where
        K: Copy,
    {
        let tiles_per_row = self.layout.tiles_per_row;
        self.tiles.iter().enumerate().map(move |(i, &key)| {
            let x = (i % tiles_per_row as usize) as u32;
            let y = (i / tiles_per_row as usize) as u32;
            (x, y, key)
        })
    }

    /// Get the pixel dimensions of the image.
    pub fn pixel_size(&self) -> (u32, u32) {
        (self.layout.size.x, self.layout.size.y)
    }

    /// Get tiles per row.
    pub fn tiles_per_row(&self) -> u32 {
        self.layout.tiles_per_row
    }

    /// Get tiles per column.
    pub fn tiles_per_column(&self) -> u32 {
        self.layout.tiles_per_column
    }

    /// Iterate over all tiles with their coordinates (compatibility API).
    pub fn iter_tiles(&self) -> impl Iterator<Item = (u32, u32, &K)> + '_ {
        let tiles_per_row = self.layout.tiles_per_row as usize;
        self.tiles.iter().enumerate().filter_map(move |(i, key)| {
            let x = (i % tiles_per_row) as u32;
            let y = (i / tiles_per_row) as u32;
            Some((x, y, key))
        })
    }
}
