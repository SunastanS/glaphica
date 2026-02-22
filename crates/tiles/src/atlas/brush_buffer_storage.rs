use std::marker::PhantomData;
use std::sync::Arc;

use crate::{
    TileAddress, TileAllocError, TileAtlasCreateError, TileAtlasLayout, TileGpuDrainError,
    TileIngestError, TileKey, TileSetError, TileSetHandle,
};

use super::core;
pub use super::core::EvictedRetainBatch;
use super::format::{
    Bgra8Spec, Bgra8SrgbSpec, R8UintSpec, R32FloatSpec, Rgba8Spec, Rgba8SrgbSpec, TileFormatSpec,
    TileGpuCreateValidator, TileGpuOpAdapter, TilePayloadSpec, TileUploadFormatSpec,
};
use super::gpu;
use super::{GenericTileAtlasConfig, TileAtlasFormat, TileAtlasUsage, TilePayloadKind};

#[derive(Debug, Clone, Copy)]
pub struct RuntimeGenericTileAtlasConfig {
    pub max_layers: u32,
    pub tiles_per_row: u32,
    pub tiles_per_column: u32,
    pub format: TileAtlasFormat,
    pub usage: TileAtlasUsage,
    pub payload_kind: TilePayloadKind,
}

impl Default for RuntimeGenericTileAtlasConfig {
    fn default() -> Self {
        Self {
            max_layers: GenericTileAtlasConfig::default().max_layers,
            tiles_per_row: GenericTileAtlasConfig::default().tiles_per_row,
            tiles_per_column: GenericTileAtlasConfig::default().tiles_per_column,
            format: Rgba8Spec::FORMAT,
            usage: GenericTileAtlasConfig::default().usage,
            payload_kind: TilePayloadKind::Rgba8,
        }
    }
}

impl From<RuntimeGenericTileAtlasConfig> for GenericTileAtlasConfig {
    fn from(value: RuntimeGenericTileAtlasConfig) -> Self {
        Self {
            max_layers: value.max_layers,
            tiles_per_row: value.tiles_per_row,
            tiles_per_column: value.tiles_per_column,
            usage: value.usage,
        }
    }
}

#[derive(Debug)]
pub enum RuntimeGenericTileAtlasStore {
    Rgba8Unorm(GenericTileAtlasStore<Rgba8Spec>),
    Rgba8UnormSrgb(GenericTileAtlasStore<Rgba8SrgbSpec>),
    Bgra8Unorm(GenericTileAtlasStore<Bgra8Spec>),
    Bgra8UnormSrgb(GenericTileAtlasStore<Bgra8SrgbSpec>),
    R32Float(GenericTileAtlasStore<R32FloatSpec>),
    R8Uint(GenericTileAtlasStore<R8UintSpec>),
}

#[derive(Debug)]
pub enum RuntimeGenericTileAtlasGpuArray {
    Rgba8Unorm(gpu::GenericTileAtlasGpuArray<Rgba8Spec>),
    Rgba8UnormSrgb(gpu::GenericTileAtlasGpuArray<Rgba8SrgbSpec>),
    Bgra8Unorm(gpu::GenericTileAtlasGpuArray<Bgra8Spec>),
    Bgra8UnormSrgb(gpu::GenericTileAtlasGpuArray<Bgra8SrgbSpec>),
    R32Float(gpu::GenericTileAtlasGpuArray<R32FloatSpec>),
    R8Uint(gpu::GenericTileAtlasGpuArray<R8UintSpec>),
}

macro_rules! dispatch_runtime_store {
    ($self:expr, $store:ident => $expr:expr) => {
        match $self {
            RuntimeGenericTileAtlasStore::Rgba8Unorm($store) => $expr,
            RuntimeGenericTileAtlasStore::Rgba8UnormSrgb($store) => $expr,
            RuntimeGenericTileAtlasStore::Bgra8Unorm($store) => $expr,
            RuntimeGenericTileAtlasStore::Bgra8UnormSrgb($store) => $expr,
            RuntimeGenericTileAtlasStore::R32Float($store) => $expr,
            RuntimeGenericTileAtlasStore::R8Uint($store) => $expr,
        }
    };
}

macro_rules! dispatch_runtime_gpu {
    ($self:expr, $gpu:ident => $expr:expr) => {
        match $self {
            RuntimeGenericTileAtlasGpuArray::Rgba8Unorm($gpu) => $expr,
            RuntimeGenericTileAtlasGpuArray::Rgba8UnormSrgb($gpu) => $expr,
            RuntimeGenericTileAtlasGpuArray::Bgra8Unorm($gpu) => $expr,
            RuntimeGenericTileAtlasGpuArray::Bgra8UnormSrgb($gpu) => $expr,
            RuntimeGenericTileAtlasGpuArray::R32Float($gpu) => $expr,
            RuntimeGenericTileAtlasGpuArray::R8Uint($gpu) => $expr,
        }
    };
}

