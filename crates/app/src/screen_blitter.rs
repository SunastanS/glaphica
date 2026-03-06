use document::{FlatRenderTree, View};
use glaphica_core::{GUTTER_SIZE, IMAGE_TILE_SIZE, TileKey};
use gpu_runtime::atlas_runtime::AtlasStorageRuntime;

const BLIT_SHADER: &str = r#"
@group(0) @binding(0) var atlas_texture: texture_2d_array<f32>;
@group(0) @binding(1) var atlas_sampler: sampler;

struct Uniforms {
    layer: u32,
    atlas_x: u32,
    atlas_y: u32,
    gutter_size: u32,
    doc_x: f32,
    doc_y: f32,
    tile_draw_size_x: f32,
    tile_draw_size_y: f32,
    view_offset_x: f32,
    view_offset_y: f32,
    view_scale: f32,
    screen_width: f32,
    screen_height: f32,
    view_rotation_cos: f32,
    view_rotation_sin: f32,
    padding0: f32,
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
    let doc_x = uniforms.doc_x + tile_pos.x * uniforms.tile_draw_size_x;
    let doc_y = uniforms.doc_y + tile_pos.y * uniforms.tile_draw_size_y;
    let rotated_x = uniforms.view_rotation_cos * doc_x - uniforms.view_rotation_sin * doc_y;
    let rotated_y = uniforms.view_rotation_sin * doc_x + uniforms.view_rotation_cos * doc_y;
    let screen_x = uniforms.view_offset_x + rotated_x * uniforms.view_scale;
    let screen_y = uniforms.view_offset_y + rotated_y * uniforms.view_scale;
    
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
        f32(uniforms.atlas_x) + f32(uniforms.gutter_size) + input.uv.x * uniforms.tile_draw_size_x;
    let sample_y =
        f32(uniforms.atlas_y) + f32(uniforms.gutter_size) + input.uv.y * uniforms.tile_draw_size_y;
    let uv_x = sample_x / atlas_dims.x;
    let uv_y = sample_y / atlas_dims.y;
    let color = textureSample(atlas_texture, atlas_sampler, vec2f(uv_x, uv_y), i32(uniforms.layer));
    return color;
}
"#;

const CHECKERBOARD_SHADER: &str = r#"
struct CheckerUniforms {
    doc_width: f32,
    doc_height: f32,
    checker_size_px: f32,
    _padding0: f32,
    view_offset_x: f32,
    view_offset_y: f32,
    view_scale: f32,
    _padding1: f32,
    view_rotation_cos: f32,
    view_rotation_sin: f32,
    _padding2: vec2f,
    light_color: vec4f,
    dark_color: vec4f,
}

@group(0) @binding(0) var<uniform> uniforms: CheckerUniforms;

struct VertexOutput {
    @builtin(position) pos: vec4f,
}

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    var positions = array<vec2f, 6>(
        vec2f(-1.0, -1.0),
        vec2f(1.0, -1.0),
        vec2f(-1.0, 1.0),
        vec2f(-1.0, 1.0),
        vec2f(1.0, -1.0),
        vec2f(1.0, 1.0),
    );

    var output: VertexOutput;
    output.pos = vec4f(positions[vertex_index], 0.0, 1.0);
    return output;
}

@fragment
fn fs_main(@builtin(position) pos: vec4f) -> @location(0) vec4f {
    let screen_x = pos.x - uniforms.view_offset_x;
    let screen_y = pos.y - uniforms.view_offset_y;
    let scaled_x = screen_x / uniforms.view_scale;
    let scaled_y = screen_y / uniforms.view_scale;
    let doc_x = uniforms.view_rotation_cos * scaled_x + uniforms.view_rotation_sin * scaled_y;
    let doc_y = -uniforms.view_rotation_sin * scaled_x + uniforms.view_rotation_cos * scaled_y;

    if doc_x < 0.0 || doc_y < 0.0 || doc_x >= uniforms.doc_width || doc_y >= uniforms.doc_height {
        return vec4f(0.0, 0.0, 0.0, 0.0);
    }

    let tile_x = u32(doc_x / uniforms.checker_size_px);
    let tile_y = u32(doc_y / uniforms.checker_size_px);
    let is_even = ((tile_x + tile_y) & 1u) == 0u;
    return select(uniforms.dark_color, uniforms.light_color, is_even);
}
"#;

