//! TileKey encoding scheme.
//!
//! REFACTORING:
//! There are two kinds of search work
//! 1. The position of a tile in image layout -> TileKey
//! 2. TileKey -> the position of a tile in atlas backend
//! We move the relatively simple one (1) to crates/model/src/lib.rs

// REFRACTORING:
// - use meaningful TileKey
// - use `slot = layer * tiles_per_layer + tile_index`
//   instead of two different keys

const SLOT_BITS: u64 = 32;
const GEN_BITS: u64 = 24;
const BACKEND_BITS: u64 = 8;

const SLOT_SHIFT: u64 = 0;
const GEN_SHIFT: u64 = SLOT_BITS;
const BACKEND_SHIFT: u64 = SLOT_BITS + GEN_BITS;

const SLOT_MASK: u64 = (1 << SLOT_BITS) - 1;
const GEN_MASK: u64 = (1 << GEN_BITS) - 1;
const BACKEND_MASK: u64 = (1 << BACKEND_BITS) - 1;

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub struct BackendId(pub u8);

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct GenerationId(pub u32);

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct SlotId(pub u32);

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub struct TileKey(u64);

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

impl model::EmptyKey for TileKey {
    const EMPTY: Self = TileKey(0);
}
