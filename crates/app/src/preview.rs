use std::error::Error;
use std::fmt::{Display, Formatter};
use std::sync::Arc;

use atlas::{AtlasLayout, Backend, BackendManager};
use gla_doc_renderer::{GlaDocRenderer, GlaDocRendererError};
use gla_document::{GlaDoc, GlaDocError, GlaImage, GlaImageCreateError, GlaImageLayout};
use gla_image::GlaImageTileAccessError;
use glaphica_core::{ATLAS_TILE_SIZE, BlendMode, IMAGE_TILE_SIZE};
use renderer::{
    GpuContext, GpuContextInitDescriptor, RenderTarget2d, TileRenderer, TileRendererError,
};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::window::{Window, WindowAttributes, WindowId};

use crate::display::{SurfaceError, SurfaceRuntime};
use crate::{AppPresentError, AppView, AppViewMatrixError, present_root_tiles};

#[derive(Debug)]
pub enum AppPreviewError {
    EventLoop(winit::error::EventLoopError),
}

impl Display for AppPreviewError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EventLoop(error) => Display::fmt(error, f),
        }
    }
}

impl Error for AppPreviewError {}

pub fn run_preview_window() -> Result<(), AppPreviewError> {
    let event_loop = EventLoop::new().map_err(AppPreviewError::EventLoop)?;
    let mut app = PreviewApp::default();
    event_loop
        .run_app(&mut app)
        .map_err(AppPreviewError::EventLoop)
}

#[derive(Debug)]
enum PreviewInitError {
    CreateWindow(winit::error::OsError),
    CreateSurface(SurfaceError),
    Gpu(renderer::GpuContextInitError),
    Atlas(atlas::AtlasError),
    AtlasManager(atlas::AtlasManagerError),
    Document(GlaDocError),
    ImageTileAccess(GlaImageTileAccessError),
    DocRenderer(GlaDocRendererError),
    TileRenderer(TileRendererError),
    View(AppViewMatrixError),
}

impl Display for PreviewInitError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CreateWindow(error) => Display::fmt(error, f),
            Self::CreateSurface(error) => Display::fmt(error, f),
            Self::Gpu(error) => Display::fmt(error, f),
            Self::Atlas(error) => Display::fmt(error, f),
            Self::AtlasManager(error) => write!(f, "atlas backend manager failed: {error:?}"),
            Self::Document(error) => Display::fmt(error, f),
            Self::ImageTileAccess(error) => Display::fmt(error, f),
            Self::DocRenderer(error) => Display::fmt(error, f),
            Self::TileRenderer(error) => Display::fmt(error, f),
            Self::View(error) => Display::fmt(error, f),
        }
    }
}

impl Error for PreviewInitError {}

impl From<SurfaceError> for PreviewInitError {
    fn from(error: SurfaceError) -> Self {
        Self::CreateSurface(error)
    }
}

impl From<renderer::GpuContextInitError> for PreviewInitError {
    fn from(error: renderer::GpuContextInitError) -> Self {
        Self::Gpu(error)
    }
}

impl From<atlas::AtlasError> for PreviewInitError {
    fn from(error: atlas::AtlasError) -> Self {
        Self::Atlas(error)
    }
}

impl From<atlas::AtlasManagerError> for PreviewInitError {
    fn from(error: atlas::AtlasManagerError) -> Self {
        Self::AtlasManager(error)
    }
}

impl From<GlaDocError> for PreviewInitError {
    fn from(error: GlaDocError) -> Self {
        Self::Document(error)
    }
}

impl From<GlaDocRendererError> for PreviewInitError {
    fn from(error: GlaDocRendererError) -> Self {
        Self::DocRenderer(error)
    }
}

impl From<TileRendererError> for PreviewInitError {
    fn from(error: TileRendererError) -> Self {
        Self::TileRenderer(error)
    }
}

impl From<AppViewMatrixError> for PreviewInitError {
    fn from(error: AppViewMatrixError) -> Self {
        Self::View(error)
    }
}

impl From<GlaImageTileAccessError> for PreviewInitError {
    fn from(error: GlaImageTileAccessError) -> Self {
        Self::ImageTileAccess(error)
    }
}