#[derive(Debug)]
pub struct GenericTileAtlasStore<F: TilePayloadSpec = Rgba8Spec> {
    cpu: Arc<core::TileAtlasCpu>,
    op_sender: core::TileOpSender<F::UploadPayload>,
    usage: core::AtlasUsage,
    _format: PhantomData<F>,
}

pub type GenericTileAtlasGpuArray<F = Rgba8Spec> = gpu::GenericTileAtlasGpuArray<F>;

impl RuntimeGenericTileAtlasStore {
    pub fn with_config(
        device: &wgpu::Device,
        config: RuntimeGenericTileAtlasConfig,
    ) -> Result<(Self, RuntimeGenericTileAtlasGpuArray), TileAtlasCreateError> {
        match (config.payload_kind, config.format) {
            (TilePayloadKind::Rgba8, TileAtlasFormat::Rgba8Unorm) => {
                let (store, gpu) = GenericTileAtlasStore::<Rgba8Spec>::with_config(
                    device,
                    GenericTileAtlasConfig::from(config),
                )?;
                Ok((
                    RuntimeGenericTileAtlasStore::Rgba8Unorm(store),
                    RuntimeGenericTileAtlasGpuArray::Rgba8Unorm(gpu),
                ))
            }
            (TilePayloadKind::Rgba8, TileAtlasFormat::Rgba8UnormSrgb) => {
                let (store, gpu) = GenericTileAtlasStore::<Rgba8SrgbSpec>::with_config(
                    device,
                    GenericTileAtlasConfig::from(config),
                )?;
                Ok((
                    RuntimeGenericTileAtlasStore::Rgba8UnormSrgb(store),
                    RuntimeGenericTileAtlasGpuArray::Rgba8UnormSrgb(gpu),
                ))
            }
            (TilePayloadKind::Rgba8, TileAtlasFormat::Bgra8Unorm) => {
                let (store, gpu) = GenericTileAtlasStore::<Bgra8Spec>::with_config(
                    device,
                    GenericTileAtlasConfig::from(config),
                )?;
                Ok((
                    RuntimeGenericTileAtlasStore::Bgra8Unorm(store),
                    RuntimeGenericTileAtlasGpuArray::Bgra8Unorm(gpu),
                ))
            }
            (TilePayloadKind::Rgba8, TileAtlasFormat::Bgra8UnormSrgb) => {
                let (store, gpu) = GenericTileAtlasStore::<Bgra8SrgbSpec>::with_config(
                    device,
                    GenericTileAtlasConfig::from(config),
                )?;
                Ok((
                    RuntimeGenericTileAtlasStore::Bgra8UnormSrgb(store),
                    RuntimeGenericTileAtlasGpuArray::Bgra8UnormSrgb(gpu),
                ))
            }
            (TilePayloadKind::R32Float, TileAtlasFormat::R32Float) => {
                let (store, gpu) = GenericTileAtlasStore::<R32FloatSpec>::with_config(
                    device,
                    GenericTileAtlasConfig::from(config),
                )?;
                Ok((
                    RuntimeGenericTileAtlasStore::R32Float(store),
                    RuntimeGenericTileAtlasGpuArray::R32Float(gpu),
                ))
            }
            (TilePayloadKind::R8Uint, TileAtlasFormat::R8Uint) => {
                let (store, gpu) = GenericTileAtlasStore::<R8UintSpec>::with_config(
                    device,
                    GenericTileAtlasConfig::from(config),
                )?;
                Ok((
                    RuntimeGenericTileAtlasStore::R8Uint(store),
                    RuntimeGenericTileAtlasGpuArray::R8Uint(gpu),
                ))
            }
            _ => Err(TileAtlasCreateError::UnsupportedPayloadFormat),
        }
    }

    pub fn is_allocated(&self, key: TileKey) -> bool {
        dispatch_runtime_store!(self, store => store.is_allocated(key))
    }

    pub fn resolve(&self, key: TileKey) -> Option<TileAddress> {
        dispatch_runtime_store!(self, store => store.resolve(key))
    }

    pub fn allocate(&self) -> Result<TileKey, TileAllocError> {
        dispatch_runtime_store!(self, store => store.allocate())
    }

    pub fn release(&self, key: TileKey) -> bool {
        dispatch_runtime_store!(self, store => store.release(key))
    }

