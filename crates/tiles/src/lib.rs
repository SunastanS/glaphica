use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, mpsc};

pub const TILE_SIZE: u32 = 256;
pub const TILE_GUTTER: u32 = 1;
pub const TILE_STRIDE: u32 = TILE_SIZE + TILE_GUTTER * 2;
pub const DEFAULT_MAX_LAYERS: u32 = 4;
pub const TILES_PER_ROW: u32 = 32;
pub const ATLAS_SIZE: u32 = TILES_PER_ROW * TILE_STRIDE;
pub const TILES_PER_ATLAS: u32 = TILES_PER_ROW * TILES_PER_ROW;
pub const ATLAS_OCCUPANCY_WORDS: usize = (TILES_PER_ATLAS as usize).div_ceil(64);

const INDEX_SHARDS: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TileKey(u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TileAddress {
    pub atlas_layer: u32,
    pub tile_index: u16,
}

impl TileAddress {
    fn tile_coords(self) -> (u32, u32) {
        tile_coords_from_index(self.tile_index)
    }

    pub fn tile_x(self) -> u32 {
        let (tile_x, _) = self.tile_coords();
        tile_x
    }

    pub fn tile_y(self) -> u32 {
        let (_, tile_y) = self.tile_coords();
        tile_y
    }

    pub fn atlas_uv_origin(self) -> (f32, f32) {
        let inv_atlas_size = 1.0 / (ATLAS_SIZE as f32);
        let origin = tile_content_origin(self);
        (
            origin.x as f32 * inv_atlas_size,
            origin.y as f32 * inv_atlas_size,
        )
    }

    pub fn atlas_slot_origin_pixels(self) -> (u32, u32) {
        let origin = tile_slot_origin(self);
        (origin.x, origin.y)
    }

    pub fn atlas_content_origin_pixels(self) -> (u32, u32) {
        let origin = tile_content_origin(self);
        (origin.x, origin.y)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TileAllocError {
    KeySpaceExhausted,
    AtlasFull,
    QueueDisconnected,
}

impl fmt::Display for TileAllocError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TileAllocError::KeySpaceExhausted => write!(formatter, "tile key space exhausted"),
            TileAllocError::AtlasFull => write!(formatter, "tile atlas has no free slots"),
            TileAllocError::QueueDisconnected => {
                write!(formatter, "tile operation queue disconnected")
            }
        }
    }
}

impl std::error::Error for TileAllocError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TileIngestError {
    SizeMismatch,
    UnsupportedFormat,
    MissingCopyDstUsage,
    SizeOverflow,
    StrideTooSmall,
    BufferLengthMismatch,
    BufferTooShort,
    Alloc(TileAllocError),
}

impl From<TileAllocError> for TileIngestError {
    fn from(value: TileAllocError) -> Self {
        Self::Alloc(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageIngestError {
    SizeOverflow,
    StrideTooSmall,
    BufferTooShort,
    RectOutOfBounds,
    NonTileAligned,
    Virtual(VirtualImageError),
    Tile(TileIngestError),
}

impl From<VirtualImageError> for ImageIngestError {
    fn from(value: VirtualImageError) -> Self {
        Self::Virtual(value)
    }
}

impl From<TileIngestError> for ImageIngestError {
    fn from(value: TileIngestError) -> Self {
        Self::Tile(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TileAtlasCreateError {
    MissingCopyDstUsage,
    MissingTextureBindingUsage,
    MaxLayersZero,
    MaxLayersExceedsDeviceLimit,
    AtlasSizeExceedsDeviceLimit,
}

impl fmt::Display for TileAtlasCreateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TileAtlasCreateError::MissingCopyDstUsage => {
                write!(formatter, "tile atlas usage must include COPY_DST")
            }
            TileAtlasCreateError::MissingTextureBindingUsage => {
                write!(formatter, "tile atlas usage must include TEXTURE_BINDING")
            }
            TileAtlasCreateError::MaxLayersZero => {
                write!(formatter, "tile atlas max_layers must be at least 1")
            }
            TileAtlasCreateError::MaxLayersExceedsDeviceLimit => {
                write!(formatter, "tile atlas max_layers exceeds device limit")
            }
            TileAtlasCreateError::AtlasSizeExceedsDeviceLimit => {
                write!(formatter, "tile atlas size exceeds device limit")
            }
        }
    }
}

impl std::error::Error for TileAtlasCreateError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TileGpuDrainError {
    UnsupportedFormat,
    MissingCopyDstUsage,
    UploadLengthMismatch,
}

impl fmt::Display for TileGpuDrainError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TileGpuDrainError::UnsupportedFormat => {
                write!(
                    formatter,
                    "tile atlas gpu executor supports only rgba8 formats"
                )
            }
            TileGpuDrainError::MissingCopyDstUsage => {
                write!(formatter, "tile atlas usage must include COPY_DST")
            }
            TileGpuDrainError::UploadLengthMismatch => {
                write!(formatter, "tile upload bytes length mismatch")
            }
        }
    }
}

impl std::error::Error for TileGpuDrainError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TileRgba8ContractError {
    UnsupportedFormat,
    MissingCopyDstUsage,
}

fn rgba8_tile_len() -> usize {
    (TILE_SIZE as usize) * (TILE_SIZE as usize) * 4
}

fn rgba8_tile_slot_len() -> usize {
    (TILE_STRIDE as usize) * (TILE_STRIDE as usize) * 4
}

fn validate_rgba8_copy_dst_contract(
    format: wgpu::TextureFormat,
    usage: wgpu::TextureUsages,
) -> Result<(), TileRgba8ContractError> {
    if !usage.contains(wgpu::TextureUsages::COPY_DST) {
        return Err(TileRgba8ContractError::MissingCopyDstUsage);
    }
    match format {
        wgpu::TextureFormat::Rgba8Unorm | wgpu::TextureFormat::Rgba8UnormSrgb => Ok(()),
        _ => Err(TileRgba8ContractError::UnsupportedFormat),
    }
}

fn map_contract_error_to_tile_ingest(error: TileRgba8ContractError) -> TileIngestError {
    match error {
        TileRgba8ContractError::UnsupportedFormat => TileIngestError::UnsupportedFormat,
        TileRgba8ContractError::MissingCopyDstUsage => TileIngestError::MissingCopyDstUsage,
    }
}

fn map_contract_error_to_gpu_drain(error: TileRgba8ContractError) -> TileGpuDrainError {
    match error {
        TileRgba8ContractError::UnsupportedFormat => TileGpuDrainError::UnsupportedFormat,
        TileRgba8ContractError::MissingCopyDstUsage => TileGpuDrainError::MissingCopyDstUsage,
    }
}

#[derive(Debug, Clone, Copy)]
pub struct TileAtlasConfig {
    pub max_layers: u32,
    pub format: wgpu::TextureFormat,
    pub usage: wgpu::TextureUsages,
}