#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    layer: u32,
    atlas_x: u32,
    atlas_y: u32,
    gutter_size: u32,
    doc_x: f32,
    doc_y: f32,
    tile_draw_size_x: f32,
    tile_draw_size_y: f32,
    view_offset_x: f32,
    view_offset_y: f32,
    view_scale: f32,
    screen_width: f32,
    screen_height: f32,
    view_rotation_cos: f32,
    view_rotation_sin: f32,
    padding0: f32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CheckerUniforms {
    doc_width: f32,
    doc_height: f32,
    checker_size_px: f32,
    padding0: f32,
    view_offset_x: f32,
    view_offset_y: f32,
    view_scale: f32,
    padding1: f32,
    view_rotation_cos: f32,
    view_rotation_sin: f32,
    padding2: [f32; 2],
    light_color: [f32; 4],
    dark_color: [f32; 4],
}

pub struct ScreenBlitter {
    pipeline: Option<wgpu::RenderPipeline>,
    checker_pipeline: Option<wgpu::RenderPipeline>,
    checker_bind_group_layout: Option<wgpu::BindGroupLayout>,
    bind_group_layout: Option<wgpu::BindGroupLayout>,
    sampler: Option<wgpu::Sampler>,
    pipeline_format: Option<wgpu::TextureFormat>,
    checker_pipeline_format: Option<wgpu::TextureFormat>,
}

impl ScreenBlitter {
    pub fn new() -> Self {
        Self {
            pipeline: None,
            checker_pipeline: None,
            checker_bind_group_layout: None,
            bind_group_layout: None,
            sampler: None,
            pipeline_format: None,
            checker_pipeline_format: None,
        }
    }

