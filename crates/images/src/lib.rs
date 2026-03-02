use atlas::key::{BackendId, TileKey};
use glaphica_constants::IMAGE_TILE_SIZE;

struct ImageLayout {
    size_x: u32,
    size_y: u32,
    tile_x: u32,
    tile_y: u32,
}

impl ImageLayout {
    pub fn new(size_x: u32, size_y: u32) -> Self {
        let tile_x = size_x.div_ceil(IMAGE_TILE_SIZE);
        let tile_y = size_y.div_ceil(IMAGE_TILE_SIZE);
        Self {
            size_x,
            size_y,
            tile_x,
            tile_y,
        }
    }

    pub const fn total_tiles(&self) -> u32 {
        self.tile_x * self.tile_y
    }

    pub fn pixel_to_index(&self, x: u32, y: u32) -> Result<usize, ImageLayoutError> {
        if x >= self.size_x || y >= self.size_y {
            Err(ImageLayoutError::OutOfBounds)
        } else {
            Ok((y / IMAGE_TILE_SIZE * self.tile_x + x / IMAGE_TILE_SIZE) as usize)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageLayoutError {
    OutOfBounds,
}

struct Image {
    /// layout.total_tiles = tile_keys.len()
    layout: ImageLayout,
    tile_keys: Box<[TileKey]>,
    backend: BackendId,
}

impl Image {
    pub fn new(layout: ImageLayout, backend: BackendId) -> Self {
        let tile_keys = vec![TileKey::EMPTY; layout.total_tiles() as usize].into_boxed_slice();
        Self {
            layout,
            tile_keys,
            backend,
        }
    }

    pub fn backend(&self) -> BackendId {
        self.backend
    }

    pub fn tile_keys(&self) -> &[TileKey] {
        &self.tile_keys
    }
}
