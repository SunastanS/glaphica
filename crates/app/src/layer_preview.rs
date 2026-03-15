use std::sync::mpsc;

use glaphica_core::{ATLAS_TILE_SIZE, GUTTER_SIZE, IMAGE_TILE_SIZE, NodeId, TileKey};
use gpu_runtime::atlas_runtime::AtlasStorageRuntime;
use images::{Image, NonEmptyTileBounds};
use wgpu::util::DeviceExt;

const PREVIEW_SIZE: u32 = 128;

const PREVIEW_SHADER: &str = r#"
struct PreviewUniforms {
    dst_min_ndc: vec2f,
    dst_max_ndc: vec2f,
    sample_origin: vec2f,
    sample_size: vec2f,
    atlas_size: vec2f,
    layer: u32,
    _padding: vec3u,
};

@group(0) @binding(0) var<uniform> uniforms: PreviewUniforms;
@group(0) @binding(1) var atlas_texture: texture_2d_array<f32>;
@group(0) @binding(2) var atlas_sampler: sampler;

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
    let sample_xy = uniforms.sample_origin + input.uv * uniforms.sample_size;
    let uv = sample_xy / uniforms.atlas_size;
    return textureSample(atlas_texture, atlas_sampler, uv, i32(uniforms.layer));
}
"#;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayerPreviewBitmap {
    pub node_id: NodeId,
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

#[derive(Clone, Copy)]
pub struct PreviewSource<'a> {
    pub image: &'a Image,
}

