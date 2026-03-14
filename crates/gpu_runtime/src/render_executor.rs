use std::error::Error;
use std::fmt::{Display, Formatter};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use wgpu::util::DeviceExt;

use document::{LeafBlendMode, MaterializeParametricCmd, ParametricVertex, RenderCmd};
use glaphica_core::{ATLAS_TILE_SIZE, GUTTER_SIZE, TileKey};
use thread_protocol::{ClearOp, CompositeOp, CopyOp, WriteBlendMode, WriteOp};

use crate::atlas_runtime::{AtlasResolvedAddress, AtlasStorageRuntime};
use crate::context::GpuContext;

#[derive(Debug)]
pub enum RenderExecutorError {
    MissingTileBackend { tile_key: TileKey },
    PipelineNotInitialized,
}

impl Display for RenderExecutorError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingTileBackend { tile_key } => {
                write!(f, "missing atlas backend for tile key {:?}", tile_key)
            }
            Self::PipelineNotInitialized => {
                write!(
                    f,
                    "render pipeline not initialized, ensure_pipelines must be called first"
                )
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
    format: wgpu::TextureFormat,
    normal: wgpu::RenderPipeline,
    multiply: wgpu::RenderPipeline,
    image_normal: wgpu::RenderPipeline,
    image_multiply: wgpu::RenderPipeline,
    write_erase: wgpu::RenderPipeline,
    composite_normal: wgpu::RenderPipeline,
    parametric: wgpu::RenderPipeline,
    clear: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    composite_bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
}

pub struct RenderExecutor {
    cache: Option<PipelineCache>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RenderPassKey {
    backend_id: u8,
    layer: u32,
    format: wgpu::TextureFormat,
}

struct PreparedRenderSource {
    bind_group: wgpu::BindGroup,
    blend_mode: LeafBlendMode,
}

struct PreparedRenderTile {
    pass_key: RenderPassKey,
    scissor_x: u32,
    scissor_y: u32,
    sources: Vec<PreparedRenderSource>,
}

struct PreparedRenderPass {
    key: RenderPassKey,
    tiles: Vec<PreparedRenderTile>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WritePassKey {
    backend_id: u8,
    layer: u32,
    format: wgpu::TextureFormat,
}

struct PreparedWriteCall {
    pass_key: WritePassKey,
    scissor_x: u32,
    scissor_y: u32,
    bind_group: wgpu::BindGroup,
    blend_mode: WriteBlendMode,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuParametricVertex {
    position: [f32; 2],
    color: [f32; 4],
}

#[derive(Debug, Clone, Copy)]
struct GpuExecTraceConfig {
    enabled: bool,
    max_events: u64,
}

const CLEAR_SHADER: &str = r#"
@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> @builtin(position) vec4f {
    var positions = array<vec2f, 3>(
        vec2f(-1.0, -1.0),
        vec2f(3.0, -1.0),
        vec2f(-1.0, 3.0),
    );
    return vec4f(positions[vertex_index], 0.0, 1.0);
}

@fragment
fn fs_main() -> @location(0) vec4f {
    return vec4f(0.0, 0.0, 0.0, 0.0);
}
"#;

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

    pub fn materialize_parametric_with_encoder(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        context: &mut RenderContext<'_>,
        cmds: &[MaterializeParametricCmd],
    ) -> Result<(), RenderExecutorError> {
        if cmds.is_empty() {
            return Ok(());
        }

        let format = self.detect_parametric_format(context, cmds);
        self.ensure_pipelines(context, format);
        let cache = self
            .cache
            .as_ref()
            .ok_or(RenderExecutorError::PipelineNotInitialized)?;

        for cmd in cmds {
            encode_parametric_cmd(context, cmd, cache, encoder)?;
        }
        Ok(())
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
        let cache = self
            .cache
            .as_ref()
            .ok_or(RenderExecutorError::PipelineNotInitialized)?;

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
        let cache = self
            .cache
            .as_ref()
            .ok_or(RenderExecutorError::PipelineNotInitialized)?;

        for cmd in cmds {
            encode_cmd(context, cmd, cache, encoder)?;
        }
        Ok(())
    }

    pub fn clear_tile(
        &mut self,
        context: &mut RenderContext<'_>,
        clear_op: &ClearOp,
    ) -> Result<(), RenderExecutorError> {
        let resolved = context.atlas_storage.resolve(clear_op.tile_key).ok_or(
            RenderExecutorError::MissingTileBackend {
                tile_key: clear_op.tile_key,
            },
        )?;
        self.ensure_pipelines(context, resolved.format);
        let cache = self
            .cache
            .as_ref()
            .ok_or(RenderExecutorError::PipelineNotInitialized)?;
        let mut encoder =
            context
                .gpu_context
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("glaphica-clear-tile-encoder"),
                });
        self.encode_clear_tile(context, clear_op, cache, &mut encoder)?;
        context.gpu_context.queue.submit(Some(encoder.finish()));
        Ok(())
    }