impl Default for TileAtlasConfig {
    fn default() -> Self {
        Self {
            max_layers: DEFAULT_MAX_LAYERS,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::COPY_SRC,
        }
    }
}

#[derive(Debug, Clone)]
enum TileOp {
    Clear {
        address: TileAddress,
    },
    UploadRgba8 {
        address: TileAddress,
        bytes: Arc<[u8]>,
    },
}

#[derive(Clone, Debug)]
struct TileOpSender {
    sender: mpsc::Sender<TileOp>,
}

impl TileOpSender {
    fn send(&self, operation: TileOp) -> Result<(), TileAllocError> {
        self.sender
            .send(operation)
            .map_err(|_| TileAllocError::QueueDisconnected)
    }
}

#[derive(Debug)]
struct TileOpQueue {
    receiver: Mutex<mpsc::Receiver<TileOp>>,
}

impl TileOpQueue {
    fn new() -> (TileOpSender, Self) {
        let (sender, receiver) = mpsc::channel();
        (
            TileOpSender { sender },
            Self {
                receiver: Mutex::new(receiver),
            },
        )
    }

    fn drain(&self) -> Vec<TileOp> {
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
    dirty_bits: Mutex<[u64; ATLAS_OCCUPANCY_WORDS]>,
}

impl TileAllocatorPage {
    fn new() -> Result<Self, TileAllocError> {
        let mut free_tiles = Vec::new();
        for tile_index in (0..TILES_PER_ATLAS).rev() {
            let tile_index_u16: u16 = tile_index
                .try_into()
                .map_err(|_| TileAllocError::AtlasFull)?;
            free_tiles.push(tile_index_u16);
        }
        Ok(Self {
            free_tiles: Mutex::new(free_tiles),
            dirty_bits: Mutex::new([0; ATLAS_OCCUPANCY_WORDS]),
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
        let (word, mask) = tile_bit(tile_index).ok_or(TileAllocError::AtlasFull)?;
        let mut dirty_bits = self
            .dirty_bits
            .lock()
            .expect("tile allocator dirty bits lock poisoned");
        dirty_bits[word] |= mask;
        Ok(())
    }

    fn take_dirty(&self, tile_index: u16) -> Result<bool, TileAllocError> {
        let (word, mask) = tile_bit(tile_index).ok_or(TileAllocError::AtlasFull)?;
        let mut dirty_bits = self
            .dirty_bits
            .lock()
            .expect("tile allocator dirty bits lock poisoned");
        let was_dirty = (dirty_bits[word] & mask) != 0;
        dirty_bits[word] &= !mask;
        Ok(was_dirty)
    }
}

#[derive(Debug)]
struct TileAtlasCpu {
    pages: Vec<TileAllocatorPage>,
    index_shards: [Mutex<HashMap<TileKey, TileAddress>>; INDEX_SHARDS],
    next_key: AtomicU64,
    next_layer_hint: AtomicU32,
    max_layers: u32,
}

impl TileAtlasCpu {
    fn new(max_layers: u32) -> Result<Self, TileAllocError> {
        let mut pages = Vec::new();
        for _ in 0..max_layers {
            pages.push(TileAllocatorPage::new()?);
        }

        Ok(Self {
            pages,
            index_shards: std::array::from_fn(|_| Mutex::new(HashMap::new())),
            next_key: AtomicU64::new(0),
            next_layer_hint: AtomicU32::new(0),
            max_layers,
        })
    }

    fn is_allocated(&self, key: TileKey) -> bool {
        let shard = self.shard_for_key(key);
        self.index_shards[shard]
            .lock()
            .expect("tile index shard lock poisoned")
            .contains_key(&key)
    }

    fn resolve(&self, key: TileKey) -> Option<TileAddress> {
        let shard = self.shard_for_key(key);
        self.index_shards[shard]
            .lock()
            .expect("tile index shard lock poisoned")
            .get(&key)
            .copied()
    }

    fn allocate_raw(&self) -> Result<(TileKey, TileAddress, bool), TileAllocError> {
        let key = self.next_key()?;
        let address = self.take_free_address()?;

        let shard = self.shard_for_key(key);
        self.index_shards[shard]
            .lock()
            .expect("tile index shard lock poisoned")
            .insert(key, address);

        let page = self
            .pages
            .get(address.atlas_layer as usize)
            .ok_or(TileAllocError::AtlasFull)?;
        let was_dirty = page.take_dirty(address.tile_index)?;

        Ok((key, address, was_dirty))
    }

    fn allocate(&self, op_sender: &TileOpSender) -> Result<(TileKey, TileAddress), TileAllocError> {
        let (key, address, was_dirty) = self.allocate_raw()?;
        if was_dirty {
            if op_sender.send(TileOp::Clear { address }).is_err() {
                self.rollback_allocate(key, address, true);
                return Err(TileAllocError::QueueDisconnected);
            }
        }

        Ok((key, address))
    }

    fn allocate_without_ops(&self) -> Result<(TileKey, TileAddress), TileAllocError> {
        let (key, address, _was_dirty) = self.allocate_raw()?;
        Ok((key, address))
    }

    fn release(&self, key: TileKey) -> bool {
        let shard = self.shard_for_key(key);
        let address = {
            let mut index = self.index_shards[shard]
                .lock()
                .expect("tile index shard lock poisoned");
            index.remove(&key)
        };

        let Some(address) = address else {
            return false;
        };
        let page = self
            .pages
            .get(address.atlas_layer as usize)
            .expect("tile address layer must be valid");
        page.mark_dirty(address.tile_index)
            .expect("tile index must be in range");
        page.push_free(address.tile_index);
        true
    }

    fn rollback_allocate(&self, key: TileKey, address: TileAddress, mark_dirty: bool) {
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
        page.push_free(address.tile_index);
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

fn validate_atlas_config(
    device: &wgpu::Device,
    config: TileAtlasConfig,
) -> Result<(), TileAtlasCreateError> {
    if !config.usage.contains(wgpu::TextureUsages::COPY_DST) {
        return Err(TileAtlasCreateError::MissingCopyDstUsage);
    }
    if !config.usage.contains(wgpu::TextureUsages::TEXTURE_BINDING) {
        return Err(TileAtlasCreateError::MissingTextureBindingUsage);
    }
    if config.max_layers == 0 {
        return Err(TileAtlasCreateError::MaxLayersZero);
    }

    let limits = device.limits();
    if config.max_layers > limits.max_texture_array_layers {
        return Err(TileAtlasCreateError::MaxLayersExceedsDeviceLimit);
    }
    if ATLAS_SIZE > limits.max_texture_dimension_2d {
        return Err(TileAtlasCreateError::AtlasSizeExceedsDeviceLimit);
    }

    Ok(())
}

fn create_atlas_texture_and_array_view(
    device: &wgpu::Device,
    config: TileAtlasConfig,
    texture_label: &'static str,
    view_label: &'static str,
) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(texture_label),
        size: wgpu::Extent3d {
            width: ATLAS_SIZE,
            height: ATLAS_SIZE,
            depth_or_array_layers: config.max_layers,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: config.format,
        usage: config.usage,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor {
        label: Some(view_label),
        format: Some(config.format),
        dimension: Some(wgpu::TextureViewDimension::D2Array),
        usage: None,
        aspect: wgpu::TextureAspect::All,
        base_mip_level: 0,
        mip_level_count: Some(1),
        base_array_layer: 0,
        array_layer_count: Some(config.max_layers),
    });

    (texture, view)
}

#[derive(Debug)]
pub struct TileAtlasStore {
    cpu: Arc<TileAtlasCpu>,
    op_sender: TileOpSender,
    format: wgpu::TextureFormat,
    usage: wgpu::TextureUsages,
}

#[derive(Debug)]
pub struct TileAtlasGpuArray {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    op_queue: TileOpQueue,
    format: wgpu::TextureFormat,
    usage: wgpu::TextureUsages,
}

#[derive(Debug)]
pub struct GroupTileAtlasStore {
    cpu: Arc<TileAtlasCpu>,
}

#[derive(Debug)]
pub struct GroupTileAtlasGpuArray {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    format: wgpu::TextureFormat,
    max_layers: u32,
}

impl TileAtlasStore {
    pub fn new(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        usage: wgpu::TextureUsages,
    ) -> Result<(Self, TileAtlasGpuArray), TileAtlasCreateError> {
        Self::with_config(
            device,
            TileAtlasConfig {
                format,
                usage,
                ..TileAtlasConfig::default()
            },
        )
    }

    pub fn with_config(
        device: &wgpu::Device,
        config: TileAtlasConfig,
    ) -> Result<(Self, TileAtlasGpuArray), TileAtlasCreateError> {
        validate_atlas_config(device, config)?;

        let (op_sender, op_queue) = TileOpQueue::new();
        let cpu = Arc::new(
            TileAtlasCpu::new(config.max_layers)
                .map_err(|_| TileAtlasCreateError::MaxLayersExceedsDeviceLimit)?,
        );

        let (texture, view) = create_atlas_texture_and_array_view(
            device,
            config,
            "tiles.atlas.array",
            "tiles.atlas.array.view",
        );

        Ok((
            Self {
                cpu,
                op_sender,
                format: config.format,
                usage: config.usage,
            },
            TileAtlasGpuArray {
                texture,
                view,
                op_queue,
                format: config.format,
                usage: config.usage,
            },
        ))
    }

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

    fn validate_ingest_contract(&self) -> Result<(), TileIngestError> {
        validate_rgba8_copy_dst_contract(self.format, self.usage)
            .map_err(map_contract_error_to_tile_ingest)
    }

    fn enqueue_packed_rgba8_tile(&self, bytes: Vec<u8>) -> Result<TileKey, TileIngestError> {
        if bytes.len() != rgba8_tile_len() {
            return Err(TileIngestError::BufferLengthMismatch);
        }

        let (key, address) = self.cpu.allocate(&self.op_sender)?;
        let bytes = Arc::<[u8]>::from(bytes);
        if let Err(error) = self.op_sender.send(TileOp::UploadRgba8 { address, bytes }) {
            self.cpu.rollback_allocate(key, address, true);
            return Err(error.into());
        }

        Ok(key)
    }

    pub fn ingest_tile(
        &self,
        width: u32,
        height: u32,
        bytes: &[u8],
    ) -> Result<Option<TileKey>, TileIngestError> {
        if width != TILE_SIZE || height != TILE_SIZE {
            return Err(TileIngestError::SizeMismatch);
        }
        if bytes.is_empty() {
            return Ok(None);
        }
        self.validate_ingest_contract()?;

        if bytes.len() != rgba8_tile_len() {
            return Err(TileIngestError::BufferLengthMismatch);
        }
        if bytes.iter().all(|&byte| byte == 0) {
            return Ok(None);
        }

        self.enqueue_packed_rgba8_tile(bytes.to_vec()).map(Some)
    }

    pub fn ingest_tile_rgba8_strided(
        &self,
        width: u32,
        height: u32,
        bytes: &[u8],
        bytes_per_row: u32,
    ) -> Result<Option<TileKey>, TileIngestError> {
        if width != TILE_SIZE || height != TILE_SIZE {
            return Err(TileIngestError::SizeMismatch);
        }
        if bytes.is_empty() {
            return Ok(None);
        }
        self.validate_ingest_contract()?;

        let bytes_per_pixel = 4u32;
        let row_bytes = width
            .checked_mul(bytes_per_pixel)
            .ok_or(TileIngestError::SizeOverflow)?;
        if bytes_per_row < row_bytes {
            return Err(TileIngestError::StrideTooSmall);
        }
        let bytes_per_row_usize: usize = bytes_per_row
            .try_into()
            .map_err(|_| TileIngestError::SizeOverflow)?;
        let row_bytes_usize: usize = row_bytes
            .try_into()
            .map_err(|_| TileIngestError::SizeOverflow)?;
        let prefix_len = bytes_per_row_usize
            .checked_mul((height as usize).saturating_sub(1))
            .ok_or(TileIngestError::SizeOverflow)?;
        let required_len = prefix_len
            .checked_add(row_bytes_usize)
            .ok_or(TileIngestError::SizeOverflow)?;
        let Some(data) = bytes.get(..required_len) else {
            return Err(TileIngestError::BufferTooShort);
        };

        let tile_is_empty = (0..height as usize).all(|row| {
            let start = row * bytes_per_row_usize;
            let end = start + row_bytes_usize;
            data.get(start..end)
                .is_some_and(|slice| slice.iter().all(|&byte| byte == 0))
        });
        if tile_is_empty {
            return Ok(None);
        }

        let mut packed = vec![0u8; (TILE_SIZE as usize) * (TILE_SIZE as usize) * 4];
        for row in 0..(TILE_SIZE as usize) {
            let src_start = row * bytes_per_row_usize;
            let src_end = src_start + row_bytes_usize;
            let source = data
                .get(src_start..src_end)
                .ok_or(TileIngestError::BufferTooShort)?;
            let dst_start = row * row_bytes_usize;
            let dst_end = dst_start + row_bytes_usize;
            packed[dst_start..dst_end].copy_from_slice(source);
        }

        self.enqueue_packed_rgba8_tile(packed).map(Some)
    }

    pub fn ingest_image_rgba8_strided(
        &self,
        size_x: u32,
        size_y: u32,
        bytes: &[u8],
        bytes_per_row: u32,
    ) -> Result<VirtualImage<TileKey>, ImageIngestError> {
        let _layout = rgba8_strided_layout(size_x, size_y, bytes, bytes_per_row)?;
        let mut image = VirtualImage::new(size_x, size_y)?;

        for tile_y in 0..image.tiles_per_column() {
            for tile_x in 0..image.tiles_per_row() {
                let source_x = tile_x
                    .checked_mul(TILE_SIZE)
                    .ok_or(ImageIngestError::SizeOverflow)?;
                let source_y = tile_y
                    .checked_mul(TILE_SIZE)
                    .ok_or(ImageIngestError::SizeOverflow)?;

                let rect_width = TILE_SIZE.min(size_x.saturating_sub(source_x));
                let rect_height = TILE_SIZE.min(size_y.saturating_sub(source_y));
                if rect_width == 0 || rect_height == 0 {
                    continue;
                }

                let Some(key) = self.ingest_subrect_rgba8_as_full_tile(
                    size_x,
                    size_y,
                    bytes,
                    bytes_per_row,
                    source_x,
                    source_y,
                    rect_width,
                    rect_height,
                )?
                else {
                    continue;
                };
                image.set_tile(tile_x, tile_y, key)?;
            }
        }

        Ok(image)
    }

    fn ingest_subrect_rgba8_as_full_tile(
        &self,
        source_size_x: u32,
        source_size_y: u32,
        source_bytes: &[u8],
        source_bytes_per_row: u32,
        source_x: u32,
        source_y: u32,
        rect_width: u32,
        rect_height: u32,
    ) -> Result<Option<TileKey>, ImageIngestError> {
        let _layout = rgba8_strided_layout(
            source_size_x,
            source_size_y,
            source_bytes,
            source_bytes_per_row,
        )?;
        self.validate_ingest_contract()
            .map_err(ImageIngestError::from)?;

        if rect_width == 0 || rect_height == 0 {
            return Ok(None);
        }
        if rect_width > TILE_SIZE || rect_height > TILE_SIZE {
            return Err(ImageIngestError::NonTileAligned);
        }

        let all_zero = rgba8_rect_all_zero_strided(
            source_size_x,
            source_size_y,
            source_bytes,
            source_bytes_per_row,
            source_x,
            source_y,
            rect_width,
            rect_height,
        )?;
        if all_zero {
            return Ok(None);
        }

        let source_bytes_per_row_usize: usize = source_bytes_per_row
            .try_into()
            .map_err(|_| ImageIngestError::SizeOverflow)?;
        let rect_row_bytes: usize = rect_width
            .checked_mul(4)
            .ok_or(ImageIngestError::SizeOverflow)?
            .try_into()
            .map_err(|_| ImageIngestError::SizeOverflow)?;
        let source_x_bytes: usize = source_x
            .checked_mul(4)
            .ok_or(ImageIngestError::SizeOverflow)?
            .try_into()
            .map_err(|_| ImageIngestError::SizeOverflow)?;

        let mut packed = vec![0u8; (TILE_SIZE as usize) * (TILE_SIZE as usize) * 4];
        let packed_row_bytes = (TILE_SIZE as usize) * 4;
        for row in 0..(rect_height as usize) {
            let source_row = (source_y as usize)
                .checked_add(row)
                .ok_or(ImageIngestError::SizeOverflow)?;
            let source_row_start = source_row
                .checked_mul(source_bytes_per_row_usize)
                .ok_or(ImageIngestError::SizeOverflow)?;
            let source_start = source_row_start
                .checked_add(source_x_bytes)
                .ok_or(ImageIngestError::SizeOverflow)?;
            let source_end = source_start
                .checked_add(rect_row_bytes)
                .ok_or(ImageIngestError::SizeOverflow)?;
            let source_slice = source_bytes
                .get(source_start..source_end)
                .ok_or(ImageIngestError::BufferTooShort)?;

            let packed_start = row
                .checked_mul(packed_row_bytes)
                .ok_or(ImageIngestError::SizeOverflow)?;
            let packed_end = packed_start
                .checked_add(rect_row_bytes)
                .ok_or(ImageIngestError::SizeOverflow)?;
            packed[packed_start..packed_end].copy_from_slice(source_slice);
        }

        self.enqueue_packed_rgba8_tile(packed)
            .map(Some)
            .map_err(ImageIngestError::from)
    }
}

impl GroupTileAtlasStore {
    pub fn new(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        usage: wgpu::TextureUsages,
    ) -> Result<(Self, GroupTileAtlasGpuArray), TileAtlasCreateError> {
        Self::with_config(
            device,
            TileAtlasConfig {
                max_layers: 2,
                format,
                usage,
            },
        )
    }

    pub fn with_config(
        device: &wgpu::Device,
        config: TileAtlasConfig,
    ) -> Result<(Self, GroupTileAtlasGpuArray), TileAtlasCreateError> {
        validate_atlas_config(device, config)?;

        let cpu = Arc::new(
            TileAtlasCpu::new(config.max_layers)
                .map_err(|_| TileAtlasCreateError::MaxLayersExceedsDeviceLimit)?,
        );
        let (texture, view) = create_atlas_texture_and_array_view(
            device,
            config,
            "tiles.group_atlas.array",
            "tiles.group_atlas.array.view",
        );

        Ok((
            Self { cpu },
            GroupTileAtlasGpuArray {
                texture,
                view,
                format: config.format,
                max_layers: config.max_layers,
            },
        ))
    }

    pub fn is_allocated(&self, key: TileKey) -> bool {
        self.cpu.is_allocated(key)
    }

    pub fn resolve(&self, key: TileKey) -> Option<TileAddress> {
        self.cpu.resolve(key)
    }

    pub fn allocate(&self) -> Result<TileKey, TileAllocError> {
        let (key, _address) = self.cpu.allocate_without_ops()?;
        Ok(key)
    }

    pub fn release(&self, key: TileKey) -> bool {
        self.cpu.release(key)
    }
}

impl TileAtlasGpuArray {
    pub fn view(&self) -> &wgpu::TextureView {
        &self.view
    }

    pub fn texture(&self) -> &wgpu::Texture {
        &self.texture
    }

    pub fn drain_and_execute(&self, queue: &wgpu::Queue) -> Result<usize, TileGpuDrainError> {
        validate_rgba8_copy_dst_contract(self.format, self.usage)
            .map_err(map_contract_error_to_gpu_drain)?;

        let operations = self.op_queue.drain();
        for operation in &operations {
            match operation {
                TileOp::Clear { address } => {
                    queue.write_texture(
                        wgpu::TexelCopyTextureInfo {
                            texture: &self.texture,
                            mip_level: 0,
                            origin: tile_slot_origin(*address),
                            aspect: wgpu::TextureAspect::All,
                        },
                        zero_tile_slot_rgba8(),
                        wgpu::TexelCopyBufferLayout {
                            offset: 0,
                            bytes_per_row: Some(TILE_STRIDE * 4),
                            rows_per_image: Some(TILE_STRIDE),
                        },
                        wgpu::Extent3d {
                            width: TILE_STRIDE,
                            height: TILE_STRIDE,
                            depth_or_array_layers: 1,
                        },
                    );
                }
                TileOp::UploadRgba8 { address, bytes } => {
                    if bytes.len() != rgba8_tile_len() {
                        return Err(TileGpuDrainError::UploadLengthMismatch);
                    }
                    let expanded = expand_tile_rgba8_with_gutter(bytes);
                    queue.write_texture(
                        wgpu::TexelCopyTextureInfo {
                            texture: &self.texture,
                            mip_level: 0,
                            origin: tile_slot_origin(*address),
                            aspect: wgpu::TextureAspect::All,
                        },
                        &expanded,
                        wgpu::TexelCopyBufferLayout {
                            offset: 0,
                            bytes_per_row: Some(TILE_STRIDE * 4),
                            rows_per_image: Some(TILE_STRIDE),
                        },
                        wgpu::Extent3d {
                            width: TILE_STRIDE,
                            height: TILE_STRIDE,
                            depth_or_array_layers: 1,
                        },
                    );
                }
            }
        }

        Ok(operations.len())
    }
}

impl GroupTileAtlasGpuArray {
    pub fn view(&self) -> &wgpu::TextureView {
        &self.view
    }

    pub fn texture(&self) -> &wgpu::Texture {
        &self.texture
    }

    pub fn layer_view(&self, layer: u32) -> wgpu::TextureView {
        assert!(
            layer < self.max_layers,
            "group atlas layer out of range: {layer} >= {}",
            self.max_layers
        );
        self.texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("tiles.group_atlas.layer.view"),
            format: Some(self.format),
            dimension: Some(wgpu::TextureViewDimension::D2),
            usage: None,
            aspect: wgpu::TextureAspect::All,
            base_mip_level: 0,
            mip_level_count: Some(1),
            base_array_layer: layer,
            array_layer_count: Some(1),
        })
    }
}

fn tile_bit(tile_index: u16) -> Option<(usize, u64)> {
    let index = tile_index as usize;
    if index >= (TILES_PER_ATLAS as usize) {
        return None;
    }
    let word = index / 64;
    if word >= ATLAS_OCCUPANCY_WORDS {
        return None;
    }
    let mask = 1u64 << (index % 64);
    Some((word, mask))
}

fn tile_coords_from_index(tile_index: u16) -> (u32, u32) {
    let tile_index: u32 = tile_index.into();
    (tile_index % TILES_PER_ROW, tile_index / TILES_PER_ROW)
}

#[cfg(test)]
fn tile_origin(address: TileAddress) -> wgpu::Origin3d {
    tile_content_origin(address)
}

fn tile_slot_origin(address: TileAddress) -> wgpu::Origin3d {
    let (tile_x, tile_y) = address.tile_coords();
    wgpu::Origin3d {
        x: tile_x * TILE_STRIDE,
        y: tile_y * TILE_STRIDE,
        z: address.atlas_layer,
    }
}

fn tile_content_origin(address: TileAddress) -> wgpu::Origin3d {
    let slot_origin = tile_slot_origin(address);
    wgpu::Origin3d {
        x: slot_origin.x + TILE_GUTTER,
        y: slot_origin.y + TILE_GUTTER,
        z: slot_origin.z,
    }
}

fn expand_tile_rgba8_with_gutter(content_bytes: &[u8]) -> Vec<u8> {
    if content_bytes.len() != rgba8_tile_len() {
        panic!(
            "tile content bytes length mismatch: expected {}, got {}",
            rgba8_tile_len(),
            content_bytes.len()
        );
    }

    let stride = TILE_STRIDE as usize;
    let gutter = TILE_GUTTER as usize;
    let content = TILE_SIZE as usize;
    let row_bytes = content * 4;
    let mut expanded = vec![0u8; rgba8_tile_slot_len()];

    for row in 0..content {
        let source_row_start = row * row_bytes;
        let source_row_end = source_row_start + row_bytes;
        let destination_row = row + gutter;
        let destination_row_start = (destination_row * stride + gutter) * 4;
        let destination_row_end = destination_row_start + row_bytes;
        expanded[destination_row_start..destination_row_end]
            .copy_from_slice(&content_bytes[source_row_start..source_row_end]);
    }

    for row in 0..content {
        let destination_row = row + gutter;
        let row_base = destination_row * stride;
        let content_start = row_base + gutter;
        let content_end = content_start + content - 1;
        for column in 0..gutter {
            let left_source_texel = {
                let left_source = content_start * 4;
                [
                    expanded[left_source],
                    expanded[left_source + 1],
                    expanded[left_source + 2],
                    expanded[left_source + 3],
                ]
            };
            let left_index = (row_base + column) * 4;
            expanded[left_index..left_index + 4].copy_from_slice(&left_source_texel);

            let right_source_texel = {
                let right_source = content_end * 4;
                [
                    expanded[right_source],
                    expanded[right_source + 1],
                    expanded[right_source + 2],
                    expanded[right_source + 3],
                ]
            };
            let right_index = (row_base + content + gutter + column) * 4;
            expanded[right_index..right_index + 4].copy_from_slice(&right_source_texel);
        }
    }

    let top_content_row = gutter;
    let bottom_content_row = gutter + content - 1;
    for row in 0..gutter {
        let top_row_base = row * stride;
        let top_source_base = top_content_row * stride;
        let top_source_row = expanded[top_source_base * 4..(top_source_base + stride) * 4].to_vec();
        expanded[top_row_base * 4..(top_row_base + stride) * 4].copy_from_slice(&top_source_row);

        let bottom_row_base = (gutter + content + row) * stride;
        let bottom_source_base = bottom_content_row * stride;
        let bottom_source_row =
            expanded[bottom_source_base * 4..(bottom_source_base + stride) * 4].to_vec();
        expanded[bottom_row_base * 4..(bottom_row_base + stride) * 4]
            .copy_from_slice(&bottom_source_row);
    }

    expanded
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Rgba8StridedLayout {
    row_bytes: u32,
    required_len: usize,
}

fn rgba8_strided_layout(
    size_x: u32,
    size_y: u32,
    bytes: &[u8],
    bytes_per_row: u32,
) -> Result<Rgba8StridedLayout, ImageIngestError> {
    if size_x == 0 || size_y == 0 {
        return Ok(Rgba8StridedLayout {
            row_bytes: 0,
            required_len: 0,
        });
    }

    let row_bytes = size_x
        .checked_mul(4)
        .ok_or(ImageIngestError::SizeOverflow)?;
    if bytes_per_row < row_bytes {
        return Err(ImageIngestError::StrideTooSmall);
    }

    let bytes_per_row_usize: usize = bytes_per_row
        .try_into()
        .map_err(|_| ImageIngestError::SizeOverflow)?;
    let row_bytes_usize: usize = row_bytes
        .try_into()
        .map_err(|_| ImageIngestError::SizeOverflow)?;
    let prefix_len = bytes_per_row_usize
        .checked_mul((size_y as usize).saturating_sub(1))
        .ok_or(ImageIngestError::SizeOverflow)?;
    let required_len = prefix_len
        .checked_add(row_bytes_usize)
        .ok_or(ImageIngestError::SizeOverflow)?;
    if bytes.len() < required_len {
        return Err(ImageIngestError::BufferTooShort);
    }

    Ok(Rgba8StridedLayout {
        row_bytes,
        required_len,
    })
}

fn rgba8_rect_all_zero_strided(
    size_x: u32,
    size_y: u32,
    bytes: &[u8],
    bytes_per_row: u32,
    rect_x: u32,
    rect_y: u32,
    rect_width: u32,
    rect_height: u32,
) -> Result<bool, ImageIngestError> {
    let _layout = rgba8_strided_layout(size_x, size_y, bytes, bytes_per_row)?;
    if rect_width == 0 || rect_height == 0 {
        return Ok(true);
    }

    let rect_x1 = rect_x
        .checked_add(rect_width)
        .ok_or(ImageIngestError::SizeOverflow)?;
    let rect_y1 = rect_y
        .checked_add(rect_height)
        .ok_or(ImageIngestError::SizeOverflow)?;
    if rect_x1 > size_x || rect_y1 > size_y {
        return Err(ImageIngestError::RectOutOfBounds);
    }

    let rect_row_bytes = rect_width
        .checked_mul(4)
        .ok_or(ImageIngestError::SizeOverflow)? as usize;
    let bytes_per_row_usize: usize = bytes_per_row
        .try_into()
        .map_err(|_| ImageIngestError::SizeOverflow)?;
    let rect_x_bytes: usize = rect_x
        .checked_mul(4)
        .ok_or(ImageIngestError::SizeOverflow)? as usize;

    for row in 0..rect_height {
        let y = rect_y
            .checked_add(row)
            .ok_or(ImageIngestError::SizeOverflow)?;
        let row_start = (y as usize)
            .checked_mul(bytes_per_row_usize)
            .ok_or(ImageIngestError::SizeOverflow)?;
        let start = row_start
            .checked_add(rect_x_bytes)
            .ok_or(ImageIngestError::SizeOverflow)?;
        let end = start
            .checked_add(rect_row_bytes)
            .ok_or(ImageIngestError::SizeOverflow)?;
        let slice = bytes
            .get(start..end)
            .ok_or(ImageIngestError::BufferTooShort)?;
        if slice.iter().any(|&byte| byte != 0) {
            return Ok(false);
        }
    }

    Ok(true)
}

fn zero_tile_slot_rgba8() -> &'static [u8] {
    static ZERO: OnceLock<Vec<u8>> = OnceLock::new();
    ZERO.get_or_init(|| vec![0u8; rgba8_tile_slot_len()])
}

#[derive(Debug, Clone, Default)]
pub struct VirtualImage<K> {
    size_x: u32,
    size_y: u32,
    tiles_per_row: u32,
    tiles_per_column: u32,
    tiles: Box<[Option<K>]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirtualImageError {
    TileCountOverflow,
    TileBytesLengthMismatch,
    TileIndexOutOfBounds,
    OutputByteCountOverflow,
}

impl<K> VirtualImage<K> {
    fn tile_index(&self, tile_x: u32, tile_y: u32) -> Result<usize, VirtualImageError> {
        if tile_x >= self.tiles_per_row || tile_y >= self.tiles_per_column {
            return Err(VirtualImageError::TileIndexOutOfBounds);
        }

        let row = (tile_y as usize)
            .checked_mul(self.tiles_per_row as usize)
            .ok_or(VirtualImageError::TileIndexOutOfBounds)?;
        row.checked_add(tile_x as usize)
            .ok_or(VirtualImageError::TileIndexOutOfBounds)
    }

