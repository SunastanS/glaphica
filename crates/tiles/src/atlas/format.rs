use std::convert::Infallible;
use std::sync::{Arc, OnceLock};

use crate::{
    TILE_GUTTER, TILE_SIZE, TILE_STRIDE, TileAtlasCreateError, TileGpuDrainError, TileIngestError,
};
use super::{TileAtlasFormat, TileAtlasUsage, gpu_runtime};

pub trait TileFormatSpec {
    const PAYLOAD_KIND: super::TilePayloadKind;
    const FORMAT: TileAtlasFormat;
}

pub trait TilePayloadSpec {
    type UploadPayload;
}

pub trait TileGpuCreateValidator {
    fn validate_gpu_create(
        device: &wgpu::Device,
        usage: TileAtlasUsage,
    ) -> Result<(), TileAtlasCreateError>;
}

pub trait TileGpuOpAdapter: TilePayloadSpec {
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

pub trait TileUploadFormatSpec:
    TileFormatSpec + TileGpuOpAdapter + TilePayloadSpec<UploadPayload = Arc<[u8]>>
{
    fn validate_ingest_contract(usage: TileAtlasUsage) -> Result<(), TileIngestError>;

    fn validate_upload_bytes(bytes: &[u8]) -> Result<(), TileIngestError>;
}

#[derive(Debug)]
pub struct Rgba8Spec;
#[derive(Debug)]
pub struct Rgba8SrgbSpec;
#[derive(Debug)]
pub struct R32FloatSpec;
#[derive(Debug)]
pub struct R8UintSpec;

impl TileFormatSpec for Rgba8Spec {
    const PAYLOAD_KIND: super::TilePayloadKind = super::TilePayloadKind::Rgba8;
    const FORMAT: TileAtlasFormat = TileAtlasFormat::Rgba8Unorm;
}

impl TileGpuCreateValidator for Rgba8Spec {
    fn validate_gpu_create(
        _device: &wgpu::Device,
        usage: TileAtlasUsage,
    ) -> Result<(), TileAtlasCreateError> {
        validate_copy_dst_create_usage(usage)
    }
}

impl TilePayloadSpec for Rgba8Spec {
    type UploadPayload = Arc<[u8]>;
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
        if bytes.len() != rgba8_tile_len() {
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

impl TileUploadFormatSpec for Rgba8Spec {
    fn validate_ingest_contract(usage: TileAtlasUsage) -> Result<(), TileIngestError> {
        if !usage.contains_copy_dst() {
            return Err(TileIngestError::MissingCopyDstUsage);
        }
        Ok(())
    }

    fn validate_upload_bytes(bytes: &[u8]) -> Result<(), TileIngestError> {
        if bytes.len() != rgba8_tile_len() {
            return Err(TileIngestError::BufferLengthMismatch);
        }
        Ok(())
    }
}

impl TileFormatSpec for Rgba8SrgbSpec {
    const PAYLOAD_KIND: super::TilePayloadKind = super::TilePayloadKind::Rgba8;
    const FORMAT: TileAtlasFormat = TileAtlasFormat::Rgba8UnormSrgb;
}

impl TileGpuCreateValidator for Rgba8SrgbSpec {
    fn validate_gpu_create(
        _device: &wgpu::Device,
        usage: TileAtlasUsage,
    ) -> Result<(), TileAtlasCreateError> {
        validate_copy_dst_create_usage(usage)
    }
}

impl TilePayloadSpec for Rgba8SrgbSpec {
    type UploadPayload = Arc<[u8]>;
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

impl TileUploadFormatSpec for Rgba8SrgbSpec {
    fn validate_ingest_contract(usage: TileAtlasUsage) -> Result<(), TileIngestError> {
        Rgba8Spec::validate_ingest_contract(usage)
    }

    fn validate_upload_bytes(bytes: &[u8]) -> Result<(), TileIngestError> {
        Rgba8Spec::validate_upload_bytes(bytes)
    }
}

impl TileFormatSpec for R32FloatSpec {
    const PAYLOAD_KIND: super::TilePayloadKind = super::TilePayloadKind::R32Float;
    const FORMAT: TileAtlasFormat = TileAtlasFormat::R32Float;
}

impl TileGpuCreateValidator for R32FloatSpec {
    fn validate_gpu_create(
        device: &wgpu::Device,
        usage: TileAtlasUsage,
    ) -> Result<(), TileAtlasCreateError> {
        validate_storage_binding_create(device, usage, Self::FORMAT)
    }
}

impl TilePayloadSpec for R32FloatSpec {
    type UploadPayload = Infallible;
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
        format: gpu_runtime::atlas_format_to_wgpu(format),
        usage: wgpu::TextureUsages::STORAGE_BINDING,
        view_formats: &[],
    });
    pollster::block_on(error_scope.pop()).is_none()
}

impl TileFormatSpec for R8UintSpec {
    const PAYLOAD_KIND: super::TilePayloadKind = super::TilePayloadKind::R8Uint;
    const FORMAT: TileAtlasFormat = TileAtlasFormat::R8Uint;
}

impl TileGpuCreateValidator for R8UintSpec {
    fn validate_gpu_create(
        _device: &wgpu::Device,
        _usage: TileAtlasUsage,
    ) -> Result<(), TileAtlasCreateError> {
        Ok(())
    }
}

impl TilePayloadSpec for R8UintSpec {
    type UploadPayload = Infallible;
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

pub(crate) fn rgba8_tile_len() -> usize {
    (TILE_SIZE as usize) * (TILE_SIZE as usize) * 4
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
    let mut expanded = vec![0u8; (TILE_STRIDE as usize) * (TILE_STRIDE as usize) * 4];

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
            let left_source = content_start * 4;
            let left_source_texel = [
                expanded[left_source],
                expanded[left_source + 1],
                expanded[left_source + 2],
                expanded[left_source + 3],
            ];
            let left_index = (row_base + column) * 4;
            expanded[left_index..left_index + 4].copy_from_slice(&left_source_texel);

            let right_source = content_end * 4;
            let right_source_texel = [
                expanded[right_source],
                expanded[right_source + 1],
                expanded[right_source + 2],
                expanded[right_source + 3],
            ];
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