    pub fn clear_tile_with_encoder(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        context: &mut RenderContext<'_>,
        clear_op: &ClearOp,
    ) -> Result<(), RenderExecutorError> {
        let resolved = context.atlas_storage.resolve(clear_op.tile_key).ok_or(
            RenderExecutorError::MissingTileBackend {
                tile_key: clear_op.tile_key,
            },
        )?;
        self.ensure_pipelines(context, resolved.format);
        let cache = self
            .cache
            .as_ref()
            .ok_or(RenderExecutorError::PipelineNotInitialized)?;
        self.encode_clear_tile(context, clear_op, cache, encoder)
    }

    fn encode_clear_tile(
        &self,
        context: &mut RenderContext<'_>,
        clear_op: &ClearOp,
        cache: &PipelineCache,
        encoder: &mut wgpu::CommandEncoder,
    ) -> Result<(), RenderExecutorError> {
        let resolved = context.atlas_storage.resolve(clear_op.tile_key).ok_or(
            RenderExecutorError::MissingTileBackend {
                tile_key: clear_op.tile_key,
            },
        )?;
        encode_clear_resolved_tile(context.gpu_context, &cache.clear, &resolved, encoder);
        Ok(())
    }

    fn detect_parametric_format(
        &self,
        context: &mut RenderContext<'_>,
        cmds: &[MaterializeParametricCmd],
    ) -> wgpu::TextureFormat {
        for cmd in cmds {
            if let Some(dst_tile_key) = cmd.dst_tile_keys.first() {
                if let Some(resolved) = context.atlas_storage.resolve(*dst_tile_key) {
                    return resolved.format;
                }
            }
        }
        wgpu::TextureFormat::Rgba8Unorm
    }
}

fn encode_clear_resolved_tile(
    _gpu_context: &GpuContext,
    clear_pipeline: &wgpu::RenderPipeline,
    resolved: &AtlasResolvedAddress<'_>,
    encoder: &mut wgpu::CommandEncoder,
) {
    if should_trace_gpu_exec_event() {
        eprintln!(
            "[PERF][gpu_exec_trace][clear] layer={} texel=({}, {})",
            resolved.address.layer,
            resolved.address.texel_offset.0,
            resolved.address.texel_offset.1
        );
    }
    let dst_view = resolved
        .texture2d_array
        .create_view(&wgpu::TextureViewDescriptor {
            label: Some("glaphica-clear-attachment-view"),
            format: Some(resolved.format),
            dimension: Some(wgpu::TextureViewDimension::D2),
            usage: Some(wgpu::TextureUsages::RENDER_ATTACHMENT),
            aspect: wgpu::TextureAspect::All,
            base_mip_level: 0,
            mip_level_count: Some(1),
            base_array_layer: resolved.address.layer,
            array_layer_count: Some(1),
        });
    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("glaphica-clear-tile-pass"),
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
    pass.set_pipeline(clear_pipeline);
    pass.set_scissor_rect(
        resolved.address.texel_offset.0,
        resolved.address.texel_offset.1,
        ATLAS_TILE_SIZE,
        ATLAS_TILE_SIZE,
    );
    pass.draw(0..3, 0..1);
}

fn encode_parametric_cmd(
    context: &mut RenderContext<'_>,
    cmd: &MaterializeParametricCmd,
    cache: &PipelineCache,
    encoder: &mut wgpu::CommandEncoder,
) -> Result<(), RenderExecutorError> {
    if cmd.mesh.vertices.is_empty() || cmd.mesh.indices.is_empty() {
        return Ok(());
    }

    let index_data: Vec<u16> = cmd.mesh.indices.clone();
    let index_buffer =
        context
            .gpu_context
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("glaphica-parametric-index-buffer"),
                contents: bytemuck::cast_slice(&index_data),
                usage: wgpu::BufferUsages::INDEX,
            });

    for ((&dst_tile_key, &tile_origin), &_tile_index) in cmd
        .dst_tile_keys
        .iter()
        .zip(&cmd.tile_origins)
        .zip(&cmd.tile_indices)
    {
        if dst_tile_key == TileKey::EMPTY {
            continue;
        }
        let dst_resolved = context.atlas_storage.resolve(dst_tile_key).ok_or(
            RenderExecutorError::MissingTileBackend {
                tile_key: dst_tile_key,
            },
        )?;
        encode_clear_resolved_tile(context.gpu_context, &cache.clear, &dst_resolved, encoder);

        let vertices: Vec<GpuParametricVertex> = cmd
            .mesh
            .vertices
            .iter()
            .map(|vertex| map_parametric_vertex(*vertex, tile_origin))
            .collect();
        let vertex_buffer =
            context
                .gpu_context
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("glaphica-parametric-vertex-buffer"),
                    contents: bytemuck::cast_slice(&vertices),
                    usage: wgpu::BufferUsages::VERTEX,
                });

        let backend = context
            .atlas_storage
            .backend_resource(dst_tile_key.backend_index())
            .ok_or(RenderExecutorError::MissingTileBackend {
                tile_key: TileKey::from_parts(dst_tile_key.backend_index(), 0, 0),
            })?;
        let dst_view = backend
            .texture2d_array
            .create_view(&wgpu::TextureViewDescriptor {
                label: Some("glaphica-parametric-attachment-view"),
                format: Some(dst_resolved.format),
                dimension: Some(wgpu::TextureViewDimension::D2),
                usage: Some(wgpu::TextureUsages::RENDER_ATTACHMENT),
                aspect: wgpu::TextureAspect::All,
                base_mip_level: 0,
                mip_level_count: Some(1),
                base_array_layer: dst_resolved.address.layer,
                array_layer_count: Some(1),
            });

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("glaphica-parametric-materialize-pass"),
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
        pass.set_pipeline(&cache.parametric);
        pass.set_viewport(
            dst_resolved.address.texel_offset.0 as f32,
            dst_resolved.address.texel_offset.1 as f32,
            ATLAS_TILE_SIZE as f32,
            ATLAS_TILE_SIZE as f32,
            0.0,
            1.0,
        );
        pass.set_scissor_rect(
            dst_resolved.address.texel_offset.0,
            dst_resolved.address.texel_offset.1,
            ATLAS_TILE_SIZE,
            ATLAS_TILE_SIZE,
        );
        pass.set_vertex_buffer(0, vertex_buffer.slice(..));
        pass.set_index_buffer(index_buffer.slice(..), wgpu::IndexFormat::Uint16);
        pass.draw_indexed(0..u32::try_from(index_data.len()).unwrap_or(0), 0, 0..1);
    }
    Ok(())
}

