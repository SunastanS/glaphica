use atlas::TileKey;
use glaphica_core::ATLAS_TILE_SIZE;

use super::atlas_texture_set::AtlasTextureStage;
use super::types::{TileCompositeSource, TileRendererError};

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

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CompositeUniforms {
    source_origin: [u32; 2],
    source_layer: u32,
    blend_mode: u32,
    opacity: f32,
    _pad0: [u32; 3],
    _padding: [u32; 3],
    _pad1: u32,
}

pub struct CompositeStage {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    scratch_a: crate::RendererTexture,
    scratch_b: crate::RendererTexture,
}

impl CompositeStage {
    pub fn new(device: &wgpu::Device) -> Result<Self, TileRendererError> {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("glaphica-tile-composite-shader"),
            source: wgpu::ShaderSource::Wgsl(COMPOSITE_SHADER.into()),
        });
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
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
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("glaphica-tile-composite-pipeline-layout"),
            bind_group_layouts: &[&bind_group_layout],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("glaphica-tile-composite-pipeline"),
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

        Ok(Self {
            pipeline,
            bind_group_layout,
            scratch_a: create_scratch_texture(device, "glaphica-tile-composite-scratch-a")?,
            scratch_b: create_scratch_texture(device, "glaphica-tile-composite-scratch-b")?,
        })
    }

    pub fn composite_tile(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        atlas_texture_set: &AtlasTextureStage,
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
            atlas_texture_set,
            target_tile_key,
            inputs,
        )?;
        queue.submit(Some(encoder.finish()));
        Ok(())
    }

    pub fn encode_composite_tile(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        atlas_texture_set: &AtlasTextureStage,
        target_tile_key: TileKey,
        inputs: &[TileCompositeSource],
    ) -> Result<(), TileRendererError> {
        let target = atlas_texture_set.resolve_tile(target_tile_key)?;
        self.encode_clear_scratch_texture(encoder, true, wgpu::Color::TRANSPARENT)?;
        let mut read_from_a = true;

        for input in inputs {
            if input.tile_key.is_empty() || input.opacity <= 0.0 {
                continue;
            }
            let source = atlas_texture_set.resolve_tile(input.tile_key)?;
            let uniforms = CompositeUniforms {
                source_origin: [
                    source.tile_x * ATLAS_TILE_SIZE,
                    source.tile_y * ATLAS_TILE_SIZE,
                ],
                source_layer: source.layer,
                blend_mode: encode_blend_mode(input.blend_mode),
                opacity: input.opacity,
                _pad0: [0; 3],
                _padding: [0; 3],
                _pad1: 0,
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
                layout: &self.bind_group_layout,
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
                pass.set_pipeline(&self.pipeline);
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
) -> Result<crate::RendererTexture, TileRendererError> {
    let descriptor = crate::RendererTextureDescriptor {
        label: Some(label),
        width: ATLAS_TILE_SIZE,
        height: ATLAS_TILE_SIZE,
        layers: 1,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC
            | wgpu::TextureUsages::COPY_DST
            | wgpu::TextureUsages::RENDER_ATTACHMENT,
    };
    Ok(crate::RendererTexture::new(device, &descriptor)?)
}

fn encode_blend_mode(blend_mode: glaphica_core::BlendMode) -> u32 {
    match blend_mode {
        glaphica_core::BlendMode::Normal => 0,
        glaphica_core::BlendMode::Multiply => 1,
    }
}

#[cfg(test)]
mod tests {
    use bytemuck::bytes_of;

    use super::CompositeUniforms;

    #[test]
    fn composite_uniforms_match_wgsl_uniform_layout() {
        let uniform = CompositeUniforms {
            source_origin: [11, 13],
            source_layer: 17,
            blend_mode: 19,
            opacity: 0.5,
            _pad0: [0; 3],
            _padding: [23, 29, 31],
            _pad1: 0,
        };
        let bytes = bytes_of(&uniform);

        assert_eq!(bytes.len(), 48);

        let opacity = match <[u8; 4]>::try_from(&bytes[16..20]) {
            Ok(bytes) => f32::from_ne_bytes(bytes),
            Err(_) => panic!("composite uniform opacity slice size mismatch"),
        };
        let padding_x = match <[u8; 4]>::try_from(&bytes[32..36]) {
            Ok(bytes) => u32::from_ne_bytes(bytes),
            Err(_) => panic!("composite uniform padding x slice size mismatch"),
        };
        let padding_y = match <[u8; 4]>::try_from(&bytes[36..40]) {
            Ok(bytes) => u32::from_ne_bytes(bytes),
            Err(_) => panic!("composite uniform padding y slice size mismatch"),
        };
        let padding_z = match <[u8; 4]>::try_from(&bytes[40..44]) {
            Ok(bytes) => u32::from_ne_bytes(bytes),
            Err(_) => panic!("composite uniform padding z slice size mismatch"),
        };

        assert_eq!(opacity, 0.5);
        assert_eq!(padding_x, 23);
        assert_eq!(padding_y, 29);
        assert_eq!(padding_z, 31);
    }
}
