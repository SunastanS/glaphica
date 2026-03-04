use std::sync::Arc;
use std::{fmt::Display, path::Path};

use brushes::{BrushGpuPipelineRegistry, BrushLayoutRegistry, BrushRegistryError, BrushSpec};
use document::{FlatRenderTree, SharedRenderTree};
use glaphica_core::{
    AtlasLayout, BackendId, BackendKind, BrushId, ImageDirtyTracker, RenderTreeGeneration, TileDirtyTracker,
};
use gpu_runtime::{
    atlas_runtime::AtlasStorageRuntime,
    brush_runtime::BrushGpuRuntime,
    surface_runtime::{SurfaceError, SurfaceRuntime},
    wgpu_brush_executor::{WgpuBrushContext, WgpuBrushExecutorError},
    GpuContext, GpuContextInitDescriptor, RenderContext, RenderExecutor,
};
use thread_protocol::GpuCmdMsg;

use crate::{config, screen_blitter::ScreenBlitter};

pub struct MainThreadState {
    gpu_context: Arc<GpuContext>,
    atlas_storage: AtlasStorageRuntime,
    surface_runtime: Option<SurfaceRuntime>,
    screen_blitter: ScreenBlitter,
    render_executor: RenderExecutor,
    brush_runtime: BrushGpuRuntime<gpu_runtime::wgpu_brush_executor::WgpuBrushExecutor>,
    brush_layouts: BrushLayoutRegistry,
    brush_pipeline_registry: BrushGpuPipelineRegistry,
    shared_tree: Arc<SharedRenderTree>,
    image_dirty_tracker: ImageDirtyTracker,
    tile_dirty_tracker: TileDirtyTracker,
    next_brush_cache_backend_id: u8,
}

impl MainThreadState {
    pub async fn init() -> Result<Self, InitError> {
        let gpu_context = Arc::new(
            GpuContext::init(&GpuContextInitDescriptor::default())
                .await
                .map_err(InitError::GpuContext)?,
        );
        Self::init_with_gpu_context(gpu_context).await
    }

    pub async fn init_with_gpu_context(gpu_context: Arc<GpuContext>) -> Result<Self, InitError> {
        let mut atlas_storage = AtlasStorageRuntime::with_capacity(config::atlas_storage::INITIAL_BACKEND_CAPACITY);
        atlas_storage
            .create_backend(
                &gpu_context.device,
                0,
                BackendKind::Leaf,
                AtlasLayout::Small11,
                Default::default(),
            )
            .map_err(InitError::Atlas)?;
        atlas_storage
            .create_backend(
                &gpu_context.device,
                1,
                BackendKind::BranchCache,
                AtlasLayout::Small11,
                Default::default(),
            )
            .map_err(InitError::Atlas)?;

        Ok(Self {
            gpu_context,
            atlas_storage,
            surface_runtime: None,
            screen_blitter: ScreenBlitter::new(),
            render_executor: RenderExecutor::new(),
            brush_runtime: BrushGpuRuntime::new(
                gpu_runtime::wgpu_brush_executor::WgpuBrushExecutor::new(),
            ),
            brush_layouts: BrushLayoutRegistry::new(config::registry_capacities::BRUSH_LAYOUT_REGISTRY),
            brush_pipeline_registry: BrushGpuPipelineRegistry::new(config::registry_capacities::BRUSH_PIPELINE_REGISTRY),
            shared_tree: Arc::new(SharedRenderTree::new(FlatRenderTree {
                generation: RenderTreeGeneration(0),
                nodes: Arc::new(std::collections::HashMap::new()),
                root_id: None,
            })),
            image_dirty_tracker: ImageDirtyTracker::default(),
            tile_dirty_tracker: TileDirtyTracker::default(),
            next_brush_cache_backend_id: 2,
        })
    }

    pub fn register_brush<S: BrushSpec>(
        &mut self,
        brush_id: BrushId,
        brush: &S,
    ) -> Result<Option<BackendId>, BrushRegisterError> {
        let layout = brush.draw_input_layout();
        let spec = brush.gpu_pipeline_spec();
        
        self.brush_layouts
            .register_layout(brush_id, layout)
            .map_err(BrushRegisterError::Layout)?;
        self.brush_pipeline_registry
            .register_pipeline_spec(brush_id, spec)
            .map_err(BrushRegisterError::Pipeline)?;

        let cache_backend_id = match brush.cache_backend_kind() {
            Some(kind) => {
                let id = self.next_brush_cache_backend_id;
                self.atlas_storage
                    .create_backend(
                        &self.gpu_context.device,
                        id,
                        kind,
                        AtlasLayout::Small11,
                        Default::default(),
                    )
                    .map_err(|_| BrushRegisterError::CacheBackendAlloc { brush_id })?;
                self.next_brush_cache_backend_id += 1;
                Some(BackendId::new(id))
            }
            None => None,
        };

        self.brush_runtime
            .executor_mut()
            .configure_brush(brush_id, spec, cache_backend_id.map(|id| id.raw()))
            .map_err(BrushRegisterError::Executor)?;
        
        Ok(cache_backend_id)
    }

