use std::error::Error;
use std::fmt::{Display, Formatter};

use brushes::{BrushDrawInputLayout, BrushGpuPipelineSpec, BrushPipelineError};
use glaphica_core::{ATLAS_TILE_SIZE, BrushId};
use thread_protocol::DrawOp;

use crate::atlas_runtime::{AtlasBackendResource, AtlasStorageRuntime};
use crate::brush_runtime::BrushDrawExecutor;

#[derive(Debug)]
pub enum WgpuBrushExecutorError {
    BrushIdOutOfRange {
        brush_id: BrushId,
    },
    MissingTargetAtlasBackend {
        brush_id: BrushId,
    },
    MissingSourceBackend {
        brush_id: BrushId,
        backend_id: u8,
    },
    CacheBackendNotConfigured {
        brush_id: BrushId,
    },
    MissingCacheBackend {
        brush_id: BrushId,
        backend_id: u8,
    },
    PipelineCreationPanicked {
        brush_id: BrushId,
        label: &'static str,
    },
    UnsupportedAtlasSampleType {
        brush_id: BrushId,
        backend_role: &'static str,
        format: wgpu::TextureFormat,
    },
}

impl Display for WgpuBrushExecutorError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BrushIdOutOfRange { brush_id } => {
                write!(
                    f,
                    "brush id {} cannot be indexed on this platform",
                    brush_id.0
                )
            }
            Self::MissingTargetAtlasBackend { brush_id } => {
                write!(
                    f,
                    "atlas draw target backend is missing for brush {}",
                    brush_id.0
                )
            }
            Self::MissingSourceBackend {
                brush_id,
                backend_id,
            } => write!(
                f,
                "source backend {} is missing for brush {}",
                backend_id, brush_id.0
            ),
            Self::CacheBackendNotConfigured { brush_id } => write!(
                f,
                "brush {} requires cache backend but no cache backend id is configured",
                brush_id.0
            ),
            Self::MissingCacheBackend {
                brush_id,
                backend_id,
            } => write!(
                f,
                "cache backend {} is missing for brush {}",
                backend_id, brush_id.0
            ),
            Self::PipelineCreationPanicked { brush_id, label } => write!(
                f,
                "wgpu pipeline creation panicked for brush {} (label: {label})",
                brush_id.0
            ),
            Self::UnsupportedAtlasSampleType {
                brush_id,
                backend_role,
                format,
            } => write!(
                f,
                "{backend_role} atlas format {format:?} is unsupported for brush {} sampling",
                brush_id.0
            ),
        }
    }
}

impl Error for WgpuBrushExecutorError {}

#[derive(Debug)]
struct CachedPipeline {
    key: PipelineKey,
    pipeline: wgpu::RenderPipeline,
}

