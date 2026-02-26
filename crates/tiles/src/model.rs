use bitvec::prelude::{BitVec, Lsb0};
use model::{ImageLayout, ImageLayoutError, TILE_STRIDE};

// REFRACTORING:
// - use meaningful TileKey
// - use `slot = layer * tiles_per_layer + tile_index`
//   instead of two different keys

const SLOT_BITS: u64 = 32;
const GEN_BITS: u64 = 24;
const BACKEND_BITS: u64 = 8;

const SLOT_SHIFT: u64 = (1 << SLOT_BITS) - 1;
const GEN_SHIFT: u64 = SLOT_SHIFT + SLOT_BITS;
const BACKEND_SHIFT: u64 = GEN_SHIFT + GEN_BITS;

const SLOT_MASK: u64 = (1 << SLOT_BITS) - 1;
const GEN_MASK: u64 = (1 << GEN_BITS) - 1;
const BACKEND_MASK: u64 = (1 << BACKEND_BITS) - 1;

#[derive(Debug, Copy, Clone)]
struct BackendId(u8);

#[derive(Debug, Copy, Clone)]
struct GenerationId(u32);

#[derive(Debug, Copy, Clone)]
struct SlotId(u32);

#[derive(Debug, Copy, Clone)]
struct TileKey(u64);

impl TileKey {
    /// TileKey:
    /// | backend (8) | generation (24) | slot_index (32) |
    /// 63          56 55             32 31              0

    pub fn new(backend: BackendId, generation: GenerationId, slot: SlotId) -> Self {
        let backend = backend.0 as u64;
        let generation = generation.0 as u64;
        let slot = slot.0 as u64;
        TileKey(
            (backend & BACKEND_MASK) << BACKEND_SHIFT
                | (generation & GEN_MASK) << GEN_SHIFT
                | (slot & SLOT_MASK) << SLOT_SHIFT,
        )
    }
    pub fn backend(&self) -> BackendId {
        BackendId((self.0 >> BACKEND_SHIFT) as u8)
    }
    pub fn generation(&self) -> GenerationId {
        GenerationId(((self.0 >> GEN_SHIFT) & GEN_MASK) as u32)
    }

    pub fn slot(&self) -> SlotId {
        SlotId(((self.0 >> SLOT_SHIFT) & SLOT_MASK) as u32)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Pow2U16(u16);

impl Pow2U16 {
    pub const fn new(value: u16) -> Self {
        assert!(value != 0 && (value & (value - 1)) == 0);
        Pow2U16(value)
    }
    pub const fn get(self) -> u16 {
        self.0
    }
    pub const fn get_u32(self) -> u32 {
        self.0 as u32
    }
    pub const fn log2(self) -> u32 {
        self.0.trailing_zeros()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AtlasLayout {
    tiles_per_edge: Pow2U16,
    array_layers: Pow2U16,
}

impl AtlasLayout {
    pub const fn atlas_edge_px(self) -> u32 {
        self.tiles_per_edge.get_u32() * TILE_STRIDE
    }
    pub const fn tiles_per_layer(self) -> u32 {
        let n = self.tiles_per_edge.get_u32();
        n * n
    }
    pub const fn capacity_tiles(self) -> u32 {
        self.tiles_per_layer() * self.array_layers.get_u32()
    }

    pub const fn x_bits(self) -> u32 {
        self.tiles_per_edge.log2()
    }
    pub const fn y_bits(self) -> u32 {
        self.tiles_per_edge.log2()
    }
    pub const fn layer_bits(self) -> u32 {
        self.array_layers.log2()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtlasTier {
    // we don't want users to know about how the tiles layout
    // the only need to know how much tiles they can get from a backend
    // here the numbers should be pow2
    Tiny10, //2^10 tiles in a backend
    Small12,
    Medium15,
    Large17,
    Huge18,
}

impl AtlasTier {
    pub const fn layout(self) -> AtlasLayout {
        match self {
            AtlasTier::Tiny10 => AtlasLayout {
                tiles_per_edge: Pow2U16::new(32),
                array_layers: Pow2U16::new(1),
            },
            AtlasTier::Small12 => AtlasLayout {
                tiles_per_edge: Pow2U16::new(32),
                array_layers: Pow2U16::new(4),
            },
            AtlasTier::Medium15 => AtlasLayout {
                tiles_per_edge: Pow2U16::new(64),
                array_layers: Pow2U16::new(8),
            },
            AtlasTier::Large17 => AtlasLayout {
                tiles_per_edge: Pow2U16::new(128),
                array_layers: Pow2U16::new(8),
            },
            AtlasTier::Huge18 => AtlasLayout {
                tiles_per_edge: Pow2U16::new(128),
                array_layers: Pow2U16::new(16),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TileDirtyBitSet {
    layout: ImageLayout,
    bits: BitVec<usize, Lsb0>,
    dirty_count: usize,
}

impl TileDirtyBitSet {
    pub fn new(layout: ImageLayout) -> Result<Self, ImageLayoutError> {
        let bits = BitVec::repeat(false, layout.max_tiles() as usize);
        Ok(TileDirtyBitSet {
            layout,
            bits,
            dirty_count: 0,
        })
    }

    pub fn is_empty(&self) -> bool {
        self.dirty_count == 0
    }

    pub fn is_full(&self) -> bool {
        self.dirty_count == self.bits.len()
    }
}