#[derive(Debug)]
pub enum LayerPreviewRenderError {
    MissingBackend { tile_key: TileKey },
    MissingTileAddress { tile_key: TileKey },
    BufferMap(wgpu::BufferAsyncError),
    MapChannelRecv(mpsc::RecvError),
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct PreviewUniforms {
    dst_min_ndc: [f32; 2],
    dst_max_ndc: [f32; 2],
    sample_origin: [f32; 2],
    sample_size: [f32; 2],
    atlas_size: [f32; 2],
    layer: u32,
    _padding: [u32; 5],
}

pub struct LayerPreviewRenderer {
    pipeline: Option<wgpu::RenderPipeline>,
    bind_group_layout: Option<wgpu::BindGroupLayout>,
    sampler: Option<wgpu::Sampler>,
    target_texture: Option<wgpu::Texture>,
    readback_buffer: Option<wgpu::Buffer>,
}

impl LayerPreviewRenderer {
    pub fn new() -> Self {
        Self {
            pipeline: None,
            bind_group_layout: None,
            sampler: None,
            target_texture: None,
            readback_buffer: None,
        }
    }

    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        atlas_storage: &AtlasStorageRuntime,
        node_id: NodeId,
        source: PreviewSource<'_>,
    ) -> Result<Option<LayerPreviewBitmap>, LayerPreviewRenderError> {
        let Some(bounds) = source.image.non_empty_tile_bounds() else {
            return Ok(None);
        };

        self.ensure_resources(device);
        let Some(pipeline) = &self.pipeline else {
            return Ok(None);
        };
        let Some(bind_group_layout) = &self.bind_group_layout else {
            return Ok(None);
        };
        let Some(target_texture) = &self.target_texture else {
            return Ok(None);
        };
        let Some(readback_buffer) = &self.readback_buffer else {
            return Ok(None);
        };

        let backend = atlas_storage
            .backend_resource(source.image.backend().raw())
            .ok_or(LayerPreviewRenderError::MissingBackend {
                tile_key: TileKey::from_parts(source.image.backend().raw(), 0, 0),
            })?;
        let atlas_view = backend
            .texture2d_array
            .create_view(&wgpu::TextureViewDescriptor {
                label: Some("glaphica-layer-preview-atlas-view"),
                format: Some(backend.format),
                dimension: Some(wgpu::TextureViewDimension::D2Array),
                usage: Some(wgpu::TextureUsages::TEXTURE_BINDING),
                aspect: wgpu::TextureAspect::All,
                base_mip_level: 0,
                mip_level_count: Some(1),
                base_array_layer: 0,
                array_layer_count: Some(backend.layers),
            });
        let Some(sampler) = &self.sampler else {
            return Ok(None);
        };
        let target_view = target_texture.create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("glaphica-layer-preview-encoder"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("glaphica-layer-preview-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::WHITE),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(pipeline);

            for draw in PreviewTileDraw::collect(source.image, bounds) {
                let tile_key = draw.tile_key;
                let resolved = atlas_storage
                    .resolve(tile_key)
                    .ok_or(LayerPreviewRenderError::MissingTileAddress { tile_key })?;
                let uniforms = PreviewUniforms {
                    dst_min_ndc: draw.dst_min_ndc,
                    dst_max_ndc: draw.dst_max_ndc,
                    sample_origin: [
                        (resolved.address.texel_offset.0 + GUTTER_SIZE) as f32,
                        (resolved.address.texel_offset.1 + GUTTER_SIZE) as f32,
                    ],
                    sample_size: [draw.sample_width as f32, draw.sample_height as f32],
                    atlas_size: [
                        backend.texture2d_array.width() as f32,
                        backend.texture2d_array.height() as f32,
                    ],
                    layer: resolved.address.layer,
                    _padding: [0; 5],
                };
                let uniform_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("glaphica-layer-preview-uniform-buffer"),
                    contents: bytemuck::bytes_of(&uniforms),
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                });
                let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("glaphica-layer-preview-bind-group"),
                    layout: bind_group_layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: uniform_buffer.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::TextureView(&atlas_view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: wgpu::BindingResource::Sampler(sampler),
                        },
                    ],
                });
                pass.set_bind_group(0, &bind_group, &[]);
                pass.draw(0..6, 0..1);
            }
        }

        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: target_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: readback_buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(PREVIEW_SIZE * 4),
                    rows_per_image: Some(PREVIEW_SIZE),
                },
            },
            wgpu::Extent3d {
                width: PREVIEW_SIZE,
                height: PREVIEW_SIZE,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(Some(encoder.finish()));

        let buffer_slice = readback_buffer.slice(..);
        let (sender, receiver) = mpsc::channel();
        buffer_slice.map_async(wgpu::MapMode::Read, move |result| {
            if let Err(send_error) = sender.send(result) {
                eprintln!("layer preview map callback send failed: {send_error}");
            }
        });
        let _ = device.poll(wgpu::PollType::wait_indefinitely());
        let map_result = receiver
            .recv()
            .map_err(LayerPreviewRenderError::MapChannelRecv)?;
        map_result.map_err(LayerPreviewRenderError::BufferMap)?;

        let mapped = buffer_slice.get_mapped_range();
        let pixels = mapped.to_vec();
        drop(mapped);
        readback_buffer.unmap();

        Ok(Some(LayerPreviewBitmap {
            node_id,
            width: PREVIEW_SIZE,
            height: PREVIEW_SIZE,
            pixels,
        }))
    }

    fn ensure_resources(&mut self, device: &wgpu::Device) {
        if self.pipeline.is_none() {
            let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("glaphica-layer-preview-shader"),
                source: wgpu::ShaderSource::Wgsl(PREVIEW_SHADER.into()),
            });
            let bind_group_layout =
                device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("glaphica-layer-preview-bind-group-layout"),
                    entries: &[
                        wgpu::BindGroupLayoutEntry {
                            binding: 0,
                            visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Buffer {
                                ty: wgpu::BufferBindingType::Uniform,
                                has_dynamic_offset: false,
                                min_binding_size: None,
                            },
                            count: None,
                        },
                        wgpu::BindGroupLayoutEntry {
                            binding: 1,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Texture {
                                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                                view_dimension: wgpu::TextureViewDimension::D2Array,
                                multisampled: false,
                            },
                            count: None,
                        },
                        wgpu::BindGroupLayoutEntry {
                            binding: 2,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                            count: None,
                        },
                    ],
                });
            let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("glaphica-layer-preview-pipeline-layout"),
                bind_group_layouts: &[&bind_group_layout],
                immediate_size: 0,
            });
            let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("glaphica-layer-preview-pipeline"),
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
                        blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            });
            self.pipeline = Some(pipeline);
            self.bind_group_layout = Some(bind_group_layout);
        }

        if self.sampler.is_none() {
            self.sampler = Some(device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("glaphica-layer-preview-sampler"),
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                mipmap_filter: wgpu::MipmapFilterMode::Nearest,
                ..Default::default()
            }));
        }

        if self.target_texture.is_none() {
            self.target_texture = Some(device.create_texture(&wgpu::TextureDescriptor {
                label: Some("glaphica-layer-preview-target"),
                size: wgpu::Extent3d {
                    width: PREVIEW_SIZE,
                    height: PREVIEW_SIZE,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            }));
        }

        if self.readback_buffer.is_none() {
            self.readback_buffer = Some(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("glaphica-layer-preview-readback"),
                size: u64::from(PREVIEW_SIZE * PREVIEW_SIZE * 4),
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            }));
        }
    }
}

