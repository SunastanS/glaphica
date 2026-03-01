use crate::key::{BackendId, GenerationId, SlotId, TileKey};
use crate::layout::AtlasLayout;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtlasBackendError {
    OutOfSlots,
    WrongBackend,
    InvalidSlot,
    GenerationMismatch,
}

pub struct Backend {
    // total_slots = layout.total_slots() = FreeSlotPool::total_slots = generations.len()
    backend_id: BackendId,
    layout: AtlasLayout,
    total_slots: u32,
    pool: FreeSlotPool,
    generations: Box<[GenerationId]>,
}

impl Backend {
    pub fn new(layout: AtlasLayout, backend_id: BackendId) -> Self {
        let total_slots = layout.total_slots();
        Self {
            backend_id,
            layout,
            total_slots,
            pool: FreeSlotPool::new(total_slots),
            generations: vec![GenerationId::new(0); total_slots as usize].into_boxed_slice(),
        }
    }

    pub fn alloc(&mut self) -> Result<TileKey, AtlasBackendError> {
        let Some(slot) = self.pool.alloc() else {
            return Err(AtlasBackendError::OutOfSlots);
        };
        let generation = self.generations[slot.raw() as usize];
        Ok(TileKey::new(self.backend_id, generation, slot))
    }

    pub fn alloc_batch(&mut self, count: usize) -> Vec<TileKey> {
        let mut keys = Vec::with_capacity(count);
        for _ in 0..count {
            match self.alloc() {
                Ok(key) => keys.push(key),
                Err(AtlasBackendError::OutOfSlots) => break,
                Err(_) => break,
            }
        }
        keys
    }

    pub fn free(&mut self, key: TileKey) -> Result<(), AtlasBackendError> {
        if key.backend() != self.backend_id {
            return Err(AtlasBackendError::WrongBackend);
        }

        let slot = key.slot();
        let index = slot.raw() as usize;
        let Some(current_generation) = self.generations.get(index).copied() else {
            return Err(AtlasBackendError::InvalidSlot);
        };
        if current_generation != key.generation() {
            return Err(AtlasBackendError::GenerationMismatch);
        };

        let generation = current_generation.raw().wrapping_add(1);
        self.generations[index] = GenerationId::new(generation);
        self.pool.free(slot);
        Ok(())
    }

    pub const fn layout(&self) -> AtlasLayout {
        self.layout
    }

    pub const fn total_slots(&self) -> u32 {
        self.total_slots
    }
}

#[derive(Debug, Default)]
struct FreeSlotPool {
    total_slots: u32,
    next_slot: u32,
    freelist: Vec<SlotId>,
}

impl FreeSlotPool {
    pub const fn new(total_slots: u32) -> Self {
        Self {
            total_slots,
            next_slot: 0,
            freelist: Vec::new(),
        }
    }

    pub fn alloc(&mut self) -> Option<SlotId> {
        if let Some(slot) = self.freelist.pop() {
            return Some(slot);
        }

        if self.next_slot >= self.total_slots {
            return None;
        }

        let slot = self.next_slot;
        self.next_slot = self.next_slot.checked_add(1).expect("slot id overflow");
        Some(SlotId::new(slot))
    }

    pub fn free(&mut self, slot: SlotId) {
        self.freelist.push(slot);
    }

    pub fn clear(&mut self) {
        self.next_slot = 0;
        self.freelist.clear();
    }

    pub fn allocated(&self) -> u32 {
        let reused = self.freelist.len().min(self.next_slot as usize) as u32;
        self.next_slot - reused
    }

    pub fn is_empty(&self) -> bool {
        self.allocated() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::{AtlasBackendError, AtlasLayout, Backend, FreeSlotPool};
    use crate::key::BackendId;

    #[test]
    fn allocate_increases_slot_id() {
        let mut pool = FreeSlotPool::new(3);
        let a = pool.alloc().unwrap();
        let b = pool.alloc().unwrap();
        let c = pool.alloc().unwrap();
        assert!(pool.alloc().is_none());
        assert_eq!(a.raw(), 0);
        assert_eq!(b.raw(), 1);
        assert_eq!(c.raw(), 2);
        assert_eq!(pool.allocated(), 3);
    }

    #[test]
    fn freed_slot_is_reused() {
        let mut pool = FreeSlotPool::new(2);
        let a = pool.alloc().unwrap();
        let b = pool.alloc().unwrap();
        pool.free(a);
        let reused = pool.alloc().unwrap();
        assert_eq!(reused.raw(), a.raw());
        assert_eq!(pool.allocated(), 2);
        assert_eq!(b.raw(), 1);
    }

    #[test]
    fn alloc_batch_returns_available_keys() {
        let mut backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(1));
        let keys = backend.alloc_batch(300);
        assert_eq!(keys.len(), 256);
        assert_eq!(backend.alloc().unwrap_err(), AtlasBackendError::OutOfSlots);
    }

    #[test]
    fn free_uses_tile_key_and_bumps_generation() {
        let mut backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(1));
        let key = backend.alloc().unwrap();
        let slot = key.slot().raw();
        let generation = key.generation().raw();

        backend.free(key).unwrap();
        let next = backend.alloc().unwrap();

        assert_eq!(next.slot().raw(), slot);
        assert_eq!(next.generation().raw(), generation.wrapping_add(1));
    }

    #[test]
    fn free_returns_error_for_wrong_backend() {
        let mut backend0 = Backend::new(AtlasLayout::Tiny8, BackendId::new(0));
        let mut backend1 = Backend::new(AtlasLayout::Tiny8, BackendId::new(1));
        let key = backend0.alloc().unwrap();
        let err = backend1.free(key).unwrap_err();
        assert_eq!(err, AtlasBackendError::WrongBackend);
    }
}
