use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Mutex, mpsc};

use crate::{INDEX_SHARDS, TILE_STRIDE};
use crate::{
    TileAddress, TileAllocError, TileAtlasCreateError, TileKey, TileSetError, TileSetHandle, TileSetId,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::atlas) struct AtlasUsage {
    bits: u8,
}

impl AtlasUsage {
    const COPY_DST_BIT: u8 = 1 << 0;
    const TEXTURE_BINDING_BIT: u8 = 1 << 1;
    const STORAGE_BINDING_BIT: u8 = 1 << 2;

    pub(in crate::atlas) const fn empty() -> Self {
        Self { bits: 0 }
    }

    pub(in crate::atlas) const fn with_copy_dst(mut self) -> Self {
        self.bits |= Self::COPY_DST_BIT;
        self
    }

    pub(in crate::atlas) const fn with_texture_binding(mut self) -> Self {
        self.bits |= Self::TEXTURE_BINDING_BIT;
        self
    }

    pub(in crate::atlas) const fn with_storage_binding(mut self) -> Self {
        self.bits |= Self::STORAGE_BINDING_BIT;
        self
    }

    pub(in crate::atlas) const fn contains_copy_dst(self) -> bool {
        (self.bits & Self::COPY_DST_BIT) != 0
    }

    pub(in crate::atlas) const fn contains_texture_binding(self) -> bool {
        (self.bits & Self::TEXTURE_BINDING_BIT) != 0
    }

    pub(in crate::atlas) const fn contains_storage_binding(self) -> bool {
        (self.bits & Self::STORAGE_BINDING_BIT) != 0
    }
}

#[derive(Debug, Clone, Copy)]
pub(in crate::atlas) struct AtlasCoreConfig {
    pub max_layers: u32,
    pub tiles_per_row: u32,
    pub tiles_per_column: u32,
}

#[derive(Debug, Clone, Copy)]
pub(in crate::atlas) struct AtlasLayout {
    pub tiles_per_row: u32,
    pub tiles_per_column: u32,
    pub atlas_width: u32,
    pub atlas_height: u32,
    pub tiles_per_atlas: u32,
    pub atlas_occupancy_words: usize,
}

impl AtlasLayout {
    pub(in crate::atlas) fn from_config(
        config: AtlasCoreConfig,
    ) -> Result<Self, TileAtlasCreateError> {
        if config.tiles_per_row == 0 || config.tiles_per_column == 0 {
            return Err(TileAtlasCreateError::AtlasTileGridZero);
        }
        if config.tiles_per_row != config.tiles_per_column {
            return Err(TileAtlasCreateError::AtlasTileGridNotSquare);
        }
        let tiles_per_atlas = config
            .tiles_per_row
            .checked_mul(config.tiles_per_column)
            .ok_or(TileAtlasCreateError::AtlasTileGridTooLarge)?;
        let _tile_index_capacity: u16 = tiles_per_atlas
            .checked_sub(1)
            .ok_or(TileAtlasCreateError::AtlasTileGridTooLarge)?
            .try_into()
            .map_err(|_| TileAtlasCreateError::AtlasTileGridTooLarge)?;
        let atlas_width = config
            .tiles_per_row
            .checked_mul(TILE_STRIDE)
            .ok_or(TileAtlasCreateError::AtlasSizeExceedsDeviceLimit)?;
        let atlas_height = config
            .tiles_per_column
            .checked_mul(TILE_STRIDE)
            .ok_or(TileAtlasCreateError::AtlasSizeExceedsDeviceLimit)?;
        let atlas_occupancy_words = (tiles_per_atlas as usize).div_ceil(64);
        Ok(Self {
            tiles_per_row: config.tiles_per_row,
            tiles_per_column: config.tiles_per_column,
            atlas_width,
            atlas_height,
            tiles_per_atlas,
            atlas_occupancy_words,
        })
    }
}

