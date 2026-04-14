use std::error::Error;
use std::fmt::{Display, Formatter};

use atlas::{AtlasError, AtlasLayout, Backend as AtlasBackend, BackendId, ClearBatch, TileKey};
use glaphica_core::{ATLAS_TILE_SIZE, BlendMode, GUTTER_SIZE};

use crate::{RendererTexture, RendererTextureDescriptor, TextureIoError};

const COMPOSITE_SHADER: &str = r#"
struct CompositeUniforms {
    source_origin: vec2u,
    source_layer: u32,
    blend_mode: u32,
    opacity: f32,
    _padding: vec3u,
};

@group(0) @binding(0) var accum_texture: texture_2d<f32>;
@group(0) @binding(1) var source_texture: texture_2d_array<f32>;
@group(0) @binding(2) var<uniform> uniforms: CompositeUniforms;

struct VsOut {
    @builtin(position) position: vec4f,
};

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VsOut {
    let positions = array<vec2f, 3>(
        vec2f(-1.0, -3.0),
        vec2f(-1.0, 1.0),
        vec2f(3.0, 1.0),
    );
    var out: VsOut;
    out.position = vec4f(positions[vertex_index], 0.0, 1.0);
    return out;
}

fn unpremultiply(color: vec4f) -> vec3f {
    if color.a <= 0.0 {
        return vec3f(0.0);
    }
    return color.rgb / color.a;
}

fn blend_color(backdrop: vec3f, source: vec3f, blend_mode: u32) -> vec3f {
    if blend_mode == 1u {
        return backdrop * source;
    }
    return source;
}

@fragment
fn fs_main(@builtin(position) position: vec4f) -> @location(0) vec4f {
    let pixel = vec2u(position.xy);
    let backdrop = textureLoad(accum_texture, vec2i(pixel), 0);
    var source = textureLoad(
        source_texture,
        vec2i(uniforms.source_origin + pixel),
        i32(uniforms.source_layer),
        0
    );
    source *= uniforms.opacity;

    let backdrop_alpha = clamp(backdrop.a, 0.0, 1.0);
    let source_alpha = clamp(source.a, 0.0, 1.0);
    let backdrop_rgb = unpremultiply(backdrop);
    let source_rgb = unpremultiply(source);
    let blended_rgb = blend_color(backdrop_rgb, source_rgb, uniforms.blend_mode);
    let out_alpha = source_alpha + backdrop_alpha * (1.0 - source_alpha);
    let out_rgb =
        (1.0 - source_alpha) * backdrop_alpha * backdrop_rgb
        + (1.0 - backdrop_alpha) * source_alpha * source_rgb
        + backdrop_alpha * source_alpha * blended_rgb;
    return vec4f(out_rgb, out_alpha);
}
"#;

const PRESENT_SHADER: &str = r#"
struct PresentUniforms {
    dst_min_ndc: vec2f,
    dst_max_ndc: vec2f,
    source_origin: vec2u,
    source_layer: u32,
    source_size: vec2u,
    _padding: vec3u,
};

@group(0) @binding(0) var source_texture: texture_2d_array<f32>;
@group(0) @binding(1) var<uniform> uniforms: PresentUniforms;

struct VsOut {
    @builtin(position) position: vec4f,
    @location(0) uv: vec2f,
};

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VsOut {
    let positions = array<vec2f, 6>(
        vec2f(0.0, 0.0),
        vec2f(1.0, 0.0),
        vec2f(0.0, 1.0),
        vec2f(0.0, 1.0),
        vec2f(1.0, 0.0),
        vec2f(1.0, 1.0),
    );
    let uv = positions[vertex_index];
    let ndc = mix(uniforms.dst_min_ndc, uniforms.dst_max_ndc, uv);
    var out: VsOut;
    out.position = vec4f(ndc, 0.0, 1.0);
    out.uv = uv;
    return out;
}

@fragment
fn fs_main(input: VsOut) -> @location(0) vec4f {
    let scaled = min(
        vec2u(input.uv * vec2f(uniforms.source_size)),
        uniforms.source_size - vec2u(1u, 1u)
    );
    return textureLoad(
        source_texture,
        vec2i(uniforms.source_origin + scaled),
        i32(uniforms.source_layer),
        0
    );
}
"#;

