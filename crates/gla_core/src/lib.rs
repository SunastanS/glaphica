use std::marker::PhantomData;

pub const ATLAS_TILE_SIZE: u32 = 64;
pub const GUTTER_SIZE: u32 = 1;
pub const IMAGE_TILE_SIZE: u32 = ATLAS_TILE_SIZE - 2 * GUTTER_SIZE;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Vec2F32<S = ()> {
    pub x: f32,
    pub y: f32,
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

pub struct Vec2U<S = ()> {
    pub x: usize,
    pub y: usize,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenSpace {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CanvasSpace {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TileSpace {}

pub type ScreenCoordF = Vec2F32<ScreenSpace>;
pub type CanvasCoordF = Vec2F32<CanvasSpace>;
pub type TileCoordF = Vec2F32<TileSpace>;
pub type ScreenCoordU = Vec2U<ScreenSpace>;
pub type CanvasCoordU = Vec2U<CanvasSpace>;
pub type TileCoordU = Vec2U<TileSpace>;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Input<T> {
    pub time_ns: u64,
    pub position: T,
    pub pressure: f32,
    pub tilt: (f32, f32), // (tilt_x, tilt_y) in radians
    pub twist: f32,       // twist in radians, `+` = clockwise
}

pub type ScreenInput = Input<ScreenCoordF>;
pub type CanvasInput = Input<CanvasCoordF>;

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
    use super::{Pool, PoolError};

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
