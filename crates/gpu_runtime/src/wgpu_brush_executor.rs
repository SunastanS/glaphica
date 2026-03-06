use crate::context::GpuContext;
use glaphica_core::StrokeId;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::num::NonZeroU64;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use brushes::{BrushDrawInputLayout, BrushGpuPipelineSpec, BrushPipelineError};
use glaphica_core::{ATLAS_TILE_SIZE, BrushId, TextureFormat};
use thread_protocol::{DrawBlendMode, DrawOp};

use crate::atlas_runtime::{AtlasBackendResource, AtlasStorageRuntime};
use crate::brush_runtime::BrushDrawExecutor;

#[derive(Debug)]
pub enum WgpuBrushExecutorError {
    BrushIdOutOfRange {
        brush_id: BrushId,
    },
    BrushNotConfigured {
        brush_id: BrushId,
    },
    MissingTargetAtlasBackend {
        brush_id: BrushId,
    },
    MissingSourceBackend {
        brush_id: BrushId,
        backend_id: u8,
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
    InconsistentSourceFormat {
        brush_id: BrushId,
        expected: wgpu::TextureFormat,
        actual: wgpu::TextureFormat,
    },
    DynamicOffsetOutOfRange {
        brush_id: BrushId,
        offset: u64,
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
            Self::BrushNotConfigured { brush_id } => {
                write!(f, "brush {} has not been configured", brush_id.0)
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
            Self::InconsistentSourceFormat {
                brush_id,
                expected,
                actual,
            } => write!(
                f,
                "source format mismatch for brush {}: expected {:?}, got {:?}",
                brush_id.0, expected, actual
            ),
            Self::DynamicOffsetOutOfRange { brush_id, offset } => write!(
                f,
                "dynamic offset {} exceeds u32 range for brush {}",
                offset, brush_id.0
            ),
        }
    }
}

impl Error for WgpuBrushExecutorError {}

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
struct StrokeAtlasBindGroupKey {
    source_backend_id: u8,
    cache_backend_id: Option<u8>,
    has_ref_image: bool,
    has_cache_tile: bool,
    layout_key: AtlasBindGroupLayoutKey,
}

#[derive(Debug)]
struct CachedStrokeAtlasBindGroup {
    key: StrokeAtlasBindGroupKey,
    bind_group: wgpu::BindGroup,
}

#[derive(Debug)]
struct DummyCacheTexture {
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
}

#[derive(Debug)]
struct TransientDrawResources {
    _atlas_bind_group: wgpu::BindGroup,
    _attachment_view: wgpu::TextureView,
}

#[derive(Debug)]
struct BrushDrawRing {
    input_buffer: wgpu::Buffer,
    params_buffer: wgpu::Buffer,
    draw_bind_group: wgpu::BindGroup,
    input_binding_size: u64,
    input_stride: u64,
    params_stride: u64,
    input_cursor: u64,
    params_cursor: u64,
    last_stroke_id: Option<StrokeId>,
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
    cache_tile_origin_x: u32,
    cache_tile_origin_y: u32,
    cache_tile_layer: u32,
    has_cache_tile: u32,
    _pad1: u32,
    _pad2: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DrawPassKey {
    backend_id: u8,
    layer: u32,
    format: wgpu::TextureFormat,
}

#[derive(Debug)]
struct PreparedDrawCall {
    pass_key: DrawPassKey,
    scissor_x: u32,
    scissor_y: u32,
    pipeline: wgpu::RenderPipeline,
    draw_bind_group: wgpu::BindGroup,
    atlas_bind_group: wgpu::BindGroup,
    input_dynamic_offset: u32,
    params_dynamic_offset: u32,
}

pub struct WgpuBrushContext<'a> {
    pub gpu_context: &'a GpuContext,
    pub atlas_storage: &'a AtlasStorageRuntime,
}

struct BrushContext {
    spec: BrushGpuPipelineSpec,
    cache_backend_id: Option<u8>,
    alpha_pipeline: Option<wgpu::RenderPipeline>,
    replace_pipeline: Option<wgpu::RenderPipeline>,
    draw_ring: Option<BrushDrawRing>,
    stroke_cached_bind_groups: Vec<CachedStrokeAtlasBindGroup>,
    cached_stroke_id: Option<StrokeId>,
}

#[derive(Default)]
pub struct WgpuBrushExecutor {
    brushes: Vec<Option<BrushContext>>,
    draw_bind_group_layout: Option<wgpu::BindGroupLayout>,
    atlas_bind_group_layouts: Vec<CachedAtlasBindGroupLayout>,
    atlas_sampler: Option<wgpu::Sampler>,
    dummy_cache_texture: Option<DummyCacheTexture>,
    transient_draw_resources: Vec<TransientDrawResources>,
}

#[derive(Debug, Clone, Copy)]
struct GpuDrawExecTraceConfig {
    enabled: bool,
    max_events: u64,
}

fn gpu_draw_exec_trace_config() -> GpuDrawExecTraceConfig {
    static CONFIG: OnceLock<GpuDrawExecTraceConfig> = OnceLock::new();
    *CONFIG.get_or_init(|| {
        let enabled = std::env::var("GLAPHICA_DEBUG_GPU_EXEC_TRACE")
            .ok()
            .is_some_and(|value| value != "0");
        let max_events = std::env::var("GLAPHICA_DEBUG_GPU_EXEC_TRACE_MAX")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(400);
        GpuDrawExecTraceConfig {
            enabled,
            max_events,
        }
    })
}

fn should_trace_gpu_draw_exec_event() -> bool {
    static TRACE_SEQ: AtomicU64 = AtomicU64::new(0);
    let config = gpu_draw_exec_trace_config();
    if !config.enabled {
        return false;
    }
    let seq = TRACE_SEQ.fetch_add(1, Ordering::Relaxed) + 1;
    seq <= config.max_events
}

const BRUSH_SHADER_PARAMS_SIZE: u64 = 60;
const BRUSH_RING_INITIAL_SLOTS: u64 = 128;

fn align_up_u64(value: u64, alignment: u64) -> u64 {
    if alignment <= 1 {
        return value;
    }
    value.div_ceil(alignment).saturating_mul(alignment)
}

impl WgpuBrushExecutor {
    pub fn new() -> Self {
        Self::default()
    }