#[derive(Debug)]
struct BrushPipelineCache {
    spec: BrushGpuPipelineSpec,
    pipelines: Vec<CachedPipeline>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct AtlasBindGroupLayoutKey {
    source_sample_type: wgpu::TextureSampleType,
    cache_sample_type: wgpu::TextureSampleType,
}

#[derive(Debug)]
struct CachedAtlasBindGroupLayout {
    key: AtlasBindGroupLayoutKey,
    layout: wgpu::BindGroupLayout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PipelineKey {
    target_format: wgpu::TextureFormat,
    atlas_layout_key: AtlasBindGroupLayoutKey,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AtlasBindGroupKey {
    source_backend_id: u8,
    cache_backend_id: Option<u8>,
    layout_key: AtlasBindGroupLayoutKey,
}

#[derive(Debug)]
struct CachedAtlasBindGroup {
    key: AtlasBindGroupKey,
    bind_group: wgpu::BindGroup,
}

#[derive(Debug)]
struct DummyCacheTexture {
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct BrushShaderParams {
    input_len: u32,
    tile_origin_x: u32,
    tile_origin_y: u32,
    tile_layer: u32,
    tile_size_x: u32,
    tile_size_y: u32,
    src_tile_origin_x: u32,
    src_tile_origin_y: u32,
    src_tile_layer: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

pub struct WgpuBrushContext<'a> {
    pub device: &'a wgpu::Device,
    pub queue: &'a wgpu::Queue,
    pub atlas_storage: &'a AtlasStorageRuntime,
    pub brush_cache_backend_id: Option<u8>,
}

#[derive(Default)]
pub struct WgpuBrushExecutor {
    pipelines: Vec<Option<BrushPipelineCache>>,
    draw_bind_group_layout: Option<wgpu::BindGroupLayout>,
    atlas_bind_group_layouts: Vec<CachedAtlasBindGroupLayout>,
    atlas_sampler: Option<wgpu::Sampler>,
    dummy_cache_texture: Option<DummyCacheTexture>,
    atlas_bind_groups: Vec<CachedAtlasBindGroup>,
}

impl WgpuBrushExecutor {
    pub fn new() -> Self {
        Self::default()
    }

    fn brush_index(brush_id: BrushId) -> Result<usize, WgpuBrushExecutorError> {
        usize::try_from(brush_id.0)
            .map_err(|_| WgpuBrushExecutorError::BrushIdOutOfRange { brush_id })
    }

    fn ensure_draw_bind_group_layout(&mut self, device: &wgpu::Device) -> wgpu::BindGroupLayout {
        if let Some(layout) = &self.draw_bind_group_layout {
            return layout.clone();
        }
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("glaphica-brush-draw-bind-group-layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
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
        self.draw_bind_group_layout = Some(layout.clone());
        layout
    }

    fn ensure_atlas_bind_group_layout(
        &mut self,
        device: &wgpu::Device,
        key: AtlasBindGroupLayoutKey,
    ) -> wgpu::BindGroupLayout {
        if let Some(existing) = self
            .atlas_bind_group_layouts
            .iter()
            .find(|entry| entry.key == key)
        {
            return existing.layout.clone();
        }
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("glaphica-brush-atlas-bind-group-layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: key.source_sample_type,
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: key.cache_sample_type,
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering),
                    count: None,
                },
            ],
        });
        self.atlas_bind_group_layouts
            .push(CachedAtlasBindGroupLayout { key, layout });
        match self.atlas_bind_group_layouts.last() {
            Some(entry) => entry.layout.clone(),
            None => unreachable!("atlas bind group layout was just pushed"),
        }
    }