#[derive(Debug, Clone)]
pub(in crate::atlas) enum TileOp<UploadPayload> {
    Clear {
        target: TileOpTarget,
    },
    ClearBatch {
        targets: Vec<TileOpTarget>,
    },
    Upload {
        target: TileOpTarget,
        payload: UploadPayload,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::atlas) struct TileOpTarget {
    pub address: TileAddress,
    generation: u32,
}

#[derive(Debug, Clone, Copy)]
struct TileRecord {
    address: TileAddress,
    generation: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TileKeyState {
    Active,
    Retained { batch_id: u64 },
}

#[derive(Debug)]
struct RetainedBatchState {
    retain_id: u64,
    keys: HashSet<TileKey>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvictedRetainBatch {
    pub retain_id: u64,
    pub keys: Vec<TileKey>,
}

#[derive(Debug, Default)]
struct TileKeyLifecycleGcState {
    key_states: HashMap<TileKey, TileKeyState>,
    retained_batches: HashMap<u64, RetainedBatchState>,
    retained_order: VecDeque<u64>,
    next_batch_id: u64,
    evicted_retain_batches: VecDeque<EvictedRetainBatch>,
}

impl TileKeyLifecycleGcState {
    fn on_key_allocated(&mut self, key: TileKey) {
        let previous = self.key_states.insert(key, TileKeyState::Active);
        if previous.is_some() {
            panic!("allocated key already exists in lifecycle state");
        }
    }

    fn on_key_released(&mut self, key: TileKey) {
        let Some(previous_state) = self.key_states.remove(&key) else {
            panic!("released key missing from lifecycle state");
        };
        if let TileKeyState::Retained { batch_id } = previous_state {
            self.remove_key_from_retain_batch(batch_id, key);
        }
    }

    fn mark_keys_active(&mut self, keys: &[TileKey]) {
        for key in keys {
            let Some(previous_state) = self.key_states.get(key).copied() else {
                panic!("cannot mark unknown key as active");
            };
            if let TileKeyState::Retained { batch_id } = previous_state {
                self.remove_key_from_retain_batch(batch_id, *key);
            }
            self.key_states.insert(*key, TileKeyState::Active);
        }
    }

    fn retain_keys_new_batch(&mut self, retain_id: u64, keys: &[TileKey]) {
        if keys.is_empty() {
            panic!("cannot retain empty key batch");
        }
        let batch_id = self.create_retain_batch(retain_id);
        for key in keys {
            let Some(previous_state) = self.key_states.get(key).copied() else {
                panic!("cannot retain unknown key");
            };
            if let TileKeyState::Retained {
                batch_id: previous_batch_id,
            } = previous_state
            {
                self.remove_key_from_retain_batch(previous_batch_id, *key);
            }
            let batch = self
                .retained_batches
                .get_mut(&batch_id)
                .expect("retain batch must exist");
            let inserted = batch.keys.insert(*key);
            if !inserted {
                panic!("retain batch duplicated key");
            }
            self.key_states
                .insert(*key, TileKeyState::Retained { batch_id });
        }
    }

    fn oldest_retain_batch_keys(&mut self) -> Option<(u64, Vec<TileKey>)> {
        loop {
            let batch_id = *self.retained_order.front()?;
            let Some(batch) = self.retained_batches.get(&batch_id) else {
                let _ = self.retained_order.pop_front();
                continue;
            };
            if batch.keys.is_empty() {
                self.retained_batches.remove(&batch_id);
                let _ = self.retained_order.pop_front();
                continue;
            }
            return Some((batch.retain_id, batch.keys.iter().copied().collect()));
        }
    }

    fn record_evicted_batch(&mut self, retain_id: u64, keys: Vec<TileKey>) {
        self.evicted_retain_batches
            .push_back(EvictedRetainBatch { retain_id, keys });
    }

    fn drain_evicted_retain_batches(&mut self) -> Vec<EvictedRetainBatch> {
        self.evicted_retain_batches.drain(..).collect()
    }

    fn remove_key_from_retain_batch(&mut self, batch_id: u64, key: TileKey) {
        let Some(batch) = self.retained_batches.get_mut(&batch_id) else {
            panic!("retained key points to missing retain batch");
        };
        let removed = batch.keys.remove(&key);
        if !removed {
            panic!("retained key missing from retain batch");
        }
        if batch.keys.is_empty() {
            self.retained_batches.remove(&batch_id);
        }
    }

