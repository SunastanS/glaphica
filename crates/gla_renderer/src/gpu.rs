use std::error::Error;
use std::fmt::{Display, Formatter};

use atlas::{AtlasLayout, AtlasTextureStore, TilePos};
use gla_color::{
    BlendMode, CompositeKind, GlaFormat, RgbaBlendMode, ValueToRgbaBlendMode, composite_kind,
};
use gla_core::{ATLAS_TILE_SIZE, GUTTER_SIZE, IMAGE_TILE_SIZE};
use wgpu::util::DeviceExt;

use crate::Pass;
use crate::texture::{
    RendererTexture, RendererTextureDescriptor, TextureFormatRuntime, TextureResourceError,
    runtime_format,
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
    if blend_mode == 1u {
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

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CompositeUniforms {
    source_origin: [u32; 2],
    source_layer: u32,
    blend_mode: u32,
    opacity: f32,
    _pad0: u32,
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
}

#[derive(Debug, Clone, Copy)]
struct ResolvedTile<'a> {
    atlas_id: u8,
    format: GlaFormat,
    runtime: TextureFormatRuntime,
    texture: &'a RendererTexture,
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

pub struct GpuRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    atlases: AtlasTextureSet,
    tile_buffers: TileTransferBuffers,
    composite: CompositeStages,
}

impl GpuRenderer {
    pub fn new(device: wgpu::Device, queue: wgpu::Queue) -> Result<Self, GpuRendererError> {
        let tile_buffers = TileTransferBuffers::new(&device)?;
        let composite = CompositeStages::new(&device)?;
        Ok(Self {
            device,
            queue,
            atlases: AtlasTextureSet::default(),
            tile_buffers,
            composite,
        })
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
            for pass in passes {
                match *pass {
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
                }
            }
        }
        self.queue.submit(Some(encoder.finish()));
        Ok(())
    }
}

