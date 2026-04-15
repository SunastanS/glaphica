use std::env;
use std::time::Instant;

use atlas::{AtlasLayout, Backend, BackendId, BackendManager};
use brush::round::ROUND_BRUSH_ID;
use gla_doc_renderer::GlaDocRenderer;
use gla_document::{GlaDoc, GlaDocError, GlaImageCreateError, GlaImageLayout};
use gla_image::GlaImageTileAccessError;
use glaphica_core::ATLAS_TILE_SIZE;
use renderer::{GpuContext, GpuContextInitDescriptor, TileRenderer};
use winit::event_loop::ActiveEventLoop;
use winit::window::{Window, WindowAttributes};

use crate::{
    ActiveTool, AppBrushRegistry, AppRuntime, AppView, AppViewMatrixError, ScreenPresentCache,
    Tool, ToolSet,
};

use super::{
    BRUSH_RING_CAPACITY, DEFAULT_DOCUMENT_HEIGHT, DEFAULT_DOCUMENT_WIDTH, INPUT_RING_CAPACITY,
    PreviewInitError, PreviewState, WORKER_BATCH_CAPACITY, WORKER_WAIT_TIMEOUT,
};

const DEFAULT_IMAGE_ATLAS_LAYOUT: AtlasLayout = AtlasLayout::Small11;
const DEFAULT_RENDER_ATLAS_LAYOUT: AtlasLayout = AtlasLayout::Small11;
const DEFAULT_BACKUP_ATLAS_LAYOUT: AtlasLayout = AtlasLayout::Small11;
const DEFAULT_BRUSH_ATLAS_LAYOUT: AtlasLayout = AtlasLayout::Small11;

impl PreviewState {
    pub(super) fn new(event_loop: &ActiveEventLoop) -> Result<Self, PreviewInitError> {
        let window = std::sync::Arc::new(
            event_loop
                .create_window(WindowAttributes::default().with_title("glaphica-dev preview"))?,
        );
        let size = window.inner_size();
        let (gpu, surface) = create_gpu_runtime(window.clone(), size.width, size.height)?;
        let (
            image_backend_id,
            render_backend_id,
            image_backend,
            render_backend,
            backup_backend,
            brush_backend,
            mut tile_renderer,
            screen_cache,
        ) = create_render_resources(&gpu, &surface)?;

        let mut doc = GlaDoc::new(
            GlaImageLayout::new(DEFAULT_DOCUMENT_WIDTH, DEFAULT_DOCUMENT_HEIGHT),
            image_backend_id,
            render_backend_id,
            backup_backend,
        )?;
        let active_layer = doc.append_layer(doc.root_id())?;
        doc.set_active_layer(active_layer)?;
        initialize_default_canvas_white(
            &mut doc,
            &image_backend,
            &mut tile_renderer,
            &gpu.device,
            &gpu.queue,
        )?;

        let session_brushes = AppBrushRegistry::with_builtin_round(brush_backend.clone());
        let worker_brushes = AppBrushRegistry::with_builtin_round(brush_backend);
        let tool_set = ToolSet::new(vec![Tool::Brush(ROUND_BRUSH_ID)]);
        let active_tool = ActiveTool::Brush(ROUND_BRUSH_ID);
        let view = fitted_view(
            DEFAULT_DOCUMENT_WIDTH,
            DEFAULT_DOCUMENT_HEIGHT,
            size.width,
            size.height,
        )?;
        let doc_renderer = GlaDocRenderer::new(render_backend);
        let mut runtime = AppRuntime::spawn(
            doc,
            doc_renderer,
            session_brushes,
            worker_brushes,
            tool_set,
            active_tool,
            view,
            INPUT_RING_CAPACITY,
            BRUSH_RING_CAPACITY,
            WORKER_BATCH_CAPACITY,
            WORKER_WAIT_TIMEOUT,
        )?;
        runtime.prepare_document_gpu(&mut tile_renderer, &gpu.device, &gpu.queue)?;

        let full_tile_count = usize::try_from(runtime.session().doc().layout().total_tiles())
            .map_err(|_| GlaDocError::ImageCreate(GlaImageCreateError::TooManyTiles))?;
        let full_tile_indices = (0..full_tile_count).collect::<Vec<_>>();

        Ok(Self {
            window,
            gpu,
            surface,
            screen_cache,
            runtime: Some(runtime),
            tile_renderer,
            image_backend,
            full_tile_indices,
            started_at: Instant::now(),
            cursor_position: None,
            modifiers: winit::keyboard::ModifiersState::default(),
            stroke_active: false,
            perf_trace: super::app_present_loop::PreviewPerfTraceConfig::from_env(),
            perf_frame_seq: 0,
        })
    }
}

