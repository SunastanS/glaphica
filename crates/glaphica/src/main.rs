mod components;
mod egui_renderer;
mod theme;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use app::{AppStats, AppThreadIntegration, LayerPreviewBitmap, trace::TraceRecorder};
use brushes::builtin_brushes::{pixel_rect::PixelRectBrush, round::RoundBrush};
use brushes::{BrushConfigItem, BrushConfigValue};
use components::{ConfigPanel, Sidebar, StatusBar, TopBar};
use document::UiBlendMode;
use document::{LayerMoveTarget, NewLayerKind, UiLayerTreeItem};
use egui::Rect;
use egui_renderer::EguiRenderer;
use glaphica_core::{BrushId, CanvasVec2, InputDeviceKind, MappedCursor, NodeId, RadianVec2};
use gpu_runtime::surface_runtime::SurfaceRuntime;
use images::layout::ImageLayout;
use theme::Theme;
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
    shutdown_after_save: bool,
    deferred_shutdown_requested: bool,
    output_finalized: bool,
    started_at: Option<Instant>,
    sigint_flag: Arc<AtomicBool>,
    overlay: Option<EguiOverlay>,
    cursor_position: Option<(f32, f32)>,
    middle_pan_active: bool,
    middle_pan_last_position: Option<(f32, f32)>,
    ctrl_pressed: bool,
    shift_pressed: bool,
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
            shutdown_after_save: false,
            deferred_shutdown_requested: false,
            output_finalized: false,
            started_at: None,
            sigint_flag,
            overlay: None,
            cursor_position: None,
            middle_pan_active: false,
            middle_pan_last_position: None,
            ctrl_pressed: false,
            shift_pressed: false,
            active_brush_kind: BrushKind::Round,
            brush_states: Vec::new(),
        }
    }

    fn is_replay_mode(&self) -> bool {
        self.run_config.replay_input_path.is_some()
    }

    fn advance_epoch(&mut self) {
        self.epoch = glaphica_core::EpochId(self.epoch.0.saturating_add(1));
    }

    fn render_frame(&mut self) {
        let mut should_advance_epoch = false;
        {
            let Some(integration) = &mut self.integration else {
                return;
            };
            integration.process_main_render();
            if let Some(overlay) = &mut self.overlay {
                overlay.set_app_stats(integration.stats());
                overlay.sync_layer_tree(
                    integration.layer_tree_items(),
                    integration.active_document_node(),
                    integration.take_layer_preview_updates(),
                );
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
                if let Some(node_id) = overlay.take_pending_layer_select() {
                    if integration.select_document_node(node_id) {
                        should_advance_epoch = true;
                    }
                }
                if overlay.take_path_dialog_cancelled() {
                    self.shutdown_after_save = false;
                }
                if let Some(kind) = overlay.take_pending_layer_create() {
                    match integration.create_layer_above_active(kind) {
                        Ok(()) => {
                            overlay.mark_document_dirty();
                            should_advance_epoch = true;
                        }
                        Err(error) => eprintln!("failed to create layer: {error:?}"),
                    }
                }
                if overlay.take_pending_group_create() {
                    match integration.create_group_above_active() {
                        Ok(()) => {
                            overlay.mark_document_dirty();
                            should_advance_epoch = true;
                        }
                        Err(error) => eprintln!("failed to create group: {error:?}"),
                    }
                }
                if let Some((node_id, target)) = overlay.take_pending_layer_move() {
                    if let Err(error) = integration.move_document_node(node_id, target) {
                        eprintln!("failed to move layer: {error:?}");
                    } else {
                        overlay.mark_document_dirty();
                        should_advance_epoch = true;
                    }
                }
                if let Some((node_id, visible)) = overlay.take_pending_layer_visibility() {
                    if let Err(error) = integration.set_document_node_visibility(node_id, visible) {
                        eprintln!("failed to set layer visibility: {error:?}");
                    } else {
                        overlay.mark_document_dirty();
                        should_advance_epoch = true;
                    }
                }
                if let Some((node_id, opacity)) = overlay.take_pending_layer_opacity() {
                    if let Err(error) = integration.set_document_node_opacity(node_id, opacity) {
                        eprintln!("failed to set layer opacity: {error:?}");
                    } else {
                        overlay.mark_document_dirty();
                        should_advance_epoch = true;
                    }
                }
                if let Some((node_id, blend_mode)) = overlay.take_pending_layer_blend_mode() {
                    if let Err(error) =
                        integration.set_document_node_blend_mode(node_id, blend_mode)
                    {
                        eprintln!("failed to set layer blend mode: {error:?}");
                    } else {
                        overlay.mark_document_dirty();
                        should_advance_epoch = true;
                    }
                }
                if let Some(path) = overlay.take_pending_document_save() {
                    match integration.save_document_bundle(&path) {
                        Ok(()) => {
                            overlay.set_document_status(format!("Saved {}", path.display()), false);
                            overlay.mark_document_clean();
                            if self.shutdown_after_save {
                                self.shutdown_after_save = false;
                                self.deferred_shutdown_requested = true;
                            }
                        }
                        Err(error) => {
                            eprintln!("failed to save document bundle: {error}");
                            overlay.set_document_status(format!("Save failed: {error}"), true);
                            self.shutdown_after_save = false;
                        }
                    }
                }
                if let Some(path) = overlay.take_pending_document_load() {
                    match integration.load_document_bundle(&path) {
                        Ok(()) => {
                            overlay
                                .set_document_status(format!("Loaded {}", path.display()), false);
                            overlay.mark_document_clean();
                            should_advance_epoch = true;
                        }
                        Err(error) => {
                            eprintln!("failed to load document bundle: {error}");
                            overlay.set_document_status(format!("Load failed: {error}"), true);
                        }
                    }
                }
                if let Some(path) = overlay.take_pending_document_export() {
                    match integration.export_document_jpeg(&path) {
                        Ok(()) => {
                            overlay
                                .set_document_status(format!("Exported {}", path.display()), false);
                        }
                        Err(error) => {
                            eprintln!("failed to export document jpeg: {error}");
                            overlay.set_document_status(format!("Export failed: {error}"), true);
                        }
                    }
                }
            } else {
                integration.present_to_screen();
            }
        }
        if should_advance_epoch {
            self.advance_epoch();
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
        if let Some(overlay) = self.overlay.as_mut()
            && overlay.document_dirty
        {
            overlay.exit_confirm_open = true;
            if let Some(window) = &self.window {
                window.request_redraw();
            }
            return;
        }
        self.perform_shutdown(event_loop);
    }

    fn perform_shutdown(&mut self, event_loop: &ActiveEventLoop) {
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
                self.run_config
                    .document_bundle_path
                    .clone()
                    .unwrap_or_else(|| PathBuf::from("document.glaphica"))
                    .display()
                    .to_string(),
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
        if let WindowEvent::KeyboardInput { event, .. } = &event
            && event.state == ElementState::Pressed
            && !event.repeat
            && matches!(event.logical_key, Key::Named(NamedKey::Tab))
        {
            if let Some(overlay) = self.overlay.as_mut() {
                let collapsed = !overlay.left_panel_collapsed || !overlay.right_panel_collapsed;
                overlay.left_panel_collapsed = collapsed;
                overlay.right_panel_collapsed = collapsed;
            }
            if let Some(window) = &self.window
                && window.id() == window_id
            {
                window.request_redraw();
            }
            return;
        }

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
                if let Some(overlay) = self.overlay.as_mut() {
                    match overlay.take_exit_confirm_action() {
                        Some(ExitConfirmAction::SaveAndExit) => {
                            self.shutdown_after_save = true;
                            overlay.open_path_dialog(PathDialogAction::Save);
                            self.render_frame();
                        }
                        Some(ExitConfirmAction::DiscardAndExit) => {
                            self.perform_shutdown(event_loop)
                        }
                        Some(ExitConfirmAction::Cancel) => {
                            self.shutdown_after_save = false;
                        }
                        None => {}
                    }
                }
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
                        if let Some(overlay) = self.overlay.as_mut() {
                            overlay.mark_document_dirty();
                        }
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
                            self.stroke_active = false;
                            if let Some(integration) = &mut self.integration {
                                if let Some(node_id) = integration.active_paint_node() {
                                    integration.begin_stroke(node_id);
                                    self.stroke_active = true;
                                }
                            }
                        }
                        ElementState::Released => {
                            let stroke_was_active = self.stroke_active;
                            self.stroke_active = false;
                            if let Some(integration) = &mut self.integration {
                                integration.end_stroke();
                            }
                            if stroke_was_active && let Some(overlay) = self.overlay.as_mut() {
                                overlay.mark_document_dirty();
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
                self.shift_pressed = modifiers.state().shift_key();
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if ui_event_consumed {
                    return;
                }
                if event.state == ElementState::Pressed && !event.repeat {
                    match &event.logical_key {
                        Key::Character(value)
                            if self.ctrl_pressed
                                && self.shift_pressed
                                && value.eq_ignore_ascii_case("z") =>
                        {
                            if let Some(integration) = &mut self.integration {
                                if integration.redo_stroke()
                                    && let Some(window) = &self.window
                                {
                                    if let Some(overlay) = self.overlay.as_mut() {
                                        overlay.mark_document_dirty();
                                    }
                                    window.request_redraw();
                                }
                            }
                        }
                        Key::Character(value)
                            if self.ctrl_pressed && value.eq_ignore_ascii_case("z") =>
                        {
                            if let Some(integration) = &mut self.integration {
                                if integration.undo_stroke()
                                    && let Some(window) = &self.window
                                {
                                    if let Some(overlay) = self.overlay.as_mut() {
                                        overlay.mark_document_dirty();
                                    }
                                    window.request_redraw();
                                }
                            }
                        }
                        Key::Character(value)
                            if self.ctrl_pressed && value.eq_ignore_ascii_case("s") =>
                        {
                            if let Some(overlay) = self.overlay.as_mut() {
                                overlay.open_path_dialog(PathDialogAction::Save);
                            }
                            if let Some(window) = &self.window {
                                window.request_redraw();
                            }
                        }
                        Key::Named(NamedKey::Escape) => self.request_shutdown(event_loop),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.deferred_shutdown_requested {
            self.deferred_shutdown_requested = false;
            self.perform_shutdown(event_loop);
            return;
        }
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
    theme: Theme,
    left_panel_collapsed: bool,
    right_panel_collapsed: bool,
    active_color_rgb: [f32; 3],
    left_panel_width: f32,
    right_panel_width: f32,
    brush_states: Vec<BrushUiState>,
    selected_brush_index: usize,
    pending_brush_update: Option<(BrushKind, Vec<BrushConfigValue>)>,
    layer_tree_items: Vec<UiLayerTreeItem>,
    layer_preview_textures: HashMap<NodeId, egui::TextureHandle>,
    selected_node: Option<NodeId>,
    pending_layer_select: Option<NodeId>,
    pending_layer_create: Option<NewLayerKind>,
    pending_group_create: bool,
    pending_layer_move: Option<(NodeId, LayerMoveTarget)>,
    pending_layer_visibility: Option<(NodeId, bool)>,
    pending_layer_opacity: Option<(NodeId, f32)>,
    pending_layer_blend_mode: Option<(NodeId, UiBlendMode)>,
    pending_document_save: bool,
    pending_document_load: bool,
    pending_document_export: bool,
    document_path: String,
    path_dialog_action: Option<PathDialogAction>,
    path_dialog_cancelled: bool,
    document_status_text: Option<String>,
    document_status_is_error: bool,
    document_dirty: bool,
    exit_confirm_open: bool,
    exit_confirm_action: Option<ExitConfirmAction>,
    config_panel_rect: Option<Rect>,
    app_stats: Option<AppStats>,
}

#[derive(Clone, Copy)]
enum ExitConfirmAction {
    SaveAndExit,
    DiscardAndExit,
    Cancel,
}

#[derive(Clone, Copy)]
enum PathDialogAction {
    Save,
    Load,
    Export,
}

impl EguiOverlay {
    fn new(
        event_loop: &ActiveEventLoop,
        window: &Window,
        device: &wgpu::Device,
        surface_format: wgpu::TextureFormat,
        brush_states: Vec<BrushUiState>,
        active_brush_kind: BrushKind,
        document_path: String,
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
            theme: Theme::dark(),
            left_panel_collapsed: false,
            right_panel_collapsed: false,
            active_color_rgb: [1.0, 0.0, 0.0],
            left_panel_width: 280.0,
            right_panel_width: 240.0,
            brush_states,
            selected_brush_index,
            pending_brush_update: None,
            layer_tree_items: Vec::new(),
            layer_preview_textures: HashMap::new(),
            selected_node: None,
            pending_layer_select: None,
            pending_layer_create: None,
            pending_group_create: false,
            pending_layer_move: None,
            pending_layer_visibility: None,
            pending_layer_opacity: None,
            pending_layer_blend_mode: None,
            pending_document_save: false,
            pending_document_load: false,
            pending_document_export: false,
            document_path,
            path_dialog_action: None,
            path_dialog_cancelled: false,
            document_status_text: None,
            document_status_is_error: false,
            document_dirty: false,
            exit_confirm_open: false,
            exit_confirm_action: None,
            config_panel_rect: None,
            app_stats: None,
        }
    }

    fn set_app_stats(&mut self, stats: AppStats) {
        self.app_stats = Some(stats);
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
        self.active_color_rgb
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

    fn sync_layer_tree(
        &mut self,
        layer_tree_items: Vec<UiLayerTreeItem>,
        selected_node: Option<NodeId>,
        preview_updates: Vec<LayerPreviewBitmap>,
    ) {
        self.layer_tree_items = layer_tree_items;
        self.selected_node = selected_node;
        self.sync_layer_preview_textures(preview_updates);
        let valid_ids = collect_layer_tree_ids(&self.layer_tree_items);
        self.layer_preview_textures
            .retain(|node_id, _| valid_ids.contains(node_id));
    }

    fn sync_layer_preview_textures(&mut self, preview_updates: Vec<LayerPreviewBitmap>) {
        for preview in preview_updates {
            let texture = self.ctx.load_texture(
                format!("layer-preview-{}", preview.node_id.0),
                egui::ColorImage::from_rgba_unmultiplied(
                    [preview.width as usize, preview.height as usize],
                    &preview.pixels,
                ),
                egui::TextureOptions::NEAREST,
            );
            self.layer_preview_textures.insert(preview.node_id, texture);
        }
    }

    fn take_pending_layer_select(&mut self) -> Option<NodeId> {
        self.pending_layer_select.take()
    }

    fn take_pending_layer_create(&mut self) -> Option<NewLayerKind> {
        self.pending_layer_create.take()
    }

    fn take_pending_group_create(&mut self) -> bool {
        std::mem::take(&mut self.pending_group_create)
    }

    fn take_pending_layer_move(&mut self) -> Option<(NodeId, LayerMoveTarget)> {
        self.pending_layer_move.take()
    }

    fn take_pending_layer_visibility(&mut self) -> Option<(NodeId, bool)> {
        self.pending_layer_visibility.take()
    }

    fn take_pending_layer_opacity(&mut self) -> Option<(NodeId, f32)> {
        self.pending_layer_opacity.take()
    }

    fn take_pending_layer_blend_mode(&mut self) -> Option<(NodeId, UiBlendMode)> {
        self.pending_layer_blend_mode.take()
    }

    fn take_pending_document_save(&mut self) -> Option<PathBuf> {
        if std::mem::take(&mut self.pending_document_save) {
            let path = self.document_path.trim();
            if !path.is_empty() {
                return Some(PathBuf::from(path));
            }
        }
        None
    }

    fn take_pending_document_load(&mut self) -> Option<PathBuf> {
        if std::mem::take(&mut self.pending_document_load) {
            let path = self.document_path.trim();
            if !path.is_empty() {
                return Some(PathBuf::from(path));
            }
        }
        None
    }

    fn take_pending_document_export(&mut self) -> Option<PathBuf> {
        if std::mem::take(&mut self.pending_document_export) {
            let path = self.document_path.trim();
            if !path.is_empty() {
                return Some(PathBuf::from(path));
            }
        }
        None
    }

    fn open_path_dialog(&mut self, action: PathDialogAction) {
        self.document_path = self.suggested_path_for_action(action);
        self.path_dialog_action = Some(action);
    }

    fn confirm_path_dialog(&mut self) {
        match self.path_dialog_action.take() {
            Some(PathDialogAction::Save) => self.pending_document_save = true,
            Some(PathDialogAction::Load) => self.pending_document_load = true,
            Some(PathDialogAction::Export) => self.pending_document_export = true,
            None => {}
        }
    }

    fn suggested_path_for_action(&self, action: PathDialogAction) -> String {
        let current = Path::new(self.document_path.trim());
        let file_stem = current
            .file_stem()
            .and_then(|stem| stem.to_str())
            .filter(|stem| !stem.is_empty())
            .unwrap_or("document");
        let extension = match action {
            PathDialogAction::Save | PathDialogAction::Load => "glaphica",
            PathDialogAction::Export => "jpeg",
        };
        let file_name = format!("{file_stem}.{extension}");
        match current.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent.join(file_name),
            _ => PathBuf::from(file_name),
        }
        .display()
        .to_string()
    }

    fn take_path_dialog_cancelled(&mut self) -> bool {
        std::mem::take(&mut self.path_dialog_cancelled)
    }

    fn set_document_status(&mut self, text: String, is_error: bool) {
        self.document_status_text = Some(text);
        self.document_status_is_error = is_error;
    }

    fn mark_document_dirty(&mut self) {
        self.document_dirty = true;
    }

    fn mark_document_clean(&mut self) {
        self.document_dirty = false;
    }

    fn take_exit_confirm_action(&mut self) -> Option<ExitConfirmAction> {
        self.exit_confirm_action.take()
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
        let theme = self.theme;
        let left_panel_collapsed = &mut self.left_panel_collapsed;
        let right_panel_collapsed = &mut self.right_panel_collapsed;
        let left_panel_width = &mut self.left_panel_width;
        let right_panel_width = &mut self.right_panel_width;
        let active_color_rgb = &mut self.active_color_rgb;
        let brush_states = &mut self.brush_states;
        let selected_brush_index = &mut self.selected_brush_index;
        let pending_brush_update = &mut self.pending_brush_update;
        let layer_tree_items = &self.layer_tree_items;
        let preview_texture_ids: HashMap<_, _> = self
            .layer_preview_textures
            .iter()
            .map(|(node_id, texture)| (*node_id, texture.id()))
            .collect();
        let selected_node = &mut self.selected_node;
        let pending_layer_select = &mut self.pending_layer_select;
        let pending_layer_create = &mut self.pending_layer_create;
        let pending_group_create = &mut self.pending_group_create;
        let pending_layer_move = &mut self.pending_layer_move;
        let pending_layer_visibility = &mut self.pending_layer_visibility;
        let pending_layer_opacity = &mut self.pending_layer_opacity;
        let pending_layer_blend_mode = &mut self.pending_layer_blend_mode;
        let mut config_panel_rect = self.config_panel_rect;
        let panel_max_width = (target_width as f32 - 96.0)
            .max(0.0)
            .min(target_width as f32);
        let mut requested_path_dialog = None;
        let mut confirm_path_dialog = false;
        let mut cancel_path_dialog = false;

        let full_output = self.ctx.run(raw_input, |ctx| {
            let mut top_bar = TopBar::new();
            let top_bar_output = top_bar.render(ctx, &theme);
            if top_bar_output.save_clicked {
                requested_path_dialog = Some(PathDialogAction::Save);
            }
            if top_bar_output.load_clicked {
                requested_path_dialog = Some(PathDialogAction::Load);
            }
            if top_bar_output.export_clicked {
                requested_path_dialog = Some(PathDialogAction::Export);
            }

            StatusBar::render(ctx, &theme, self.app_stats.as_ref());

            let sidebar = Sidebar::new(
                *left_panel_collapsed,
                *left_panel_width,
                panel_max_width,
                layer_tree_items,
                *selected_node,
                &preview_texture_ids,
            );
            let sidebar_output = sidebar.render(ctx, &theme);

            if sidebar_output.toggle_collapse {
                *left_panel_collapsed = !*left_panel_collapsed;
            }
            if let Some(kind) = sidebar_output.create_layer {
                *pending_layer_create = Some(kind);
            }
            if sidebar_output.create_group {
                *pending_group_create = true;
            }
            if let Some(node_id) = sidebar_output.select_layer {
                *pending_layer_select = Some(node_id);
            }
            if let Some(layer_move) = sidebar_output.move_layer {
                *pending_layer_move = Some((layer_move.node_id, layer_move.target));
            }
            if let Some((node_id, visible)) = sidebar_output.set_layer_visibility {
                *pending_layer_visibility = Some((node_id, visible));
            }
            if let Some((node_id, opacity)) = sidebar_output.set_layer_opacity {
                *pending_layer_opacity = Some((node_id, opacity));
            }
            if let Some((node_id, blend_mode)) = sidebar_output.set_layer_blend_mode {
                *pending_layer_blend_mode = Some((node_id, blend_mode));
            }
            if let Some(rect) = sidebar_output.panel_rect {
                *left_panel_width = rect.width();
            }

            let mut config_panel = ConfigPanel::new(
                *right_panel_collapsed,
                *right_panel_width,
                panel_max_width,
                active_color_rgb,
                brush_states,
                *selected_brush_index,
            );
            let config_output = config_panel.render(ctx, &theme);

            if config_output.toggle_collapse {
                *right_panel_collapsed = !*right_panel_collapsed;
            }
            if let Some((kind, values)) = config_output.pending_brush_update {
                *pending_brush_update = Some((kind, values));
            }
            if config_output.brush_selection_changed {
                if let Some(new_index) = config_output.new_selected_index {
                    *selected_brush_index = new_index;
                }
            }
            config_panel_rect = config_output.panel_rect;
            if let Some(rect) = config_output.panel_rect {
                *right_panel_width = rect.width();
            }

            if self.exit_confirm_open {
                egui::Window::new("Unsaved Changes")
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .collapsible(false)
                    .resizable(false)
                    .show(ctx, |ui| {
                        ui.label("Save changes before exit?");
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            if ui.button("Save").clicked() {
                                self.exit_confirm_open = false;
                                self.exit_confirm_action = Some(ExitConfirmAction::SaveAndExit);
                            }
                            if ui.button("Discard").clicked() {
                                self.exit_confirm_open = false;
                                self.exit_confirm_action = Some(ExitConfirmAction::DiscardAndExit);
                            }
                            if ui.button("Cancel").clicked() {
                                self.exit_confirm_open = false;
                                self.exit_confirm_action = Some(ExitConfirmAction::Cancel);
                            }
                        });
                    });
            }

            if let Some(action) = self.path_dialog_action {
                let (title, confirm_label, hint) = match action {
                    PathDialogAction::Save => ("Save Document", "Save", "Enter bundle output path"),
                    PathDialogAction::Load => ("Load Document", "Load", "Enter bundle input path"),
                    PathDialogAction::Export => {
                        ("Export JPEG", "Export", "Enter .jpg or .jpeg output path")
                    }
                };
                egui::Window::new(title)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .collapsible(false)
                    .resizable(false)
                    .show(ctx, |ui| {
                        ui.label(hint);
                        ui.add_space(8.0);
                        ui.add(
                            egui::TextEdit::singleline(&mut self.document_path)
                                .desired_width(360.0),
                        );
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            if ui.button(confirm_label).clicked() {
                                confirm_path_dialog = true;
                            }
                            if ui.button("Cancel").clicked() {
                                cancel_path_dialog = true;
                            }
                        });
                    });
            }
        });

        if let Some(action) = requested_path_dialog {
            self.open_path_dialog(action);
        }
        if confirm_path_dialog {
            self.confirm_path_dialog();
        }
        if cancel_path_dialog {
            self.path_dialog_action = None;
            self.path_dialog_cancelled = true;
        }
        self.config_panel_rect = config_panel_rect;

        self.state
            .handle_platform_output(window, full_output.platform_output);

        let pointer_pos = self.ctx.input(|input| input.pointer.latest_pos());
        if self.pending_brush_update.is_none()
            && let (Some(rect), Some(pointer_pos)) = (config_panel_rect, pointer_pos)
            && !rect.contains(pointer_pos)
            && let Some(brush_state) = self.brush_states.get(self.selected_brush_index)
            && brush_state.dirty
        {
            let brush_state = &mut self.brush_states[self.selected_brush_index];
            brush_state.dirty = false;
            self.pending_brush_update = Some((brush_state.kind, brush_state.values.clone()));
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

fn collect_layer_tree_ids(items: &[UiLayerTreeItem]) -> std::collections::HashSet<NodeId> {
    fn collect_into(items: &[UiLayerTreeItem], output: &mut std::collections::HashSet<NodeId>) {
        for item in items {
            output.insert(item.id);
            collect_into(&item.children, output);
        }
    }

    let mut ids = std::collections::HashSet::new();
    collect_into(items, &mut ids);
    ids
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
    document_bundle_path: Option<PathBuf>,
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
                "--document-bundle" => {
                    if let Some(path) = args.get(index + 1) {
                        config.document_bundle_path = Some(Path::new(path).to_path_buf());
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
