use std::error::Error;
use std::fmt::{Display, Formatter};
use std::marker::PhantomData;

use serde::{Deserialize, Serialize};

pub const ATLAS_TILE_SIZE: u32 = 64;
pub const GUTTER_SIZE: u32 = 1;
pub const IMAGE_TILE_SIZE: u32 = ATLAS_TILE_SIZE - 2 * GUTTER_SIZE;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Vec2F32<S = ()> {
    pub x: f32,
    pub y: f32,
    #[serde(skip)]
    _phantom: PhantomData<S>,
}

impl<S> Vec2F32<S> {
    pub fn new(x: f32, y: f32) -> Self {
        Self {
            x,
            y,
            _phantom: PhantomData::<S>,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Vec2U<S = ()> {
    pub x: usize,
    pub y: usize,
    #[serde(skip)]
    _phantom: PhantomData<S>,
}

impl<S> Vec2U<S> {
    pub fn new(x: usize, y: usize) -> Self {
        Self {
            x,
            y,
            _phantom: PhantomData::<S>,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScreenSpace {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CanvasSpace {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TileSpace {}

pub type ScreenCoordF = Vec2F32<ScreenSpace>;
pub type CanvasCoordF = Vec2F32<CanvasSpace>;
pub type TileCoordF = Vec2F32<TileSpace>;
pub type ScreenCoordU = Vec2U<ScreenSpace>;
pub type CanvasCoordU = Vec2U<CanvasSpace>;
pub type TileCoordU = Vec2U<TileSpace>;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Input<T> {
    pub time_ns: u64,
    pub position: T,
    pub pressure: f32,
    pub tilt: (f32, f32), // (tilt_x, tilt_y) in radians
    pub twist: f32,       // twist in radians, `+` = clockwise
}

pub type ScreenInput = Input<ScreenCoordF>;
pub type CanvasInput = Input<CanvasCoordF>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PixelRect {
    pub min_x: u32,
    pub min_y: u32,
    pub max_x: u32,
    pub max_y: u32,
}

impl PixelRect {
    pub const fn new(min_x: u32, min_y: u32, max_x: u32, max_y: u32) -> Self {
        Self {
            min_x,
            min_y,
            max_x,
            max_y,
        }
    }

    pub fn is_empty(self) -> bool {
        self.min_x >= self.max_x || self.min_y >= self.max_y
    }

    pub fn intersect(self, other: Self) -> Option<Self> {
        let rect = Self {
            min_x: self.min_x.max(other.min_x),
            min_y: self.min_y.max(other.min_y),
            max_x: self.max_x.min(other.max_x),
            max_y: self.max_y.min(other.max_y),
        };
        (!rect.is_empty()).then_some(rect)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TileGrid {
    pub width_px: u32,
    pub height_px: u32,
    pub tile_size: u32,
}

impl TileGrid {
    pub const fn new(width_px: u32, height_px: u32, tile_size: u32) -> Self {
        Self {
            width_px,
            height_px,
            tile_size,
        }
    }

    pub fn image_rect(self) -> PixelRect {
        PixelRect::new(0, 0, self.width_px, self.height_px)
    }

    pub fn tile_count_x(self) -> Result<u32, TileGridError> {
        self.validate()?;
        Ok(self.width_px.div_ceil(self.tile_size))
    }

    pub fn tile_count_y(self) -> Result<u32, TileGridError> {
        self.validate()?;
        Ok(self.height_px.div_ceil(self.tile_size))
    }

    pub fn checked_tile_count(self) -> Result<u32, TileGridError> {
        self.tile_count_x()?
            .checked_mul(self.tile_count_y()?)
            .ok_or(TileGridError::TileCountOverflow)
    }

    fn validate(self) -> Result<(), TileGridError> {
        if self.tile_size == 0 {
            return Err(TileGridError::InvalidTileSize);
        }
        if self.width_px == 0 || self.height_px == 0 {
            return Err(TileGridError::Empty);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TileGridError {
    Empty,
    InvalidTileSize,
    TileCountOverflow,
    TileIndexOutOfBounds { tile_index: u32, tile_count: u32 },
}

impl Display for TileGridError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => f.write_str("tile grid must contain pixels"),
            Self::InvalidTileSize => f.write_str("tile grid tile size must be non-zero"),
            Self::TileCountOverflow => f.write_str("tile grid tile count overflows u32"),
            Self::TileIndexOutOfBounds {
                tile_index,
                tile_count,
            } => write!(
                f,
                "tile index {tile_index} out of bounds for grid with {tile_count} tiles"
            ),
        }
    }
}

impl Error for TileGridError {}

pub fn tile_rect(grid: TileGrid, tile_index: u32) -> Result<PixelRect, TileGridError> {
    let tile_count = grid.checked_tile_count()?;
    if tile_index >= tile_count {
        return Err(TileGridError::TileIndexOutOfBounds {
            tile_index,
            tile_count,
        });
    }

    let tiles_x = grid.tile_count_x()?;
    let tile_x = tile_index % tiles_x;
    let tile_y = tile_index / tiles_x;
    let min_x = tile_x * grid.tile_size;
    let min_y = tile_y * grid.tile_size;
    Ok(PixelRect::new(
        min_x,
        min_y,
        min_x.saturating_add(grid.tile_size).min(grid.width_px),
        min_y.saturating_add(grid.tile_size).min(grid.height_px),
    ))
}

pub fn tiles_covering_rect(grid: TileGrid, rect: PixelRect) -> Result<Vec<u32>, TileGridError> {
    grid.checked_tile_count()?;
    let Some(rect) = rect.intersect(grid.image_rect()) else {
        return Ok(Vec::new());
    };

    let tiles_x = grid.tile_count_x()?;
    let start_x = rect.min_x / grid.tile_size;
    let start_y = rect.min_y / grid.tile_size;
    let end_x = (rect.max_x - 1) / grid.tile_size;
    let end_y = (rect.max_y - 1) / grid.tile_size;
    let tile_count = (end_x - start_x + 1)
        .checked_mul(end_y - start_y + 1)
        .ok_or(TileGridError::TileCountOverflow)?;
    let mut tiles = Vec::with_capacity(tile_count as usize);

    for tile_y in start_y..=end_y {
        for tile_x in start_x..=end_x {
            tiles.push(
                tile_y
                    .checked_mul(tiles_x)
                    .and_then(|row| row.checked_add(tile_x))
                    .ok_or(TileGridError::TileCountOverflow)?,
            );
        }
    }
    Ok(tiles)
}

#[derive(Debug)]
pub struct Pool {
    total: u32,
    used: u32,
    next: u32,
    free: Vec<u32>,
    generations: Vec<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PoolError {
    Full,
}

impl Pool {
    pub fn new(total: u32) -> Self {
        Self {
            total,
            next: 0,
            free: Vec::new(),
            used: 0,
            generations: Vec::new(),
        }
    }

    pub fn check(&self, index: u32, generation: u32) -> bool {
        if let Some(pool_generation) = self.generations.get(index as usize) {
            *pool_generation == generation
        } else {
            false
        }
    }

    pub fn alloc(&mut self) -> Result<(u32, u32), PoolError> {
        if let Some(index) = self.free.pop() {
            self.used = self.used.checked_add(1).ok_or(PoolError::Full)?;
            return Ok((index, self.generations[index as usize]));
        }

        if self.next == self.total {
            return Err(PoolError::Full);
        }

        let index = self.next;
        self.next = self.next.checked_add(1).ok_or(PoolError::Full)?;
        self.generations.push(1);
        self.used = self.used.checked_add(1).ok_or(PoolError::Full)?;
        Ok((index, 1))
    }

    pub fn remaining(&self) -> u32 {
        self.total - self.used
    }

    /// caller should check before free
    pub fn free(&mut self, index: u32) {
        assert!(!self.free.contains(&index), "double free of index {index}");
        assert!(index < self.next, "free of never-allocated index {index}");

        let generation = self.generations.get_mut(index as usize).unwrap();
        *generation = (*generation).wrapping_add(1);
        if *generation == 0 {
            *generation = 1;
        }

        self.free.push(index);
        self.used -= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::{
        IMAGE_TILE_SIZE, PixelRect, Pool, PoolError, TileGrid, TileGridError, tile_rect,
        tiles_covering_rect,
    };

    #[test]
    fn tile_rect_uses_row_major_indices_and_clamps_edge_tiles() {
        let grid = TileGrid::new(125, 63, IMAGE_TILE_SIZE);

        assert_eq!(
            tile_rect(grid, 0).unwrap(),
            PixelRect::new(0, 0, IMAGE_TILE_SIZE, IMAGE_TILE_SIZE)
        );
        assert_eq!(
            tile_rect(grid, 2).unwrap(),
            PixelRect::new(IMAGE_TILE_SIZE * 2, 0, 125, IMAGE_TILE_SIZE)
        );
        assert_eq!(
            tile_rect(grid, 5).unwrap(),
            PixelRect::new(IMAGE_TILE_SIZE * 2, IMAGE_TILE_SIZE, 125, 63)
        );
    }

    #[test]
    fn tiles_covering_rect_returns_row_major_coverage() {
        let grid = TileGrid::new(IMAGE_TILE_SIZE * 3, IMAGE_TILE_SIZE * 2, IMAGE_TILE_SIZE);
        let rect = PixelRect::new(
            IMAGE_TILE_SIZE - 1,
            1,
            IMAGE_TILE_SIZE + 1,
            IMAGE_TILE_SIZE + 1,
        );

        assert_eq!(tiles_covering_rect(grid, rect).unwrap(), vec![0, 1, 3, 4]);
    }

    #[test]
    fn tiles_covering_rect_clips_to_image_bounds() {
        let grid = TileGrid::new(IMAGE_TILE_SIZE, IMAGE_TILE_SIZE, IMAGE_TILE_SIZE);

        assert_eq!(
            tiles_covering_rect(
                grid,
                PixelRect::new(
                    IMAGE_TILE_SIZE,
                    IMAGE_TILE_SIZE,
                    IMAGE_TILE_SIZE + 1,
                    IMAGE_TILE_SIZE + 1
                )
            )
            .unwrap(),
            Vec::<u32>::new()
        );
        assert_eq!(
            tiles_covering_rect(
                grid,
                PixelRect::new(
                    IMAGE_TILE_SIZE - 1,
                    IMAGE_TILE_SIZE - 1,
                    IMAGE_TILE_SIZE + 5,
                    IMAGE_TILE_SIZE + 5
                )
            )
            .unwrap(),
            vec![0]
        );
    }

    #[test]
    fn tile_grid_rejects_invalid_dimensions() {
        assert!(matches!(
            tile_rect(TileGrid::new(0, IMAGE_TILE_SIZE, IMAGE_TILE_SIZE), 0),
            Err(TileGridError::Empty)
        ));
        assert!(matches!(
            tile_rect(TileGrid::new(IMAGE_TILE_SIZE, IMAGE_TILE_SIZE, 0), 0),
            Err(TileGridError::InvalidTileSize)
        ));
        assert!(matches!(
            tile_rect(
                TileGrid::new(IMAGE_TILE_SIZE, IMAGE_TILE_SIZE, IMAGE_TILE_SIZE),
                1
            ),
            Err(TileGridError::TileIndexOutOfBounds {
                tile_index: 1,
                tile_count: 1
            })
        ));
    }

    #[test]
    fn alloc_reuses_freed_slots_after_pool_reaches_capacity() {
        let mut pool = Pool::new(1);
        assert_eq!(pool.alloc().unwrap(), (0, 1));
        assert!(matches!(pool.alloc(), Err(PoolError::Full)));

        pool.free(0);

        assert_eq!(pool.alloc().unwrap(), (0, 2));
        assert_eq!(pool.remaining(), 0);
    }

    #[test]
    #[should_panic(expected = "double free of index 0")]
    fn free_panics_on_double_free() {
        let mut pool = Pool::new(1);
        let (index, _) = pool.alloc().unwrap();
        pool.free(index);
        pool.free(index);
    }
}
