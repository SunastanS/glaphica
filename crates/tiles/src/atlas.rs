use super::DEFAULT_MAX_LAYERS;
use std::ops::{BitOr, BitOrAssign};

mod core; // mod core
mod format;
mod gpu_runtime;

mod brush_buffer_storage;
mod group_preview;
mod layer_pixel_storage;

#[derive(Debug, Clone, Copy)]
pub struct TileAtlasConfig {
    pub max_layers: u32,
    pub tiles_per_row: u32,
    pub tiles_per_column: u32,
    pub format: TileAtlasFormat,
    pub usage: TileAtlasUsage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TileAtlasFormat {
    Rgba8Unorm,
    Rgba8UnormSrgb,
    R32Float,
    R8Uint,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TileAtlasUsage {
    bits: u8,
}

impl TileAtlasUsage {
    const COPY_DST_BIT: u8 = 1 << 0;
    const TEXTURE_BINDING_BIT: u8 = 1 << 1;
    const COPY_SRC_BIT: u8 = 1 << 2;
    const STORAGE_BINDING_BIT: u8 = 1 << 3;

    pub const COPY_DST: Self = Self {
        bits: Self::COPY_DST_BIT,
    };
    pub const TEXTURE_BINDING: Self = Self {
        bits: Self::TEXTURE_BINDING_BIT,
    };
    pub const COPY_SRC: Self = Self {
        bits: Self::COPY_SRC_BIT,
    };
    pub const STORAGE_BINDING: Self = Self {
        bits: Self::STORAGE_BINDING_BIT,
    };

    pub const fn empty() -> Self {
        Self { bits: 0 }
    }

    pub const fn contains_copy_dst(self) -> bool {
        (self.bits & Self::COPY_DST_BIT) != 0
    }

    pub const fn contains_texture_binding(self) -> bool {
        (self.bits & Self::TEXTURE_BINDING_BIT) != 0
    }

    pub const fn contains_copy_src(self) -> bool {
        (self.bits & Self::COPY_SRC_BIT) != 0
    }

    pub const fn contains_storage_binding(self) -> bool {
        (self.bits & Self::STORAGE_BINDING_BIT) != 0
    }
}

impl BitOr for TileAtlasUsage {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self {
            bits: self.bits | rhs.bits,
        }
    }
}

impl BitOrAssign for TileAtlasUsage {
    fn bitor_assign(&mut self, rhs: Self) {
        self.bits |= rhs.bits;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TilePayloadKind {
    Rgba8,
    R32Float,
    R8Uint,
}

#[derive(Debug, Clone, Copy)]
pub struct GenericTileAtlasConfig {
    pub max_layers: u32,
    pub tiles_per_row: u32,
    pub tiles_per_column: u32,
    pub usage: TileAtlasUsage,
}

impl Default for GenericTileAtlasConfig {
    fn default() -> Self {
        Self {
            max_layers: DEFAULT_MAX_LAYERS,
            tiles_per_row: crate::TILES_PER_ROW,
            tiles_per_column: crate::TILES_PER_ROW,
            usage: TileAtlasUsage::TEXTURE_BINDING
                | TileAtlasUsage::COPY_DST
                | TileAtlasUsage::COPY_SRC,
        }
    }
}

impl From<TileAtlasConfig> for GenericTileAtlasConfig {
    fn from(value: TileAtlasConfig) -> Self {
        Self {
            max_layers: value.max_layers,
            tiles_per_row: value.tiles_per_row,
            tiles_per_column: value.tiles_per_column,
            usage: value.usage,
        }
    }
}

impl Default for TileAtlasConfig {
    fn default() -> Self {
        Self {
            max_layers: DEFAULT_MAX_LAYERS,
            tiles_per_row: crate::TILES_PER_ROW,
            tiles_per_column: crate::TILES_PER_ROW,
            format: TileAtlasFormat::Rgba8Unorm,
            usage: TileAtlasUsage::TEXTURE_BINDING
                | TileAtlasUsage::COPY_DST
                | TileAtlasUsage::COPY_SRC,
        }
    }
}

pub use brush_buffer_storage::{
    GenericR8UintTileAtlasGpuArray, GenericR8UintTileAtlasStore, GenericR32FloatTileAtlasGpuArray,
    GenericR32FloatTileAtlasStore, GenericTileAtlasGpuArray, GenericTileAtlasStore,
    RuntimeGenericTileAtlasConfig, RuntimeGenericTileAtlasGpuArray, RuntimeGenericTileAtlasStore,
};
pub use group_preview::{GroupTileAtlasGpuArray, GroupTileAtlasStore};
pub use layer_pixel_storage::{TileAtlasGpuArray, TileAtlasStore};

#[cfg(test)]
pub(crate) use gpu_runtime::tile_origin;
#[cfg(test)]
pub(crate) use format::rgba8_tile_len;
