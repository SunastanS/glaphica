use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use app::{AppThreadIntegration, trace::TraceRecorder};
use brushes::builtin_brushes::{pixel_rect::PixelRectBrush, round::RoundBrush};
use glaphica_core::{EpochId, NodeId};
use gpu_runtime::surface_runtime::SurfaceRuntime;
use images::layout::ImageLayout;
use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, EventLoop},
    window::{Window, WindowId},
};

use crate::brush_ui::state::{BrushKind, BrushUiState, PIXEL_RECT_BRUSH_ID, ROUND_BRUSH_ID};
use crate::input::{MouseInputResult, handle_window_event};
use crate::overlay::{EguiOverlay, ExitConfirmAction, OverlayAction, PathDialogAction};
use crate::run_config::RunConfig;

#[derive(Debug)]
pub enum AppActionError {
    BrushUpdate(String),
    BrushBuild(String),
    LayerSelectFailed(NodeId),
    LayerCreate(String),
    GroupCreate(String),
    LayerMove(NodeId, String),
    LayerVisibility(NodeId, String),
    LayerOpacity(NodeId, String),
    LayerBlendMode(NodeId, String),
    DocumentSave(PathBuf, String),
    DocumentLoad(PathBuf, String),
    DocumentExport(PathBuf, String),
    Multiple(Vec<AppActionError>),
}