    fn brush_index(brush_id: BrushId) -> Result<usize, WgpuBrushExecutorError> {
        usize::try_from(brush_id.0)
            .map_err(|_| WgpuBrushExecutorError::BrushIdOutOfRange { brush_id })
    }

    fn create_brush_draw_ring(
        device: &wgpu::Device,
        draw_bind_group_layout: &wgpu::BindGroupLayout,
        input_binding_size: u64,
        input_stride: u64,
        params_stride: u64,
        slots: u64,
    ) -> BrushDrawRing {
        let input_capacity = input_stride.saturating_mul(slots);
        let params_capacity = params_stride.saturating_mul(slots);

        let input_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("glaphica-brush-input-ring-storage"),
            size: input_capacity.max(input_stride),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let params_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("glaphica-brush-params-ring-uniform"),
            size: params_capacity.max(params_stride),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let draw_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("glaphica-brush-draw-ring-bind-group"),
            layout: draw_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &input_buffer,
                        offset: 0,
                        size: NonZeroU64::new(input_binding_size),
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &params_buffer,
                        offset: 0,
                        size: NonZeroU64::new(BRUSH_SHADER_PARAMS_SIZE),
                    }),
                },
            ],
        });

        BrushDrawRing {
            input_buffer,
            params_buffer,
            draw_bind_group,
            input_binding_size,
            input_stride,
            params_stride,
            input_cursor: 0,
            params_cursor: 0,
            last_stroke_id: None,
        }
    }

    fn get_or_create_stroke_atlas_bind_group(
        &mut self,
        brush_id: BrushId,
        brush_index: usize,
        key: StrokeAtlasBindGroupKey,
        device: &wgpu::Device,
        atlas_storage: &AtlasStorageRuntime,
        layout: &wgpu::BindGroupLayout,
    ) -> Result<wgpu::BindGroup, WgpuBrushExecutorError> {
        {
            let brush_context = self
                .brushes
                .get(brush_index)
                .and_then(|entry| entry.as_ref())
                .ok_or(WgpuBrushExecutorError::BrushNotConfigured { brush_id })?;
            if let Some(existing) = brush_context
                .stroke_cached_bind_groups
                .iter()
                .find(|cached| cached.key == key)
            {
                return Ok(existing.bind_group.clone());
            }
        }

        let bind_group = self.create_atlas_bind_group(
            device,
            atlas_storage,
            key.source_backend_id,
            key.cache_backend_id,
            layout,
            brush_id,
            key.has_ref_image,
            key.has_cache_tile,
        )?;
        let brush_context = self
            .brushes
            .get_mut(brush_index)
            .and_then(|entry| entry.as_mut())
            .ok_or(WgpuBrushExecutorError::BrushNotConfigured { brush_id })?;
        brush_context
            .stroke_cached_bind_groups
            .push(CachedStrokeAtlasBindGroup { key, bind_group });
        match brush_context.stroke_cached_bind_groups.last() {
            Some(cached) => Ok(cached.bind_group.clone()),
            None => unreachable!("stroke atlas bind group was just pushed"),
        }
    }

    pub fn configure_brush(
        &mut self,
        brush_id: BrushId,
        spec: BrushGpuPipelineSpec,
        cache_backend_id: Option<u8>,
    ) -> Result<(), WgpuBrushExecutorError> {
        let brush_index = Self::brush_index(brush_id)?;
        if self.brushes.len() <= brush_index {
            self.brushes.resize_with(brush_index + 1, || None);
        }
        self.brushes[brush_index] = Some(BrushContext {
            spec,
            cache_backend_id,
            alpha_pipeline: None,
            replace_pipeline: None,
            draw_ring: None,
            stroke_cached_bind_groups: Vec::new(),
            cached_stroke_id: None,
        });
        Ok(())
    }

    pub fn clear_transient_draw_resources(&mut self) {
        self.transient_draw_resources.clear();
        for brush in &mut self.brushes {
            if let Some(brush) = brush.as_mut() {
                if let Some(ring) = brush.draw_ring.as_mut() {
                    ring.input_cursor = 0;
                    ring.params_cursor = 0;
                    ring.last_stroke_id = None;
                }
            }
        }
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
                        has_dynamic_offset: true,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: true,
                        min_binding_size: NonZeroU64::new(BRUSH_SHADER_PARAMS_SIZE),
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

    fn create_render_pipeline(
        device: &wgpu::Device,
        spec: &BrushGpuPipelineSpec,
        target_format: wgpu::TextureFormat,
        draw_bind_group_layout: &wgpu::BindGroupLayout,
        atlas_bind_group_layout: &wgpu::BindGroupLayout,
        brush_id: BrushId,
        blend_mode: DrawBlendMode,
    ) -> Result<wgpu::RenderPipeline, WgpuBrushExecutorError> {
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
            let blend = match blend_mode {
                DrawBlendMode::Alpha => Some(wgpu::BlendState::ALPHA_BLENDING),
                DrawBlendMode::Replace => Some(wgpu::BlendState::REPLACE),
            };
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
                        format: target_format,
                        blend,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(create_pipeline)).map_err(|_| {
            WgpuBrushExecutorError::PipelineCreationPanicked {
                brush_id,
                label: spec.label,
            }
        })
    }

    fn create_atlas_bind_group(
        &mut self,
        device: &wgpu::Device,
        atlas_storage: &AtlasStorageRuntime,
        source_backend_id: u8,
        cache_backend_id: Option<u8>,
        atlas_bind_group_layout: &wgpu::BindGroupLayout,
        brush_id: BrushId,
        has_ref_image: bool,
        has_cache_tile: bool,
    ) -> Result<wgpu::BindGroup, WgpuBrushExecutorError> {
        let source_view = if has_ref_image {
            let source_backend = atlas_storage.backend_resource(source_backend_id).ok_or(
                WgpuBrushExecutorError::MissingSourceBackend {
                    brush_id,
                    backend_id: source_backend_id,
                },
            )?;
            create_atlas_sampling_view(source_backend, "glaphica-brush-source-atlas-view")
        } else {
            // Use dummy texture when there's no ref_image to avoid binding the same texture
            // as both RESOURCE and COLOR_TARGET
            self.ensure_dummy_cache_texture(device).clone()
        };

        let cache_view = match (cache_backend_id, has_cache_tile) {
            (_, false) => self.ensure_dummy_cache_texture(device).clone(),
            (Some(cache_backend_id), true) => {
                let cache_backend = atlas_storage.backend_resource(cache_backend_id).ok_or(
                    WgpuBrushExecutorError::MissingCacheBackend {
                        brush_id,
                        backend_id: cache_backend_id,
                    },
                )?;
                create_atlas_sampling_view(cache_backend, "glaphica-brush-cache-atlas-view")
            }
            (None, true) => self.ensure_dummy_cache_texture(device).clone(),
        };

        let atlas_sampler = self.ensure_atlas_sampler(device);
        Ok(device.create_bind_group(&wgpu::BindGroupDescriptor {
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
        }))
    }

    fn prepare_draw_call(
        &mut self,
        context: &mut WgpuBrushContext<'_>,
        draw_op: &DrawOp,
    ) -> Result<PreparedDrawCall, BrushPipelineError> {
        let brush_index = Self::brush_index(draw_op.brush_id)?;

        let source_backend_id = draw_op
            .ref_image
            .map(|image| image.tile_key.backend_index())
            .unwrap_or(draw_op.tile_key.backend_index());

        let (cache_backend_id, needs_alpha_pipeline, needs_replace_pipeline, spec) = {
            let brush_context = self
                .brushes
                .get(brush_index)
                .ok_or(WgpuBrushExecutorError::BrushNotConfigured {
                    brush_id: draw_op.brush_id,
                })?
                .as_ref()
                .ok_or(WgpuBrushExecutorError::BrushNotConfigured {
                    brush_id: draw_op.brush_id,
                })?;
            (
                brush_context.cache_backend_id,
                brush_context.alpha_pipeline.is_none(),
                brush_context.replace_pipeline.is_none(),
                brush_context.spec,
            )
        };

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
        if should_trace_gpu_draw_exec_event() {
            eprintln!(
                "[PERF][gpu_exec_trace][draw] node={} tile_index={} dst={:?}@({}, {}, l{}) src={:?}@({}, {}, l{}) origin={:?} ref={:?}",
                draw_op.node_id.0,
                draw_op.tile_index,
                draw_op.tile_key,
                resolved.address.texel_offset.0,
                resolved.address.texel_offset.1,
                resolved.address.layer,
                source_tile_key,
                source_resolved.address.texel_offset.0,
                source_resolved.address.texel_offset.1,
                source_resolved.address.layer,
                draw_op.origin_tile,
                draw_op.ref_image.map(|image| image.tile_key)
            );
        }

        if source_resolved.format != resolved.format {
            return Err(WgpuBrushExecutorError::InconsistentSourceFormat {
                brush_id: draw_op.brush_id,
                expected: resolved.format,
                actual: source_resolved.format,
            }
            .into());
        }

        let device_features = context.gpu_context.device.features();
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

        let cache_format = cache_backend
            .map(|b| b.format)
            .unwrap_or(wgpu::TextureFormat::Rgba8Unorm);

        let atlas_layout_key = AtlasBindGroupLayoutKey {
            source_sample_type: Self::texture_sample_type_for_atlas(
                draw_op.brush_id,
                "source",
                source_resolved.format,
                device_features,
            )?,
            cache_sample_type: Self::texture_sample_type_for_atlas(
                draw_op.brush_id,
                "cache",
                cache_format,
                device_features,
            )?,
        };

        let draw_bind_group_layout =
            self.ensure_draw_bind_group_layout(&context.gpu_context.device);
        let atlas_bind_group_layout =
            self.ensure_atlas_bind_group_layout(&context.gpu_context.device, atlas_layout_key);
        let cache_resolved = if draw_op.origin_tile == glaphica_core::TileKey::EMPTY {
            None
        } else {
            context.atlas_storage.resolve(draw_op.origin_tile)
        };

        {
            let brush_context = self.brushes[brush_index].as_mut().unwrap();
            if brush_context.cached_stroke_id != Some(draw_op.stroke_id) {
                brush_context.cached_stroke_id = Some(draw_op.stroke_id);
                brush_context.stroke_cached_bind_groups.clear();
            }
        }
        let atlas_bind_group_key = StrokeAtlasBindGroupKey {
            source_backend_id,
            cache_backend_id,
            has_ref_image: draw_op.ref_image.is_some(),
            has_cache_tile: cache_resolved.is_some(),
            layout_key: atlas_layout_key,
        };
        let atlas_bind_group = self.get_or_create_stroke_atlas_bind_group(
            draw_op.brush_id,
            brush_index,
            atlas_bind_group_key,
            &context.gpu_context.device,
            context.atlas_storage,
            &atlas_bind_group_layout,
        )?;

        if needs_alpha_pipeline {
            let pipeline = Self::create_render_pipeline(
                &context.gpu_context.device,
                &spec,
                resolved.format,
                &draw_bind_group_layout,
                &atlas_bind_group_layout,
                draw_op.brush_id,
                DrawBlendMode::Alpha,
            )?;
            let brush_context = self.brushes[brush_index].as_mut().unwrap();
            brush_context.alpha_pipeline = Some(pipeline);
        }

        if needs_replace_pipeline {
            let pipeline = Self::create_render_pipeline(
                &context.gpu_context.device,
                &spec,
                resolved.format,
                &draw_bind_group_layout,
                &atlas_bind_group_layout,
                draw_op.brush_id,
                DrawBlendMode::Replace,
            )?;
            let brush_context = self.brushes[brush_index].as_mut().unwrap();
            brush_context.replace_pipeline = Some(pipeline);
        }

        let pipeline = {
            let brush_context = self.brushes[brush_index].as_ref().unwrap();
            match draw_op.blend_mode {
                DrawBlendMode::Alpha => brush_context.alpha_pipeline.as_ref().unwrap().clone(),
                DrawBlendMode::Replace => {
                    brush_context.replace_pipeline.as_ref().unwrap().clone()
                }
            }
        };

        let input_bytes = encode_input_bytes(&draw_op.input);
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
            cache_tile_origin_x: cache_resolved
                .map(|resolved| resolved.address.texel_offset.0)
                .unwrap_or(0),
            cache_tile_origin_y: cache_resolved
                .map(|resolved| resolved.address.texel_offset.1)
                .unwrap_or(0),
            cache_tile_layer: cache_resolved.map(|resolved| resolved.address.layer).unwrap_or(0),
            has_cache_tile: if cache_resolved.is_some() { 1 } else { 0 },
            _pad1: 0,
            _pad2: 0,
        };
        let params_bytes = encode_shader_params_bytes(params);
        let limits = context.gpu_context.device.limits();
        let input_alignment = u64::from(limits.min_storage_buffer_offset_alignment);
        let params_alignment = u64::from(limits.min_uniform_buffer_offset_alignment);
        let input_binding_size = (input_bytes.len() as u64).max(4);
        let input_stride = align_up_u64(input_binding_size, input_alignment.max(1));
        let params_stride = align_up_u64(BRUSH_SHADER_PARAMS_SIZE, params_alignment.max(1));

        let brush_context = self.brushes[brush_index].as_mut().unwrap();
        let should_recreate_ring = match brush_context.draw_ring.as_ref() {
            Some(ring) => {
                ring.input_binding_size != input_binding_size
                    || ring.input_stride != input_stride
                    || ring.params_stride != params_stride
            }
            None => true,
        };
        if should_recreate_ring {
            brush_context.draw_ring = Some(Self::create_brush_draw_ring(
                &context.gpu_context.device,
                &draw_bind_group_layout,
                input_binding_size,
                input_stride,
                params_stride,
                BRUSH_RING_INITIAL_SLOTS,
            ));
        }

        let ring_ref = brush_context.draw_ring.as_ref().unwrap();
        let next_input_end = ring_ref.input_cursor.saturating_add(ring_ref.input_stride);
        let next_params_end = ring_ref.params_cursor.saturating_add(ring_ref.params_stride);
        let input_capacity = ring_ref.input_buffer.size();
        let params_capacity = ring_ref.params_buffer.size();
        if next_input_end > input_capacity || next_params_end > params_capacity {
            let needed_slots_input = next_input_end.div_ceil(ring_ref.input_stride);
            let needed_slots_params = next_params_end.div_ceil(ring_ref.params_stride);
            let current_slots_input = input_capacity.div_ceil(ring_ref.input_stride).max(1);
            let current_slots_params = params_capacity.div_ceil(ring_ref.params_stride).max(1);
            let mut slots = current_slots_input.max(current_slots_params).max(1);
            let needed_slots = needed_slots_input.max(needed_slots_params).max(1);
            while slots < needed_slots {
                slots = slots.saturating_mul(2);
            }
            brush_context.draw_ring = Some(Self::create_brush_draw_ring(
                &context.gpu_context.device,
                &draw_bind_group_layout,
                ring_ref.input_binding_size,
                ring_ref.input_stride,
                ring_ref.params_stride,
                slots,
            ));
        }

        let ring = brush_context.draw_ring.as_mut().unwrap();
        let stroke_changed = ring.last_stroke_id != Some(draw_op.stroke_id);
        let input_offset = ring.input_cursor;
        let params_offset = ring.params_cursor;
        ring.input_cursor = ring.input_cursor.saturating_add(ring.input_stride);
        ring.params_cursor = ring.params_cursor.saturating_add(ring.params_stride);
        if stroke_changed {
            ring.last_stroke_id = Some(draw_op.stroke_id);
        }

        context
            .gpu_context
            .queue
            .write_buffer(&ring.input_buffer, input_offset, &input_bytes);
        context
            .gpu_context
            .queue
            .write_buffer(&ring.params_buffer, params_offset, &params_bytes);

        let input_dynamic_offset = u32::try_from(input_offset).map_err(|_| {
            WgpuBrushExecutorError::DynamicOffsetOutOfRange {
                brush_id: draw_op.brush_id,
                offset: input_offset,
            }
        })?;
        let params_dynamic_offset = u32::try_from(params_offset).map_err(|_| {
            WgpuBrushExecutorError::DynamicOffsetOutOfRange {
                brush_id: draw_op.brush_id,
                offset: params_offset,
            }
        })?;

        Ok(PreparedDrawCall {
            pass_key: DrawPassKey {
                backend_id: draw_op.tile_key.backend_index(),
                layer: resolved.address.layer,
                format: resolved.format,
            },
            scissor_x: resolved.address.texel_offset.0,
            scissor_y: resolved.address.texel_offset.1,
            pipeline,
            draw_bind_group: ring.draw_bind_group.clone(),
            atlas_bind_group,
            input_dynamic_offset,
            params_dynamic_offset,
        })
    }

    fn encode_draw(
        &mut self,
        context: &mut WgpuBrushContext<'_>,
        draw_op: &DrawOp,
        encoder: &mut wgpu::CommandEncoder,
        retain_resources: bool,
    ) -> Result<(), BrushPipelineError> {
        let call = self.prepare_draw_call(context, draw_op)?;
        let backend = context
            .atlas_storage
            .backend_resource(call.pass_key.backend_id)
            .ok_or(WgpuBrushExecutorError::MissingTargetAtlasBackend {
                brush_id: draw_op.brush_id,
            })?;
        let attachment_view = backend
            .texture2d_array
            .create_view(&wgpu::TextureViewDescriptor {
                label: Some("glaphica-brush-atlas-layer-view"),
                format: Some(call.pass_key.format),
                dimension: Some(wgpu::TextureViewDimension::D2),
                usage: Some(wgpu::TextureUsages::RENDER_ATTACHMENT),
                aspect: wgpu::TextureAspect::All,
                base_mip_level: 0,
                mip_level_count: Some(1),
                base_array_layer: call.pass_key.layer,
                array_layer_count: Some(1),
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
            pass.set_pipeline(&call.pipeline);
            pass.set_bind_group(
                0,
                &call.draw_bind_group,
                &[call.input_dynamic_offset, call.params_dynamic_offset],
            );
            pass.set_bind_group(1, &call.atlas_bind_group, &[]);
            pass.set_scissor_rect(call.scissor_x, call.scissor_y, ATLAS_TILE_SIZE, ATLAS_TILE_SIZE);
            pass.draw(0..3, 0..1);
        }

        if retain_resources {
            self.transient_draw_resources.push(TransientDrawResources {
                _atlas_bind_group: call.atlas_bind_group,
                _attachment_view: attachment_view,
            });
        }

        Ok(())
    }
}