fn map_parametric_vertex(
    vertex: ParametricVertex,
    tile_origin: glaphica_core::CanvasVec2,
) -> GpuParametricVertex {
    let tile_x = vertex.position.x - tile_origin.x + GUTTER_SIZE as f32;
    let tile_y = vertex.position.y - tile_origin.y + GUTTER_SIZE as f32;
    let ndc_x = tile_x / ATLAS_TILE_SIZE as f32 * 2.0 - 1.0;
    let ndc_y = 1.0 - tile_y / ATLAS_TILE_SIZE as f32 * 2.0;
    GpuParametricVertex {
        position: [ndc_x, ndc_y],
        color: vertex.color,
    }
}

impl RenderExecutor {
    fn prepare_render_passes(
        context: &mut RenderContext<'_>,
        cmd: &RenderCmd,
        cache: &PipelineCache,
        encoder: &mut wgpu::CommandEncoder,
    ) -> Result<Vec<PreparedRenderPass>, RenderExecutorError> {
        let mut passes = Vec::<PreparedRenderPass>::new();
        if should_trace_gpu_exec_event() {
            eprintln!(
                "[PERF][gpu_exec_trace][render_cmd] dst_tiles={}",
                cmd.to.len()
            );
        }

        for (tile_idx, &dst_tile_key) in cmd.to.iter().enumerate() {
            if dst_tile_key == TileKey::EMPTY {
                continue;
            }

            let dst_resolved = context.atlas_storage.resolve(dst_tile_key).ok_or(
                RenderExecutorError::MissingTileBackend {
                    tile_key: dst_tile_key,
                },
            )?;
            encode_clear_resolved_tile(context.gpu_context, &cache.clear, &dst_resolved, encoder);

            let mut sources = Vec::new();
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
                    None,
                    None,
                )?;
                sources.push(PreparedRenderSource {
                    bind_group,
                    blend_mode: source.config.blend_mode,
                });
            }

            if sources.is_empty() {
                continue;
            }

            let pass_key = RenderPassKey {
                backend_id: dst_tile_key.backend_index(),
                layer: dst_resolved.address.layer,
                format: dst_resolved.format,
            };
            let prepared_tile = PreparedRenderTile {
                pass_key,
                scissor_x: dst_resolved.address.texel_offset.0,
                scissor_y: dst_resolved.address.texel_offset.1,
                sources,
            };

