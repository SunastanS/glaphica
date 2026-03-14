use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

mod egui_renderer;

use app::{AppThreadIntegration, trace::TraceRecorder};
use brushes::builtin_brushes::{pixel_rect::PixelRectBrush, round::RoundBrush};
use brushes::{
    BrushConfigItem, BrushConfigKind, BrushConfigValue, UnitIntervalPoint,
    eval_unit_interval_curve_polynomial,
};
use egui::{Color32, Pos2, Rect, Sense, Shape, SidePanel, Stroke, TopBottomPanel, vec2};
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

const ROUND_BRUSH_ID: BrushId = BrushId(0);
const PIXEL_RECT_BRUSH_ID: BrushId = BrushId(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BrushKind {
    Round,
    PixelRect,
}

impl BrushKind {
    const ALL: [Self; 2] = [Self::Round, Self::PixelRect];

    const fn brush_id(self) -> BrushId {
        match self {
            Self::Round => ROUND_BRUSH_ID,
            Self::PixelRect => PIXEL_RECT_BRUSH_ID,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Round => "Round",
            Self::PixelRect => "PixelRect",
        }
    }
}

#[derive(Debug, Clone)]
struct BrushUiState {
    kind: BrushKind,
    color_rgb: [f32; 3],
    eraser: bool,
    items: Vec<BrushConfigItem>,
    values: Vec<BrushConfigValue>,
    visible: Vec<bool>,
    dirty: bool,
}

impl BrushUiState {
    fn new(kind: BrushKind, items: Vec<BrushConfigItem>) -> Self {
        let values = items
            .iter()
            .map(|item| item.default_value.clone())
            .collect::<Vec<_>>();
        let visible = items.iter().map(|item| !item.default_hidden).collect();
        Self {
            kind,
            color_rgb: [1.0, 0.0, 0.0],
            eraser: false,
            items,
            values,
            visible,
            dirty: false,
        }
    }

    fn reset(&mut self) {
        for (item, value) in self.items.iter().zip(self.values.iter_mut()) {
            *value = item.default_value.clone();
        }
        for (item, visible) in self.items.iter().zip(self.visible.iter_mut()) {
            *visible = !item.default_hidden;
        }
        self.dirty = true;
    }
}

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
    active_brush_kind: BrushKind,
    brush_states: Vec<BrushUiState>,
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
            active_brush_kind: BrushKind::Round,
            brush_states: Vec::new(),
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
            let selected_brush_kind = overlay.selected_brush_kind();
            if self.active_brush_kind != selected_brush_kind {
                self.active_brush_kind = selected_brush_kind;
                integration.set_active_brush(self.active_brush_kind.brush_id());
            }
            integration.set_active_brush_color_rgb(overlay.selected_brush_color_rgb());
            integration.set_active_brush_erase(overlay.selected_brush_erase());
            if let Some((brush_kind, values)) = overlay.take_pending_brush_update() {
                match brush_kind {
                    BrushKind::Round => match RoundBrush::from_config_values(&values) {
                        Ok(updated_brush) => {
                            if let Err(error) =
                                integration.update_brush(brush_kind.brush_id(), updated_brush)
                            {
                                eprintln!("failed to update brush: {error:?}");
                            }
                        }
                        Err(error) => {
                            eprintln!("failed to build round brush from config: {error}");
                        }
                    },
                    BrushKind::PixelRect => match PixelRectBrush::from_config_values(&values) {
                        Ok(updated_brush) => {
                            if let Err(error) =
                                integration.update_brush(brush_kind.brush_id(), updated_brush)
                            {
                                eprintln!("failed to update brush: {error:?}");
                            }
                        }
                        Err(error) => {
                            eprintln!("failed to build pixel rect brush from config: {error}");
                        }
                    },
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
            if let Some(integration) = &mut self.integration {
                let (width, height) = integration.document_size();
                if let Err(error) = integration.save_screenshot(screenshot_path, width, height) {
                    eprintln!("Screenshot save failed: {error}");
                }
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
        let window =
            match event_loop.create_window(Window::default_attributes().with_title("glaphica")) {
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
            let integration = rt.block_on(async {
                AppThreadIntegration::new("test-document".to_string(), ImageLayout::new(1024, 1024))
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
            let round_brush = match RoundBrush::with_default_curves(3.0, 0.8) {
                Ok(brush) => brush,
                Err(error) => {
                    eprintln!("failed to build default brush: {error}");
                    event_loop.exit();
                    return;
                }
            };
            let pixel_rect_brush = PixelRectBrush::new(8);
            self.brush_states = vec![
                BrushUiState::new(BrushKind::Round, round_brush.config_items()),
                BrushUiState::new(BrushKind::PixelRect, pixel_rect_brush.config_items()),
            ];
            if let Err(error) = integration.register_brush(ROUND_BRUSH_ID, round_brush) {
                eprintln!("failed to register round brush: {error:?}");
                event_loop.exit();
                return;
            }
            if let Err(error) = integration.register_brush(PIXEL_RECT_BRUSH_ID, pixel_rect_brush) {
                eprintln!("failed to register pixel rect brush: {error:?}");
                event_loop.exit();
                return;
            }
            integration.set_active_brush(self.active_brush_kind.brush_id());

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

            let surface_runtime =
                match SurfaceRuntime::new(surface, adapter, device, size.0, size.1) {
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
                self.brush_states.clone(),
                self.active_brush_kind,
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
                            if let Some(overlay) = self.overlay.as_mut() {
                                overlay.flush_selected_brush_if_dirty();
                            }
                            self.render_frame();
                            self.stroke_active = true;
                            if let Some(integration) = &mut self.integration {
                                if let Some(node_id) = integration.active_document_node() {
                                    integration.begin_stroke(node_id);
                                }
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
    brush_states: Vec<BrushUiState>,
    selected_brush_index: usize,
    pending_brush_update: Option<(BrushKind, Vec<BrushConfigValue>)>,
    config_panel_rect: Option<Rect>,
}

impl EguiOverlay {
    fn new(
        event_loop: &ActiveEventLoop,
        window: &Window,
        device: &wgpu::Device,
        surface_format: wgpu::TextureFormat,
        brush_states: Vec<BrushUiState>,
        active_brush_kind: BrushKind,
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
        let selected_brush_index = brush_states
            .iter()
            .position(|state| state.kind == active_brush_kind)
            .unwrap_or(0);
        Self {
            ctx,
            state,
            renderer,
            left_panel_collapsed: false,
            right_panel_collapsed: false,
            brush_states,
            selected_brush_index,
            pending_brush_update: None,
            config_panel_rect: None,
        }
    }

    fn on_window_event(
        &mut self,
        window: &Window,
        event: &WindowEvent,
    ) -> egui_winit::EventResponse {
        self.state.on_window_event(window, event)
    }

    fn selected_brush_kind(&self) -> BrushKind {
        self.brush_states
            .get(self.selected_brush_index)
            .map(|state| state.kind)
            .unwrap_or(BrushKind::Round)
    }

    fn selected_brush_color_rgb(&self) -> [f32; 3] {
        self.brush_states
            .get(self.selected_brush_index)
            .map(|state| state.color_rgb)
            .unwrap_or([1.0, 0.0, 0.0])
    }

    fn selected_brush_erase(&self) -> bool {
        self.brush_states
            .get(self.selected_brush_index)
            .map(|state| state.eraser)
            .unwrap_or(false)
    }

    fn take_pending_brush_update(&mut self) -> Option<(BrushKind, Vec<BrushConfigValue>)> {
        self.pending_brush_update.take()
    }

    fn flush_selected_brush_if_dirty(&mut self) {
        self.queue_brush_update_if_dirty(self.selected_brush_index);
    }

    fn queue_brush_update_if_dirty(&mut self, index: usize) {
        if self.pending_brush_update.is_some() {
            return;
        }
        let Some(brush_state) = self.brush_states.get_mut(index) else {
            return;
        };
        if !brush_state.dirty {
            return;
        }
        brush_state.dirty = false;
        self.pending_brush_update = Some((brush_state.kind, brush_state.values.clone()));
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
        let brush_states = &mut self.brush_states;
        let selected_brush_index = &mut self.selected_brush_index;
        let pending_brush_update = &mut self.pending_brush_update;
        let config_panel_rect = &mut self.config_panel_rect;
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
                        ui.label("Brushes");
                        ui.label("Edit in right panel");
                    });
            }
            if *right_panel_collapsed {
                *config_panel_rect = None;
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
                let panel = SidePanel::right("overlay-right-panel")
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
                            ui.heading("Brush Config");
                        });
                        ui.separator();
                        let previous_index = *selected_brush_index;
                        let selected_label = brush_states
                            .get(*selected_brush_index)
                            .map(|state| state.kind.label())
                            .unwrap_or("Unknown");
                        egui::ComboBox::from_label("Engine")
                            .selected_text(selected_label)
                            .show_ui(ui, |ui| {
                                for kind in BrushKind::ALL {
                                    if let Some(index) =
                                        brush_states.iter().position(|state| state.kind == kind)
                                        && ui
                                            .selectable_label(
                                                *selected_brush_index == index,
                                                kind.label(),
                                            )
                                            .clicked()
                                    {
                                        *selected_brush_index = index;
                                    }
                                }
                            });
                        if previous_index != *selected_brush_index
                            && pending_brush_update.is_none()
                            && brush_states
                                .get(previous_index)
                                .map(|state| state.dirty)
                                .unwrap_or(false)
                        {
                            let previous = &mut brush_states[previous_index];
                            previous.dirty = false;
                            *pending_brush_update = Some((previous.kind, previous.values.clone()));
                        }
                        if let Some(brush_state) = brush_states.get_mut(*selected_brush_index) {
                            ui.separator();
                            egui::ScrollArea::vertical()
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    ui.group(|ui| {
                                        ui.label("Color");
                                        ui.checkbox(&mut brush_state.eraser, "Eraser");
                                        let mut srgb = [
                                            (brush_state.color_rgb[0].clamp(0.0, 1.0) * 255.0)
                                                .round()
                                                as u8,
                                            (brush_state.color_rgb[1].clamp(0.0, 1.0) * 255.0)
                                                .round()
                                                as u8,
                                            (brush_state.color_rgb[2].clamp(0.0, 1.0) * 255.0)
                                                .round()
                                                as u8,
                                        ];
                                        if ui.color_edit_button_srgb(&mut srgb).changed() {
                                            brush_state.color_rgb = [
                                                f32::from(srgb[0]) / 255.0,
                                                f32::from(srgb[1]) / 255.0,
                                                f32::from(srgb[2]) / 255.0,
                                            ];
                                        }
                                    });
                                    ui.add_space(8.0);

                                    ui.horizontal(|ui| {
                                        let hidden_items = brush_state
                                            .items
                                            .iter()
                                            .zip(brush_state.visible.iter())
                                            .enumerate()
                                            .filter(|(_, (item, visible))| {
                                                item.default_hidden && !**visible
                                            })
                                            .map(|(index, (item, _))| (index, item.label))
                                            .collect::<Vec<_>>();
                                        ui.add_enabled_ui(!hidden_items.is_empty(), |ui| {
                                            ui.menu_button("+", |ui| {
                                                for (index, label) in &hidden_items {
                                                    if ui.button(*label).clicked() {
                                                        if let Some(visible) =
                                                            brush_state.visible.get_mut(*index)
                                                        {
                                                            *visible = true;
                                                            brush_state.dirty = true;
                                                        }
                                                        ui.close();
                                                    }
                                                }
                                            });
                                        });
                                        if ui.button("Reset").clicked() {
                                            brush_state.reset();
                                        }
                                        if ui.button("Apply").clicked() {
                                            brush_state.dirty = false;
                                            *pending_brush_update = Some((
                                                brush_state.kind,
                                                brush_state.values.clone(),
                                            ));
                                        }
                                    });
                                    ui.separator();

                                    for index in 0..brush_state.items.len() {
                                        if !brush_state.visible.get(index).copied().unwrap_or(false)
                                        {
                                            continue;
                                        }
                                        let item_key = brush_state.items[index].key;
                                        let item_label = brush_state.items[index].label;
                                        let default_hidden =
                                            brush_state.items[index].default_hidden;
                                        let item_kind = brush_state.items[index].kind.clone();
                                        ui.group(|ui| {
                                            ui.horizontal(|ui| {
                                                ui.label(item_label);
                                                if default_hidden
                                                    && ui.small_button("Hide").clicked()
                                                    && let Some(visible) =
                                                        brush_state.visible.get_mut(index)
                                                {
                                                    *visible = false;
                                                    brush_state.dirty = true;
                                                }
                                            });
                                            match (&item_kind, &mut brush_state.values[index]) {
                                                (
                                                    BrushConfigKind::ScalarF32 { min, max },
                                                    BrushConfigValue::ScalarF32(current),
                                                ) => {
                                                    render_scalar_config(
                                                        ui,
                                                        item_key,
                                                        current,
                                                        *min,
                                                        *max,
                                                        &mut brush_state.dirty,
                                                    );
                                                }
                                                (
                                                    BrushConfigKind::UnitIntervalCurve,
                                                    BrushConfigValue::UnitIntervalCurve(points),
                                                ) => {
                                                    render_curve_config(
                                                        ui,
                                                        item_key,
                                                        points,
                                                        &mut brush_state.dirty,
                                                    );
                                                }
                                                _ => {
                                                    ui.colored_label(
                                                        Color32::from_rgb(220, 90, 90),
                                                        "Config type mismatch",
                                                    );
                                                }
                                            }
                                        });
                                        ui.add_space(8.0);
                                    }
                                });
                        }
                    });
                *config_panel_rect = Some(panel.response.rect);
            }
            TopBottomPanel::bottom("overlay-bottom-bar")
                .exact_height(48.0)
                .frame(egui::Frame::default().fill(Color32::from_rgb(20, 20, 20)))
                .show(ctx, |_ui| {});
        });
        self.state
            .handle_platform_output(window, full_output.platform_output);

        let pointer_pos = self.ctx.input(|input| input.pointer.latest_pos());
        if pending_brush_update.is_none()
            && let (Some(rect), Some(pointer_pos)) = (*config_panel_rect, pointer_pos)
            && !rect.contains(pointer_pos)
            && let Some(brush_state) = brush_states.get(*selected_brush_index)
            && brush_state.dirty
        {
            let brush_state = &mut brush_states[*selected_brush_index];
            brush_state.dirty = false;
            *pending_brush_update = Some((brush_state.kind, brush_state.values.clone()));
        }

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