    pub fn set_shared_tree(&mut self, shared_tree: Arc<SharedRenderTree>) {
        self.shared_tree = shared_tree;
    }

    pub fn gpu_context(&self) -> &Arc<GpuContext> {
        &self.gpu_context
    }

    pub fn set_surface(&mut self, surface_runtime: SurfaceRuntime) {
        self.surface_runtime = Some(surface_runtime);
    }

    pub fn resize_surface(&mut self, width: u32, height: u32) {
        if let Some(surface) = &mut self.surface_runtime {
            surface.resize(&self.gpu_context.device, width, height);
        }
    }

    pub fn present_to_screen(&mut self) -> Result<(), PresentError> {
        let surface = self.surface_runtime.as_mut().ok_or(PresentError::NoSurface)?;
        let frame = surface.acquire_frame().map_err(PresentError::Surface)?;
        let tree = self.shared_tree.read();

        self.screen_blitter.blit(
            &self.gpu_context.device,
            &self.gpu_context.queue,
            &self.atlas_storage,
            &tree,
            &frame.view,
            surface.format(),
            surface.width(),
            surface.height(),
        );

        SurfaceRuntime::present(frame);
        Ok(())
    }

    pub fn process_render(&mut self) -> bool {
        let tree = self.shared_tree.read();
        let cmds = tree.build_render_cmds(&self.image_dirty_tracker);
        
        if cmds.is_empty() {
            return false;
        }

        let mut context = RenderContext {
            gpu_context: &self.gpu_context,
            atlas_storage: &self.atlas_storage,
        };

        if let Err(e) = self.render_executor.execute(&mut context, &cmds) {
            eprintln!("Render execution failed: {e}");
            return false;
        }

        self.image_dirty_tracker.clear();
        true
    }

    pub fn clear_dirty_markers(&mut self) {
        self.image_dirty_tracker.clear();
        self.tile_dirty_tracker.clear();
    }

    pub fn process_gpu_commands(&mut self, commands: &[GpuCmdMsg]) {
        for cmd in commands {
            match cmd {
                GpuCmdMsg::DrawOp(draw_op) => {
                let mut context = WgpuBrushContext {
                    gpu_context: &self.gpu_context,
                    atlas_storage: &self.atlas_storage,
                    source_backend_id: draw_op.tile_key.backend_index(),
                };
                if let Err(e) = self.brush_runtime.apply_draw_op(
                    &mut context,
                    draw_op,
                    &self.brush_layouts,
                ) {
                    eprintln!("Draw operation failed: {e}");
                }
                    self.image_dirty_tracker.mark(draw_op.node_id, draw_op.tile_index);
                    self.tile_dirty_tracker.mark(draw_op.tile_key);
                }
                GpuCmdMsg::CopyOp(copy_op) => {
                    let mut context = RenderContext {
                        gpu_context: &self.gpu_context,
                        atlas_storage: &self.atlas_storage,
                    };
                    if let Err(e) = self.render_executor.copy_tile(&mut context, copy_op) {
                        eprintln!("Copy operation failed: {e}");
                    }
                    self.tile_dirty_tracker.mark(copy_op.dst_tile_key);
                }
                GpuCmdMsg::ClearOp(clear_op) => {
                    let mut context = RenderContext {
                        gpu_context: &self.gpu_context,
                        atlas_storage: &self.atlas_storage,
                    };
                    if let Err(e) = self.render_executor.clear_tile(&mut context, clear_op) {
                        eprintln!("Clear operation failed: {e}");
                    }
                    self.tile_dirty_tracker.mark(clear_op.tile_key);
                }
                GpuCmdMsg::RenderTreeUpdated(msg) => {
                    for branch_id in &msg.dirty_branch_caches {
                        let tree = self.shared_tree.read();
                        if let Some(node) = tree.nodes.get(branch_id) {
                            let cache = match &node.kind {
                                document::FlatNodeKind::Branch { cache, .. } => cache,
                                document::FlatNodeKind::Leaf { .. } => continue,
                            };
                            for tile_index in 0..cache.tile_count() {
                                self.image_dirty_tracker.mark(*branch_id, tile_index);
                            }
                        }
                    }
                }
                GpuCmdMsg::TileSlotKeyUpdate(op) => {
                    for (node_id, tile_index, tile_key) in &op.updates {
                        self.image_dirty_tracker.mark(*node_id, *tile_index);
                        self.tile_dirty_tracker.mark(*tile_key);
                    }
                }
            }
        }
    }

