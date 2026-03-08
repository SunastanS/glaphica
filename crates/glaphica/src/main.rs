use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

mod egui_renderer;

use app::{AppThreadIntegration, trace::TraceRecorder};
use brushes::builtin_brushes::round::RoundBrush;
use brushes::{BrushConfigItem, BrushConfigKind, BrushConfigValue, UnitIntervalPoint};
use egui::{Color32, SidePanel, TopBottomPanel};
use egui_renderer::EguiRenderer;
use glaphica_core::{BrushId, CanvasVec2, InputDeviceKind, MappedCursor, RadianVec2};
use gpu_runtime::surface_runtime::SurfaceRuntime;
use images::layout::ImageLayout;
use thread_protocol::InputRingSample;
use winit::{
    application::ApplicationHandler,
    event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoop},
    keyboard::{Key, NamedKey},
    window::{Window, WindowId},
};

const DEFAULT_BRUSH_ID: BrushId = BrushId(0);

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
    overlay: Option<EguiOverlay>,
    cursor_position: Option<(f32, f32)>,
    middle_pan_active: bool,
    middle_pan_last_position: Option<(f32, f32)>,
    ctrl_pressed: bool,
    brush_config_items: Vec<BrushConfigItem>,
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
            overlay: None,
            cursor_position: None,
            middle_pan_active: false,
            middle_pan_last_position: None,
            ctrl_pressed: false,
            brush_config_items: Vec::new(),
        }
    }

    fn is_replay_mode(&self) -> bool {
        self.run_config.replay_input_path.is_some()
    }

    fn render_frame(&mut self) {
        let Some(integration) = &mut self.integration else {
            return;
        };
        integration.process_main_render();
        if let Some(overlay) = &mut self.overlay {
            let Some(window) = self.window.as_deref() else {
                integration.present_to_screen();
                return;
            };
            integration.present_to_screen_with_overlay(
                |device, queue, encoder, view, format, width, height| {
                    overlay.paint(window, device, queue, encoder, view, format, width, height);
                },
            );
            if let Some(values) = overlay.take_pending_brush_update() {
                match RoundBrush::from_config_values(&values) {
                    Ok(updated_brush) => {
                        if let Err(error) =
                            integration.update_brush(DEFAULT_BRUSH_ID, updated_brush)
                        {
                            eprintln!("failed to update brush: {error:?}");
                        }
                    }
                    Err(error) => {
                        eprintln!("failed to build brush from config: {error}");
                    }
                }
            }
        } else {
            integration.present_to_screen();
        }
    }

    fn finalize_outputs(&mut self) {
        if self.output_finalized {
            return;
        }
        if self.integration.is_none() {
            return;
        }
        self.render_frame();

        if let Some(screenshot_path) = &self.run_config.screenshot_path {
            let (width, height) = match &self.window {
                Some(window) => {
                    let size = window.inner_size();
                    (size.width.max(1), size.height.max(1))
                }
                None => (1024, 1024),
            };
            if let Some(integration) = &mut self.integration
                && let Err(error) = integration.save_screenshot(screenshot_path, width, height)
            {
                eprintln!("Screenshot save failed: {error}");
            }
        }

        if self.run_config.record_input_path.is_some()
            || self.run_config.record_output_path.is_some()
        {
            if let Some(integration) = &self.integration
                && let Err(error) = integration.save_trace_files(
                    self.run_config.record_input_path.as_deref(),
                    self.run_config.record_output_path.as_deref(),
                )
            {
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
        let window = match event_loop.create_window(
            Window::default_attributes().with_title("glaphica"),
        ) {
            Ok(window) => Arc::new(window),
            Err(error) => {
                eprintln!("failed to create window: {error}");
                event_loop.exit();
                return;
            }
        };
        self.window = Some(window.clone());
        if self.started_at.is_none() {
            self.started_at = Some(Instant::now());
        }

        if self.integration.is_none() {
            let rt = match tokio::runtime::Runtime::new() {
                Ok(rt) => rt,
                Err(error) => {
                    eprintln!("failed to create tokio runtime: {error}");
                    event_loop.exit();
                    return;
                }
            };
            let integration = rt
                .block_on(async {
                    AppThreadIntegration::new(
                        "test-document".to_string(),
                        ImageLayout::new(1024, 1024),
                    )
                    .await
                });

            let mut integration = match integration {
                Ok(i) => i,
                Err(error) => {
                    eprintln!("failed to init app: {error:?}");
                    event_loop.exit();
                    return;
                }
            };

            // registe a fallback brush
            let default_brush = match RoundBrush::with_default_curves(3.0, 0.8) {
                Ok(brush) => brush,
                Err(error) => {
                    eprintln!("failed to build default brush: {error}");
                    event_loop.exit();
                    return;
                }
            };
            self.brush_config_items = default_brush.config_items();
            if let Err(error) = integration.register_brush(DEFAULT_BRUSH_ID, default_brush) {
                eprintln!("failed to register default brush: {error:?}");
                event_loop.exit();
                return;
            }
            integration.set_active_brush(DEFAULT_BRUSH_ID);

            if self.run_config.record_input_path.is_some()
                || self.run_config.record_output_path.is_some()
            {
                integration.enable_trace_recording();
            }

            self.integration = Some(integration);
        }

        if let Some(integration) = &mut self.integration {
            let gpu_context = integration.main_state().gpu_context().clone();
            let instance = &gpu_context.instance;
            let adapter = &gpu_context.adapter;
            let device = &gpu_context.device;

            let surface = match instance.create_surface(window.clone()) {
                Ok(surface) => surface,
                Err(error) => {
                    eprintln!("failed to create surface: {error}");
                    event_loop.exit();
                    return;
                }
            };

            let size = (window.inner_size().width, window.inner_size().height);

            let surface_runtime = match SurfaceRuntime::new(surface, adapter, device, size.0, size.1)
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    eprintln!("failed to init surface: {error:?}");
                    event_loop.exit();
                    return;
                }
            };
            let surface_format = surface_runtime.format();

            integration.set_surface(surface_runtime);
            self.overlay = Some(EguiOverlay::new(
                event_loop,
                &window,
                device,
                surface_format,
                self.brush_config_items.clone(),
            ));
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
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let mut ui_event_consumed = false;
        if let (Some(window), Some(overlay)) = (self.window.as_deref(), self.overlay.as_mut())
            && window.id() == window_id
        {
            let response = overlay.on_window_event(window, &event);
            ui_event_consumed = response.consumed;
            if response.repaint {
                window.request_redraw();
            }
        }
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
                self.render_frame();
            }
            WindowEvent::MouseInput { button, state, .. } => {
                if self.is_replay_mode() {
                    return;
                }
                if ui_event_consumed {
                    if button == MouseButton::Left
                        && state == ElementState::Released
                        && self.stroke_active
                    {
                        self.stroke_active = false;
                        if let Some(integration) = &mut self.integration {
                            integration.end_stroke();
                        }
                    }
                    if button == MouseButton::Middle && state == ElementState::Released {
                        self.middle_pan_active = false;
                        self.middle_pan_last_position = None;
                    }
                    return;
                }
                match button {
                    MouseButton::Left => match state {
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
                    },
                    MouseButton::Middle => match state {
                        ElementState::Pressed => {
                            self.middle_pan_active = true;
                            self.middle_pan_last_position = self.cursor_position;
                        }
                        ElementState::Released => {
                            self.middle_pan_active = false;
                            self.middle_pan_last_position = None;
                        }
                    },
                    _ => {}
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                if self.is_replay_mode() {
                    return;
                }
                let current_position = (position.x as f32, position.y as f32);
                self.cursor_position = Some(current_position);
                if ui_event_consumed {
                    return;
                }
                if self.middle_pan_active {
                    if let Some((last_x, last_y)) = self.middle_pan_last_position {
                        let dx = current_position.0 - last_x;
                        let dy = current_position.1 - last_y;
                        if let Some(integration) = &mut self.integration {
                            integration.pan_view(dx, dy);
                        }
                        if let Some(window) = &self.window {
                            window.request_redraw();
                        }
                    }
                    self.middle_pan_last_position = Some(current_position);
                }
                if self.stroke_active {
                    if let Some(integration) = &mut self.integration {
                        let (doc_x, doc_y) = integration
                            .map_screen_to_document(current_position.0, current_position.1);
                        let sample = InputRingSample {
                            epoch: self.epoch,
                            time_ns: current_time_ns(),
                            device: InputDeviceKind::Cursor,
                            cursor: MappedCursor {
                                cursor: CanvasVec2::new(doc_x, doc_y),
                                tilt: RadianVec2::new(0.0, 0.0),
                                pressure: 1.0,
                                twist: 0.0,
                            },
                        };
                        integration.push_input_sample(sample);
                    }
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if self.is_replay_mode() {
                    return;
                }
                if ui_event_consumed {
                    return;
                }
                let scroll = scroll_delta_lines(&delta);
                if scroll.abs() <= f32::EPSILON {
                    return;
                }

                let (center_x, center_y) = match self.cursor_position {
                    Some(position) => position,
                    None => match &self.window {
                        Some(window) => {
                            let size = window.inner_size();
                            (size.width as f32 * 0.5, size.height as f32 * 0.5)
                        }
                        None => return,
                    },
                };

                if let Some(integration) = &mut self.integration {
                    if self.ctrl_pressed {
                        integration.rotate_view(scroll * 0.05, center_x, center_y);
                    } else {
                        integration.zoom_view((scroll * 0.12).exp(), center_x, center_y);
                    }
                }
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                self.ctrl_pressed = modifiers.state().control_key();
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if ui_event_consumed {
                    return;
                }
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

struct EguiOverlay {
    ctx: egui::Context,
    state: egui_winit::State,
    renderer: EguiRenderer,
    left_panel_collapsed: bool,
    right_panel_collapsed: bool,
    brush_config_items: Vec<BrushConfigItem>,
    brush_config_values: Vec<BrushConfigValue>,
    pending_brush_update: Option<Vec<BrushConfigValue>>,
}

impl EguiOverlay {
    fn new(
        event_loop: &ActiveEventLoop,
        window: &Window,
        device: &wgpu::Device,
        surface_format: wgpu::TextureFormat,
        brush_config_items: Vec<BrushConfigItem>,
    ) -> Self {
        let ctx = egui::Context::default();
        let state = egui_winit::State::new(
            ctx.clone(),
            egui::ViewportId::ROOT,
            event_loop,
            Some(window.scale_factor() as f32),
            window.theme(),
            None,
        );
        let renderer = EguiRenderer::new(device, surface_format);
        let brush_config_values = brush_config_items
            .iter()
            .map(|item| item.default_value.clone())
            .collect();
        Self {
            ctx,
            state,
            renderer,
            left_panel_collapsed: false,
            right_panel_collapsed: false,
            brush_config_items,
            brush_config_values,
            pending_brush_update: None,
        }
    }

    fn on_window_event(
        &mut self,
        window: &Window,
        event: &WindowEvent,
    ) -> egui_winit::EventResponse {
        self.state.on_window_event(window, event)
    }

    fn take_pending_brush_update(&mut self) -> Option<Vec<BrushConfigValue>> {
        self.pending_brush_update.take()
    }

    fn paint(
        &mut self,
        window: &Window,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target_view: &wgpu::TextureView,
        _target_format: wgpu::TextureFormat,
        target_width: u32,
        target_height: u32,
    ) {
        let raw_input = self.state.take_egui_input(window);
        let left_panel_collapsed = &mut self.left_panel_collapsed;
        let right_panel_collapsed = &mut self.right_panel_collapsed;
        let brush_config_items = &self.brush_config_items;
        let brush_config_values = &mut self.brush_config_values;
        let pending_brush_update = &mut self.pending_brush_update;
        let full_output = self.ctx.run(raw_input, |ctx| {
            if *left_panel_collapsed {
                SidePanel::left("overlay-left-panel-collapsed")
                    .resizable(false)
                    .exact_width(28.0)
                    .frame(egui::Frame::default().fill(Color32::from_rgb(26, 26, 26)))
                    .show(ctx, |ui| {
                        if ui.button(">").clicked() {
                            *left_panel_collapsed = false;
                        }
                    });
            } else {
                SidePanel::left("overlay-left-panel")
                    .resizable(true)
                    .default_width(220.0)
                    .min_width(180.0)
                    .max_width(360.0)
                    .frame(egui::Frame::default().fill(Color32::from_rgb(26, 26, 26)))
                    .show(ctx, |ui| {
                        ui.horizontal(|ui| {
                            ui.heading("Tools");
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.button("<").clicked() {
                                        *left_panel_collapsed = true;
                                    }
                                },
                            );
                        });
                        ui.separator();
                        ui.label("Brush");
                        ui.label("Eraser");
                        ui.label("Move");
                    });
            }
            if *right_panel_collapsed {
                SidePanel::right("overlay-right-panel-collapsed")
                    .resizable(false)
                    .exact_width(28.0)
                    .frame(egui::Frame::default().fill(Color32::from_rgb(26, 26, 26)))
                    .show(ctx, |ui| {
                        if ui.button("<").clicked() {
                            *right_panel_collapsed = false;
                        }
                    });
            } else {
                SidePanel::right("overlay-right-panel")
                    .resizable(true)
                    .default_width(240.0)
                    .min_width(180.0)
                    .max_width(420.0)
                    .frame(egui::Frame::default().fill(Color32::from_rgb(26, 26, 26)))
                    .show(ctx, |ui| {
                        ui.horizontal(|ui| {
                            if ui.button(">").clicked() {
                                *right_panel_collapsed = true;
                            }
                            ui.heading("Inspector");
                        });
                        ui.separator();
                        ui.label("Brush Config");
                        ui.separator();
                        for (item, value) in brush_config_items
                            .iter()
                            .zip(brush_config_values.iter_mut())
                        {
                            ui.label(item.label);
                            match (&item.kind, value) {
                                (
                                    BrushConfigKind::ScalarF32 { min, max },
                                    BrushConfigValue::ScalarF32(current),
                                ) => {
                                    ui.add(egui::Slider::new(current, *min..=*max));
                                }
                                (
                                    BrushConfigKind::UnitIntervalCurve,
                                    BrushConfigValue::UnitIntervalCurve(points),
                                ) => {
                                    ui.horizontal(|ui| {
                                        if ui.small_button("+ point").clicked() {
                                            let x = points
                                                .last()
                                                .map(|point| (point.x + 0.1).min(1.0))
                                                .unwrap_or(1.0);
                                            let y =
                                                points.last().map(|point| point.y).unwrap_or(1.0);
                                            points.push(UnitIntervalPoint::new(x, y));
                                        }
                                        if points.len() > 2 && ui.small_button("- point").clicked()
                                        {
                                            points.pop();
                                        }
                                    });
                                    for point in points.iter_mut() {
                                        ui.horizontal(|ui| {
                                            ui.label("x");
                                            ui.add(egui::Slider::new(&mut point.x, 0.0..=1.0));
                                            ui.label("y");
                                            ui.add(egui::Slider::new(&mut point.y, 0.0..=1.0));
                                        });
                                    }
                                    points.sort_by(|a, b| a.x.total_cmp(&b.x));
                                }
                                _ => {
                                    ui.colored_label(
                                        Color32::from_rgb(220, 90, 90),
                                        "Config type mismatch",
                                    );
                                }
                            }
                            ui.separator();
                        }
                        ui.horizontal(|ui| {
                            if ui.button("Reset").clicked() {
                                for (item, value) in brush_config_items
                                    .iter()
                                    .zip(brush_config_values.iter_mut())
                                {
                                    *value = item.default_value.clone();
                                }
                            }
                            if ui.button("Apply").clicked() {
                                *pending_brush_update = Some(brush_config_values.clone());
                            }
                        });
                    });
            }
            TopBottomPanel::bottom("overlay-bottom-bar")
                .exact_height(48.0)
                .frame(egui::Frame::default().fill(Color32::from_rgb(20, 20, 20)))
                .show(ctx, |_ui| {});
        });
        self.state
            .handle_platform_output(window, full_output.platform_output);

        if target_width == 0 || target_height == 0 {
            return;
        }
        let clipped_primitives = self
            .ctx
            .tessellate(full_output.shapes, full_output.pixels_per_point);
        self.renderer
            .upload_textures(device, queue, &full_output.textures_delta);
        self.renderer
            .upload_meshes(device, queue, &clipped_primitives);
        self.renderer.render(
            queue,
            encoder,
            target_view,
            [target_width, target_height],
            full_output.pixels_per_point,
        );
    }
}

fn main() {
    let event_loop = EventLoop::new().expect("failed to create event loop: winit backend initialization failed");
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
    event_loop.run_app(&mut app).expect("failed to run app: event loop terminated unexpectedly");
}

fn current_time_ns() -> u64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_nanos() as u64,
        Err(_) => 0,
    }
}

fn scroll_delta_lines(delta: &MouseScrollDelta) -> f32 {
    match delta {
        MouseScrollDelta::LineDelta(_, y) => *y,
        MouseScrollDelta::PixelDelta(position) => position.y as f32 / 40.0,
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
