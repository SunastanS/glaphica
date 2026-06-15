use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt::{Display, Formatter};

use atlas::{AtlasLayout, AtlasTextureStore, TilePos};
use gla_color::{
    BlendMode, CompositeKind, GlaFormat, RgbaBlendMode, ValueToRgbaBlendMode, composite_kind,
};
use gla_core::{ATLAS_TILE_SIZE, GUTTER_SIZE, IMAGE_TILE_SIZE};
use gla_draw_on::{DrawOnInvocation, DrawOnToolKind, DrawOnToolSpec};
use wgpu::util::DeviceExt;

use crate::texture::{
    RendererTexture, RendererTextureDescriptor, TextureFormatRuntime, TextureResourceError,
    runtime_format,
};
use crate::{
    Pass, PresentTarget, PresentTile, RenderBackend, RendererCapabilities, RendererDrawOnInvocation,
};

const RGBA_COMPOSITE_SHADER: &str = r#"
struct CompositeUniforms {
    source_origin: vec2u,
    source_layer: u32,
    blend_mode: u32,
    opacity: f32,
};

@group(0) @binding(0) var backdrop_texture: texture_2d<f32>;
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

fn overlay_channel(backdrop: f32, source: f32) -> f32 {
    if backdrop <= 0.5 {
        return 2.0 * backdrop * source;
    }
    return 1.0 - 2.0 * (1.0 - backdrop) * (1.0 - source);
}

fn blend_color(backdrop: vec3f, source: vec3f, blend_mode: u32) -> vec3f {
    if blend_mode == 0u {
        return source;
    }
    if blend_mode == 2u {
        return backdrop * source;
    }
    return vec3f(
        overlay_channel(backdrop.r, source.r),
        overlay_channel(backdrop.g, source.g),
        overlay_channel(backdrop.b, source.b)
    );
}

@fragment
fn fs_main(@builtin(position) position: vec4f) -> @location(0) vec4f {
    let pixel = vec2u(position.xy);
    let backdrop = textureLoad(backdrop_texture, vec2i(pixel), 0);
    var source = textureLoad(
        source_texture,
        vec2i(uniforms.source_origin + pixel),
        i32(uniforms.source_layer),
        0
    );
    source *= clamp(uniforms.opacity, 0.0, 1.0);

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

const VALUE_MASK_SHADER: &str = r#"
struct CompositeUniforms {
    source_origin: vec2u,
    source_layer: u32,
    blend_mode: u32,
    opacity: f32,
};

@group(0) @binding(0) var color_texture: texture_2d<f32>;
@group(0) @binding(1) var value_texture: texture_2d_array<f32>;
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

@fragment
fn fs_main(@builtin(position) position: vec4f) -> @location(0) vec4f {
    let pixel = vec2u(position.xy);
    let color = textureLoad(color_texture, vec2i(pixel), 0);
    let value = textureLoad(
        value_texture,
        vec2i(uniforms.source_origin + pixel),
        i32(uniforms.source_layer),
        0
    ).r;
    let factor = clamp(value * uniforms.opacity, 0.0, 1.0);
    return color * factor;
}
"#;

const RADIAL_KERNEL_1D_SHADER: &str = r#"
requires readonly_and_readwrite_storage_textures;

const IMAGE_TILE_SIZE: u32 = 62u;
const ATLAS_TILE_SIZE: u32 = 64u;
const GUTTER_SIZE: u32 = 1u;

struct TileProgram {
    tile_x: u32,
    tile_y: u32,
    instance_start: u32,
    instance_count: u32,
};

struct RadialInstance {
    center_in_tile: vec2f,
    radius_px: f32,
    amplitude: f32,
};

@group(0) @binding(0) var dst_texture: texture_storage_2d<r32float, read_write>;
@group(0) @binding(1) var<storage, read> tile_programs: array<TileProgram>;
@group(0) @binding(2) var<storage, read> instances: array<RadialInstance>;

@compute @workgroup_size(8, 8, 1)
fn cs_main(@builtin(global_invocation_id) id: vec3u) {
    if id.x >= IMAGE_TILE_SIZE || id.y >= IMAGE_TILE_SIZE || id.z >= arrayLength(&tile_programs) {
        return;
    }

    let program = tile_programs[id.z];
    let coord = vec2i(
        i32(program.tile_x * ATLAS_TILE_SIZE + GUTTER_SIZE + id.x),
        i32(program.tile_y * ATLAS_TILE_SIZE + GUTTER_SIZE + id.y)
    );
    let pixel = vec2f(f32(id.x) + 0.5, f32(id.y) + 0.5);
    var value = textureLoad(dst_texture, coord).r;

    for (var i = 0u; i < program.instance_count; i = i + 1u) {
        let instance = instances[program.instance_start + i];
        if instance.radius_px > 0.0 && instance.amplitude > 0.0 {
            let d = distance(pixel, instance.center_in_tile);
            if d <= instance.radius_px {
                value = value + max(0.0, 1.0 - d / max(instance.radius_px, 1.0)) * instance.amplitude;
            }
        }
    }

    textureStore(dst_texture, coord, vec4f(value, 0.0, 0.0, 1.0));
}
"#;

const REPLACE_CIRCLE_4D_SHADER: &str = r#"
const IMAGE_TILE_SIZE: u32 = 62u;
const ATLAS_TILE_SIZE: u32 = 64u;
const GUTTER_SIZE: u32 = 1u;

struct TileProgram {
    tile_x: u32,
    tile_y: u32,
    instance_start: u32,
    instance_count: u32,
};

struct ReplaceInstance {
    center_in_tile: vec2f,
    radius_px: f32,
    _pad0: f32,
    color: vec4f,
};

@group(0) @binding(0) var dst_texture: texture_storage_2d<rgba32float, write>;
@group(0) @binding(1) var<storage, read> tile_programs: array<TileProgram>;
@group(0) @binding(2) var<storage, read> instances: array<ReplaceInstance>;

@compute @workgroup_size(8, 8, 1)
fn cs_main(@builtin(global_invocation_id) id: vec3u) {
    if id.x >= IMAGE_TILE_SIZE || id.y >= IMAGE_TILE_SIZE || id.z >= arrayLength(&tile_programs) {
        return;
    }

    let program = tile_programs[id.z];
    let coord = vec2i(
        i32(program.tile_x * ATLAS_TILE_SIZE + GUTTER_SIZE + id.x),
        i32(program.tile_y * ATLAS_TILE_SIZE + GUTTER_SIZE + id.y)
    );
    let pixel = vec2f(f32(id.x) + 0.5, f32(id.y) + 0.5);
    var hit = false;
    var color = vec4f(0.0);

    for (var i = 0u; i < program.instance_count; i = i + 1u) {
        let instance = instances[program.instance_start + i];
        if instance.radius_px > 0.0 && distance(pixel, instance.center_in_tile) <= instance.radius_px {
            hit = true;
            color = instance.color;
        }
    }

    if hit {
        textureStore(dst_texture, coord, color);
    }
}
"#;

const TILE_PRESENT_SHADER: &str = r#"
struct PresentUniforms {
    dst_min_ndc: vec2f,
    dst_max_ndc: vec2f,
    source_origin: vec2u,
    source_layer: u32,
    _pad0: u32,
    source_size: vec2u,
    _padding: vec2u,
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

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CompositeUniforms {
    source_origin: [u32; 2],
    source_layer: u32,
    blend_mode: u32,
    opacity: f32,
    _pad0: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct TileProgram {
    tile_x: u32,
    tile_y: u32,
    instance_start: u32,
    instance_count: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct RadialInstance {
    center_in_tile: [f32; 2],
    radius_px: f32,
    amplitude: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct ReplaceInstance {
    center_in_tile: [f32; 2],
    radius_px: f32,
    _pad0: f32,
    color: [f32; 4],
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
    _padding: [u32; 2],
}

#[derive(Debug)]
pub enum GpuRendererError {
    UnsupportedTextureFormat(GlaFormat),
    InvalidTextureExtent {
        width: u32,
        height: u32,
    },
    InvalidTextureLayerCount(u32),
    TextureLayerOutOfBounds {
        layer: u32,
        layers: u32,
    },
    InvalidAtlasLayout {
        layout: AtlasLayout,
    },
    UnsupportedTileTransferFormat {
        bytes_per_pixel: u32,
    },
    TileTransferFormatMismatch {
        format: GlaFormat,
        bytes_per_pixel: u32,
    },
    InvalidTileTransferLength {
        expected: usize,
        actual: usize,
    },
    MissingAtlas {
        atlas_id: u8,
    },
    AtlasTextureMismatch {
        atlas_id: u8,
        expected_layout: atlas::AtlasLayout,
        actual_layout: atlas::AtlasLayout,
        expected_format: GlaFormat,
        actual_format: GlaFormat,
    },
    InvalidTilePosition(TilePos),
    TileFormatMismatch {
        src: GlaFormat,
        dst: GlaFormat,
    },
    UnsupportedComposite {
        src: GlaFormat,
        dst: GlaFormat,
        blend_mode: BlendMode,
    },
    MissingDrawOnFeature {
        tool: DrawOnToolKind,
        format: wgpu::TextureFormat,
        feature: &'static str,
    },
    MissingDrawOnPipeline {
        tool: DrawOnToolKind,
    },
    DrawOnFormatMismatch {
        tool: DrawOnToolKind,
        format: GlaFormat,
    },
    ReadbackPollFailed,
    ReadbackChannelClosed,
    ReadbackMapFailed,
}

impl Display for GpuRendererError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedTextureFormat(format) => {
                write!(f, "unsupported renderer texture format {format:?}")
            }
            Self::InvalidTextureExtent { width, height } => {
                write!(f, "invalid renderer texture extent {width}x{height}")
            }
            Self::InvalidTextureLayerCount(layers) => {
                write!(f, "invalid renderer texture layer count {layers}")
            }
            Self::TextureLayerOutOfBounds { layer, layers } => {
                write!(
                    f,
                    "renderer texture layer {layer} out of bounds for {layers} layers"
                )
            }
            Self::InvalidAtlasLayout { layout } => {
                write!(f, "invalid atlas texture layout {layout:?}")
            }
            Self::UnsupportedTileTransferFormat { bytes_per_pixel } => write!(
                f,
                "unsupported tile transfer format with {bytes_per_pixel} bytes per pixel"
            ),
            Self::TileTransferFormatMismatch {
                format,
                bytes_per_pixel,
            } => write!(
                f,
                "cannot transfer {format:?} tile using {bytes_per_pixel} bytes per pixel"
            ),
            Self::InvalidTileTransferLength { expected, actual } => {
                write!(f, "tile transfer has {actual} bytes, expected {expected}")
            }
            Self::MissingAtlas { atlas_id } => {
                write!(f, "missing GPU texture for atlas {atlas_id}")
            }
            Self::AtlasTextureMismatch {
                atlas_id,
                expected_layout,
                actual_layout,
                expected_format,
                actual_format,
            } => write!(
                f,
                "atlas {atlas_id} GPU texture mismatch: expected {expected_layout:?} {expected_format:?}, got {actual_layout:?} {actual_format:?}"
            ),
            Self::InvalidTilePosition(position) => {
                write!(f, "invalid tile position {position:?}")
            }
            Self::TileFormatMismatch { src, dst } => {
                write!(f, "cannot copy tile from {src:?} into {dst:?}")
            }
            Self::UnsupportedComposite {
                src,
                dst,
                blend_mode,
            } => {
                write!(
                    f,
                    "unsupported render_to composite from {src:?} into {dst:?} with {blend_mode:?}"
                )
            }
            Self::MissingDrawOnFeature {
                tool,
                format,
                feature,
            } => {
                write!(f, "{tool:?} requires {format:?} texture feature {feature}")
            }
            Self::MissingDrawOnPipeline { tool } => {
                write!(f, "DrawOn pipeline for {tool:?} was not registered")
            }
            Self::DrawOnFormatMismatch { tool, format } => {
                write!(f, "{tool:?} cannot draw into atlas format {format:?}")
            }
            Self::ReadbackPollFailed => write!(f, "GPU readback polling failed"),
            Self::ReadbackChannelClosed => write!(f, "GPU readback channel closed"),
            Self::ReadbackMapFailed => write!(f, "GPU readback buffer mapping failed"),
        }
    }
}

impl Error for GpuRendererError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        None
    }
}