    fn create_retain_batch(&mut self, retain_id: u64) -> u64 {
        let batch_id = self
            .next_batch_id
            .checked_add(1)
            .expect("retain batch id overflow");
        self.next_batch_id = batch_id;
        self.retained_order.push_back(batch_id);
        self.retained_batches.insert(
            batch_id,
            RetainedBatchState {
                retain_id,
                keys: HashSet::new(),
            },
        );
        batch_id
    }
}

#[derive(Clone, Debug)]
pub(in crate::atlas) struct TileOpSender<UploadPayload> {
    sender: mpsc::Sender<TileOp<UploadPayload>>,
}

impl<UploadPayload> TileOpSender<UploadPayload> {
    pub(in crate::atlas) fn send(
        &self,
        operation: TileOp<UploadPayload>,
    ) -> Result<(), TileAllocError> {
        self.sender
            .send(operation)
            .map_err(|_| TileAllocError::QueueDisconnected)
    }
}

#[derive(Debug)]
pub(in crate::atlas) struct TileOpQueue<UploadPayload> {
    receiver: Mutex<mpsc::Receiver<TileOp<UploadPayload>>>,
}

impl<UploadPayload> TileOpQueue<UploadPayload> {
    pub(in crate::atlas) fn new() -> (TileOpSender<UploadPayload>, Self) {
        let (sender, receiver) = mpsc::channel();
        (
            TileOpSender { sender },
            Self {
                receiver: Mutex::new(receiver),
            },
        )
    }

    pub(in crate::atlas) fn drain(&self) -> Vec<TileOp<UploadPayload>> {
        let mut operations = Vec::new();
        let receiver = self
            .receiver
            .lock()
            .expect("tile op queue receiver lock poisoned");
        loop {
            match receiver.try_recv() {
                Ok(operation) => operations.push(operation),
                Err(mpsc::TryRecvError::Empty) | Err(mpsc::TryRecvError::Disconnected) => break,
            }
        }
        operations
    }
}

#[derive(Debug)]
struct TileAllocatorPage {
    free_tiles: Mutex<Vec<u16>>,
    dirty_bits: Mutex<Vec<u64>>,
    generations: Mutex<Vec<u32>>,
    tiles_per_atlas: u32,
    atlas_occupancy_words: usize,
}

impl TileAllocatorPage {
    fn new(layout: AtlasLayout) -> Result<Self, TileAllocError> {
        let mut free_tiles = Vec::new();
        for tile_index in (0..layout.tiles_per_atlas).rev() {
            let tile_index_u16: u16 = tile_index
                .try_into()
                .map_err(|_| TileAllocError::AtlasFull)?;
            free_tiles.push(tile_index_u16);
        }
        Ok(Self {
            free_tiles: Mutex::new(free_tiles),
            dirty_bits: Mutex::new(vec![0; layout.atlas_occupancy_words]),
            generations: Mutex::new(vec![0; layout.tiles_per_atlas as usize]),
            tiles_per_atlas: layout.tiles_per_atlas,
            atlas_occupancy_words: layout.atlas_occupancy_words,
        })
    }

    fn pop_free(&self) -> Option<u16> {
        self.free_tiles
            .lock()
            .expect("tile allocator free list lock poisoned")
            .pop()
    }

    fn push_free(&self, tile_index: u16) {
        self.free_tiles
            .lock()
            .expect("tile allocator free list lock poisoned")
            .push(tile_index);
    }

    fn mark_dirty(&self, tile_index: u16) -> Result<(), TileAllocError> {
        let (word, mask) = tile_bit(tile_index, self.tiles_per_atlas, self.atlas_occupancy_words)
            .ok_or(TileAllocError::AtlasFull)?;
        let mut dirty_bits = self
            .dirty_bits
            .lock()
            .expect("tile allocator dirty bits lock poisoned");
        dirty_bits[word] |= mask;
        Ok(())
    }

    fn take_dirty(&self, tile_index: u16) -> Result<bool, TileAllocError> {
        let (word, mask) = tile_bit(tile_index, self.tiles_per_atlas, self.atlas_occupancy_words)
            .ok_or(TileAllocError::AtlasFull)?;
        let mut dirty_bits = self
            .dirty_bits
            .lock()
            .expect("tile allocator dirty bits lock poisoned");
        let was_dirty = (dirty_bits[word] & mask) != 0;
        dirty_bits[word] &= !mask;
        Ok(was_dirty)
    }

    fn generation(&self, tile_index: u16) -> Result<u32, TileAllocError> {
        let index =
            tile_slot_index(tile_index, self.tiles_per_atlas).ok_or(TileAllocError::AtlasFull)?;
        let generations = self
            .generations
            .lock()
            .expect("tile allocator generations lock poisoned");
        generations
            .get(index)
            .copied()
            .ok_or(TileAllocError::AtlasFull)
    }

