use gla_core::{ATLAS_TILE_SIZE, Pool};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TileAddress {
    pub layer: usize,
    pub tile_x: usize,
    pub tile_y: usize,
}

impl TileAddress {
    pub const fn offset_x(self) -> usize {
        self.tile_x * ATLAS_TILE_SIZE as usize
    }

    pub const fn offset_y(self) -> usize {
        self.tile_y * ATLAS_TILE_SIZE as usize
    }
}

/// Binding(u64):
/// - [0,  32) position
///     - [0,  24) tile index (0xFF_FFFF = empty)
///     - [24, 32) atlas id
/// - [32, 64) tile generation (when a tile is empty, its generation is meaningless)

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(transparent)]
pub struct Position(u32);

impl Position {
    const TILE_BITS: u32 = 24;
    const ATLAS_SHIFT: u32 = Self::TILE_BITS;
    // const ATLAS_BITS: u32 = 8;

    const TILE_MASK: u32 = (1 << Self::TILE_BITS) - 1;

    #[inline]
    pub fn new(atlas_id: u8, tile_index: u32) -> Self {
        debug_assert!(
            tile_index < (1 << 20),
            "tile_index out of range for largest atlas"
        );
        debug_assert_ne!(
            tile_index,
            Self::TILE_MASK,
            "tile_index collides with empty sentinel"
        );
        Self((atlas_id as u32) << Self::ATLAS_SHIFT | tile_index)
    }

    #[inline]
    pub fn empty(atlas_id: u8) -> Self {
        Self((atlas_id as u32) << Self::ATLAS_SHIFT | Self::TILE_MASK)
    }

    #[inline]
    pub fn is_empty(self) -> bool {
        (self.0 & Self::TILE_MASK) == Self::TILE_MASK
    }

    #[inline]
    pub fn atlas_id(self) -> u8 {
        (self.0 >> Self::ATLAS_SHIFT) as u8
    }

    #[inline]
    pub fn tile_index(self) -> u32 {
        self.0 & Self::TILE_MASK
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(transparent)]
pub struct KeyBinding(u64);

impl KeyBinding {
    const POSITION_BITS: u64 = 32;
    // const TILE_GEN_BITS: u64 = 32;

    const TILE_GEN_SHIFT: u64 = Self::POSITION_BITS;

    #[inline]
    pub(crate) fn new(position: Position, tile_gen: u32) -> Self {
        Self((position.0 as u64) | ((tile_gen as u64) << Self::TILE_GEN_SHIFT))
    }

    #[inline]
    pub fn position(self) -> Position {
        Position(self.0 as u32)
    }

    #[inline]
    pub fn is_empty(self) -> bool {
        self.position().is_empty()
    }

    pub fn empty(atlas_id: u8) -> Self {
        Self::new(Position::empty(atlas_id), 0)
    }

    #[inline]
    pub fn tile_generation(self) -> u32 {
        (self.0 >> Self::TILE_GEN_SHIFT) as u32
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AtlasLayout {
    pub tiles_per_edge: usize,
    pub layer_num: usize,
}

impl AtlasLayout {
    pub const TINY8: AtlasLayout = AtlasLayout {
        tiles_per_edge: 16,
        layer_num: 1,
    };
    pub const SMALL11: AtlasLayout = AtlasLayout {
        tiles_per_edge: 32,
        layer_num: 2,
    };
    pub const MEDIUM14: AtlasLayout = AtlasLayout {
        tiles_per_edge: 64,
        layer_num: 4,
    };
    pub const LARGE17: AtlasLayout = AtlasLayout {
        tiles_per_edge: 128,
        layer_num: 8,
    };
    pub const HUGE20: AtlasLayout = AtlasLayout {
        tiles_per_edge: 256,
        layer_num: 16,
    };

    pub const fn total_slots(self) -> usize {
        self.tiles_per_edge * self.tiles_per_edge * self.layer_num
    }

    pub fn index_to_address(self, index: usize) -> Result<TileAddress, AtlasLayoutError> {
        if index >= self.total_slots() {
            return Err(AtlasLayoutError::OutOfBounds);
        }

        let tiles_per_edge = self.tiles_per_edge;
        let slots_per_layer = tiles_per_edge * tiles_per_edge;
        let layer = index / slots_per_layer;
        let layer_slot = index % slots_per_layer;
        Ok(TileAddress {
            layer,
            tile_x: layer_slot % tiles_per_edge,
            tile_y: layer_slot / tiles_per_edge,
        })
    }

    pub fn address_to_index(self, address: TileAddress) -> Result<usize, AtlasLayoutError> {
        let tiles_per_edge = self.tiles_per_edge;
        let index = address
            .layer
            .checked_mul(tiles_per_edge)
            .and_then(|v| v.checked_add(address.tile_y))
            .and_then(|v| v.checked_mul(tiles_per_edge))
            .and_then(|v| v.checked_add(address.tile_x))
            .ok_or(AtlasLayoutError::OutOfBounds)?;
        if index >= self.total_slots() {
            return Err(AtlasLayoutError::OutOfBounds);
        }
        Ok(index)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtlasLayoutError {
    OutOfBounds,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChannelCount {
    D1,
    D2,
    D4,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChannelType {
    U8,
    U16,
    Unorm8,
    Unorm16,
    F16,
    F32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AtlasFormat {
    pub channel_count: ChannelCount,
    pub channel_type: ChannelType,
}

pub struct Atlas {
    pub id: u8,
    pub layout: AtlasLayout,
    pub format: AtlasFormat,
    tile_pool: Pool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtlasError {
    OutOfTiles { atlas_id: u8 },
    IdMismatch { atlas_id: u8 },
}

impl Atlas {
    pub fn new(id: u8, layout: AtlasLayout, format: AtlasFormat) -> Self {
        Self {
            id,
            layout,
            format,
            tile_pool: Pool::new(layout.total_slots() as u32),
        }
    }

    pub fn alloc(&mut self) -> Result<KeyBinding, AtlasError> {
        let (index, tile_gen) = self
            .tile_pool
            .alloc()
            .map_err(|_| AtlasError::OutOfTiles { atlas_id: self.id })?;
        Ok(KeyBinding::new(Position::new(self.id, index), tile_gen))
    }

    pub fn remaining(&self) -> u32 {
        self.tile_pool.remaining()
    }

    pub fn check(&self, binding: KeyBinding) -> bool {
        binding.position().atlas_id() == self.id
            && self
                .tile_pool
                .check(binding.position().tile_index(), binding.tile_generation())
    }

    pub fn free(&mut self, position: Position) {
        if position.atlas_id() != self.id {
            return;
        }
        self.tile_pool.free(position.tile_index() as u32);
    }
}