impl From<TextureResourceError> for GpuRendererError {
    fn from(error: TextureResourceError) -> Self {
        match error {
            TextureResourceError::UnsupportedFormat(format) => {
                Self::UnsupportedTextureFormat(format)
            }
            TextureResourceError::InvalidExtent { width, height } => {
                Self::InvalidTextureExtent { width, height }
            }
            TextureResourceError::InvalidLayerCount(layers) => {
                Self::InvalidTextureLayerCount(layers)
            }
            TextureResourceError::LayerOutOfBounds { layer, layers } => {
                Self::TextureLayerOutOfBounds { layer, layers }
            }
        }
    }
}

#[derive(Debug)]
struct AtlasTexture {
    layout: atlas::AtlasLayout,
    format: GlaFormat,
    runtime: TextureFormatRuntime,
    texture: RendererTexture,
    layer_views: Vec<wgpu::TextureView>,
}

#[derive(Debug, Clone, Copy)]
struct ResolvedTile<'a> {
    atlas_id: u8,
    format: GlaFormat,
    runtime: TextureFormatRuntime,
    texture: &'a RendererTexture,
    layer_views: &'a [wgpu::TextureView],
    origin: wgpu::Origin3d,
}

#[derive(Debug, Default)]
struct AtlasTextureSet {
    atlases: Vec<Option<AtlasTexture>>,
}

#[derive(Debug)]
struct TileTransferBuffer {
    bytes_per_pixel: u32,
    padded_bytes_per_row: u32,
    buffer: wgpu::Buffer,
}

impl TileTransferBuffer {
    fn layout(&self) -> wgpu::TexelCopyBufferLayout {
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(self.padded_bytes_per_row),
            rows_per_image: Some(ATLAS_TILE_SIZE),
        }
    }
}

#[derive(Debug)]
struct TileTransferBuffers {
    zero: Vec<TileTransferBuffer>,
    staging: Vec<TileTransferBuffer>,
}

impl TileTransferBuffers {
    const BYTES_PER_PIXEL: [u32; 5] = [1, 2, 4, 8, 16];

    fn new(device: &wgpu::Device) -> Result<Self, GpuRendererError> {
        let mut zero = Vec::with_capacity(Self::BYTES_PER_PIXEL.len());
        let mut staging = Vec::with_capacity(Self::BYTES_PER_PIXEL.len());
        for bytes_per_pixel in Self::BYTES_PER_PIXEL {
            zero.push(create_zero_tile_buffer(device, bytes_per_pixel)?);
            staging.push(create_staging_tile_buffer(device, bytes_per_pixel)?);
        }
        Ok(Self { zero, staging })
    }

    fn zero_for(&self, bytes_per_pixel: u32) -> Result<&TileTransferBuffer, GpuRendererError> {
        Self::find(&self.zero, bytes_per_pixel)
    }

    fn staging_for(&self, bytes_per_pixel: u32) -> Result<&TileTransferBuffer, GpuRendererError> {
        Self::find(&self.staging, bytes_per_pixel)
    }

    fn find(
        buffers: &[TileTransferBuffer],
        bytes_per_pixel: u32,
    ) -> Result<&TileTransferBuffer, GpuRendererError> {
        buffers
            .iter()
            .find(|buffer| buffer.bytes_per_pixel == bytes_per_pixel)
            .ok_or(GpuRendererError::UnsupportedTileTransferFormat { bytes_per_pixel })
    }
}

struct DrawOnStages {
    tools: BTreeSet<DrawOnToolKind>,
    radial_kernel_1d: Option<DrawOnComputeStage>,
    replace_circle_4d: Option<DrawOnComputeStage>,
}

struct DrawOnComputeStage {
    pipeline: wgpu::ComputePipeline,
    bind_group_layout: wgpu::BindGroupLayout,
}

impl DrawOnStages {
    fn disabled() -> Self {
        Self {
            tools: BTreeSet::new(),
            radial_kernel_1d: None,
            replace_circle_4d: None,
        }
    }

    fn new(
        adapter: &wgpu::Adapter,
        device: &wgpu::Device,
        tools: impl IntoIterator<Item = DrawOnToolKind>,
    ) -> Result<Self, GpuRendererError> {
        let tools = tools.into_iter().collect::<BTreeSet<_>>();
        if tools.contains(&DrawOnToolKind::RadialKernel1D) {
            let format_features =
                adapter.get_texture_format_features(wgpu::TextureFormat::R32Float);
            if !device
                .features()
                .contains(wgpu::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES)
                || !format_features
                    .flags
                    .contains(wgpu::TextureFormatFeatureFlags::STORAGE_READ_WRITE)
            {
                return Err(GpuRendererError::MissingDrawOnFeature {
                    tool: DrawOnToolKind::RadialKernel1D,
                    format: wgpu::TextureFormat::R32Float,
                    feature: "STORAGE_READ_WRITE",
                });
            }
        }
        if tools.contains(&DrawOnToolKind::ReplaceCircle4D) {
            let format_features =
                adapter.get_texture_format_features(wgpu::TextureFormat::Rgba32Float);
            if !format_features
                .flags
                .contains(wgpu::TextureFormatFeatureFlags::STORAGE_WRITE_ONLY)
            {
                return Err(GpuRendererError::MissingDrawOnFeature {
                    tool: DrawOnToolKind::ReplaceCircle4D,
                    format: wgpu::TextureFormat::Rgba32Float,
                    feature: "STORAGE_WRITE_ONLY",
                });
            }
        }

        let radial_kernel_1d = if tools.contains(&DrawOnToolKind::RadialKernel1D) {
            Some(DrawOnComputeStage::new(
                device,
                "glaphica-radial-kernel-1d",
                RADIAL_KERNEL_1D_SHADER,
                wgpu::TextureFormat::R32Float,
                wgpu::StorageTextureAccess::ReadWrite,
            ))
        } else {
            None
        };
        let replace_circle_4d = if tools.contains(&DrawOnToolKind::ReplaceCircle4D) {
            Some(DrawOnComputeStage::new(
                device,
                "glaphica-replace-circle-4d",
                REPLACE_CIRCLE_4D_SHADER,
                wgpu::TextureFormat::Rgba32Float,
                wgpu::StorageTextureAccess::WriteOnly,
            ))
        } else {
            None
        };

        Ok(Self {
            tools,
            radial_kernel_1d,
            replace_circle_4d,
        })
    }
}

