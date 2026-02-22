use std::sync::OnceLock;

use crate::{
    TILE_GUTTER, TILE_SIZE, TILE_STRIDE, TileAtlasCreateError, TileAtlasUsage, TileGpuDrainError,
};
use crate::atlas::TileAtlasFormat;

use super::format_core::{R32FloatSpec, R8UintSpec, Rgba8Spec, Rgba8SrgbSpec, TileFormatSpec};
use crate::atlas::gpu;

pub trait TileGpuCreateValidator {
    fn validate_gpu_create(
        device: &wgpu::Device,
        usage: TileAtlasUsage,
    ) -> Result<(), TileAtlasCreateError>;
}

pub trait TileGpuOpAdapter: super::format_core::TilePayloadSpec {
    fn validate_gpu_drain_usage(usage: TileAtlasUsage) -> Result<(), TileGpuDrainError>;

    fn execute_clear_slot(
        queue: &wgpu::Queue,
        texture: &wgpu::Texture,
        slot_origin: wgpu::Origin3d,
    ) -> Result<(), TileGpuDrainError>;

    fn execute_upload_payload(
        queue: &wgpu::Queue,
        texture: &wgpu::Texture,
        slot_origin: wgpu::Origin3d,
        payload: Self::UploadPayload,
    ) -> Result<(), TileGpuDrainError>;
}

impl TileGpuCreateValidator for Rgba8Spec {
    fn validate_gpu_create(
        _device: &wgpu::Device,
        usage: TileAtlasUsage,
    ) -> Result<(), TileAtlasCreateError> {
        validate_copy_dst_create_usage(usage)
    }
}

impl TileGpuOpAdapter for Rgba8Spec {
    fn validate_gpu_drain_usage(usage: TileAtlasUsage) -> Result<(), TileGpuDrainError> {
        validate_copy_dst_drain_usage(usage)
    }

    fn execute_clear_slot(
        queue: &wgpu::Queue,
        texture: &wgpu::Texture,
        slot_origin: wgpu::Origin3d,
    ) -> Result<(), TileGpuDrainError> {
        execute_clear_slot_write_texture(
            queue,
            texture,
            slot_origin,
            rgba8_zero_slot_bytes(),
            TILE_STRIDE * 4,
        )
    }

    fn execute_upload_payload(
        queue: &wgpu::Queue,
        texture: &wgpu::Texture,
        slot_origin: wgpu::Origin3d,
        payload: Self::UploadPayload,
    ) -> Result<(), TileGpuDrainError> {
        let bytes = payload.as_ref();
        if bytes.len() != super::format_core::rgba8_tile_len() {
            return Err(TileGpuDrainError::UploadLengthMismatch);
        }
        let expanded = expand_tile_rgba8_with_gutter(bytes);
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: slot_origin,
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
        Ok(())
    }
}

impl TileGpuCreateValidator for Rgba8SrgbSpec {
    fn validate_gpu_create(
        _device: &wgpu::Device,
        usage: TileAtlasUsage,
    ) -> Result<(), TileAtlasCreateError> {
        validate_copy_dst_create_usage(usage)
    }
}

impl TileGpuOpAdapter for Rgba8SrgbSpec {
    fn validate_gpu_drain_usage(usage: TileAtlasUsage) -> Result<(), TileGpuDrainError> {
        Rgba8Spec::validate_gpu_drain_usage(usage)
    }

    fn execute_clear_slot(
        queue: &wgpu::Queue,
        texture: &wgpu::Texture,
        slot_origin: wgpu::Origin3d,
    ) -> Result<(), TileGpuDrainError> {
        Rgba8Spec::execute_clear_slot(queue, texture, slot_origin)
    }

    fn execute_upload_payload(
        queue: &wgpu::Queue,
        texture: &wgpu::Texture,
        slot_origin: wgpu::Origin3d,
        payload: Self::UploadPayload,
    ) -> Result<(), TileGpuDrainError> {
        Rgba8Spec::execute_upload_payload(queue, texture, slot_origin, payload)
    }
}

impl TileGpuCreateValidator for R32FloatSpec {
    fn validate_gpu_create(
        device: &wgpu::Device,
        usage: TileAtlasUsage,
    ) -> Result<(), TileAtlasCreateError> {
        validate_storage_binding_create(device, usage, <R32FloatSpec as TileFormatSpec>::FORMAT)
    }
}

impl TileGpuOpAdapter for R32FloatSpec {
    fn validate_gpu_drain_usage(usage: TileAtlasUsage) -> Result<(), TileGpuDrainError> {
        validate_copy_dst_drain_usage(usage)
    }

    fn execute_clear_slot(
        queue: &wgpu::Queue,
        texture: &wgpu::Texture,
        slot_origin: wgpu::Origin3d,
    ) -> Result<(), TileGpuDrainError> {
        execute_clear_slot_write_texture(
            queue,
            texture,
            slot_origin,
            r32float_zero_slot_bytes(),
            TILE_STRIDE * 4,
        )
    }