impl From<winit::error::OsError> for PreviewInitError {
    fn from(error: winit::error::OsError) -> Self {
        Self::CreateWindow(error)
    }
}

#[derive(Default)]
struct PreviewApp {
    state: Option<PreviewState>,
}

struct PreviewState {
    window: Arc<Window>,
    gpu: GpuContext,
    surface: SurfaceRuntime,
    doc: GlaDoc,
    doc_renderer: GlaDocRenderer,
    tile_renderer: TileRenderer,
    view: AppView,
    full_tile_indices: Vec<usize>,
}

impl ApplicationHandler for PreviewApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_some() {
            return;
        }

        match PreviewState::new(event_loop) {
            Ok(state) => {
                state.window.request_redraw();
                self.state = Some(state);
            }
            Err(error) => {
                eprintln!("preview init failed: {error}");
                event_loop.exit();
            }
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        if state.window.id() != window_id {
            return;
        }

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                state
                    .surface
                    .resize(&state.gpu.device, size.width.max(1), size.height.max(1));
                if let Err(error) = state.update_view(state.surface.width(), state.surface.height())
                {
                    eprintln!("preview resize failed: {error}");
                    event_loop.exit();
                    return;
                }
                state.window.request_redraw();
            }
            WindowEvent::RedrawRequested => {
                if let Err(error) = state.redraw() {
                    eprintln!("preview redraw failed: {error}");
                    event_loop.exit();
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(state) = &self.state {
            state.window.request_redraw();
        }
    }
}

impl PreviewState {
    fn new(event_loop: &ActiveEventLoop) -> Result<Self, PreviewInitError> {
        let window = Arc::new(
            event_loop
                .create_window(WindowAttributes::default().with_title("glaphica-dev preview"))?,
        );
        let size = window.inner_size();
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        let surface = instance
            .create_surface(window.clone())
            .map_err(SurfaceError::CreateSurface)?;
        let gpu = pollster::block_on(GpuContext::init_with_instance_and_surface(
            &GpuContextInitDescriptor::default(),
            instance,
            Some(&surface),
        ))?;
        let surface = SurfaceRuntime::new(
            surface,
            &gpu.adapter,
            &gpu.device,
            size.width.max(1),
            size.height.max(1),
        )?;

        let mut backend_manager = BackendManager::new();
        let image_backend_id = backend_manager.add_backend(AtlasLayout::Tiny8)?;
        let render_backend_id = backend_manager.add_backend(AtlasLayout::Tiny8)?;
        let backup_backend_id = backend_manager.add_backend(AtlasLayout::Tiny8)?;
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

        let mut tile_renderer = TileRenderer::new(&gpu.device)?;
        tile_renderer.ensure_backend(&gpu.device, &image_backend)?;
        tile_renderer.ensure_backend(&gpu.device, &render_backend)?;
        tile_renderer.ensure_backend(&gpu.device, &backup_backend)?;

        let mut doc = GlaDoc::new(
            GlaImageLayout::new(IMAGE_TILE_SIZE * 6, IMAGE_TILE_SIZE * 4),
            image_backend_id,
            render_backend_id,
            backup_backend,
        )?;
        let back_layer = doc.append_layer(doc.root_id())?;
        let front_layer = doc.append_layer(doc.root_id())?;
        doc.set_opacity(front_layer, 0.65)?;
        doc.set_blend_mode(front_layer, BlendMode::Multiply)?;
        doc.set_active_layer(front_layer)?;

        populate_demo_image(
            doc.node_image_mut(back_layer)?,
            &image_backend,
            &mut tile_renderer,
            &gpu.device,
            &gpu.queue,
            [0x40, 0x92, 0xD9, 0xFF],
            0,
        )?;
        populate_demo_image(
            doc.node_image_mut(front_layer)?,
            &image_backend,
            &mut tile_renderer,
            &gpu.device,
            &gpu.queue,
            [0xF2, 0x6D, 0x3D, 0xFF],
            1,
        )?;

        let mut doc_renderer = GlaDocRenderer::new(render_backend.clone());
        doc_renderer.prepare_active_plan_gpu(&doc, &gpu.device, &gpu.queue, &mut tile_renderer)?;

        let full_tile_indices = (0..usize::try_from(doc.layout().total_tiles())
            .map_err(|_| GlaDocError::ImageCreate(GlaImageCreateError::TooManyTiles))?)
            .collect::<Vec<_>>();
        doc_renderer.render_active_tiles_gpu(
            &doc,
            &gpu.device,
            &gpu.queue,
            &mut tile_renderer,
            &full_tile_indices,
        )?;

        let mut state = Self {
            window,
            gpu,
            surface,
            doc,
            doc_renderer,
            tile_renderer,
            view: AppView::identity(),
            full_tile_indices,
        };
        state.update_view(state.surface.width(), state.surface.height())?;
        Ok(state)
    }