fn to_wgpu_texture_format(format: TextureFormat) -> wgpu::TextureFormat {
    match format {
        TextureFormat::Rgba8Unorm => wgpu::TextureFormat::Rgba8Unorm,
        TextureFormat::Rgba16Float => wgpu::TextureFormat::Rgba16Float,
        TextureFormat::Bgra8Unorm => wgpu::TextureFormat::Bgra8Unorm,
        TextureFormat::R8Unorm => wgpu::TextureFormat::R8Unorm,
        TextureFormat::Rg8Unorm => wgpu::TextureFormat::Rg8Unorm,
    }
}

impl BrushDrawExecutor<WgpuBrushContext<'_>> for WgpuBrushExecutor {
    fn execute_draw(
        &mut self,
        context: &mut WgpuBrushContext<'_>,
        draw_op: &DrawOp,
        _layout: BrushDrawInputLayout,
    ) -> Result<(), BrushPipelineError> {
        let mut encoder =
            context
                .gpu_context
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("glaphica-brush-draw-encoder"),
                });
        self.encode_draw(context, draw_op, &mut encoder, false)?;
        context.gpu_context.queue.submit(Some(encoder.finish()));
        self.clear_transient_draw_resources();
        Ok(())
    }

    fn execute_draw_with_encoder(
        &mut self,
        context: &mut WgpuBrushContext<'_>,
        draw_op: &DrawOp,
        _layout: BrushDrawInputLayout,
        encoder: &mut wgpu::CommandEncoder,
    ) -> Result<(), BrushPipelineError> {
        self.encode_draw(context, draw_op, encoder, true)
    }

    fn execute_draw_batch_with_encoder(
        &mut self,
        context: &mut WgpuBrushContext<'_>,
        draw_ops: &[&DrawOp],
        _layouts: &[BrushDrawInputLayout],
        encoder: &mut wgpu::CommandEncoder,
    ) -> Result<(), BrushPipelineError> {
        if draw_ops.is_empty() {
            return Ok(());
        }

        let mut calls = Vec::with_capacity(draw_ops.len());
        for draw_op in draw_ops {
            calls.push(self.prepare_draw_call(context, draw_op)?);
        }

        let mut start = 0usize;
        while start < calls.len() {
            let pass_key = calls[start].pass_key;
            let mut end = start + 1;
            while end < calls.len() && calls[end].pass_key == pass_key {
                end += 1;
            }

            let backend = context
                .atlas_storage
                .backend_resource(pass_key.backend_id)
                .ok_or(WgpuBrushExecutorError::MissingTargetAtlasBackend {
                    brush_id: draw_ops[start].brush_id,
                })?;
            let attachment_view = backend
                .texture2d_array
                .create_view(&wgpu::TextureViewDescriptor {
                    label: Some("glaphica-brush-atlas-layer-view"),
                    format: Some(pass_key.format),
                    dimension: Some(wgpu::TextureViewDimension::D2),
                    usage: Some(wgpu::TextureUsages::RENDER_ATTACHMENT),
                    aspect: wgpu::TextureAspect::All,
                    base_mip_level: 0,
                    mip_level_count: Some(1),
                    base_array_layer: pass_key.layer,
                    array_layer_count: Some(1),
                });

            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("glaphica-brush-pass-batch"),
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

                for call in &calls[start..end] {
                    pass.set_pipeline(&call.pipeline);
                    pass.set_bind_group(
                        0,
                        &call.draw_bind_group,
                        &[call.input_dynamic_offset, call.params_dynamic_offset],
                    );
                    pass.set_bind_group(1, &call.atlas_bind_group, &[]);
                    pass.set_scissor_rect(
                        call.scissor_x,
                        call.scissor_y,
                        ATLAS_TILE_SIZE,
                        ATLAS_TILE_SIZE,
                    );
                    pass.draw(0..3, 0..1);
                }
            }

            self.transient_draw_resources.push(TransientDrawResources {
                _atlas_bind_group: calls[start].atlas_bind_group.clone(),
                _attachment_view: attachment_view,
            });

            start = end;
        }

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