fn create_gpu_runtime(
    window: std::sync::Arc<Window>,
    width: u32,
    height: u32,
) -> Result<(GpuContext, crate::SurfaceRuntime), PreviewInitError> {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
    let surface = instance
        .create_surface(window)
        .map_err(crate::SurfaceError::CreateSurface)?;
    let gpu = pollster::block_on(GpuContext::init_with_instance_and_surface(
        &GpuContextInitDescriptor::default(),
        instance,
        Some(&surface),
    ))?;
    let surface = crate::SurfaceRuntime::new(
        surface,
        &gpu.adapter,
        &gpu.device,
        width.max(1),
        height.max(1),
    )?;
    Ok((gpu, surface))
}

fn create_render_resources(
    gpu: &GpuContext,
    surface: &crate::SurfaceRuntime,
) -> Result<
    (
        BackendId,
        BackendId,
        Backend,
        Backend,
        Backend,
        Backend,
        TileRenderer,
        ScreenPresentCache,
    ),
    PreviewInitError,
> {
    let mut backend_manager = BackendManager::new();
    let image_backend_id = backend_manager.add_backend(DEFAULT_IMAGE_ATLAS_LAYOUT)?;
    let render_backend_id = backend_manager.add_backend(DEFAULT_RENDER_ATLAS_LAYOUT)?;
    let backup_backend_id = backend_manager.add_backend(DEFAULT_BACKUP_ATLAS_LAYOUT)?;
    let brush_backend_id = backend_manager.add_backend(DEFAULT_BRUSH_ATLAS_LAYOUT)?;
    let image_backend = backend_manager
        .backend(image_backend_id)
        .ok_or(atlas::AtlasError::WrongBackend)?
        .clone();
    let render_backend = backend_manager
        .backend(render_backend_id)
        .ok_or(atlas::AtlasError::WrongBackend)?
        .clone();
    let backup_backend = backend_manager
        .backend(backup_backend_id)
        .ok_or(atlas::AtlasError::WrongBackend)?
        .clone();
    let brush_backend = backend_manager
        .backend(brush_backend_id)
        .ok_or(atlas::AtlasError::WrongBackend)?
        .clone();

    let mut tile_renderer = TileRenderer::new(&gpu.device)?;
    let screen_cache = ScreenPresentCache::new(
        &gpu.device,
        surface.format(),
        surface.width(),
        surface.height(),
    )?;
    tile_renderer.ensure_backend(&gpu.device, &image_backend)?;
    tile_renderer.ensure_backend(&gpu.device, &render_backend)?;
    tile_renderer.ensure_backend(&gpu.device, &backup_backend)?;
    tile_renderer.ensure_backend(&gpu.device, &brush_backend)?;

    Ok((
        image_backend_id,
        render_backend_id,
        image_backend,
        render_backend,
        backup_backend,
        brush_backend,
        tile_renderer,
        screen_cache,
    ))
}

fn initialize_default_canvas_white(
    doc: &mut GlaDoc,
    image_backend: &Backend,
    tile_renderer: &mut TileRenderer,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
) -> Result<(), PreviewInitError> {
    tile_renderer.ensure_backend(device, image_backend)?;

    let tile_count = doc.active_layer_image()?.tile_count();
    let white_tile = vec![255; (ATLAS_TILE_SIZE * ATLAS_TILE_SIZE * 4) as usize];
    for tile_index in 0..tile_count {
        let tile_owner = image_backend.alloc_active()?;
        let tile_key = tile_owner.tile_key();
        doc.active_layer_image_mut()?
            .replace_tile_owner(tile_index, tile_owner)
            .map_err(|error| match error {
                GlaImageTileAccessError::OutOfBounds => {
                    PreviewInitError::Document(GlaDocError::InvalidTileIndex {
                        tile_index,
                        tile_count,
                    })
                }
                GlaImageTileAccessError::WrongBackend { .. } => {
                    PreviewInitError::Atlas(atlas::AtlasError::WrongBackend)
                }
            })?;
        tile_renderer.upload_rgba8_tile(device, queue, image_backend, tile_key, &white_tile)?;
    }

    Ok(())
}

pub(super) fn fitted_view(
    doc_width: u32,
    doc_height: u32,
    width: u32,
    height: u32,
) -> Result<AppView, AppViewMatrixError> {
    let doc_width = doc_width as f32;
    let doc_height = doc_height as f32;
    let scale = (width as f32 / doc_width)
        .min(height as f32 / doc_height)
        .max(0.01);
    let translate_x = (width as f32 - doc_width * scale) * 0.5;
    let translate_y = (height as f32 - doc_height * scale) * 0.5;
    AppView::from_scale_rotation_translation(scale, scale, 0.0, translate_x, translate_y)
}

pub(super) fn env_flag(name: &str) -> bool {
    env::var(name)
        .ok()
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(false)
}

pub(super) fn env_millis(name: &str) -> Option<u64> {
    env::var(name).ok()?.parse::<u64>().ok()
}