    fn ensure_atlas_sampler(&mut self, device: &wgpu::Device) -> wgpu::Sampler {
        if let Some(sampler) = &self.atlas_sampler {
            return sampler.clone();
        }
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("glaphica-brush-atlas-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        self.atlas_sampler = Some(sampler.clone());
        sampler
    }

    fn ensure_dummy_cache_texture<'a>(
        &'a mut self,
        device: &wgpu::Device,
    ) -> &'a wgpu::TextureView {
        if self.dummy_cache_texture.is_none() {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("glaphica-brush-dummy-cache-texture"),
                size: wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor {
                label: Some("glaphica-brush-dummy-cache-view"),
                format: Some(wgpu::TextureFormat::Rgba8Unorm),
                dimension: Some(wgpu::TextureViewDimension::D2Array),
                usage: Some(wgpu::TextureUsages::TEXTURE_BINDING),
                aspect: wgpu::TextureAspect::All,
                base_mip_level: 0,
                mip_level_count: Some(1),
                base_array_layer: 0,
                array_layer_count: Some(1),
            });
            self.dummy_cache_texture = Some(DummyCacheTexture {
                _texture: texture,
                view,
            });
        }
        match self.dummy_cache_texture.as_ref() {
            Some(cache) => &cache.view,
            None => unreachable!("dummy cache texture is initialized above"),
        }
    }

    fn texture_sample_type_for_atlas(
        brush_id: BrushId,
        backend_role: &'static str,
        format: wgpu::TextureFormat,
        device_features: wgpu::Features,
    ) -> Result<wgpu::TextureSampleType, WgpuBrushExecutorError> {
        let sample_type = format.sample_type(None, Some(device_features)).ok_or(
            WgpuBrushExecutorError::UnsupportedAtlasSampleType {
                brush_id,
                backend_role,
                format,
            },
        )?;
        match sample_type {
            wgpu::TextureSampleType::Float { .. }
            | wgpu::TextureSampleType::Sint
            | wgpu::TextureSampleType::Uint => Ok(sample_type),
            wgpu::TextureSampleType::Depth => {
                Err(WgpuBrushExecutorError::UnsupportedAtlasSampleType {
                    brush_id,
                    backend_role,
                    format,
                })
            }
        }
    }

    fn ensure_render_pipeline(
        &mut self,
        device: &wgpu::Device,
        brush_id: BrushId,
        spec: BrushGpuPipelineSpec,
        pipeline_key: PipelineKey,
        draw_bind_group_layout: &wgpu::BindGroupLayout,
        atlas_bind_group_layout: &wgpu::BindGroupLayout,
    ) -> Result<&wgpu::RenderPipeline, WgpuBrushExecutorError> {
        let brush_index = Self::brush_index(brush_id)?;
        if self.pipelines.len() <= brush_index {
            self.pipelines.resize_with(brush_index + 1, || None);
        }
        let slot = &mut self.pipelines[brush_index];
        match slot {
            Some(cache) if cache.spec == spec => {}
            _ => {
                *slot = Some(BrushPipelineCache {
                    spec,
                    pipelines: Vec::new(),
                });
            }
        }
        let cache = match slot.as_mut() {
            Some(cache) => cache,
            None => unreachable!("pipeline slot is initialized above"),
        };

        if let Some(existing_index) = cache
            .pipelines
            .iter()
            .position(|cached| cached.key == pipeline_key)
        {
            return Ok(&cache.pipelines[existing_index].pipeline);
        }

        let shader_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(spec.label),
            source: wgpu::ShaderSource::Wgsl(spec.wgsl_source.into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some(spec.label),
            bind_group_layouts: &[draw_bind_group_layout, atlas_bind_group_layout],
            immediate_size: 0,
        });
        let create_pipeline = || {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(spec.label),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader_module,
                    entry_point: Some(spec.vertex_entry),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &[],
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &shader_module,
                    entry_point: Some(spec.fragment_entry),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: pipeline_key.target_format,
                        blend: Some(wgpu::BlendState::REPLACE),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let pipeline = std::panic::catch_unwind(std::panic::AssertUnwindSafe(create_pipeline))
            .map_err(|_| WgpuBrushExecutorError::PipelineCreationPanicked {
                brush_id,
                label: spec.label,
            })?;
        cache.pipelines.push(CachedPipeline {
            key: pipeline_key,
            pipeline,
        });
        let pipeline = match cache.pipelines.last() {
            Some(cached) => &cached.pipeline,
            None => unreachable!("pipeline was just pushed"),
        };
        Ok(pipeline)
    }

    fn ensure_atlas_bind_group(
        &mut self,
        device: &wgpu::Device,
        atlas_storage: &AtlasStorageRuntime,
        brush_id: BrushId,
        source_backend_id: u8,
        cache_backend_id: Option<u8>,
        layout_key: AtlasBindGroupLayoutKey,
        atlas_bind_group_layout: &wgpu::BindGroupLayout,
    ) -> Result<&wgpu::BindGroup, WgpuBrushExecutorError> {
        let key = AtlasBindGroupKey {
            source_backend_id,
            cache_backend_id,
            layout_key,
        };
        if let Some(existing_index) = self
            .atlas_bind_groups
            .iter()
            .position(|entry| entry.key == key)
        {
            return Ok(&self.atlas_bind_groups[existing_index].bind_group);
        }

        let source_backend = atlas_storage.backend_resource(source_backend_id).ok_or(
            WgpuBrushExecutorError::MissingSourceBackend {
                brush_id,
                backend_id: source_backend_id,
            },
        )?;
        let source_view =
            create_atlas_sampling_view(source_backend, "glaphica-brush-source-atlas-view");

        let cache_view = match cache_backend_id {
            Some(cache_backend_id) => {
                let cache_backend = atlas_storage.backend_resource(cache_backend_id).ok_or(
                    WgpuBrushExecutorError::MissingCacheBackend {
                        brush_id,
                        backend_id: cache_backend_id,
                    },
                )?;
                create_atlas_sampling_view(cache_backend, "glaphica-brush-cache-atlas-view")
            }
            None => self.ensure_dummy_cache_texture(device).clone(),
        };

        let atlas_sampler = self.ensure_atlas_sampler(device);
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("glaphica-brush-atlas-bind-group"),
            layout: atlas_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&source_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&cache_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&atlas_sampler),
                },
            ],
        });
        self.atlas_bind_groups
            .push(CachedAtlasBindGroup { key, bind_group });
        let bind_group = match self.atlas_bind_groups.last() {
            Some(entry) => &entry.bind_group,
            None => unreachable!("atlas bind group was just pushed"),
        };
        Ok(bind_group)
    }
}

