use egui::epaint::{self, Primitive, TextureId};
use wgpu::util::DeviceExt;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct UniformData {
    screen_size_in_points: [f32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuVertex {
    pos: [f32; 2],
    uv: [f32; 2],
    color: [u8; 4],
}

#[derive(Clone, Copy)]
struct DrawCmd {
    clip_rect: epaint::Rect,
    texture_id: TextureId,
    first_index: u32,
    index_count: u32,
}

struct TextureEntry {
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    size: [u32; 2],
}

pub struct EguiRenderer {
    pipeline: wgpu::RenderPipeline,
    uniform_buffer: wgpu::Buffer,
    uniform_bind_group: wgpu::BindGroup,
    texture_bind_group_layout: wgpu::BindGroupLayout,
    vertex_buffer: wgpu::Buffer,
    vertex_capacity: usize,
    index_buffer: wgpu::Buffer,
    index_capacity: usize,
    managed_textures: Vec<Option<TextureEntry>>,
    user_textures: Vec<Option<TextureEntry>>,
    draw_cmds: Vec<DrawCmd>,
}

impl EguiRenderer {
    pub fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("egui-shader"),
            source: wgpu::ShaderSource::Wgsl(EGUI_SHADER.into()),
        });
        let uniform_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("egui-uniform-buffer"),
            contents: bytemuck::bytes_of(&UniformData {
                screen_size_in_points: [1.0, 1.0],
            }),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let uniform_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("egui-uniform-bind-group-layout"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            });
        let uniform_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("egui-uniform-bind-group"),
            layout: &uniform_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buffer.as_entire_binding(),
            }],
        });
        let texture_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("egui-texture-bind-group-layout"),
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
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                ],
            });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("egui-pipeline-layout"),
            bind_group_layouts: &[&uniform_bind_group_layout, &texture_bind_group_layout],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("egui-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<GpuVertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[
                        wgpu::VertexAttribute {
                            shader_location: 0,
                            offset: 0,
                            format: wgpu::VertexFormat::Float32x2,
                        },
                        wgpu::VertexAttribute {
                            shader_location: 1,
                            offset: std::mem::size_of::<[f32; 2]>() as u64,
                            format: wgpu::VertexFormat::Float32x2,
                        },
                        wgpu::VertexAttribute {
                            shader_location: 2,
                            offset: (std::mem::size_of::<[f32; 2]>() * 2) as u64,
                            format: wgpu::VertexFormat::Unorm8x4,
                        },
                    ],
                }],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                unclipped_depth: false,
                polygon_mode: wgpu::PolygonMode::Fill,
                conservative: false,
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: target_format,
                    blend: Some(wgpu::BlendState {
                        color: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                        alpha: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::OneMinusDstAlpha,
                            dst_factor: wgpu::BlendFactor::One,
                            operation: wgpu::BlendOperation::Add,
                        },
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let vertex_capacity = 1024usize;
        let index_capacity = 1024usize * 3;
        let vertex_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("egui-vertex-buffer"),
            size: (vertex_capacity * std::mem::size_of::<GpuVertex>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let index_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("egui-index-buffer"),
            size: (index_capacity * std::mem::size_of::<u32>()) as u64,
            usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            pipeline,
            uniform_buffer,
            uniform_bind_group,
            texture_bind_group_layout,
            vertex_buffer,
            vertex_capacity,
            index_buffer,
            index_capacity,
            managed_textures: Vec::new(),
            user_textures: Vec::new(),
            draw_cmds: Vec::new(),
        }
    }

    pub fn upload_textures(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        textures_delta: &egui::TexturesDelta,
    ) {
        for (texture_id, delta) in &textures_delta.set {
            self.upload_texture_delta(device, queue, *texture_id, delta);
        }
        for texture_id in &textures_delta.free {
            self.free_texture(*texture_id);
        }
    }

    pub fn upload_meshes(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        primitives: &[egui::ClippedPrimitive],
    ) {
        let mut vertex_count = 0usize;
        let mut index_count = 0usize;
        for primitive in primitives {
            if let Primitive::Mesh(mesh) = &primitive.primitive {
                vertex_count += mesh.vertices.len();
                index_count += mesh.indices.len();
            }
        }
        self.ensure_mesh_buffers(device, vertex_count, index_count);

        let mut vertices = Vec::with_capacity(vertex_count);
        let mut indices = Vec::with_capacity(index_count);
        self.draw_cmds.clear();

        for primitive in primitives {
            let Primitive::Mesh(mesh) = &primitive.primitive else {
                continue;
            };

            let first_index = indices.len() as u32;
            let index_count = mesh.indices.len() as u32;
            let vertex_base = vertices.len() as u32;

            for vertex in &mesh.vertices {
                vertices.push(GpuVertex {
                    pos: [vertex.pos.x, vertex.pos.y],
                    uv: [vertex.uv.x, vertex.uv.y],
                    color: vertex.color.to_array(),
                });
            }
            for index in &mesh.indices {
                indices.push(vertex_base + *index);
            }

            self.draw_cmds.push(DrawCmd {
                clip_rect: primitive.clip_rect,
                texture_id: mesh.texture_id,
                first_index,
                index_count,
            });
        }

        if !vertices.is_empty() {
            queue.write_buffer(&self.vertex_buffer, 0, bytemuck::cast_slice(&vertices));
        }
        if !indices.is_empty() {
            queue.write_buffer(&self.index_buffer, 0, bytemuck::cast_slice(&indices));
        }
    }

    pub fn render(
        &self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target_view: &wgpu::TextureView,
        target_size_in_pixels: [u32; 2],
        pixels_per_point: f32,
    ) {
        queue.write_buffer(
            &self.uniform_buffer,
            0,
            bytemuck::bytes_of(&UniformData {
                screen_size_in_points: [
                    target_size_in_pixels[0] as f32 / pixels_per_point,
                    target_size_in_pixels[1] as f32 / pixels_per_point,
                ],
            }),
        );
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("egui-render-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target_view,
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
        pass.set_bind_group(0, &self.uniform_bind_group, &[]);
        pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
        pass.set_index_buffer(self.index_buffer.slice(..), wgpu::IndexFormat::Uint32);

        for cmd in &self.draw_cmds {
            let Some(texture_bind_group) = self.texture_bind_group(cmd.texture_id) else {
                continue;
            };
            let Some(scissor) = compute_scissor_rect(
                cmd.clip_rect,
                pixels_per_point,
                target_size_in_pixels[0],
                target_size_in_pixels[1],
            ) else {
                continue;
            };
            pass.set_scissor_rect(scissor.x, scissor.y, scissor.width, scissor.height);
            pass.set_bind_group(1, texture_bind_group, &[]);
            pass.draw_indexed(
                cmd.first_index..(cmd.first_index + cmd.index_count),
                0,
                0..1,
            );
        }
    }

    fn ensure_mesh_buffers(
        &mut self,
        device: &wgpu::Device,
        vertex_count: usize,
        index_count: usize,
    ) {
        if vertex_count > self.vertex_capacity {
            self.vertex_capacity = self.vertex_capacity.max(1);
            while self.vertex_capacity < vertex_count {
                self.vertex_capacity = self.vertex_capacity.saturating_mul(2);
            }
            self.vertex_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("egui-vertex-buffer"),
                size: (self.vertex_capacity * std::mem::size_of::<GpuVertex>()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        if index_count > self.index_capacity {
            self.index_capacity = self.index_capacity.max(1);
            while self.index_capacity < index_count {
                self.index_capacity = self.index_capacity.saturating_mul(2);
            }
            self.index_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("egui-index-buffer"),
                size: (self.index_capacity * std::mem::size_of::<u32>()) as u64,
                usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
    }

    fn upload_texture_delta(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        texture_id: TextureId,
        delta: &epaint::ImageDelta,
    ) {
        let epaint::ImageData::Color(image) = &delta.image;
        let width = match u32::try_from(image.size[0]) {
            Ok(value) => value,
            Err(_) => return,
        };
        let height = match u32::try_from(image.size[1]) {
            Ok(value) => value,
            Err(_) => return,
        };
        if width == 0 || height == 0 {
            return;
        }

        let (origin_x, origin_y, is_partial_update) = match delta.pos {
            Some(pos) => (pos[0] as u32, pos[1] as u32, true),
            None => (0, 0, false),
        };
        let required_width = match origin_x.checked_add(width) {
            Some(value) => value,
            None => return,
        };
        let required_height = match origin_y.checked_add(height) {
            Some(value) => value,
            None => return,
        };

        match self.texture_size(texture_id) {
            Some([current_width, current_height]) => {
                if !is_partial_update && (current_width != width || current_height != height) {
                    self.create_or_replace_texture(device, texture_id, width, height, delta.options);
                } else if current_width < required_width || current_height < required_height {
                    eprintln!(
                        "skip egui texture delta: update region {}x{} at ({}, {}) exceeds texture {}x{}",
                        width, height, origin_x, origin_y, current_width, current_height
                    );
                    return;
                }
            }
            None => {
                let create_width = if is_partial_update {
                    required_width
                } else {
                    width
                };
                let create_height = if is_partial_update {
                    required_height
                } else {
                    height
                };
                self.create_or_replace_texture(
                    device,
                    texture_id,
                    create_width,
                    create_height,
                    delta.options,
                );
            }
        }

        let Some(texture) = self.texture(texture_id) else {
            return;
        };
        let origin = wgpu::Origin3d {
            x: origin_x,
            y: origin_y,
            z: 0,
        };

        let bytes: Vec<u8> = image.pixels.iter().flat_map(|px| px.to_array()).collect();
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin,
                aspect: wgpu::TextureAspect::All,
            },
            &bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width.saturating_mul(4)),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
    }

    fn create_or_replace_texture(
        &mut self,
        device: &wgpu::Device,
        texture_id: TextureId,
        width: u32,
        height: u32,
        options: epaint::textures::TextureOptions,
    ) {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("egui-texture"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let sampler = create_sampler(device, options);
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("egui-texture-bind-group"),
            layout: &self.texture_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });
        self.set_texture(
            texture_id,
            TextureEntry {
                texture,
                bind_group,
                size: [width, height],
            },
        );
    }

    fn texture(&self, texture_id: TextureId) -> Option<&wgpu::Texture> {
        match texture_id {
            TextureId::Managed(index) => self
                .managed_textures
                .get(index as usize)?
                .as_ref()
                .map(|x| &x.texture),
            TextureId::User(index) => self
                .user_textures
                .get(index as usize)?
                .as_ref()
                .map(|x| &x.texture),
        }
    }

    fn texture_bind_group(&self, texture_id: TextureId) -> Option<&wgpu::BindGroup> {
        match texture_id {
            TextureId::Managed(index) => self
                .managed_textures
                .get(index as usize)?
                .as_ref()
                .map(|x| &x.bind_group),
            TextureId::User(index) => self
                .user_textures
                .get(index as usize)?
                .as_ref()
                .map(|x| &x.bind_group),
        }
    }

    fn texture_size(&self, texture_id: TextureId) -> Option<[u32; 2]> {
        match texture_id {
            TextureId::Managed(index) => self
                .managed_textures
                .get(index as usize)?
                .as_ref()
                .map(|x| x.size),
            TextureId::User(index) => self
                .user_textures
                .get(index as usize)?
                .as_ref()
                .map(|x| x.size),
        }
    }

    fn free_texture(&mut self, texture_id: TextureId) {
        match texture_id {
            TextureId::Managed(index) => {
                if let Some(slot) = self.managed_textures.get_mut(index as usize) {
                    *slot = None;
                }
            }
            TextureId::User(index) => {
                if let Some(slot) = self.user_textures.get_mut(index as usize) {
                    *slot = None;
                }
            }
        }
    }

    fn set_texture(&mut self, texture_id: TextureId, entry: TextureEntry) {
        match texture_id {
            TextureId::Managed(index) => {
                ensure_slot(&mut self.managed_textures, index as usize);
                self.managed_textures[index as usize] = Some(entry);
            }
            TextureId::User(index) => {
                ensure_slot(&mut self.user_textures, index as usize);
                self.user_textures[index as usize] = Some(entry);
            }
        }
    }
}

