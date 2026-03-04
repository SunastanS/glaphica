use document::FlatRenderTree;
use glaphica_core::{GUTTER_SIZE, IMAGE_TILE_SIZE, TileKey};
use gpu_runtime::atlas_runtime::AtlasStorageRuntime;

const BLIT_SHADER: &str = r#"
@group(0) @binding(0) var atlas_texture: texture_2d_array<f32>;
@group(0) @binding(1) var atlas_sampler: sampler;

struct Uniforms {
    layer: u32,
    atlas_x: u32,
    atlas_y: u32,
    image_tile_size: u32,
    screen_x: f32,
    screen_y: f32,
    scale: f32,
    screen_width: f32,
    screen_height: f32,
    gutter_size: u32,
    padding1: u32,
    padding2: u32,
};

@group(0) @binding(2) var<uniform> uniforms: Uniforms;

struct VertexOutput {
    @builtin(position) pos: vec4f,
    @location(0) uv: vec2f,
}

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    var positions = array<vec2f, 6>(
        vec2f(0.0, 0.0),
        vec2f(1.0, 0.0),
        vec2f(0.0, 1.0),
        vec2f(0.0, 1.0),
        vec2f(1.0, 0.0),
        vec2f(1.0, 1.0),
    );
    
    let tile_pos = positions[vertex_index];
    let screen_x = uniforms.screen_x + tile_pos.x * f32(uniforms.image_tile_size) * uniforms.scale;
    let screen_y = uniforms.screen_y + tile_pos.y * f32(uniforms.image_tile_size) * uniforms.scale;
    
    let ndc_x = (screen_x / uniforms.screen_width) * 2.0 - 1.0;
    let ndc_y = 1.0 - (screen_y / uniforms.screen_height) * 2.0;
    
    var output: VertexOutput;
    output.pos = vec4f(ndc_x, ndc_y, 0.0, 1.0);
    output.uv = tile_pos;
    return output;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4f {
    let atlas_dims = vec2f(textureDimensions(atlas_texture));
    let sample_x =
        f32(uniforms.atlas_x) + f32(uniforms.gutter_size) + input.uv.x * f32(uniforms.image_tile_size);
    let sample_y =
        f32(uniforms.atlas_y) + f32(uniforms.gutter_size) + input.uv.y * f32(uniforms.image_tile_size);
    let uv_x = sample_x / atlas_dims.x;
    let uv_y = sample_y / atlas_dims.y;
    let color = textureSample(atlas_texture, atlas_sampler, vec2f(uv_x, uv_y), i32(uniforms.layer));
    return color;
}
"#;

#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    layer: u32,
    atlas_x: u32,
    atlas_y: u32,
    image_tile_size: u32,
    screen_x: f32,
    screen_y: f32,
    scale: f32,
    screen_width: f32,
    screen_height: f32,
    gutter_size: u32,
    padding1: u32,
    padding2: u32,
}

pub struct ScreenBlitter {
    pipeline: Option<wgpu::RenderPipeline>,
    bind_group_layout: Option<wgpu::BindGroupLayout>,
    sampler: Option<wgpu::Sampler>,
}

impl ScreenBlitter {
    pub fn new() -> Self {
        Self {
            pipeline: None,
            bind_group_layout: None,
            sampler: None,
        }
    }

    pub fn blit(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        atlas_storage: &AtlasStorageRuntime,
        render_tree: &FlatRenderTree,
        target_view: &wgpu::TextureView,
        target_format: wgpu::TextureFormat,
        target_width: u32,
        target_height: u32,
    ) {
        let Some(root_id) = render_tree.root_id else {
            return;
        };

        let Some(root_node) = render_tree.nodes.get(&root_id) else {
            return;
        };

        let image = match &root_node.kind {
            document::FlatNodeKind::Leaf { image } => image,
            document::FlatNodeKind::Branch { cache, .. } => cache,
        };

        let scale = 1.0;

        self.ensure_pipeline(device, target_format, target_width, target_height);

        let Some(pipeline) = &self.pipeline else {
            return;
        };
        let Some(bind_group_layout) = &self.bind_group_layout else {
            return;
        };

        let source_backend = match atlas_storage.backend_resource(0) {
            Some(b) => b,
            None => return,
        };

        let source_view =
            source_backend
                .texture2d_array
                .create_view(&wgpu::TextureViewDescriptor {
                    dimension: Some(wgpu::TextureViewDimension::D2Array),
                    ..Default::default()
                });

        let sampler = self.sampler.get_or_insert_with(|| {
            device.create_sampler(&wgpu::SamplerDescriptor {
                mag_filter: wgpu::FilterMode::Nearest,
                min_filter: wgpu::FilterMode::Nearest,
                ..Default::default()
            })
        });

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("screen-blitter-encoder"),
        });

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("screen-blitter-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.8,
                            g: 0.8,
                            b: 0.8,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            pass.set_pipeline(pipeline);

            for tile_index in 0..image.tile_count() {
                let Some(tile_key) = image.tile_key(tile_index) else {
                    continue;
                };
                if tile_key == TileKey::EMPTY {
                    continue;
                }

                let Some(atlas_address) = atlas_storage.resolve(tile_key) else {
                    continue;
                };

                let Some(canvas_origin) = image.tile_canvas_origin(tile_index) else {
                    continue;
                };

                // Create per-tile uniform buffer
                let uniforms = Uniforms {
                    layer: atlas_address.address.layer,
                    atlas_x: atlas_address.address.texel_offset.0,
                    atlas_y: atlas_address.address.texel_offset.1,
                    image_tile_size: IMAGE_TILE_SIZE,
                    screen_x: canvas_origin.x * scale,
                    screen_y: canvas_origin.y * scale,
                    scale,
                    screen_width: target_width as f32,
                    screen_height: target_height as f32,
                    gutter_size: GUTTER_SIZE,
                    padding1: 0,
                    padding2: 0,
                };

                let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some(&format!("screen-blitter-uniforms-{}", tile_index)),
                    size: std::mem::size_of::<Uniforms>() as u64,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                queue.write_buffer(&uniform_buffer, 0, bytemuck::bytes_of(&uniforms));

                let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    layout: bind_group_layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(&source_view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::Sampler(sampler),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: uniform_buffer.as_entire_binding(),
                        },
                    ],
                    label: Some(&format!("screen-blitter-bind-group-{}", tile_index)),
                });

                pass.set_bind_group(0, &bind_group, &[]);
                pass.draw(0..6, 0..1);
            }
        }

        queue.submit(Some(encoder.finish()));
    }

    fn ensure_pipeline(
        &mut self,
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        _target_width: u32,
        _target_height: u32,
    ) {
        if self.pipeline.is_some() {
            return;
        }

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("screen-blitter-shader"),
            source: wgpu::ShaderSource::Wgsl(BLIT_SHADER.into()),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("screen-blitter-bind-group-layout"),
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
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
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
            label: Some("screen-blitter-pipeline-layout"),
            bind_group_layouts: &[&bind_group_layout],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("screen-blitter-pipeline"),
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

        self.bind_group_layout = Some(bind_group_layout);
        self.pipeline = Some(pipeline);
    }
}

impl Default for ScreenBlitter {
    fn default() -> Self {
        Self::new()
    }
}
