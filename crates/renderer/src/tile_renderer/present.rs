use glaphica_core::GUTTER_SIZE;

use super::atlas_texture_set::AtlasTextureStage;
use super::types::{PresentTileParams, RenderTarget2d, TileRendererError};
use crate::RendererTexture;

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

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct PresentUniforms {
    pub dst_min_ndc: [f32; 2],
    pub dst_max_ndc: [f32; 2],
    pub source_origin: [u32; 2],
    pub source_layer: u32,
    pub _pad0: u32,
    pub source_size: [u32; 2],
    pub padding: [u32; 6],
}

pub struct PresentStage {
    bind_group_layout: wgpu::BindGroupLayout,
    texture_bind_group_layout: wgpu::BindGroupLayout,
    pipelines: Vec<(wgpu::TextureFormat, wgpu::RenderPipeline)>,
    texture_pipelines: Vec<(wgpu::TextureFormat, wgpu::RenderPipeline)>,
}

impl PresentStage {
    pub fn new(device: &wgpu::Device) -> Self {
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
        let texture_bind_group_layout =
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

        Self {
            bind_group_layout,
            texture_bind_group_layout,
            pipelines: Vec::new(),
            texture_pipelines: Vec::new(),
        }
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

    pub fn present_tile(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        atlas_texture_set: &AtlasTextureStage,
        source_tile_key: atlas::TileKey,
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
            atlas_texture_set,
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
            layout: &self.texture_bind_group_layout,
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

    pub fn encode_present_tile(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        atlas_texture_set: &AtlasTextureStage,
        source_tile_key: atlas::TileKey,
        params: PresentTileParams,
        target: RenderTarget2d<'_>,
    ) -> Result<(), TileRendererError> {
        let source = atlas_texture_set.resolve_tile(source_tile_key)?;
        let uniform = PresentUniforms {
            dst_min_ndc: screen_to_ndc(params.target_min_px, target.width, target.height),
            dst_max_ndc: screen_to_ndc(params.target_max_px, target.width, target.height),
            source_origin: [
                source.tile_x * glaphica_core::ATLAS_TILE_SIZE + GUTTER_SIZE,
                source.tile_y * glaphica_core::ATLAS_TILE_SIZE + GUTTER_SIZE,
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
            source: wgpu::ShaderSource::Wgsl(PRESENT_SHADER.into()),
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

    fn present_texture_pipeline(
        &mut self,
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
    ) -> &wgpu::RenderPipeline {
        if let Some(index) = self
            .texture_pipelines
            .iter()
            .position(|(candidate, _)| *candidate == format)
        {
            return &self.texture_pipelines[index].1;
        }

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("glaphica-present-texture-shader"),
            source: wgpu::ShaderSource::Wgsl(PRESENT_TEXTURE_2D_SHADER.into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("glaphica-present-texture-pipeline-layout"),
            bind_group_layouts: &[&self.texture_bind_group_layout],
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
        self.texture_pipelines.push((format, pipeline));
        let last = self.texture_pipelines.len() - 1;
        &self.texture_pipelines[last].1
    }
}

fn screen_to_ndc(point: [f32; 2], width: u32, height: u32) -> [f32; 2] {
    [
        point[0] / width as f32 * 2.0 - 1.0,
        1.0 - point[1] / height as f32 * 2.0,
    ]
}
