use atlas::{AtlasLayout, BackendId, TileKey};
use glaphica_core::ATLAS_TILE_SIZE;

use super::types::{BrushIntermediateFormat, TileRendererError};
use crate::{RendererTexture, RendererTextureDescriptor};

#[derive(Debug)]
struct AtlasBackendTexture {
    layout: AtlasLayout,
    format: BrushIntermediateFormat,
    texture: RendererTexture,
}

#[derive(Debug, Default)]
pub(crate) struct AtlasTextureSet {
    backends: Vec<Option<AtlasBackendTexture>>,
}

#[derive(Debug, Clone, Copy)]
pub struct ResolvedAtlasTile<'a> {
    pub texture: &'a RendererTexture,
    pub layer: u32,
    pub tile_x: u32,
    pub tile_y: u32,
}

pub struct AtlasTextureStage {
    atlas_textures: AtlasTextureSet,
}

impl AtlasTextureSet {
    fn new() -> Self {
        Self::default()
    }

    fn ensure_backend_texture(
        &mut self,
        device: &wgpu::Device,
        backend: &atlas::Backend,
        format: BrushIntermediateFormat,
    ) -> Result<&AtlasBackendTexture, TileRendererError> {
        let backend_id = backend.backend_id();
        let index = backend_id.raw() as usize;
        if self.backends.len() <= index {
            self.backends.resize_with(index + 1, || None);
        }
        if self.backends[index].is_none() {
            let layout = backend.layout()?;
            let edge = layout.tiles_per_edge() * ATLAS_TILE_SIZE;
            let texture = RendererTexture::new(
                device,
                &RendererTextureDescriptor::atlas_with_format(
                    None,
                    edge,
                    edge,
                    layout.layers(),
                    format,
                ),
            )?;
            self.backends[index] = Some(AtlasBackendTexture {
                layout,
                format,
                texture,
            });
        }
        if let Some(backend) = self.backends[index].as_ref() {
            if backend.format != format {
                return Err(TileRendererError::BackendTextureFormatMismatch {
                    backend_id,
                    expected: format,
                    actual: backend.format,
                });
            }
        }
        self.backends[index]
            .as_ref()
            .ok_or(TileRendererError::MissingBackendTexture(backend_id))
    }

    fn ensure_backend_texture_default(
        &mut self,
        device: &wgpu::Device,
        backend: &atlas::Backend,
    ) -> Result<&AtlasBackendTexture, TileRendererError> {
        let backend_id = backend.backend_id();
        let index = backend_id.raw() as usize;
        if self.backends.len() <= index || self.backends[index].is_none() {
            return self.ensure_backend_texture(
                device,
                backend,
                BrushIntermediateFormat::Rgba8Unorm,
            );
        }
        self.backends[index]
            .as_ref()
            .ok_or(TileRendererError::MissingBackendTexture(backend_id))
    }

    fn backend_texture(&self, backend_id: BackendId) -> Option<&AtlasBackendTexture> {
        self.backends
            .get(backend_id.raw() as usize)
            .and_then(Option::as_ref)
    }

    fn resolve_tile(&self, tile_key: TileKey) -> Result<ResolvedAtlasTile<'_>, TileRendererError> {
        if tile_key == TileKey::EMPTY {
            return Err(TileRendererError::InvalidTileKey);
        }
        let parts = tile_key.parts();
        let backend = self
            .backend_texture(parts.backend_id)
            .ok_or(TileRendererError::MissingBackendTexture(parts.backend_id))?;
        let address = backend
            .layout
            .slot_address(parts.slot_index)
            .ok_or(TileRendererError::InvalidTileKey)?;
        Ok(ResolvedAtlasTile {
            texture: &backend.texture,
            layer: address.layer,
            tile_x: address.tile_x,
            tile_y: address.tile_y,
        })
    }
}

impl AtlasTextureStage {
    pub fn new() -> Self {
        Self {
            atlas_textures: AtlasTextureSet::new(),
        }
    }

    pub fn ensure_backend(
        &mut self,
        device: &wgpu::Device,
        backend: &atlas::Backend,
    ) -> Result<(), TileRendererError> {
        self.atlas_textures
            .ensure_backend_texture_default(device, backend)?;
        Ok(())
    }

