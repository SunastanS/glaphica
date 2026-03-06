use std::error::Error;
use std::fmt::{Display, Formatter};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use wgpu::util::DeviceExt;

use document::{LeafBlendMode, RenderCmd};
use glaphica_core::{ATLAS_TILE_SIZE, TileKey};
use thread_protocol::{ClearOp, CopyOp, WriteBlendMode, WriteOp};

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
}

pub struct RenderExecutor {
    cache: Option<PipelineCache>,
}

#[derive(Debug, Clone, Copy)]
struct GpuExecTraceConfig {
    enabled: bool,
    max_events: u64,
}

fn gpu_exec_trace_config() -> GpuExecTraceConfig {
    static CONFIG: OnceLock<GpuExecTraceConfig> = OnceLock::new();
    *CONFIG.get_or_init(|| {
        let enabled = std::env::var("GLAPHICA_DEBUG_GPU_EXEC_TRACE")
            .ok()
            .is_some_and(|value| value != "0");
        let max_events = std::env::var("GLAPHICA_DEBUG_GPU_EXEC_TRACE_MAX")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(400);
        GpuExecTraceConfig {
            enabled,
            max_events,
        }
    })
}

fn should_trace_gpu_exec_event() -> bool {
    static TRACE_SEQ: AtomicU64 = AtomicU64::new(0);
    let config = gpu_exec_trace_config();
    if !config.enabled {
        return false;
    }
    let seq = TRACE_SEQ.fetch_add(1, Ordering::Relaxed) + 1;
    seq <= config.max_events
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

        let mut encoder =
            context
                .gpu_context
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("glaphica-render-cmd-encoder"),
                });
        for cmd in cmds {
            encode_cmd(context, cmd, cache, &mut encoder)?;
        }
        context.gpu_context.queue.submit(Some(encoder.finish()));
        Ok(())
    }

    pub fn execute_with_encoder(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
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
            encode_cmd(context, cmd, cache, encoder)?;
        }
        Ok(())
    }

    pub fn clear_tile(
        &self,
        context: &mut RenderContext<'_>,
        clear_op: &ClearOp,
    ) -> Result<(), RenderExecutorError> {
        let mut encoder =
            context
                .gpu_context
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("glaphica-clear-tile-encoder"),
                });
        self.encode_clear_tile(context, clear_op, &mut encoder)?;
        context.gpu_context.queue.submit(Some(encoder.finish()));
        Ok(())
    }

    pub fn clear_tile_with_encoder(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        context: &mut RenderContext<'_>,
        clear_op: &ClearOp,
    ) -> Result<(), RenderExecutorError> {
        self.encode_clear_tile(context, clear_op, encoder)
    }

    fn encode_clear_tile(
        &self,
        context: &mut RenderContext<'_>,
        clear_op: &ClearOp,
        encoder: &mut wgpu::CommandEncoder,
    ) -> Result<(), RenderExecutorError> {
        let resolved = context.atlas_storage.resolve(clear_op.tile_key).ok_or(
            RenderExecutorError::MissingTileBackend {
                tile_key: clear_op.tile_key,
            },
        )?;
        if should_trace_gpu_exec_event() {
            eprintln!(
                "[PERF][gpu_exec_trace][clear] tile={:?} layer={} texel=({}, {})",
                clear_op.tile_key,
                resolved.address.layer,
                resolved.address.texel_offset.0,
                resolved.address.texel_offset.1
            );
        }
        let bytes_per_pixel = texture_bytes_per_pixel(resolved.format);
        let unpadded_bytes_per_row = bytes_per_pixel * ATLAS_TILE_SIZE;
        let padded_bytes_per_row = unpadded_bytes_per_row.div_ceil(256).saturating_mul(256);
        let clear_size = usize::try_from(padded_bytes_per_row.saturating_mul(ATLAS_TILE_SIZE))
            .unwrap_or(0);
        if clear_size == 0 {
            return Ok(());
        }
        let clear_data = vec![0u8; clear_size];
        let clear_buffer = context
            .gpu_context
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("glaphica-clear-tile-buffer"),
                contents: &clear_data,
                usage: wgpu::BufferUsages::COPY_SRC,
            });
        encoder.copy_buffer_to_texture(
            wgpu::TexelCopyBufferInfo {
                buffer: &clear_buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_bytes_per_row),
                    rows_per_image: Some(ATLAS_TILE_SIZE),
                },
            },
            wgpu::TexelCopyTextureInfo {
                texture: resolved.texture2d_array,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: resolved.address.texel_offset.0,
                    y: resolved.address.texel_offset.1,
                    z: resolved.address.layer,
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

    pub fn copy_tile(
        &self,
        context: &mut RenderContext<'_>,
        copy_op: &CopyOp,
    ) -> Result<(), RenderExecutorError> {
        let mut encoder =
            context
                .gpu_context
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("glaphica-copy-tile-encoder"),
                });
        self.encode_copy_tile(context, copy_op, &mut encoder)?;
        context.gpu_context.queue.submit(Some(encoder.finish()));
        Ok(())
    }

    pub fn copy_tile_with_encoder(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        context: &mut RenderContext<'_>,
        copy_op: &CopyOp,
    ) -> Result<(), RenderExecutorError> {
        self.encode_copy_tile(context, copy_op, encoder)
    }

    fn encode_copy_tile(
        &self,
        context: &mut RenderContext<'_>,
        copy_op: &CopyOp,
        encoder: &mut wgpu::CommandEncoder,
    ) -> Result<(), RenderExecutorError> {
        let src_resolved = context.atlas_storage.resolve(copy_op.src_tile_key).ok_or(
            RenderExecutorError::MissingTileBackend {
                tile_key: copy_op.src_tile_key,
            },
        )?;
        let dst_resolved = context.atlas_storage.resolve(copy_op.dst_tile_key).ok_or(
            RenderExecutorError::MissingTileBackend {
                tile_key: copy_op.dst_tile_key,
            },
        )?;
        if should_trace_gpu_exec_event() {
            eprintln!(
                "[PERF][gpu_exec_trace][copy] src={:?}@({}, {}, l{}) dst={:?}@({}, {}, l{})",
                copy_op.src_tile_key,
                src_resolved.address.texel_offset.0,
                src_resolved.address.texel_offset.1,
                src_resolved.address.layer,
                copy_op.dst_tile_key,
                dst_resolved.address.texel_offset.0,
                dst_resolved.address.texel_offset.1,
                dst_resolved.address.layer
            );
        }

        encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: src_resolved.texture2d_array,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: src_resolved.address.texel_offset.0,
                    y: src_resolved.address.texel_offset.1,
                    z: src_resolved.address.layer,
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture: dst_resolved.texture2d_array,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: dst_resolved.address.texel_offset.0,
                    y: dst_resolved.address.texel_offset.1,
                    z: dst_resolved.address.layer,
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

    pub fn write_tile(
        &mut self,
        context: &mut RenderContext<'_>,
        write_op: &WriteOp,
    ) -> Result<(), RenderExecutorError> {
        let mut encoder =
            context
                .gpu_context
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("glaphica-write-tile-encoder"),
                });
        self.encode_write_tile(context, write_op, &mut encoder)?;
        context.gpu_context.queue.submit(Some(encoder.finish()));
        Ok(())
    }

    pub fn write_tile_with_encoder(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        context: &mut RenderContext<'_>,
        write_op: &WriteOp,
    ) -> Result<(), RenderExecutorError> {
        self.encode_write_tile(context, write_op, encoder)
    }

    fn encode_write_tile(
        &mut self,
        context: &mut RenderContext<'_>,
        write_op: &WriteOp,
        encoder: &mut wgpu::CommandEncoder,
    ) -> Result<(), RenderExecutorError> {
        if context.atlas_storage.resolve(write_op.src_tile_key).is_none() {
            return Err(RenderExecutorError::MissingTileBackend {
                tile_key: write_op.src_tile_key,
            });
        }
        let dst_resolved = context.atlas_storage.resolve(write_op.dst_tile_key).ok_or(
            RenderExecutorError::MissingTileBackend {
                tile_key: write_op.dst_tile_key,
            },
        )?;
        let src_resolved = context.atlas_storage.resolve(write_op.src_tile_key).ok_or(
            RenderExecutorError::MissingTileBackend {
                tile_key: write_op.src_tile_key,
            },
        )?;
        if should_trace_gpu_exec_event() {
            eprintln!(
                "[PERF][gpu_exec_trace][write] src={:?}@({}, {}, l{}) dst={:?}@({}, {}, l{}) opacity={:.3}",
                write_op.src_tile_key,
                src_resolved.address.texel_offset.0,
                src_resolved.address.texel_offset.1,
                src_resolved.address.layer,
                write_op.dst_tile_key,
                dst_resolved.address.texel_offset.0,
                dst_resolved.address.texel_offset.1,
                dst_resolved.address.layer,
                write_op.opacity
            );
        }

        self.ensure_pipelines(context, dst_resolved.format);
        let cache = self.cache.as_ref().unwrap();
        let bind_group = create_bind_group(
            context,
            &cache.bind_group_layout,
            &cache.sampler,
            write_op.src_tile_key,
            write_op.opacity,
        );
        let dst_view = create_render_attachment_view(&dst_resolved);
        let pipeline = match write_op.blend_mode {
            WriteBlendMode::Normal => &cache.normal,
        };

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("glaphica-render-write-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &dst_view,
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
            dst_resolved.address.texel_offset.0,
            dst_resolved.address.texel_offset.1,
            ATLAS_TILE_SIZE,
            ATLAS_TILE_SIZE,
        );
        pass.draw(0..3, 0..1);

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

        self.cache = Some(PipelineCache {
            normal,
            multiply,
            bind_group_layout,
            sampler,
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
            LeafBlendMode::Normal => (
                wgpu::BlendState {
                    color: wgpu::BlendComponent {
                        src_factor: wgpu::BlendFactor::One,
                        dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                        operation: wgpu::BlendOperation::Add,
                    },
                    alpha: wgpu::BlendComponent {
                        src_factor: wgpu::BlendFactor::One,
                        dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                        operation: wgpu::BlendOperation::Add,
                    },
                },
                "fs_normal",
            ),
            LeafBlendMode::Multiply => (
                wgpu::BlendState {
                    color: wgpu::BlendComponent {
                        src_factor: wgpu::BlendFactor::Dst,
                        dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                        operation: wgpu::BlendOperation::Add,
                    },
                    alpha: wgpu::BlendComponent {
                        src_factor: wgpu::BlendFactor::One,
                        dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                        operation: wgpu::BlendOperation::Add,
                    },
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

fn encode_cmd(
    context: &mut RenderContext<'_>,
    cmd: &RenderCmd,
    cache: &PipelineCache,
    encoder: &mut wgpu::CommandEncoder,
) -> Result<(), RenderExecutorError> {
    if cmd.to.is_empty() || cmd.from.is_empty() {
        return Ok(());
    }

    // Dev assertion: verify all sources have matching tile_keys length
    #[cfg(debug_assertions)]
    {
        for (i, source) in cmd.from.iter().enumerate() {
            debug_assert!(
                source.tile_keys.len() == cmd.to.len(),
                "RenderCmd source {} has {} tile_keys but cmd.to has {} tiles. \
                 This indicates a bug in build_render_cmds - all sources must have \
                 matching tile_counts for the dirty tile indices.",
                i,
                source.tile_keys.len(),
                cmd.to.len()
            );
        }
    }

    for (tile_idx, &dst_tile_key) in cmd.to.iter().enumerate() {
        if dst_tile_key == TileKey::EMPTY {
            continue;
        }

        let mut src_keys = Vec::new();
        for source in &cmd.from {
            if tile_idx >= source.tile_keys.len() {
                continue;
            }
            let src_tile_key = source.tile_keys[tile_idx];
            if src_tile_key == TileKey::EMPTY {
                continue;
            }
            src_keys.push(src_tile_key);
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

    Ok(())
}

fn create_bind_group(
    context: &RenderContext<'_>,
    layout: &wgpu::BindGroupLayout,
    sampler: &wgpu::Sampler,
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
    let params_buffer = context
        .gpu_context
        .device
        .create_buffer(&wgpu::BufferDescriptor {
            label: Some("glaphica-render-params"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

    context
        .gpu_context
        .queue
        .write_buffer(&params_buffer, 0, &params_bytes);

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

fn texture_bytes_per_pixel(format: wgpu::TextureFormat) -> u32 {
    match format {
        wgpu::TextureFormat::R8Unorm => 1,
        wgpu::TextureFormat::Rg8Unorm => 2,
        wgpu::TextureFormat::Rgba8Unorm | wgpu::TextureFormat::Bgra8Unorm => 4,
        wgpu::TextureFormat::Rgba16Float => 8,
        _ => 4,
    }
}

#[cfg(all(test, feature = "blocking"))]
mod tests {
    use crate::atlas_runtime::AtlasStorageRuntime;
    use crate::context::{GpuContext, GpuContextInitDescriptor};

    use super::{RenderContext, RenderExecutor};
    use glaphica_core::{ATLAS_TILE_SIZE, AtlasLayout, BackendKind, TileKey};
    use thread_protocol::ClearOp;

    #[test]
    fn clear_tile_does_not_modify_neighbor_tile_in_same_layer() {
        let Ok(gpu_context) = GpuContext::init_blocking(&GpuContextInitDescriptor::default())
        else {
            eprintln!("skip test: gpu context init failed");
            return;
        };

        let mut atlas_storage = AtlasStorageRuntime::with_capacity(1);
        if atlas_storage
            .create_backend(
                &gpu_context.device,
                0,
                BackendKind::Leaf,
                AtlasLayout::Small11,
                Default::default(),
            )
            .is_err()
        {
            eprintln!("skip test: atlas backend init failed");
            return;
        }

        // slot=0 and slot=1 are neighbors in the same parity/layer for Small11.
        let left_tile = TileKey::from_parts(0, 0, 0);
        let right_tile = TileKey::from_parts(0, 0, 1);
        fill_tile_rgba8(&gpu_context, &atlas_storage, left_tile, [255, 0, 0, 255]);
        fill_tile_rgba8(&gpu_context, &atlas_storage, right_tile, [0, 255, 0, 255]);

        let executor = RenderExecutor::new();
        let mut context = RenderContext {
            gpu_context: &gpu_context,
            atlas_storage: &atlas_storage,
        };
        let clear_result = executor.clear_tile(
            &mut context,
            &ClearOp {
                tile_key: left_tile,
            },
        );
        assert!(clear_result.is_ok());

        let left_pixel = sample_tile_pixel_rgba8(&gpu_context, &atlas_storage, left_tile);
        let right_pixel = sample_tile_pixel_rgba8(&gpu_context, &atlas_storage, right_tile);

        assert_eq!(left_pixel, [0, 0, 0, 0]);
        assert_eq!(right_pixel, [0, 255, 0, 255]);
    }

    fn fill_tile_rgba8(
        gpu_context: &GpuContext,
        atlas_storage: &AtlasStorageRuntime,
        key: TileKey,
        color: [u8; 4],
    ) {
        let Some(resolved) = atlas_storage.resolve(key) else {
            return;
        };
        let pixel_count = (ATLAS_TILE_SIZE * ATLAS_TILE_SIZE) as usize;
        let mut data = Vec::with_capacity(pixel_count * 4);
        for _ in 0..pixel_count {
            data.extend_from_slice(&color);
        }
        gpu_context.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: resolved.texture2d_array,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: resolved.address.texel_offset.0,
                    y: resolved.address.texel_offset.1,
                    z: resolved.address.layer,
                },
                aspect: wgpu::TextureAspect::All,
            },
            &data,
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

    fn sample_tile_pixel_rgba8(
        gpu_context: &GpuContext,
        atlas_storage: &AtlasStorageRuntime,
        key: TileKey,
    ) -> [u8; 4] {
        let Some(resolved) = atlas_storage.resolve(key) else {
            return [0, 0, 0, 0];
        };
        let width = ATLAS_TILE_SIZE;
        let height = ATLAS_TILE_SIZE;
        let bytes_per_row = width * 4;
        let buffer_size = (bytes_per_row * height) as u64;

        let buffer = gpu_context.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("render-executor-test-readback"),
            size: buffer_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut encoder =
            gpu_context
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("render-executor-test-readback-encoder"),
                });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: resolved.texture2d_array,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: resolved.address.texel_offset.0,
                    y: resolved.address.texel_offset.1,
                    z: resolved.address.layer,
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(bytes_per_row),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        gpu_context.queue.submit(Some(encoder.finish()));

        let slice = buffer.slice(..);
        let (sender, receiver) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            if sender.send(result).is_err() {
                eprintln!("readback map callback send failed");
            }
        });
        let _ = gpu_context
            .device
            .poll(wgpu::PollType::wait_indefinitely());
        if receiver.recv().is_err() {
            return [0, 0, 0, 0];
        }

        let mapped = slice.get_mapped_range();
        let pixel = [mapped[0], mapped[1], mapped[2], mapped[3]];
        drop(mapped);
        buffer.unmap();
        pixel
    }
}