            if let Some(existing) = passes.iter_mut().find(|pass| pass.key == pass_key) {
                existing.tiles.push(prepared_tile);
            } else {
                passes.push(PreparedRenderPass {
                    key: pass_key,
                    tiles: vec![prepared_tile],
                });
            }
        }

        Ok(passes)
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

    pub fn write_tiles_with_encoder(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        context: &mut RenderContext<'_>,
        write_ops: &[&WriteOp],
    ) -> Result<(), RenderExecutorError> {
        if write_ops.is_empty() {
            return Ok(());
        }

        let prepared = self.prepare_write_calls(context, write_ops)?;
        let mut start = 0usize;
        while start < prepared.len() {
            let pass_key = prepared[start].pass_key;
            let mut end = start + 1;
            while end < prepared.len() && prepared[end].pass_key == pass_key {
                end += 1;
            }

            // Buffered stroke writes only sample transient source tiles and blend into disjoint
            // destination tiles, so grouping by destination layer preserves the same result while
            // avoiding one render pass per tile.
            let backend = context
                .atlas_storage
                .backend_resource(pass_key.backend_id)
                .ok_or(RenderExecutorError::MissingTileBackend {
                    tile_key: TileKey::from_parts(pass_key.backend_id, 0, 0),
                })?;
            let dst_view = backend
                .texture2d_array
                .create_view(&wgpu::TextureViewDescriptor {
                    label: Some("glaphica-render-write-attachment-view"),
                    format: Some(pass_key.format),
                    dimension: Some(wgpu::TextureViewDimension::D2),
                    usage: Some(wgpu::TextureUsages::RENDER_ATTACHMENT),
                    aspect: wgpu::TextureAspect::All,
                    base_mip_level: 0,
                    mip_level_count: Some(1),
                    base_array_layer: pass_key.layer,
                    array_layer_count: Some(1),
                });

            let cache = self
                .cache
                .as_ref()
                .ok_or(RenderExecutorError::PipelineNotInitialized)?;
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("glaphica-render-write-pass-batch"),
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

            for call in &prepared[start..end] {
                let pipeline = match call.blend_mode {
                    WriteBlendMode::Normal => &cache.normal,
                    WriteBlendMode::Erase => &cache.write_erase,
                };
                pass.set_pipeline(pipeline);
                pass.set_bind_group(0, &call.bind_group, &[]);
                pass.set_scissor_rect(
                    call.scissor_x,
                    call.scissor_y,
                    ATLAS_TILE_SIZE,
                    ATLAS_TILE_SIZE,
                );
                pass.draw(0..3, 0..1);
            }
            start = end;
        }

        Ok(())
    }

    fn encode_write_tile(
        &mut self,
        context: &mut RenderContext<'_>,
        write_op: &WriteOp,
        encoder: &mut wgpu::CommandEncoder,
    ) -> Result<(), RenderExecutorError> {
        if context
            .atlas_storage
            .resolve(write_op.src_tile_key)
            .is_none()
        {
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
        let cache = self
            .cache
            .as_ref()
            .ok_or(RenderExecutorError::PipelineNotInitialized)?;
        let bind_group = create_bind_group(
            context,
            &cache.bind_group_layout,
            &cache.sampler,
            write_op.src_tile_key,
            write_op.opacity,
            write_op.rgb,
            write_op.origin_tile_key,
        )?;
        let dst_view = create_render_attachment_view(&dst_resolved);
        let pipeline = match write_op.blend_mode {
            WriteBlendMode::Normal => &cache.normal,
            WriteBlendMode::Erase => &cache.write_erase,
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

    fn prepare_write_calls(
        &mut self,
        context: &mut RenderContext<'_>,
        write_ops: &[&WriteOp],
    ) -> Result<Vec<PreparedWriteCall>, RenderExecutorError> {
        let mut prepared = Vec::with_capacity(write_ops.len());
        for write_op in write_ops {
            if context
                .atlas_storage
                .resolve(write_op.src_tile_key)
                .is_none()
            {
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
            let cache = self
                .cache
                .as_ref()
                .ok_or(RenderExecutorError::PipelineNotInitialized)?;
            let bind_group = create_bind_group(
                context,
                &cache.bind_group_layout,
                &cache.sampler,
                write_op.src_tile_key,
                write_op.opacity,
                write_op.rgb,
                write_op.origin_tile_key,
            )?;
            prepared.push(PreparedWriteCall {
                pass_key: WritePassKey {
                    backend_id: write_op.dst_tile_key.backend_index(),
                    layer: dst_resolved.address.layer,
                    format: dst_resolved.format,
                },
                scissor_x: dst_resolved.address.texel_offset.0,
                scissor_y: dst_resolved.address.texel_offset.1,
                bind_group,
                blend_mode: write_op.blend_mode,
            });
        }
        Ok(prepared)
    }

    pub fn composite_tile_with_encoder(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        context: &mut RenderContext<'_>,
        composite_op: &CompositeOp,
    ) -> Result<(), RenderExecutorError> {
        self.encode_composite_tile(context, composite_op, encoder)
    }

    fn encode_composite_tile(
        &mut self,
        context: &mut RenderContext<'_>,
        composite_op: &CompositeOp,
        encoder: &mut wgpu::CommandEncoder,
    ) -> Result<(), RenderExecutorError> {
        let base_resolved = context
            .atlas_storage
            .resolve(composite_op.base_tile_key)
            .ok_or(RenderExecutorError::MissingTileBackend {
                tile_key: composite_op.base_tile_key,
            })?;
        let overlay_resolved = context
            .atlas_storage
            .resolve(composite_op.overlay_tile_key)
            .ok_or(RenderExecutorError::MissingTileBackend {
                tile_key: composite_op.overlay_tile_key,
            })?;
        let dst_resolved = context
            .atlas_storage
            .resolve(composite_op.dst_tile_key)
            .ok_or(RenderExecutorError::MissingTileBackend {
                tile_key: composite_op.dst_tile_key,
            })?;
        if should_trace_gpu_exec_event() {
            eprintln!(
                "[PERF][gpu_exec_trace][composite] base={:?}@({}, {}, l{}) overlay={:?}@({}, {}, l{}) dst={:?}@({}, {}, l{}) opacity={:.3}",
                composite_op.base_tile_key,
                base_resolved.address.texel_offset.0,
                base_resolved.address.texel_offset.1,
                base_resolved.address.layer,
                composite_op.overlay_tile_key,
                overlay_resolved.address.texel_offset.0,
                overlay_resolved.address.texel_offset.1,
                overlay_resolved.address.layer,
                composite_op.dst_tile_key,
                dst_resolved.address.texel_offset.0,
                dst_resolved.address.texel_offset.1,
                dst_resolved.address.layer,
                composite_op.opacity
            );
        }

        self.ensure_pipelines(context, dst_resolved.format);
        let cache = self
            .cache
            .as_ref()
            .ok_or(RenderExecutorError::PipelineNotInitialized)?;
        let bind_group =
            create_composite_bind_group(context, &cache.composite_bind_group_layout, composite_op)?;
        let dst_view = create_render_attachment_view(&dst_resolved);
        let pipeline = match composite_op.blend_mode {
            WriteBlendMode::Normal => &cache.composite_normal,
            WriteBlendMode::Erase => &cache.composite_normal,
        };

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("glaphica-render-composite-op-pass"),
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
        if let Some(cache) = &self.cache
            && cache.format == format
        {
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
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
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

        let composite_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("glaphica-render-composite-bind-group-layout"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
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
                ],
            });
        let composite_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("glaphica-render-composite-pipeline-layout"),
                bind_group_layouts: &[&composite_bind_group_layout],
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
        let image_normal = Self::create_image_pipeline(
            device,
            &pipeline_layout,
            &shader,
            format,
            LeafBlendMode::Normal,
        );
        let image_multiply = Self::create_image_pipeline(
            device,
            &pipeline_layout,
            &shader,
            format,
            LeafBlendMode::Multiply,
        );
        let write_erase =
            Self::create_write_erase_pipeline(device, &pipeline_layout, &shader, format);
        let composite_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("glaphica-render-composite-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("render_composite_shader.wgsl").into()),
        });
        let clear_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("glaphica-render-clear-shader"),
            source: wgpu::ShaderSource::Wgsl(CLEAR_SHADER.into()),
        });
        let parametric_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("glaphica-render-parametric-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("render_parametric_shader.wgsl").into()),
        });
        let clear_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("glaphica-render-clear-pipeline-layout"),
                bind_group_layouts: &[],
                immediate_size: 0,
            });
        let parametric_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("glaphica-render-parametric-pipeline-layout"),
                bind_group_layouts: &[],
                immediate_size: 0,
            });
        let composite_normal = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("glaphica-render-pipeline-composite-normal"),
            layout: Some(&composite_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &composite_shader,
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
                module: &composite_shader,
                entry_point: Some("fs_composite_normal"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let parametric = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("glaphica-render-pipeline-parametric"),
            layout: Some(&parametric_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &parametric_shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<GpuParametricVertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[
                        wgpu::VertexAttribute {
                            offset: 0,
                            shader_location: 0,
                            format: wgpu::VertexFormat::Float32x2,
                        },
                        wgpu::VertexAttribute {
                            offset: 8,
                            shader_location: 1,
                            format: wgpu::VertexFormat::Float32x4,
                        },
                    ],
                }],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &parametric_shader,
                entry_point: Some("fs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState {
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
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let clear = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("glaphica-render-pipeline-clear"),
            layout: Some(&clear_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &clear_shader,
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
                module: &clear_shader,
                entry_point: Some("fs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });

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
            format,
            normal,
            multiply,
            image_normal,
            image_multiply,
            write_erase,
            composite_normal,
            parametric,
            clear,
            bind_group_layout,
            composite_bind_group_layout,
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

    fn create_image_pipeline(
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
                "fs_image_normal",
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
                "fs_image_multiply",
            ),
        };

        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(&format!("glaphica-render-image-pipeline-{:?}", blend_mode)),
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

    fn create_write_erase_pipeline(
        device: &wgpu::Device,
        layout: &wgpu::PipelineLayout,
        shader: &wgpu::ShaderModule,
        format: wgpu::TextureFormat,
    ) -> wgpu::RenderPipeline {
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("glaphica-render-pipeline-write-erase"),
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
                entry_point: Some("fs_erase"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::REPLACE),
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

    let passes = RenderExecutor::prepare_render_passes(context, cmd, cache, encoder)?;
    for prepared_pass in passes {
        let backend = context
            .atlas_storage
            .backend_resource(prepared_pass.key.backend_id)
            .ok_or(RenderExecutorError::MissingTileBackend {
                tile_key: TileKey::from_parts(prepared_pass.key.backend_id, 0, 0),
            })?;
        let dst_view = backend
            .texture2d_array
            .create_view(&wgpu::TextureViewDescriptor {
                label: Some("glaphica-render-attachment-view"),
                format: Some(prepared_pass.key.format),
                dimension: Some(wgpu::TextureViewDimension::D2),
                usage: Some(wgpu::TextureUsages::RENDER_ATTACHMENT),
                aspect: wgpu::TextureAspect::All,
                base_mip_level: 0,
                mip_level_count: Some(1),
                base_array_layer: prepared_pass.key.layer,
                array_layer_count: Some(1),
            });

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("glaphica-render-composite-pass"),
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

        for tile in prepared_pass.tiles {
            debug_assert_eq!(tile.pass_key, prepared_pass.key);
            pass.set_scissor_rect(
                tile.scissor_x,
                tile.scissor_y,
                ATLAS_TILE_SIZE,
                ATLAS_TILE_SIZE,
            );
            for source in tile.sources {
                let pipeline = match source.blend_mode {
                    LeafBlendMode::Normal => &cache.image_normal,
                    LeafBlendMode::Multiply => &cache.image_multiply,
                };
                pass.set_pipeline(pipeline);
                pass.set_bind_group(0, &source.bind_group, &[]);
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
    rgb: Option<[f32; 3]>,
    origin_tile_key: Option<TileKey>,
) -> Result<wgpu::BindGroup, RenderExecutorError> {
    let src_resolved = context.atlas_storage.resolve(src_tile_key).ok_or(
        RenderExecutorError::MissingTileBackend {
            tile_key: src_tile_key,
        },
    )?;

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
            base_array_layer: src_resolved.address.layer,
            array_layer_count: Some(1),
        });

    let origin_resolved =
        origin_tile_key.and_then(|tile_key| context.atlas_storage.resolve(tile_key));
    let origin_view = origin_resolved
        .map(|resolved| {
            resolved
                .texture2d_array
                .create_view(&wgpu::TextureViewDescriptor {
                    label: Some("glaphica-render-origin-view"),
                    format: Some(resolved.format),
                    dimension: Some(wgpu::TextureViewDimension::D2Array),
                    usage: Some(wgpu::TextureUsages::TEXTURE_BINDING),
                    aspect: wgpu::TextureAspect::All,
                    base_mip_level: 0,
                    mip_level_count: Some(1),
                    base_array_layer: resolved.address.layer,
                    array_layer_count: Some(1),
                })
        })
        .unwrap_or_else(|| src_view.clone());

    let params = RenderParams {
        src_layer: 0,
        src_x: src_resolved.address.texel_offset.0,
        src_y: src_resolved.address.texel_offset.1,
        origin_layer: 0,
        origin_x: origin_resolved
            .map(|resolved| resolved.address.texel_offset.0)
            .unwrap_or(0),
        origin_y: origin_resolved
            .map(|resolved| resolved.address.texel_offset.1)
            .unwrap_or(0),
        has_origin: if origin_resolved.is_some() { 1 } else { 0 },
        has_tint: if rgb.is_some() { 1 } else { 0 },
        tint_r: rgb.map(|value| value[0]).unwrap_or(0.0),
        tint_g: rgb.map(|value| value[1]).unwrap_or(0.0),
        tint_b: rgb.map(|value| value[2]).unwrap_or(0.0),
        opacity,
    };
    let params_bytes: [u8; 48] = params.encode();
    let params_buffer = context
        .gpu_context
        .device
        .create_buffer(&wgpu::BufferDescriptor {
            label: Some("glaphica-render-params"),
            size: 48,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

    context
        .gpu_context
        .queue
        .write_buffer(&params_buffer, 0, &params_bytes);

    Ok(context
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
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&origin_view),
                },
            ],
        }))
}

fn create_composite_bind_group(
    context: &RenderContext<'_>,
    layout: &wgpu::BindGroupLayout,
    composite_op: &CompositeOp,
) -> Result<wgpu::BindGroup, RenderExecutorError> {
    let base_resolved = context
        .atlas_storage
        .resolve(composite_op.base_tile_key)
        .ok_or(RenderExecutorError::MissingTileBackend {
            tile_key: composite_op.base_tile_key,
        })?;
    let overlay_resolved = context
        .atlas_storage
        .resolve(composite_op.overlay_tile_key)
        .ok_or(RenderExecutorError::MissingTileBackend {
            tile_key: composite_op.overlay_tile_key,
        })?;

    let base_view = base_resolved
        .texture2d_array
        .create_view(&wgpu::TextureViewDescriptor {
            label: Some("glaphica-render-composite-base-view"),
            format: Some(base_resolved.format),
            dimension: Some(wgpu::TextureViewDimension::D2),
            usage: Some(wgpu::TextureUsages::TEXTURE_BINDING),
            aspect: wgpu::TextureAspect::All,
            base_mip_level: 0,
            mip_level_count: Some(1),
            base_array_layer: base_resolved.address.layer,
            array_layer_count: Some(1),
        });
    let overlay_view = overlay_resolved
        .texture2d_array
        .create_view(&wgpu::TextureViewDescriptor {
            label: Some("glaphica-render-composite-overlay-view"),
            format: Some(overlay_resolved.format),
            dimension: Some(wgpu::TextureViewDimension::D2),
            usage: Some(wgpu::TextureUsages::TEXTURE_BINDING),
            aspect: wgpu::TextureAspect::All,
            base_mip_level: 0,
            mip_level_count: Some(1),
            base_array_layer: overlay_resolved.address.layer,
            array_layer_count: Some(1),
        });

    let params = CompositeParams {
        base_x: base_resolved.address.texel_offset.0,
        base_y: base_resolved.address.texel_offset.1,
        overlay_x: overlay_resolved.address.texel_offset.0,
        overlay_y: overlay_resolved.address.texel_offset.1,
        opacity: composite_op.opacity,
        _pad0: 0.0,
        _pad1: 0.0,
    };
    let params_bytes: [u8; 32] = params.encode();
    let params_buffer = context
        .gpu_context
        .device
        .create_buffer(&wgpu::BufferDescriptor {
            label: Some("glaphica-render-composite-params"),
            size: 32,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

    context
        .gpu_context
        .queue
        .write_buffer(&params_buffer, 0, &params_bytes);

    Ok(context
        .gpu_context
        .device
        .create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("glaphica-render-composite-bind-group"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&base_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&overlay_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: params_buffer.as_entire_binding(),
                },
            ],
        }))
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct RenderParams {
    src_layer: u32,
    src_x: u32,
    src_y: u32,
    origin_layer: u32,
    origin_x: u32,
    origin_y: u32,
    has_origin: u32,
    has_tint: u32,
    tint_r: f32,
    tint_g: f32,
    tint_b: f32,
    opacity: f32,
}

impl RenderParams {
    fn encode(&self) -> [u8; 48] {
        let mut bytes = [0u8; 48];
        bytes[0..4].copy_from_slice(&self.src_layer.to_ne_bytes());
        bytes[4..8].copy_from_slice(&self.src_x.to_ne_bytes());
        bytes[8..12].copy_from_slice(&self.src_y.to_ne_bytes());
        bytes[12..16].copy_from_slice(&self.origin_layer.to_ne_bytes());
        bytes[16..20].copy_from_slice(&self.origin_x.to_ne_bytes());
        bytes[20..24].copy_from_slice(&self.origin_y.to_ne_bytes());
        bytes[24..28].copy_from_slice(&self.has_origin.to_ne_bytes());
        bytes[28..32].copy_from_slice(&self.has_tint.to_ne_bytes());
        bytes[32..36].copy_from_slice(&self.tint_r.to_ne_bytes());
        bytes[36..40].copy_from_slice(&self.tint_g.to_ne_bytes());
        bytes[40..44].copy_from_slice(&self.tint_b.to_ne_bytes());
        bytes[44..48].copy_from_slice(&self.opacity.to_ne_bytes());
        bytes
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct CompositeParams {
    base_x: u32,
    base_y: u32,
    overlay_x: u32,
    overlay_y: u32,
    opacity: f32,
    _pad0: f32,
    _pad1: f32,
}

impl CompositeParams {
    fn encode(&self) -> [u8; 32] {
        let mut bytes = [0u8; 32];
        bytes[0..4].copy_from_slice(&self.base_x.to_ne_bytes());
        bytes[4..8].copy_from_slice(&self.base_y.to_ne_bytes());
        bytes[8..12].copy_from_slice(&self.overlay_x.to_ne_bytes());
        bytes[12..16].copy_from_slice(&self.overlay_y.to_ne_bytes());
        bytes[16..20].copy_from_slice(&self.opacity.to_ne_bytes());
        bytes[20..24].copy_from_slice(&self._pad0.to_ne_bytes());
        bytes[24..28].copy_from_slice(&self._pad1.to_ne_bytes());
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
    use crate::atlas_runtime::{AtlasStorageRuntime, AtlasTextureConfig};
    use crate::context::{GpuContext, GpuContextInitDescriptor};

    use super::{RenderContext, RenderExecutor};
    use document::{LeafBlendMode, NodeConfig, RenderCmd, RenderSource};
    use glaphica_core::{ATLAS_TILE_SIZE, AtlasLayout, BackendKind, TileKey};
    use thread_protocol::{ClearOp, WriteBlendMode, WriteOp};

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

        let mut executor = RenderExecutor::new();
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

    #[test]
    fn execute_preserves_neighbor_tile_in_same_layer() {
        let Ok(gpu_context) = GpuContext::init_blocking(&GpuContextInitDescriptor::default())
        else {
            eprintln!("skip test: gpu context init failed");
            return;
        };

        let mut atlas_storage = AtlasStorageRuntime::with_capacity(2);
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
            eprintln!("skip test: source atlas backend init failed");
            return;
        }
        if atlas_storage
            .create_backend(
                &gpu_context.device,
                1,
                BackendKind::BranchCache,
                AtlasLayout::Small11,
                AtlasTextureConfig {
                    usage: wgpu::TextureUsages::COPY_DST
                        | wgpu::TextureUsages::COPY_SRC
                        | wgpu::TextureUsages::TEXTURE_BINDING
                        | wgpu::TextureUsages::RENDER_ATTACHMENT,
                    ..Default::default()
                },
            )
            .is_err()
        {
            eprintln!("skip test: destination atlas backend init failed");
            return;
        }

        let src_tile = TileKey::from_parts(0, 0, 0);
        let left_dst = TileKey::from_parts(1, 0, 0);
        let right_dst = TileKey::from_parts(1, 0, 1);
        fill_tile_rgba8(&gpu_context, &atlas_storage, src_tile, [255, 0, 0, 255]);
        fill_tile_rgba8(&gpu_context, &atlas_storage, left_dst, [0, 0, 255, 255]);
        fill_tile_rgba8(&gpu_context, &atlas_storage, right_dst, [0, 255, 0, 255]);

        let mut executor = RenderExecutor::new();
        let mut context = RenderContext {
            gpu_context: &gpu_context,
            atlas_storage: &atlas_storage,
        };
        let render_result = executor.execute(
            &mut context,
            &[RenderCmd {
                from: vec![RenderSource {
                    tile_keys: vec![src_tile],
                    config: NodeConfig {
                        opacity: 1.0,
                        blend_mode: LeafBlendMode::Normal,
                    },
                }],
                to: vec![left_dst],
            }],
        );
        assert!(render_result.is_ok());

        let left_pixel = sample_tile_pixel_rgba8(&gpu_context, &atlas_storage, left_dst);
        let right_pixel = sample_tile_pixel_rgba8(&gpu_context, &atlas_storage, right_dst);

        assert_eq!(left_pixel, [255, 0, 0, 255]);
        assert_eq!(right_pixel, [0, 255, 0, 255]);
    }

    #[test]
    fn execute_preserves_premultiplied_alpha_for_image_tiles() {
        let Ok(gpu_context) = GpuContext::init_blocking(&GpuContextInitDescriptor::default())
        else {
            eprintln!("skip test: gpu context init failed");
            return;
        };

        let mut atlas_storage = AtlasStorageRuntime::with_capacity(2);
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
            eprintln!("skip test: source atlas backend init failed");
            return;
        }
        if atlas_storage
            .create_backend(
                &gpu_context.device,
                1,
                BackendKind::BranchCache,
                AtlasLayout::Small11,
                AtlasTextureConfig {
                    usage: wgpu::TextureUsages::COPY_DST
                        | wgpu::TextureUsages::COPY_SRC
                        | wgpu::TextureUsages::TEXTURE_BINDING
                        | wgpu::TextureUsages::RENDER_ATTACHMENT,
                    ..Default::default()
                },
            )
            .is_err()
        {
            eprintln!("skip test: destination atlas backend init failed");
            return;
        }

        let src_tile = TileKey::from_parts(0, 0, 0);
        let dst_tile = TileKey::from_parts(1, 0, 0);
        fill_tile_rgba8(&gpu_context, &atlas_storage, src_tile, [128, 0, 0, 128]);
        fill_tile_rgba8(&gpu_context, &atlas_storage, dst_tile, [0, 0, 0, 0]);

        let mut executor = RenderExecutor::new();
        let mut context = RenderContext {
            gpu_context: &gpu_context,
            atlas_storage: &atlas_storage,
        };
        let render_result = executor.execute(
            &mut context,
            &[RenderCmd {
                from: vec![RenderSource {
                    tile_keys: vec![src_tile],
                    config: NodeConfig {
                        opacity: 1.0,
                        blend_mode: LeafBlendMode::Normal,
                    },
                }],
                to: vec![dst_tile],
            }],
        );
        assert!(render_result.is_ok());

        let pixel = sample_tile_pixel_rgba8(&gpu_context, &atlas_storage, dst_tile);
        assert_eq!(pixel, [128, 0, 0, 128]);
    }

    #[test]
    fn write_tile_erase_subtracts_alpha_from_origin_snapshot() {
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

        let mask_tile = TileKey::from_parts(0, 0, 0);
        let origin_tile = TileKey::from_parts(0, 0, 1);
        let dst_tile = TileKey::from_parts(0, 0, 2);
        fill_tile_rgba8(&gpu_context, &atlas_storage, mask_tile, [0, 0, 0, 64]);
        fill_tile_rgba8(&gpu_context, &atlas_storage, origin_tile, [128, 0, 0, 128]);
        fill_tile_rgba8(&gpu_context, &atlas_storage, dst_tile, [0, 255, 0, 255]);

        let mut executor = RenderExecutor::new();
        let mut context = RenderContext {
            gpu_context: &gpu_context,
            atlas_storage: &atlas_storage,
        };
        let result = executor.write_tile(
            &mut context,
            &WriteOp {
                src_tile_key: mask_tile,
                dst_tile_key: dst_tile,
                blend_mode: WriteBlendMode::Erase,
                opacity: 1.0,
                rgb: None,
                origin_tile_key: Some(origin_tile),
                frame_merge: thread_protocol::GpuCmdFrameMergeTag::None,
            },
        );
        assert!(result.is_ok());

        let pixel = sample_tile_pixel_rgba8(&gpu_context, &atlas_storage, dst_tile);
        assert_eq!(pixel, [64, 0, 0, 64]);
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
        let _ = gpu_context.device.poll(wgpu::PollType::wait_indefinitely());
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
