use std::collections::VecDeque;
use std::fmt::{Display, Formatter};
use std::sync::{Arc, Mutex, MutexGuard};

const SLOT_BITS: u32 = 32;
const GENERATION_BITS: u32 = 24;
const BACKEND_BITS: u32 = 8;

const SLOT_SHIFT: u32 = 0;
const GENERATION_SHIFT: u32 = SLOT_BITS;
const BACKEND_SHIFT: u32 = GENERATION_SHIFT + GENERATION_BITS;

const SLOT_MASK: u64 = (1u64 << SLOT_BITS) - 1;
const GENERATION_MASK: u64 = (1u64 << GENERATION_BITS) - 1;
const BACKEND_MASK: u64 = (1u64 << BACKEND_BITS) - 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(transparent)]
pub struct BackendId(u8);

impl BackendId {
    pub const fn new(raw: u8) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> u8 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(transparent)]
pub struct TileKey(u64);

impl TileKey {
    pub const EMPTY: Self = Self(u64::MAX);

    const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    const fn raw(self) -> u64 {
        self.0
    }

    pub fn parts(self) -> TileKeyParts {
        decode_tile_key(self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TileKeyParts {
    pub backend_id: BackendId,
    pub generation: u32,
    pub slot_index: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AtlasSlotAddress {
    pub layer: u32,
    pub tile_x: u32,
    pub tile_y: u32,
}

#[derive(Debug)]
pub struct TileOwner {
    recycle: BackendRecycleHandle,
    key: TileKey,
}

impl TileOwner {
    pub fn empty() -> Self {
        Self {
            recycle: BackendRecycleHandle::new(),
            key: TileKey::EMPTY,
        }
    }

    pub const fn tile_key(&self) -> TileKey {
        self.key
    }

    pub fn backend_id(&self) -> BackendId {
        decode_tile_key(self.key).backend_id
    }

    fn new(recycle: BackendRecycleHandle, key: TileKey) -> Self {
        Self { recycle, key }
    }
}

impl Drop for TileOwner {
    fn drop(&mut self) {
        if self.key == TileKey::EMPTY {
            return;
        }
        self.recycle.enqueue(self.key);
        self.key = TileKey::EMPTY;
    }
}

#[derive(Debug, Clone)]
struct BackendRecycleHandle {
    pending_keys: Arc<Mutex<Vec<TileKey>>>,
}

impl BackendRecycleHandle {
    fn new() -> Self {
        Self {
            pending_keys: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn enqueue(&self, key: TileKey) {
        let Ok(mut pending_keys) = self.pending_keys.lock() else {
            eprintln!("atlas: failed to lock recycle queue while dropping tile owner");
            return;
        };
        pending_keys.push(key);
    }

    fn drain(&self) -> Result<Vec<TileKey>, AtlasError> {
        let mut pending_keys = self
            .pending_keys
            .lock()
            .map_err(|_| AtlasError::BackendPoisoned)?;
        Ok(std::mem::take(&mut *pending_keys))
    }
}

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
            Self::Tiny8 => 1 << 8,
            Self::Small11 => 1 << 11,
            Self::Medium14 => 1 << 14,
            Self::Large17 => 1 << 17,
            Self::Huge20 => 1 << 20,
        }
    }

    pub const fn layers(self) -> u32 {
        match self {
            Self::Tiny8 => 1,
            Self::Small11 => 2,
            Self::Medium14 => 4,
            Self::Large17 => 8,
            Self::Huge20 => 16,
        }
    }

    pub const fn tiles_per_edge(self) -> u32 {
        match self {
            Self::Tiny8 => 16,
            Self::Small11 => 32,
            Self::Medium14 => 64,
            Self::Large17 => 128,
            Self::Huge20 => 256,
        }
    }

    pub fn slot_address(self, slot_index: u32) -> Option<AtlasSlotAddress> {
        if slot_index >= self.total_slots() {
            return None;
        }

        let tiles_per_edge = self.tiles_per_edge();
        let slots_per_layer = tiles_per_edge.checked_mul(tiles_per_edge)?;
        let layer = slot_index / slots_per_layer;
        let layer_slot = slot_index % slots_per_layer;
        Some(AtlasSlotAddress {
            layer,
            tile_x: layer_slot % tiles_per_edge,
            tile_y: layer_slot / tiles_per_edge,
        })
    }

    pub fn tile_key_address(self, tile_key: TileKey) -> Option<AtlasSlotAddress> {
        if tile_key == TileKey::EMPTY {
            return None;
        }
        self.slot_address(tile_key.parts().slot_index)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtlasError {
    OutOfSlots,
    WrongBackend,
    InvalidSlot,
    GenerationMismatch,
    InvalidState,
    BackendPoisoned,
}

impl Display for AtlasError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OutOfSlots => write!(f, "atlas is out of slots"),
            Self::WrongBackend => write!(f, "tile key belongs to a different backend"),
            Self::InvalidSlot => write!(f, "tile slot is invalid"),
            Self::GenerationMismatch => write!(f, "tile generation does not match"),
            Self::InvalidState => write!(f, "atlas slot is in an invalid state for this operation"),
            Self::BackendPoisoned => write!(f, "atlas backend lock is poisoned"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtlasManagerError {
    TooManyBackends,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TileState {
    Active,
    Cached,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackendTileStats {
    pub backend_id: BackendId,
    pub active: u32,
    pub cached: u32,
    pub free: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClearBatch {
    pub backend_id: BackendId,
    pub slots: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedTileGroup {
    keys: Vec<TileKey>,
}

impl CachedTileGroup {
    pub fn keys(&self) -> &[TileKey] {
        &self.keys
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct CacheGroupId(u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SlotOwner {
    Vacant,
    Active,
    Cached(CacheGroupId),
}

#[derive(Debug, Clone, Default)]
struct CacheGroup {
    slots: Vec<u32>,
}

#[derive(Debug, Default)]
struct SlotPool {
    next_slot: u32,
    free_list: Vec<u32>,
}

impl SlotPool {
    fn alloc(&mut self, total_slots: u32) -> Option<u32> {
        if let Some(slot) = self.free_list.pop() {
            return Some(slot);
        }

        if self.next_slot >= total_slots {
            return None;
        }

        let slot = self.next_slot;
        self.next_slot = self.next_slot.checked_add(1)?;
        Some(slot)
    }

    fn free(&mut self, slot: u32) {
        self.free_list.push(slot);
    }

    fn available(&self, total_slots: u32) -> usize {
        total_slots as usize - self.allocated()
    }

    fn allocated(&self) -> usize {
        self.next_slot as usize - self.free_list.len()
    }
}

#[derive(Debug)]
struct BackendInner {
    backend_id: BackendId,
    layout: AtlasLayout,
    slot_pool: SlotPool,
    generations: Box<[u32]>,
    slot_owners: Box<[SlotOwner]>,
    cache_groups: Vec<CacheGroup>,
    cached_group_queue: VecDeque<CacheGroupId>,
    next_group_id: u32,
    pending_clear_batches: Vec<ClearBatch>,
}

impl BackendInner {
    fn new(layout: AtlasLayout, backend_id: BackendId) -> Self {
        let total_slots = layout.total_slots();
        Self {
            backend_id,
            layout,
            slot_pool: SlotPool::default(),
            generations: vec![0; total_slots as usize].into_boxed_slice(),
            slot_owners: vec![SlotOwner::Vacant; total_slots as usize].into_boxed_slice(),
            cache_groups: Vec::new(),
            cached_group_queue: VecDeque::new(),
            next_group_id: 0,
            pending_clear_batches: Vec::new(),
        }
    }

    fn alloc_active(&mut self) -> Result<TileKey, AtlasError> {
        self.ensure_capacity(1)?;
        let slot = self.alloc_slot()?;
        self.slot_owners[slot as usize] = SlotOwner::Active;
        Ok(encode_tile_key(
            self.backend_id,
            self.generations[slot as usize],
            slot,
        ))
    }

    fn alloc_cached(&mut self, count: usize) -> Result<CachedTileGroup, AtlasError> {
        self.ensure_capacity(count)?;
        let group_id = self.acquire_vacant_group();
        let mut keys = Vec::with_capacity(count);

        for _ in 0..count {
            let slot = self.alloc_slot()?;
            self.slot_owners[slot as usize] = SlotOwner::Cached(group_id);
            self.group_mut(group_id)?.slots.push(slot);
            keys.push(encode_tile_key(
                self.backend_id,
                self.generations[slot as usize],
                slot,
            ));
        }

        self.cached_group_queue.push_back(group_id);
        Ok(CachedTileGroup { keys })
    }

    fn cache_active_tiles(&mut self, keys: &[TileKey]) -> Result<CachedTileGroup, AtlasError> {
        if keys.is_empty() {
            return Ok(CachedTileGroup { keys: Vec::new() });
        }

        let group_id = self.acquire_vacant_group();
        let mut cached_keys = Vec::with_capacity(keys.len());
        for &key in keys {
            let slot = self.validate_key(key)?;
            if self.slot_owners[slot as usize] != SlotOwner::Active {
                return Err(AtlasError::InvalidState);
            }
            self.slot_owners[slot as usize] = SlotOwner::Cached(group_id);
            self.group_mut(group_id)?.slots.push(slot);
            cached_keys.push(key);
        }

        self.cached_group_queue.push_back(group_id);
        Ok(CachedTileGroup { keys: cached_keys })
    }

    fn activate_cached_tile(&mut self, key: TileKey) -> Result<TileKey, AtlasError> {
        let slot = self.validate_key(key)?;
        let SlotOwner::Cached(group_id) = self.slot_owners[slot as usize] else {
            return Err(AtlasError::InvalidState);
        };

        self.detach_slot_from_group(group_id, slot)?;
        self.slot_owners[slot as usize] = SlotOwner::Active;
        Ok(key)
    }

    fn free(&mut self, key: TileKey) -> Result<(), AtlasError> {
        let slot = self.validate_key(key)?;
        match self.slot_owners[slot as usize] {
            SlotOwner::Vacant => Err(AtlasError::InvalidSlot),
            SlotOwner::Active => {
                let mut cleared_slots = Vec::with_capacity(1);
                self.release_slot(slot, &mut cleared_slots)?;
                self.push_clear_batch(cleared_slots);
                Ok(())
            }
            SlotOwner::Cached(group_id) => self.release_group(group_id),
        }
    }

    fn free_owned_active(&mut self, key: TileKey) -> Result<(), AtlasError> {
        let slot = self.validate_key(key)?;
        if self.slot_owners[slot as usize] != SlotOwner::Active {
            return Err(AtlasError::InvalidState);
        }

        let mut cleared_slots = Vec::with_capacity(1);
        self.release_slot(slot, &mut cleared_slots)?;
        self.push_clear_batch(cleared_slots);
        Ok(())
    }

    fn tile_state(&self, key: TileKey) -> Result<TileState, AtlasError> {
        let slot = self.validate_key(key)?;
        match self.slot_owners[slot as usize] {
            SlotOwner::Vacant => Err(AtlasError::InvalidSlot),
            SlotOwner::Active => Ok(TileState::Active),
            SlotOwner::Cached(_) => Ok(TileState::Cached),
        }
    }

    fn tile_stats(&self) -> BackendTileStats {
        let mut active = 0u32;
        let mut cached = 0u32;
        let mut free = 0u32;

        for owner in self.slot_owners.iter().copied() {
            match owner {
                SlotOwner::Vacant => free += 1,
                SlotOwner::Active => active += 1,
                SlotOwner::Cached(_) => cached += 1,
            }
        }

        BackendTileStats {
            backend_id: self.backend_id,
            active,
            cached,
            free,
        }
    }

    fn take_pending_clear_batches(&mut self) -> Vec<ClearBatch> {
        std::mem::take(&mut self.pending_clear_batches)
    }

    fn alloc_slot(&mut self) -> Result<u32, AtlasError> {
        self.slot_pool
            .alloc(self.layout.total_slots())
            .ok_or(AtlasError::OutOfSlots)
    }

    fn ensure_capacity(&mut self, count: usize) -> Result<(), AtlasError> {
        while self.slot_pool.available(self.layout.total_slots()) < count {
            if !self.reclaim_oldest_cached_group()? {
                return Err(AtlasError::OutOfSlots);
            }
        }
        Ok(())
    }

    fn reclaim_oldest_cached_group(&mut self) -> Result<bool, AtlasError> {
        while let Some(group_id) = self.cached_group_queue.pop_front() {
            if !self.group(group_id)?.slots.is_empty() {
                self.release_group(group_id)?;
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn validate_key(&self, key: TileKey) -> Result<u32, AtlasError> {
        let decoded = decode_tile_key(key);
        if decoded.backend_id != self.backend_id {
            return Err(AtlasError::WrongBackend);
        }

        let Some(generation) = self.generations.get(decoded.slot_index as usize).copied() else {
            return Err(AtlasError::InvalidSlot);
        };
        if generation != decoded.generation {
            return Err(AtlasError::GenerationMismatch);
        }
        if self.slot_owners[decoded.slot_index as usize] == SlotOwner::Vacant {
            return Err(AtlasError::InvalidSlot);
        }

        Ok(decoded.slot_index)
    }

    fn release_slot(&mut self, slot: u32, cleared_slots: &mut Vec<u32>) -> Result<(), AtlasError> {
        match self.slot_owners[slot as usize] {
            SlotOwner::Vacant => return Err(AtlasError::InvalidSlot),
            SlotOwner::Active => {}
            SlotOwner::Cached(group_id) => {
                self.detach_slot_from_group(group_id, slot)?;
            }
        }

        self.generations[slot as usize] = self.generations[slot as usize].wrapping_add(1);
        self.slot_owners[slot as usize] = SlotOwner::Vacant;
        self.slot_pool.free(slot);
        cleared_slots.push(slot);
        Ok(())
    }

    fn release_group(&mut self, group_id: CacheGroupId) -> Result<(), AtlasError> {
        let slots = self.group(group_id)?.slots.clone();
        let mut cleared_slots = Vec::with_capacity(slots.len());
        for slot in slots {
            self.release_slot(slot, &mut cleared_slots)?;
        }
        let group = self.group_mut(group_id)?;
        group.slots.clear();
        self.push_clear_batch(cleared_slots);
        Ok(())
    }

    fn push_clear_batch(&mut self, slots: Vec<u32>) {
        if slots.is_empty() {
            return;
        }

        self.pending_clear_batches.push(ClearBatch {
            backend_id: self.backend_id,
            slots,
        });
    }

    fn acquire_vacant_group(&mut self) -> CacheGroupId {
        if let Some(index) = self
            .cache_groups
            .iter()
            .position(|group| group.slots.is_empty())
        {
            return CacheGroupId(index as u32);
        }

        let id = CacheGroupId(self.next_group_id);
        self.next_group_id = self.next_group_id.wrapping_add(1);
        self.cache_groups.push(CacheGroup::default());
        id
    }

    fn group(&self, group_id: CacheGroupId) -> Result<&CacheGroup, AtlasError> {
        self.cache_groups
            .get(group_id.0 as usize)
            .ok_or(AtlasError::InvalidState)
    }

    fn group_mut(&mut self, group_id: CacheGroupId) -> Result<&mut CacheGroup, AtlasError> {
        self.cache_groups
            .get_mut(group_id.0 as usize)
            .ok_or(AtlasError::InvalidState)
    }

    fn detach_slot_from_group(
        &mut self,
        group_id: CacheGroupId,
        slot: u32,
    ) -> Result<(), AtlasError> {
        let slots = &mut self.group_mut(group_id)?.slots;
        let Some(index) = slots.iter().position(|&candidate| candidate == slot) else {
            return Err(AtlasError::InvalidSlot);
        };
        slots.swap_remove(index);
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct Backend {
    inner: Arc<Mutex<BackendInner>>,
    recycle: BackendRecycleHandle,
}

impl Backend {
    pub fn new(layout: AtlasLayout, backend_id: BackendId) -> Self {
        Self {
            inner: Arc::new(Mutex::new(BackendInner::new(layout, backend_id))),
            recycle: BackendRecycleHandle::new(),
        }
    }

    pub fn backend_id(&self) -> Result<BackendId, AtlasError> {
        Ok(self.lock_inner()?.backend_id)
    }

    pub fn layout(&self) -> Result<AtlasLayout, AtlasError> {
        Ok(self.lock_inner()?.layout)
    }

    pub fn alloc_active(&self) -> Result<TileOwner, AtlasError> {
        let mut inner = self.lock_inner()?;
        self.drain_owned_reclaims(&mut inner)?;
        let key = inner.alloc_active()?;
        Ok(TileOwner::new(self.recycle.clone(), key))
    }

    pub fn alloc_cached(&self, count: usize) -> Result<CachedTileGroup, AtlasError> {
        let mut inner = self.lock_inner()?;
        self.drain_owned_reclaims(&mut inner)?;
        inner.alloc_cached(count)
    }

    pub fn cache_active_tiles(&self, keys: &[TileKey]) -> Result<CachedTileGroup, AtlasError> {
        let mut inner = self.lock_inner()?;
        self.drain_owned_reclaims(&mut inner)?;
        inner.cache_active_tiles(keys)
    }

    pub fn activate_cached_tile(&self, key: TileKey) -> Result<TileOwner, AtlasError> {
        let mut inner = self.lock_inner()?;
        self.drain_owned_reclaims(&mut inner)?;
        let key = inner.activate_cached_tile(key)?;
        Ok(TileOwner::new(self.recycle.clone(), key))
    }

    pub fn free(&self, key: TileKey) -> Result<(), AtlasError> {
        let mut inner = self.lock_inner()?;
        self.drain_owned_reclaims(&mut inner)?;
        inner.free(key)
    }

    pub fn tile_state(&self, key: TileKey) -> Result<TileState, AtlasError> {
        let mut inner = self.lock_inner()?;
        self.drain_owned_reclaims(&mut inner)?;
        inner.tile_state(key)
    }

    pub fn tile_stats(&self) -> Result<BackendTileStats, AtlasError> {
        let mut inner = self.lock_inner()?;
        self.drain_owned_reclaims(&mut inner)?;
        Ok(inner.tile_stats())
    }

    pub fn take_pending_clear_batches(&self) -> Result<Vec<ClearBatch>, AtlasError> {
        let mut inner = self.lock_inner()?;
        self.drain_owned_reclaims(&mut inner)?;
        Ok(inner.take_pending_clear_batches())
    }

    fn lock_inner(&self) -> Result<MutexGuard<'_, BackendInner>, AtlasError> {
        self.inner.lock().map_err(|_| AtlasError::BackendPoisoned)
    }

    fn drain_owned_reclaims(&self, inner: &mut BackendInner) -> Result<(), AtlasError> {
        for key in self.recycle.drain()? {
            match inner.free_owned_active(key) {
                Ok(()) | Err(AtlasError::GenerationMismatch) | Err(AtlasError::InvalidSlot) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
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

    pub fn add_backend(&mut self, layout: AtlasLayout) -> Result<BackendId, AtlasManagerError> {
        let raw =
            u8::try_from(self.backends.len()).map_err(|_| AtlasManagerError::TooManyBackends)?;
        let backend_id = BackendId::new(raw);
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
        let decoded = decode_tile_key(key);
        self.backend(decoded.backend_id)
    }

    pub fn backend_for_key_mut(&mut self, key: TileKey) -> Option<&mut Backend> {
        let decoded = decode_tile_key(key);
        self.backend_mut(decoded.backend_id)
    }

    pub fn backend_tile_stats(&self) -> Result<Vec<BackendTileStats>, AtlasError> {
        self.backends.iter().map(Backend::tile_stats).collect()
    }
}

fn encode_tile_key(backend_id: BackendId, generation: u32, slot_index: u32) -> TileKey {
    let raw = ((backend_id.raw() as u64 & BACKEND_MASK) << BACKEND_SHIFT)
        | ((generation as u64 & GENERATION_MASK) << GENERATION_SHIFT)
        | ((slot_index as u64 & SLOT_MASK) << SLOT_SHIFT);
    TileKey::from_raw(raw)
}

fn decode_tile_key(key: TileKey) -> TileKeyParts {
    let raw = key.raw();
    TileKeyParts {
        backend_id: BackendId::new(((raw >> BACKEND_SHIFT) & BACKEND_MASK) as u8),
        generation: ((raw >> GENERATION_SHIFT) & GENERATION_MASK) as u32,
        slot_index: ((raw >> SLOT_SHIFT) & SLOT_MASK) as u32,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AtlasError, AtlasLayout, Backend, BackendId, BackendManager, ClearBatch, TileState,
        decode_tile_key,
    };

    #[test]
    fn active_allocation_uses_sequential_slots() {
        let backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(2));

        let first = backend.alloc_active();
        assert!(first.is_ok());
        let first = match first {
            Ok(owner) => owner,
            Err(_) => return,
        };
        let second = backend.alloc_active();
        assert!(second.is_ok());
        let second = match second {
            Ok(owner) => owner,
            Err(_) => return,
        };

        assert_eq!(
            decode_tile_key(first.tile_key()).backend_id,
            BackendId::new(2)
        );
        assert_eq!(decode_tile_key(first.tile_key()).slot_index, 0);
        assert_eq!(decode_tile_key(second.tile_key()).slot_index, 1);
    }

    #[test]
    fn cached_groups_reclaim_oldest_first() {
        let backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(0));
        let total_slots = AtlasLayout::Tiny8.total_slots() as usize;

        let oldest = backend.alloc_cached(total_slots / 2);
        assert!(oldest.is_ok());
        let oldest = match oldest {
            Ok(group) => group,
            Err(_) => return,
        };
        let newest = backend.alloc_cached(total_slots / 2);
        assert!(newest.is_ok());
        let newest = match newest {
            Ok(group) => group,
            Err(_) => return,
        };
        let replacement = backend.alloc_active();
        assert!(replacement.is_ok());
        let replacement_slot = match replacement {
            Ok(owner) => decode_tile_key(owner.tile_key()).slot_index,
            Err(_) => return,
        };
        let oldest_slots: Vec<u32> = oldest
            .keys()
            .iter()
            .copied()
            .map(|key| decode_tile_key(key).slot_index)
            .collect();

        assert_eq!(
            backend.tile_state(oldest.keys()[0]),
            Err(AtlasError::GenerationMismatch)
        );
        assert_eq!(backend.tile_state(newest.keys()[0]), Ok(TileState::Cached));
        assert!(oldest_slots.contains(&replacement_slot));
    }

    #[test]
    fn dropping_active_owner_returns_slot_directly_to_pool() {
        let backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(0));
        let active = backend.alloc_active();
        assert!(active.is_ok());
        let active = match active {
            Ok(owner) => owner,
            Err(_) => return,
        };
        let active_slot = decode_tile_key(active.tile_key()).slot_index;

        drop(active);
        assert_eq!(
            backend.take_pending_clear_batches(),
            Ok(vec![ClearBatch {
                backend_id: BackendId::new(0),
                slots: vec![active_slot],
            }])
        );

        let replacement = backend.alloc_active();
        assert!(replacement.is_ok());
        let replacement = match replacement {
            Ok(owner) => owner,
            Err(_) => return,
        };
        assert_eq!(
            decode_tile_key(replacement.tile_key()).slot_index,
            active_slot
        );
    }

    #[test]
    fn freeing_cached_key_releases_whole_group() {
        let backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(0));
        let cached = backend.alloc_cached(3);
        assert!(cached.is_ok());
        let cached = match cached {
            Ok(group) => group,
            Err(_) => return,
        };
        let cached_slots: Vec<u32> = cached
            .keys()
            .iter()
            .copied()
            .map(|key| decode_tile_key(key).slot_index)
            .collect();

        assert!(backend.free(cached.keys()[1]).is_ok());

        for &key in cached.keys() {
            assert_eq!(backend.tile_state(key), Err(AtlasError::GenerationMismatch));
        }
        assert_eq!(
            backend.take_pending_clear_batches(),
            Ok(vec![ClearBatch {
                backend_id: BackendId::new(0),
                slots: cached_slots,
            }])
        );
    }

    #[test]
    fn activate_cached_tile_returns_active_owner() {
        let backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(0));
        let cached = backend.alloc_cached(2);
        assert!(cached.is_ok());
        let cached = match cached {
            Ok(group) => group,
            Err(_) => return,
        };

        let reactivated = backend.activate_cached_tile(cached.keys()[0]);
        assert!(reactivated.is_ok());
        let reactivated = match reactivated {
            Ok(owner) => owner,
            Err(_) => return,
        };

        assert_eq!(reactivated.tile_key(), cached.keys()[0]);
        assert_eq!(backend.tile_state(cached.keys()[0]), Ok(TileState::Active));
        assert_eq!(backend.tile_state(cached.keys()[1]), Ok(TileState::Cached));
    }

    #[test]
    fn manager_resolves_backend_from_tile_key() {
        let mut manager = BackendManager::new();
        let backend_id = manager.add_backend(AtlasLayout::Tiny8).unwrap();
        let tile = manager
            .backend_mut(backend_id)
            .and_then(|backend| backend.alloc_active().ok());
        assert!(tile.is_some());
        let tile = match tile {
            Some(owner) => owner.tile_key(),
            None => return,
        };

        let resolved = manager.backend_for_key(tile);
        assert!(resolved.is_some());
        let resolved = match resolved {
            Some(backend) => backend,
            None => return,
        };

        assert_eq!(resolved.backend_id(), Ok(backend_id));
    }
}
