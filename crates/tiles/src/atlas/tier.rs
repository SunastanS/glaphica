//! Atlas tier-based layout system.
//!
//! This module defines a tier-based atlas sizing scheme where all atlas configurations
//! are predefined powers-of-2 for simplicity and performance optimization.
//!
//! Tier capacity formula: tiles_per_edge² × array_layers
//! - tiles_per_edge: power-of-2 number of tiles along each edge of a layer
//! - array_layers: power-of-2 number of texture array layers
//!
//! Example tier sizes:
//! - Tiny10:   32×32 tiles × 1 layer  = 1,024 tiles
//! - Small12:  32×32 tiles × 4 layers = 4,096 tiles
//! - Medium15: 64×64 tiles × 8 layers = 32,768 tiles

use crate::{TileAtlasLayout, TILE_STRIDE};

/// A power-of-2 unsigned 16-bit value.
/// Used for atlas dimensions to enable efficient bit-shift calculations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pow2U16(u16);

impl Pow2U16 {
    /// Creates a new Pow2U16, panicking if value is not a power of 2 or is zero.
    pub const fn new(value: u16) -> Self {
        assert!(
            value != 0 && (value & (value - 1)) == 0,
            "value must be a non-zero power of 2"
        );
        Pow2U16(value)
    }

    /// Returns the underlying value.
    pub const fn get(self) -> u16 {
        self.0
    }

    /// Returns the value as u32.
    pub const fn get_u32(self) -> u32 {
        self.0 as u32
    }

    /// Returns log2 of the value (position of the single set bit).
    pub const fn log2(self) -> u32 {
        self.0.trailing_zeros()
    }
}

/// Layout specification for a tier-based atlas.
///
/// All dimensions are powers of 2 to enable efficient calculations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TierAtlasLayout {
    /// Number of tiles along each edge of a layer (power of 2).
    tiles_per_edge: Pow2U16,
    /// Number of texture array layers (power of 2).
    array_layers: Pow2U16,
}

impl TierAtlasLayout {
    /// Returns the width/height of the atlas texture in pixels.
    pub const fn atlas_edge_px(self) -> u32 {
        self.tiles_per_edge.get_u32() * TILE_STRIDE
    }

    /// Returns the number of tiles per layer (tiles_per_edge²).
    pub const fn tiles_per_layer(self) -> u32 {
        let n = self.tiles_per_edge.get_u32();
        n * n
    }

    /// Returns the total tile capacity (tiles_per_layer × array_layers).
    pub const fn capacity_tiles(self) -> u32 {
        self.tiles_per_layer() * self.array_layers.get_u32()
    }

    /// Returns the number of bits needed for X coordinate.
    pub const fn x_bits(self) -> u32 {
        self.tiles_per_edge.log2()
    }

    /// Returns the number of bits needed for Y coordinate.
    pub const fn y_bits(self) -> u32 {
        self.tiles_per_edge.log2()
    }

    /// Returns the number of bits needed for layer index.
    pub const fn layer_bits(self) -> u32 {
        self.array_layers.log2()
    }

    /// Returns tiles per row (same as tiles_per_edge for square layers).
    pub const fn tiles_per_row(self) -> u32 {
        self.tiles_per_edge.get_u32()
    }

    /// Returns tiles per column (same as tiles_per_edge for square layers).
    pub const fn tiles_per_column(self) -> u32 {
        self.tiles_per_edge.get_u32()
    }

    /// Returns the number of array layers.
    pub const fn max_layers(self) -> u32 {
        self.array_layers.get_u32()
    }

    /// Converts to the public TileAtlasLayout representation.
    pub const fn to_public_layout(self) -> TileAtlasLayout {
        TileAtlasLayout {
            tiles_per_row: self.tiles_per_row(),
            tiles_per_column: self.tiles_per_column(),
            atlas_width: self.atlas_edge_px(),
            atlas_height: self.atlas_edge_px(),
        }
    }
}

