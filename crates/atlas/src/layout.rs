use crate::key::SlotId;

pub(crate) struct TileAddress {
    offset: (u32, u32),
    layer: u32,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum AtlasLayout {
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

    pub fn get_tile_address(self, slot: SlotId) -> TileAddress {
        let slot = slot.raw();

        let edge_bits = self.tiles_per_edge_bits();
        let layer_bits = edge_bits * 2;

        let layer = slot >> layer_bits;

        let index_mask = (1 << layer_bits) - 1;
        let index = slot & index_mask;

        let y = index >> edge_bits;
        let x = index & ((1 << edge_bits) - 1);

        TileAddress {
            offset: (x, y),
            layer,
        }
    }
}