impl DrawOnComputeStage {
    fn new(
        device: &wgpu::Device,
        label: &'static str,
        shader_source: &'static str,
        format: wgpu::TextureFormat,
        access: wgpu::StorageTextureAccess,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(label),
            source: wgpu::ShaderSource::Wgsl(shader_source.into()),
        });
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("glaphica-draw-on-bind-group-layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::StorageTexture {
                        access,
                        format,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("glaphica-draw-on-pipeline-layout"),
            bind_group_layouts: &[&bind_group_layout],
            immediate_size: 0,
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some(label),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: Some("cs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });
        Self {
            pipeline,
            bind_group_layout,
        }
    }
}

pub struct GpuRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    atlases: AtlasTextureSet,
    tile_buffers: TileTransferBuffers,
    composite: CompositeStages,
    draw_on: DrawOnStages,
    present: TilePresentStage,
}

impl GpuRenderer {
    pub fn new(device: wgpu::Device, queue: wgpu::Queue) -> Result<Self, GpuRendererError> {
        let tile_buffers = TileTransferBuffers::new(&device)?;
        let composite = CompositeStages::new(&device)?;
        let draw_on = DrawOnStages::disabled();
        let present = TilePresentStage::new(&device);
        Ok(Self {
            device,
            queue,
            atlases: AtlasTextureSet::default(),
            tile_buffers,
            composite,
            draw_on,
            present,
        })
    }

    pub fn with_draw_on_tools(
        adapter: &wgpu::Adapter,
        device: wgpu::Device,
        queue: wgpu::Queue,
        draw_on_tools: impl IntoIterator<Item = DrawOnToolKind>,
    ) -> Result<Self, GpuRendererError> {
        let tile_buffers = TileTransferBuffers::new(&device)?;
        let composite = CompositeStages::new(&device)?;
        let draw_on = DrawOnStages::new(adapter, &device, draw_on_tools)?;
        let present = TilePresentStage::new(&device);
        Ok(Self {
            device,
            queue,
            atlases: AtlasTextureSet::default(),
            tile_buffers,
            composite,
            draw_on,
            present,
        })
    }

    pub fn capabilities(&self) -> RendererCapabilities {
        RendererCapabilities {
            draw_radial_kernel_1d: self.draw_on.tools.contains(&DrawOnToolKind::RadialKernel1D),
            replace_circle_4d: self
                .draw_on
                .tools
                .contains(&DrawOnToolKind::ReplaceCircle4D),
        }
    }

    pub fn execute_passes(&mut self, passes: &[Pass]) -> Result<(), GpuRendererError> {
        if passes.is_empty() {
            return Ok(());
        }

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("glaphica-renderer-pass-encoder"),
            });
        {
            let mut ctx = GpuEncodeCtx {
                device: &self.device,
                queue: &self.queue,
                encoder: &mut encoder,
            };
            let mut index = 0;
            while index < passes.len() {
                if draw_on_pass_invocation(passes[index]).is_some() {
                    let start = index;
                    while index < passes.len() && draw_on_pass_invocation(passes[index]).is_some() {
                        index += 1;
                    }
                    encode_draw_on_block(
                        &mut ctx,
                        &self.atlases,
                        &self.draw_on,
                        &passes[start..index],
                    )?;
                    continue;
                }

                match passes[index] {
                    Pass::Clear { dst } => {
                        encode_clear_tile(&mut ctx, &self.atlases, &self.tile_buffers, dst)?
                    }
                    Pass::Copy { src, dst } => {
                        encode_copy_tile(&mut ctx, &self.atlases, &self.tile_buffers, src, dst)?
                    }
                    Pass::RenderTo {
                        src,
                        dst,
                        blend_mode,
                        opacity,
                    } => encode_render_to(
                        &mut ctx,
                        &self.atlases,
                        &mut self.composite,
                        src,
                        dst,
                        blend_mode,
                        opacity,
                    )?,
                    Pass::FixGutter { dst } => encode_fix_gutter(&mut ctx, &self.atlases, dst)?,
                    Pass::DrawOn(_) => {
                        unreachable!("DrawOn passes are handled as contiguous blocks")
                    }
                }
                index += 1;
            }
        }
        self.queue.submit(Some(encoder.finish()));
        Ok(())
    }

    pub fn present_tiles(
        &mut self,
        tiles: &[PresentTile],
        target: PresentTarget<'_>,
    ) -> Result<(), GpuRendererError> {
        self.present.present_tiles(
            &self.device,
            &self.queue,
            &self.atlases,
            tiles,
            target,
            true,
        )
    }

    pub fn present_tiles_incremental(
        &mut self,
        tiles: &[PresentTile],
        target: PresentTarget<'_>,
    ) -> Result<(), GpuRendererError> {
        self.present.present_tiles(
            &self.device,
            &self.queue,
            &self.atlases,
            tiles,
            target,
            false,
        )
    }

    #[doc(hidden)]
    pub fn read_tile_bytes(
        &self,
        position: TilePos,
        bytes_per_pixel: u32,
    ) -> Result<Vec<u8>, GpuRendererError> {
        let resolved = self.atlases.resolve_non_empty(position)?;
        if resolved.runtime.bytes_per_pixel != bytes_per_pixel {
            return Err(GpuRendererError::TileTransferFormatMismatch {
                format: resolved.format,
                bytes_per_pixel,
            });
        }
        let (padded_bytes_per_row, buffer_size) = tile_transfer_layout(bytes_per_pixel)?;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("glaphica-readback-buffer"),
            size: buffer_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("glaphica-readback-encoder"),
            });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &resolved.texture.texture,
                mip_level: 0,
                origin: resolved.origin,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_bytes_per_row),
                    rows_per_image: Some(ATLAS_TILE_SIZE),
                },
            },
            wgpu::Extent3d {
                width: ATLAS_TILE_SIZE,
                height: ATLAS_TILE_SIZE,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit(Some(encoder.finish()));

        let slice = buffer.slice(..);
        let (sender, receiver) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
        self.device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: None,
            })
            .map_err(|_| GpuRendererError::ReadbackPollFailed)?;
        receiver
            .recv()
            .map_err(|_| GpuRendererError::ReadbackChannelClosed)?
            .map_err(|_| GpuRendererError::ReadbackMapFailed)?;
        let mapped = slice.get_mapped_range();
        let bytes = mapped.to_vec();
        drop(mapped);
        buffer.unmap();
        Ok(bytes)
    }

    #[doc(hidden)]
    pub fn write_tile_bytes(
        &self,
        position: TilePos,
        bytes_per_pixel: u32,
        bytes: &[u8],
    ) -> Result<(), GpuRendererError> {
        let resolved = self.atlases.resolve_non_empty(position)?;
        if resolved.runtime.bytes_per_pixel != bytes_per_pixel {
            return Err(GpuRendererError::TileTransferFormatMismatch {
                format: resolved.format,
                bytes_per_pixel,
            });
        }
        let (padded_bytes_per_row, buffer_size) = tile_transfer_layout(bytes_per_pixel)?;
        let expected = buffer_size as usize;
        if bytes.len() != expected {
            return Err(GpuRendererError::InvalidTileTransferLength {
                expected,
                actual: bytes.len(),
            });
        }

        let buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("glaphica-write-tile-buffer"),
                contents: bytes,
                usage: wgpu::BufferUsages::COPY_SRC,
            });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("glaphica-write-tile-encoder"),
            });
        encoder.copy_buffer_to_texture(
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_bytes_per_row),
                    rows_per_image: Some(ATLAS_TILE_SIZE),
                },
            },
            wgpu::TexelCopyTextureInfo {
                texture: &resolved.texture.texture,
                mip_level: 0,
                origin: resolved.origin,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width: ATLAS_TILE_SIZE,
                height: ATLAS_TILE_SIZE,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit(Some(encoder.finish()));
        Ok(())
    }
}

impl RenderBackend for GpuRenderer {
    type Error = GpuRendererError;

    fn submit(&mut self, passes: &[Pass]) -> Result<(), Self::Error> {
        self.execute_passes(passes)
    }
}

struct TilePresentStage {
    bind_group_layout: wgpu::BindGroupLayout,
    pipelines: Vec<(wgpu::TextureFormat, wgpu::RenderPipeline)>,
}

