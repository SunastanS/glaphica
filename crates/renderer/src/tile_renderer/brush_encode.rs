use glaphica_core::{ATLAS_TILE_SIZE, BrushId};

use super::atlas_texture_set::AtlasTextureStage;
use super::types::{
    ApplyDabBlend, ApplyDabCommand, ApplyDabShaderValidation, ApplyDabShaderVariant,
    BrushShaderProvider, BrushShaderSource, BrushShaderSpec, BrushShaderStage, BrushTileFormat,
    MergeTileCommand, TileRendererError,
};

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ApplyDabUniformHeader {
    tile_origin_x: u32,
    tile_origin_y: u32,
    source_origin_x: u32,
    source_origin_y: u32,
    source_layer: u32,
    _pad0: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MergeTileUniformHeader {
    origin_origin: [u32; 2],
    origin_layer: u32,
    _pad0: u32,
    brush_tile_origin: [u32; 2],
    brush_tile_layer: u32,
    _pad1: u32,
}

#[derive(Debug, Default)]
struct BrushPipelineSet {
    apply_dab: Vec<Vec<Option<wgpu::RenderPipeline>>>,
    merge_tile: Vec<Option<wgpu::RenderPipeline>>,
}

pub struct BrushEncodeStage {
    bind_group_layout: wgpu::BindGroupLayout,
    pipelines: BrushPipelineSet,
    scratch_a: crate::RendererTexture,
    scratch_b: crate::RendererTexture,
    scratch_r16: crate::RendererTexture,
}

impl BrushEncodeStage {
    pub fn new(device: &wgpu::Device) -> Result<Self, TileRendererError> {
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("glaphica-brush-bind-group-layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        Ok(Self {
            bind_group_layout,
            pipelines: BrushPipelineSet::default(),
            scratch_a: create_scratch_texture(
                device,
                "glaphica-brush-scratch-a",
                BrushTileFormat::Rgba8Unorm,
            )?,
            scratch_b: create_scratch_texture(
                device,
                "glaphica-brush-scratch-b",
                BrushTileFormat::Rgba8Unorm,
            )?,
            scratch_r16: create_scratch_texture(
                device,
                "glaphica-brush-scratch-r16",
                BrushTileFormat::R16Float,
            )?,
        })
    }

    fn apply_scratch(&self, format: BrushTileFormat) -> &crate::RendererTexture {
        match format {
            BrushTileFormat::Rgba8Unorm => &self.scratch_b,
            BrushTileFormat::R16Float => &self.scratch_r16,
        }
    }

    fn build_brush_buffer(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        label: &'static str,
        bytes: &[u8],
        usage: wgpu::BufferUsages,
    ) -> wgpu::Buffer {
        let size = bytes.len().max(4) as u64;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size,
            usage: usage | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        if !bytes.is_empty() {
            queue.write_buffer(&buffer, 0, bytes);
        }
        buffer
    }

    pub fn ensure_brush_pipeline(
        &mut self,
        device: &wgpu::Device,
        provider: &impl BrushShaderProvider,
        brush_id: BrushId,
        stage: BrushShaderStage,
    ) -> Result<(), TileRendererError> {
        let index = brush_id.raw() as usize;
        let shader_spec = provider
            .shader_spec(brush_id)
            .ok_or(TileRendererError::MissingBrushShader { brush_id, stage })?;
        match stage {
            BrushShaderStage::ApplyDab => {
                for (variant_index, variant) in
                    shader_spec.apply_dab_variants.iter().copied().enumerate()
                {
                    if variant.validation != ApplyDabShaderValidation::Always {
                        continue;
                    }
                    self.ensure_apply_dab_variant_pipeline(
                        device,
                        shader_spec,
                        brush_id,
                        variant_index,
                    )?;
                }
                Ok(())
            }
            BrushShaderStage::MergeTile => {
                let pipeline_slots = &mut self.pipelines.merge_tile;
                if pipeline_slots.len() <= index {
                    pipeline_slots.resize_with(index + 1, || None);
                }
                if pipeline_slots[index].is_some() {
                    return Ok(());
                }

                let pipeline = Self::create_brush_pipeline(
                    device,
                    &self.bind_group_layout,
                    shader_spec.merge_tile,
                    wgpu::TextureFormat::Rgba8Unorm,
                    None,
                    "glaphica-brush-pipeline",
                );
                pipeline_slots[index] = Some(pipeline);
                Ok(())
            }
        }
    }

    fn ensure_apply_dab_variant_pipeline(
        &mut self,
        device: &wgpu::Device,
        shader_spec: BrushShaderSpec,
        brush_id: BrushId,
        variant_index: usize,
    ) -> Result<(), TileRendererError> {
        let variant = shader_spec.apply_dab_variants.get(variant_index).ok_or(
            TileRendererError::MissingBrushShader {
                brush_id,
                stage: BrushShaderStage::ApplyDab,
            },
        )?;
        let brush_index = brush_id.raw() as usize;
        if self.pipelines.apply_dab.len() <= brush_index {
            self.pipelines
                .apply_dab
                .resize_with(brush_index + 1, Vec::new);
        }
        let pipeline_slots = self.pipelines.apply_dab.get_mut(brush_index).ok_or(
            TileRendererError::MissingBrushShader {
                brush_id,
                stage: BrushShaderStage::ApplyDab,
            },
        )?;
        if pipeline_slots.len() <= variant_index {
            pipeline_slots.resize_with(variant_index + 1, || None);
        }
        if pipeline_slots[variant_index].is_some() {
            return Ok(());
        }
        let target_format = map_brush_tile_format(shader_spec.brush_tile_format);
        let pipeline = Self::create_brush_pipeline(
            device,
            &self.bind_group_layout,
            variant.source,
            target_format,
            blend_state_for_apply_dab_variant(*variant),
            "glaphica-brush-apply-pipeline",
        );
        pipeline_slots[variant_index] = Some(pipeline);
        Ok(())
    }

    fn create_brush_pipeline(
        device: &wgpu::Device,
        bind_group_layout: &wgpu::BindGroupLayout,
        shader_source: BrushShaderSource,
        target_format: wgpu::TextureFormat,
        blend: Option<wgpu::BlendState>,
        label: &'static str,
    ) -> wgpu::RenderPipeline {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("glaphica-brush-shader"),
            source: wgpu::ShaderSource::Wgsl(shader_source.wgsl.into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("glaphica-brush-pipeline-layout"),
            bind_group_layouts: &[bind_group_layout],
            immediate_size: 0,
        });
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(label),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some(shader_source.entry_point),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: target_format,
                    blend,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        })
    }

    fn brush_pipeline(
        &mut self,
        device: &wgpu::Device,
        provider: &impl BrushShaderProvider,
        brush_id: BrushId,
        stage: BrushShaderStage,
    ) -> Result<wgpu::RenderPipeline, TileRendererError> {
        self.ensure_brush_pipeline(device, provider, brush_id, stage)?;
        match stage {
            BrushShaderStage::ApplyDab => self.apply_dab_variant_pipeline(
                device,
                provider
                    .shader_spec(brush_id)
                    .ok_or(TileRendererError::MissingBrushShader { brush_id, stage })?,
                brush_id,
                0,
            ),
            BrushShaderStage::MergeTile => self
                .pipelines
                .merge_tile
                .get(brush_id.raw() as usize)
                .and_then(Option::as_ref)
                .cloned()
                .ok_or(TileRendererError::MissingBrushShader { brush_id, stage }),
        }
    }

    fn apply_dab_variant_pipeline(
        &mut self,
        device: &wgpu::Device,
        shader_spec: BrushShaderSpec,
        brush_id: BrushId,
        variant_index: usize,
    ) -> Result<wgpu::RenderPipeline, TileRendererError> {
        self.ensure_apply_dab_variant_pipeline(device, shader_spec, brush_id, variant_index)?;
        self.pipelines
            .apply_dab
            .get(brush_id.raw() as usize)
            .and_then(|pipelines| pipelines.get(variant_index))
            .and_then(Option::as_ref)
            .cloned()
            .ok_or(TileRendererError::MissingBrushShader {
                brush_id,
                stage: BrushShaderStage::ApplyDab,
            })
    }

    pub fn encode_apply_dab(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        atlas_texture_set: &AtlasTextureStage,
        provider: &impl BrushShaderProvider,
        command: &ApplyDabCommand,
    ) -> Result<(), TileRendererError> {
        let destination = atlas_texture_set.resolve_tile(command.destination_tile_key)?;
        let shader_spec = provider.shader_spec(command.brush_id).ok_or(
            TileRendererError::MissingBrushShader {
                brush_id: command.brush_id,
                stage: BrushShaderStage::ApplyDab,
            },
        )?;
        for (variant_index, variant) in shader_spec.apply_dab_variants.iter().copied().enumerate() {
            if !self.apply_dab_variant_is_supported(device, shader_spec, variant, command) {
                continue;
            }
            let pipeline = self.apply_dab_variant_pipeline(
                device,
                shader_spec,
                command.brush_id,
                variant_index,
            )?;
            match variant.blend {
                ApplyDabBlend::Replace => {
                    return self.encode_apply_dab_read_modify_write(
                        device,
                        queue,
                        encoder,
                        atlas_texture_set,
                        shader_spec,
                        &pipeline,
                        &destination,
                        command,
                    );
                }
                ApplyDabBlend::LinearAdd => {
                    return self.encode_apply_dab_direct_linear_add(
                        device,
                        queue,
                        encoder,
                        &pipeline,
                        &destination,
                        command,
                    );
                }
            }
        }

        Err(TileRendererError::MissingBrushShader {
            brush_id: command.brush_id,
            stage: BrushShaderStage::ApplyDab,
        })
    }

    fn encode_apply_dab_read_modify_write(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        atlas_texture_set: &AtlasTextureStage,
        shader_spec: BrushShaderSpec,
        pipeline: &wgpu::RenderPipeline,
        destination: &super::atlas_texture_set::ResolvedAtlasTile<'_>,
        command: &ApplyDabCommand,
    ) -> Result<(), TileRendererError> {
        let scratch = self.apply_scratch(shader_spec.brush_tile_format);
        let source_tile_key = command
            .source_tile_key
            .unwrap_or(command.destination_tile_key);
        let source = atlas_texture_set.resolve_tile(source_tile_key)?;
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
                texture: &scratch.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width: ATLAS_TILE_SIZE,
                height: ATLAS_TILE_SIZE,
                depth_or_array_layers: 1,
            },
        );
        let uniform_header = ApplyDabUniformHeader {
            tile_origin_x: destination.tile_x * ATLAS_TILE_SIZE,
            tile_origin_y: destination.tile_y * ATLAS_TILE_SIZE,
            source_origin_x: 0,
            source_origin_y: 0,
            source_layer: 0,
            _pad0: 0,
        };
        let uniform_buffer = Self::build_brush_buffer(
            device,
            queue,
            "glaphica-brush-apply-uniform",
            bytemuck::bytes_of(&uniform_header),
            wgpu::BufferUsages::UNIFORM,
        );
        let payload_buffer = Self::build_brush_buffer(
            device,
            queue,
            "glaphica-brush-apply-payload",
            &command.brush_payload,
            wgpu::BufferUsages::STORAGE,
        );
        let destination_view = destination.texture.create_layer_view(destination.layer)?;
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("glaphica-brush-apply-bind-group"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&scratch.view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&scratch.view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: uniform_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: payload_buffer.as_entire_binding(),
                },
            ],
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("glaphica-brush-apply-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &destination_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.set_scissor_rect(
                destination.tile_x * ATLAS_TILE_SIZE,
                destination.tile_y * ATLAS_TILE_SIZE,
                ATLAS_TILE_SIZE,
                ATLAS_TILE_SIZE,
            );
            pass.draw(0..3, 0..1);
        }
        Ok(())
    }

    fn apply_dab_variant_is_supported(
        &self,
        device: &wgpu::Device,
        shader_spec: BrushShaderSpec,
        variant: ApplyDabShaderVariant,
        command: &ApplyDabCommand,
    ) -> bool {
        match variant.validation {
            ApplyDabShaderValidation::Always => true,
            ApplyDabShaderValidation::LinearAddBlend => {
                let source_matches_destination = command
                    .source_tile_key
                    .is_none_or(|source_tile_key| source_tile_key == command.destination_tile_key);
                if !source_matches_destination {
                    return false;
                }

                let format = map_brush_tile_format(shader_spec.brush_tile_format);
                format
                    .guaranteed_format_features(device.features())
                    .flags
                    .contains(wgpu::TextureFormatFeatureFlags::BLENDABLE)
            }
        }
    }

    fn encode_apply_dab_direct_linear_add(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        pipeline: &wgpu::RenderPipeline,
        destination: &super::atlas_texture_set::ResolvedAtlasTile<'_>,
        command: &ApplyDabCommand,
    ) -> Result<(), TileRendererError> {
        let uniform_header = ApplyDabUniformHeader {
            tile_origin_x: destination.tile_x * ATLAS_TILE_SIZE,
            tile_origin_y: destination.tile_y * ATLAS_TILE_SIZE,
            source_origin_x: 0,
            source_origin_y: 0,
            source_layer: 0,
            _pad0: 0,
        };
        let uniform_buffer = Self::build_brush_buffer(
            device,
            queue,
            "glaphica-brush-apply-linear-add-uniform",
            bytemuck::bytes_of(&uniform_header),
            wgpu::BufferUsages::UNIFORM,
        );
        let payload_buffer = Self::build_brush_buffer(
            device,
            queue,
            "glaphica-brush-apply-linear-add-payload",
            &command.brush_payload,
            wgpu::BufferUsages::STORAGE,
        );
        let destination_view = destination.texture.create_layer_view(destination.layer)?;
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("glaphica-brush-apply-linear-add-bind-group"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&self.scratch_r16.view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&self.scratch_r16.view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: uniform_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: payload_buffer.as_entire_binding(),
                },
            ],
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("glaphica-brush-apply-linear-add-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &destination_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.set_scissor_rect(
                destination.tile_x * ATLAS_TILE_SIZE,
                destination.tile_y * ATLAS_TILE_SIZE,
                ATLAS_TILE_SIZE,
                ATLAS_TILE_SIZE,
            );
            pass.draw(0..3, 0..1);
        }
        Ok(())
    }

    pub fn encode_merge_tile(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        atlas_texture_set: &AtlasTextureStage,
        provider: &impl BrushShaderProvider,
        command: &MergeTileCommand,
    ) -> Result<(), TileRendererError> {
        let pipeline = self.brush_pipeline(
            device,
            provider,
            command.brush_id,
            BrushShaderStage::MergeTile,
        )?;
        let brush_tiles = atlas_texture_set.resolve_tile(command.brush_tile_key)?;
        let destination = atlas_texture_set.resolve_tile(command.destination_tile_key)?;
        let (origin_texture_view, origin_origin, origin_layer) =
            if command.origin_tile_key.is_empty() {
                self.encode_clear_scratch_texture(encoder, false, wgpu::Color::TRANSPARENT)?;
                (&self.scratch_b.view, [0, 0], 0)
            } else {
                let origin = atlas_texture_set.resolve_tile(command.origin_tile_key)?;
                (
                    &origin.texture.view,
                    [
                        origin.tile_x * ATLAS_TILE_SIZE,
                        origin.tile_y * ATLAS_TILE_SIZE,
                    ],
                    origin.layer,
                )
            };
        let uniform_header = MergeTileUniformHeader {
            origin_origin,
            origin_layer,
            _pad0: 0,
            brush_tile_origin: [
                brush_tiles.tile_x * ATLAS_TILE_SIZE,
                brush_tiles.tile_y * ATLAS_TILE_SIZE,
            ],
            brush_tile_layer: brush_tiles.layer,
            _pad1: 0,
        };
        let uniform_buffer = Self::build_brush_buffer(
            device,
            queue,
            "glaphica-brush-merge-uniform",
            bytemuck::bytes_of(&uniform_header),
            wgpu::BufferUsages::UNIFORM,
        );
        let payload_buffer = Self::build_brush_buffer(
            device,
            queue,
            "glaphica-brush-merge-payload",
            &command.brush_payload,
            wgpu::BufferUsages::STORAGE,
        );
        let scratch_view = self.scratch_a.create_layer_view(0)?;
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("glaphica-brush-merge-bind-group"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(origin_texture_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&brush_tiles.texture.view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: uniform_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: payload_buffer.as_entire_binding(),
                },
            ],
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("glaphica-brush-merge-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &scratch_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.scratch_a.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
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

    fn encode_clear_scratch_texture(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        clear_a: bool,
        color: wgpu::Color,
    ) -> Result<(), TileRendererError> {
        let texture = if clear_a {
            &self.scratch_a
        } else {
            &self.scratch_b
        };
        let texture_view = texture.create_layer_view(0)?;
        {
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("glaphica-tile-clear-scratch-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &texture_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(color),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
        }
        Ok(())
    }
}

fn create_scratch_texture(
    device: &wgpu::Device,
    label: &'static str,
    format: BrushTileFormat,
) -> Result<crate::RendererTexture, TileRendererError> {
    let descriptor = crate::RendererTextureDescriptor {
        label: Some(label),
        width: ATLAS_TILE_SIZE,
        height: ATLAS_TILE_SIZE,
        layers: 1,
        format: map_brush_tile_format(format),
        usage: wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC
            | wgpu::TextureUsages::COPY_DST
            | wgpu::TextureUsages::RENDER_ATTACHMENT,
    };
    Ok(crate::RendererTexture::new(device, &descriptor)?)
}

fn map_brush_tile_format(format: BrushTileFormat) -> wgpu::TextureFormat {
    match format {
        BrushTileFormat::Rgba8Unorm => wgpu::TextureFormat::Rgba8Unorm,
        BrushTileFormat::R16Float => wgpu::TextureFormat::R16Float,
    }
}

fn blend_state_for_apply_dab_variant(variant: ApplyDabShaderVariant) -> Option<wgpu::BlendState> {
    match variant.blend {
        ApplyDabBlend::Replace => None,
        ApplyDabBlend::LinearAdd => Some(linear_add_blend_state()),
    }
}

fn linear_add_blend_state() -> wgpu::BlendState {
    wgpu::BlendState {
        color: wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::One,
            operation: wgpu::BlendOperation::Add,
        },
        alpha: wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::One,
            operation: wgpu::BlendOperation::Add,
        },
    }
}