struct GpuEncodeCtx<'device, 'encoder> {
    device: &'device wgpu::Device,
    queue: &'device wgpu::Queue,
    encoder: &'encoder mut wgpu::CommandEncoder,
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
    if src.is_empty() {
        return encode_clear_tile(ctx, atlases, tile_buffers, dst);
    }

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
    if src.is_empty() || opacity <= 0.0 {
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

    ctx.encoder.copy_texture_to_texture(
        tc(ox + g, oy + g),
        tc(ox + g, oy),
        wgpu::Extent3d {
            width: i,
            height: g,
            depth_or_array_layers: 1,
        },
    );

    ctx.encoder.copy_texture_to_texture(
        tc(ox + g, oy + ATLAS_TILE_SIZE - g - 1),
        tc(ox + g, oy + ATLAS_TILE_SIZE - g),
        wgpu::Extent3d {
            width: i,
            height: g,
            depth_or_array_layers: 1,
        },
    );

    ctx.encoder.copy_texture_to_texture(
        tc(ox + g, oy + g),
        tc(ox, oy + g),
        wgpu::Extent3d {
            width: g,
            height: i,
            depth_or_array_layers: 1,
        },
    );

    ctx.encoder.copy_texture_to_texture(
        tc(ox + ATLAS_TILE_SIZE - g - 1, oy + g),
        tc(ox + ATLAS_TILE_SIZE - g, oy + g),
        wgpu::Extent3d {
            width: g,
            height: i,
            depth_or_array_layers: 1,
        },
    );

    ctx.encoder.copy_texture_to_texture(
        tc(ox + g, oy + g),
        tc(ox, oy),
        wgpu::Extent3d {
            width: g,
            height: g,
            depth_or_array_layers: 1,
        },
    );

    ctx.encoder.copy_texture_to_texture(
        tc(ox + ATLAS_TILE_SIZE - g - 1, oy + g),
        tc(ox + ATLAS_TILE_SIZE - g, oy),
        wgpu::Extent3d {
            width: g,
            height: g,
            depth_or_array_layers: 1,
        },
    );

    ctx.encoder.copy_texture_to_texture(
        tc(ox + g, oy + ATLAS_TILE_SIZE - g - 1),
        tc(ox, oy + ATLAS_TILE_SIZE - g),
        wgpu::Extent3d {
            width: g,
            height: g,
            depth_or_array_layers: 1,
        },
    );

    ctx.encoder.copy_texture_to_texture(
        tc(ox + ATLAS_TILE_SIZE - g - 1, oy + ATLAS_TILE_SIZE - g - 1),
        tc(ox + ATLAS_TILE_SIZE - g, oy + ATLAS_TILE_SIZE - g),
        wgpu::Extent3d {
            width: g,
            height: g,
            depth_or_array_layers: 1,
        },
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
        self.atlases
            .create_atlas_texture(&self.device, atlas_id, layout, format)?;
        Ok(())
    }
}

impl AtlasTextureSet {
    fn create_atlas_texture(
        &mut self,
        device: &wgpu::Device,
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
            let texture = RendererTexture::new(
                device,
                &RendererTextureDescriptor {
                    label: Some("glaphica-atlas-texture"),
                    width,
                    height,
                    layers,
                    format: runtime.texture_format,
                    usage: wgpu::TextureUsages::COPY_SRC
                        | wgpu::TextureUsages::COPY_DST
                        | wgpu::TextureUsages::TEXTURE_BINDING,
                },
            )?;
            self.atlases[index] = Some(AtlasTexture {
                layout,
                format,
                runtime,
                texture,
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
        if position.is_empty() {
            return Err(GpuRendererError::InvalidTilePosition(position));
        }
        let atlas =
            self.atlas_texture(position.atlas_id())
                .ok_or(GpuRendererError::MissingAtlas {
                    atlas_id: position.atlas_id(),
                })?;
        let address = atlas
            .layout
            .index_to_address(position.tile_index() as usize)
            .map_err(|_| GpuRendererError::InvalidTilePosition(position))?;
        Ok(ResolvedTile {
            atlas_id: position.atlas_id(),
            format: atlas.format,
            runtime: atlas.runtime,
            texture: &atlas.texture,
            origin: wgpu::Origin3d {
                x: address.offset_x() as u32,
                y: address.offset_y() as u32,
                z: address.layer as u32,
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

fn encode_rgba_blend_mode(blend_mode: RgbaBlendMode) -> u32 {
    match blend_mode {
        RgbaBlendMode::Overlay => 0,
        RgbaBlendMode::Multiply => 1,
    }
}

fn encode_value_to_rgba_blend_mode(blend_mode: ValueToRgbaBlendMode) -> u32 {
    match blend_mode {
        ValueToRgbaBlendMode::MaskAlpha => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::{CompositeUniforms, encode_rgba_blend_mode};
    use crate::{GpuRenderer, Renderer};
    use atlas::{AtlasLayout, TilePos};
    use bytemuck::bytes_of;
    use gla_color::{BlendMode, ChannelCount, ChannelType, GlaFormat, RgbaBlendMode};
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
        assert_eq!(blend_mode, 1);
    }

    #[test]
    fn gpu_encodes_basic_passes_when_adapter_is_available() {
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
        let mut renderer = Renderer::new();

        renderer.clear(rgba_src);
        renderer.clear(rgba_dst);
        renderer.clear(value_src);
        renderer.copy(TilePos::empty(rgba_atlas_id), rgba_dst);
        renderer.render_to(rgba_src, rgba_dst, BlendMode::Multiply, 1.0);
        renderer.render_to(value_src, rgba_dst, BlendMode::MaskAlpha, 1.0);
        renderer.execute(&mut gpu).unwrap();
        assert!(renderer.passes().is_empty());
    }

    async fn test_device() -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance = wgpu::Instance::default();
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions::default())
            .await
            .ok()?;
        adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("glaphica-renderer-test-device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
                memory_hints: wgpu::MemoryHints::default(),
                trace: wgpu::Trace::default(),
            })
            .await
            .ok()
    }
}