impl TilePresentStage {
    fn new(device: &wgpu::Device) -> Self {
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
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
        Self {
            bind_group_layout,
            pipelines: Vec::new(),
        }
    }

    fn present_tiles(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        atlases: &AtlasTextureSet,
        tiles: &[PresentTile],
        target: PresentTarget<'_>,
        clear_target: bool,
    ) -> Result<(), GpuRendererError> {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("glaphica-tile-present-encoder"),
        });
        if clear_target {
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("glaphica-tile-present-clear-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target.view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(target.clear_color),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
        }

        for tile in tiles.iter().copied() {
            if tile.params.source_width == 0 || tile.params.source_height == 0 {
                continue;
            }
            self.encode_present_tile(device, queue, &mut encoder, atlases, tile, target)?;
        }

        queue.submit(Some(encoder.finish()));
        Ok(())
    }

    fn encode_present_tile(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        atlases: &AtlasTextureSet,
        tile: PresentTile,
        target: PresentTarget<'_>,
    ) -> Result<(), GpuRendererError> {
        let source = atlases.resolve_non_empty(tile.src)?;
        let uniform = PresentUniforms {
            dst_min_ndc: screen_to_ndc(tile.params.target_min_px, target.width, target.height),
            dst_max_ndc: screen_to_ndc(tile.params.target_max_px, target.width, target.height),
            source_origin: [source.origin.x + GUTTER_SIZE, source.origin.y + GUTTER_SIZE],
            source_layer: source.origin.z,
            _pad0: 0,
            source_size: [tile.params.source_width, tile.params.source_height],
            _padding: [0; 2],
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
            layout: &self.bind_group_layout,
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
            .pipelines
            .iter()
            .position(|(candidate, _)| *candidate == format)
        {
            return &self.pipelines[index].1;
        }

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("glaphica-tile-present-shader"),
            source: wgpu::ShaderSource::Wgsl(TILE_PRESENT_SHADER.into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("glaphica-tile-present-pipeline-layout"),
            bind_group_layouts: &[&self.bind_group_layout],
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
        self.pipelines.push((format, pipeline));
        let last = self.pipelines.len() - 1;
        &self.pipelines[last].1
    }
}

struct GpuEncodeCtx<'device, 'encoder> {
    device: &'device wgpu::Device,
    queue: &'device wgpu::Queue,
    encoder: &'encoder mut wgpu::CommandEncoder,
}

fn draw_on_pass_invocation(pass: Pass) -> Option<RendererDrawOnInvocation> {
    match pass {
        Pass::DrawOn(invocation) => Some(invocation),
        Pass::Clear { .. } | Pass::Copy { .. } | Pass::RenderTo { .. } | Pass::FixGutter { .. } => {
            None
        }
    }
}

fn encode_draw_on_block(
    ctx: &mut GpuEncodeCtx<'_, '_>,
    atlases: &AtlasTextureSet,
    draw_on: &DrawOnStages,
    passes: &[Pass],
) -> Result<(), GpuRendererError> {
    let mut layer_streams: BTreeMap<(u8, u32), Vec<RendererDrawOnInvocation>> = BTreeMap::new();
    for pass in passes.iter().copied() {
        let invocation =
            draw_on_pass_invocation(pass).expect("DrawOn block only contains DrawOn passes");
        let dst = invocation.target();
        atlases.resolve_non_empty(dst)?;
        layer_streams
            .entry((dst.atlas_id(), dst.layer))
            .or_default()
            .push(invocation);
    }

    for ((_atlas_id, _layer), stream) in layer_streams {
        let mut index = 0;
        while index < stream.len() {
            let tool = stream[index].tool();
            let start = index;
            while index < stream.len() && stream[index].tool() == tool {
                index += 1;
            }
            match tool {
                DrawOnToolKind::RadialKernel1D => {
                    encode_radial_kernel_1d_run(ctx, atlases, draw_on, &stream[start..index])?
                }
                DrawOnToolKind::ReplaceCircle4D => {
                    encode_replace_circle_4d_run(ctx, atlases, draw_on, &stream[start..index])?
                }
            }
        }
    }

    Ok(())
}

fn encode_radial_kernel_1d_run(
    ctx: &mut GpuEncodeCtx<'_, '_>,
    atlases: &AtlasTextureSet,
    draw_on: &DrawOnStages,
    passes: &[RendererDrawOnInvocation],
) -> Result<(), GpuRendererError> {
    let stage =
        draw_on
            .radial_kernel_1d
            .as_ref()
            .ok_or(GpuRendererError::MissingDrawOnPipeline {
                tool: DrawOnToolKind::RadialKernel1D,
            })?;
    let DrawOnInvocation::RadialKernel1D { dst: first, .. } = passes[0] else {
        unreachable!("radial run starts with a radial pass");
    };
    let atlas = atlases.resolve_non_empty(first)?;
    if !DrawOnToolKind::RadialKernel1D.accepts_target_format(atlas.format) {
        return Err(GpuRendererError::DrawOnFormatMismatch {
            tool: DrawOnToolKind::RadialKernel1D,
            format: atlas.format,
        });
    }

    let mut per_tile: BTreeMap<(u32, u32), Vec<RadialInstance>> = BTreeMap::new();
    for pass in passes.iter().copied() {
        let DrawOnInvocation::RadialKernel1D {
            dst,
            center_in_tile_x,
            center_in_tile_y,
            radius_px,
            amplitude,
        } = pass
        else {
            unreachable!("radial run only contains radial passes");
        };
        if dst.atlas_id() != first.atlas_id() || dst.layer != first.layer {
            return Err(GpuRendererError::InvalidTilePosition(dst));
        }
        per_tile
            .entry((dst.tile_x, dst.tile_y))
            .or_default()
            .push(RadialInstance {
                center_in_tile: [center_in_tile_x, center_in_tile_y],
                radius_px,
                amplitude,
            });
    }

    let mut tile_programs = Vec::with_capacity(per_tile.len());
    let mut instances = Vec::new();
    for ((tile_x, tile_y), tile_instances) in per_tile {
        let instance_start = instances.len() as u32;
        let instance_count = tile_instances.len() as u32;
        instances.extend(tile_instances);
        tile_programs.push(TileProgram {
            tile_x,
            tile_y,
            instance_start,
            instance_count,
        });
    }

    encode_draw_on_compute(
        ctx,
        atlas,
        first.layer,
        stage,
        &tile_programs,
        bytemuck::cast_slice(&instances),
    )
}

fn encode_replace_circle_4d_run(
    ctx: &mut GpuEncodeCtx<'_, '_>,
    atlases: &AtlasTextureSet,
    draw_on: &DrawOnStages,
    passes: &[RendererDrawOnInvocation],
) -> Result<(), GpuRendererError> {
    let stage =
        draw_on
            .replace_circle_4d
            .as_ref()
            .ok_or(GpuRendererError::MissingDrawOnPipeline {
                tool: DrawOnToolKind::ReplaceCircle4D,
            })?;
    let DrawOnInvocation::ReplaceCircle4D { dst: first, .. } = passes[0] else {
        unreachable!("replace run starts with a replace pass");
    };
    let atlas = atlases.resolve_non_empty(first)?;
    if !DrawOnToolKind::ReplaceCircle4D.accepts_target_format(atlas.format) {
        return Err(GpuRendererError::DrawOnFormatMismatch {
            tool: DrawOnToolKind::ReplaceCircle4D,
            format: atlas.format,
        });
    }

    let mut per_tile: BTreeMap<(u32, u32), Vec<ReplaceInstance>> = BTreeMap::new();
    for pass in passes.iter().copied() {
        let DrawOnInvocation::ReplaceCircle4D {
            dst,
            center_in_tile_x,
            center_in_tile_y,
            radius_px,
            color,
        } = pass
        else {
            unreachable!("replace run only contains replace passes");
        };
        if dst.atlas_id() != first.atlas_id() || dst.layer != first.layer {
            return Err(GpuRendererError::InvalidTilePosition(dst));
        }
        per_tile
            .entry((dst.tile_x, dst.tile_y))
            .or_default()
            .push(ReplaceInstance {
                center_in_tile: [center_in_tile_x, center_in_tile_y],
                radius_px,
                _pad0: 0.0,
                color: [color.r, color.g, color.b, color.a],
            });
    }

    let mut tile_programs = Vec::with_capacity(per_tile.len());
    let mut instances = Vec::new();
    for ((tile_x, tile_y), tile_instances) in per_tile {
        let instance_start = instances.len() as u32;
        let instance_count = tile_instances.len() as u32;
        instances.extend(tile_instances);
        tile_programs.push(TileProgram {
            tile_x,
            tile_y,
            instance_start,
            instance_count,
        });
    }

    encode_draw_on_compute(
        ctx,
        atlas,
        first.layer,
        stage,
        &tile_programs,
        bytemuck::cast_slice(&instances),
    )
}

fn encode_draw_on_compute(
    ctx: &mut GpuEncodeCtx<'_, '_>,
    atlas: ResolvedTile<'_>,
    layer: u32,
    stage: &DrawOnComputeStage,
    tile_programs: &[TileProgram],
    instance_bytes: &[u8],
) -> Result<(), GpuRendererError> {
    if tile_programs.is_empty() {
        return Ok(());
    }
    let tile_program_buffer = ctx
        .device
        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("glaphica-draw-on-tile-programs"),
            contents: bytemuck::cast_slice(tile_programs),
            usage: wgpu::BufferUsages::STORAGE,
        });
    let instance_buffer = ctx
        .device
        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("glaphica-draw-on-instances"),
            contents: instance_bytes,
            usage: wgpu::BufferUsages::STORAGE,
        });
    let view =
        atlas
            .layer_views
            .get(layer as usize)
            .ok_or(GpuRendererError::TextureLayerOutOfBounds {
                layer,
                layers: atlas.layer_views.len() as u32,
            })?;
    let bind_group = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("glaphica-draw-on-bind-group"),
        layout: &stage.bind_group_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: tile_program_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: instance_buffer.as_entire_binding(),
            },
        ],
    });
    let mut pass = ctx
        .encoder
        .begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("glaphica-draw-on-compute-pass"),
            timestamp_writes: None,
        });
    pass.set_pipeline(&stage.pipeline);
    pass.set_bind_group(0, &bind_group, &[]);
    pass.dispatch_workgroups(
        IMAGE_TILE_SIZE.div_ceil(8),
        IMAGE_TILE_SIZE.div_ceil(8),
        tile_programs.len() as u32,
    );
    Ok(())
}