fn encode_shader_params_bytes(params: BrushShaderParams) -> [u8; 60] {
    let mut bytes = [0u8; 60];
    bytes[0..4].copy_from_slice(&params.input_len.to_ne_bytes());
    bytes[4..8].copy_from_slice(&params.tile_origin_x.to_ne_bytes());
    bytes[8..12].copy_from_slice(&params.tile_origin_y.to_ne_bytes());
    bytes[12..16].copy_from_slice(&params.tile_layer.to_ne_bytes());
    bytes[16..20].copy_from_slice(&params.tile_size_x.to_ne_bytes());
    bytes[20..24].copy_from_slice(&params.tile_size_y.to_ne_bytes());
    bytes[24..28].copy_from_slice(&params.src_tile_origin_x.to_ne_bytes());
    bytes[28..32].copy_from_slice(&params.src_tile_origin_y.to_ne_bytes());
    bytes[32..36].copy_from_slice(&params.src_tile_layer.to_ne_bytes());
    bytes[36..40].copy_from_slice(&params.cache_tile_origin_x.to_ne_bytes());
    bytes[40..44].copy_from_slice(&params.cache_tile_origin_y.to_ne_bytes());
    bytes[44..48].copy_from_slice(&params.cache_tile_layer.to_ne_bytes());
    bytes[48..52].copy_from_slice(&params.has_cache_tile.to_ne_bytes());
    bytes[52..56].copy_from_slice(&params._pad1.to_ne_bytes());
    bytes[56..60].copy_from_slice(&params._pad2.to_ne_bytes());
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
            cache_tile_origin_x: 300,
            cache_tile_origin_y: 400,
            cache_tile_layer: 10,
            has_cache_tile: 1,
            _pad1: 0,
            _pad2: 0,
        };
        let encoded = encode_shader_params_bytes(params);
        assert_eq!(encoded.len(), 60);
        assert_eq!(
            u32::from_ne_bytes([encoded[0], encoded[1], encoded[2], encoded[3]]),
            3
        );
        assert_eq!(
            u32::from_ne_bytes([encoded[12], encoded[13], encoded[14], encoded[15]]),
            2
        );
        assert_eq!(
            u32::from_ne_bytes([encoded[40], encoded[41], encoded[42], encoded[43]]),
            400
        );
    }
}