    fn bump_generation(&self, tile_index: u16) -> Result<(), TileAllocError> {
        let index =
            tile_slot_index(tile_index, self.tiles_per_atlas).ok_or(TileAllocError::AtlasFull)?;
        let mut generations = self
            .generations
            .lock()
            .expect("tile allocator generations lock poisoned");
        let generation = generations
            .get_mut(index)
            .ok_or(TileAllocError::AtlasFull)?;
        *generation = generation.wrapping_add(1);
        Ok(())
    }
}

#[derive(Debug)]
pub(in crate::atlas) struct TileAtlasCpu {
    pages: Vec<TileAllocatorPage>,
    index_shards: [Mutex<HashMap<TileKey, TileRecord>>; INDEX_SHARDS],
    lifecycle_gc: Mutex<TileKeyLifecycleGcState>,
    next_key: AtomicU64,
    next_retain_id: AtomicU64,
    owner_tag: u64,
    next_set_id: AtomicU64,
    next_layer_hint: AtomicU32,
    max_layers: u32,
}

static NEXT_ATLAS_OWNER_TAG: AtomicU64 = AtomicU64::new(1);

impl TileAtlasCpu {
    pub(in crate::atlas) fn new(
        max_layers: u32,
        layout: AtlasLayout,
    ) -> Result<Self, TileAllocError> {
        let mut pages = Vec::new();
        for _ in 0..max_layers {
            pages.push(TileAllocatorPage::new(layout)?);
        }

        Ok(Self {
            pages,
            index_shards: std::array::from_fn(|_| Mutex::new(HashMap::new())),
            lifecycle_gc: Mutex::new(TileKeyLifecycleGcState::default()),
            next_key: AtomicU64::new(0),
            next_retain_id: AtomicU64::new(0),
            owner_tag: NEXT_ATLAS_OWNER_TAG.fetch_add(1, Ordering::Relaxed),
            next_set_id: AtomicU64::new(0),
            next_layer_hint: AtomicU32::new(0),
            max_layers,
        })
    }

    pub(in crate::atlas) fn owner_tag(&self) -> u64 {
        self.owner_tag
    }

    pub(in crate::atlas) fn next_set_id(&self) -> Result<TileSetId, TileSetError> {
        loop {
            let current = self.next_set_id.load(Ordering::Relaxed);
            let Some(next) = current.checked_add(1) else {
                return Err(TileSetError::KeySpaceExhausted);
            };
            if self
                .next_set_id
                .compare_exchange(current, next, Ordering::SeqCst, Ordering::Relaxed)
                .is_ok()
            {
                return Ok(TileSetId(current));
            }
        }
    }

    pub(in crate::atlas) fn is_allocated(&self, key: TileKey) -> bool {
        let shard = self.shard_for_key(key);
        self.index_shards[shard]
            .lock()
            .expect("tile index shard lock poisoned")
            .contains_key(&key)
    }

    pub(in crate::atlas) fn resolve(&self, key: TileKey) -> Option<TileAddress> {
        let shard = self.shard_for_key(key);
        self.index_shards[shard]
            .lock()
            .expect("tile index shard lock poisoned")
            .get(&key)
            .map(|record| record.address)
    }

    pub(in crate::atlas) fn resolve_op_target(&self, key: TileKey) -> Option<TileOpTarget> {
        let shard = self.shard_for_key(key);
        self.index_shards[shard]
            .lock()
            .expect("tile index shard lock poisoned")
            .get(&key)
            .map(|record| TileOpTarget {
                address: record.address,
                generation: record.generation,
            })
    }

    fn allocate_raw(&self) -> Result<(TileKey, TileAddress, bool), TileAllocError> {
        let key = self.next_key()?;
        let address = self.take_free_address()?;

        let page = self
            .pages
            .get(address.atlas_layer as usize)
            .ok_or(TileAllocError::AtlasFull)?;
        let generation = page.generation(address.tile_index)?;

        let shard = self.shard_for_key(key);
        self.index_shards[shard]
            .lock()
            .expect("tile index shard lock poisoned")
            .insert(
                key,
                TileRecord {
                    address,
                    generation,
                },
            );
        self.lifecycle_gc
            .lock()
            .expect("tile lifecycle gc lock poisoned")
            .on_key_allocated(key);

        let was_dirty = page.take_dirty(address.tile_index)?;

        Ok((key, address, was_dirty))
    }

    pub(in crate::atlas) fn allocate<UploadPayload>(
        &self,
        op_sender: &TileOpSender<UploadPayload>,
    ) -> Result<(TileKey, TileAddress), TileAllocError> {
        let (key, address, was_dirty) = self.allocate_raw()?;
        if was_dirty {
            let Some(target) = self.resolve_op_target(key) else {
                panic!("allocated tile key must resolve to clear target");
            };
            if op_sender.send(TileOp::Clear { target }).is_err() {
                self.rollback_allocate(key, address, true);
                return Err(TileAllocError::QueueDisconnected);
            }
        }

        Ok((key, address))
    }