    pub fn new(size_x: u32, size_y: u32) -> Result<Self, VirtualImageError> {
        let tiles_per_row = size_x.div_ceil(TILE_SIZE);
        let tiles_per_column = size_y.div_ceil(TILE_SIZE);
        let tile_count = (tiles_per_row as usize)
            .checked_mul(tiles_per_column as usize)
            .ok_or(VirtualImageError::TileCountOverflow)?;

        Ok(Self {
            size_x,
            size_y,
            tiles_per_row,
            tiles_per_column,
            tiles: {
                let mut tiles = Vec::new();
                tiles.resize_with(tile_count, || None);
                tiles.into_boxed_slice()
            },
        })
    }

    pub fn size_x(&self) -> u32 {
        self.size_x
    }

    pub fn size_y(&self) -> u32 {
        self.size_y
    }

    pub fn tiles_per_row(&self) -> u32 {
        self.tiles_per_row
    }

    pub fn tiles_per_column(&self) -> u32 {
        self.tiles_per_column
    }

    pub fn get_tile(&self, tile_x: u32, tile_y: u32) -> Result<Option<&K>, VirtualImageError> {
        let index = self.tile_index(tile_x, tile_y)?;
        Ok(self.tiles.get(index).and_then(|value| value.as_ref()))
    }