/// Predefined atlas tiers with power-of-2 sizing.
///
/// The tier names indicate the log2 of total tile capacity:
/// - Tiny10:   2^10 = 1,024 tiles
/// - Small12:  2^12 = 4,096 tiles
/// - Medium15: 2^15 = 32,768 tiles
/// - Large17:  2^17 = 131,072 tiles
/// - Huge18:   2^18 = 262,144 tiles
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AtlasTier {
    /// 32×32 tiles × 1 layer = 1,024 tiles (2^10)
    Tiny10,
    /// 32×32 tiles × 4 layers = 4,096 tiles (2^12)
    Small12,
    /// 64×64 tiles × 8 layers = 32,768 tiles (2^15)
    Medium15,
    /// 128×128 tiles × 8 layers = 131,072 tiles (2^17)
    Large17,
    /// 128×128 tiles × 16 layers = 262,144 tiles (2^18)
    Huge18,
}

impl AtlasTier {
    /// Returns the layout specification for this tier.
    pub const fn layout(self) -> TierAtlasLayout {
        match self {
            AtlasTier::Tiny10 => TierAtlasLayout {
                tiles_per_edge: Pow2U16::new(32),
                array_layers: Pow2U16::new(1),
            },
            AtlasTier::Small12 => TierAtlasLayout {
                tiles_per_edge: Pow2U16::new(32),
                array_layers: Pow2U16::new(4),
            },
            AtlasTier::Medium15 => TierAtlasLayout {
                tiles_per_edge: Pow2U16::new(64),
                array_layers: Pow2U16::new(8),
            },
            AtlasTier::Large17 => TierAtlasLayout {
                tiles_per_edge: Pow2U16::new(128),
                array_layers: Pow2U16::new(8),
            },
            AtlasTier::Huge18 => TierAtlasLayout {
                tiles_per_edge: Pow2U16::new(128),
                array_layers: Pow2U16::new(16),
            },
        }
    }

    /// Returns the total tile capacity for this tier.
    pub const fn capacity_tiles(self) -> u32 {
        self.layout().capacity_tiles()
    }

    /// Returns the texture size in pixels for this tier.
    pub const fn texture_size_px(self) -> u32 {
        self.layout().atlas_edge_px()
    }

    /// Converts to the public TileAtlasLayout representation.
    pub const fn to_public_layout(self) -> TileAtlasLayout {
        self.layout().to_public_layout()
    }
}

impl Default for AtlasTier {
    /// Default tier for production use (Medium15: 32,768 tiles).
    fn default() -> Self {
        AtlasTier::Medium15
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_capacities_are_pow2() {
        assert_eq!(AtlasTier::Tiny10.capacity_tiles(), 1024); // 2^10
        assert_eq!(AtlasTier::Small12.capacity_tiles(), 4096); // 2^12
        assert_eq!(AtlasTier::Medium15.capacity_tiles(), 32768); // 2^15
        assert_eq!(AtlasTier::Large17.capacity_tiles(), 131072); // 2^17
        assert_eq!(AtlasTier::Huge18.capacity_tiles(), 262144); // 2^18
    }

    #[test]
    fn tier_texture_sizes() {
        // 128 stride × tiles_per_edge
        assert_eq!(AtlasTier::Tiny10.texture_size_px(), 128 * 32); // 4096
        assert_eq!(AtlasTier::Small12.texture_size_px(), 128 * 32); // 4096
        assert_eq!(AtlasTier::Medium15.texture_size_px(), 128 * 64); // 8192
        assert_eq!(AtlasTier::Large17.texture_size_px(), 128 * 128); // 16384
        assert_eq!(AtlasTier::Huge18.texture_size_px(), 128 * 128); // 16384
    }

    #[test]
    fn layout_converts_to_public() {
        let tier = AtlasTier::Medium15;
        let public = tier.to_public_layout();
        let layout = tier.layout();

        assert_eq!(public.tiles_per_row, layout.tiles_per_row());
        assert_eq!(public.tiles_per_column, layout.tiles_per_column());
        assert_eq!(public.atlas_width, layout.atlas_edge_px());
        assert_eq!(public.atlas_height, layout.atlas_edge_px());
    }
}
