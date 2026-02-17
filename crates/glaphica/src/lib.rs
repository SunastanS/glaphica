use std::sync::Arc;

use document::Document;
use render_protocol::{BlendMode, ImageHandle, RenderOp, RenderStepSupportMatrix, Viewport};
use renderer::{PresentError, RenderDataResolver, Renderer, ViewOpSender};
use tiles::{TileAddress, TileAtlasStore, TileKey};
use view::ViewTransform;
use winit::dpi::PhysicalSize;
use winit::window::Window;

struct DocumentRenderDataResolver {
    document: Arc<Document>,
    atlas_store: Arc<TileAtlasStore>,
}

impl RenderDataResolver for DocumentRenderDataResolver {
    fn document_size(&self) -> (u32, u32) {
        (self.document.size_x(), self.document.size_y())
    }

    fn visit_image_tiles(
        &self,
        image_handle: ImageHandle,
        visitor: &mut dyn FnMut(u32, u32, TileKey),
    ) {
        let Some(image) = self.document.image(image_handle) else {
            return;
        };

        for (tile_x, tile_y, tile_key) in image.iter_tiles() {
            visitor(tile_x, tile_y, *tile_key);
        }
    }

    fn visit_image_tiles_for_coords(
        &self,
        image_handle: ImageHandle,
        tile_coords: &[(u32, u32)],
        visitor: &mut dyn FnMut(u32, u32, TileKey),
    ) {
        let Some(image) = self.document.image(image_handle) else {
            return;
        };

        for (tile_x, tile_y) in tile_coords {
            let tile_key = image
                .get_tile(*tile_x, *tile_y)
                .unwrap_or_else(|error| panic!("tile coordinate lookup failed: {error:?}"));
            let Some(tile_key) = tile_key else {
                continue;
            };
            visitor(*tile_x, *tile_y, *tile_key);
        }
    }

    fn resolve_tile_address(&self, tile_key: TileKey) -> Option<TileAddress> {
        self.atlas_store.resolve(tile_key)
    }
}

pub struct GpuState {
    renderer: Renderer,
    view_sender: ViewOpSender,
    view_transform: ViewTransform,
    surface_size: PhysicalSize<u32>,
    next_frame_id: u64,
}