    fn update_view(&mut self, width: u32, height: u32) -> Result<(), PreviewInitError> {
        let doc_width = self.doc.layout().size_x() as f32;
        let doc_height = self.doc.layout().size_y() as f32;
        let scale = (width as f32 / doc_width)
            .min(height as f32 / doc_height)
            .max(0.01);
        let translate_x = (width as f32 - doc_width * scale) * 0.5;
        let translate_y = (height as f32 - doc_height * scale) * 0.5;
        self.view =
            AppView::from_scale_rotation_translation(scale, scale, 0.0, translate_x, translate_y)?;
        Ok(())
    }

    fn redraw(&mut self) -> Result<(), AppPresentError> {
        let frame = self.surface.acquire_frame().map_err(|error| {
            AppPresentError::DocRenderer(GlaDocRendererError::RenderExecution(
                gla_doc_renderer::RenderExecutionError::new(error.to_string()),
            ))
        })?;
        let result = {
            let target = RenderTarget2d {
                view: &frame.view,
                format: self.surface.format(),
                width: self.surface.width(),
                height: self.surface.height(),
            };
            self.tile_renderer.clear_render_target(
                &self.gpu.device,
                &self.gpu.queue,
                target,
                wgpu::Color {
                    r: 0.96,
                    g: 0.96,
                    b: 0.94,
                    a: 1.0,
                },
            );
            present_root_tiles(
                &self.doc,
                &self.doc_renderer,
                &mut self.tile_renderer,
                &self.gpu.device,
                &self.gpu.queue,
                &self.view,
                target,
                &self.full_tile_indices,
            )
        };

        match result {
            Ok(()) => {
                SurfaceRuntime::present(frame);
                Ok(())
            }
            Err(error) => {
                drop(frame);
                Err(error)
            }
        }
    }
}

fn populate_demo_image(
    image: &mut GlaImage,
    image_backend: &Backend,
    tile_renderer: &mut TileRenderer,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    base_rgba: [u8; 4],
    variant: u8,
) -> Result<(), PreviewInitError> {
    for tile_index in 0..image.tile_count() {
        let tile_owner = image_backend.alloc_active()?;
        let tile_key = tile_owner.tile_key();
        image.replace_tile_owner(tile_index, tile_owner)?;
        tile_renderer.upload_rgba8_tile(
            device,
            queue,
            image_backend,
            tile_key,
            &build_demo_tile_pixels(base_rgba, tile_index, variant),
        )?;
    }
    Ok(())
}

fn build_demo_tile_pixels(base_rgba: [u8; 4], tile_index: usize, variant: u8) -> Vec<u8> {
    let mut pixels = vec![0u8; (ATLAS_TILE_SIZE * ATLAS_TILE_SIZE * 4) as usize];
    for y in 0..ATLAS_TILE_SIZE {
        for x in 0..ATLAS_TILE_SIZE {
            let idx = ((y * ATLAS_TILE_SIZE + x) * 4) as usize;
            let checker = (((x / 8) + (y / 8) + tile_index as u32) & 1) as u8;
            let stripe = (((x + y + tile_index as u32 * 7) / 10) & 1) as u8;
            pixels[idx] = base_rgba[0]
                .saturating_sub(20 * checker)
                .saturating_add(10 * stripe);
            pixels[idx + 1] = base_rgba[1]
                .saturating_sub(18 * stripe)
                .saturating_add(8 * checker);
            pixels[idx + 2] = base_rgba[2]
                .saturating_sub(16 * checker)
                .saturating_add(12 * variant);
            pixels[idx + 3] = base_rgba[3];
        }
    }
    pixels
}
