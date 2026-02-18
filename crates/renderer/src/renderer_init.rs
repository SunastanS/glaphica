//! Renderer initialization and GPU resource construction.
//!
//! This module owns `Renderer::new` and helper routines that allocate pipelines,
//! bind groups, and per-frame buffers.

use std::collections::HashMap;
use std::sync::mpsc;

use render_protocol::TransformMatrix4x4;
use tiles::{GroupTileAtlasStore, TileAtlasConfig, TileAtlasGpuArray};

use crate::{
    create_composite_pipeline, multiply_blend_state, BrushWorkState, CacheState, DataState,
    DirtyStateStore, FrameState, FrameSync, GpuFrameTimingSlot, GpuFrameTimingSlotState,
    GpuFrameTimingState, GpuState, GroupTargetCacheEntry, InputState, RenderDataResolver, Renderer,
    TileInstanceGpu, ViewState, GPU_TIMING_SLOTS, IDENTITY_MATRIX, INITIAL_TILE_INSTANCE_CAPACITY,
};

impl Renderer {
    pub fn new(
        device: wgpu::Device,
        queue: wgpu::Queue,
        surface: wgpu::Surface<'static>,
        surface_config: wgpu::SurfaceConfiguration,
        tile_atlas: TileAtlasGpuArray,
        render_data_resolver: Box<dyn RenderDataResolver>,
    ) -> (Self, crate::ViewOpSender) {
        let (view_sender, view_receiver) = mpsc::channel();

        surface.configure(&device, &surface_config);

        let view_uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("renderer.view_uniform"),
            size: std::mem::size_of::<TransformMatrix4x4>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(
            &view_uniform_buffer,
            0,
            bytemuck::bytes_of(&IDENTITY_MATRIX),
        );

        let per_frame_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("renderer.per_frame_layout"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::VERTEX,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::VERTEX,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: true },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                ],
            });

        let atlas_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("renderer.atlas_layout"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
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
                ],
            });

        let tile_sampler_linear = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("renderer.tile_sampler.linear"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let tile_sampler_nearest = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("renderer.tile_sampler.nearest"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("renderer.tile_composite"),
            source: wgpu::ShaderSource::Wgsl(include_str!("tile_composite.wgsl").into()),
        });
        let slot_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("renderer.tile_composite.slot"),
            source: wgpu::ShaderSource::Wgsl(include_str!("tile_composite_slot.wgsl").into()),
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("renderer.composite_layout"),
            bind_group_layouts: &[&per_frame_bind_group_layout, &atlas_bind_group_layout],
            immediate_size: 0,
        });
        let alpha_composite_pipeline = create_composite_pipeline(
            &device,
            &pipeline_layout,
            &shader,
            surface_config.format,
            wgpu::BlendState::ALPHA_BLENDING,
            "renderer.composite_pipeline.alpha",
        );
        let multiply_composite_pipeline = create_composite_pipeline(
            &device,
            &pipeline_layout,
            &shader,
            surface_config.format,
            multiply_blend_state(),
            "renderer.composite_pipeline.multiply",
        );
        let alpha_composite_slot_pipeline = create_composite_pipeline(
            &device,
            &pipeline_layout,
            &slot_shader,
            surface_config.format,
            wgpu::BlendState::ALPHA_BLENDING,
            "renderer.composite_pipeline.slot.alpha",
        );
        let multiply_composite_slot_pipeline = create_composite_pipeline(
            &device,
            &pipeline_layout,
            &slot_shader,
            surface_config.format,
            multiply_blend_state(),
            "renderer.composite_pipeline.slot.multiply",
        );

        let atlas_bind_group_linear = Self::create_atlas_bind_group(
            &device,
            &atlas_bind_group_layout,
            tile_atlas.view(),
            &tile_sampler_linear,
            "renderer.atlas_bind_group.linear",
        );

        let (group_tile_store, group_tile_atlas) = GroupTileAtlasStore::with_config(
            &device,
            TileAtlasConfig {
                max_layers: 2,
                format: surface_config.format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_DST
                    | wgpu::TextureUsages::COPY_SRC,
            },
        )
        .expect("create group tile atlas");
        let group_atlas_bind_group_linear = Self::create_atlas_bind_group(
            &device,
            &atlas_bind_group_layout,
            group_tile_atlas.view(),
            &tile_sampler_linear,
            "renderer.group_atlas_bind_group.linear",
        );
        let group_atlas_bind_group_nearest = Self::create_atlas_bind_group(
            &device,
            &atlas_bind_group_layout,
            group_tile_atlas.view(),
            &tile_sampler_nearest,
            "renderer.group_atlas_bind_group.nearest",
        );

        let tile_instance_buffer =
            Self::create_tile_instance_buffer(&device, INITIAL_TILE_INSTANCE_CAPACITY);
        let per_frame_bind_group = Self::create_per_frame_bind_group(
            &device,
            &per_frame_bind_group_layout,
            &view_uniform_buffer,
            &tile_instance_buffer,
        );
        let gpu_timing = Self::create_gpu_frame_timing_state(&device, &queue);
        let brush_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("renderer.brush_pipeline_layout"),
                bind_group_layouts: &[],
                immediate_size: 0,
            });

        let renderer = Self {
            input_state: InputState { view_receiver },
            data_state: DataState {
                render_data_resolver,
            },
            gpu_state: GpuState {
                device,
                queue,
                surface,
                surface_config,
                view_uniform_buffer,
                alpha_composite_pipeline,
                multiply_composite_pipeline,
                alpha_composite_slot_pipeline,
                multiply_composite_slot_pipeline,
                per_frame_bind_group_layout,
                per_frame_bind_group,
                group_tile_store,
                group_tile_atlas,
                group_atlas_bind_group_linear,
                group_atlas_bind_group_nearest,
                tile_instance_buffer,
                tile_instance_capacity: INITIAL_TILE_INSTANCE_CAPACITY,
                tile_instance_gpu_staging: Vec::new(),
                atlas_bind_group_linear,
                tile_atlas,
                gpu_timing,
                brush_pipeline_layout,
            },
            view_state: ViewState {
                view_matrix: IDENTITY_MATRIX,
                view_matrix_dirty: false,
                viewport: None,
                brush_command_quota: 0,
                drop_before_revision: 0,
                present_requested: false,
            },
            cache_state: CacheState {
                group_target_cache: HashMap::<u64, GroupTargetCacheEntry>::new(),
                leaf_draw_cache: HashMap::new(),
            },
            brush_work_state: BrushWorkState {
                pending_commands: std::collections::VecDeque::new(),
                pending_dab_count: 0,
                carry_credit_dabs: 0,
                prepared_programs: HashMap::new(),
                active_program_by_brush: HashMap::new(),
                active_strokes: HashMap::new(),
                executing_strokes: HashMap::new(),
                reference_sets: HashMap::new(),
                stroke_reference_set: HashMap::new(),
                stroke_target_layer: HashMap::new(),
                ended_strokes_pending_merge: HashMap::new(),
            },
            frame_state: FrameState {
                bound_tree: None,
                cached_render_tree: None,
                render_tree_dirty: false,
                dirty_state_store: DirtyStateStore::with_document_dirty(true),
                frame_sync: FrameSync::default(),
            },
        };

        (renderer, crate::ViewOpSender(view_sender))
    }

    pub(super) fn create_tile_instance_buffer(
        device: &wgpu::Device,
        capacity: usize,
    ) -> wgpu::Buffer {
        let instance_size = std::mem::size_of::<TileInstanceGpu>() as u64;
        let capacity_u64 = u64::try_from(capacity).expect("tile instance capacity exceeds u64");
        let size = capacity_u64
            .checked_mul(instance_size)
            .expect("tile instance buffer size overflow");
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("renderer.tile_instances"),
            size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    pub(super) fn create_per_frame_bind_group(
        device: &wgpu::Device,
        per_frame_bind_group_layout: &wgpu::BindGroupLayout,
        view_uniform_buffer: &wgpu::Buffer,
        tile_instance_buffer: &wgpu::Buffer,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("renderer.per_frame_bind_group"),
            layout: per_frame_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: view_uniform_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: tile_instance_buffer.as_entire_binding(),
                },
            ],
        })
    }

    pub(super) fn create_atlas_bind_group(
        device: &wgpu::Device,
        atlas_bind_group_layout: &wgpu::BindGroupLayout,
        atlas_view: &wgpu::TextureView,
        sampler: &wgpu::Sampler,
        label: &str,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(label),
            layout: atlas_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(atlas_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
            ],
        })
    }

    pub(super) fn ensure_tile_instance_capacity(&mut self, required_len: usize) {
        if required_len <= self.gpu_state.tile_instance_capacity {
            return;
        }

        let required_capacity = required_len.max(INITIAL_TILE_INSTANCE_CAPACITY);
        let expanded_capacity = required_capacity
            .checked_next_power_of_two()
            .expect("tile instance capacity overflow");
        self.gpu_state.tile_instance_buffer =
            Self::create_tile_instance_buffer(&self.gpu_state.device, expanded_capacity);
        self.gpu_state.per_frame_bind_group = Self::create_per_frame_bind_group(
            &self.gpu_state.device,
            &self.gpu_state.per_frame_bind_group_layout,
            &self.gpu_state.view_uniform_buffer,
            &self.gpu_state.tile_instance_buffer,
        );
        self.gpu_state.tile_instance_capacity = expanded_capacity;
    }

    pub(super) fn create_gpu_frame_timing_state(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> GpuFrameTimingState {
        if !device.features().contains(wgpu::Features::TIMESTAMP_QUERY) {
            return GpuFrameTimingState {
                query_set: None,
                timestamp_period_ns: 0.0,
                slots: Vec::new(),
                latest_report: None,
            };
        }

        let query_count = u32::try_from(
            GPU_TIMING_SLOTS
                .checked_mul(2)
                .expect("gpu timing query count overflow"),
        )
        .expect("gpu timing query count exceeds u32");
        let query_set = device.create_query_set(&wgpu::QuerySetDescriptor {
            label: Some("renderer.frame_gpu_timing.query_set"),
            ty: wgpu::QueryType::Timestamp,
            count: query_count,
        });

        let mut slots = Vec::with_capacity(GPU_TIMING_SLOTS);
        for slot_index in 0..GPU_TIMING_SLOTS {
            let resolve_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(&format!("renderer.frame_gpu_timing.resolve.{slot_index}")),
                size: 16,
                usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            });
            let readback_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(&format!("renderer.frame_gpu_timing.readback.{slot_index}")),
                size: 16,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            slots.push(GpuFrameTimingSlot {
                resolve_buffer,
                readback_buffer,
                state: GpuFrameTimingSlotState::Idle,
            });
        }

        GpuFrameTimingState {
            query_set: Some(query_set),
            timestamp_period_ns: f64::from(queue.get_timestamp_period()),
            slots,
            latest_report: None,
        }
    }
}