    pub(in crate::atlas) fn allocate_without_ops(
        &self,
    ) -> Result<(TileKey, TileAddress), TileAllocError> {
        let (key, address, _was_dirty) = self.allocate_raw()?;
        Ok((key, address))
    }

    pub(in crate::atlas) fn release(&self, key: TileKey) -> bool {
        let shard = self.shard_for_key(key);
        let address = {
            let mut index = self.index_shards[shard]
                .lock()
                .expect("tile index shard lock poisoned");
            index.remove(&key)
        };

        let Some(record) = address else {
            return false;
        };
        self.lifecycle_gc
            .lock()
            .expect("tile lifecycle gc lock poisoned")
            .on_key_released(key);
        let address = record.address;
        let page = self
            .pages
            .get(address.atlas_layer as usize)
            .expect("tile address layer must be valid");
        page.mark_dirty(address.tile_index)
            .expect("tile index must be in range");
        page.bump_generation(address.tile_index)
            .expect("tile index must be in range");
        page.push_free(address.tile_index);
        true
    }

    pub(in crate::atlas) fn release_all(&self) -> usize {
        let mut released = Vec::new();
        for shard_id in 0..INDEX_SHARDS {
            let mut shard = self.index_shards[shard_id]
                .lock()
                .expect("tile index shard lock poisoned");
            released.extend(shard.drain());
        }

        {
            let mut lifecycle_gc = self
                .lifecycle_gc
                .lock()
                .expect("tile lifecycle gc lock poisoned");
            for (key, _) in &released {
                lifecycle_gc.on_key_released(*key);
            }
        }

        for (_, record) in &released {
            let address = record.address;
            let page = self
                .pages
                .get(address.atlas_layer as usize)
                .expect("tile address layer must be valid");
            page.mark_dirty(address.tile_index)
                .expect("tile index must be in range");
            page.bump_generation(address.tile_index)
                .expect("tile index must be in range");
            page.push_free(address.tile_index);
        }

        released.len()
    }

    pub(in crate::atlas) fn release_set_atomic(
        &self,
        keys: &[TileKey],
    ) -> Result<u32, TileSetError> {
        let mut seen = HashSet::with_capacity(keys.len());
        for key in keys {
            if !seen.insert(*key) {
                return Err(TileSetError::DuplicateTileKey);
            }
        }

        let mut shard_ids = keys
            .iter()
            .map(|key| self.shard_for_key(*key))
            .collect::<Vec<_>>();
        shard_ids.sort_unstable();
        shard_ids.dedup();

        let mut shard_locks = shard_ids
            .into_iter()
            .map(|shard_id| {
                (
                    shard_id,
                    self.index_shards[shard_id]
                        .lock()
                        .expect("tile index shard lock poisoned"),
                )
            })
            .collect::<Vec<_>>();

        for key in keys {
            let shard_id = self.shard_for_key(*key);
            let (_, shard) = shard_locks
                .iter_mut()
                .find(|(id, _)| *id == shard_id)
                .expect("target shard lock must exist");
            if !shard.contains_key(key) {
                return Err(TileSetError::UnknownTileKey);
            }
        }

        let mut released = Vec::with_capacity(keys.len());
        for key in keys {
            let shard_id = self.shard_for_key(*key);
            let (_, shard) = shard_locks
                .iter_mut()
                .find(|(id, _)| *id == shard_id)
                .expect("target shard lock must exist");
            let record = shard
                .remove(key)
                .expect("tile key must exist after atomic precheck");
            released.push((*key, record));
        }

        drop(shard_locks);

        {
            let mut lifecycle_gc = self
                .lifecycle_gc
                .lock()
                .expect("tile lifecycle gc lock poisoned");
            for (key, _) in &released {
                lifecycle_gc.on_key_released(*key);
            }
        }

        for (_, record) in &released {
            let address = record.address;
            let page = self
                .pages
                .get(address.atlas_layer as usize)
                .expect("tile address layer must be valid");
            page.mark_dirty(address.tile_index)
                .expect("tile index must be in range");
            page.bump_generation(address.tile_index)
                .expect("tile index must be in range");
            page.push_free(address.tile_index);
        }

        let mut released_count = 0u32;
        for _ in &released {
            released_count = released_count
                .checked_add(1)
                .ok_or(TileSetError::KeySpaceExhausted)?;
        }
        Ok(released_count)
    }

