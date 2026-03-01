use std::ops::{BitOr, BitOrAssign};

mod core; // mod core
mod format;
#[cfg(feature = "atlas-gpu")]
mod gpu;
mod tier;

#[cfg(feature = "atlas-gpu")]
mod brush_buffer_storage;
#[cfg(feature = "atlas-gpu")]
mod group_preview;
#[cfg(feature = "atlas-gpu")]
mod layer_pixel_storage;

// Re-export tier types
pub use tier::{AtlasTier, TierAtlasLayout};

#[derive(Debug, Clone, Copy)]
pub struct TileAtlasConfig {
    pub tier: AtlasTier,
    pub format: TileAtlasFormat,
    pub usage: TileAtlasUsage,
}

impl TileAtlasConfig {
    /// Creates a config with the specified tier.
    pub fn with_tier(tier: AtlasTier) -> Self {
        Self {
            tier,
            format: TileAtlasFormat::Rgba8Unorm,
            usage: TileAtlasUsage::TEXTURE_BINDING
                | TileAtlasUsage::COPY_DST
                | TileAtlasUsage::COPY_SRC,
        }
    }

    /// Creates a config with the Tiny10 tier (for testing).
    pub fn tiny10() -> Self {
        Self::with_tier(AtlasTier::Tiny10)
    }

    /// Creates a config with the Medium15 tier (default for production).
    pub fn medium15() -> Self {
        Self::with_tier(AtlasTier::Medium15)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TileAtlasFormat {
    Rgba8Unorm,
    Rgba8UnormSrgb,
    Bgra8Unorm,
    Bgra8UnormSrgb,
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
    pub tier: AtlasTier,
    pub usage: TileAtlasUsage,
}

impl GenericTileAtlasConfig {
    /// Creates a config with the specified tier.
    pub fn with_tier(tier: AtlasTier) -> Self {
        Self {
            tier,
            usage: TileAtlasUsage::TEXTURE_BINDING
                | TileAtlasUsage::COPY_DST
                | TileAtlasUsage::COPY_SRC,
        }
    }

    /// Creates a config with the Tiny10 tier (for testing).
    pub fn tiny10() -> Self {
        Self::with_tier(AtlasTier::Tiny10)
    }

    /// Creates a config with the Medium15 tier (default for production).
    pub fn medium15() -> Self {
        Self::with_tier(AtlasTier::Medium15)
    }
}

impl Default for GenericTileAtlasConfig {
    fn default() -> Self {
        Self::medium15()
    }
}

impl From<TileAtlasConfig> for GenericTileAtlasConfig {
    fn from(value: TileAtlasConfig) -> Self {
        Self {
            tier: value.tier,
            usage: value.usage,
        }
    }
}

impl Default for TileAtlasConfig {
    fn default() -> Self {
        Self::medium15()
    }
}

#[cfg(feature = "atlas-gpu")]
pub use brush_buffer_storage::{
    GenericR32FloatTileAtlasGpuArray, GenericR32FloatTileAtlasStore,
    GenericR8UintTileAtlasGpuArray, GenericR8UintTileAtlasStore, GenericTileAtlasGpuArray,
    GenericTileAtlasStore, RuntimeGenericTileAtlasConfig, RuntimeGenericTileAtlasGpuArray,
    RuntimeGenericTileAtlasStore,
};
#[cfg(feature = "atlas-gpu")]
pub use group_preview::{GroupTileAtlasGpuArray, GroupTileAtlasStore};
#[cfg(feature = "atlas-gpu")]
pub use layer_pixel_storage::{TileAtlasGpuArray, TileAtlasStore};

#[cfg(test)]
pub(crate) use format::rgba8_tile_len;
#[cfg(all(test, feature = "atlas-gpu"))]
pub(crate) use gpu::tile_origin;
