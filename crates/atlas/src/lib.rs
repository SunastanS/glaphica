use glaphica_core::{AtlasLayout, BackendId, GenerationId, SlotId, TileKey};
use std::collections::VecDeque;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtlasBackendError {
    OutOfSlots,
    WrongBackend,
    InvalidSlot,
    GenerationMismatch,
    InvalidGroup,
    InvalidState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtlasBackendManagerError {
    TooManyBackends,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TileState {
    Active,
    Cached,
    Vacant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackendTileStats {
    pub backend_id: BackendId,
    pub active: u32,
    pub cached: u32,
    pub free: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct TileGroupId(u32);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TileGroupAllocation {
    pub group: TileGroupId,
    pub keys: Vec<TileKey>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ActiveTile {
    key: TileKey,
}

impl ActiveTile {
    pub const fn key(&self) -> TileKey {
        self.key
    }
}

#[derive(Debug, Default)]
pub struct EditSession {
    retired: Vec<TileKey>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TileKeySwap {
    pub restore_key: TileKey,
    pub retire_key: TileKey,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SlotOwner {
    Vacant,
    UngroupedActive,
    Grouped(TileGroupId),
}

pub struct Backend {
    backend_id: BackendId,
    layout: AtlasLayout,
    total_slots: u32,
    even_pool: ParityPool,
    odd_pool: ParityPool,
    generations: Box<[GenerationId]>,
    slot_owners: Box<[SlotOwner]>,
    groups: Vec<TileGroup>,
    cached_groups: VecDeque<TileGroupId>,
    next_group_id: u32,
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
            slot_owners: vec![SlotOwner::Vacant; total_slots as usize].into_boxed_slice(),
            groups: Vec::new(),
            cached_groups: VecDeque::new(),
            next_group_id: 0,
        }
    }

    pub fn begin_edit(&self) -> EditSession {
        EditSession::default()
    }

    pub fn alloc_active(&mut self) -> Result<ActiveTile, AtlasBackendError> {
        self.alloc_active_internal(None)
    }

    pub fn alloc_active_with_parity(
        &mut self,
        parity: bool,
    ) -> Result<ActiveTile, AtlasBackendError> {
        self.alloc_active_internal(Some(parity))
    }

    pub fn replace(
        &mut self,
        session: &mut EditSession,
        old: ActiveTile,
        new_tile: ActiveTile,
    ) -> Result<ActiveTile, AtlasBackendError> {
        self.ensure_tile_is_active(old.key())?;
        self.ensure_tile_is_active(new_tile.key())?;
        session.retired.push(old.key());
        Ok(new_tile)
    }

    pub fn replace_key(
        &mut self,
        session: &mut EditSession,
        old: TileKey,
        new_tile: TileKey,
    ) -> Result<(), AtlasBackendError> {
        self.ensure_tile_is_active(old)?;
        self.ensure_tile_is_active(new_tile)?;
        session.retired.push(old);
        Ok(())
    }

    pub fn release(
        &mut self,
        session: &mut EditSession,
        tile: ActiveTile,
    ) -> Result<(), AtlasBackendError> {
        self.ensure_tile_is_active(tile.key())?;
        session.retired.push(tile.key());
        Ok(())
    }

    pub fn release_key(
        &mut self,
        session: &mut EditSession,
        key: TileKey,
    ) -> Result<(), AtlasBackendError> {
        self.ensure_tile_is_active(key)?;
        session.retired.push(key);
        Ok(())
    }

    pub fn drop(
        &mut self,
        session: &mut EditSession,
        tile: ActiveTile,
    ) -> Result<(), AtlasBackendError> {
        self.drop_key(session, tile.key())
    }

    pub fn drop_key(
        &mut self,
        session: &mut EditSession,
        key: TileKey,
    ) -> Result<(), AtlasBackendError> {
        self.ensure_tile_is_active(key)?;
        session.retired.push(key);
        Ok(())
    }

    pub fn finish_edit(&mut self, session: EditSession) -> Result<(), AtlasBackendError> {
        if session.retired.is_empty() {
            return Ok(());
        }

        let group = self.acquire_vacant_group();
        self.assign_tiles_to_group(group, &session.retired)?;
        self.mark_group_cached(group)
    }

    pub fn finish_drop(&mut self, session: EditSession) -> Result<(), AtlasBackendError> {
        if session.retired.is_empty() {
            return Ok(());
        }

        let group = self.acquire_vacant_group();
        self.assign_tiles_to_group(group, &session.retired)?;
        self.mark_group_drop_first(group)
    }

    pub fn restore_cached_keys(&mut self, swaps: &[TileKeySwap]) -> Result<(), AtlasBackendError> {
        for swap in swaps {
            if swap.restore_key == TileKey::EMPTY {
                continue;
            }
            if self.tile_state(swap.restore_key)? != TileState::Cached {
                return Err(AtlasBackendError::InvalidState);
            }
        }

        for swap in swaps {
            if swap.restore_key != TileKey::EMPTY {
                self.reactivate_cached_key(swap.restore_key)?;
            }
        }
        Ok(())
    }

    pub(crate) fn create_group(&mut self) -> TileGroupId {
        let group = TileGroupId(self.next_group_id);
        self.next_group_id = self.next_group_id.wrapping_add(1);
        self.groups.push(TileGroup::default());
        group
    }

    pub(crate) fn alloc_group(
        &mut self,
        count: usize,
    ) -> Result<TileGroupAllocation, AtlasBackendError> {
        let group = self.create_group();
        self.alloc_in_group_internal(group, count, None)
    }

    pub(crate) fn alloc_in_group(
        &mut self,
        group: TileGroupId,
        count: usize,
    ) -> Result<TileGroupAllocation, AtlasBackendError> {
        self.alloc_in_group_internal(group, count, None)
    }

    fn assign_tiles_to_group(
        &mut self,
        group: TileGroupId,
        keys: &[TileKey],
    ) -> Result<(), AtlasBackendError> {
        if self.group(group)?.state == TileState::Cached {
            return Err(AtlasBackendError::InvalidState);
        }
        if self.group(group)?.state == TileState::Vacant {
            self.group_mut(group)?.state = TileState::Active;
        }
        for &key in keys {
            let raw_slot = self.validate_key(key)?;
            match self.slot_owners[raw_slot as usize] {
                SlotOwner::Grouped(source_group) if source_group == group => continue,
                SlotOwner::Grouped(source_group) => {
                    self.detach_slot_from_group(source_group, raw_slot)?;
                }
                SlotOwner::UngroupedActive => {}
                SlotOwner::Vacant => return Err(AtlasBackendError::InvalidSlot),
            }
            self.attach_slot_to_group(group, raw_slot)?;
        }
        Ok(())
    }

    fn mark_group_cached(&mut self, group: TileGroupId) -> Result<(), AtlasBackendError> {
        let tile_group = self.group_mut(group)?;
        if tile_group.state != TileState::Cached {
            tile_group.state = TileState::Cached;
            self.cached_groups.push_back(group);
        }
        Ok(())
    }

    fn mark_group_drop_first(&mut self, group: TileGroupId) -> Result<(), AtlasBackendError> {
        let tile_group = self.group_mut(group)?;
        if tile_group.state != TileState::Cached {
            tile_group.state = TileState::Cached;
            self.cached_groups.push_front(group);
        }
        Ok(())
    }

    pub(crate) fn clear_group(&mut self, group: TileGroupId) -> Result<(), AtlasBackendError> {
        if self.group(group)?.state == TileState::Vacant {
            return Ok(());
        }
        self.release_group(group)
    }

    pub fn tile_state(&self, key: TileKey) -> Result<TileState, AtlasBackendError> {
        let raw_slot = self.validate_key(key)?;
        match self.slot_owners[raw_slot as usize] {
            SlotOwner::Vacant => Err(AtlasBackendError::InvalidSlot),
            SlotOwner::UngroupedActive => Ok(TileState::Active),
            SlotOwner::Grouped(group) => Ok(self.group(group)?.state),
        }
    }

    pub(crate) fn group_state(&self, group: TileGroupId) -> Result<TileState, AtlasBackendError> {
        Ok(self.group(group)?.state)
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

    pub fn alloc_active_batch(&mut self, count: usize) -> Vec<TileKey> {
        let mut keys = Vec::with_capacity(count);
        for _ in 0..count {
            match self.alloc_active() {
                Ok(tile) => keys.push(tile.key()),
                Err(AtlasBackendError::OutOfSlots) => break,
                Err(_) => break,
            }
        }
        keys
    }

    pub fn free(&mut self, key: TileKey) -> Result<(), AtlasBackendError> {
        let raw_slot = self.validate_key(key)?;
        match self.slot_owners[raw_slot as usize] {
            SlotOwner::Vacant => Err(AtlasBackendError::InvalidSlot),
            SlotOwner::UngroupedActive => self.release_slot(raw_slot),
            SlotOwner::Grouped(group) => self.release_group(group),
        }
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

    pub fn tile_stats(&self) -> BackendTileStats {
        let mut active = 0u32;
        let mut cached = 0u32;
        let mut free = 0u32;

        for owner in self.slot_owners.iter().copied() {
            match owner {
                SlotOwner::Vacant => free += 1,
                SlotOwner::UngroupedActive => active += 1,
                SlotOwner::Grouped(group) => match self.group(group) {
                    Ok(tile_group) if tile_group.state == TileState::Cached => cached += 1,
                    Ok(_) => active += 1,
                    Err(_) => free += 1,
                },
            }
        }

        BackendTileStats {
            backend_id: self.backend_id,
            active,
            cached,
            free,
        }
    }

    fn alloc_in_group_internal(
        &mut self,
        group: TileGroupId,
        count: usize,
        parity: Option<bool>,
    ) -> Result<TileGroupAllocation, AtlasBackendError> {
        let state = self.group(group)?.state;
        if state == TileState::Cached {
            return Err(AtlasBackendError::InvalidState);
        }
        if state == TileState::Vacant {
            self.group_mut(group)?.state = TileState::Active;
        }
        self.ensure_capacity(count, parity)?;
        let mut keys = Vec::with_capacity(count);

        for _ in 0..count {
            let (encoded_slot, raw_slot) = match parity {
                Some(parity) => self.alloc_from_parity(parity),
                None => self.alloc_internal(),
            }
            .ok_or(AtlasBackendError::OutOfSlots)?;
            let generation = self.generations[raw_slot as usize];
            self.slot_owners[raw_slot as usize] = SlotOwner::UngroupedActive;
            self.attach_slot_to_group(group, raw_slot)?;
            keys.push(TileKey::new(
                self.backend_id,
                generation,
                SlotId::new(encoded_slot),
            ));
        }

        Ok(TileGroupAllocation { group, keys })
    }

    fn alloc_active_internal(
        &mut self,
        parity: Option<bool>,
    ) -> Result<ActiveTile, AtlasBackendError> {
        self.ensure_capacity(1, parity)?;
        let (encoded_slot, raw_slot) = match parity {
            Some(parity) => self.alloc_from_parity(parity),
            None => self.alloc_internal(),
        }
        .ok_or(AtlasBackendError::OutOfSlots)?;
        let generation = self.generations[raw_slot as usize];
        self.slot_owners[raw_slot as usize] = SlotOwner::UngroupedActive;
        Ok(ActiveTile {
            key: TileKey::new(self.backend_id, generation, SlotId::new(encoded_slot)),
        })
    }

    fn alloc_internal(&mut self) -> Option<(u32, u32)> {
        self.alloc_from_parity(false)
            .or_else(|| self.alloc_from_parity(true))
    }

    fn alloc_from_parity(&mut self, parity: bool) -> Option<(u32, u32)> {
        let index = if parity {
            self.odd_pool.alloc()?
        } else {
            self.even_pool.alloc()?
        };
        Some((
            encode_slot(parity, index),
            self.decode_raw_slot(parity, index),
        ))
    }

    fn ensure_capacity(
        &mut self,
        count: usize,
        parity: Option<bool>,
    ) -> Result<(), AtlasBackendError> {
        while !self.has_capacity(count, parity) {
            if !self.reclaim_oldest_cached_group()? {
                return Err(AtlasBackendError::OutOfSlots);
            }
        }
        Ok(())
    }

    fn has_capacity(&self, count: usize, parity: Option<bool>) -> bool {
        match parity {
            Some(true) => self.odd_pool.available() as usize >= count,
            Some(false) => self.even_pool.available() as usize >= count,
            None => (self.even_pool.available() + self.odd_pool.available()) as usize >= count,
        }
    }

    fn reclaim_oldest_cached_group(&mut self) -> Result<bool, AtlasBackendError> {
        while let Some(group) = self.cached_groups.pop_front() {
            if self.group(group)?.state == TileState::Cached {
                self.release_group(group)?;
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn acquire_vacant_group(&mut self) -> TileGroupId {
        if let Some(index) = self
            .groups
            .iter()
            .position(|group| group.state == TileState::Vacant)
        {
            return TileGroupId(index as u32);
        }
        self.create_group()
    }

    fn release_group(&mut self, group: TileGroupId) -> Result<(), AtlasBackendError> {
        let slots = self.group(group)?.slots.clone();
        for raw_slot in slots {
            self.release_slot(raw_slot)?;
        }
        let tile_group = self.group_mut(group)?;
        tile_group.slots.clear();
        tile_group.state = TileState::Vacant;
        Ok(())
    }

    fn release_slot(&mut self, raw_slot: u32) -> Result<(), AtlasBackendError> {
        match self.slot_owners[raw_slot as usize] {
            SlotOwner::Vacant => return Err(AtlasBackendError::InvalidSlot),
            SlotOwner::Grouped(group) => {
                self.detach_slot_from_group(group, raw_slot)?;
            }
            SlotOwner::UngroupedActive => {}
        }

        let generation = self.generations[raw_slot as usize].raw().wrapping_add(1);
        self.generations[raw_slot as usize] = GenerationId::new(generation);
        self.slot_owners[raw_slot as usize] = SlotOwner::Vacant;

        let parity =
            ((raw_slot / (self.layout.tiles_per_edge() * self.layout.tiles_per_edge())) & 1) == 1;
        let index_within_parity = raw_slot_to_parity_index(self.layout, raw_slot);
        if parity {
            self.odd_pool.free(index_within_parity);
        } else {
            self.even_pool.free(index_within_parity);
        }
        Ok(())
    }

    fn reactivate_cached_key(&mut self, key: TileKey) -> Result<(), AtlasBackendError> {
        let raw_slot = self.validate_key(key)?;
        let SlotOwner::Grouped(group) = self.slot_owners[raw_slot as usize] else {
            return Err(AtlasBackendError::InvalidState);
        };
        if self.group(group)?.state != TileState::Cached {
            return Err(AtlasBackendError::InvalidState);
        }

        self.detach_slot_from_group(group, raw_slot)?;
        self.slot_owners[raw_slot as usize] = SlotOwner::UngroupedActive;
        if self.group(group)?.slots.is_empty() {
            self.group_mut(group)?.state = TileState::Vacant;
        }
        Ok(())
    }

    fn validate_key(&self, key: TileKey) -> Result<u32, AtlasBackendError> {
        if key.backend() != self.backend_id {
            return Err(AtlasBackendError::WrongBackend);
        }
        let raw_slot = self.decode_raw_slot(key.slot_parity(), key.slot_index_within_parity());
        let Some(current_generation) = self.generations.get(raw_slot as usize).copied() else {
            return Err(AtlasBackendError::InvalidSlot);
        };
        if current_generation != key.generation() {
            return Err(AtlasBackendError::GenerationMismatch);
        }
        if self.slot_owners[raw_slot as usize] == SlotOwner::Vacant {
            return Err(AtlasBackendError::InvalidSlot);
        }
        Ok(raw_slot)
    }

    fn ensure_tile_is_active(&self, key: TileKey) -> Result<u32, AtlasBackendError> {
        let raw_slot = self.validate_key(key)?;
        if self.tile_state(key)? != TileState::Active {
            return Err(AtlasBackendError::InvalidState);
        }
        Ok(raw_slot)
    }

    fn attach_slot_to_group(
        &mut self,
        group: TileGroupId,
        raw_slot: u32,
    ) -> Result<(), AtlasBackendError> {
        self.slot_owners[raw_slot as usize] = SlotOwner::Grouped(group);
        self.group_mut(group)?.slots.push(raw_slot);
        Ok(())
    }

    fn detach_slot_from_group(
        &mut self,
        group: TileGroupId,
        raw_slot: u32,
    ) -> Result<(), AtlasBackendError> {
        let slots = &mut self.group_mut(group)?.slots;
        let Some(index) = slots.iter().position(|&slot| slot == raw_slot) else {
            return Err(AtlasBackendError::InvalidSlot);
        };
        slots.swap_remove(index);
        Ok(())
    }

    fn group(&self, group: TileGroupId) -> Result<&TileGroup, AtlasBackendError> {
        self.groups
            .get(group.0 as usize)
            .ok_or(AtlasBackendError::InvalidGroup)
    }

    fn group_mut(&mut self, group: TileGroupId) -> Result<&mut TileGroup, AtlasBackendError> {
        self.groups
            .get_mut(group.0 as usize)
            .ok_or(AtlasBackendError::InvalidGroup)
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

    pub fn available(&self) -> u32 {
        self.total_slots - self.allocated()
    }

    pub fn is_empty(&self) -> bool {
        self.allocated() == 0
    }
}

#[derive(Debug, Clone)]
struct TileGroup {
    state: TileState,
    slots: Vec<u32>,
}

impl Default for TileGroup {
    fn default() -> Self {
        Self {
            state: TileState::Vacant,
            slots: Vec::new(),
        }
    }
}

fn raw_slot_to_parity_index(layout: AtlasLayout, raw_slot: u32) -> u32 {
    let tiles_per_layer = layout.tiles_per_edge() * layout.tiles_per_edge();
    let layer = raw_slot / tiles_per_layer;
    let tile_in_layer = raw_slot % tiles_per_layer;
    (layer / 2) * tiles_per_layer + tile_in_layer
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

    pub fn backend_tile_stats(&self) -> Vec<BackendTileStats> {
        self.backends.iter().map(Backend::tile_stats).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AtlasBackendError, AtlasBackendManagerError, AtlasLayout, Backend, BackendManager,
        ParityPool, TileGroupId, TileKeySwap, TileState,
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
        let key = backend.alloc_active().unwrap().key();
        assert!(!key.slot_parity());
    }

    #[test]
    fn alloc_fills_even_layers_before_odd() {
        let mut backend = Backend::new(AtlasLayout::Small11, BackendId::new(0));
        let tiles_per_layer = 32 * 32;

        for _ in 0..tiles_per_layer {
            let key = backend.alloc_active().unwrap().key();
            assert!(!key.slot_parity());
        }

        let odd_key = backend.alloc_active().unwrap().key();
        assert!(odd_key.slot_parity());
    }

    #[test]
    fn alloc_with_parity_uses_requested_pool() {
        let mut backend = Backend::new(AtlasLayout::Small11, BackendId::new(0));
        let odd_key = backend.alloc_active_with_parity(true).unwrap().key();
        let even_key = backend.alloc_active_with_parity(false).unwrap().key();
        assert!(odd_key.slot_parity());
        assert!(!even_key.slot_parity());
    }

    #[test]
    fn alloc_with_parity_rejects_unavailable_parity_pool() {
        let mut backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(0));
        let err = backend.alloc_active_with_parity(true).unwrap_err();
        assert_eq!(err, AtlasBackendError::OutOfSlots);
    }

    #[test]
    fn alloc_batch_returns_available_keys() {
        let mut backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(1));
        let keys = backend.alloc_active_batch(300);
        assert_eq!(keys.len(), 256);
        assert_eq!(
            backend.alloc_active().unwrap_err(),
            AtlasBackendError::OutOfSlots
        );
    }

    #[test]
    fn free_uses_tile_key_and_bumps_generation() {
        let mut backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(1));
        let key = backend.alloc_active().unwrap().key();
        assert_eq!(backend.tile_state(key).unwrap(), TileState::Active);
        let slot = key.slot().raw();
        let generation = key.generation().raw();

        backend.free(key).unwrap();
        let next = backend.alloc_active().unwrap().key();

        assert_eq!(next.slot().raw(), slot);
        assert_eq!(next.generation().raw(), generation.wrapping_add(1));
    }

    #[test]
    fn finish_edit_moves_replaced_tile_into_cached_state() {
        let mut backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(0));
        let old_tile = backend.alloc_active().unwrap();
        let old_key = old_tile.key();
        let new_tile = backend.alloc_active().unwrap();
        let mut session = backend.begin_edit();

        let new_tile = backend.replace(&mut session, old_tile, new_tile).unwrap();
        assert_eq!(
            backend.tile_state(new_tile.key()).unwrap(),
            TileState::Active
        );

        backend.finish_edit(session).unwrap();

        assert_eq!(
            backend.tile_state(new_tile.key()).unwrap(),
            TileState::Active
        );
        assert_eq!(backend.tile_state(old_key).unwrap(), TileState::Cached);
    }

    #[test]
    fn restore_cached_keys_restores_previous_tile_without_retiring_current_tile() {
        let mut backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(0));
        let old_key = backend.alloc_active().unwrap().key();
        let new_key = backend.alloc_active().unwrap().key();
        let mut session = backend.begin_edit();
        backend.replace_key(&mut session, old_key, new_key).unwrap();
        backend.finish_edit(session).unwrap();

        backend
            .restore_cached_keys(&[TileKeySwap {
                restore_key: old_key,
                retire_key: new_key,
            }])
            .unwrap();

        assert_eq!(backend.tile_state(old_key).unwrap(), TileState::Active);
        assert_eq!(backend.tile_state(new_key).unwrap(), TileState::Active);
    }

    #[test]
    fn restore_cached_keys_rejects_reclaimed_restore_key() {
        let mut backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(0));
        let old_key = backend.alloc_active().unwrap().key();
        let new_key = backend.alloc_active().unwrap().key();
        let mut session = backend.begin_edit();
        backend.replace_key(&mut session, old_key, new_key).unwrap();
        backend.finish_edit(session).unwrap();
        backend.free(old_key).unwrap();

        let err = backend
            .restore_cached_keys(&[TileKeySwap {
                restore_key: old_key,
                retire_key: new_key,
            }])
            .unwrap_err();

        assert!(matches!(
            err,
            AtlasBackendError::GenerationMismatch | AtlasBackendError::InvalidSlot
        ));
        assert_eq!(backend.tile_state(new_key).unwrap(), TileState::Active);
    }

    #[test]
    fn finish_drop_prioritizes_group_for_next_reclaim() {
        let mut backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(0));
        let first = backend.alloc_group(1).unwrap();
        let dropped_key = backend.alloc_active().unwrap().key();
        backend.mark_group_cached(first.group).unwrap();

        let mut drop_session = backend.begin_edit();
        backend.drop_key(&mut drop_session, dropped_key).unwrap();
        backend.finish_drop(drop_session).unwrap();

        for _ in 0..254 {
            backend.alloc_active().unwrap();
        }

        let reused = backend.alloc_active().unwrap().key();
        assert_eq!(reused.slot().raw(), dropped_key.slot().raw());
        assert_eq!(
            backend.tile_state(first.keys[0]).unwrap(),
            TileState::Cached
        );
    }

    #[test]
    fn finish_edit_reuses_vacant_group_ids_for_cached_tiles() {
        let mut backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(0));
        let old_tile = backend.alloc_active().unwrap();
        let new_tile = backend.alloc_active().unwrap();
        let mut first_session = backend.begin_edit();
        backend
            .replace(&mut first_session, old_tile, new_tile)
            .unwrap();
        backend.finish_edit(first_session).unwrap();

        let first_group = backend
            .groups
            .iter()
            .position(|group| group.state == TileState::Cached)
            .unwrap() as u32;
        backend.clear_group(TileGroupId(first_group)).unwrap();

        let old_tile = backend.alloc_active().unwrap();
        let new_tile = backend.alloc_active().unwrap();
        let mut second_session = backend.begin_edit();
        backend
            .replace(&mut second_session, old_tile, new_tile)
            .unwrap();
        backend.finish_edit(second_session).unwrap();

        let reused_group = backend
            .groups
            .iter()
            .position(|group| group.state == TileState::Cached)
            .unwrap() as u32;
        assert_eq!(reused_group, first_group);
    }

    #[test]
    fn free_releases_the_entire_layer() {
        let mut backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(1));
        let allocation = backend.alloc_group(2).unwrap();
        let slots: Vec<_> = allocation.keys.iter().map(|key| key.slot().raw()).collect();
        let generations: Vec<_> = allocation
            .keys
            .iter()
            .map(|key| key.generation().raw())
            .collect();

        backend.free(allocation.keys[0]).unwrap();
        let next = backend.alloc_group(2).unwrap();
        let next_slots: Vec<_> = next.keys.iter().map(|key| key.slot().raw()).collect();
        let next_generations: Vec<_> = next.keys.iter().map(|key| key.generation().raw()).collect();

        assert_eq!(next_slots, vec![slots[1], slots[0]]);
        assert_eq!(
            next_generations,
            vec![
                generations[1].wrapping_add(1),
                generations[0].wrapping_add(1),
            ]
        );
    }

    #[test]
    fn free_returns_error_for_wrong_backend() {
        let mut backend0 = Backend::new(AtlasLayout::Tiny8, BackendId::new(0));
        let mut backend1 = Backend::new(AtlasLayout::Tiny8, BackendId::new(1));
        let key = backend0.alloc_active().unwrap().key();
        let err = backend1.free(key).unwrap_err();
        assert_eq!(err, AtlasBackendError::WrongBackend);
    }

    #[test]
    fn alloc_prefers_vacant_tiles_before_cached_reclaim() {
        let mut backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(0));
        let cached = backend.alloc_group(1).unwrap();
        let vacant = backend.alloc_group(1).unwrap();

        for _ in 0..254 {
            backend.alloc_active().unwrap();
        }

        backend.mark_group_cached(cached.group).unwrap();
        backend.free(vacant.keys[0]).unwrap();

        let next = backend.alloc_active().unwrap().key();
        assert_eq!(next.slot().raw(), vacant.keys[0].slot().raw());
        assert_eq!(
            backend.tile_state(cached.keys[0]).unwrap(),
            TileState::Cached
        );
    }

    #[test]
    fn alloc_reclaims_cached_groups_in_fifo_order() {
        let mut backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(0));
        let first = backend.alloc_group(1).unwrap();
        let second = backend.alloc_group(1).unwrap();

        for _ in 0..254 {
            backend.alloc_active().unwrap();
        }

        backend.mark_group_cached(first.group).unwrap();
        backend.mark_group_cached(second.group).unwrap();

        let reused = backend.alloc_active().unwrap().key();
        assert_eq!(reused.slot().raw(), first.keys[0].slot().raw());
        assert_eq!(
            reused.generation().raw(),
            first.keys[0].generation().raw().wrapping_add(1)
        );
        assert_eq!(
            backend.tile_state(second.keys[0]).unwrap(),
            TileState::Cached
        );
    }

    #[test]
    fn backend_maintains_monotonic_group_ids() {
        let mut backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(0));
        let first = backend.create_group();
        let second = backend.create_group();
        let third = backend.create_group();

        assert_eq!(first.0, 0);
        assert_eq!(second.0, 1);
        assert_eq!(third.0, 2);
    }

    #[test]
    fn same_group_id_cycles_vacant_active_cached_vacant() {
        let mut backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(0));
        let group = backend.create_group();
        assert_eq!(backend.group_state(group).unwrap(), TileState::Vacant);

        let allocation = backend.alloc_in_group(group, 2).unwrap();
        assert_eq!(allocation.group, group);
        assert_eq!(backend.group_state(group).unwrap(), TileState::Active);

        backend.mark_group_cached(group).unwrap();
        assert_eq!(backend.group_state(group).unwrap(), TileState::Cached);

        backend.clear_group(group).unwrap();
        assert_eq!(backend.group_state(group).unwrap(), TileState::Vacant);

        let reused = backend.alloc_in_group(group, 1).unwrap();
        assert_eq!(reused.group, group);
        assert_eq!(backend.group_state(group).unwrap(), TileState::Active);
    }

    #[test]
    fn assign_tiles_to_cached_group_rejects_invalid_state() {
        let mut backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(0));
        let source = backend.alloc_group(1).unwrap();
        let cached = backend.create_group();
        backend.mark_group_cached(cached).unwrap();

        let err = backend
            .assign_tiles_to_group(cached, &source.keys)
            .unwrap_err();
        assert_eq!(err, AtlasBackendError::InvalidState);
    }

    #[test]
    fn assign_tiles_to_group_regroups_old_active_keys() {
        let mut backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(0));
        let image_group = backend.create_group();
        let image_tiles = backend.alloc_in_group(image_group, 2).unwrap();
        let replaced_key = image_tiles.keys[0];
        let untouched_key = image_tiles.keys[1];

        let stroke_group = backend.create_group();
        let replacement = backend.alloc_in_group(stroke_group, 1).unwrap();

        let retained_group = backend.create_group();
        backend
            .assign_tiles_to_group(retained_group, &[replaced_key])
            .unwrap();
        backend.mark_group_cached(retained_group).unwrap();

        assert_eq!(
            backend.tile_state(replacement.keys[0]).unwrap(),
            TileState::Active
        );
        assert_eq!(
            backend.tile_state(untouched_key).unwrap(),
            TileState::Active
        );
        assert_eq!(backend.tile_state(replaced_key).unwrap(), TileState::Cached);
        assert_eq!(backend.group_state(image_group).unwrap(), TileState::Active);
    }

    #[test]
    fn manager_maps_backend_id_and_supports_key_lookup() {
        let mut manager = BackendManager::new();
        let backend0 = manager.add_backend(AtlasLayout::Tiny8).unwrap();
        let _backend1 = manager.add_backend(AtlasLayout::Tiny8).unwrap();
        assert_eq!(backend0.raw(), 0);

        let key = manager
            .backend_mut(backend0)
            .unwrap()
            .alloc_active()
            .unwrap()
            .key();

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
        let keys = backend.alloc_active_batch(300);
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
            let key = backend.alloc_active().unwrap().key();
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
            let key = backend.alloc_active().unwrap().key();
            assert!(key.slot_parity());
            odd_count += 1;
        }

        assert_eq!(odd_count, odd_layers * tiles_per_layer);
    }
}