fn render_scalar_config(
    ui: &mut egui::Ui,
    key: &'static str,
    value: &mut f32,
    min: f32,
    max: f32,
    dirty: &mut bool,
) {
    ui.push_id(key, |ui| {
        if ui
            .add(egui::Slider::new(value, min..=max).show_value(true))
            .changed()
        {
            *dirty = true;
        }
    });
}

fn render_curve_config(
    ui: &mut egui::Ui,
    key: &'static str,
    points: &mut Vec<UnitIntervalPoint>,
    dirty: &mut bool,
) {
    ui.push_id(key, |ui| {
        ui.horizontal(|ui| {
            if ui.small_button("+ point").clicked() {
                insert_curve_point(points);
                *dirty = true;
            }
            if points.len() > 2 && ui.small_button("- point").clicked() {
                points.remove(points.len().saturating_sub(2));
                *dirty = true;
            }
        });

        let desired_size = vec2(ui.available_width().max(120.0), 160.0);
        let (rect, response) = ui.allocate_exact_size(desired_size, Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        paint_curve_editor(&painter, rect, points);
        interact_with_curve(ui, key, rect, &response, points, dirty);
    });
}

fn insert_curve_point(points: &mut Vec<UnitIntervalPoint>) {
    if points.len() < 2 {
        points.push(UnitIntervalPoint::new(1.0, 1.0));
        return;
    }

    let mut insert_index = 1usize;
    let mut max_gap = 0.0f32;
    for index in 0..points.len() - 1 {
        let gap = points[index + 1].x - points[index].x;
        if gap > max_gap {
            max_gap = gap;
            insert_index = index + 1;
        }
    }
    let prev = points[insert_index - 1];
    let next = points[insert_index];
    points.insert(
        insert_index,
        UnitIntervalPoint::new((prev.x + next.x) * 0.5, (prev.y + next.y) * 0.5),
    );
}

fn paint_curve_editor(painter: &egui::Painter, rect: Rect, points: &[UnitIntervalPoint]) {
    let bg = Color32::from_rgb(18, 18, 18);
    let grid = Color32::from_rgb(48, 48, 48);
    let line = Color32::from_rgb(128, 214, 255);
    let point_fill = Color32::from_rgb(244, 174, 68);
    let point_stroke = Stroke::new(1.0, Color32::BLACK);

    painter.rect_filled(rect, 6.0, bg);
    painter.rect_stroke(rect, 6.0, Stroke::new(1.0, grid), egui::StrokeKind::Inside);

    for step in 1..4 {
        let t = step as f32 / 4.0;
        let x = egui::lerp(rect.left()..=rect.right(), t);
        let y = egui::lerp(rect.bottom()..=rect.top(), t);
        painter.line_segment(
            [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
            Stroke::new(1.0, grid),
        );
        painter.line_segment(
            [Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)],
            Stroke::new(1.0, grid),
        );
    }

    let mut curve = Vec::with_capacity(65);
    for step in 0..=64 {
        let x = step as f32 / 64.0;
        let y = eval_unit_interval_curve_polynomial(points, x).unwrap_or(0.0);
        curve.push(curve_pos(rect, x, y));
    }
    painter.add(Shape::line(curve, Stroke::new(2.0, line)));

    for point in points {
        painter.circle(
            curve_pos(rect, point.x, point.y),
            5.0,
            point_fill,
            point_stroke,
        );
    }
}

fn interact_with_curve(
    ui: &mut egui::Ui,
    key: &'static str,
    rect: Rect,
    response: &egui::Response,
    points: &mut [UnitIntervalPoint],
    dirty: &mut bool,
) {
    let drag_id = ui.id().with(key).with("curve_drag_index");
    if response.drag_started()
        && let Some(pointer_pos) = response.interact_pointer_pos()
    {
        let closest = points
            .iter()
            .enumerate()
            .map(|(index, point)| {
                (
                    index,
                    curve_pos(rect, point.x, point.y).distance(pointer_pos),
                )
            })
            .min_by(|(_, lhs), (_, rhs)| lhs.total_cmp(rhs));
        if let Some((index, distance)) = closest
            && distance <= 14.0
        {
            ui.memory_mut(|memory| memory.data.insert_temp(drag_id, index));
        }
    }

    if response.dragged()
        && let Some(pointer_pos) = response.interact_pointer_pos()
        && let Some(index) = ui.memory(|memory| memory.data.get_temp::<usize>(drag_id))
    {
        let mut x = ((pointer_pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
        let y = ((rect.bottom() - pointer_pos.y) / rect.height()).clamp(0.0, 1.0);
        if index == 0 {
            x = 0.0;
        } else if index + 1 == points.len() {
            x = 1.0;
        } else {
            let min_x = points[index - 1].x + 0.01;
            let max_x = points[index + 1].x - 0.01;
            x = x.clamp(min_x, max_x);
        }
        points[index] = UnitIntervalPoint::new(x, y);
        *dirty = true;
    }

    if response.drag_stopped() {
        ui.memory_mut(|memory| memory.data.remove::<usize>(drag_id));
    }
}

fn curve_pos(rect: Rect, x: f32, y: f32) -> Pos2 {
    Pos2::new(
        egui::lerp(rect.left()..=rect.right(), x),
        egui::lerp(rect.bottom()..=rect.top(), y),
    )
}

fn main() {
    let event_loop =
        EventLoop::new().expect("failed to create event loop: winit backend initialization failed");
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
    event_loop
        .run_app(&mut app)
        .expect("failed to run app: event loop terminated unexpectedly");
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
