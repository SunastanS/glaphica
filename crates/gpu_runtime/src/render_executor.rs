use std::error::Error;
use std::fmt::{Display, Formatter};

use document::{LeafBlendMode, RenderCmd};
use glaphica_core::{TileKey, ATLAS_TILE_SIZE};

use crate::atlas_runtime::{AtlasResolvedAddress, AtlasStorageRuntime};
use crate::context::GpuContext;

#[derive(Debug)]
pub enum RenderExecutorError {
    MissingTileBackend { tile_key: TileKey },
}

impl Display for RenderExecutorError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingTileBackend { tile_key } => {
                write!(f, "missing atlas backend for tile key {:?}", tile_key)
            }
        }
    }
}

impl Error for RenderExecutorError {}

pub struct RenderContext<'a> {
    pub gpu_context: &'a GpuContext,
    pub atlas_storage: &'a AtlasStorageRuntime,
}

struct PipelineCache {
    normal: wgpu::RenderPipeline,
    multiply: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    params_buffer: wgpu::Buffer,
}

pub struct RenderExecutor {
    cache: Option<PipelineCache>,
}

impl RenderExecutor {
    pub fn new() -> Self {
        Self { cache: None }
    }

    pub fn execute(
        &mut self,
        context: &mut RenderContext<'_>,
        cmds: &[RenderCmd],
    ) -> Result<(), RenderExecutorError> {
        if cmds.is_empty() {
            return Ok(());
        }

        let format = self.detect_format(context, cmds);
        self.ensure_pipelines(context, format);
        let cache = self.cache.as_ref().unwrap();

        for cmd in cmds {
            execute_cmd(context, cmd, cache)?;
        }
        Ok(())
    }

    fn detect_format(
        &self,
        context: &RenderContext<'_>,
        cmds: &[RenderCmd],
    ) -> wgpu::TextureFormat {
        for cmd in cmds {
            if let Some(dst_tile_key) = cmd.to.first() {
                if let Some(resolved) = context.atlas_storage.resolve(*dst_tile_key) {
                    return resolved.format;
                }
            }
        }
        wgpu::TextureFormat::Rgba8Unorm
    }

    fn ensure_pipelines(&mut self, context: &mut RenderContext<'_>, format: wgpu::TextureFormat) {
        if let Some(_) = &self.cache {
            return;
        }

        let device = &context.gpu_context.device;

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("glaphica-render-bind-group-layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
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
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("glaphica-render-pipeline-layout"),
            bind_group_layouts: &[&bind_group_layout],
            immediate_size: 0,
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("glaphica-render-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("render_shader.wgsl").into()),
        });

        let normal = Self::create_pipeline(
            device,
            &pipeline_layout,
            &shader,
            format,
            LeafBlendMode::Normal,
        );
        let multiply = Self::create_pipeline(
            device,
            &pipeline_layout,
            &shader,
            format,
            LeafBlendMode::Multiply,
        );

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("glaphica-render-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        let params_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("glaphica-render-params"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        self.cache = Some(PipelineCache {
            normal,
            multiply,
            bind_group_layout,
            sampler,
            params_buffer,
        });
    }

    fn create_pipeline(
        device: &wgpu::Device,
        layout: &wgpu::PipelineLayout,
        shader: &wgpu::ShaderModule,
        format: wgpu::TextureFormat,
        blend_mode: LeafBlendMode,
    ) -> wgpu::RenderPipeline {
        let (blend, fs_entry) = match blend_mode {
            LeafBlendMode::Normal => (wgpu::BlendState::ALPHA_BLENDING, "fs_normal"),
            LeafBlendMode::Multiply => (
                wgpu::BlendState {
                    color: wgpu::BlendComponent {
                        src_factor: wgpu::BlendFactor::Dst,
                        dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                        operation: wgpu::BlendOperation::Add,
                    },
                    alpha: wgpu::BlendComponent::REPLACE,
                },
                "fs_multiply",
            ),
        };

        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(&format!("glaphica-render-pipeline-{:?}", blend_mode)),
            layout: Some(layout),
            vertex: wgpu::VertexState {
                module: shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: shader,
                entry_point: Some(fs_entry),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(blend),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        })
    }
}