fn encode_clear_tile(
    ctx: &mut GpuEncodeCtx<'_, '_>,
    atlases: &AtlasTextureSet,
    tile_buffers: &TileTransferBuffers,
    dst: TilePos,
) -> Result<(), GpuRendererError> {
    let dst = atlases.resolve_non_empty(dst)?;
    let buffer = tile_buffers.zero_for(dst.runtime.bytes_per_pixel)?;
    ctx.encoder.copy_buffer_to_texture(
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer.buffer,
            layout: buffer.layout(),
        },
        wgpu::TexelCopyTextureInfo {
            texture: &dst.texture.texture,
            mip_level: 0,
            origin: dst.origin,
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

fn encode_copy_tile(
    ctx: &mut GpuEncodeCtx<'_, '_>,
    atlases: &AtlasTextureSet,
    tile_buffers: &TileTransferBuffers,
    src: TilePos,
    dst: TilePos,
) -> Result<(), GpuRendererError> {
    let src = atlases.resolve_non_empty(src)?;
    let dst = atlases.resolve_non_empty(dst)?;
    if src.format != dst.format {
        return Err(GpuRendererError::TileFormatMismatch {
            src: src.format,
            dst: dst.format,
        });
    }
    if src.atlas_id == dst.atlas_id {
        return encode_copy_tile_via_buffer(ctx, tile_buffers, src, dst);
    }
    ctx.encoder.copy_texture_to_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &src.texture.texture,
            mip_level: 0,
            origin: src.origin,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyTextureInfo {
            texture: &dst.texture.texture,
            mip_level: 0,
            origin: dst.origin,
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

fn encode_copy_tile_via_buffer(
    ctx: &mut GpuEncodeCtx<'_, '_>,
    tile_buffers: &TileTransferBuffers,
    src: ResolvedTile<'_>,
    dst: ResolvedTile<'_>,
) -> Result<(), GpuRendererError> {
    let buffer = tile_buffers.staging_for(src.runtime.bytes_per_pixel)?;
    let layout = buffer.layout();
    ctx.encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &src.texture.texture,
            mip_level: 0,
            origin: src.origin,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer.buffer,
            layout,
        },
        wgpu::Extent3d {
            width: ATLAS_TILE_SIZE,
            height: ATLAS_TILE_SIZE,
            depth_or_array_layers: 1,
        },
    );
    ctx.encoder.copy_buffer_to_texture(
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer.buffer,
            layout,
        },
        wgpu::TexelCopyTextureInfo {
            texture: &dst.texture.texture,
            mip_level: 0,
            origin: dst.origin,
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

fn encode_render_to(
    ctx: &mut GpuEncodeCtx<'_, '_>,
    atlases: &AtlasTextureSet,
    composite: &mut CompositeStages,
    src: TilePos,
    dst: TilePos,
    blend_mode: BlendMode,
    opacity: f32,
) -> Result<(), GpuRendererError> {
    if opacity <= 0.0 {
        return Ok(());
    }
    let src = atlases.resolve_non_empty(src)?;
    let dst = atlases.resolve_non_empty(dst)?;
    match composite_kind(src.format, dst.format, blend_mode) {
        Some(CompositeKind::Rgba(mode)) => {
            let uniforms = CompositeUniforms {
                source_origin: [src.origin.x, src.origin.y],
                source_layer: src.origin.z,
                blend_mode: encode_rgba_blend_mode(mode),
                opacity,
                _pad0: 0,
            };
            composite.rgba.encode_resolved(ctx, src, dst, uniforms)
        }
        Some(CompositeKind::ValueToRgba(mode)) => {
            let uniforms = CompositeUniforms {
                source_origin: [src.origin.x, src.origin.y],
                source_layer: src.origin.z,
                blend_mode: encode_value_to_rgba_blend_mode(mode),
                opacity,
                _pad0: 0,
            };
            composite
                .value_mask
                .encode_resolved(ctx, src, dst, uniforms)
        }
        None => Err(GpuRendererError::UnsupportedComposite {
            src: src.format,
            dst: dst.format,
            blend_mode,
        }),
    }
}

fn encode_fix_gutter(
    ctx: &mut GpuEncodeCtx<'_, '_>,
    atlases: &AtlasTextureSet,
    dst: TilePos,
) -> Result<(), GpuRendererError> {
    let dst = atlases.resolve_non_empty(dst)?;
    let texture = &dst.texture.texture;
    let runtime = dst.runtime;
    let ox = dst.origin.x;
    let oy = dst.origin.y;
    let z = dst.origin.z;
    let layer = z;

    let g = GUTTER_SIZE;
    let i = IMAGE_TILE_SIZE;

    let tc = |x, y| wgpu::TexelCopyTextureInfo {
        texture,
        mip_level: 0,
        origin: wgpu::Origin3d { x, y, z: layer },
        aspect: wgpu::TextureAspect::All,
    };

    encode_texture_self_copy_via_buffer(
        ctx,
        runtime,
        tc(ox + g, oy + g),
        tc(ox + g, oy),
        wgpu::Extent3d {
            width: i,
            height: g,
            depth_or_array_layers: 1,
        },
    )?;

    encode_texture_self_copy_via_buffer(
        ctx,
        runtime,
        tc(ox + g, oy + ATLAS_TILE_SIZE - g - 1),
        tc(ox + g, oy + ATLAS_TILE_SIZE - g),
        wgpu::Extent3d {
            width: i,
            height: g,
            depth_or_array_layers: 1,
        },
    )?;

    encode_texture_self_copy_via_buffer(
        ctx,
        runtime,
        tc(ox + g, oy + g),
        tc(ox, oy + g),
        wgpu::Extent3d {
            width: g,
            height: i,
            depth_or_array_layers: 1,
        },
    )?;

    encode_texture_self_copy_via_buffer(
        ctx,
        runtime,
        tc(ox + ATLAS_TILE_SIZE - g - 1, oy + g),
        tc(ox + ATLAS_TILE_SIZE - g, oy + g),
        wgpu::Extent3d {
            width: g,
            height: i,
            depth_or_array_layers: 1,
        },
    )?;

    encode_texture_self_copy_via_buffer(
        ctx,
        runtime,
        tc(ox + g, oy + g),
        tc(ox, oy),
        wgpu::Extent3d {
            width: g,
            height: g,
            depth_or_array_layers: 1,
        },
    )?;

    encode_texture_self_copy_via_buffer(
        ctx,
        runtime,
        tc(ox + ATLAS_TILE_SIZE - g - 1, oy + g),
        tc(ox + ATLAS_TILE_SIZE - g, oy),
        wgpu::Extent3d {
            width: g,
            height: g,
            depth_or_array_layers: 1,
        },
    )?;

    encode_texture_self_copy_via_buffer(
        ctx,
        runtime,
        tc(ox + g, oy + ATLAS_TILE_SIZE - g - 1),
        tc(ox, oy + ATLAS_TILE_SIZE - g),
        wgpu::Extent3d {
            width: g,
            height: g,
            depth_or_array_layers: 1,
        },
    )?;

    encode_texture_self_copy_via_buffer(
        ctx,
        runtime,
        tc(ox + ATLAS_TILE_SIZE - g - 1, oy + ATLAS_TILE_SIZE - g - 1),
        tc(ox + ATLAS_TILE_SIZE - g, oy + ATLAS_TILE_SIZE - g),
        wgpu::Extent3d {
            width: g,
            height: g,
            depth_or_array_layers: 1,
        },
    )?;

    Ok(())
}

fn encode_texture_self_copy_via_buffer(
    ctx: &mut GpuEncodeCtx<'_, '_>,
    runtime: TextureFormatRuntime,
    src: wgpu::TexelCopyTextureInfo<'_>,
    dst: wgpu::TexelCopyTextureInfo<'_>,
    extent: wgpu::Extent3d,
) -> Result<(), GpuRendererError> {
    let bytes_per_row = extent.width.checked_mul(runtime.bytes_per_pixel).ok_or(
        GpuRendererError::UnsupportedTileTransferFormat {
            bytes_per_pixel: runtime.bytes_per_pixel,
        },
    )?;
    let padded_bytes_per_row = bytes_per_row.div_ceil(256) * 256;
    let buffer_size = u64::from(padded_bytes_per_row) * u64::from(extent.height);
    let buffer = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("glaphica-self-copy-staging-buffer"),
        size: buffer_size,
        usage: wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let layout = wgpu::TexelCopyBufferLayout {
        offset: 0,
        bytes_per_row: Some(padded_bytes_per_row),
        rows_per_image: Some(extent.height),
    };
    ctx.encoder.copy_texture_to_buffer(
        src,
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout,
        },
        extent,
    );
    ctx.encoder.copy_buffer_to_texture(
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout,
        },
        dst,
        extent,
    );
    Ok(())
}

impl AtlasTextureStore for GpuRenderer {
    type Error = GpuRendererError;

    fn create_atlas_texture(
        &mut self,
        atlas_id: u8,
        layout: AtlasLayout,
        format: GlaFormat,
    ) -> Result<(), Self::Error> {
        self.atlases.create_atlas_texture(
            &self.device,
            &self.draw_on.tools,
            atlas_id,
            layout,
            format,
        )?;
        Ok(())
    }
}

impl AtlasTextureSet {
    fn create_atlas_texture(
        &mut self,
        device: &wgpu::Device,
        draw_on_tools: &BTreeSet<DrawOnToolKind>,
        atlas_id: u8,
        layout: AtlasLayout,
        format: GlaFormat,
    ) -> Result<&AtlasTexture, GpuRendererError> {
        let index = atlas_id as usize;
        if self.atlases.len() <= index {
            self.atlases.resize_with(index + 1, || None);
        }
        if self.atlases[index].is_none() {
            let runtime = runtime_format(format)?;
            let width = layout
                .tiles_per_edge
                .checked_mul(ATLAS_TILE_SIZE as usize)
                .and_then(|value| u32::try_from(value).ok())
                .ok_or(GpuRendererError::InvalidAtlasLayout { layout })?;
            let height = width;
            let layers = u32::try_from(layout.layer_num)
                .map_err(|_| GpuRendererError::InvalidAtlasLayout { layout })?;
            let mut usage = wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::TEXTURE_BINDING;
            if DrawOnToolKind::RadialKernel1D.accepts_target_format(format)
                && draw_on_tools.contains(&DrawOnToolKind::RadialKernel1D)
            {
                usage |= wgpu::TextureUsages::STORAGE_BINDING;
            }
            if DrawOnToolKind::ReplaceCircle4D.accepts_target_format(format)
                && draw_on_tools.contains(&DrawOnToolKind::ReplaceCircle4D)
            {
                usage |= wgpu::TextureUsages::STORAGE_BINDING;
            }
            let texture = RendererTexture::new(
                device,
                &RendererTextureDescriptor {
                    label: Some("glaphica-atlas-texture"),
                    width,
                    height,
                    layers,
                    format: runtime.texture_format,
                    usage,
                },
            )?;
            let mut layer_views = Vec::with_capacity(layers as usize);
            for layer in 0..layers {
                layer_views.push(texture.create_layer_view(layer)?);
            }
            self.atlases[index] = Some(AtlasTexture {
                layout,
                format,
                runtime,
                texture,
                layer_views,
            });
        }

        let texture = self.atlases[index]
            .as_ref()
            .ok_or(GpuRendererError::MissingAtlas { atlas_id })?;
        if texture.format != format || texture.layout != layout {
            return Err(GpuRendererError::AtlasTextureMismatch {
                atlas_id,
                expected_layout: layout,
                actual_layout: texture.layout,
                expected_format: format,
                actual_format: texture.format,
            });
        }
        Ok(texture)
    }

    fn atlas_texture(&self, atlas_id: u8) -> Option<&AtlasTexture> {
        self.atlases.get(atlas_id as usize).and_then(Option::as_ref)
    }

    fn resolve_non_empty(&self, position: TilePos) -> Result<ResolvedTile<'_>, GpuRendererError> {
        let atlas =
            self.atlas_texture(position.atlas_id())
                .ok_or(GpuRendererError::MissingAtlas {
                    atlas_id: position.atlas_id(),
                })?;
        atlas
            .layout
            .address_to_index(position.address())
            .map_err(|_| GpuRendererError::InvalidTilePosition(position))?;
        Ok(ResolvedTile {
            atlas_id: position.atlas_id(),
            format: atlas.format,
            runtime: atlas.runtime,
            texture: &atlas.texture,
            layer_views: &atlas.layer_views,
            origin: wgpu::Origin3d {
                x: position.offset_x(),
                y: position.offset_y(),
                z: position.layer,
            },
        })
    }
}

struct CompositeStages {
    rgba: RgbaCompositeStage,
    value_mask: ValueMaskStage,
}

impl CompositeStages {
    fn new(device: &wgpu::Device) -> Result<Self, GpuRendererError> {
        Ok(Self {
            rgba: RgbaCompositeStage::new(device)?,
            value_mask: ValueMaskStage::new(device)?,
        })
    }
}

struct RgbaCompositeStage {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    scratch_a: RendererTexture,
    scratch_a_view: wgpu::TextureView,
    scratch_b: RendererTexture,
    scratch_b_view: wgpu::TextureView,
}

impl RgbaCompositeStage {
    fn new(device: &wgpu::Device) -> Result<Self, GpuRendererError> {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("glaphica-render-to-shader"),
            source: wgpu::ShaderSource::Wgsl(RGBA_COMPOSITE_SHADER.into()),
        });
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("glaphica-render-to-bind-group-layout"),
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
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("glaphica-render-to-pipeline-layout"),
            bind_group_layouts: &[&bind_group_layout],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("glaphica-render-to-pipeline"),
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
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });

        let scratch_a = create_scratch_texture(device, "glaphica-render-to-scratch-a")?;
        let scratch_a_view = scratch_a.create_layer_view(0)?;
        let scratch_b = create_scratch_texture(device, "glaphica-render-to-scratch-b")?;
        let scratch_b_view = scratch_b.create_layer_view(0)?;

        Ok(Self {
            pipeline,
            bind_group_layout,
            scratch_a,
            scratch_a_view,
            scratch_b,
            scratch_b_view,
        })
    }

    fn encode_resolved(
        &mut self,
        ctx: &mut GpuEncodeCtx<'_, '_>,
        src: ResolvedTile<'_>,
        dst: ResolvedTile<'_>,
        uniforms: CompositeUniforms,
    ) -> Result<(), GpuRendererError> {
        ctx.encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &dst.texture.texture,
                mip_level: 0,
                origin: dst.origin,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture: &self.scratch_a.texture,
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

        let uniform_buffer = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("glaphica-render-to-uniform"),
            size: std::mem::size_of::<CompositeUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        ctx.queue
            .write_buffer(&uniform_buffer, 0, bytemuck::bytes_of(&uniforms));

        let bind_group = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("glaphica-render-to-bind-group"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&self.scratch_a_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&src.texture.view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: uniform_buffer.as_entire_binding(),
                },
            ],
        });
        {
            let mut pass = ctx.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("glaphica-render-to-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.scratch_b_view,
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
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..3, 0..1);
        }

        ctx.encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.scratch_b.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture: &dst.texture.texture,
                mip_level: 0,
                origin: dst.origin,
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

