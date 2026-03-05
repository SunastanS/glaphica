use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use app::{AppThreadIntegration, trace::TraceRecorder};
use brushes::builtin_brushes::pixel_rect::PixelRectBrush;
use glaphica_core::{BrushId, CanvasVec2, InputDeviceKind, MappedCursor, RadianVec2};
use gpu_runtime::surface_runtime::SurfaceRuntime;
use images::layout::ImageLayout;
use thread_protocol::InputRingSample;
use winit::{
    application::ApplicationHandler,
    event::{ElementState, MouseButton, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoop},
    keyboard::{Key, NamedKey},
    window::{Window, WindowId},
};

struct App {
    window: Option<Arc<Window>>,
    integration: Option<AppThreadIntegration>,
    stroke_active: bool,
    epoch: glaphica_core::EpochId,
    run_config: RunConfig,
    replay_frames: Vec<app::trace::TraceInputFrame>,
    replay_index: usize,
    replay_finished: bool,
    shutdown_requested: bool,
    output_finalized: bool,
    started_at: Option<Instant>,
    sigint_flag: Arc<AtomicBool>,
}

impl App {
    fn new(run_config: RunConfig, sigint_flag: Arc<AtomicBool>) -> Self {
        Self {
            window: None,
            integration: None,
            stroke_active: false,
            epoch: glaphica_core::EpochId(0),
            run_config,
            replay_frames: Vec::new(),
            replay_index: 0,
            replay_finished: false,
            shutdown_requested: false,
            output_finalized: false,
            started_at: None,
            sigint_flag,
        }
    }

    fn is_replay_mode(&self) -> bool {
        self.run_config.replay_input_path.is_some()
    }

    fn finalize_outputs(&mut self) {
        if self.output_finalized {
            return;
        }
        let Some(integration) = &mut self.integration else {
            return;
        };

        integration.process_main_render();
        integration.present_to_screen();

        if let Some(screenshot_path) = &self.run_config.screenshot_path {
            let (width, height) = match &self.window {
                Some(window) => {
                    let size = window.inner_size();
                    (size.width.max(1), size.height.max(1))
                }
                None => (1024, 1024),
            };
            if let Err(error) = integration.save_screenshot(screenshot_path, width, height) {
                eprintln!("Screenshot save failed: {error}");
            }
        }

        if self.run_config.record_input_path.is_some()
            || self.run_config.record_output_path.is_some()
        {
            if let Err(error) = integration.save_trace_files(
                self.run_config.record_input_path.as_deref(),
                self.run_config.record_output_path.as_deref(),
            ) {
                eprintln!("Trace files save failed: {error}");
            }
        }
        self.output_finalized = true;
    }