impl GpuState {
    pub async fn new(window: Arc<Window>) -> Self {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..Default::default()
        });

        let surface = instance
            .create_surface(window.clone())
            .expect("create wgpu surface");

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .expect("request wgpu adapter");

        let limits = adapter.limits();
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: None,
                required_features: wgpu::Features::empty(),
                required_limits: limits,
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
                memory_hints: wgpu::MemoryHints::Performance,
                trace: wgpu::Trace::Off,
            })
            .await
            .expect("request wgpu device");

        let caps = surface.get_capabilities(&adapter);
        let surface_format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(caps.formats[0]);

        let mut size = window.inner_size();
        size.width = size.width.max(1);
        size.height = size.height.max(1);

        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: surface_format,
            width: size.width,
            height: size.height,
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };

        let (atlas_store, tile_atlas) = TileAtlasStore::new(
            &device,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::COPY_SRC,
        )
        .expect("create tile atlas store");
        let atlas_store = Arc::new(atlas_store);

        let (image_width, image_height, image_bytes) = demo_image_rgba8();
        let virtual_image = atlas_store
            .ingest_image_rgba8_strided(image_width, image_height, &image_bytes, image_width * 4)
            .expect("ingest demo image");

        let mut document = Document::new(image_width, image_height);
        let _ = document.new_layer_root_with_image(virtual_image, BlendMode::Normal);
        let initial_snapshot = document.render_step_snapshot(0);
        initial_snapshot
            .validate_executable(&RenderStepSupportMatrix::current_executable_semantics())
            .unwrap_or_else(|error| {
                panic!(
                    "initial render steps include unsupported feature at step {}: {:?}",
                    error.step_index, error.reason
                )
            });
        let document = Arc::new(document);

        let render_data_resolver = Box::new(DocumentRenderDataResolver {
            document,
            atlas_store,
        });

        let (renderer, view_sender) = Renderer::new(
            device,
            queue,
            surface,
            config,
            tile_atlas,
            render_data_resolver,
        );

        let view_transform = ViewTransform::default();
        push_view_state(&view_sender, &view_transform, size);
        view_sender
            .send(RenderOp::BindRenderSteps(initial_snapshot))
            .expect("send initial render steps");

        Self {
            renderer,
            view_sender,
            view_transform,
            surface_size: size,
            next_frame_id: 0,
        }
    }

    pub fn resize(&mut self, new_size: PhysicalSize<u32>) {
        let width = new_size.width.max(1);
        let height = new_size.height.max(1);
        if self.surface_size.width == width && self.surface_size.height == height {
            return;
        }

        self.surface_size = PhysicalSize::new(width, height);
        self.renderer.resize(width, height);
        push_view_state(&self.view_sender, &self.view_transform, self.surface_size);
    }

    pub fn render(&mut self) -> Result<(), wgpu::SurfaceError> {
        self.renderer.drain_view_ops();

        let frame_id = self.next_frame_id;
        self.next_frame_id = self
            .next_frame_id
            .checked_add(1)
            .expect("frame id overflow");

        match self.renderer.present_frame(frame_id) {
            Ok(()) => Ok(()),
            Err(PresentError::Surface(error)) => Err(error),
            Err(PresentError::TileDrain(error)) => {
                panic!("tile atlas drain failed during present: {error}")
            }
        }
    }

    pub fn pan_canvas(&mut self, delta_x: f32, delta_y: f32) {
        self.view_transform
            .pan_by(delta_x, delta_y)
            .unwrap_or_else(|error| panic!("pan canvas failed: {error:?}"));
        push_view_state(&self.view_sender, &self.view_transform, self.surface_size);
    }

    pub fn rotate_canvas(&mut self, delta_radians: f32) {
        self.view_transform
            .rotate_by(delta_radians)
            .unwrap_or_else(|error| panic!("rotate canvas failed: {error:?}"));
        push_view_state(&self.view_sender, &self.view_transform, self.surface_size);
    }

    pub fn zoom_canvas_about_viewport_point(
        &mut self,
        zoom_factor: f32,
        viewport_x: f32,
        viewport_y: f32,
    ) {
        self.view_transform
            .zoom_about_point(zoom_factor, viewport_x, viewport_y)
            .unwrap_or_else(|error| panic!("zoom canvas failed: {error:?}"));
        push_view_state(&self.view_sender, &self.view_transform, self.surface_size);
    }
}

fn push_view_state(
    view_sender: &ViewOpSender,
    view_transform: &ViewTransform,
    size: PhysicalSize<u32>,
) {
    view_sender
        .send(RenderOp::SetViewport(Viewport {
            origin_x: 0,
            origin_y: 0,
            width: size.width,
            height: size.height,
        }))
        .expect("send viewport");

    let matrix = view_transform
        .to_clip_matrix4x4(size.width as f32, size.height as f32)
        .expect("build clip matrix");
    view_sender
        .send(RenderOp::SetViewTransform { matrix })
        .expect("send view transform");
}