    fn execute_upload_payload(
        _queue: &wgpu::Queue,
        _texture: &wgpu::Texture,
        _slot_origin: wgpu::Origin3d,
        payload: Self::UploadPayload,
    ) -> Result<(), TileGpuDrainError> {
        match payload {}
    }
}

impl TileGpuCreateValidator for R8UintSpec {
    fn validate_gpu_create(
        _device: &wgpu::Device,
        _usage: TileAtlasUsage,
    ) -> Result<(), TileAtlasCreateError> {
        Ok(())
    }
}

impl TileGpuOpAdapter for R8UintSpec {
    fn validate_gpu_drain_usage(usage: TileAtlasUsage) -> Result<(), TileGpuDrainError> {
        validate_copy_dst_drain_usage(usage)
    }

    fn execute_clear_slot(
        queue: &wgpu::Queue,
        texture: &wgpu::Texture,
        slot_origin: wgpu::Origin3d,
    ) -> Result<(), TileGpuDrainError> {
        execute_clear_slot_write_texture(
            queue,
            texture,
            slot_origin,
            r8uint_zero_slot_bytes(),
            TILE_STRIDE,
        )
    }

    fn execute_upload_payload(
        _queue: &wgpu::Queue,
        _texture: &wgpu::Texture,
        _slot_origin: wgpu::Origin3d,
        payload: Self::UploadPayload,
    ) -> Result<(), TileGpuDrainError> {
        match payload {}
    }
}

fn validate_storage_binding_create(
    device: &wgpu::Device,
    usage: TileAtlasUsage,
    format: TileAtlasFormat,
) -> Result<(), TileAtlasCreateError> {
    if !usage.contains_storage_binding() {
        return Err(TileAtlasCreateError::MissingStorageBindingUsage);
    }
    if !supports_storage_binding_usage_for_format(device, format) {
        return Err(TileAtlasCreateError::StorageBindingUnsupportedForFormat);
    }
    Ok(())
}

fn supports_storage_binding_usage_for_format(
    device: &wgpu::Device,
    format: TileAtlasFormat,
) -> bool {
    let error_scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let _probe_texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("tiles.storage_binding_probe"),
        size: wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: gpu::atlas_format_to_wgpu(format),
        usage: wgpu::TextureUsages::STORAGE_BINDING,
        view_formats: &[],
    });
    pollster::block_on(error_scope.pop()).is_none()
}

fn validate_copy_dst_create_usage(usage: TileAtlasUsage) -> Result<(), TileAtlasCreateError> {
    if !usage.contains_copy_dst() {
        return Err(TileAtlasCreateError::MissingCopyDstUsage);
    }
    Ok(())
}

fn validate_copy_dst_drain_usage(usage: TileAtlasUsage) -> Result<(), TileGpuDrainError> {
    if !usage.contains_copy_dst() {
        return Err(TileGpuDrainError::MissingCopyDstUsage);
    }
    Ok(())
}

fn execute_clear_slot_write_texture(
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    slot_origin: wgpu::Origin3d,
    clear_bytes: &'static [u8],
    bytes_per_row: u32,
) -> Result<(), TileGpuDrainError> {
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: slot_origin,
            aspect: wgpu::TextureAspect::All,
        },
        clear_bytes,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(bytes_per_row),
            rows_per_image: Some(TILE_STRIDE),
        },
        wgpu::Extent3d {
            width: TILE_STRIDE,
            height: TILE_STRIDE,
            depth_or_array_layers: 1,
        },
    );
    Ok(())
}

fn rgba8_zero_slot_bytes() -> &'static [u8] {
    static ZERO_RGBA8: OnceLock<Vec<u8>> = OnceLock::new();
    ZERO_RGBA8
        .get_or_init(|| vec![0u8; (TILE_STRIDE as usize) * (TILE_STRIDE as usize) * 4])
        .as_slice()
}

fn r32float_zero_slot_bytes() -> &'static [u8] {
    static ZERO_R32FLOAT: OnceLock<Vec<u8>> = OnceLock::new();
    ZERO_R32FLOAT
        .get_or_init(|| vec![0u8; (TILE_STRIDE as usize) * (TILE_STRIDE as usize) * 4])
        .as_slice()
}

fn r8uint_zero_slot_bytes() -> &'static [u8] {
    static ZERO_R8UINT: OnceLock<Vec<u8>> = OnceLock::new();
    ZERO_R8UINT
        .get_or_init(|| vec![0u8; (TILE_STRIDE as usize) * (TILE_STRIDE as usize)])
        .as_slice()
}

fn expand_tile_rgba8_with_gutter(content: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; (TILE_STRIDE as usize) * (TILE_STRIDE as usize) * 4];
    for y in 0..TILE_SIZE as usize {
        for x in 0..TILE_SIZE as usize {
            let src = (y * TILE_SIZE as usize + x) * 4;
            let dst_x = x + TILE_GUTTER as usize;
            let dst_y = y + TILE_GUTTER as usize;
            let dst = (dst_y * TILE_STRIDE as usize + dst_x) * 4;
            out[dst..dst + 4].copy_from_slice(&content[src..src + 4]);
        }
    }
    out
}