    pub(in crate::atlas) fn rollback_allocate(
        &self,
        key: TileKey,
        address: TileAddress,
        mark_dirty: bool,
    ) {
        let shard = self.shard_for_key(key);
        self.index_shards[shard]
            .lock()
            .expect("tile index shard lock poisoned")
            .remove(&key);
        self.lifecycle_gc
            .lock()
            .expect("tile lifecycle gc lock poisoned")
            .on_key_released(key);

        let page = self
            .pages
            .get(address.atlas_layer as usize)
            .expect("tile address layer must be valid");
        if mark_dirty {
            page.mark_dirty(address.tile_index)
                .expect("tile index must be in range");
        }
        page.bump_generation(address.tile_index)
            .expect("tile index must be in range");
        page.push_free(address.tile_index);
    }

    pub(in crate::atlas) fn should_execute_target(&self, target: TileOpTarget) -> bool {
        let page = self
            .pages
            .get(target.address.atlas_layer as usize)
            .expect("tile address layer must be valid");
        let Ok(generation) = page.generation(target.address.tile_index) else {
            return false;
        };
        generation == target.generation
    }

    fn shard_for_key(&self, key: TileKey) -> usize {
        (key.0 as usize) & (INDEX_SHARDS - 1)
    }

    fn next_key(&self) -> Result<TileKey, TileAllocError> {
        loop {
            let current = self.next_key.load(Ordering::Relaxed);
            let Some(next) = current.checked_add(1) else {
                return Err(TileAllocError::KeySpaceExhausted);
            };
            if self
                .next_key
                .compare_exchange(current, next, Ordering::SeqCst, Ordering::Relaxed)
                .is_ok()
            {
                return Ok(TileKey(current));
            }
        }
    }

    fn next_retain_id(&self) -> u64 {
        loop {
            let current = self.next_retain_id.load(Ordering::Relaxed);
            let Some(next) = current.checked_add(1) else {
                panic!("retain id space exhausted");
            };
            if self
                .next_retain_id
                .compare_exchange(current, next, Ordering::SeqCst, Ordering::Relaxed)
                .is_ok()
            {
                return current;
            }
        }
    }

    fn take_free_address(&self) -> Result<TileAddress, TileAllocError> {
        loop {
            if let Some(address) = self.try_take_free_address() {
                return Ok(address);
            }
            let Some((retain_id, evict_keys)) = self.select_oldest_retain_batch_for_eviction()
            else {
                return Err(TileAllocError::AtlasFull);
            };
            let released_count = self
                .release_set_atomic(&evict_keys)
                .expect("gc retain batch eviction failed to release keys");
            if usize::try_from(released_count).ok() != Some(evict_keys.len()) {
                panic!("gc retain batch eviction released unexpected key count");
            }
            self.lifecycle_gc
                .lock()
                .expect("tile lifecycle gc lock poisoned")
                .record_evicted_batch(retain_id, evict_keys);
        }
    }

    fn try_take_free_address(&self) -> Option<TileAddress> {
        let start = self.next_layer_hint.fetch_add(1, Ordering::Relaxed) % self.max_layers;
        for offset in 0..self.max_layers {
            let layer = (start + offset) % self.max_layers;
            let page = self.pages.get(layer as usize)?;
            if let Some(tile_index) = page.pop_free() {
                return Some(TileAddress {
                    atlas_layer: layer,
                    tile_index,
                });
            }
        }
        None
    }

    fn select_oldest_retain_batch_for_eviction(&self) -> Option<(u64, Vec<TileKey>)> {
        self.lifecycle_gc
            .lock()
            .expect("tile lifecycle gc lock poisoned")
            .oldest_retain_batch_keys()
    }

    pub(in crate::atlas) fn mark_keys_active(&self, keys: &[TileKey]) {
        for key in keys {
            if !self.is_allocated(*key) {
                panic!("cannot mark non-allocated key as active");
            }
        }
        self.lifecycle_gc
            .lock()
            .expect("tile lifecycle gc lock poisoned")
            .mark_keys_active(keys);
    }

    pub(in crate::atlas) fn retain_keys_new_batch(&self, keys: &[TileKey]) -> u64 {
        for key in keys {
            if !self.is_allocated(*key) {
                panic!("cannot retain non-allocated key");
            }
        }
        let retain_id = self.next_retain_id();
        self.lifecycle_gc
            .lock()
            .expect("tile lifecycle gc lock poisoned")
            .retain_keys_new_batch(retain_id, keys);
        retain_id
    }