    fn request_shutdown(&mut self, event_loop: &ActiveEventLoop) {
        self.shutdown_requested = true;
        self.finalize_outputs();
        event_loop.exit();
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let window = Arc::new(
            event_loop
                .create_window(Window::default_attributes().with_title("glaphica"))
                .expect("failed to create window"),
        );
        self.window = Some(window.clone());
        if self.started_at.is_none() {
            self.started_at = Some(Instant::now());
        }

        if self.integration.is_none() {
            let rt = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
            let mut integration = rt
                .block_on(async {
                    AppThreadIntegration::new(
                        "test-document".to_string(),
                        ImageLayout::new(1024, 1024),
                    )
                    .await
                })
                .expect("failed to init app");

            // registe a fallback brush
            integration
                .register_brush(BrushId(0), PixelRectBrush::new(7))
                .expect("failed to register default brush");
            integration.set_active_brush(BrushId(0));

            if self.run_config.record_input_path.is_some()
                || self.run_config.record_output_path.is_some()
            {
                integration.enable_trace_recording();
            }

            self.integration = Some(integration);
        }

        if let Some(integration) = &mut self.integration {
            let gpu_context = integration.main_state().gpu_context();
            let instance = &gpu_context.instance;
            let adapter = &gpu_context.adapter;
            let device = &gpu_context.device;

            let surface = instance
                .create_surface(window.clone())
                .expect("failed to create surface");

            let size = (window.inner_size().width, window.inner_size().height);

            let surface_runtime = SurfaceRuntime::new(surface, adapter, device, size.0, size.1)
                .expect("failed to init surface");

            integration.set_surface(surface_runtime);
        }

        if let Some(replay_input_path) = &self.run_config.replay_input_path {
            match TraceRecorder::load_input_file(replay_input_path) {
                Ok(input_file) => {
                    self.replay_frames = input_file.frames;
                    self.replay_index = 0;
                    self.replay_finished = false;
                }
                Err(error) => {
                    eprintln!("Replay input file load failed: {error}");
                    event_loop.exit();
                    return;
                }
            }
        }

        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => {
                self.request_shutdown(event_loop);
            }
            WindowEvent::Resized(size) => {
                if let Some(integration) = &mut self.integration {
                    integration.resize_surface(size.width, size.height);
                }
            }
            WindowEvent::RedrawRequested => {
                if let Some(integration) = &mut self.integration {
                    integration.process_main_render();
                    integration.present_to_screen();
                }
            }
            WindowEvent::MouseInput { button, state, .. } => {
                if self.is_replay_mode() {
                    return;
                }
                if button == MouseButton::Left {
                    match state {
                        ElementState::Pressed => {
                            self.stroke_active = true;
                            if let Some(integration) = &mut self.integration {
                                integration.begin_stroke(glaphica_core::NodeId(0));
                            }
                        }
                        ElementState::Released => {
                            self.stroke_active = false;
                            if let Some(integration) = &mut self.integration {
                                integration.end_stroke();
                            }
                        }
                    }
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                if self.is_replay_mode() {
                    return;
                }
                if self.stroke_active {
                    if let Some(integration) = &mut self.integration {
                        let sample = InputRingSample {
                            epoch: self.epoch,
                            time_ns: current_time_ns(),
                            device: InputDeviceKind::Cursor,
                            cursor: MappedCursor {
                                cursor: CanvasVec2::new(position.x as f32, position.y as f32),
                                tilt: RadianVec2::new(0.0, 0.0),
                                pressure: 1.0,
                                twist: 0.0,
                            },
                        };
                        integration.push_input_sample(sample);
                    }
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state == ElementState::Pressed && !event.repeat {
                    match &event.logical_key {
                        Key::Named(NamedKey::Escape) => self.request_shutdown(event_loop),
                        Key::Character(value) if value.eq_ignore_ascii_case("q") => {
                            self.request_shutdown(event_loop)
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.shutdown_requested {
            return;
        }
        if self.sigint_flag.load(Ordering::Relaxed) {
            self.request_shutdown(event_loop);
            return;
        }
        if let (Some(max_duration_ms), Some(started_at)) =
            (self.run_config.exit_after_ms, self.started_at)
        {
            if started_at.elapsed() >= Duration::from_millis(max_duration_ms) {
                self.request_shutdown(event_loop);
                return;
            }
        }

        let replay_mode = self.is_replay_mode();
        if let Some(integration) = &mut self.integration {
            let has_work = if replay_mode && !self.replay_finished {
                if let Some(input_frame) = self.replay_frames.get(self.replay_index) {
                    self.replay_index += 1;
                    integration.process_replay_input_frame(input_frame)
                } else {
                    self.replay_finished = true;
                    false
                }
            } else {
                integration.process_engine_frame(Duration::from_millis(0))
            };
            if has_work {
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }

            if replay_mode && self.replay_finished {
                self.request_shutdown(event_loop);
            }
        }
    }
}

fn main() {
    let event_loop = EventLoop::new().expect("failed to create event loop");
    let run_config = RunConfig::from_args(std::env::args().skip(1).collect());
    let sigint_flag = Arc::new(AtomicBool::new(false));
    {
        let sigint_flag = sigint_flag.clone();
        if let Err(error) = ctrlc::set_handler(move || {
            sigint_flag.store(true, Ordering::Relaxed);
        }) {
            eprintln!("failed to set Ctrl+C handler: {error}");
        }
    }
    let mut app = App::new(run_config, sigint_flag);
    event_loop.run_app(&mut app).expect("failed to run app");
}

fn current_time_ns() -> u64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_nanos() as u64,
        Err(_) => 0,
    }
}

#[derive(Debug, Default)]
struct RunConfig {
    replay_input_path: Option<PathBuf>,
    record_input_path: Option<PathBuf>,
    record_output_path: Option<PathBuf>,
    screenshot_path: Option<PathBuf>,
    exit_after_ms: Option<u64>,
}

impl RunConfig {
    fn from_args(args: Vec<String>) -> Self {
        let mut config = Self::default();
        let mut index = 0usize;
        while index < args.len() {
            match args[index].as_str() {
                "--replay-input" => {
                    if let Some(path) = args.get(index + 1) {
                        config.replay_input_path = Some(Path::new(path).to_path_buf());
                    }
                    index += 2;
                }
                "--record-input" => {
                    if let Some(path) = args.get(index + 1) {
                        config.record_input_path = Some(Path::new(path).to_path_buf());
                    }
                    index += 2;
                }
                "--record-output" => {
                    if let Some(path) = args.get(index + 1) {
                        config.record_output_path = Some(Path::new(path).to_path_buf());
                    }
                    index += 2;
                }
                "--screenshot" => {
                    if let Some(path) = args.get(index + 1) {
                        config.screenshot_path = Some(Path::new(path).to_path_buf());
                    }
                    index += 2;
                }
                "--exit-after-ms" => {
                    if let Some(value) = args.get(index + 1) {
                        if let Ok(ms) = value.parse::<u64>() {
                            config.exit_after_ms = Some(ms);
                        }
                    }
                    index += 2;
                }
                _ => {
                    index += 1;
                }
            }
        }
        config
    }
}