    pub fn force_release_all_keys(&self) -> usize {
        dispatch_runtime_store!(self, store => store.force_release_all_keys())
    }

    pub fn mark_keys_active(&self, keys: &[TileKey]) {
        dispatch_runtime_store!(self, store => store.mark_keys_active(keys))
    }

    pub fn retain_keys_new_batch(&self, keys: &[TileKey]) -> u64 {
        dispatch_runtime_store!(self, store => store.retain_keys_new_batch(keys))
    }

    pub fn drain_evicted_retain_batches(&self) -> Vec<EvictedRetainBatch> {
        dispatch_runtime_store!(self, store => store.drain_evicted_retain_batches())
    }

    pub fn clear(&self, key: TileKey) -> Result<bool, TileAllocError> {
        dispatch_runtime_store!(self, store => store.clear(key))
    }

    pub fn reserve_tile_set(&self, count: u32) -> Result<TileSetHandle, TileSetError> {
        dispatch_runtime_store!(self, store => store.reserve_tile_set(count))
    }

    pub fn adopt_tile_set(
        &self,
        keys: impl IntoIterator<Item = TileKey>,
    ) -> Result<TileSetHandle, TileSetError> {
        let keys = keys.into_iter().collect::<Vec<_>>();
        dispatch_runtime_store!(self, store => store.adopt_tile_set(keys.iter().copied()))
    }

    pub fn release_tile_set(&self, set: TileSetHandle) -> Result<u32, TileSetError> {
        dispatch_runtime_store!(self, store => store.release_tile_set(set))
    }

    pub fn clear_tile_set(&self, set: &TileSetHandle) -> Result<u32, TileSetError> {
        dispatch_runtime_store!(self, store => store.clear_tile_set(set))
    }

    pub fn resolve_tile_set(
        &self,
        set: &TileSetHandle,
    ) -> Result<Vec<(TileKey, TileAddress)>, TileSetError> {
        dispatch_runtime_store!(self, store => store.resolve_tile_set(set))
    }
}

impl RuntimeGenericTileAtlasGpuArray {
    pub fn view(&self) -> &wgpu::TextureView {
        dispatch_runtime_gpu!(self, gpu => gpu.view())
    }

    pub fn texture(&self) -> &wgpu::Texture {
        dispatch_runtime_gpu!(self, gpu => gpu.texture())
    }

    pub fn layout(&self) -> TileAtlasLayout {
        dispatch_runtime_gpu!(self, gpu => gpu.layout())
    }

    pub fn drain_and_execute(&self, queue: &wgpu::Queue) -> Result<usize, TileGpuDrainError> {
        dispatch_runtime_gpu!(self, gpu => gpu.drain_and_execute(queue))
    }
}

impl<F: TileFormatSpec + TileGpuCreateValidator + TileGpuOpAdapter> GenericTileAtlasStore<F> {
    pub fn new(
        device: &wgpu::Device,
        usage: TileAtlasUsage,
    ) -> Result<(Self, GenericTileAtlasGpuArray<F>), TileAtlasCreateError> {
        Self::with_config(
            device,
            GenericTileAtlasConfig {
                max_layers: GenericTileAtlasConfig::default().max_layers,
                tiles_per_row: GenericTileAtlasConfig::default().tiles_per_row,
                tiles_per_column: GenericTileAtlasConfig::default().tiles_per_column,
                usage,
            },
        )
    }

    pub fn with_config(
        device: &wgpu::Device,
        config: GenericTileAtlasConfig,
    ) -> Result<(Self, GenericTileAtlasGpuArray<F>), TileAtlasCreateError> {
        gpu::validate_generic_atlas_config(device, config)?;
        let layout = core::AtlasLayout::from_config(gpu::core_config_from_generic(config))?;
        F::validate_gpu_create(device, config.usage)?;

        let (op_sender, op_queue) = core::TileOpQueue::new();
        let cpu = Arc::new(
            core::TileAtlasCpu::new(config.max_layers, layout)
                .map_err(|_| TileAtlasCreateError::MaxLayersExceedsDeviceLimit)?,
        );
        let atlas_usage = gpu::core_usage_from_public(config.usage);

        let (texture, view) = gpu::create_atlas_texture_and_array_view(
            device,
            layout,
            config.max_layers,
            gpu::atlas_format_to_wgpu(F::FORMAT),
            gpu::atlas_usage_to_wgpu(config.usage),
            "tiles.atlas.array",
            "tiles.atlas.array.view",
        );

        Ok((
            Self {
                cpu: Arc::clone(&cpu),
                op_sender,
                usage: atlas_usage,
                _format: PhantomData,
            },
            gpu::GenericTileAtlasGpuArray::new(
                Arc::clone(&cpu),
                texture,
                view,
                op_queue,
                atlas_usage,
                layout,
            ),
        ))
    }
}

