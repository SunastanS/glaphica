use glaphica_core::{AtlasLayout, BackendKind, TileKey, ATLAS_TILE_SIZE};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AtlasAddress {
    pub layer: u32,
    pub tile_offset: (u32, u32),
    pub texel_offset: (u32, u32),
}

#[derive(Debug, Clone, Copy)]
pub struct AtlasResolvedAddress<'a> {
    pub texture2d_array: &'a wgpu::Texture,
    pub format: wgpu::TextureFormat,
    pub address: AtlasAddress,
}

#[derive(Debug, Clone, Copy)]
pub struct AtlasBackendResource<'a> {
    pub texture2d_array: &'a wgpu::Texture,
    pub format: wgpu::TextureFormat,
    pub layers: u32,
    pub kind: BackendKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtlasStorageRuntimeRegisterError {
    BackendLimitReached,
    NonContiguousBackendId { expected: u8, provided: u8 },
}

#[derive(Debug)]
struct BackendBinding {
    layout: AtlasLayout,
    kind: BackendKind,
    format: wgpu::TextureFormat,
    texture2d_array: wgpu::Texture,
}

#[derive(Debug)]
pub struct AtlasStorageRuntime {
    backends: Vec<BackendBinding>,
}

#[derive(Debug, Clone, Copy)]
pub struct AtlasTextureConfig<'a> {
    pub label: Option<&'a str>,
    pub format: wgpu::TextureFormat,
    pub usage: wgpu::TextureUsages,
    pub mip_level_count: u32,
    pub sample_count: u32,
}

fn default_usage_for_kind(kind: BackendKind) -> wgpu::TextureUsages {
    match kind {
        BackendKind::Leaf => {
            wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::RENDER_ATTACHMENT
        }
        BackendKind::BranchCache => {
            wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT
        }
    }
}

impl Default for AtlasTextureConfig<'_> {
    fn default() -> Self {
        Self {
            label: None,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::empty(),
            mip_level_count: 1,
            sample_count: 1,
        }
    }
}

impl Default for AtlasStorageRuntime {
    fn default() -> Self {
        Self {
            backends: Vec::new(),
        }
    }
}

impl AtlasStorageRuntime {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            backends: Vec::with_capacity(capacity),
        }
    }

    pub fn create_backend(
        &mut self,
        device: &wgpu::Device,
        backend_id: u8,
        kind: BackendKind,
        layout: AtlasLayout,
        config: AtlasTextureConfig<'_>,
    ) -> Result<(), AtlasStorageRuntimeRegisterError> {
        self.validate_backend_id(backend_id)?;
        let edge_size = layout.tiles_per_edge() * ATLAS_TILE_SIZE;
        let usage = if config.usage.is_empty() {
            default_usage_for_kind(kind)
        } else {
            config.usage
        };
        let texture2d_array = device.create_texture(&wgpu::TextureDescriptor {
            label: config.label,
            size: wgpu::Extent3d {
                width: edge_size,
                height: edge_size,
                depth_or_array_layers: layout.layers(),
            },
            mip_level_count: config.mip_level_count,
            sample_count: config.sample_count,
            dimension: wgpu::TextureDimension::D2,
            format: config.format,
            usage,
            view_formats: &[],
        });

        self.backends.push(BackendBinding {
            layout,
            kind,
            format: config.format,
            texture2d_array,
        });
        Ok(())
    }

    pub fn resolve(&self, key: TileKey) -> Option<AtlasResolvedAddress<'_>> {
        let backend_index = key.backend_index() as usize;
        let backend = self.backends.get(backend_index)?;
        let address = build_address(backend.layout, key.slot_index());
        Some(AtlasResolvedAddress {
            texture2d_array: &backend.texture2d_array,
            format: backend.format,
            address,
        })
    }

    pub fn backend_resource(&self, backend_id: u8) -> Option<AtlasBackendResource<'_>> {
        let backend = self.backends.get(backend_id as usize)?;
        Some(AtlasBackendResource {
            texture2d_array: &backend.texture2d_array,
            format: backend.format,
            layers: backend.layout.layers(),
            kind: backend.kind,
        })
    }

    fn validate_backend_id(&self, backend_id: u8) -> Result<(), AtlasStorageRuntimeRegisterError> {
        let expected = u8::try_from(self.backends.len())
            .map_err(|_| AtlasStorageRuntimeRegisterError::BackendLimitReached)?;
        if backend_id != expected {
            return Err(AtlasStorageRuntimeRegisterError::NonContiguousBackendId {
                expected,
                provided: backend_id,
            });
        }
        Ok(())
    }
}

fn build_address(layout: AtlasLayout, slot: u32) -> AtlasAddress {
    let edge_bits = layout.tiles_per_edge_bits();
    let layer_bits = edge_bits * 2;
    let layer = slot >> layer_bits;
    let index_mask = (1u32 << layer_bits) - 1;
    let index = slot & index_mask;
    let y = index >> edge_bits;
    let x = index & ((1u32 << edge_bits) - 1);

    let tile_offset = (x, y);
    AtlasAddress {
        layer,
        tile_offset,
        texel_offset: (
            tile_offset.0.saturating_mul(ATLAS_TILE_SIZE),
            tile_offset.1.saturating_mul(ATLAS_TILE_SIZE),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::{AtlasStorageRuntime, AtlasStorageRuntimeRegisterError};
    use glaphica_core::AtlasLayout;
    use glaphica_core::TileKey;
    use glaphica_core::ATLAS_TILE_SIZE;

    #[test]
    fn validate_backend_requires_contiguous_backend_ids() {
        let runtime = AtlasStorageRuntime::new();
        let err = runtime.validate_backend_id(1);
        assert_eq!(
            err,
            Err(AtlasStorageRuntimeRegisterError::NonContiguousBackendId {
                expected: 0,
                provided: 1
            })
        );
        assert!(runtime.validate_backend_id(0).is_ok());
    }

    #[test]
    fn build_address_maps_slot_to_layer_and_offsets() {
        let x = 3;
        let y = 7;
        let layer = 1;
        let edge_bits = AtlasLayout::Small11.tiles_per_edge_bits();
        let layer_bits = edge_bits * 2;
        let slot = (layer << layer_bits) | (y << edge_bits) | x;
        let address = super::build_address(AtlasLayout::Small11, slot);

        assert_eq!(address.layer, layer);
        assert_eq!(address.tile_offset, (x, y));
        assert_eq!(
            address.texel_offset,
            (x * ATLAS_TILE_SIZE, y * ATLAS_TILE_SIZE)
        );
    }

    #[test]
    fn resolve_returns_none_for_missing_backend() {
        let runtime = AtlasStorageRuntime::new();
        let key = TileKey::from_parts(0, 0, 0);
        assert!(runtime.resolve(key).is_none());
    }
}