    pub(in crate::atlas) fn drain_evicted_retain_batches(&self) -> Vec<EvictedRetainBatch> {
        self.lifecycle_gc
            .lock()
            .expect("tile lifecycle gc lock poisoned")
            .drain_evicted_retain_batches()
    }
}

pub(in crate::atlas) fn validate_tile_set_ownership(
    cpu: &TileAtlasCpu,
    set: &TileSetHandle,
) -> Result<(), TileSetError> {
    if set.owner_tag() != cpu.owner_tag() {
        return Err(TileSetError::SetNotOwnedByStore);
    }
    Ok(())
}

pub(in crate::atlas) fn reserve_tile_set_with(
    cpu: &TileAtlasCpu,
    count: u32,
    mut allocate: impl FnMut() -> Result<TileKey, TileAllocError>,
    mut release: impl FnMut(TileKey) -> bool,
) -> Result<TileSetHandle, TileSetError> {
    let set_id = cpu.next_set_id()?;
    let key_count = usize::try_from(count).map_err(|_| TileSetError::KeySpaceExhausted)?;
    let mut keys = Vec::with_capacity(key_count);
    for _ in 0..count {
        match allocate() {
            Ok(key) => keys.push(key),
            Err(error) => {
                for key in keys {
                    let released = release(key);
                    if !released {
                        return Err(TileSetError::RollbackReleaseFailed);
                    }
                }
                return Err(error.into());
            }
        }
    }
    Ok(TileSetHandle::new(set_id, cpu.owner_tag(), keys))
}

pub(in crate::atlas) fn adopt_tile_set(
    cpu: &TileAtlasCpu,
    keys: impl IntoIterator<Item = TileKey>,
) -> Result<TileSetHandle, TileSetError> {
    let set_id = cpu.next_set_id()?;
    let mut tile_keys = Vec::new();
    let mut seen = HashSet::new();
    for key in keys {
        if !seen.insert(key) {
            return Err(TileSetError::DuplicateTileKey);
        }
        if !cpu.is_allocated(key) {
            return Err(TileSetError::UnknownTileKey);
        }
        tile_keys.push(key);
    }
    Ok(TileSetHandle::new(set_id, cpu.owner_tag(), tile_keys))
}

pub(in crate::atlas) fn release_tile_set(
    cpu: &TileAtlasCpu,
    set: TileSetHandle,
) -> Result<u32, TileSetError> {
    validate_tile_set_ownership(cpu, &set)?;
    cpu.release_set_atomic(set.keys())
}

pub(in crate::atlas) fn resolve_tile_set(
    cpu: &TileAtlasCpu,
    set: &TileSetHandle,
) -> Result<Vec<(TileKey, TileAddress)>, TileSetError> {
    validate_tile_set_ownership(cpu, set)?;
    let mut resolved = Vec::with_capacity(set.len());
    for key in set.keys() {
        let Some(address) = cpu.resolve(*key) else {
            return Err(TileSetError::UnknownTileKey);
        };
        resolved.push((*key, address));
    }
    Ok(resolved)
}

fn tile_bit(
    tile_index: u16,
    tiles_per_atlas: u32,
    atlas_occupancy_words: usize,
) -> Option<(usize, u64)> {
    let index = tile_index as usize;
    if index >= (tiles_per_atlas as usize) {
        return None;
    }
    let word = index / 64;
    if word >= atlas_occupancy_words {
        return None;
    }
    let mask = 1u64 << (index % 64);
    Some((word, mask))
}

fn tile_slot_index(tile_index: u16, tiles_per_atlas: u32) -> Option<usize> {
    let index = tile_index as usize;
    if index >= (tiles_per_atlas as usize) {
        return None;
    }
    Some(index)
}

pub(in crate::atlas) fn tile_coords_from_index_with_row(
    tile_index: u16,
    tiles_per_row: u32,
) -> (u32, u32) {
    let tile_index: u32 = tile_index.into();
    (tile_index % tiles_per_row, tile_index / tiles_per_row)
}

#[cfg(test)]
mod tests {
    use super::{AtlasLayout, TileAtlasCpu};
    use crate::{TILE_STRIDE, TileAllocError};