impl BrushDrawExecutor<WgpuBrushContext<'_>> for WgpuBrushExecutor {
    fn execute_draw(
        &mut self,
        context: &mut WgpuBrushContext<'_>,
        draw_op: &DrawOp,
        _layout: BrushDrawInputLayout,
        pipeline_spec: BrushGpuPipelineSpec,
    ) -> Result<(), BrushPipelineError> {
        let resolved = context.atlas_storage.resolve(draw_op.tile_key).ok_or(
            WgpuBrushExecutorError::MissingTargetAtlasBackend {
                brush_id: draw_op.brush_id,
            },
        )?;
        let source_tile_key = draw_op
            .ref_image
            .map(|ref_image| ref_image.tile_key)
            .unwrap_or(draw_op.tile_key);
        let source_resolved = context.atlas_storage.resolve(source_tile_key).ok_or(
            WgpuBrushExecutorError::MissingSourceBackend {
                brush_id: draw_op.brush_id,
                backend_id: source_tile_key.backend_index(),
            },
        )?;
        let source_backend_id = source_tile_key.backend_index();
        let cache_backend_id = if pipeline_spec.uses_brush_cache_backend {
            Some(context.brush_cache_backend_id.ok_or(
                WgpuBrushExecutorError::CacheBackendNotConfigured {
                    brush_id: draw_op.brush_id,
                },
            )?)
        } else {
            None
        };

        let source_backend = context
            .atlas_storage
            .backend_resource(source_backend_id)
            .ok_or(WgpuBrushExecutorError::MissingSourceBackend {
                brush_id: draw_op.brush_id,
                backend_id: source_backend_id,
            })?;
        let cache_backend = match cache_backend_id {
            Some(cache_backend_id) => Some(
                context
                    .atlas_storage
                    .backend_resource(cache_backend_id)
                    .ok_or(WgpuBrushExecutorError::MissingCacheBackend {
                        brush_id: draw_op.brush_id,
                        backend_id: cache_backend_id,
                    })?,
            ),
            None => None,
        };
        let device_features = context.device.features();
        let atlas_layout_key = AtlasBindGroupLayoutKey {
            source_sample_type: Self::texture_sample_type_for_atlas(
                draw_op.brush_id,
                "source",
                source_backend.format,
                device_features,
            )?,
            cache_sample_type: Self::texture_sample_type_for_atlas(
                draw_op.brush_id,
                "cache",
                cache_backend
                    .map(|backend| backend.format)
                    .unwrap_or(wgpu::TextureFormat::Rgba8Unorm),
                device_features,
            )?,
        };
        let draw_bind_group_layout = self.ensure_draw_bind_group_layout(context.device);
        let atlas_bind_group_layout =
            self.ensure_atlas_bind_group_layout(context.device, atlas_layout_key);
        let atlas_bind_group = self
            .ensure_atlas_bind_group(
                context.device,
                context.atlas_storage,
                draw_op.brush_id,
                source_backend_id,
                cache_backend_id,
                atlas_layout_key,
                &atlas_bind_group_layout,
            )?
            .clone();
        let pipeline_key = PipelineKey {
            target_format: resolved.format,
            atlas_layout_key,
        };
        let pipeline = self.ensure_render_pipeline(
            context.device,
            draw_op.brush_id,
            pipeline_spec,
            pipeline_key,
            &draw_bind_group_layout,
            &atlas_bind_group_layout,
        )?;

        let input_bytes = encode_input_bytes(&draw_op.input);
        let input_buffer = context.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("glaphica-brush-input-storage"),
            size: input_bytes.len() as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        context.queue.write_buffer(&input_buffer, 0, &input_bytes);

        let params = BrushShaderParams {
            input_len: draw_op.input.len() as u32,
            tile_origin_x: resolved.address.texel_offset.0,
            tile_origin_y: resolved.address.texel_offset.1,
            tile_layer: resolved.address.layer,
            tile_size_x: ATLAS_TILE_SIZE,
            tile_size_y: ATLAS_TILE_SIZE,
            src_tile_origin_x: source_resolved.address.texel_offset.0,
            src_tile_origin_y: source_resolved.address.texel_offset.1,
            src_tile_layer: source_resolved.address.layer,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };
        let params_bytes = encode_shader_params_bytes(params);
        let params_buffer = context.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("glaphica-brush-params-uniform"),
            size: params_bytes.len() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        context.queue.write_buffer(&params_buffer, 0, &params_bytes);

        let draw_bind_group = context
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("glaphica-brush-draw-bind-group"),
                layout: &draw_bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: input_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: params_buffer.as_entire_binding(),
                    },
                ],
            });

        let attachment_view = resolved
            .texture2d_array
            .create_view(&wgpu::TextureViewDescriptor {
                label: Some("glaphica-brush-atlas-layer-view"),
                format: Some(resolved.format),
                dimension: Some(wgpu::TextureViewDimension::D2),
                usage: Some(wgpu::TextureUsages::RENDER_ATTACHMENT),
                aspect: wgpu::TextureAspect::All,
                base_mip_level: 0,
                mip_level_count: Some(1),
                base_array_layer: resolved.address.layer,
                array_layer_count: Some(1),
            });

        let mut encoder = context
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("glaphica-brush-draw-encoder"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("glaphica-brush-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &attachment_view,
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
            pass.set_bind_group(0, &draw_bind_group, &[]);
            pass.set_bind_group(1, &atlas_bind_group, &[]);
            pass.set_scissor_rect(
                resolved.address.texel_offset.0,
                resolved.address.texel_offset.1,
                ATLAS_TILE_SIZE,
                ATLAS_TILE_SIZE,
            );
            pass.draw(0..3, 0..1);
        }
        context.queue.submit(Some(encoder.finish()));
        Ok(())
    }
}