    pub fn blit(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        atlas_storage: &AtlasStorageRuntime,
        render_tree: &FlatRenderTree,
        view: &View,
        target_view: &wgpu::TextureView,
        target_format: wgpu::TextureFormat,
        target_width: u32,
        target_height: u32,
    ) {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("screen-blitter-encoder"),
        });
        self.blit_into_encoder(
            device,
            queue,
            atlas_storage,
            render_tree,
            view,
            target_view,
            target_format,
            target_width,
            target_height,
            &mut encoder,
        );
        queue.submit(Some(encoder.finish()));
    }

    pub fn blit_into_encoder(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        atlas_storage: &AtlasStorageRuntime,
        render_tree: &FlatRenderTree,
        view: &View,
        target_view: &wgpu::TextureView,
        target_format: wgpu::TextureFormat,
        target_width: u32,
        target_height: u32,
        encoder: &mut wgpu::CommandEncoder,
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

        let doc_width = image.layout().size_x() as f32;
        let doc_height = image.layout().size_y() as f32;
        let view_scale = view.scale();
        let (view_offset_x, view_offset_y) = view.offset();
        let view_rotation_cos = view.rotation().cos();
        let view_rotation_sin = view.rotation().sin();

        self.ensure_pipeline(device, target_format, target_width, target_height);
        self.ensure_checker_pipeline(device, target_format);

        let Some(pipeline) = &self.pipeline else {
            return;
        };
        let Some(checker_pipeline) = &self.checker_pipeline else {
            return;
        };
        let Some(checker_bind_group_layout) = &self.checker_bind_group_layout else {
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
        let checker_uniforms = CheckerUniforms {
            doc_width,
            doc_height,
            checker_size_px: crate::config::render_background::DOC_CHECKER_SIZE_PX,
            padding0: 0.0,
            view_offset_x,
            view_offset_y,
            view_scale,
            padding1: 0.0,
            view_rotation_cos,
            view_rotation_sin,
            padding2: [0.0, 0.0],
            light_color: crate::config::render_background::DOC_CHECKER_LIGHT,
            dark_color: crate::config::render_background::DOC_CHECKER_DARK,
        };
        let checker_uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("screen-blitter-checkerboard-uniforms"),
            size: std::mem::size_of::<CheckerUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(
            &checker_uniform_buffer,
            0,
            bytemuck::bytes_of(&checker_uniforms),
        );
        let checker_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            layout: checker_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: checker_uniform_buffer.as_entire_binding(),
            }],
            label: Some("screen-blitter-checkerboard-bind-group"),
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
                            r: crate::config::render_background::CANVAS_CLEAR_COLOR[0],
                            g: crate::config::render_background::CANVAS_CLEAR_COLOR[1],
                            b: crate::config::render_background::CANVAS_CLEAR_COLOR[2],
                            a: crate::config::render_background::CANVAS_CLEAR_COLOR[3],
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            pass.set_pipeline(checker_pipeline);
            pass.set_bind_group(0, &checker_bind_group, &[]);
            pass.draw(0..6, 0..1);
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
                let Some((tile_draw_size_x, tile_draw_size_y)) = tile_draw_size(
                    image.layout().size_x(),
                    image.layout().size_y(),
                    canvas_origin,
                ) else {
                    continue;
                };

                // Create per-tile uniform buffer
                let uniforms = Uniforms {
                    layer: atlas_address.address.layer,
                    atlas_x: atlas_address.address.texel_offset.0,
                    atlas_y: atlas_address.address.texel_offset.1,
                    gutter_size: GUTTER_SIZE,
                    doc_x: canvas_origin.x,
                    doc_y: canvas_origin.y,
                    tile_draw_size_x,
                    tile_draw_size_y,
                    view_offset_x,
                    view_offset_y,
                    view_scale,
                    screen_width: target_width as f32,
                    screen_height: target_height as f32,
                    view_rotation_cos,
                    view_rotation_sin,
                    padding0: 0.0,
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
    }

    fn ensure_pipeline(
        &mut self,
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        _target_width: u32,
        _target_height: u32,
    ) {
        if self.pipeline.is_some() && self.pipeline_format == Some(format) {
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

        self.bind_group_layout = Some(bind_group_layout);
        self.pipeline = Some(pipeline);
        self.pipeline_format = Some(format);
    }

    fn ensure_checker_pipeline(&mut self, device: &wgpu::Device, format: wgpu::TextureFormat) {
        if self.checker_pipeline.is_some() && self.checker_pipeline_format == Some(format) {
            return;
        }

        let checker_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("screen-blitter-checkerboard-bind-group-layout"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("screen-blitter-checkerboard-shader"),
            source: wgpu::ShaderSource::Wgsl(CHECKERBOARD_SHADER.into()),
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("screen-blitter-checkerboard-pipeline-layout"),
            bind_group_layouts: &[&checker_bind_group_layout],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("screen-blitter-checkerboard-pipeline"),
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

        self.checker_bind_group_layout = Some(checker_bind_group_layout);
        self.checker_pipeline = Some(pipeline);
        self.checker_pipeline_format = Some(format);
    }
}

pub(crate) fn tile_draw_size(
    doc_width: u32,
    doc_height: u32,
    canvas_origin: glaphica_core::CanvasVec2,
) -> Option<(f32, f32)> {
    let remaining_x = doc_width as f32 - canvas_origin.x;
    let remaining_y = doc_height as f32 - canvas_origin.y;
    if remaining_x <= 0.0 || remaining_y <= 0.0 {
        return None;
    }

    Some((
        remaining_x.min(IMAGE_TILE_SIZE as f32),
        remaining_y.min(IMAGE_TILE_SIZE as f32),
    ))
}

impl Default for ScreenBlitter {
    fn default() -> Self {
        Self::new()
    }
}