    pub fn save_screenshot(
        &mut self,
        output_path: &Path,
        width: u32,
        height: u32,
    ) -> Result<(), ScreenshotError> {
        if width == 0 || height == 0 {
            return Err(ScreenshotError::InvalidSize);
        }
        if let Some(parent_dir) = output_path.parent() {
            if !parent_dir.as_os_str().is_empty() {
                std::fs::create_dir_all(parent_dir).map_err(ScreenshotError::Io)?;
            }
        }

        let screenshot_texture = self.gpu_context.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("glaphica-e2e-screenshot-texture"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let screenshot_view = screenshot_texture.create_view(&wgpu::TextureViewDescriptor::default());

        let tree = self.shared_tree.read();
        self.screen_blitter.blit(
            &self.gpu_context.device,
            &self.gpu_context.queue,
            &self.atlas_storage,
            &tree,
            &screenshot_view,
            wgpu::TextureFormat::Rgba8Unorm,
            width,
            height,
        );

        let bytes_per_pixel = 4u32;
        let unpadded_bytes_per_row = width.saturating_mul(bytes_per_pixel);
        let padded_bytes_per_row = unpadded_bytes_per_row.div_ceil(256).saturating_mul(256);
        let output_buffer_size = u64::from(padded_bytes_per_row) * u64::from(height);

        let output_buffer = self.gpu_context.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("glaphica-e2e-screenshot-readback-buffer"),
            size: output_buffer_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut encoder = self
            .gpu_context
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("glaphica-e2e-screenshot-readback-encoder"),
            });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &screenshot_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &output_buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_bytes_per_row),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        self.gpu_context.queue.submit(Some(encoder.finish()));

        let buffer_slice = output_buffer.slice(..);
        let (result_sender, result_receiver) = std::sync::mpsc::channel();
        buffer_slice.map_async(wgpu::MapMode::Read, move |result| {
            if let Err(send_error) = result_sender.send(result) {
                eprintln!("screenshot map callback send failed: {send_error}");
            }
        });
        let _ = self
            .gpu_context
            .device
            .poll(wgpu::PollType::wait_indefinitely());
        let map_result = result_receiver.recv().map_err(ScreenshotError::MapChannel)?;
        map_result.map_err(ScreenshotError::Map)?;

        let mapped_range = buffer_slice.get_mapped_range();
        let unpadded_row_len =
            usize::try_from(unpadded_bytes_per_row).map_err(|_| ScreenshotError::InvalidSize)?;
        let padded_row_len = usize::try_from(padded_bytes_per_row).map_err(|_| ScreenshotError::InvalidSize)?;
        let height_usize = usize::try_from(height).map_err(|_| ScreenshotError::InvalidSize)?;
        let mut pixels = vec![0u8; unpadded_row_len.saturating_mul(height_usize)];
        for row_index in 0..height_usize {
            let source_start = row_index * padded_row_len;
            let source_end = source_start + unpadded_row_len;
            let destination_start = row_index * unpadded_row_len;
            let destination_end = destination_start + unpadded_row_len;
            pixels[destination_start..destination_end]
                .copy_from_slice(&mapped_range[source_start..source_end]);
        }
        drop(mapped_range);
        output_buffer.unmap();

        let file = std::fs::File::create(output_path).map_err(ScreenshotError::Io)?;
        let mut encoder = png::Encoder::new(file, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(ScreenshotError::Png)?;
        writer
            .write_image_data(&pixels)
            .map_err(ScreenshotError::Png)?;
        Ok(())
    }
}

#[derive(Debug)]
pub enum InitError {
    GpuContext(gpu_runtime::GpuContextInitError),
    Atlas(gpu_runtime::atlas_runtime::AtlasStorageRuntimeRegisterError),
    Document(document::ImageCreateError),
}

#[derive(Debug)]
pub enum BrushRegisterError {
    Engine(BrushRegistryError),
    Layout(BrushRegistryError),
    Pipeline(BrushRegistryError),
    Executor(WgpuBrushExecutorError),
    CacheBackendAlloc { brush_id: BrushId },
}

#[derive(Debug)]
pub enum PresentError {
    NoSurface,
    Surface(SurfaceError),
}

#[derive(Debug)]
pub enum ScreenshotError {
    InvalidSize,
    Io(std::io::Error),
    Map(wgpu::BufferAsyncError),
    MapChannel(std::sync::mpsc::RecvError),
    Png(png::EncodingError),
}

impl Display for ScreenshotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidSize => write!(f, "invalid screenshot size"),
            Self::Io(error) => write!(f, "screenshot io error: {error}"),
            Self::Map(error) => write!(f, "screenshot map error: {error}"),
            Self::MapChannel(error) => write!(f, "screenshot map channel error: {error}"),
            Self::Png(error) => write!(f, "screenshot png error: {error}"),
        }
    }
}

impl std::error::Error for ScreenshotError {}
