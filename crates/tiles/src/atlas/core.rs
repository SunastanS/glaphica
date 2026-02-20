use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{mpsc, Mutex};

use super::GenericTileAtlasConfig;
use crate::{
    TileAddress, TileAllocError, TileAtlasCreateError, TileKey, TileSetError, TileSetHandle,
    TileSetId,
};
use crate::{INDEX_SHARDS, TILE_STRIDE};
#[cfg(test)]
use crate::{TILES_PER_ROW, TILE_GUTTER};

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
        config: GenericTileAtlasConfig,
    ) -> Result<Self, TileAtlasCreateError> {
        if config.tiles_per_row == 0 || config.tiles_per_column == 0 {
            return Err(TileAtlasCreateError::AtlasTileGridZero);
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
    next_key: AtomicU64,
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
            next_key: AtomicU64::new(0),
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

        let mut released_records = Vec::with_capacity(keys.len());
        for key in keys {
            let shard_id = self.shard_for_key(*key);
            let (_, shard) = shard_locks
                .iter_mut()
                .find(|(id, _)| *id == shard_id)
                .expect("target shard lock must exist");
            let record = shard
                .remove(key)
                .expect("tile key must exist after atomic precheck");
            released_records.push(record);
        }

        drop(shard_locks);

        for record in &released_records {
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
        for _ in &released_records {
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

    fn take_free_address(&self) -> Result<TileAddress, TileAllocError> {
        let start = self.next_layer_hint.fetch_add(1, Ordering::Relaxed) % self.max_layers;
        for offset in 0..self.max_layers {
            let layer = (start + offset) % self.max_layers;
            let page = self
                .pages
                .get(layer as usize)
                .ok_or(TileAllocError::AtlasFull)?;
            if let Some(tile_index) = page.pop_free() {
                return Ok(TileAddress {
                    atlas_layer: layer,
                    tile_index,
                });
            }
        }
        Err(TileAllocError::AtlasFull)
    }
}

pub(in crate::atlas) fn validate_generic_atlas_config(
    device: &wgpu::Device,
    config: GenericTileAtlasConfig,
) -> Result<(), TileAtlasCreateError> {
    if config.max_layers == 0 {
        return Err(TileAtlasCreateError::MaxLayersZero);
    }
    let layout = AtlasLayout::from_config(config)?;

    let limits = device.limits();
    if config.max_layers > limits.max_texture_array_layers {
        return Err(TileAtlasCreateError::MaxLayersExceedsDeviceLimit);
    }
    if layout.atlas_width > limits.max_texture_dimension_2d
        || layout.atlas_height > limits.max_texture_dimension_2d
    {
        return Err(TileAtlasCreateError::AtlasSizeExceedsDeviceLimit);
    }

    Ok(())
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

pub(in crate::atlas) fn create_atlas_texture_and_array_view(
    device: &wgpu::Device,
    layout: AtlasLayout,
    max_layers: u32,
    format: wgpu::TextureFormat,
    usage: wgpu::TextureUsages,
    texture_label: &'static str,
    view_label: &'static str,
) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(texture_label),
        size: wgpu::Extent3d {
            width: layout.atlas_width,
            height: layout.atlas_height,
            depth_or_array_layers: max_layers,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor {
        label: Some(view_label),
        format: Some(format),
        dimension: Some(wgpu::TextureViewDimension::D2Array),
        usage: None,
        aspect: wgpu::TextureAspect::All,
        base_mip_level: 0,
        mip_level_count: Some(1),
        base_array_layer: 0,
        array_layer_count: Some(max_layers),
    });

    (texture, view)
}

pub(in crate::atlas) fn supports_texture_usage_for_format(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    usage: wgpu::TextureUsages,
) -> bool {
    let error_scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let _probe_texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("tiles.format_usage_probe"),
        size: wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage,
        view_formats: &[],
    });
    pollster::block_on(error_scope.pop()).is_none()
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
pub(crate) fn tile_origin(address: TileAddress) -> wgpu::Origin3d {
    let slot_origin = tile_slot_origin_with_row(address, TILES_PER_ROW);
    wgpu::Origin3d {
        x: slot_origin.x + TILE_GUTTER,
        y: slot_origin.y + TILE_GUTTER,
        z: slot_origin.z,
    }
}

pub(in crate::atlas) fn tile_slot_origin_with_row(
    address: TileAddress,
    tiles_per_row: u32,
) -> wgpu::Origin3d {
    let (tile_x, tile_y) = tile_coords_from_index_with_row(address.tile_index, tiles_per_row);
    wgpu::Origin3d {
        x: tile_x * TILE_STRIDE,
        y: tile_y * TILE_STRIDE,
        z: address.atlas_layer,
    }
}