impl std::fmt::Display for AppActionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AppActionError::BrushUpdate(e) => write!(f, "brush update failed: {:?}", e),
            AppActionError::BrushBuild(e) => write!(f, "brush build failed: {}", e),
            AppActionError::LayerSelectFailed(id) => write!(f, "layer select failed: {}", id.0),
            AppActionError::LayerCreate(e) => write!(f, "layer create failed: {:?}", e),
            AppActionError::GroupCreate(e) => write!(f, "group create failed: {:?}", e),
            AppActionError::LayerMove(id, e) => write!(f, "layer move failed ({}): {:?}", id.0, e),
            AppActionError::LayerVisibility(id, e) => {
                write!(f, "layer visibility failed ({}): {:?}", id.0, e)
            }
            AppActionError::LayerOpacity(id, e) => {
                write!(f, "layer opacity failed ({}): {:?}", id.0, e)
            }
            AppActionError::LayerBlendMode(id, e) => {
                write!(f, "layer blend mode failed ({}): {:?}", id.0, e)
            }
            AppActionError::DocumentSave(path, e) => {
                write!(f, "document save failed ({}): {:?}", path.display(), e)
            }
            AppActionError::DocumentLoad(path, e) => {
                write!(f, "document load failed ({}): {:?}", path.display(), e)
            }
            AppActionError::DocumentExport(path, e) => {
                write!(f, "document export failed ({}): {:?}", path.display(), e)
            }
            AppActionError::Multiple(errors) => {
                writeln!(f, "multiple action errors:")?;
                for error in errors {
                    writeln!(f, "  {}", error)?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for AppActionError {}

pub struct DesktopApp {
    pub(crate) window: Option<Arc<Window>>,
    pub(crate) integration: Option<AppThreadIntegration>,
    pub(crate) stroke_active: bool,
    pub(crate) epoch: EpochId,
    pub(crate) run_config: RunConfig,
    pub(crate) replay_frames: Vec<app::trace::TraceInputFrame>,
    pub(crate) replay_index: usize,
    pub(crate) replay_finished: bool,
    pub(crate) shutdown_requested: bool,
    pub(crate) shutdown_after_save: bool,
    pub(crate) deferred_shutdown_requested: bool,
    pub(crate) output_finalized: bool,
    pub(crate) started_at: Option<Instant>,
    pub(crate) sigint_flag: Arc<AtomicBool>,
    pub(crate) overlay: Option<EguiOverlay>,
    pub(crate) cursor_position: Option<(f32, f32)>,
    pub(crate) middle_pan_active: bool,
    pub(crate) middle_pan_last_position: Option<(f32, f32)>,
    pub(crate) ctrl_pressed: bool,
    pub(crate) shift_pressed: bool,
    pub(crate) active_brush_kind: BrushKind,
    pub(crate) brush_states: Vec<BrushUiState>,
}

impl DesktopApp {
    pub fn new(run_config: RunConfig, sigint_flag: Arc<AtomicBool>) -> Self {
        Self {
            window: None,
            integration: None,
            stroke_active: false,
            epoch: EpochId(0),
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

    pub fn is_replay_mode(&self) -> bool {
        self.run_config.replay_input_path.is_some()
    }

    pub fn advance_epoch(&mut self) {
        self.epoch = EpochId(self.epoch.0.saturating_add(1));
    }

    pub fn render_frame(&mut self) {
        let mut overlay_actions = Vec::new();
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
                overlay_actions = overlay.take_pending_actions();
            } else {
                integration.present_to_screen();
            }
        }
        match self.apply_overlay_actions(overlay_actions) {
            Ok(advance) => should_advance_epoch |= advance,
            Err(error) => eprintln!("overlay action errors: {}", error),
        }
        if should_advance_epoch {
            self.advance_epoch();
        }
    }

    fn apply_overlay_actions(&mut self, actions: Vec<OverlayAction>) -> Result<bool, AppActionError> {
        let mut should_advance_epoch = false;
        let mut errors = Vec::new();
        for action in actions {
            match self.apply_single_overlay_action(action) {
                Ok(advance) => should_advance_epoch |= advance,
                Err(error) => errors.push(error),
            }
        }
        if errors.is_empty() {
            Ok(should_advance_epoch)
        } else {
            Err(AppActionError::Multiple(errors))
        }
    }

    fn apply_single_overlay_action(&mut self, action: OverlayAction) -> Result<bool, AppActionError> {
        match action {
            OverlayAction::BrushUpdated(brush_kind, values) => {
                self.apply_brush_action(brush_kind, &values)
            }
            OverlayAction::LayerSelected(node_id) => self.apply_layer_select(node_id),
            OverlayAction::LayerCreated(kind) => self.apply_layer_create(kind),
            OverlayAction::GroupCreated => self.apply_group_create(),
            OverlayAction::LayerMoved(node_id, target) => self.apply_layer_move(node_id, target),
            OverlayAction::LayerVisibilityChanged(node_id, visible) => {
                self.apply_layer_visibility(node_id, visible)
            }
            OverlayAction::LayerOpacityChanged(node_id, opacity) => {
                self.apply_layer_opacity(node_id, opacity)
            }
            OverlayAction::LayerBlendModeChanged(node_id, blend_mode) => {
                self.apply_layer_blend_mode(node_id, blend_mode)
            }
            OverlayAction::DocumentSaveRequested(path) => self.apply_document_save(path),
            OverlayAction::DocumentLoadRequested(path) => self.apply_document_load(path),
            OverlayAction::DocumentExportRequested(path) => self.apply_document_export(path),
            OverlayAction::ExitConfirmed(action) => self.apply_exit_confirm(action),
            OverlayAction::PathDialogCancelled => self.apply_path_dialog_cancel(),
        }
    }

    fn apply_brush_action(
        &mut self,
        brush_kind: BrushKind,
        values: &[brushes::BrushConfigValue],
    ) -> Result<bool, AppActionError> {
        match brush_kind {
            BrushKind::Round => match RoundBrush::from_config_values(values) {
                Ok(updated_brush) => {
                    let Some(integration) = self.integration.as_mut() else {
                        return Ok(false);
                    };
                    integration
                        .update_brush(brush_kind.brush_id(), updated_brush)
                        .map_err(|e| AppActionError::BrushUpdate(format!("{:?}", e)))?;
                    Ok(true)
                }
                Err(error) => Err(AppActionError::BrushBuild(format!("round brush: {}", error))),
            },
            BrushKind::PixelRect => match PixelRectBrush::from_config_values(values) {
                Ok(updated_brush) => {
                    let Some(integration) = self.integration.as_mut() else {
                        return Ok(false);
                    };
                    integration
                        .update_brush(brush_kind.brush_id(), updated_brush)
                        .map_err(|e| AppActionError::BrushUpdate(format!("{:?}", e)))?;
                    Ok(true)
                }
                Err(error) => Err(AppActionError::BrushBuild(format!("pixel rect brush: {}", error))),
            },
        }
    }

    fn apply_layer_select(&mut self, node_id: NodeId) -> Result<bool, AppActionError> {
        let Some(integration) = self.integration.as_mut() else {
            return Ok(false);
        };
        integration
            .select_document_node(node_id)
            .then_some(true)
            .ok_or(AppActionError::LayerSelectFailed(node_id))
    }

    fn apply_layer_create(&mut self, kind: document::NewLayerKind) -> Result<bool, AppActionError> {
        let Some(integration) = self.integration.as_mut() else {
            return Ok(false);
        };
        integration
            .create_layer_above_active(kind)
            .inspect(|_| {
                if let Some(overlay) = self.overlay.as_mut() {
                    overlay.mark_document_dirty();
                }
            })
            .map(|()| true)
            .map_err(|e| AppActionError::LayerCreate(format!("{:?}", e)))
    }

    fn apply_group_create(&mut self) -> Result<bool, AppActionError> {
        let Some(integration) = self.integration.as_mut() else {
            return Ok(false);
        };
        integration
            .create_group_above_active()
            .inspect(|_| {
                if let Some(overlay) = self.overlay.as_mut() {
                    overlay.mark_document_dirty();
                }
            })
            .map(|()| true)
            .map_err(|e| AppActionError::GroupCreate(format!("{:?}", e)))
    }

    fn apply_layer_move(
        &mut self,
        node_id: NodeId,
        target: document::LayerMoveTarget,
    ) -> Result<bool, AppActionError> {
        let Some(integration) = self.integration.as_mut() else {
            return Ok(false);
        };
        integration
            .move_document_node(node_id, target)
            .inspect(|_| {
                if let Some(overlay) = self.overlay.as_mut() {
                    overlay.mark_document_dirty();
                }
            })
            .map(|()| true)
            .map_err(|e| AppActionError::LayerMove(node_id, format!("{:?}", e)))
    }

    fn apply_layer_visibility(
        &mut self,
        node_id: NodeId,
        visible: bool,
    ) -> Result<bool, AppActionError> {
        let Some(integration) = self.integration.as_mut() else {
            return Ok(false);
        };
        integration
            .set_document_node_visibility(node_id, visible)
            .inspect(|_| {
                if let Some(overlay) = self.overlay.as_mut() {
                    overlay.mark_document_dirty();
                }
            })
            .map(|()| true)
            .map_err(|e| AppActionError::LayerVisibility(node_id, format!("{:?}", e)))
    }

    fn apply_layer_opacity(
        &mut self,
        node_id: NodeId,
        opacity: f32,
    ) -> Result<bool, AppActionError> {
        let Some(integration) = self.integration.as_mut() else {
            return Ok(false);
        };
        integration
            .set_document_node_opacity(node_id, opacity)
            .inspect(|_| {
                if let Some(overlay) = self.overlay.as_mut() {
                    overlay.mark_document_dirty();
                }
            })
            .map(|()| true)
            .map_err(|e| AppActionError::LayerOpacity(node_id, format!("{:?}", e)))
    }

    fn apply_layer_blend_mode(
        &mut self,
        node_id: NodeId,
        blend_mode: document::UiBlendMode,
    ) -> Result<bool, AppActionError> {
        let Some(integration) = self.integration.as_mut() else {
            return Ok(false);
        };
        integration
            .set_document_node_blend_mode(node_id, blend_mode)
            .inspect(|_| {
                if let Some(overlay) = self.overlay.as_mut() {
                    overlay.mark_document_dirty();
                }
            })
            .map(|()| true)
            .map_err(|e| AppActionError::LayerBlendMode(node_id, format!("{:?}", e)))
    }

    fn apply_document_save(&mut self, path: std::path::PathBuf) -> Result<bool, AppActionError> {
        let Some(integration) = self.integration.as_mut() else {
            return Ok(false);
        };
        integration
            .save_document_bundle(&path)
            .inspect(|_| {
                if let Some(overlay) = self.overlay.as_mut() {
                    overlay.set_document_status(format!("Saved {}", path.display()), false);
                    overlay.mark_document_clean();
                }
                if self.shutdown_after_save {
                    self.shutdown_after_save = false;
                    self.deferred_shutdown_requested = true;
                }
            })
            .inspect_err(|error| {
                eprintln!("failed to save document bundle: {}", error);
                if let Some(overlay) = self.overlay.as_mut() {
                    overlay.set_document_status(format!("Save failed: {}", error), true);
                }
                self.shutdown_after_save = false;
            })
            .map(|()| false)
            .map_err(|error| AppActionError::DocumentSave(path, format!("{:?}", error)))
    }

    fn apply_document_load(&mut self, path: std::path::PathBuf) -> Result<bool, AppActionError> {
        let Some(integration) = self.integration.as_mut() else {
            return Ok(false);
        };
        integration
            .load_document_bundle(&path)
            .inspect(|_| {
                if let Some(overlay) = self.overlay.as_mut() {
                    overlay.set_document_status(format!("Loaded {}", path.display()), false);
                    overlay.mark_document_clean();
                }
            })
            .inspect_err(|error| {
                eprintln!("failed to load document bundle: {}", error);
                if let Some(overlay) = self.overlay.as_mut() {
                    overlay.set_document_status(format!("Load failed: {}", error), true);
                }
            })
            .map(|()| true)
            .map_err(|error| AppActionError::DocumentLoad(path, format!("{:?}", error)))
    }

    fn apply_document_export(&mut self, path: std::path::PathBuf) -> Result<bool, AppActionError> {
        let Some(integration) = self.integration.as_mut() else {
            return Ok(false);
        };
        integration
            .export_document_jpeg(&path)
            .inspect(|_| {
                if let Some(overlay) = self.overlay.as_mut() {
                    overlay.set_document_status(format!("Exported {}", path.display()), false);
                }
            })
            .inspect_err(|error| {
                eprintln!("failed to export document jpeg: {}", error);
                if let Some(overlay) = self.overlay.as_mut() {
                    overlay.set_document_status(format!("Export failed: {}", error), true);
                }
            })
            .map(|()| false)
            .map_err(|error| AppActionError::DocumentExport(path, format!("{:?}", error)))
    }

    fn apply_exit_confirm(&mut self, action: ExitConfirmAction) -> Result<bool, AppActionError> {
        match action {
            ExitConfirmAction::SaveAndExit => {
                self.shutdown_after_save = true;
                if let Some(overlay) = self.overlay.as_mut() {
                    overlay.open_path_dialog(PathDialogAction::Save);
                }
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            ExitConfirmAction::DiscardAndExit => {
                self.deferred_shutdown_requested = true;
            }
            ExitConfirmAction::Cancel => {
                self.shutdown_after_save = false;
            }
        }
        Ok(false)
    }

    fn apply_path_dialog_cancel(&mut self) -> Result<bool, AppActionError> {
        self.shutdown_after_save = false;
        Ok(false)
    }

    pub fn finalize_outputs(&mut self) {
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
                    eprintln!("Screenshot save failed: {}", error);
                }
            }
        }

        if self.run_config.record_input_path.is_some()
            || self.run_config.record_output_path.is_some()
        {
            if let Some(integration) = &mut self.integration
                && let Err(error) = integration.save_trace_files(
                    self.run_config.record_input_path.as_deref(),
                    self.run_config.record_output_path.as_deref(),
                )
            {
                eprintln!("Trace files save failed: {}", error);
            }
        }
        self.output_finalized = true;
    }

    pub fn request_shutdown(&mut self, event_loop: &ActiveEventLoop) {
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

    pub fn perform_shutdown(&mut self, event_loop: &ActiveEventLoop) {
        self.shutdown_requested = true;
        self.finalize_outputs();
        event_loop.exit();
    }
}

impl ApplicationHandler for DesktopApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let window =
            match event_loop.create_window(Window::default_attributes().with_title("glaphica")) {
                Ok(window) => Arc::new(window),
                Err(error) => {
                    eprintln!("failed to create window: {}", error);
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
                    eprintln!("failed to create tokio runtime: {}", error);
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
                    eprintln!("failed to init app: {:?}", error);
                    event_loop.exit();
                    return;
                }
            };

            let round_brush = match RoundBrush::with_default_curves(3.0, 0.8) {
                Ok(brush) => brush,
                Err(error) => {
                    eprintln!("failed to build default brush: {}", error);
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
                eprintln!("failed to register round brush: {:?}", error);
                event_loop.exit();
                return;
            }
            if let Err(error) = integration.register_brush(PIXEL_RECT_BRUSH_ID, pixel_rect_brush) {
                eprintln!("failed to register pixel rect brush: {:?}", error);
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
                    eprintln!("failed to create surface: {}", error);
                    event_loop.exit();
                    return;
                }
            };

            let size = (window.inner_size().width, window.inner_size().height);

            let surface_runtime =
                match SurfaceRuntime::new(surface, adapter, device, size.0, size.1) {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        eprintln!("failed to init surface: {:?}", error);
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
                    eprintln!("Replay input file load failed: {}", error);
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
        use winit::event::{ElementState, KeyEvent};
        use winit::keyboard::{Key, NamedKey};

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
            }
            WindowEvent::KeyboardInput {
                event: KeyEvent {
                    state, logical_key, ..
                },
                ..
            } => {
                if ui_event_consumed {
                    return;
                }
                if state == ElementState::Pressed {
                    match logical_key {
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
            _ => match handle_window_event(self, &event, ui_event_consumed) {
                MouseInputResult::StrokeBegan => {
                    if let Some(overlay) = &mut self.overlay {
                        overlay.flush_selected_brush_if_dirty();
                        let overlay_actions = overlay.take_pending_actions();
                        if let Err(error) = self.apply_overlay_actions(overlay_actions) {
                            eprintln!("overlay action errors: {}", error);
                        }
                    }
                    self.render_frame();
                    if let Some(integration) = &mut self.integration {
                        if let Some(node_id) = integration.active_paint_node() {
                            integration.begin_stroke(node_id);
                        }
                    }
                    if let Some(window) = &self.window {
                        window.request_redraw();
                    }
                }
                MouseInputResult::StrokeEnded
                | MouseInputResult::PanStarted
                | MouseInputResult::PanEnded
                | MouseInputResult::None => {
                    if let Some(window) = &self.window {
                        window.request_redraw();
                    }
                }
            },
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

pub fn run_app(run_config: RunConfig) {
    let event_loop =
        EventLoop::new().expect("failed to create event loop: winit backend initialization failed");
    let sigint_flag = Arc::new(AtomicBool::new(false));
    {
        let sigint_flag = sigint_flag.clone();
        if let Err(error) = ctrlc::set_handler(move || {
            sigint_flag.store(true, Ordering::Relaxed);
        }) {
            eprintln!("failed to set Ctrl+C handler: {}", error);
        }
    }
    let mut app = DesktopApp::new(run_config, sigint_flag);
    event_loop
        .run_app(&mut app)
        .expect("failed to run app: event loop terminated unexpectedly");
}