    fn test_layout(tiles_per_row: u32, tiles_per_column: u32) -> AtlasLayout {
        let tiles_per_atlas = tiles_per_row
            .checked_mul(tiles_per_column)
            .expect("tiles per atlas overflow");
        AtlasLayout {
            tiles_per_row,
            tiles_per_column,
            atlas_width: tiles_per_row
                .checked_mul(TILE_STRIDE)
                .expect("atlas width overflow"),
            atlas_height: tiles_per_column
                .checked_mul(TILE_STRIDE)
                .expect("atlas height overflow"),
            tiles_per_atlas,
            atlas_occupancy_words: (tiles_per_atlas as usize).div_ceil(64),
        }
    }

    #[test]
    fn allocate_evicts_oldest_retain_batch_when_full() {
        let cpu = TileAtlasCpu::new(1, test_layout(3, 1)).expect("create cpu atlas");
        let (key_a, _) = cpu.allocate_without_ops().expect("allocate key a");
        let (key_b, _) = cpu.allocate_without_ops().expect("allocate key b");
        let (key_c, _) = cpu.allocate_without_ops().expect("allocate key c");

        cpu.retain_keys_new_batch(&[key_a, key_b]);
        cpu.retain_keys_new_batch(&[key_c]);

        let (new_key, _) = cpu
            .allocate_without_ops()
            .expect("allocate with gc eviction");

        assert!(!cpu.is_allocated(key_a));
        assert!(!cpu.is_allocated(key_b));
        assert!(cpu.is_allocated(key_c));
        assert!(cpu.is_allocated(new_key));
    }

    #[test]
    fn allocate_fails_when_full_and_no_retain_batches_exist() {
        let cpu = TileAtlasCpu::new(1, test_layout(2, 1)).expect("create cpu atlas");
        let (key_a, _) = cpu.allocate_without_ops().expect("allocate key a");
        let (key_b, _) = cpu.allocate_without_ops().expect("allocate key b");
        cpu.mark_keys_active(&[key_a, key_b]);

        let error = cpu
            .allocate_without_ops()
            .expect_err("allocate should fail without retained keys");
        assert!(matches!(error, TileAllocError::AtlasFull));
        assert!(cpu.is_allocated(key_a));
        assert!(cpu.is_allocated(key_b));
    }

    #[test]
    fn retain_batch_eviction_uses_batch_insertion_order() {
        let cpu = TileAtlasCpu::new(1, test_layout(2, 1)).expect("create cpu atlas");
        let (oldest_key, _) = cpu.allocate_without_ops().expect("allocate oldest key");
        let (newer_key, _) = cpu.allocate_without_ops().expect("allocate newer key");

        cpu.retain_keys_new_batch(&[oldest_key]);
        cpu.retain_keys_new_batch(&[newer_key]);

        let (_replacement_key, _) = cpu
            .allocate_without_ops()
            .expect("allocate with retain eviction");

        assert!(!cpu.is_allocated(oldest_key));
        assert!(cpu.is_allocated(newer_key));
    }

    #[test]
    fn drain_evicted_batches_reports_gc_evictions_and_is_idempotent() {
        let cpu = TileAtlasCpu::new(1, test_layout(2, 1)).expect("create cpu atlas");
        let (key_a, _) = cpu.allocate_without_ops().expect("allocate key a");
        let (key_b, _) = cpu.allocate_without_ops().expect("allocate key b");
        let retain_id = cpu.retain_keys_new_batch(&[key_a, key_b]);

        let _ = cpu
            .allocate_without_ops()
            .expect("allocate with gc retain eviction");

        let first = cpu.drain_evicted_retain_batches();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].retain_id, retain_id);
        assert_eq!(first[0].keys.len(), 2);
        assert!(first[0].keys.contains(&key_a));
        assert!(first[0].keys.contains(&key_b));

        let second = cpu.drain_evicted_retain_batches();
        assert!(second.is_empty());
    }

    #[test]
    fn retain_reuses_key_moves_to_new_batch() {
        let cpu = TileAtlasCpu::new(1, test_layout(2, 1)).expect("create cpu atlas");
        let (key_a, _) = cpu.allocate_without_ops().expect("allocate key a");
        let (key_b, _) = cpu.allocate_without_ops().expect("allocate key b");

        cpu.retain_keys_new_batch(&[key_a]);
        let retain_id = cpu.retain_keys_new_batch(&[key_a]);

        let (key_c, _) = cpu
            .allocate_without_ops()
            .expect("allocate with gc eviction");

        assert!(!cpu.is_allocated(key_a));
        assert!(cpu.is_allocated(key_b));
        assert!(cpu.is_allocated(key_c));

        let first = cpu.drain_evicted_retain_batches();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].retain_id, retain_id);
        assert_eq!(first[0].keys, vec![key_a]);
    }
}
