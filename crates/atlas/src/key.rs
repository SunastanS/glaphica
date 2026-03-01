use std::marker::PhantomData;

const SLOT_BITS: u32 = 32;
const GEN_BITS: u32 = 24;
const BACKEND_BITS: u32 = 8;

const SLOT_SHIFT: u32 = 0;
const GEN_SHIFT: u32 = SLOT_BITS;
const BACKEND_SHIFT: u32 = GEN_SHIFT + GEN_BITS;

const SLOT_MASK: u64 = (1 << SLOT_BITS) - 1;
const GEN_MASK: u64 = (1 << GEN_BITS) - 1;
const BACKEND_MASK: u64 = (1 << BACKEND_BITS) - 1;

#[derive(Debug, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct Id<Tag, Repr> {
    raw: Repr,
    _marker: PhantomData<Tag>,
}

impl<Tag, Repr: Copy> Copy for Id<Tag, Repr> {}

impl<Tag, Repr: Copy> Clone for Id<Tag, Repr> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<Tag, Repr> Id<Tag, Repr> {
    pub(crate) const fn new(raw: Repr) -> Self {
        Self {
            raw,
            _marker: PhantomData,
        }
    }

    pub(crate) const fn raw(self) -> Repr
    where
        Repr: Copy,
    {
        self.raw
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum BackendTag {}
pub type BackendId = Id<BackendTag, u8>;

#[derive(Debug, PartialEq, Eq)]
pub enum GenerationTag {}
pub type GenerationId = Id<GenerationTag, u32>;

#[derive(Debug, PartialEq, Eq)]
pub enum SlotTag {}
pub type SlotId = Id<SlotTag, u32>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TileKey(u64);

impl TileKey {
    /// TileKey:
    /// | backend (8) | generation (24) | slot_index (32) |
    /// 63          56 55             32 31              0

    pub fn new(backend: BackendId, generation: GenerationId, slot: SlotId) -> Self {
        let backend = backend.raw() as u64;
        let generation = generation.raw() as u64;
        let slot = slot.raw() as u64;
        TileKey(
            (backend & BACKEND_MASK) << BACKEND_SHIFT
                | (generation & GEN_MASK) << GEN_SHIFT
                | (slot & SLOT_MASK) << SLOT_SHIFT,
        )
    }

    const EMPTY: TileKey = TileKey(u64::MAX);

    pub fn backend(&self) -> BackendId {
        BackendId::new((self.0 >> BACKEND_SHIFT) as u8)
    }

    pub fn generation(&self) -> GenerationId {
        GenerationId::new(((self.0 >> GEN_SHIFT) & GEN_MASK) as u32)
    }

    pub fn slot(&self) -> SlotId {
        SlotId::new(((self.0 >> SLOT_SHIFT) & SLOT_MASK) as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tile_key_round_trip() {
        let backend = BackendId::new(42);
        let generation = GenerationId::new(0x12_3456);
        let slot = SlotId::new(0x89AB_CDEF);
        let key = TileKey::new(backend, generation, slot);

        assert_eq!(key.backend().raw(), 42);
        assert_eq!(key.generation().raw(), 0x12_3456);
        assert_eq!(key.slot().raw(), 0x89AB_CDEF);
    }

    #[test]
    fn generation_is_masked_to_24_bits() {
        let key = TileKey::new(
            BackendId::new(1),
            GenerationId::new(0xFF12_3456),
            SlotId::new(7),
        );
        assert_eq!(key.generation().raw(), 0x12_3456);
    }

    #[test]
    fn empty_key_decodes_to_all_ones_fields() {
        assert_eq!(TileKey::EMPTY.backend().raw(), 0xFF);
        assert_eq!(TileKey::EMPTY.generation().raw(), 0xFF_FFFF);
        assert_eq!(TileKey::EMPTY.slot().raw(), u32::MAX);
    }
}
