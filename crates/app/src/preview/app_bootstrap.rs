use std::env;
use std::time::Instant;

use atlas::{AtlasLayout, Backend, BackendManager};
use brush::BrushRegistry;
use brush::round::{ROUND_BRUSH_ID, ROUND_SHADER_SPEC, RoundBrushSettings};
use gla_doc_renderer::GlaDocRenderer;
use gla_document::{GlaDoc, GlaDocError, GlaImageCreateError, GlaImageLayout};
use gla_image::GlaImageTileAccessError;
use glaphica_core::ATLAS_TILE_SIZE;
use renderer::{EguiRenderer, GpuContext, GpuContextInitDescriptor, TileRenderer};
use ui::AppUi;
use winit::event_loop::ActiveEventLoop;
use winit::window::{Window, WindowAttributes};

use crate::{
    ActiveTool, AppRuntime, AppView, AppViewMatrixError, ScreenPresentTile, Tool, ToolSet,
};

use super::{
    BRUSH_RING_CAPACITY, DEFAULT_DOCUMENT_HEIGHT, DEFAULT_DOCUMENT_WIDTH, INPUT_RING_CAPACITY,
    PreviewInitError, PreviewState, PreviewTraceConfig, WORKER_BATCH_CAPACITY, WORKER_WAIT_TIMEOUT,
};

const DEFAULT_IMAGE_ATLAS_LAYOUT: AtlasLayout = AtlasLayout::Small11;
const DEFAULT_RENDER_ATLAS_LAYOUT: AtlasLayout = AtlasLayout::Small11;
const DEFAULT_BACKUP_ATLAS_LAYOUT: AtlasLayout = AtlasLayout::Small11;
const DEFAULT_BRUSH_ATLAS_LAYOUT: AtlasLayout = AtlasLayout::Small11;

impl PreviewState {
    pub(super) fn new(
        event_loop: &ActiveEventLoop,
        trace_config: &PreviewTraceConfig,
    ) -> Result<Self, PreviewInitError> {
        let perf_trace = super::app_present_loop::PreviewPerfTraceConfig::from_env();
        puffin::set_scopes_on(perf_trace.puffin_enabled());
        let puffin_server = start_puffin_server(perf_trace)?;

        let window = std::sync::Arc::new(
            event_loop
                .create_window(WindowAttributes::default().with_title("glaphica-dev preview"))?,
        );
        let size = window.inner_size();
        let (gpu, surface) = create_gpu_runtime(window.clone(), size.width, size.height)?;
        let (
            image_backend,
            render_backend,
            backup_backend,
            brush_backend,
            mut tile_renderer,
            screen_present,
        ) = create_render_resources(&gpu, &surface)?;

        let mut doc = GlaDoc::new(
            GlaImageLayout::new(DEFAULT_DOCUMENT_WIDTH, DEFAULT_DOCUMENT_HEIGHT),
            image_backend.clone(),
            render_backend.clone(),
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

        let round_brush_settings = RoundBrushSettings::default()
            .with_base_radius_px(20.0)
            .with_base_hardness(0.3);
        let session_brushes = BrushRegistry::with_builtin_round_settings(
            brush_backend.clone(),
            round_brush_settings.clone(),
        );
        let worker_brushes =
            BrushRegistry::with_builtin_round_settings(brush_backend, round_brush_settings.clone());
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

        let full_slot_count = usize::try_from(runtime.session().doc().layout().total_slots())
            .map_err(|_| GlaDocError::ImageCreate(GlaImageCreateError::TooManyTiles))?;
        let full_tile_indices = (0..full_slot_count).collect::<Vec<_>>();
        let ui = AppUi::new(event_loop, &window, round_brush_settings);
        let ui_renderer = EguiRenderer::new(&gpu.device, surface.format());
        let mut trace = super::trace::PreviewTraceState::default();
        if let Some(path) = trace_config.record_input_path() {
            trace.start_recording(path);
        }
        if let Some(path) = trace_config.replay_input_path() {
            trace
                .load_replay(path)
                .map_err(|error| PreviewInitError::Trace(error.to_string()))?;
        }

        Ok(Self {
            window,
            gpu,
            surface,
            screen_present,
            runtime: Some(runtime),
            tile_renderer,
            ui_renderer,
            ui,
            full_tile_indices,
            started_at: Instant::now(),
            cursor_position: None,
            middle_pan_active: false,
            middle_pan_last_position: None,
            modifiers: winit::keyboard::ModifiersState::default(),
            stroke_active: false,
            trace,
            trace_default_path: trace_config.default_trace_path().to_path_buf(),
            perf_trace,
            perf_frame_seq: 0,
            _puffin_server: puffin_server,
        })
    }
}

fn start_puffin_server(
    perf_trace: super::app_present_loop::PreviewPerfTraceConfig,
) -> Result<Option<puffin_http::Server>, PreviewInitError> {
    if !perf_trace.http_enabled() {
        return Ok(None);
    }

    let bind_addr = env::var("GLAPHICA_PREVIEW_PERF_TRACE_HTTP_ADDR")
        .unwrap_or_else(|_| format!("127.0.0.1:{}", puffin_http::DEFAULT_PORT));
    let server = puffin_http::Server::new(&bind_addr)
        .map_err(|error| PreviewInitError::PuffinHttpServer(error.to_string()))?;
    eprintln!(
        "preview puffin profiler serving raw TCP on {}. this is not a browser HTTP endpoint; run `puffin_viewer` to inspect traces.",
        bind_addr
    );
    Ok(Some(server))
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
        Backend,
        Backend,
        Backend,
        Backend,
        TileRenderer,
        ScreenPresentTile,
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
    let screen_present = ScreenPresentTile::new(
        &gpu.device,
        surface.format(),
        surface.width(),
        surface.height(),
    )?;
    tile_renderer.ensure_backend(&gpu.device, &image_backend)?;
    tile_renderer.ensure_backend(&gpu.device, &render_backend)?;
    tile_renderer.ensure_backend(&gpu.device, &backup_backend)?;
    tile_renderer.ensure_backend_with_format(
        &gpu.device,
        &brush_backend,
        ROUND_SHADER_SPEC.brush_tile_format,
    )?;

    Ok((
        image_backend,
        render_backend,
        backup_backend,
        brush_backend,
        tile_renderer,
        screen_present,
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

    let slot_count = doc.active_layer_image()?.slot_count();
    let white_tile = vec![255; (ATLAS_TILE_SIZE * ATLAS_TILE_SIZE * 4) as usize];
    for tile_index in 0..slot_count {
        let tile_owner = image_backend.alloc_active()?;
        let tile_key = tile_owner.tile_key();
        doc.active_layer_image_mut()?
            .replace_tile_owner(tile_index, tile_owner)
            .map_err(|error| match error {
                GlaImageTileAccessError::OutOfBounds => {
                    PreviewInitError::Document(GlaDocError::InvalidSlotIndex {
                        slot_index: tile_index,
                        slot_count,
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