fn ensure_slot<T>(slots: &mut Vec<Option<T>>, index: usize) {
    if slots.len() <= index {
        slots.resize_with(index + 1, || None);
    }
}

struct ScissorRect {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

fn compute_scissor_rect(
    clip_rect: epaint::Rect,
    pixels_per_point: f32,
    target_width: u32,
    target_height: u32,
) -> Option<ScissorRect> {
    let min_x = (clip_rect.min.x * pixels_per_point).round();
    let min_y = (clip_rect.min.y * pixels_per_point).round();
    let max_x = (clip_rect.max.x * pixels_per_point).round();
    let max_y = (clip_rect.max.y * pixels_per_point).round();
    let min_x = min_x.max(0.0).min(target_width as f32) as u32;
    let min_y = min_y.max(0.0).min(target_height as f32) as u32;
    let max_x = max_x.max(min_x as f32).min(target_width as f32) as u32;
    let max_y = max_y.max(min_y as f32).min(target_height as f32) as u32;
    let width = max_x.saturating_sub(min_x);
    let height = max_y.saturating_sub(min_y);
    if width == 0 || height == 0 {
        return None;
    }
    Some(ScissorRect {
        x: min_x,
        y: min_y,
        width,
        height,
    })
}

fn create_sampler(
    device: &wgpu::Device,
    options: epaint::textures::TextureOptions,
) -> wgpu::Sampler {
    let min_filter = match options.minification {
        epaint::textures::TextureFilter::Nearest => wgpu::FilterMode::Nearest,
        epaint::textures::TextureFilter::Linear => wgpu::FilterMode::Linear,
    };
    let mag_filter = match options.magnification {
        epaint::textures::TextureFilter::Nearest => wgpu::FilterMode::Nearest,
        epaint::textures::TextureFilter::Linear => wgpu::FilterMode::Linear,
    };
    let address_mode = match options.wrap_mode {
        epaint::textures::TextureWrapMode::ClampToEdge => wgpu::AddressMode::ClampToEdge,
        epaint::textures::TextureWrapMode::Repeat => wgpu::AddressMode::Repeat,
        epaint::textures::TextureWrapMode::MirroredRepeat => wgpu::AddressMode::MirrorRepeat,
    };
    device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("egui-sampler"),
        address_mode_u: address_mode,
        address_mode_v: address_mode,
        address_mode_w: address_mode,
        mag_filter,
        min_filter,
        mipmap_filter: wgpu::MipmapFilterMode::Nearest,
        ..Default::default()
    })
}

const EGUI_SHADER: &str = r#"
struct Uniforms {
    screen_size_in_points: vec2f,
};

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(1) @binding(0) var ui_texture: texture_2d<f32>;
@group(1) @binding(1) var ui_sampler: sampler;

struct VertexIn {
    @location(0) pos: vec2f,
    @location(1) uv: vec2f,
    @location(2) color: vec4f,
}

struct VertexOut {
    @builtin(position) pos: vec4f,
    @location(0) uv: vec2f,
    @location(1) color: vec4f,
}

@vertex
fn vs_main(v: VertexIn) -> VertexOut {
    let x = (v.pos.x / uniforms.screen_size_in_points.x) * 2.0 - 1.0;
    let y = 1.0 - (v.pos.y / uniforms.screen_size_in_points.y) * 2.0;
    var out: VertexOut;
    out.pos = vec4f(x, y, 0.0, 1.0);
    out.uv = v.uv;
    out.color = v.color;
    return out;
}

@fragment
fn fs_main(v: VertexOut) -> @location(0) vec4f {
    let tex = textureSample(ui_texture, ui_sampler, v.uv);
    return tex * v.color;
}
"#;
