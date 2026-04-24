pub mod atlas_texture_set;
pub mod brush_encode;
pub mod composite;
pub mod present;
pub mod types;

pub use atlas_texture_set::AtlasTextureStage;
pub use brush_encode::BrushEncodeStage;
pub use types::{
    ApplyDabBlend, ApplyDabCommand, ApplyDabShaderValidation, ApplyDabShaderVariant,
    BrushCommandExecutor, BrushIntermediateFormat, BrushShaderProvider, BrushShaderSource,
    BrushShaderSpec, BrushShaderStage, CompositeTileCommand, CopyTileCommand, MergeTileCommand,
    PresentTileCommand, PresentTileParams, RenderCommand, RenderTarget2d, TileCompositeSource,
    TileRendererError,
};

use atlas::TileKey;

use composite::CompositeStage;
use present::PresentStage;

pub struct TileRenderer {
    atlas_texture_set: AtlasTextureStage,
    composite: CompositeStage,
    brush_encode: BrushEncodeStage,
    present: PresentStage,
}

impl TileRenderer {
    pub fn new(device: &wgpu::Device) -> Result<Self, TileRendererError> {
        Ok(Self {
            atlas_texture_set: AtlasTextureStage::new(),
            composite: CompositeStage::new(device)?,
            brush_encode: BrushEncodeStage::new(device)?,
            present: PresentStage::new(device),
        })
    }

    pub fn ensure_backend(
        &mut self,
        device: &wgpu::Device,
        backend: &atlas::Backend,
    ) -> Result<(), TileRendererError> {
        self.atlas_texture_set.ensure_backend(device, backend)
    }

    pub fn ensure_backend_with_format(
        &mut self,
        device: &wgpu::Device,
        backend: &atlas::Backend,
        format: BrushIntermediateFormat,
    ) -> Result<(), TileRendererError> {
        self.atlas_texture_set
            .ensure_backend_with_format(device, backend, format)
    }

    pub fn apply_clear_batches(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        backends: &[&atlas::Backend],
        clear_batches: &[atlas::ClearBatch],
    ) -> Result<(), TileRendererError> {
        self.atlas_texture_set
            .apply_clear_batches(device, queue, backends, clear_batches)
    }

    pub fn execute_commands(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        backends: &[&atlas::Backend],
        clear_batches: &[atlas::ClearBatch],
        commands: &[RenderCommand],
        present_target: Option<RenderTarget2d<'_>>,
    ) -> Result<(), TileRendererError> {
        struct UnsupportedBrushExecutor;

        impl BrushCommandExecutor for UnsupportedBrushExecutor {
            fn apply_dab(
                &mut self,
                _atlas_texture_set: &AtlasTextureStage,
                _brush_encode: &mut BrushEncodeStage,
                _device: &wgpu::Device,
                _queue: &wgpu::Queue,
                _encoder: &mut wgpu::CommandEncoder,
                _command: &ApplyDabCommand,
            ) -> Result<(), TileRendererError> {
                Err(TileRendererError::UnsupportedCommand("ApplyDab"))
            }

            fn merge_tile(
                &mut self,
                _atlas_texture_set: &AtlasTextureStage,
                _brush_encode: &mut BrushEncodeStage,
                _device: &wgpu::Device,
                _queue: &wgpu::Queue,
                _encoder: &mut wgpu::CommandEncoder,
                _command: &MergeTileCommand,
            ) -> Result<(), TileRendererError> {
                Err(TileRendererError::UnsupportedCommand("MergeTile"))
            }
        }

        let mut brush_executor = UnsupportedBrushExecutor;
        self.execute_commands_with_brush_executor(
            device,
            queue,
            backends,
            clear_batches,
            commands,
            present_target,
            &mut brush_executor,
        )
    }