    pub fn ensure_backend_with_format(
        &mut self,
        device: &wgpu::Device,
        backend: &atlas::Backend,
        format: BrushIntermediateFormat,
    ) -> Result<(), TileRendererError> {
        self.atlas_textures
            .ensure_backend_texture(device, backend, format)?;
        Ok(())
    }

    pub fn resolve_tile(
        &self,
        tile_key: TileKey,
    ) -> Result<ResolvedAtlasTile<'_>, TileRendererError> {
        self.atlas_textures.resolve_tile(tile_key)
    }

    pub fn apply_clear_batches(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        backends: &[&atlas::Backend],
        clear_batches: &[atlas::ClearBatch],
    ) -> Result<(), TileRendererError> {
        for backend in backends {
            self.atlas_textures
                .ensure_backend_texture_default(device, backend)?;
        }
        let zero_tile = vec![0u8; (ATLAS_TILE_SIZE * ATLAS_TILE_SIZE * 4) as usize];
        for batch in clear_batches {
            let backend = self
                .atlas_textures
                .backend_texture(batch.backend_id)
                .ok_or(TileRendererError::MissingBackendTexture(batch.backend_id))?;
            for &slot in &batch.slots {
                let Some(address) = backend.layout.slot_address(slot) else {
                    return Err(TileRendererError::InvalidTileKey);
                };
                queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &backend.texture.texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d {
                            x: address.tile_x * ATLAS_TILE_SIZE,
                            y: address.tile_y * ATLAS_TILE_SIZE,
                            z: address.layer,
                        },
                        aspect: wgpu::TextureAspect::All,
                    },
                    &zero_tile,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(ATLAS_TILE_SIZE * 4),
                        rows_per_image: Some(ATLAS_TILE_SIZE),
                    },
                    wgpu::Extent3d {
                        width: ATLAS_TILE_SIZE,
                        height: ATLAS_TILE_SIZE,
                        depth_or_array_layers: 1,
                    },
                );
            }
        }
        Ok(())
    }

    pub fn upload_rgba8_tile(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        backend: &atlas::Backend,
        tile_key: TileKey,
        pixels_rgba8: &[u8],
    ) -> Result<(), TileRendererError> {
        self.atlas_textures.ensure_backend_texture(
            device,
            backend,
            BrushIntermediateFormat::Rgba8Unorm,
        )?;
        let resolved = self.atlas_textures.resolve_tile(tile_key)?;
        let expected_len = (ATLAS_TILE_SIZE * ATLAS_TILE_SIZE * 4) as usize;
        if pixels_rgba8.len() != expected_len {
            return Err(TileRendererError::InvalidTileKey);
        }
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &resolved.texture.texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: resolved.tile_x * ATLAS_TILE_SIZE,
                    y: resolved.tile_y * ATLAS_TILE_SIZE,
                    z: resolved.layer,
                },
                aspect: wgpu::TextureAspect::All,
            },
            pixels_rgba8,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(ATLAS_TILE_SIZE * 4),
                rows_per_image: Some(ATLAS_TILE_SIZE),
            },
            wgpu::Extent3d {
                width: ATLAS_TILE_SIZE,
                height: ATLAS_TILE_SIZE,
                depth_or_array_layers: 1,
            },
        );
        Ok(())
    }

    pub fn encode_copy_tile(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        source_tile_key: TileKey,
        destination_tile_key: TileKey,
    ) -> Result<(), TileRendererError> {
        let source = self.atlas_textures.resolve_tile(source_tile_key)?;
        let destination = self.atlas_textures.resolve_tile(destination_tile_key)?;
        encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &source.texture.texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: source.tile_x * ATLAS_TILE_SIZE,
                    y: source.tile_y * ATLAS_TILE_SIZE,
                    z: source.layer,
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture: &destination.texture.texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: destination.tile_x * ATLAS_TILE_SIZE,
                    y: destination.tile_y * ATLAS_TILE_SIZE,
                    z: destination.layer,
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width: ATLAS_TILE_SIZE,
                height: ATLAS_TILE_SIZE,
                depth_or_array_layers: 1,
            },
        );
        Ok(())
    }
}