fn create_atlas_sampling_view(
    backend: AtlasBackendResource<'_>,
    label: &'static str,
) -> wgpu::TextureView {
    backend
        .texture2d_array
        .create_view(&wgpu::TextureViewDescriptor {
            label: Some(label),
            format: Some(backend.format),
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            usage: Some(wgpu::TextureUsages::TEXTURE_BINDING),
            aspect: wgpu::TextureAspect::All,
            base_mip_level: 0,
            mip_level_count: Some(1),
            base_array_layer: 0,
            array_layer_count: Some(backend.layers),
        })
}

fn encode_input_bytes(input: &[f32]) -> Vec<u8> {
    if input.is_empty() {
        return 0.0f32.to_ne_bytes().to_vec();
    }
    let mut bytes = Vec::with_capacity(input.len() * std::mem::size_of::<f32>());
    for value in input {
        bytes.extend_from_slice(&value.to_ne_bytes());
    }
    bytes
}

fn encode_shader_params_bytes(params: BrushShaderParams) -> [u8; 48] {
    let mut bytes = [0u8; 48];
    bytes[0..4].copy_from_slice(&params.input_len.to_ne_bytes());
    bytes[4..8].copy_from_slice(&params.tile_origin_x.to_ne_bytes());
    bytes[8..12].copy_from_slice(&params.tile_origin_y.to_ne_bytes());
    bytes[12..16].copy_from_slice(&params.tile_layer.to_ne_bytes());
    bytes[16..20].copy_from_slice(&params.tile_size_x.to_ne_bytes());
    bytes[20..24].copy_from_slice(&params.tile_size_y.to_ne_bytes());
    bytes[24..28].copy_from_slice(&params.src_tile_origin_x.to_ne_bytes());
    bytes[28..32].copy_from_slice(&params.src_tile_origin_y.to_ne_bytes());
    bytes[32..36].copy_from_slice(&params.src_tile_layer.to_ne_bytes());
    bytes[36..40].copy_from_slice(&params._pad0.to_ne_bytes());
    bytes[40..44].copy_from_slice(&params._pad1.to_ne_bytes());
    bytes[44..48].copy_from_slice(&params._pad2.to_ne_bytes());
    bytes
}

#[cfg(test)]
mod tests {
    use super::{BrushShaderParams, encode_input_bytes, encode_shader_params_bytes};

    #[test]
    fn encode_input_bytes_keeps_empty_input_buffer_non_zero_sized() {
        let encoded = encode_input_bytes(&[]);
        assert_eq!(encoded.len(), 4);
    }

    #[test]
    fn encode_shader_params_bytes_matches_fixed_layout() {
        let params = BrushShaderParams {
            input_len: 3,
            tile_origin_x: 64,
            tile_origin_y: 128,
            tile_layer: 2,
            tile_size_x: 64,
            tile_size_y: 64,
            src_tile_origin_x: 256,
            src_tile_origin_y: 512,
            src_tile_layer: 9,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };
        let encoded = encode_shader_params_bytes(params);
        assert_eq!(encoded.len(), 48);
        assert_eq!(
            u32::from_ne_bytes([encoded[0], encoded[1], encoded[2], encoded[3]]),
            3
        );
        assert_eq!(
            u32::from_ne_bytes([encoded[12], encoded[13], encoded[14], encoded[15]]),
            2
        );
        assert_eq!(
            u32::from_ne_bytes([encoded[32], encoded[33], encoded[34], encoded[35]]),
            9
        );
    }
}
