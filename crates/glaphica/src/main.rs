use std::path::PathBuf;
use std::sync::Arc;

use glaphica::GpuState;
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowAttributes, WindowId};

const ROTATION_RADIANS_PER_PIXEL: f32 = 0.01;
const WHEEL_ZOOM_SPEED: f32 = 0.1;
const PIXELS_PER_SCROLL_LINE: f32 = 120.0;

#[derive(Default)]
struct App {
    window: Option<Arc<Window>>,
    gpu: Option<GpuState>,
    startup_image_path: Option<PathBuf>,
    is_space_pressed: bool,
    is_rotate_pressed: bool,
    is_left_mouse_pressed: bool,
    last_cursor_position: Option<(f64, f64)>,
}

impl App {
    fn window_id(&self) -> Option<WindowId> {
        self.window.as_ref().map(|w| w.id())
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        event_loop.set_control_flow(ControlFlow::Wait);

        if self.window.is_some() {
            return;
        }

        let window = Arc::new(
            event_loop
                .create_window(
                    WindowAttributes::default()
                        .with_title("glaphica")
                        .with_inner_size(PhysicalSize::new(1280u32, 720u32)),
                )
                .expect("create window"),
        );

        let gpu = pollster::block_on(GpuState::new(
            window.clone(),
            self.startup_image_path.clone(),
        ));
        window.request_redraw();

        self.window = Some(window);
        self.gpu = Some(gpu);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        if self.window_id() != Some(window_id) {
            return;
        }

        match event {
            WindowEvent::CloseRequested => {
                event_loop.exit();
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let is_pressed = event.state == ElementState::Pressed;
                match event.physical_key {
                    PhysicalKey::Code(KeyCode::Space) => {
                        self.is_space_pressed = is_pressed;
                    }
                    PhysicalKey::Code(KeyCode::KeyR) => {
                        self.is_rotate_pressed = is_pressed;
                    }
                    _ => {}
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                if button == MouseButton::Left {
                    self.is_left_mouse_pressed = state == ElementState::Pressed;
                    if !self.is_left_mouse_pressed {
                        self.last_cursor_position = None;
                    }
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                if self.is_left_mouse_pressed {
                    if let Some((last_x, last_y)) = self.last_cursor_position {
                        let delta_x = (position.x - last_x) as f32;
                        let delta_y = (position.y - last_y) as f32;

                        if let Some(gpu) = self.gpu.as_mut() {
                            if self.is_space_pressed {
                                gpu.pan_canvas(delta_x, delta_y);
                            } else if self.is_rotate_pressed {
                                gpu.rotate_canvas(delta_x * ROTATION_RADIANS_PER_PIXEL);
                            }
                        }
                    }
                }
                self.last_cursor_position = Some((position.x, position.y));

                if let Some(window) = self.window.as_ref() {
                    window.request_redraw();
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let scroll_lines = match delta {
                    MouseScrollDelta::LineDelta(_, vertical_lines) => vertical_lines,
                    MouseScrollDelta::PixelDelta(physical_position) => {
                        (physical_position.y as f32) / PIXELS_PER_SCROLL_LINE
                    }
                };
                let zoom_factor = (scroll_lines * WHEEL_ZOOM_SPEED).exp();
                let (anchor_x, anchor_y) =
                    if let Some((cursor_x, cursor_y)) = self.last_cursor_position {
                        (cursor_x as f32, cursor_y as f32)
                    } else if let Some(window) = self.window.as_ref() {
                        let size = window.inner_size();
                        (size.width as f32 * 0.5, size.height as f32 * 0.5)
                    } else {
                        (0.0, 0.0)
                    };
                if let Some(gpu) = self.gpu.as_mut() {
                    gpu.zoom_canvas_about_viewport_point(zoom_factor, anchor_x, anchor_y);
                }
                if let Some(window) = self.window.as_ref() {
                    window.request_redraw();
                }
            }
            WindowEvent::Resized(new_size) => {
                if let Some(gpu) = self.gpu.as_mut() {
                    gpu.resize(new_size);
                }
                if let Some(window) = self.window.as_ref() {
                    window.request_redraw();
                }
            }
            WindowEvent::RedrawRequested => {
                let Some(gpu) = self.gpu.as_mut() else {
                    return;
                };

                match gpu.render() {
                    Ok(()) => {}
                    Err(wgpu::SurfaceError::Outdated | wgpu::SurfaceError::Lost) => {
                        if let Some(window) = self.window.as_ref() {
                            gpu.resize(window.inner_size());
                            window.request_redraw();
                        }
                    }
                    Err(wgpu::SurfaceError::Timeout) => {
                        if let Some(window) = self.window.as_ref() {
                            window.request_redraw();
                        }
                    }
                    Err(wgpu::SurfaceError::OutOfMemory) => {
                        event_loop.exit();
                    }
                    Err(_) => {
                        if let Some(window) = self.window.as_ref() {
                            window.request_redraw();
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }
}

fn main() {
    let event_loop = EventLoop::new().expect("create event loop");
    let mut app = App {
        startup_image_path: parse_startup_image_path(),
        ..App::default()
    };
    event_loop.run_app(&mut app).expect("run app");
}

fn parse_startup_image_path() -> Option<PathBuf> {
    let mut args = std::env::args_os();
    let _program = args.next();

    let Some(first_arg) = args.next() else {
        return None;
    };

    if first_arg == "--image" {
        let image_path = args.next().unwrap_or_else(|| {
            panic!("missing image path after --image; usage: glaphica [--image <path>] | [<path>]")
        });
        assert!(
            args.next().is_none(),
            "too many arguments; usage: glaphica [--image <path>] | [<path>]"
        );
        return Some(PathBuf::from(image_path));
    }

    assert!(
        args.next().is_none(),
        "too many arguments; usage: glaphica [--image <path>] | [<path>]"
    );
    Some(PathBuf::from(first_arg))
}