fn execute_cmd(
    context: &mut RenderContext<'_>,
    cmd: &RenderCmd,
    cache: &PipelineCache,
) -> Result<(), RenderExecutorError> {
    if cmd.to.is_empty() || cmd.from.is_empty() {
        return Ok(());
    }

    let mut encoder =
        context
            .gpu_context
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("glaphica-render-cmd-encoder"),
            });

    for (tile_idx, &dst_tile_key) in cmd.to.iter().enumerate() {
        if dst_tile_key == TileKey::EMPTY {
            continue;
        }

        let dst_resolved = context.atlas_storage.resolve(dst_tile_key).ok_or(
            RenderExecutorError::MissingTileBackend {
                tile_key: dst_tile_key,
            },
        )?;

        let mut bind_groups: Vec<(wgpu::BindGroup, LeafBlendMode)> = Vec::new();
        for source in &cmd.from {
            if tile_idx >= source.tile_keys.len() {
                continue;
            }
            let src_tile_key = source.tile_keys[tile_idx];
            if src_tile_key == TileKey::EMPTY {
                continue;
            }

            let bind_group = create_bind_group(
                context,
                &cache.bind_group_layout,
                &cache.sampler,
                &cache.params_buffer,
                src_tile_key,
                source.config.opacity,
            );
            bind_groups.push((bind_group, source.config.blend_mode));
        }

        if bind_groups.is_empty() {
            continue;
        }

        let dst_view = create_render_attachment_view(&dst_resolved);

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("glaphica-render-composite-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &dst_view,
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

            for (bind_group, blend_mode) in &bind_groups {
                let pipeline = match blend_mode {
                    LeafBlendMode::Normal => &cache.normal,
                    LeafBlendMode::Multiply => &cache.multiply,
                };
                pass.set_pipeline(pipeline);
                pass.set_bind_group(0, bind_group, &[]);
                pass.set_scissor_rect(
                    dst_resolved.address.texel_offset.0,
                    dst_resolved.address.texel_offset.1,
                    ATLAS_TILE_SIZE,
                    ATLAS_TILE_SIZE,
                );
                pass.draw(0..3, 0..1);
            }
        }
    }

    context.gpu_context.queue.submit(Some(encoder.finish()));
    Ok(())
}

fn create_bind_group(
    context: &RenderContext<'_>,
    layout: &wgpu::BindGroupLayout,
    sampler: &wgpu::Sampler,
    params_buffer: &wgpu::Buffer,
    src_tile_key: TileKey,
    opacity: f32,
) -> wgpu::BindGroup {
    let src_resolved = context.atlas_storage.resolve(src_tile_key).unwrap();

    let src_view = src_resolved
        .texture2d_array
        .create_view(&wgpu::TextureViewDescriptor {
            label: Some("glaphica-render-src-view"),
            format: Some(src_resolved.format),
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            usage: Some(wgpu::TextureUsages::TEXTURE_BINDING),
            aspect: wgpu::TextureAspect::All,
            base_mip_level: 0,
            mip_level_count: Some(1),
            base_array_layer: 0,
            array_layer_count: None,
        });

    let params = RenderParams {
        src_layer: src_resolved.address.layer,
        src_x: src_resolved.address.texel_offset.0,
        src_y: src_resolved.address.texel_offset.1,
        opacity,
    };
    let params_bytes: [u8; 16] = params.encode();

    context
        .gpu_context
        .queue
        .write_buffer(params_buffer, 0, &params_bytes);

    context
        .gpu_context
        .device
        .create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("glaphica-render-bind-group"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&src_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: params_buffer.as_entire_binding(),
                },
            ],
        })
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct RenderParams {
    src_layer: u32,
    src_x: u32,
    src_y: u32,
    opacity: f32,
}

impl RenderParams {
    fn encode(&self) -> [u8; 16] {
        let mut bytes = [0u8; 16];
        bytes[0..4].copy_from_slice(&self.src_layer.to_ne_bytes());
        bytes[4..8].copy_from_slice(&self.src_x.to_ne_bytes());
        bytes[8..12].copy_from_slice(&self.src_y.to_ne_bytes());
        bytes[12..16].copy_from_slice(&self.opacity.to_ne_bytes());
        bytes
    }
}

fn create_render_attachment_view(resolved: &AtlasResolvedAddress<'_>) -> wgpu::TextureView {
    resolved
        .texture2d_array
        .create_view(&wgpu::TextureViewDescriptor {
            label: Some("glaphica-render-attachment-view"),
            format: Some(resolved.format),
            dimension: Some(wgpu::TextureViewDimension::D2),
            usage: Some(wgpu::TextureUsages::RENDER_ATTACHMENT),
            aspect: wgpu::TextureAspect::All,
            base_mip_level: 0,
            mip_level_count: Some(1),
            base_array_layer: resolved.address.layer,
            array_layer_count: Some(1),
        })
}
