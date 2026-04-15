use std::error::Error;
use std::fmt::{Display, Formatter};

use atlas::{AtlasError, AtlasLayout, Backend as AtlasBackend, BackendId, ClearBatch, TileKey};
use glaphica_core::{ATLAS_TILE_SIZE, BlendMode, BrushId, GUTTER_SIZE};

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

const PRESENT_TEXTURE_2D_SHADER: &str = r#"
struct PresentTextureUniforms {
    dst_min_ndc: vec2f,
    dst_max_ndc: vec2f,
};

@group(0) @binding(0) var source_texture: texture_2d<f32>;

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
    let ndc = vec2f(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
    var out: VsOut;
    out.position = vec4f(ndc, 0.0, 1.0);
    out.uv = uv;
    return out;
}

@fragment
fn fs_main(input: VsOut) -> @location(0) vec4f {
    let dims = textureDimensions(source_texture);
    let scaled = min(vec2u(input.uv * vec2f(dims)), dims - vec2u(1u, 1u));
    return textureLoad(source_texture, vec2i(scaled), 0);
}
"#;

#[derive(Debug)]
pub enum TileRendererError {
    Atlas(AtlasError),
    TextureIo(TextureIoError),
    MissingBackendTexture(BackendId),
    InvalidTileKey,
    MissingPresentTarget,
    UnsupportedCommand(&'static str),
    MissingBrushShader {
        brush_id: BrushId,
        stage: BrushShaderStage,
    },
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
            Self::MissingPresentTarget => f.write_str("present command requires a render target"),
            Self::UnsupportedCommand(name) => {
                write!(f, "renderer command {name} is not implemented")
            }
            Self::MissingBrushShader { brush_id, stage } => {
                write!(
                    f,
                    "missing brush shader for brush {} stage {stage:?}",
                    brush_id.raw()
                )
            }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CopyTileCommand {
    pub source_tile_key: TileKey,
    pub destination_tile_key: TileKey,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ApplyDabCommand {
    pub brush_id: BrushId,
    pub destination_tile_key: TileKey,
    pub source_tile_key: Option<TileKey>,
    pub brush_payload: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MergeTileCommand {
    pub brush_id: BrushId,
    pub origin_tile_key: TileKey,
    pub intermediate_tile_key: TileKey,
    pub destination_tile_key: TileKey,
    pub brush_payload: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CompositeTileCommand {
    pub target_tile_key: TileKey,
    pub inputs: Vec<TileCompositeSource>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PresentTileCommand {
    pub source_tile_key: TileKey,
    pub params: PresentTileParams,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrushShaderStage {
    ApplyDab,
    MergeTile,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrushShaderSource {
    pub wgsl: &'static str,
    pub entry_point: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrushShaderSpec {
    pub apply_dab: BrushShaderSource,
    pub merge_tile: BrushShaderSource,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RenderCommand {
    CopyTile(CopyTileCommand),
    ApplyDab(ApplyDabCommand),
    MergeTile(MergeTileCommand),
    CompositeTile(CompositeTileCommand),
    PresentTile(PresentTileCommand),
}

pub trait BrushShaderProvider {
    fn shader_spec(&self, brush_id: BrushId) -> Option<BrushShaderSpec>;
}

pub trait BrushCommandExecutor {
    fn apply_dab(
        &mut self,
        renderer: &mut TileRenderer,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        command: &ApplyDabCommand,
    ) -> Result<(), TileRendererError>;

    fn merge_tile(
        &mut self,
        renderer: &mut TileRenderer,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        command: &MergeTileCommand,
    ) -> Result<(), TileRendererError>;
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
    _pad0: u32,
    source_size: [u32; 2],
    padding: [u32; 6],
}

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
    intermediate_origin: [u32; 2],
    intermediate_layer: u32,
    _pad1: u32,
}

#[derive(Debug, Default)]
struct BrushPipelineSet {
    apply_dab: Vec<Option<wgpu::RenderPipeline>>,
    merge_tile: Vec<Option<wgpu::RenderPipeline>>,
}

pub struct TileRenderer {
    atlas_textures: AtlasTextureSet,
    compose_pipeline: wgpu::RenderPipeline,
    compose_bind_group_layout: wgpu::BindGroupLayout,
    present_bind_group_layout: wgpu::BindGroupLayout,
    present_texture_bind_group_layout: wgpu::BindGroupLayout,
    brush_bind_group_layout: wgpu::BindGroupLayout,
    present_pipelines: Vec<(wgpu::TextureFormat, wgpu::RenderPipeline)>,
    present_texture_pipelines: Vec<(wgpu::TextureFormat, wgpu::RenderPipeline)>,
    brush_pipelines: BrushPipelineSet,
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
        let present_texture_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("glaphica-present-texture-bind-group-layout"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                }],
            });
        let brush_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
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
            present_texture_bind_group_layout,
            brush_bind_group_layout,
            present_pipelines: Vec::new(),
            present_texture_pipelines: Vec::new(),
            brush_pipelines: BrushPipelineSet::default(),
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

    pub fn execute_commands(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        backends: &[&AtlasBackend],
        clear_batches: &[ClearBatch],
        commands: &[RenderCommand],
        present_target: Option<RenderTarget2d<'_>>,
    ) -> Result<(), TileRendererError> {
        struct UnsupportedBrushExecutor;

        impl BrushCommandExecutor for UnsupportedBrushExecutor {
            fn apply_dab(
                &mut self,
                _renderer: &mut TileRenderer,
                _device: &wgpu::Device,
                _queue: &wgpu::Queue,
                _encoder: &mut wgpu::CommandEncoder,
                _command: &ApplyDabCommand,
            ) -> Result<(), TileRendererError> {
                Err(TileRendererError::UnsupportedCommand("ApplyDab"))
            }

            fn merge_tile(
                &mut self,
                _renderer: &mut TileRenderer,
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
        backends: &[&AtlasBackend],
        clear_batches: &[ClearBatch],
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
                RenderCommand::CopyTile(command) => self.encode_copy_tile(
                    device,
                    queue,
                    &mut encoder,
                    command.source_tile_key,
                    command.destination_tile_key,
                )?,
                RenderCommand::ApplyDab(command) => {
                    brush_executor.apply_dab(self, device, queue, &mut encoder, command)?
                }
                RenderCommand::MergeTile(command) => {
                    brush_executor.merge_tile(self, device, queue, &mut encoder, command)?
                }
                RenderCommand::CompositeTile(command) => {
                    self.encode_composite_tile(
                        device,
                        queue,
                        &mut encoder,
                        command.target_tile_key,
                        &command.inputs,
                    )?
                }
                RenderCommand::PresentTile(command) => {
                    let target = present_target.ok_or(TileRendererError::MissingPresentTarget)?;
                    self.encode_present_tile(
                        device,
                        queue,
                        &mut encoder,
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
        backends: &[&AtlasBackend],
        clear_batches: &[ClearBatch],
        commands: &[RenderCommand],
        present_target: Option<RenderTarget2d<'_>>,
        provider: &impl BrushShaderProvider,
    ) -> Result<(), TileRendererError> {
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
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("glaphica-tile-composite-encoder"),
        });
        self.encode_composite_tile(
            device,
            queue,
            &mut encoder,
            target_tile_key,
            inputs,
        )?;
        queue.submit(Some(encoder.finish()));
        Ok(())
    }

    fn encode_composite_tile(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target_tile_key: TileKey,
        inputs: &[TileCompositeSource],
    ) -> Result<(), TileRendererError> {
        let target = self.atlas_textures.resolve_tile(target_tile_key)?;
        self.encode_clear_scratch_texture(
            encoder,
            true,
            wgpu::Color::TRANSPARENT,
        )?;
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
            read_from_a = !read_from_a;
        }

        let final_texture = if read_from_a {
            &self.scratch_a
        } else {
            &self.scratch_b
        };
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
        Ok(())
    }

    fn brush_pipeline(
        &mut self,
        device: &wgpu::Device,
        provider: &impl BrushShaderProvider,
        brush_id: BrushId,
        stage: BrushShaderStage,
    ) -> Result<wgpu::RenderPipeline, TileRendererError> {
        self.ensure_brush_pipeline(device, provider, brush_id, stage)?;
        let pipelines = match stage {
            BrushShaderStage::ApplyDab => &self.brush_pipelines.apply_dab,
            BrushShaderStage::MergeTile => &self.brush_pipelines.merge_tile,
        };
        pipelines
            .get(brush_id.raw() as usize)
            .and_then(Option::as_ref)
            .cloned()
            .ok_or(TileRendererError::MissingBrushShader { brush_id, stage })
    }

    fn ensure_brush_pipeline(
        &mut self,
        device: &wgpu::Device,
        provider: &impl BrushShaderProvider,
        brush_id: BrushId,
        stage: BrushShaderStage,
    ) -> Result<(), TileRendererError> {
        let index = brush_id.raw() as usize;
        let pipeline_slots = match stage {
            BrushShaderStage::ApplyDab => &mut self.brush_pipelines.apply_dab,
            BrushShaderStage::MergeTile => &mut self.brush_pipelines.merge_tile,
        };
        if pipeline_slots.len() <= index {
            pipeline_slots.resize_with(index + 1, || None);
        }
        if pipeline_slots[index].is_some() {
            return Ok(());
        }

        let shader_spec = provider
            .shader_spec(brush_id)
            .ok_or(TileRendererError::MissingBrushShader { brush_id, stage })?;
        let shader_source = match stage {
            BrushShaderStage::ApplyDab => shader_spec.apply_dab,
            BrushShaderStage::MergeTile => shader_spec.merge_tile,
        };
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("glaphica-brush-shader"),
            source: wgpu::ShaderSource::Wgsl(shader_source.wgsl.into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("glaphica-brush-pipeline-layout"),
            bind_group_layouts: &[&self.brush_bind_group_layout],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("glaphica-brush-pipeline"),
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
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        pipeline_slots[index] = Some(pipeline);
        Ok(())
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

    fn encode_apply_dab(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        provider: &impl BrushShaderProvider,
        command: &ApplyDabCommand,
    ) -> Result<(), TileRendererError> {
        let pipeline = self.brush_pipeline(
            device,
            provider,
            command.brush_id,
            BrushShaderStage::ApplyDab,
        )?;
        let destination = self
            .atlas_textures
            .resolve_tile(command.destination_tile_key)?;
        let source_tile_key = command
            .source_tile_key
            .unwrap_or(command.destination_tile_key);
        let source = self.atlas_textures.resolve_tile(source_tile_key)?;
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
                texture: &self.scratch_b.texture,
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
            layout: &self.brush_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&self.scratch_b.view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&self.scratch_b.view),
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
            pass.set_pipeline(&pipeline);
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

    fn encode_merge_tile(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        provider: &impl BrushShaderProvider,
        command: &MergeTileCommand,
    ) -> Result<(), TileRendererError> {
        let pipeline = self.brush_pipeline(
            device,
            provider,
            command.brush_id,
            BrushShaderStage::MergeTile,
        )?;
        let intermediate = self
            .atlas_textures
            .resolve_tile(command.intermediate_tile_key)?;
        let destination = self
            .atlas_textures
            .resolve_tile(command.destination_tile_key)?;
        let (origin_texture_view, origin_origin, origin_layer) =
            if command.origin_tile_key == TileKey::EMPTY {
                self.encode_clear_scratch_texture(
                    encoder,
                    false,
                    wgpu::Color::TRANSPARENT,
                )?;
                (&self.scratch_b.view, [0, 0], 0)
            } else {
                let origin = self.atlas_textures.resolve_tile(command.origin_tile_key)?;
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
            intermediate_origin: [
                intermediate.tile_x * ATLAS_TILE_SIZE,
                intermediate.tile_y * ATLAS_TILE_SIZE,
            ],
            intermediate_layer: intermediate.layer,
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
            layout: &self.brush_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(origin_texture_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&intermediate.texture.view),
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
        self.encode_copy_tile(
            device,
            queue,
            &mut encoder,
            source_tile_key,
            destination_tile_key,
        )?;
        queue.submit(Some(encoder.finish()));
        Ok(())
    }

    fn encode_copy_tile(
        &mut self,
        _device: &wgpu::Device,
        _queue: &wgpu::Queue,
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

    pub fn present_tile(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        source_tile_key: TileKey,
        params: PresentTileParams,
        target: RenderTarget2d<'_>,
    ) -> Result<(), TileRendererError> {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("glaphica-tile-present-encoder"),
        });
        self.encode_present_tile(
            device,
            queue,
            &mut encoder,
            source_tile_key,
            params,
            target,
        )?;
        queue.submit(Some(encoder.finish()));
        Ok(())
    }

    pub fn present_texture_2d(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        source: &RendererTexture,
        target: RenderTarget2d<'_>,
    ) -> Result<(), TileRendererError> {
        let source_view = source.create_layer_view(0)?;
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("glaphica-present-texture-bind-group"),
            layout: &self.present_texture_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&source_view),
            }],
        });
        let pipeline = self.present_texture_pipeline(device, target.format);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("glaphica-present-texture-encoder"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("glaphica-present-texture-pass"),
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

    fn encode_present_tile(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
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
            _pad0: 0,
            source_size: [params.source_width, params.source_height],
            padding: [0; 6],
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

    fn present_texture_pipeline(
        &mut self,
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
    ) -> &wgpu::RenderPipeline {
        if let Some(index) = self
            .present_texture_pipelines
            .iter()
            .position(|(candidate, _)| *candidate == format)
        {
            return &self.present_texture_pipelines[index].1;
        }

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("glaphica-present-texture-shader"),
            source: wgpu::ShaderSource::Wgsl(PRESENT_TEXTURE_2D_SHADER.into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("glaphica-present-texture-pipeline-layout"),
            bind_group_layouts: &[&self.present_texture_bind_group_layout],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("glaphica-present-texture-pipeline"),
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
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        self.present_texture_pipelines.push((format, pipeline));
        let last = self.present_texture_pipelines.len() - 1;
        &self.present_texture_pipelines[last].1
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

struct RegisteredBrushExecutor<'a, Provider> {
    provider: &'a Provider,
}

impl<Provider> BrushCommandExecutor for RegisteredBrushExecutor<'_, Provider>
where
    Provider: BrushShaderProvider,
{
    fn apply_dab(
        &mut self,
        renderer: &mut TileRenderer,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        command: &ApplyDabCommand,
    ) -> Result<(), TileRendererError> {
        renderer.encode_apply_dab(device, queue, encoder, self.provider, command)
    }

    fn merge_tile(
        &mut self,
        renderer: &mut TileRenderer,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        command: &MergeTileCommand,
    ) -> Result<(), TileRendererError> {
        renderer.encode_merge_tile(device, queue, encoder, self.provider, command)
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
    use bytemuck::bytes_of;

    use super::{PresentTileParams, PresentUniforms, TileCompositeSource};
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