struct PreviewTileDraw {
    tile_key: TileKey,
    dst_min_ndc: [f32; 2],
    dst_max_ndc: [f32; 2],
    sample_width: u32,
    sample_height: u32,
}

impl PreviewTileDraw {
    fn collect(image: &Image, bounds: NonEmptyTileBounds) -> Vec<Self> {
        let tile_x = image.layout().tile_x();
        let bbox_min_x = bounds.min_tile_x * IMAGE_TILE_SIZE;
        let bbox_min_y = bounds.min_tile_y * IMAGE_TILE_SIZE;
        let bbox_max_x = ((bounds.max_tile_x + 1) * IMAGE_TILE_SIZE).min(image.layout().size_x());
        let bbox_max_y = ((bounds.max_tile_y + 1) * IMAGE_TILE_SIZE).min(image.layout().size_y());
        let bbox_width = bbox_max_x.saturating_sub(bbox_min_x).max(1);
        let bbox_height = bbox_max_y.saturating_sub(bbox_min_y).max(1);
        let scale = PREVIEW_SIZE as f32 / (bbox_width.max(bbox_height) as f32);
        let preview_width = bbox_width as f32 * scale;
        let preview_height = bbox_height as f32 * scale;
        let offset_x = (PREVIEW_SIZE as f32 - preview_width) * 0.5;
        let offset_y = (PREVIEW_SIZE as f32 - preview_height) * 0.5;

        let mut draws = Vec::new();
        for tile_y in bounds.min_tile_y..=bounds.max_tile_y {
            for tile_x_coord in bounds.min_tile_x..=bounds.max_tile_x {
                let tile_index = (tile_y as usize) * (tile_x as usize) + (tile_x_coord as usize);
                let Some(tile_key) = image.tile_key(tile_index) else {
                    continue;
                };
                if tile_key == TileKey::EMPTY {
                    continue;
                }

                let tile_min_x = tile_x_coord * IMAGE_TILE_SIZE;
                let tile_min_y = tile_y * IMAGE_TILE_SIZE;
                let tile_max_x =
                    ((tile_x_coord + 1) * IMAGE_TILE_SIZE).min(image.layout().size_x());
                let tile_max_y = ((tile_y + 1) * IMAGE_TILE_SIZE).min(image.layout().size_y());
                let dst_min_x = tile_min_x.saturating_sub(bbox_min_x);
                let dst_min_y = tile_min_y.saturating_sub(bbox_min_y);
                let dst_max_x = tile_max_x.saturating_sub(bbox_min_x);
                let dst_max_y = tile_max_y.saturating_sub(bbox_min_y);
                let preview_min_x = offset_x + dst_min_x as f32 * scale;
                let preview_min_y = offset_y + dst_min_y as f32 * scale;
                let preview_max_x = offset_x + dst_max_x as f32 * scale;
                let preview_max_y = offset_y + dst_max_y as f32 * scale;

                draws.push(Self {
                    tile_key,
                    dst_min_ndc: [
                        preview_min_x / PREVIEW_SIZE as f32 * 2.0 - 1.0,
                        1.0 - preview_max_y / PREVIEW_SIZE as f32 * 2.0,
                    ],
                    dst_max_ndc: [
                        preview_max_x / PREVIEW_SIZE as f32 * 2.0 - 1.0,
                        1.0 - preview_min_y / PREVIEW_SIZE as f32 * 2.0,
                    ],
                    sample_width: tile_max_x
                        .saturating_sub(tile_min_x)
                        .min(ATLAS_TILE_SIZE - 2 * GUTTER_SIZE),
                    sample_height: tile_max_y
                        .saturating_sub(tile_min_y)
                        .min(ATLAS_TILE_SIZE - 2 * GUTTER_SIZE),
                });
            }
        }
        draws
    }
}