struct ValueMaskStage {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    scratch_a: RendererTexture,
    scratch_a_view: wgpu::TextureView,
    scratch_b: RendererTexture,
    scratch_b_view: wgpu::TextureView,
}

impl ValueMaskStage {
    fn new(device: &wgpu::Device) -> Result<Self, GpuRendererError> {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("glaphica-value-mask-shader"),
            source: wgpu::ShaderSource::Wgsl(VALUE_MASK_SHADER.into()),
        });
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("glaphica-value-mask-bind-group-layout"),
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
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("glaphica-value-mask-pipeline-layout"),
            bind_group_layouts: &[&bind_group_layout],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("glaphica-value-mask-pipeline"),
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
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });

        let scratch_a = create_scratch_texture(device, "glaphica-value-mask-scratch-a")?;
        let scratch_a_view = scratch_a.create_layer_view(0)?;
        let scratch_b = create_scratch_texture(device, "glaphica-value-mask-scratch-b")?;
        let scratch_b_view = scratch_b.create_layer_view(0)?;

        Ok(Self {
            pipeline,
            bind_group_layout,
            scratch_a,
            scratch_a_view,
            scratch_b,
            scratch_b_view,
        })
    }

    fn encode_resolved(
        &mut self,
        ctx: &mut GpuEncodeCtx<'_, '_>,
        value: ResolvedTile<'_>,
        color: ResolvedTile<'_>,
        uniforms: CompositeUniforms,
    ) -> Result<(), GpuRendererError> {
        ctx.encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &color.texture.texture,
                mip_level: 0,
                origin: color.origin,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture: &self.scratch_a.texture,
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

        let uniform_buffer = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("glaphica-value-mask-uniform"),
            size: std::mem::size_of::<CompositeUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        ctx.queue
            .write_buffer(&uniform_buffer, 0, bytemuck::bytes_of(&uniforms));

        let bind_group = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("glaphica-value-mask-bind-group"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&self.scratch_a_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&value.texture.view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: uniform_buffer.as_entire_binding(),
                },
            ],
        });
        {
            let mut pass = ctx.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("glaphica-value-mask-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.scratch_b_view,
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
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..3, 0..1);
        }

        ctx.encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.scratch_b.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture: &color.texture.texture,
                mip_level: 0,
                origin: color.origin,
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