    pub fn set_tile(&mut self, tile_x: u32, tile_y: u32, key: K) -> Result<(), VirtualImageError> {
        let index = self.tile_index(tile_x, tile_y)?;
        let slot = self
            .tiles
            .get_mut(index)
            .ok_or(VirtualImageError::TileIndexOutOfBounds)?;
        *slot = Some(key);
        Ok(())
    }

    pub fn iter_tiles(&self) -> impl Iterator<Item = (u32, u32, &K)> + '_ {
        self.tiles
            .iter()
            .enumerate()
            .filter_map(move |(index, slot)| {
                let key = slot.as_ref()?;
                let tiles_per_row = self.tiles_per_row as usize;
                let tile_x = (index % tiles_per_row) as u32;
                let tile_y = (index / tiles_per_row) as u32;
                Some((tile_x, tile_y, key))
            })
    }

    pub fn export_rgba8(
        &self,
        mut load_tile: impl FnMut(&K) -> Option<Vec<u8>>,
    ) -> Result<Vec<u8>, VirtualImageError> {
        let size_x_usize: usize = self
            .size_x
            .try_into()
            .map_err(|_| VirtualImageError::OutputByteCountOverflow)?;
        let size_y_usize: usize = self
            .size_y
            .try_into()
            .map_err(|_| VirtualImageError::OutputByteCountOverflow)?;
        let out_len = size_x_usize
            .checked_mul(size_y_usize)
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or(VirtualImageError::OutputByteCountOverflow)?;
        let mut out = vec![0u8; out_len];
        let tile_row_bytes = (TILE_SIZE as usize) * 4;
        let expected_tile_len = (TILE_SIZE as usize) * tile_row_bytes;

        for tile_y in 0..self.tiles_per_column {
            for tile_x in 0..self.tiles_per_row {
                let index = (tile_y as usize) * (self.tiles_per_row as usize) + (tile_x as usize);
                let Some(key) = self.tiles.get(index).and_then(|value| value.as_ref()) else {
                    continue;
                };
                let Some(tile) = load_tile(key) else {
                    continue;
                };
                if tile.len() != expected_tile_len {
                    return Err(VirtualImageError::TileBytesLengthMismatch);
                }

                let dst_x0 = (tile_x * TILE_SIZE) as usize;
                let dst_y0 = (tile_y * TILE_SIZE) as usize;
                for row in 0..(TILE_SIZE as usize) {
                    let dst_y = dst_y0 + row;
                    if dst_y >= self.size_y as usize {
                        break;
                    }
                    let copy_pixels =
                        (TILE_SIZE as usize).min((self.size_x as usize).saturating_sub(dst_x0));
                    let copy_bytes = copy_pixels * 4;
                    let src = &tile[(row * tile_row_bytes)..(row * tile_row_bytes + copy_bytes)];
                    let dst_offset = (dst_y * (self.size_x as usize) + dst_x0) * 4;
                    out[dst_offset..(dst_offset + copy_bytes)].copy_from_slice(src);
                }
            }
        }

        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_device_queue() -> (wgpu::Device, wgpu::Queue) {
        pollster::block_on(async {
            let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
                backends: wgpu::Backends::all(),
                ..Default::default()
            });
            let adapter = instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::HighPerformance,
                    compatible_surface: None,
                    force_fallback_adapter: false,
                })
                .await
                .expect("request wgpu adapter");
            let limits = adapter.limits();
            adapter
                .request_device(&wgpu::DeviceDescriptor {
                    label: Some("tiles tests"),
                    required_features: wgpu::Features::empty(),
                    required_limits: limits,
                    experimental_features: wgpu::ExperimentalFeatures::disabled(),
                    memory_hints: wgpu::MemoryHints::Performance,
                    trace: wgpu::Trace::Off,
                })
                .await
                .expect("request wgpu device")
        })
    }

    fn create_store(
        device: &wgpu::Device,
        config: TileAtlasConfig,
    ) -> (TileAtlasStore, TileAtlasGpuArray) {
        TileAtlasStore::with_config(device, config).expect("TileAtlasStore::with_config")
    }

    fn read_tile_rgba8(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        texture: &wgpu::Texture,
        address: TileAddress,
    ) -> Vec<u8> {
        let buffer_size = (TILE_SIZE as u64) * (TILE_SIZE as u64) * 4;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("tile readback"),
            size: buffer_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("tile readback"),
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: tile_origin(address),
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(TILE_SIZE * 4),
                    rows_per_image: Some(TILE_SIZE),
                },
            },
            wgpu::Extent3d {
                width: TILE_SIZE,
                height: TILE_SIZE,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(Some(encoder.finish()));

        let slice = buffer.slice(..);
        let (sender, receiver) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            sender.send(result).expect("map callback send");
        });
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("device poll");
        receiver
            .recv()
            .expect("map callback recv")
            .expect("map tile readback");
        let tile = slice.get_mapped_range().to_vec();
        buffer.unmap();
        tile
    }

    fn read_texel_rgba8(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        texture: &wgpu::Texture,
        x: u32,
        y: u32,
        z: u32,
    ) -> [u8; 4] {
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("tile texel readback"),
            size: 256,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("tile texel readback"),
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x, y, z },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(256),
                    rows_per_image: Some(1),
                },
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(Some(encoder.finish()));

        let slice = buffer.slice(..);
        let (sender, receiver) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            sender.send(result).expect("map callback send");
        });
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("device poll");
        receiver
            .recv()
            .expect("map callback recv")
            .expect("map texel readback");
        let mapped = slice.get_mapped_range();
        let texel = [mapped[0], mapped[1], mapped[2], mapped[3]];
        drop(mapped);
        buffer.unmap();
        texel
    }

    fn source_texel(bytes: &[u8], x: u32, y: u32) -> [u8; 4] {
        let index = ((y as usize) * (TILE_SIZE as usize) + (x as usize)) * 4;
        [
            bytes[index],
            bytes[index + 1],
            bytes[index + 2],
            bytes[index + 3],
        ]
    }

    #[test]
    fn config_default_max_layers_is_four() {
        let config = TileAtlasConfig::default();
        assert_eq!(config.max_layers, 4);
    }

    #[test]
    fn atlas_size_should_match_tile_stride_and_capacity_contract() {
        let tile_stride = TILE_STRIDE;
        assert_eq!(
            ATLAS_SIZE,
            TILES_PER_ROW * tile_stride,
            "atlas should preserve tiles-per-row capacity when adding 1px gutter"
        );
    }

    #[test]
    fn ingest_tile_should_define_gutter_pixels_from_edge_texels() {
        let (device, queue) = create_device_queue();
        let (store, gpu) = create_store(
            &device,
            TileAtlasConfig {
                max_layers: 1,
                ..TileAtlasConfig::default()
            },
        );

        let mut bytes = vec![0u8; (TILE_SIZE as usize) * (TILE_SIZE as usize) * 4];
        for y in 0..TILE_SIZE {
            for x in 0..TILE_SIZE {
                let index = ((y as usize) * (TILE_SIZE as usize) + (x as usize)) * 4;
                bytes[index] = (x % 251) as u8;
                bytes[index + 1] = (y % 251) as u8;
                bytes[index + 2] = ((x + y) % 251) as u8;
                bytes[index + 3] = 255;
            }
        }

        let key = store
            .ingest_tile(TILE_SIZE, TILE_SIZE, &bytes)
            .expect("ingest tile")
            .expect("non-empty tile");
        let address = store.resolve(key).expect("resolve key");
        assert_eq!(gpu.drain_and_execute(&queue).expect("drain upload"), 1);

        let tile_stride = TILE_STRIDE;
        let atlas_tile_origin_x = address.tile_x() * tile_stride;
        let atlas_tile_origin_y = address.tile_y() * tile_stride;

        let source_top_left = source_texel(&bytes, 0, 0);
        let source_top_right = source_texel(&bytes, TILE_SIZE - 1, 0);
        let source_bottom_left = source_texel(&bytes, 0, TILE_SIZE - 1);
        let source_bottom_right = source_texel(&bytes, TILE_SIZE - 1, TILE_SIZE - 1);

        assert_eq!(
            read_texel_rgba8(
                &device,
                &queue,
                gpu.texture(),
                atlas_tile_origin_x,
                atlas_tile_origin_y,
                address.atlas_layer,
            ),
            source_top_left,
            "top-left gutter texel should match top-left source texel"
        );
        assert_eq!(
            read_texel_rgba8(
                &device,
                &queue,
                gpu.texture(),
                atlas_tile_origin_x + tile_stride - 1,
                atlas_tile_origin_y,
                address.atlas_layer,
            ),
            source_top_right,
            "top-right gutter texel should match top-right source texel"
        );
        assert_eq!(
            read_texel_rgba8(
                &device,
                &queue,
                gpu.texture(),
                atlas_tile_origin_x,
                atlas_tile_origin_y + tile_stride - 1,
                address.atlas_layer,
            ),
            source_bottom_left,
            "bottom-left gutter texel should match bottom-left source texel"
        );
        assert_eq!(
            read_texel_rgba8(
                &device,
                &queue,
                gpu.texture(),
                atlas_tile_origin_x + tile_stride - 1,
                atlas_tile_origin_y + tile_stride - 1,
                address.atlas_layer,
            ),
            source_bottom_right,
            "bottom-right gutter texel should match bottom-right source texel"
        );
        assert_eq!(
            read_texel_rgba8(
                &device,
                &queue,
                gpu.texture(),
                atlas_tile_origin_x + 1,
                atlas_tile_origin_y + 1,
                address.atlas_layer,
            ),
            source_top_left,
            "content origin should be shifted by 1px due to gutter"
        );
    }

    #[test]
    fn release_is_cpu_only_and_dirty_triggers_clear_on_reuse() {
        let (device, queue) = create_device_queue();
        let (store, gpu) = create_store(
            &device,
            TileAtlasConfig {
                max_layers: 1,
                usage: wgpu::TextureUsages::COPY_DST
                    | wgpu::TextureUsages::COPY_SRC
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                ..TileAtlasConfig::default()
            },
        );

        let key0 = store.allocate().expect("allocate key0");
        let address0 = store.resolve(key0).expect("key0 address");
        assert_eq!(gpu.drain_and_execute(&queue).expect("drain after key0"), 0);

        assert!(store.release(key0));
        assert_eq!(
            gpu.drain_and_execute(&queue).expect("drain after release"),
            0
        );

        let key1 = store.allocate().expect("allocate key1");
        let address1 = store.resolve(key1).expect("key1 address");
        assert_eq!(address1, address0);
        assert_eq!(gpu.drain_and_execute(&queue).expect("drain clear"), 1);

        let tile = read_tile_rgba8(&device, &queue, gpu.texture(), address1);
        assert!(tile.iter().all(|&byte| byte == 0));
    }

    #[test]
    fn ingest_tile_enqueues_upload_and_writes_after_drain() {
        let (device, queue) = create_device_queue();
        let (store, gpu) = create_store(&device, TileAtlasConfig::default());

        let mut bytes = vec![0u8; (TILE_SIZE as usize) * (TILE_SIZE as usize) * 4];
        bytes[0] = 9;
        bytes[1] = 8;
        bytes[2] = 7;
        bytes[3] = 6;
        let key = store
            .ingest_tile(TILE_SIZE, TILE_SIZE, &bytes)
            .expect("ingest tile")
            .expect("non-empty tile");
        let address = store.resolve(key).expect("resolve key");

        assert_eq!(gpu.drain_and_execute(&queue).expect("drain upload"), 1);
        let tile = read_tile_rgba8(&device, &queue, gpu.texture(), address);
        assert_eq!(&tile[..4], &[9, 8, 7, 6]);
    }

    #[test]
    fn ingest_image_rgba8_strided_keeps_sparse_tiles() {
        let (device, queue) = create_device_queue();
        let (store, gpu) = create_store(&device, TileAtlasConfig::default());

        let size_x = TILE_SIZE + 1;
        let size_y = TILE_SIZE + 1;
        let row_bytes = size_x * 4;
        let bytes_per_row = row_bytes + 8;
        let required_len =
            (bytes_per_row as usize) * ((size_y as usize).saturating_sub(1)) + (row_bytes as usize);
        let mut bytes = vec![0u8; required_len];
        let pixel_x = TILE_SIZE;
        let pixel_y = TILE_SIZE;
        let index = (pixel_y as usize) * (bytes_per_row as usize) + (pixel_x as usize) * 4;
        bytes[index] = 1;

        let image = store
            .ingest_image_rgba8_strided(size_x, size_y, &bytes, bytes_per_row)
            .expect("ingest image");
        assert_eq!(image.tiles_per_row(), 2);
        assert_eq!(image.tiles_per_column(), 2);
        assert_eq!(image.get_tile(0, 0), Ok(None));
        assert_eq!(image.get_tile(1, 0), Ok(None));
        assert_eq!(image.get_tile(0, 1), Ok(None));
        assert!(image.get_tile(1, 1).expect("get tile").copied().is_some());

        assert_eq!(gpu.drain_and_execute(&queue).expect("drain"), 1);
    }

    #[test]
    fn max_layers_limits_total_capacity() {
        let (device, _queue) = create_device_queue();
        let (store, _gpu) = create_store(
            &device,
            TileAtlasConfig {
                max_layers: 1,
                ..TileAtlasConfig::default()
            },
        );

        for _ in 0..TILES_PER_ATLAS {
            store.allocate().expect("allocate within layer capacity");
        }
        assert_eq!(store.allocate(), Err(TileAllocError::AtlasFull));
    }

    #[test]
    fn tile_address_helpers_match_tile_origin_math() {
        let address = TileAddress {
            atlas_layer: 2,
            tile_index: (TILES_PER_ROW + 3) as u16,
        };

        assert_eq!(address.tile_x(), 3);
        assert_eq!(address.tile_y(), 1);

        let (u, v) = address.atlas_uv_origin();
        assert!((u - ((3 * TILE_STRIDE + TILE_GUTTER) as f32 / (ATLAS_SIZE as f32))).abs() < 1e-6);
        assert!((v - ((TILE_STRIDE + TILE_GUTTER) as f32 / (ATLAS_SIZE as f32))).abs() < 1e-6);

        let origin = tile_origin(address);
        assert_eq!(origin.x, 3 * TILE_STRIDE + TILE_GUTTER);
        assert_eq!(origin.y, TILE_STRIDE + TILE_GUTTER);
        assert_eq!(origin.z, 2);
    }

    #[test]
    fn virtual_image_iter_tiles_skips_empty_and_preserves_tile_coordinates() {
        let mut image = VirtualImage::<u8>::new(TILE_SIZE * 2, TILE_SIZE * 2).expect("new image");
        image.set_tile(1, 0, 7).expect("set tile 1,0");
        image.set_tile(0, 1, 9).expect("set tile 0,1");

        let tiles: Vec<(u32, u32, u8)> = image
            .iter_tiles()
            .map(|(tile_x, tile_y, value)| (tile_x, tile_y, *value))
            .collect();

        assert_eq!(tiles, vec![(1, 0, 7), (0, 1, 9)]);
    }

    #[test]
    fn new_export_is_transparent_black() {
        let image = VirtualImage::<u8>::new(17, 9).expect("new image");
        let bytes = image
            .export_rgba8(|_key| panic!("no tiles to load"))
            .expect("export");
        assert_eq!(bytes.len(), 17 * 9 * 4);
        assert!(bytes.iter().all(|&byte| byte == 0));
    }
}