fn demo_image_rgba8() -> (u32, u32, Vec<u8>) {
    let width = 512u32;
    let height = 512u32;
    let mut bytes = vec![0u8; (width as usize) * (height as usize) * 4];

    for y in 0..height {
        for x in 0..width {
            let index = ((y as usize) * (width as usize) + (x as usize)) * 4;
            let checker = ((x / 32) + (y / 32)) % 2;
            let r = ((x * 255) / width) as u8;
            let g = ((y * 255) / height) as u8;
            let b = if checker == 0 { 220 } else { 40 };

            bytes[index] = r;
            bytes[index + 1] = g;
            bytes[index + 2] = b;
            bytes[index + 3] = 255;
        }
    }

    (width, height, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_device_queue() -> (wgpu::Device, wgpu::Queue) {
        pollster::block_on(async {
            let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
                backends: wgpu::Backends::all(),
                ..Default::default()
            });
            let adapter = instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::LowPower,
                    compatible_surface: None,
                    force_fallback_adapter: true,
                })
                .await
                .expect("request test adapter");
            adapter
                .request_device(&wgpu::DeviceDescriptor {
                    label: Some("glaphica tests"),
                    required_features: wgpu::Features::empty(),
                    required_limits: adapter.limits(),
                    experimental_features: wgpu::ExperimentalFeatures::disabled(),
                    memory_hints: wgpu::MemoryHints::Performance,
                    trace: wgpu::Trace::Off,
                })
                .await
                .expect("request test device")
        })
    }

    fn read_tile_rgba8(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        texture: &wgpu::Texture,
        address: TileAddress,
    ) -> Vec<u8> {
        let buffer_size = (tiles::TILE_SIZE as u64) * (tiles::TILE_SIZE as u64) * 4;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("glaphica.tests.readback"),
            size: buffer_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("glaphica.tests.readback"),
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: address.tile_x() * tiles::TILE_SIZE,
                    y: address.tile_y() * tiles::TILE_SIZE,
                    z: address.atlas_layer,
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(tiles::TILE_SIZE * 4),
                    rows_per_image: Some(tiles::TILE_SIZE),
                },
            },
            wgpu::Extent3d {
                width: tiles::TILE_SIZE,
                height: tiles::TILE_SIZE,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(Some(encoder.finish()));

        let slice = buffer.slice(..);
        let (sender, receiver) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            sender.send(result).expect("map callback send");
        });
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("device poll");
        receiver
            .recv()
            .expect("map callback recv")
            .expect("map tile readback");
        let tile = slice.get_mapped_range().to_vec();
        buffer.unmap();
        tile
    }

    #[test]
    fn image_from_tests_resources_round_trips_through_document_and_gpu_atlas() {
        let image_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/resources/document_import_e2e.png");
        let decoded = image::ImageReader::open(&image_path)
            .expect("open e2e source image")
            .decode()
            .expect("decode e2e source image")
            .to_rgba8();
        let size_x = decoded.width();
        let size_y = decoded.height();
        let source_bytes = decoded.into_raw();

        let (device, queue) = create_device_queue();
        let (atlas_store, atlas_gpu) = tiles::TileAtlasStore::new(
            &device,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::COPY_SRC,
        )
        .expect("create tile atlas store");

        let virtual_image = atlas_store
            .ingest_image_rgba8_strided(size_x, size_y, &source_bytes, size_x * 4)
            .expect("ingest source image into tile atlas");
        atlas_gpu
            .drain_and_execute(&queue)
            .expect("flush tile uploads to gpu atlas");

        let mut document = Document::new(size_x, size_y);
        let _layer_id = document.new_layer_root_with_image(virtual_image, BlendMode::Normal);
        let snapshot = document.render_step_snapshot(1);
        let image_handle = snapshot
            .steps
            .iter()
            .find_map(|step| match step {
                render_protocol::RenderStepEntry::Leaf { image_handle, .. } => Some(*image_handle),
                render_protocol::RenderStepEntry::Group { .. } => None,
            })
            .expect("snapshot should contain a leaf image");
        let document_image = document
            .image(image_handle)
            .expect("snapshot leaf image handle should resolve");

        let rendered_bytes = document_image
            .export_rgba8(|tile_key| {
                let address = atlas_store.resolve(*tile_key)?;
                Some(read_tile_rgba8(
                    &device,
                    &queue,
                    atlas_gpu.texture(),
                    address,
                ))
            })
            .expect("export rendered image from document tiles");

        assert_eq!(rendered_bytes, source_bytes);
    }
}
