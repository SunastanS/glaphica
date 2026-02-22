use std::sync::Arc;

use crate::{
    TileAddress, TileAllocError, TileAtlasCreateError, TileAtlasLayout, TileKey, TileSetError,
    TileSetHandle,
};

use super::{GenericTileAtlasConfig, TileAtlasConfig, TileAtlasFormat, TileAtlasUsage, core, gpu};

#[derive(Debug)]
pub struct GroupTileAtlasStore {
    cpu: Arc<core::TileAtlasCpu>,
}

#[derive(Debug)]
pub struct GroupTileAtlasGpuArray {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    format: TileAtlasFormat,
    max_layers: u32,
    layout: TileAtlasLayout,
}

impl GroupTileAtlasStore {
    pub fn new(
        device: &wgpu::Device,
        format: TileAtlasFormat,
        usage: TileAtlasUsage,
    ) -> Result<(Self, GroupTileAtlasGpuArray), TileAtlasCreateError> {
        Self::with_config(
            device,
            TileAtlasConfig {
                max_layers: 2,
                format,
                usage,
                ..TileAtlasConfig::default()
            },
        )
    }

    pub fn with_config(
        device: &wgpu::Device,
        config: TileAtlasConfig,
    ) -> Result<(Self, GroupTileAtlasGpuArray), TileAtlasCreateError> {
        gpu::validate_generic_atlas_config(device, GenericTileAtlasConfig::from(config))?;
        validate_group_atlas_config(device, config)?;
        let layout = core::AtlasLayout::from_config(gpu::core_config_from_generic(
            GenericTileAtlasConfig::from(config),
        ))?;

        let cpu = Arc::new(
            core::TileAtlasCpu::new(config.max_layers, layout)
                .map_err(|_| TileAtlasCreateError::MaxLayersExceedsDeviceLimit)?,
        );
        let (texture, view) = gpu::create_atlas_texture_and_array_view(
            device,
            layout,
            config.max_layers,
            gpu::atlas_format_to_wgpu(config.format),
            gpu::atlas_usage_to_wgpu(config.usage),
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
                layout: TileAtlasLayout {
                    tiles_per_row: layout.tiles_per_row,
                    tiles_per_column: layout.tiles_per_column,
                    atlas_width: layout.atlas_width,
                    atlas_height: layout.atlas_height,
                },
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

    pub fn force_release_all_keys(&self) -> usize {
        self.cpu.release_all()
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

    pub fn resolve_tile_set(
        &self,
        set: &TileSetHandle,
    ) -> Result<Vec<(TileKey, TileAddress)>, TileSetError> {
        core::resolve_tile_set(&self.cpu, set)
    }
}

fn validate_group_atlas_config(
    device: &wgpu::Device,
    config: TileAtlasConfig,
) -> Result<(), TileAtlasCreateError> {
    if !config.usage.contains_copy_dst() {
        return Err(TileAtlasCreateError::MissingCopyDstUsage);
    }
    if !config.usage.contains_texture_binding() {
        return Err(TileAtlasCreateError::MissingTextureBindingUsage);
    }
    if !gpu::supports_texture_usage_for_format(
        device,
        gpu::atlas_format_to_wgpu(config.format),
        gpu::atlas_usage_to_wgpu(config.usage),
    ) {
        return Err(TileAtlasCreateError::UnsupportedFormatUsage);
    }
    Ok(())
}

impl GroupTileAtlasGpuArray {
    pub fn view(&self) -> &wgpu::TextureView {
        &self.view
    }

    pub fn texture(&self) -> &wgpu::Texture {
        &self.texture
    }

    pub fn layout(&self) -> TileAtlasLayout {
        self.layout
    }

    pub fn layer_view(&self, layer: u32) -> wgpu::TextureView {
        assert!(
            layer < self.max_layers,
            "group atlas layer out of range: {layer} >= {}",
            self.max_layers
        );
        self.texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("tiles.group_atlas.layer.view"),
            format: Some(gpu::atlas_format_to_wgpu(self.format)),
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