impl<F: TilePayloadSpec> GenericTileAtlasStore<F> {
    pub fn is_allocated(&self, key: TileKey) -> bool {
        self.cpu.is_allocated(key)
    }

    pub fn resolve(&self, key: TileKey) -> Option<TileAddress> {
        self.cpu.resolve(key)
    }

    pub fn allocate(&self) -> Result<TileKey, TileAllocError> {
        let (key, _address) = self.cpu.allocate(&self.op_sender)?;
        Ok(key)
    }

    pub fn release(&self, key: TileKey) -> bool {
        self.cpu.release(key)
    }

    pub fn force_release_all_keys(&self) -> usize {
        self.cpu.release_all()
    }

    pub fn mark_keys_active(&self, keys: &[TileKey]) {
        self.cpu.mark_keys_active(keys);
    }

    pub fn retain_keys_new_batch(&self, keys: &[TileKey]) -> u64 {
        self.cpu.retain_keys_new_batch(keys)
    }

    pub fn drain_evicted_retain_batches(&self) -> Vec<EvictedRetainBatch> {
        self.cpu.drain_evicted_retain_batches()
    }

    pub fn clear(&self, key: TileKey) -> Result<bool, TileAllocError> {
        let Some(target) = self.cpu.resolve_op_target(key) else {
            return Ok(false);
        };
        self.op_sender.send(core::TileOp::Clear { target })?;
        Ok(true)
    }

    pub fn reserve_tile_set(&self, count: u32) -> Result<TileSetHandle, TileSetError> {
        core::reserve_tile_set_with(
            &self.cpu,
            count,
            || self.allocate(),
            |key| self.cpu.release(key),
        )
    }

    pub fn adopt_tile_set(
        &self,
        keys: impl IntoIterator<Item = TileKey>,
    ) -> Result<TileSetHandle, TileSetError> {
        core::adopt_tile_set(&self.cpu, keys)
    }

    pub fn release_tile_set(&self, set: TileSetHandle) -> Result<u32, TileSetError> {
        core::release_tile_set(&self.cpu, set)
    }

    pub fn clear_tile_set(&self, set: &TileSetHandle) -> Result<u32, TileSetError> {
        core::validate_tile_set_ownership(&self.cpu, set)?;
        let mut targets = Vec::with_capacity(set.len());
        for key in set.keys() {
            let Some(target) = self.cpu.resolve_op_target(*key) else {
                return Err(TileSetError::UnknownTileKey);
            };
            targets.push(target);
        }

        if targets.is_empty() {
            return Ok(0);
        }

        let cleared_count =
            u32::try_from(targets.len()).map_err(|_| TileSetError::KeySpaceExhausted)?;
        self.op_sender.send(core::TileOp::ClearBatch { targets })?;
        Ok(cleared_count)
    }

    pub fn resolve_tile_set(
        &self,
        set: &TileSetHandle,
    ) -> Result<Vec<(TileKey, TileAddress)>, TileSetError> {
        core::resolve_tile_set(&self.cpu, set)
    }

    pub(in crate::atlas) fn usage(&self) -> core::AtlasUsage {
        self.usage
    }
}

impl<F: TileUploadFormatSpec> GenericTileAtlasStore<F> {
    pub(in crate::atlas) fn enqueue_upload_bytes(
        &self,
        bytes: Arc<[u8]>,
    ) -> Result<TileKey, TileIngestError> {
        F::validate_upload_bytes(&bytes)?;
        let (key, address) = self.cpu.allocate(&self.op_sender)?;
        let Some(target) = self.cpu.resolve_op_target(key) else {
            panic!("allocated tile key must resolve to op target");
        };
        if let Err(error) = self.op_sender.send(core::TileOp::Upload {
            target,
            payload: bytes,
        }) {
            self.cpu.rollback_allocate(key, address, true);
            return Err(error.into());
        }
        Ok(key)
    }
}

pub type GenericR32FloatTileAtlasStore = GenericTileAtlasStore<super::format::R32FloatSpec>;
pub type GenericR32FloatTileAtlasGpuArray = GenericTileAtlasGpuArray<super::format::R32FloatSpec>;
pub type GenericR8UintTileAtlasStore = GenericTileAtlasStore<super::format::R8UintSpec>;
pub type GenericR8UintTileAtlasGpuArray = GenericTileAtlasGpuArray<super::format::R8UintSpec>;