fn create_scratch_texture(
    device: &wgpu::Device,
    label: &'static str,
) -> Result<RendererTexture, GpuRendererError> {
    Ok(RendererTexture::new(
        device,
        &RendererTextureDescriptor {
            label: Some(label),
            width: ATLAS_TILE_SIZE,
            height: ATLAS_TILE_SIZE,
            layers: 1,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::RENDER_ATTACHMENT,
        },
    )?)
}

fn create_zero_tile_buffer(
    device: &wgpu::Device,
    bytes_per_pixel: u32,
) -> Result<TileTransferBuffer, GpuRendererError> {
    let (padded_bytes_per_row, buffer_size) = tile_transfer_layout(bytes_per_pixel)?;
    let zero_tile = vec![0; buffer_size as usize];
    let label = format!("glaphica-clear-tile-zero-buffer-{bytes_per_pixel}bpp");
    let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some(&label),
        contents: &zero_tile,
        usage: wgpu::BufferUsages::COPY_SRC,
    });
    Ok(TileTransferBuffer {
        bytes_per_pixel,
        padded_bytes_per_row,
        buffer,
    })
}

fn create_staging_tile_buffer(
    device: &wgpu::Device,
    bytes_per_pixel: u32,
) -> Result<TileTransferBuffer, GpuRendererError> {
    let (padded_bytes_per_row, buffer_size) = tile_transfer_layout(bytes_per_pixel)?;
    let label = format!("glaphica-copy-tile-staging-buffer-{bytes_per_pixel}bpp");
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(&label),
        size: buffer_size,
        usage: wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    Ok(TileTransferBuffer {
        bytes_per_pixel,
        padded_bytes_per_row,
        buffer,
    })
}

fn tile_transfer_layout(bytes_per_pixel: u32) -> Result<(u32, u64), GpuRendererError> {
    let bytes_per_row = ATLAS_TILE_SIZE
        .checked_mul(bytes_per_pixel)
        .ok_or(GpuRendererError::UnsupportedTileTransferFormat { bytes_per_pixel })?;
    let padded_bytes_per_row = bytes_per_row.div_ceil(256) * 256;
    let buffer_size = u64::from(padded_bytes_per_row) * u64::from(ATLAS_TILE_SIZE);
    Ok((padded_bytes_per_row, buffer_size))
}

fn screen_to_ndc(point: [f32; 2], width: u32, height: u32) -> [f32; 2] {
    [
        point[0] / width.max(1) as f32 * 2.0 - 1.0,
        1.0 - point[1] / height.max(1) as f32 * 2.0,
    ]
}

fn encode_rgba_blend_mode(blend_mode: RgbaBlendMode) -> u32 {
    match blend_mode {
        RgbaBlendMode::Normal => 0,
        RgbaBlendMode::Overlay => 1,
        RgbaBlendMode::Multiply => 2,
    }
}