#[derive(Debug)]
pub enum TileRendererError {
    Atlas(AtlasError),
    TextureIo(TextureIoError),
    MissingBackendTexture(BackendId),
    InvalidTileKey,
}

impl Display for TileRendererError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Atlas(error) => Display::fmt(error, f),
            Self::TextureIo(error) => Display::fmt(error, f),
            Self::MissingBackendTexture(backend_id) => {
                write!(
                    f,
                    "missing renderer texture for atlas backend {}",
                    backend_id.raw()
                )
            }
            Self::InvalidTileKey => f.write_str("invalid tile key"),
        }
    }
}

impl Error for TileRendererError {}

impl From<AtlasError> for TileRendererError {
    fn from(error: AtlasError) -> Self {
        Self::Atlas(error)
    }
}

impl From<TextureIoError> for TileRendererError {
    fn from(error: TextureIoError) -> Self {
        Self::TextureIo(error)
    }
}

#[derive(Debug)]
struct AtlasBackendTexture {
    layout: AtlasLayout,
    texture: RendererTexture,
}

#[derive(Debug, Default)]
struct AtlasTextureSet {
    backends: Vec<Option<AtlasBackendTexture>>,
}

#[derive(Debug, Clone, Copy)]
struct ResolvedAtlasTile<'a> {
    texture: &'a RendererTexture,
    layer: u32,
    tile_x: u32,
    tile_y: u32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TileCompositeSource {
    pub tile_key: TileKey,
    pub opacity: f32,
    pub blend_mode: BlendMode,
}

