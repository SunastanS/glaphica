use glaphica_core::{AtlasLayout, BackendId, GenerationId, SlotId, TileKey};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtlasBackendError {
    OutOfSlots,
    WrongBackend,
    InvalidSlot,
    GenerationMismatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtlasBackendManagerError {
    TooManyBackends,
}

pub struct Backend {
    backend_id: BackendId,
    layout: AtlasLayout,
    total_slots: u32,
    even_pool: ParityPool,
    odd_pool: ParityPool,
    generations: Box<[GenerationId]>,
}

impl Backend {
    pub fn new(layout: AtlasLayout, backend_id: BackendId) -> Self {
        let total_slots = layout.total_slots();
        let tiles_per_layer = layout.tiles_per_edge() * layout.tiles_per_edge();
        let layers = layout.layers();
        let even_layers = layers.div_ceil(2);
        let odd_layers = layers / 2;
        let even_slots = even_layers * tiles_per_layer;
        let odd_slots = odd_layers * tiles_per_layer;

        Self {
            backend_id,
            layout,
            total_slots,
            even_pool: ParityPool::new(even_slots),
            odd_pool: ParityPool::new(odd_slots),
            generations: vec![GenerationId::new(0); total_slots as usize].into_boxed_slice(),
        }
    }

    pub fn alloc(&mut self) -> Result<TileKey, AtlasBackendError> {
        let (encoded_slot, raw_slot) = self.alloc_internal()?;
        let generation = self.generations[raw_slot as usize];
        Ok(TileKey::new(
            self.backend_id,
            generation,
            SlotId::new(encoded_slot),
        ))
    }

    pub fn alloc_with_parity(&mut self, parity: bool) -> Result<TileKey, AtlasBackendError> {
        let (encoded_slot, raw_slot) = self
            .alloc_from_parity(parity)
            .ok_or(AtlasBackendError::OutOfSlots)?;
        let generation = self.generations[raw_slot as usize];
        Ok(TileKey::new(
            self.backend_id,
            generation,
            SlotId::new(encoded_slot),
        ))
    }

    fn alloc_internal(&mut self) -> Result<(u32, u32), AtlasBackendError> {
        if let Some(alloc) = self.alloc_from_parity(false) {
            return Ok(alloc);
        }

        if let Some(alloc) = self.alloc_from_parity(true) {
            return Ok(alloc);
        }

        Err(AtlasBackendError::OutOfSlots)
    }

    fn alloc_from_parity(&mut self, parity: bool) -> Option<(u32, u32)> {
        let index = if parity {
            self.odd_pool.alloc()?
        } else {
            self.even_pool.alloc()?
        };
        let encoded = encode_slot(parity, index);
        let raw_slot = self.decode_raw_slot(parity, index);
        Some((encoded, raw_slot))
    }

    fn decode_raw_slot(&self, parity: bool, index_within_parity: u32) -> u32 {
        let tiles_per_layer = self.layout.tiles_per_edge() * self.layout.tiles_per_edge();
        let layer_in_group = index_within_parity / tiles_per_layer;
        let tile_in_layer = index_within_parity % tiles_per_layer;
        let layer = if parity {
            1 + 2 * layer_in_group
        } else {
            2 * layer_in_group
        };
        layer * tiles_per_layer + tile_in_layer
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

        let parity = key.slot_parity();
        let index_within_parity = key.slot_index_within_parity();
        let raw_slot = self.decode_raw_slot(parity, index_within_parity);
        let index = raw_slot as usize;

        let Some(current_generation) = self.generations.get(index).copied() else {
            return Err(AtlasBackendError::InvalidSlot);
        };
        if current_generation != key.generation() {
            return Err(AtlasBackendError::GenerationMismatch);
        };

        let generation = current_generation.raw().wrapping_add(1);
        self.generations[index] = GenerationId::new(generation);

        if parity {
            self.odd_pool.free(index_within_parity);
        } else {
            self.even_pool.free(index_within_parity);
        }
        Ok(())
    }

    pub const fn layout(&self) -> AtlasLayout {
        self.layout
    }

    pub const fn total_slots(&self) -> u32 {
        self.total_slots
    }

    pub const fn backend_id(&self) -> BackendId {
        self.backend_id
    }
}

fn encode_slot(parity: bool, index_within_parity: u32) -> u32 {
    let parity_bit = if parity { 1u32 << 31 } else { 0u32 };
    parity_bit | (index_within_parity & 0x7FFF_FFFF)
}

#[derive(Debug, Default)]
struct ParityPool {
    total_slots: u32,
    next_index: u32,
    freelist: Vec<u32>,
}

impl ParityPool {
    pub const fn new(total_slots: u32) -> Self {
        Self {
            total_slots,
            next_index: 0,
            freelist: Vec::new(),
        }
    }

    pub fn alloc(&mut self) -> Option<u32> {
        if let Some(index) = self.freelist.pop() {
            return Some(index);
        }

        if self.next_index >= self.total_slots {
            return None;
        }

        let index = self.next_index;
        self.next_index = self
            .next_index
            .checked_add(1)
            .expect("ParityPool index overflow: this indicates a logic bug in slot allocation");
        Some(index)
    }

    pub fn free(&mut self, index: u32) {
        self.freelist.push(index);
    }

    pub fn clear(&mut self) {
        self.next_index = 0;
        self.freelist.clear();
    }

    pub fn allocated(&self) -> u32 {
        let reused = self.freelist.len().min(self.next_index as usize) as u32;
        self.next_index - reused
    }

    pub fn is_empty(&self) -> bool {
        self.allocated() == 0
    }
}

#[derive(Default)]
pub struct BackendManager {
    backends: Vec<Backend>,
}

impl BackendManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_backend(
        &mut self,
        layout: AtlasLayout,
    ) -> Result<BackendId, AtlasBackendManagerError> {
        let raw_id = u8::try_from(self.backends.len())
            .map_err(|_| AtlasBackendManagerError::TooManyBackends)?;
        let backend_id = BackendId::new(raw_id);
        self.backends.push(Backend::new(layout, backend_id));
        Ok(backend_id)
    }

    pub fn backend(&self, backend_id: BackendId) -> Option<&Backend> {
        self.backends.get(backend_id.raw() as usize)
    }

    pub fn backend_mut(&mut self, backend_id: BackendId) -> Option<&mut Backend> {
        self.backends.get_mut(backend_id.raw() as usize)
    }

    pub fn backend_for_key(&self, key: TileKey) -> Option<&Backend> {
        self.backend(key.backend())
    }

    pub fn backend_for_key_mut(&mut self, key: TileKey) -> Option<&mut Backend> {
        self.backend_mut(key.backend())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AtlasBackendError, AtlasBackendManagerError, AtlasLayout, Backend, BackendManager,
        ParityPool,
    };
    use glaphica_core::BackendId;

    #[test]
    fn parity_pool_allocate_increases_index() {
        let mut pool = ParityPool::new(3);
        let a = pool.alloc().unwrap();
        let b = pool.alloc().unwrap();
        let c = pool.alloc().unwrap();
        assert!(pool.alloc().is_none());
        assert_eq!(a, 0);
        assert_eq!(b, 1);
        assert_eq!(c, 2);
        assert_eq!(pool.allocated(), 3);
    }

    #[test]
    fn parity_pool_freed_index_is_reused() {
        let mut pool = ParityPool::new(2);
        let a = pool.alloc().unwrap();
        let b = pool.alloc().unwrap();
        pool.free(a);
        let reused = pool.alloc().unwrap();
        assert_eq!(reused, a);
        assert_eq!(pool.allocated(), 2);
        assert_eq!(b, 1);
    }

    #[test]
    fn alloc_returns_even_slots_first() {
        let mut backend = Backend::new(AtlasLayout::Small11, BackendId::new(0));
        let key = backend.alloc().unwrap();
        assert!(!key.slot_parity());
    }

    #[test]
    fn alloc_fills_even_layers_before_odd() {
        let mut backend = Backend::new(AtlasLayout::Small11, BackendId::new(0));
        let tiles_per_layer = 32 * 32;

        for _ in 0..tiles_per_layer {
            let key = backend.alloc().unwrap();
            assert!(!key.slot_parity());
        }

        let odd_key = backend.alloc().unwrap();
        assert!(odd_key.slot_parity());
    }

    #[test]
    fn alloc_with_parity_uses_requested_pool() {
        let mut backend = Backend::new(AtlasLayout::Small11, BackendId::new(0));
        let odd_key = backend.alloc_with_parity(true).unwrap();
        let even_key = backend.alloc_with_parity(false).unwrap();
        assert!(odd_key.slot_parity());
        assert!(!even_key.slot_parity());
    }

    #[test]
    fn alloc_with_parity_rejects_unavailable_parity_pool() {
        let mut backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(0));
        let err = backend.alloc_with_parity(true).unwrap_err();
        assert_eq!(err, AtlasBackendError::OutOfSlots);
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

    #[test]
    fn manager_maps_backend_id_and_supports_key_lookup() {
        let mut manager = BackendManager::new();
        let backend0 = manager.add_backend(AtlasLayout::Tiny8).unwrap();
        let _backend1 = manager.add_backend(AtlasLayout::Tiny8).unwrap();
        assert_eq!(backend0.raw(), 0);

        let key = manager.backend_mut(backend0).unwrap().alloc().unwrap();

        let backend = manager.backend_for_key(key).unwrap();
        assert_eq!(backend.backend_id().raw(), backend0.raw());
    }

    #[test]
    fn manager_rejects_more_than_256_backends() {
        let mut manager = BackendManager::new();
        for _ in 0..256 {
            manager.add_backend(AtlasLayout::Tiny8).unwrap();
        }

        let err = manager.add_backend(AtlasLayout::Tiny8).unwrap_err();
        assert_eq!(err, AtlasBackendManagerError::TooManyBackends);
    }

    #[test]
    fn tiny8_only_uses_even_pool() {
        let mut backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(0));
        let keys = backend.alloc_batch(300);
        assert_eq!(keys.len(), 256);
        for key in &keys {
            assert!(!key.slot_parity());
        }
    }

    #[test]
    fn medium14_allocates_across_parity_groups() {
        let mut backend = Backend::new(AtlasLayout::Medium14, BackendId::new(0));
        let tiles_per_layer = 64 * 64;
        let even_layers = 2;
        let odd_layers = 2;

        let mut even_count = 0;
        let mut odd_count = 0;

        for _ in 0..(even_layers * tiles_per_layer) {
            let key = backend.alloc().unwrap();
            if key.slot_parity() {
                odd_count += 1;
            } else {
                even_count += 1;
            }
        }

        assert_eq!(even_count, even_layers * tiles_per_layer);
        assert_eq!(odd_count, 0);

        odd_count = 0;
        for _ in 0..(odd_layers * tiles_per_layer) {
            let key = backend.alloc().unwrap();
            assert!(key.slot_parity());
            odd_count += 1;
        }

        assert_eq!(odd_count, odd_layers * tiles_per_layer);
    }
}
