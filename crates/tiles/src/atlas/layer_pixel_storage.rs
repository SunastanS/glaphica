use std::sync::Arc;

use crate::{
    ImageIngestError, TileAddress, TileAllocError, TileAtlasCreateError, TileAtlasLayout,
    TileGpuDrainError, TileIngestError, TileKey, TileSetError, TileSetHandle, VirtualImage,
    TILE_SIZE,
};

use super::brush_buffer_storage;
use super::format::{rgba8_tile_len, Rgba8Spec, Rgba8SrgbSpec, TileUploadFormatSpec};
use super::{GenericTileAtlasConfig, TileAtlasConfig};

#[derive(Debug)]
pub struct TileAtlasStore {
    generic: LayerStoreBackend,
}

#[derive(Debug)]
pub struct TileAtlasGpuArray {
    generic: LayerGpuBackend,
}

#[derive(Debug)]
enum LayerStoreBackend {
    Unorm(brush_buffer_storage::GenericTileAtlasStore<Rgba8Spec>),
    Srgb(brush_buffer_storage::GenericTileAtlasStore<Rgba8SrgbSpec>),
}

#[derive(Debug)]
enum LayerGpuBackend {
    Unorm(brush_buffer_storage::GenericTileAtlasGpuArray<Rgba8Spec>),
    Srgb(brush_buffer_storage::GenericTileAtlasGpuArray<Rgba8SrgbSpec>),
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
        match config.format {
            wgpu::TextureFormat::Rgba8Unorm => {
                let (store, gpu) =
                    brush_buffer_storage::GenericTileAtlasStore::<Rgba8Spec>::with_config(
                        device,
                        GenericTileAtlasConfig::from(config),
                    )?;
                Ok((
                    Self {
                        generic: LayerStoreBackend::Unorm(store),
                    },
                    TileAtlasGpuArray {
                        generic: LayerGpuBackend::Unorm(gpu),
                    },
                ))
            }
            wgpu::TextureFormat::Rgba8UnormSrgb => {
                let (store, gpu) =
                    brush_buffer_storage::GenericTileAtlasStore::<Rgba8SrgbSpec>::with_config(
                        device,
                        GenericTileAtlasConfig::from(config),
                    )?;
                Ok((
                    Self {
                        generic: LayerStoreBackend::Srgb(store),
                    },
                    TileAtlasGpuArray {
                        generic: LayerGpuBackend::Srgb(gpu),
                    },
                ))
            }
            _ => Err(TileAtlasCreateError::UnsupportedPayloadFormat),
        }
    }

    pub fn is_allocated(&self, key: TileKey) -> bool {
        match &self.generic {
            LayerStoreBackend::Unorm(store) => store.is_allocated(key),
            LayerStoreBackend::Srgb(store) => store.is_allocated(key),
        }
    }

    pub fn resolve(&self, key: TileKey) -> Option<TileAddress> {
        match &self.generic {
            LayerStoreBackend::Unorm(store) => store.resolve(key),
            LayerStoreBackend::Srgb(store) => store.resolve(key),
        }
    }

    pub fn allocate(&self) -> Result<TileKey, TileAllocError> {
        match &self.generic {
            LayerStoreBackend::Unorm(store) => store.allocate(),
            LayerStoreBackend::Srgb(store) => store.allocate(),
        }
    }

    pub fn release(&self, key: TileKey) -> bool {
        match &self.generic {
            LayerStoreBackend::Unorm(store) => store.release(key),
            LayerStoreBackend::Srgb(store) => store.release(key),
        }
    }

    pub fn reserve_tile_set(&self, count: u32) -> Result<TileSetHandle, TileSetError> {
        match &self.generic {
            LayerStoreBackend::Unorm(store) => store.reserve_tile_set(count),
            LayerStoreBackend::Srgb(store) => store.reserve_tile_set(count),
        }
    }

    pub fn adopt_tile_set(
        &self,
        keys: impl IntoIterator<Item = TileKey>,
    ) -> Result<TileSetHandle, TileSetError> {
        let keys = keys.into_iter().collect::<Vec<_>>();
        match &self.generic {
            LayerStoreBackend::Unorm(store) => store.adopt_tile_set(keys.iter().copied()),
            LayerStoreBackend::Srgb(store) => store.adopt_tile_set(keys.iter().copied()),
        }
    }

    pub fn release_tile_set(&self, set: TileSetHandle) -> Result<u32, TileSetError> {
        match &self.generic {
            LayerStoreBackend::Unorm(store) => store.release_tile_set(set),
            LayerStoreBackend::Srgb(store) => store.release_tile_set(set),
        }
    }

    pub fn clear_tile_set(&self, set: &TileSetHandle) -> Result<u32, TileSetError> {
        match &self.generic {
            LayerStoreBackend::Unorm(store) => store.clear_tile_set(set),
            LayerStoreBackend::Srgb(store) => store.clear_tile_set(set),
        }
    }

    pub fn resolve_tile_set(
        &self,
        set: &TileSetHandle,
    ) -> Result<Vec<(TileKey, TileAddress)>, TileSetError> {
        match &self.generic {
            LayerStoreBackend::Unorm(store) => store.resolve_tile_set(set),
            LayerStoreBackend::Srgb(store) => store.resolve_tile_set(set),
        }
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
        Rgba8Spec::validate_ingest_contract(self.usage())?;

        if bytes.len() != rgba8_tile_len() {
            return Err(TileIngestError::BufferLengthMismatch);
        }
        if bytes.iter().all(|&byte| byte == 0) {
            return Ok(None);
        }

        self.enqueue_upload_bytes(Arc::<[u8]>::from(bytes.to_vec()))
            .map(Some)
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
        Rgba8Spec::validate_ingest_contract(self.usage())?;

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

        self.enqueue_upload_bytes(Arc::<[u8]>::from(packed))
            .map(Some)
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
        Rgba8Spec::validate_ingest_contract(self.usage()).map_err(ImageIngestError::from)?;

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

        self.enqueue_upload_bytes(Arc::<[u8]>::from(packed))
            .map(Some)
            .map_err(ImageIngestError::from)
    }

    fn usage(&self) -> wgpu::TextureUsages {
        match &self.generic {
            LayerStoreBackend::Unorm(store) => store.usage(),
            LayerStoreBackend::Srgb(store) => store.usage(),
        }
    }

    fn enqueue_upload_bytes(&self, bytes: Arc<[u8]>) -> Result<TileKey, TileIngestError> {
        match &self.generic {
            LayerStoreBackend::Unorm(store) => store.enqueue_upload_bytes(bytes),
            LayerStoreBackend::Srgb(store) => store.enqueue_upload_bytes(bytes),
        }
    }
}

impl TileAtlasGpuArray {
    pub fn view(&self) -> &wgpu::TextureView {
        match &self.generic {
            LayerGpuBackend::Unorm(gpu) => gpu.view(),
            LayerGpuBackend::Srgb(gpu) => gpu.view(),
        }
    }

    pub fn texture(&self) -> &wgpu::Texture {
        match &self.generic {
            LayerGpuBackend::Unorm(gpu) => gpu.texture(),
            LayerGpuBackend::Srgb(gpu) => gpu.texture(),
        }
    }

    pub fn layout(&self) -> TileAtlasLayout {
        match &self.generic {
            LayerGpuBackend::Unorm(gpu) => gpu.layout(),
            LayerGpuBackend::Srgb(gpu) => gpu.layout(),
        }
    }

    pub fn drain_and_execute(&self, queue: &wgpu::Queue) -> Result<usize, TileGpuDrainError> {
        match &self.generic {
            LayerGpuBackend::Unorm(gpu) => gpu.drain_and_execute(queue),
            LayerGpuBackend::Srgb(gpu) => gpu.drain_and_execute(queue),
        }
    }
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
