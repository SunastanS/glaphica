pub const ATLAS_TILE_SIZE: u32 = 64;
pub const GUTTER_SIZE: u32 = 1;
pub const IMAGE_TILE_SIZE: u32 = ATLAS_TILE_SIZE - 2 * GUTTER_SIZE;

mod key;

pub use key::{BackendId, BackendTag, GenerationId, GenerationTag, Id, SlotId, SlotTag, TileKey};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PipelineId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ShaderId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AtlasLayout {
    Tiny8,
    Small11,
    Medium14,
    Large17,
    Huge20,
}

impl AtlasLayout {
    pub const fn total_slots(self) -> u32 {
        match self {
            AtlasLayout::Tiny8 => 1 << 8,
            AtlasLayout::Small11 => 1 << 11,
            AtlasLayout::Medium14 => 1 << 14,
            AtlasLayout::Large17 => 1 << 17,
            AtlasLayout::Huge20 => 1 << 20,
        }
    }

    pub const fn layers(self) -> u32 {
        match self {
            AtlasLayout::Tiny8 => 1,
            AtlasLayout::Small11 => 2,
            AtlasLayout::Medium14 => 4,
            AtlasLayout::Large17 => 8,
            AtlasLayout::Huge20 => 16,
        }
    }

    pub const fn tiles_per_edge(self) -> u32 {
        match self {
            AtlasLayout::Tiny8 => 16,
            AtlasLayout::Small11 => 32,
            AtlasLayout::Medium14 => 64,
            AtlasLayout::Large17 => 128,
            AtlasLayout::Huge20 => 256,
        }
    }

    pub const fn tiles_per_edge_bits(self) -> u32 {
        match self {
            AtlasLayout::Tiny8 => 4,
            AtlasLayout::Small11 => 5,
            AtlasLayout::Medium14 => 6,
            AtlasLayout::Large17 => 7,
            AtlasLayout::Huge20 => 8,
        }
    }
}