    pub fn execute_commands_with_brush_executor(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        backends: &[&atlas::Backend],
        clear_batches: &[atlas::ClearBatch],
        commands: &[RenderCommand],
        present_target: Option<RenderTarget2d<'_>>,
        brush_executor: &mut impl BrushCommandExecutor,
    ) -> Result<(), TileRendererError> {
        for backend in backends {
            self.ensure_backend(device, backend)?;
        }
        self.apply_clear_batches(device, queue, backends, clear_batches)?;
        if commands.is_empty() {
            return Ok(());
        }
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("glaphica-render-command-batch-encoder"),
        });
        for command in commands {
            match command {
                RenderCommand::CopyTile(command) => self.atlas_texture_set.encode_copy_tile(
                    &mut encoder,
                    command.source_tile_key,
                    command.destination_tile_key,
                )?,
                RenderCommand::ApplyDab(command) => brush_executor.apply_dab(
                    &self.atlas_texture_set,
                    &mut self.brush_encode,
                    device,
                    queue,
                    &mut encoder,
                    command,
                )?,
                RenderCommand::MergeTile(command) => brush_executor.merge_tile(
                    &self.atlas_texture_set,
                    &mut self.brush_encode,
                    device,
                    queue,
                    &mut encoder,
                    command,
                )?,
                RenderCommand::CompositeTile(command) => self.composite.encode_composite_tile(
                    device,
                    queue,
                    &mut encoder,
                    &self.atlas_texture_set,
                    command.target_tile_key,
                    &command.inputs,
                )?,
                RenderCommand::PresentTile(command) => {
                    let target = present_target.ok_or(TileRendererError::MissingPresentTarget)?;
                    self.present.encode_present_tile(
                        device,
                        queue,
                        &mut encoder,
                        &self.atlas_texture_set,
                        command.source_tile_key,
                        command.params,
                        target,
                    )?;
                }
            }
        }
        queue.submit(Some(encoder.finish()));
        Ok(())
    }

    pub fn execute_commands_with_shader_provider(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        backends: &[&atlas::Backend],
        clear_batches: &[atlas::ClearBatch],
        commands: &[RenderCommand],
        present_target: Option<RenderTarget2d<'_>>,
        provider: &impl BrushShaderProvider,
    ) -> Result<(), TileRendererError> {
        for command in commands {
            let (brush_id, tile_key) = match command {
                RenderCommand::ApplyDab(command) => {
                    (command.brush_id, command.destination_tile_key)
                }
                RenderCommand::MergeTile(command) => {
                    (command.brush_id, command.intermediate_tile_key)
                }
                _ => continue,
            };
            let shader_spec =
                provider
                    .shader_spec(brush_id)
                    .ok_or(TileRendererError::MissingBrushShader {
                        brush_id,
                        stage: BrushShaderStage::ApplyDab,
                    })?;
            let backend_id = tile_key.parts().backend_id;
            if let Some(backend) = backends
                .iter()
                .copied()
                .find(|backend| backend.backend_id().ok() == Some(backend_id))
            {
                self.ensure_backend_with_format(device, backend, shader_spec.intermediate_format)?;
            }
        }
        let mut executor = RegisteredBrushExecutor { provider };
        self.execute_commands_with_brush_executor(
            device,
            queue,
            backends,
            clear_batches,
            commands,
            present_target,
            &mut executor,
        )
    }

    pub fn upload_rgba8_tile(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        backend: &atlas::Backend,
        tile_key: TileKey,
        pixels_rgba8: &[u8],
    ) -> Result<(), TileRendererError> {
        self.atlas_texture_set
            .upload_rgba8_tile(device, queue, backend, tile_key, pixels_rgba8)
    }

    pub fn clear_render_target(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target: RenderTarget2d<'_>,
        color: wgpu::Color,
    ) {
        self.present
            .clear_render_target(device, queue, target, color);
    }

    pub fn composite_tile(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target_tile_key: TileKey,
        inputs: &[TileCompositeSource],
    ) -> Result<(), TileRendererError> {
        self.composite.composite_tile(
            device,
            queue,
            &self.atlas_texture_set,
            target_tile_key,
            inputs,
        )
    }

    pub fn copy_tile(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        source_tile_key: TileKey,
        destination_tile_key: TileKey,
    ) -> Result<(), TileRendererError> {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("glaphica-tile-copy-encoder"),
        });
        self.atlas_texture_set.encode_copy_tile(
            &mut encoder,
            source_tile_key,
            destination_tile_key,
        )?;
        queue.submit(Some(encoder.finish()));
        Ok(())
    }

    pub fn present_tile(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        source_tile_key: TileKey,
        params: PresentTileParams,
        target: RenderTarget2d<'_>,
    ) -> Result<(), TileRendererError> {
        self.present.present_tile(
            device,
            queue,
            &self.atlas_texture_set,
            source_tile_key,
            params,
            target,
        )
    }

    pub fn present_texture_2d(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        source: &crate::RendererTexture,
        target: RenderTarget2d<'_>,
    ) -> Result<(), TileRendererError> {
        self.present
            .present_texture_2d(device, queue, source, target)
    }
}