#[derive(Debug, Clone, Copy)]
pub struct RenderTarget2d<'a> {
    pub view: &'a wgpu::TextureView,
    pub format: wgpu::TextureFormat,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PresentTileParams {
    pub target_min_px: [f32; 2],
    pub target_max_px: [f32; 2],
    pub source_width: u32,
    pub source_height: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CompositeUniforms {
    source_origin: [u32; 2],
    source_layer: u32,
    blend_mode: u32,
    opacity: f32,
    padding: [u32; 7],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct PresentUniforms {
    dst_min_ndc: [f32; 2],
    dst_max_ndc: [f32; 2],
    source_origin: [u32; 2],
    source_layer: u32,
    source_size: [u32; 2],
    padding: [u32; 7],
}

pub struct TileRenderer {
    atlas_textures: AtlasTextureSet,
    compose_pipeline: wgpu::RenderPipeline,
    compose_bind_group_layout: wgpu::BindGroupLayout,
    present_bind_group_layout: wgpu::BindGroupLayout,
    present_pipelines: Vec<(wgpu::TextureFormat, wgpu::RenderPipeline)>,
    scratch_a: RendererTexture,
    scratch_b: RendererTexture,
}

impl AtlasTextureSet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn ensure_backend_texture(
        &mut self,
        device: &wgpu::Device,
        backend: &AtlasBackend,
    ) -> Result<&AtlasBackendTexture, TileRendererError> {
        let backend_id = backend.backend_id()?;
        let index = backend_id.raw() as usize;
        if self.backends.len() <= index {
            self.backends.resize_with(index + 1, || None);
        }
        if self.backends[index].is_none() {
            let layout = backend.layout()?;
            let edge = layout.tiles_per_edge() * ATLAS_TILE_SIZE;
            let texture = RendererTexture::new(
                device,
                &RendererTextureDescriptor::atlas_rgba8_unorm(None, edge, edge, layout.layers()),
            )?;
            self.backends[index] = Some(AtlasBackendTexture { layout, texture });
        }
        self.backends[index]
            .as_ref()
            .ok_or(TileRendererError::MissingBackendTexture(backend_id))
    }

    pub fn backend_texture(&self, backend_id: BackendId) -> Option<&AtlasBackendTexture> {
        self.backends
            .get(backend_id.raw() as usize)
            .and_then(Option::as_ref)
    }

    pub fn resolve_tile(
        &self,
        tile_key: TileKey,
    ) -> Result<ResolvedAtlasTile<'_>, TileRendererError> {
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

impl TileRenderer {
    pub fn new(device: &wgpu::Device) -> Result<Self, TileRendererError> {
        let compose_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("glaphica-tile-composite-shader"),
            source: wgpu::ShaderSource::Wgsl(COMPOSITE_SHADER.into()),
        });
        let compose_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("glaphica-tile-composite-bind-group-layout"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: false },
                            view_dimension: wgpu::TextureViewDimension::D2,
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
                ],
            });
        let compose_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("glaphica-tile-composite-pipeline-layout"),
                bind_group_layouts: &[&compose_bind_group_layout],
                immediate_size: 0,
            });
        let compose_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("glaphica-tile-composite-pipeline"),
            layout: Some(&compose_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &compose_shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &compose_shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let present_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("glaphica-tile-present-bind-group-layout"),
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
                        visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                ],
            });
        let scratch_descriptor = RendererTextureDescriptor {
            label: Some("glaphica-tile-scratch"),
            width: ATLAS_TILE_SIZE,
            height: ATLAS_TILE_SIZE,
            layers: 1,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::RENDER_ATTACHMENT,
        };

        Ok(Self {
            atlas_textures: AtlasTextureSet::new(),
            compose_pipeline,
            compose_bind_group_layout,
            present_bind_group_layout,
            present_pipelines: Vec::new(),
            scratch_a: RendererTexture::new(device, &scratch_descriptor)?,
            scratch_b: RendererTexture::new(device, &scratch_descriptor)?,
        })
    }

    pub fn ensure_backend(
        &mut self,
        device: &wgpu::Device,
        backend: &AtlasBackend,
    ) -> Result<(), TileRendererError> {
        self.atlas_textures
            .ensure_backend_texture(device, backend)?;
        Ok(())
    }

    pub fn apply_clear_batches(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        backends: &[&AtlasBackend],
        clear_batches: &[ClearBatch],
    ) -> Result<(), TileRendererError> {
        for backend in backends {
            self.atlas_textures
                .ensure_backend_texture(device, backend)?;
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
        backend: &AtlasBackend,
        tile_key: TileKey,
        pixels_rgba8: &[u8],
    ) -> Result<(), TileRendererError> {
        self.atlas_textures
            .ensure_backend_texture(device, backend)?;
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

    pub fn clear_render_target(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target: RenderTarget2d<'_>,
        color: wgpu::Color,
    ) {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("glaphica-clear-render-target-encoder"),
        });
        {
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("glaphica-clear-render-target-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target.view,
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
        queue.submit(Some(encoder.finish()));
    }

    pub fn composite_tile(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target_tile_key: TileKey,
        inputs: &[TileCompositeSource],
    ) -> Result<(), TileRendererError> {
        let target = self.atlas_textures.resolve_tile(target_tile_key)?;
        self.clear_scratch_texture(device, queue, true, wgpu::Color::TRANSPARENT);
        let mut read_from_a = true;

        for input in inputs {
            if input.tile_key == TileKey::EMPTY || input.opacity <= 0.0 {
                continue;
            }
            let source = self.atlas_textures.resolve_tile(input.tile_key)?;
            let uniforms = CompositeUniforms {
                source_origin: [
                    source.tile_x * ATLAS_TILE_SIZE,
                    source.tile_y * ATLAS_TILE_SIZE,
                ],
                source_layer: source.layer,
                blend_mode: encode_blend_mode(input.blend_mode),
                opacity: input.opacity,
                padding: [0; 7],
            };
            let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("glaphica-tile-composite-uniform"),
                size: std::mem::size_of::<CompositeUniforms>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            queue.write_buffer(&uniform_buffer, 0, bytemuck::bytes_of(&uniforms));

            let accum_texture = if read_from_a {
                &self.scratch_a
            } else {
                &self.scratch_b
            };
            let dst_texture = if read_from_a {
                &self.scratch_b
            } else {
                &self.scratch_a
            };
            let accum_view = accum_texture.create_layer_view(0)?;
            let dst_view = dst_texture.create_layer_view(0)?;
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("glaphica-tile-composite-bind-group"),
                layout: &self.compose_bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&accum_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&source.texture.view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: uniform_buffer.as_entire_binding(),
                    },
                ],
            });
            let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("glaphica-tile-composite-encoder"),
            });
            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("glaphica-tile-composite-pass"),
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
                pass.set_pipeline(&self.compose_pipeline);
                pass.set_bind_group(0, &bind_group, &[]);
                pass.draw(0..3, 0..1);
            }
            queue.submit(Some(encoder.finish()));
            read_from_a = !read_from_a;
        }

        let final_texture = if read_from_a {
            &self.scratch_a
        } else {
            &self.scratch_b
        };
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("glaphica-tile-copy-to-atlas-encoder"),
        });
        encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &final_texture.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture: &target.texture.texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: target.tile_x * ATLAS_TILE_SIZE,
                    y: target.tile_y * ATLAS_TILE_SIZE,
                    z: target.layer,
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width: ATLAS_TILE_SIZE,
                height: ATLAS_TILE_SIZE,
                depth_or_array_layers: 1,
            },
        );
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
        let source = self.atlas_textures.resolve_tile(source_tile_key)?;
        let uniform = PresentUniforms {
            dst_min_ndc: screen_to_ndc(params.target_min_px, target.width, target.height),
            dst_max_ndc: screen_to_ndc(params.target_max_px, target.width, target.height),
            source_origin: [
                source.tile_x * ATLAS_TILE_SIZE + GUTTER_SIZE,
                source.tile_y * ATLAS_TILE_SIZE + GUTTER_SIZE,
            ],
            source_layer: source.layer,
            source_size: [params.source_width, params.source_height],
            padding: [0; 7],
        };
        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("glaphica-tile-present-uniform"),
            size: std::mem::size_of::<PresentUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&uniform_buffer, 0, bytemuck::bytes_of(&uniform));
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("glaphica-tile-present-bind-group"),
            layout: &self.present_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&source.texture.view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: uniform_buffer.as_entire_binding(),
                },
            ],
        });
        let pipeline = self.present_pipeline(device, target.format);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("glaphica-tile-present-encoder"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("glaphica-tile-present-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target.view,
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
            pass.draw(0..6, 0..1);
        }
        queue.submit(Some(encoder.finish()));
        Ok(())
    }

    fn present_pipeline(
        &mut self,
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
    ) -> &wgpu::RenderPipeline {
        if let Some(index) = self
            .present_pipelines
            .iter()
            .position(|(candidate, _)| *candidate == format)
        {
            return &self.present_pipelines[index].1;
        }

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("glaphica-tile-present-shader"),
            source: wgpu::ShaderSource::Wgsl(PRESENT_SHADER.into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("glaphica-tile-present-pipeline-layout"),
            bind_group_layouts: &[&self.present_bind_group_layout],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("glaphica-tile-present-pipeline"),
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
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        self.present_pipelines.push((format, pipeline));
        let last = self.present_pipelines.len() - 1;
        &self.present_pipelines[last].1
    }

    fn clear_scratch_texture(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        clear_a: bool,
        color: wgpu::Color,
    ) {
        let texture = if clear_a {
            &self.scratch_a
        } else {
            &self.scratch_b
        };
        let Ok(texture_view) = texture.create_layer_view(0) else {
            return;
        };
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("glaphica-tile-clear-scratch-encoder"),
        });
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
        queue.submit(Some(encoder.finish()));
    }
}

fn screen_to_ndc(point: [f32; 2], width: u32, height: u32) -> [f32; 2] {
    [
        point[0] / width as f32 * 2.0 - 1.0,
        1.0 - point[1] / height as f32 * 2.0,
    ]
}

fn encode_blend_mode(blend_mode: BlendMode) -> u32 {
    match blend_mode {
        BlendMode::Normal => 0,
        BlendMode::Multiply => 1,
    }
}

#[cfg(test)]
mod tests {
    use atlas::TileKey;

    use super::{PresentTileParams, TileCompositeSource};
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
}
