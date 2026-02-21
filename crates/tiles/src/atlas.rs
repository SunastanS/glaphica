use super::DEFAULT_MAX_LAYERS;

mod core; // mod core
mod format;

mod brush_buffer_storage;
mod group_preview;
mod layer_pixel_storage;

#[derive(Debug, Clone, Copy)]
pub struct TileAtlasConfig {
    pub max_layers: u32,
    pub tiles_per_row: u32,
    pub tiles_per_column: u32,
    pub format: wgpu::TextureFormat,
    pub usage: wgpu::TextureUsages,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TilePayloadKind {
    Rgba8,
    R32Float,
    R8Uint,
}

#[derive(Debug, Clone, Copy)]
pub struct GenericTileAtlasConfig {
    pub max_layers: u32,
    pub tiles_per_row: u32,
    pub tiles_per_column: u32,
    pub usage: wgpu::TextureUsages,
}

impl Default for GenericTileAtlasConfig {
    fn default() -> Self {
        Self {
            max_layers: DEFAULT_MAX_LAYERS,
            tiles_per_row: crate::TILES_PER_ROW,
            tiles_per_column: crate::TILES_PER_ROW,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::COPY_SRC,
        }
    }
}

impl From<TileAtlasConfig> for GenericTileAtlasConfig {
    fn from(value: TileAtlasConfig) -> Self {
        Self {
            max_layers: value.max_layers,
            tiles_per_row: value.tiles_per_row,
            tiles_per_column: value.tiles_per_column,
            usage: value.usage,
        }
    }
}

impl Default for TileAtlasConfig {
    fn default() -> Self {
        Self {
            max_layers: DEFAULT_MAX_LAYERS,
            tiles_per_row: crate::TILES_PER_ROW,
            tiles_per_column: crate::TILES_PER_ROW,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::COPY_SRC,
        }
    }
}

pub use brush_buffer_storage::{
    GenericR8UintTileAtlasGpuArray, GenericR8UintTileAtlasStore, GenericR32FloatTileAtlasGpuArray,
    GenericR32FloatTileAtlasStore, GenericTileAtlasGpuArray, GenericTileAtlasStore,
    RuntimeGenericTileAtlasConfig, RuntimeGenericTileAtlasGpuArray, RuntimeGenericTileAtlasStore,
};
pub use group_preview::{GroupTileAtlasGpuArray, GroupTileAtlasStore};
pub use layer_pixel_storage::{TileAtlasGpuArray, TileAtlasStore};

#[cfg(test)]
pub(crate) use core::tile_origin;
#[cfg(test)]
pub(crate) use format::rgba8_tile_len;
