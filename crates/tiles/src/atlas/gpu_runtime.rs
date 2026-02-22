use super::{GenericTileAtlasConfig, core};
use crate::{TILE_STRIDE, TileAddress, TileAtlasCreateError};
#[cfg(test)]
use crate::{TILE_GUTTER, TILES_PER_ROW};
use super::{TileAtlasFormat, TileAtlasUsage};

pub(in crate::atlas) fn validate_generic_atlas_config(
    device: &wgpu::Device,
    config: GenericTileAtlasConfig,
) -> Result<(), TileAtlasCreateError> {
    let core_config = core_config_from_generic(config);
    if core_config.max_layers == 0 {
        return Err(TileAtlasCreateError::MaxLayersZero);
    }
    let layout = core::AtlasLayout::from_config(core_config)?;

    let limits = device.limits();
    if core_config.max_layers > limits.max_texture_array_layers {
        return Err(TileAtlasCreateError::MaxLayersExceedsDeviceLimit);
    }
    if layout.atlas_width > limits.max_texture_dimension_2d
        || layout.atlas_height > limits.max_texture_dimension_2d
    {
        return Err(TileAtlasCreateError::AtlasSizeExceedsDeviceLimit);
    }

    Ok(())
}

pub(in crate::atlas) fn core_config_from_generic(
    config: GenericTileAtlasConfig,
) -> core::AtlasCoreConfig {
    core::AtlasCoreConfig {
        max_layers: config.max_layers,
        tiles_per_row: config.tiles_per_row,
        tiles_per_column: config.tiles_per_column,
    }
}

pub(in crate::atlas) fn core_usage_from_public(usage: TileAtlasUsage) -> core::AtlasUsage {
    let mut core_usage = core::AtlasUsage::empty();
    if usage.contains_copy_dst() {
        core_usage = core_usage.with_copy_dst();
    }
    if usage.contains_texture_binding() {
        core_usage = core_usage.with_texture_binding();
    }
    if usage.contains_storage_binding() {
        core_usage = core_usage.with_storage_binding();
    }
    core_usage
}

pub(in crate::atlas) fn public_usage_from_core(usage: core::AtlasUsage) -> TileAtlasUsage {
    let mut public_usage = TileAtlasUsage::empty();
    if usage.contains_copy_dst() {
        public_usage |= TileAtlasUsage::COPY_DST;
    }
    if usage.contains_texture_binding() {
        public_usage |= TileAtlasUsage::TEXTURE_BINDING;
    }
    if usage.contains_storage_binding() {
        public_usage |= TileAtlasUsage::STORAGE_BINDING;
    }
    public_usage
}

pub(in crate::atlas) fn atlas_usage_to_wgpu(usage: TileAtlasUsage) -> wgpu::TextureUsages {
    let mut wgpu_usage = wgpu::TextureUsages::empty();
    if usage.contains_copy_dst() {
        wgpu_usage |= wgpu::TextureUsages::COPY_DST;
    }
    if usage.contains_texture_binding() {
        wgpu_usage |= wgpu::TextureUsages::TEXTURE_BINDING;
    }
    if usage.contains_copy_src() {
        wgpu_usage |= wgpu::TextureUsages::COPY_SRC;
    }
    if usage.contains_storage_binding() {
        wgpu_usage |= wgpu::TextureUsages::STORAGE_BINDING;
    }
    wgpu_usage
}

pub(in crate::atlas) fn atlas_format_to_wgpu(format: TileAtlasFormat) -> wgpu::TextureFormat {
    match format {
        TileAtlasFormat::Rgba8Unorm => wgpu::TextureFormat::Rgba8Unorm,
        TileAtlasFormat::Rgba8UnormSrgb => wgpu::TextureFormat::Rgba8UnormSrgb,
        TileAtlasFormat::R32Float => wgpu::TextureFormat::R32Float,
        TileAtlasFormat::R8Uint => wgpu::TextureFormat::R8Uint,
    }
}

pub(in crate::atlas) fn create_atlas_texture_and_array_view(
    device: &wgpu::Device,
    layout: core::AtlasLayout,
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
    let (tile_x, tile_y) = core::tile_coords_from_index_with_row(address.tile_index, tiles_per_row);
    wgpu::Origin3d {
        x: tile_x * TILE_STRIDE,
        y: tile_y * TILE_STRIDE,
        z: address.atlas_layer,
    }
}