fn encode_value_to_rgba_blend_mode(blend_mode: ValueToRgbaBlendMode) -> u32 {
    match blend_mode {
        ValueToRgbaBlendMode::MaskAlpha => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CompositeUniforms, GpuRendererError, PresentUniforms, encode_rgba_blend_mode,
        screen_to_ndc, tile_transfer_layout,
    };
    use crate::{GpuRenderer, Pass, PresentTarget, PresentTile, PresentTileParams, RenderBackend};
    use atlas::{AtlasLayout, AtlasTextureStore, TilePos};
    use bytemuck::bytes_of;
    use gla_color::{
        BlendMode, ChannelCount, ChannelType, GlaFormat, PremultipliedRgbaF32, RgbaBlendMode,
    };
    use gla_core::GUTTER_SIZE;
    use gla_draw_on::{DrawOnInvocation, DrawOnToolKind};
    use std::sync::{Mutex, OnceLock};
    use tile_key::Tiles;

    #[test]
    fn composite_uniform_layout_keeps_source_offsets_stable() {
        let uniform = CompositeUniforms {
            source_origin: [11, 13],
            source_layer: 17,
            blend_mode: encode_rgba_blend_mode(RgbaBlendMode::Multiply),
            opacity: 0.5,
            _pad0: 0,
        };
        let bytes = bytes_of(&uniform);
        assert_eq!(bytes.len(), 24);
        let source_x = u32::from_ne_bytes(bytes[0..4].try_into().unwrap());
        let source_layer = u32::from_ne_bytes(bytes[8..12].try_into().unwrap());
        let blend_mode = u32::from_ne_bytes(bytes[12..16].try_into().unwrap());

        assert_eq!(source_x, 11);
        assert_eq!(source_layer, 17);
        assert_eq!(blend_mode, 2);
    }

    #[test]
    fn present_uniform_layout_keeps_source_offsets_stable() {
        assert_eq!(std::mem::size_of::<PresentUniforms>(), 48);
        let params = PresentTileParams {
            target_min_px: [0.0, 0.0],
            target_max_px: [62.0, 62.0],
            source_width: 62,
            source_height: 62,
        };

        assert_eq!(params.source_width, gla_core::IMAGE_TILE_SIZE);
        assert_eq!(params.source_height, gla_core::IMAGE_TILE_SIZE);
    }

    #[test]
    fn screen_to_ndc_maps_surface_pixels_to_clip_space() {
        assert_eq!(screen_to_ndc([0.0, 0.0], 100, 50), [-1.0, 1.0]);
        assert_eq!(screen_to_ndc([100.0, 50.0], 100, 50), [1.0, -1.0]);
    }

    #[test]
    fn gpu_encodes_basic_passes_when_adapter_is_available() {
        let _guard = gpu_test_lock().lock().unwrap();
        let (device, queue) = match pollster::block_on(test_device()) {
            Some(device) => device,
            None => {
                eprintln!("skipping GPU smoke test: no adapter available");
                return;
            }
        };
        let rgba_format = GlaFormat {
            channel_count: ChannelCount::D4,
            channel_type: ChannelType::U8,
        };
        let value_format = GlaFormat {
            channel_count: ChannelCount::D1,
            channel_type: ChannelType::U8,
        };
        let mut gpu = GpuRenderer::new(device, queue).unwrap();
        let mut tiles = Tiles::new();
        let rgba_atlas_id = tiles
            .new_atlas(AtlasLayout::TINY8, rgba_format, &mut gpu)
            .unwrap();
        let value_atlas_id = tiles
            .new_atlas(AtlasLayout::TINY8, value_format, &mut gpu)
            .unwrap();
        let mut rgba_src_tile = tiles.reserve(rgba_atlas_id).unwrap();
        let rgba_src = tiles.write_pos(&mut rgba_src_tile).unwrap();
        let mut rgba_dst_tile = tiles.reserve(rgba_atlas_id).unwrap();
        let rgba_dst = tiles.write_pos(&mut rgba_dst_tile).unwrap();
        let mut value_src_tile = tiles.reserve(value_atlas_id).unwrap();
        let value_src = tiles.write_pos(&mut value_src_tile).unwrap();
        let passes = [
            Pass::Clear { dst: rgba_src },
            Pass::Clear { dst: rgba_dst },
            Pass::Clear { dst: value_src },
            Pass::Copy {
                src: rgba_src,
                dst: rgba_dst,
            },
            Pass::RenderTo {
                src: rgba_src,
                dst: rgba_dst,
                blend_mode: BlendMode::Normal,
                opacity: 1.0,
            },
            Pass::RenderTo {
                src: rgba_src,
                dst: rgba_dst,
                blend_mode: BlendMode::Multiply,
                opacity: 1.0,
            },
            Pass::RenderTo {
                src: value_src,
                dst: rgba_dst,
                blend_mode: BlendMode::MaskAlpha,
                opacity: 1.0,
            },
        ];

        gpu.submit(&passes).unwrap();
    }

    #[test]
    fn gpu_write_tile_bytes_round_trips_when_adapter_is_available() {
        let _guard = gpu_test_lock().lock().unwrap();
        let (device, queue) = match pollster::block_on(test_device()) {
            Some(device) => device,
            None => {
                eprintln!("skipping GPU tile write test: no adapter available");
                return;
            }
        };
        let mut gpu = GpuRenderer::new(device, queue).unwrap();
        let format = GlaFormat {
            channel_count: ChannelCount::D4,
            channel_type: ChannelType::F32,
        };
        gpu.create_atlas_texture(0, AtlasLayout::TINY8, format)
            .unwrap();
        let dst = TilePos::new(0, 0, 0, 0);
        let (padded_bytes_per_row, buffer_size) = tile_transfer_layout(16).unwrap();
        let mut bytes = vec![0_u8; buffer_size as usize];
        let offset = ((2 + GUTTER_SIZE) * padded_bytes_per_row + (3 + GUTTER_SIZE) * 16) as usize;
        for (channel, value) in [0.25_f32, 0.5, 0.75, 1.0].into_iter().enumerate() {
            let start = offset + channel * 4;
            bytes[start..start + 4].copy_from_slice(&value.to_ne_bytes());
        }

        gpu.write_tile_bytes(dst, 16, &bytes).unwrap();

        let readback = gpu.read_tile_bytes(dst, 16).unwrap();
        assert_eq!(read_rgba_pixel(&readback, 16, 3, 2), [0.25, 0.5, 0.75, 1.0]);
    }

    #[test]
    fn gpu_presents_atlas_tile_to_render_target_when_adapter_is_available() {
        let _guard = gpu_test_lock().lock().unwrap();
        let (device, queue) = match pollster::block_on(test_device()) {
            Some(device) => device,
            None => {
                eprintln!("skipping GPU present test: no adapter available");
                return;
            }
        };
        let target_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("glaphica-present-test-target"),
            size: wgpu::Extent3d {
                width: 64,
                height: 64,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let target_view = target_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut gpu = GpuRenderer::new(device, queue).unwrap();
        let format = GlaFormat {
            channel_count: ChannelCount::D4,
            channel_type: ChannelType::F32,
        };
        gpu.create_atlas_texture(0, AtlasLayout::TINY8, format)
            .unwrap();
        let src = TilePos::new(0, 0, 0, 0);

        gpu.submit(&[Pass::Clear { dst: src }]).unwrap();
        gpu.present_tiles(
            &[PresentTile {
                src,
                params: PresentTileParams {
                    target_min_px: [0.0, 0.0],
                    target_max_px: [62.0, 62.0],
                    source_width: 62,
                    source_height: 62,
                },
            }],
            PresentTarget {
                view: &target_view,
                format: wgpu::TextureFormat::Rgba8Unorm,
                width: 64,
                height: 64,
                clear_color: wgpu::Color::BLACK,
            },
        )
        .unwrap();
    }

    #[test]
    fn gpu_draw_radial_kernel_1d_writes_r32float_when_supported() {
        let _guard = gpu_test_lock().lock().unwrap();
        let (adapter, device, queue) = match pollster::block_on(test_device_with_features(
            wgpu::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES,
        )) {
            Some(device) => device,
            None => {
                eprintln!("skipping radial kernel GPU test: required feature unavailable");
                return;
            }
        };
        let mut gpu = match GpuRenderer::with_draw_on_tools(
            &adapter,
            device,
            queue,
            [DrawOnToolKind::RadialKernel1D],
        ) {
            Ok(gpu) => gpu,
            Err(GpuRendererError::MissingDrawOnFeature { .. }) => {
                eprintln!("skipping radial kernel GPU test: storage read_write unavailable");
                return;
            }
            Err(error) => panic!("{error}"),
        };
        let format = GlaFormat {
            channel_count: ChannelCount::D1,
            channel_type: ChannelType::F32,
        };
        gpu.create_atlas_texture(0, AtlasLayout::TINY8, format)
            .unwrap();
        let dst = TilePos::new(0, 0, 0, 0);

        gpu.submit(&[
            Pass::Clear { dst },
            Pass::DrawOn(DrawOnInvocation::RadialKernel1D {
                dst,
                center_in_tile_x: 1.5,
                center_in_tile_y: 1.5,
                radius_px: 2.0,
                amplitude: 3.0,
            }),
        ])
        .unwrap();

        let bytes = gpu.read_tile_bytes(dst, 4).unwrap();
        assert_eq!(read_f32_pixel(&bytes, 4, 1, 1), 3.0);
        assert_eq!(read_f32_pixel(&bytes, 4, 10, 10), 0.0);
    }

    #[test]
    fn gpu_replace_circle_4d_uses_last_matching_instance_when_supported() {
        let _guard = gpu_test_lock().lock().unwrap();
        let (adapter, device, queue) =
            match pollster::block_on(test_device_with_features(wgpu::Features::empty())) {
                Some(device) => device,
                None => {
                    eprintln!("skipping replace circle GPU test: no adapter available");
                    return;
                }
            };
        let mut gpu = match GpuRenderer::with_draw_on_tools(
            &adapter,
            device,
            queue,
            [DrawOnToolKind::ReplaceCircle4D],
        ) {
            Ok(gpu) => gpu,
            Err(GpuRendererError::MissingDrawOnFeature { .. }) => {
                eprintln!("skipping replace circle GPU test: storage write unavailable");
                return;
            }
            Err(error) => panic!("{error}"),
        };
        let format = GlaFormat {
            channel_count: ChannelCount::D4,
            channel_type: ChannelType::F32,
        };
        gpu.create_atlas_texture(0, AtlasLayout::TINY8, format)
            .unwrap();
        let dst = TilePos::new(0, 0, 0, 0);
        let red = PremultipliedRgbaF32::new(1.0, 0.0, 0.0, 1.0);
        let green = PremultipliedRgbaF32::new(0.0, 1.0, 0.0, 1.0);

        gpu.submit(&[
            Pass::Clear { dst },
            Pass::DrawOn(DrawOnInvocation::ReplaceCircle4D {
                dst,
                center_in_tile_x: 5.5,
                center_in_tile_y: 5.5,
                radius_px: 3.0,
                color: red,
            }),
            Pass::DrawOn(DrawOnInvocation::ReplaceCircle4D {
                dst,
                center_in_tile_x: 5.5,
                center_in_tile_y: 5.5,
                radius_px: 1.0,
                color: green,
            }),
        ])
        .unwrap();

        let bytes = gpu.read_tile_bytes(dst, 16).unwrap();
        assert_eq!(read_rgba_pixel(&bytes, 16, 5, 5), [0.0, 1.0, 0.0, 1.0]);
        assert_eq!(read_rgba_pixel(&bytes, 16, 7, 5), [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(read_rgba_pixel(&bytes, 16, 20, 20), [0.0, 0.0, 0.0, 0.0]);
    }

    fn read_f32_pixel(bytes: &[u8], bytes_per_pixel: u32, x: u32, y: u32) -> f32 {
        let (padded_bytes_per_row, _) = tile_transfer_layout(bytes_per_pixel).unwrap();
        let offset = ((y + GUTTER_SIZE) * padded_bytes_per_row
            + (x + GUTTER_SIZE) * bytes_per_pixel) as usize;
        f32::from_ne_bytes(bytes[offset..offset + 4].try_into().unwrap())
    }

    fn read_rgba_pixel(bytes: &[u8], bytes_per_pixel: u32, x: u32, y: u32) -> [f32; 4] {
        let (padded_bytes_per_row, _) = tile_transfer_layout(bytes_per_pixel).unwrap();
        let offset = ((y + GUTTER_SIZE) * padded_bytes_per_row
            + (x + GUTTER_SIZE) * bytes_per_pixel) as usize;
        [
            f32::from_ne_bytes(bytes[offset..offset + 4].try_into().unwrap()),
            f32::from_ne_bytes(bytes[offset + 4..offset + 8].try_into().unwrap()),
            f32::from_ne_bytes(bytes[offset + 8..offset + 12].try_into().unwrap()),
            f32::from_ne_bytes(bytes[offset + 12..offset + 16].try_into().unwrap()),
        ]
    }

    fn gpu_test_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    async fn test_device() -> Option<(wgpu::Device, wgpu::Queue)> {
        let (_adapter, device, queue) = test_device_with_features(wgpu::Features::empty()).await?;
        Some((device, queue))
    }

    async fn test_device_with_features(
        required_features: wgpu::Features,
    ) -> Option<(wgpu::Adapter, wgpu::Device, wgpu::Queue)> {
        let instance = wgpu::Instance::default();
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions::default())
            .await
            .ok()?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("glaphica-renderer-test-device"),
                required_features,
                required_limits: wgpu::Limits::default(),
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
                memory_hints: wgpu::MemoryHints::default(),
                trace: wgpu::Trace::default(),
            })
            .await
            .ok()?;
        Some((adapter, device, queue))
    }
}