struct RegisteredBrushExecutor<'a, Provider> {
    provider: &'a Provider,
}

impl<Provider> BrushCommandExecutor for RegisteredBrushExecutor<'_, Provider>
where
    Provider: BrushShaderProvider,
{
    fn apply_dab(
        &mut self,
        atlas_texture_set: &AtlasTextureStage,
        brush_encode: &mut BrushEncodeStage,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        command: &ApplyDabCommand,
    ) -> Result<(), TileRendererError> {
        brush_encode.encode_apply_dab(
            device,
            queue,
            encoder,
            atlas_texture_set,
            self.provider,
            command,
        )
    }

    fn merge_tile(
        &mut self,
        atlas_texture_set: &AtlasTextureStage,
        brush_encode: &mut BrushEncodeStage,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        command: &MergeTileCommand,
    ) -> Result<(), TileRendererError> {
        brush_encode.encode_merge_tile(
            device,
            queue,
            encoder,
            atlas_texture_set,
            self.provider,
            command,
        )
    }
}

#[cfg(test)]
mod tests {
    use atlas::TileKey;
    use bytemuck::bytes_of;

    use super::{PresentTileParams, TileCompositeSource, present::PresentUniforms};
    use glaphica_core::BlendMode;

    #[test]
    fn composite_source_keeps_opacity_and_blend_mode() {
        let source = TileCompositeSource {
            tile_key: TileKey::EMPTY,
            opacity: 0.5,
            blend_mode: BlendMode::Multiply,
        };
        assert_eq!(source.opacity, 0.5);
        assert_eq!(source.blend_mode, BlendMode::Multiply);
    }

    #[test]
    fn present_params_store_logical_tile_rect() {
        let params = PresentTileParams {
            target_min_px: [10.0, 20.0],
            target_max_px: [40.0, 50.0],
            source_width: 32,
            source_height: 16,
        };
        assert_eq!(params.source_width, 32);
        assert_eq!(params.target_max_px, [40.0, 50.0]);
    }

    #[test]
    fn present_uniform_source_size_matches_wgsl_offsets() {
        let uniform = PresentUniforms {
            dst_min_ndc: [0.0, 0.0],
            dst_max_ndc: [1.0, 1.0],
            source_origin: [11, 13],
            source_layer: 17,
            _pad0: 0,
            source_size: [62, 31],
            padding: [0; 6],
        };
        let bytes = bytes_of(&uniform);
        let source_width = match <[u8; 4]>::try_from(&bytes[32..36]) {
            Ok(bytes) => u32::from_ne_bytes(bytes),
            Err(_) => panic!("present uniform width slice size mismatch"),
        };
        let source_height = match <[u8; 4]>::try_from(&bytes[36..40]) {
            Ok(bytes) => u32::from_ne_bytes(bytes),
            Err(_) => panic!("present uniform height slice size mismatch"),
        };
        assert_eq!(source_width, 62);
        assert_eq!(source_height, 31);
    }
}
