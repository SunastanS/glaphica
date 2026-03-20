use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use app::{AppThreadIntegration, trace::TraceRecorder};
use brushes::builtin_brushes::{pixel_rect::PixelRectBrush, round::RoundBrush};
use egui::Pos2;
use glaphica_core::{EpochId, NodeId};
use gpu_runtime::{GpuContext, GpuContextInitDescriptor, surface_runtime::SurfaceRuntime};
use images::layout::ImageLayout;
use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, EventLoop},
    window::{Window, WindowId},
};

use crate::brush_ui::state::{BrushKind, BrushUiState, PIXEL_RECT_BRUSH_ID, ROUND_BRUSH_ID};
use crate::input::{MouseInputResult, handle_window_event};
use crate::overlay::{EguiOverlay, ExitConfirmAction, PathDialogAction, UiCommand};
use crate::run_config::RunConfig;

#[derive(Debug, Default)]
pub struct ApplyActionsEffect {
    pub advance_epoch: bool,
    pub request_redraw: bool,
}

impl ApplyActionsEffect {
    pub fn merge(&mut self, other: Self) {
        self.advance_epoch |= other.advance_epoch;
        self.request_redraw |= other.request_redraw;
    }
}

pub struct ApplyActionsReport {
    pub effect: ApplyActionsEffect,
    pub errors: Vec<AppActionError>,
}

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
}

impl std::fmt::Display for AppActionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AppActionError::BrushUpdate(e) => write!(f, "brush update failed: {}", e),
            AppActionError::BrushBuild(e) => write!(f, "brush build failed: {}", e),
            AppActionError::LayerSelectFailed(id) => write!(f, "layer select failed: {}", id.0),
            AppActionError::LayerCreate(e) => write!(f, "layer create failed: {}", e),
            AppActionError::GroupCreate(e) => write!(f, "group create failed: {}", e),
            AppActionError::LayerMove(id, e) => write!(f, "layer move failed ({}): {}", id.0, e),
            AppActionError::LayerVisibility(id, e) => {
                write!(f, "layer visibility failed ({}): {}", id.0, e)
            }
            AppActionError::LayerOpacity(id, e) => {
                write!(f, "layer opacity failed ({}): {}", id.0, e)
            }
            AppActionError::LayerBlendMode(id, e) => {
                write!(f, "layer blend mode failed ({}): {}", id.0, e)
            }
            AppActionError::DocumentSave(path, e) => {
                write!(f, "document save failed ({}): {}", path.display(), e)
            }
            AppActionError::DocumentLoad(path, e) => {
                write!(f, "document load failed ({}): {}", path.display(), e)
            }
            AppActionError::DocumentExport(path, e) => {
                write!(f, "document export failed ({}): {}", path.display(), e)
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
    pub(crate) canvas_crop: CanvasCropState,
}

#[derive(Default)]
pub struct CanvasCropState {
    pub(crate) active_drag: bool,
    pub(crate) preview_size: Option<(u32, u32)>,
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
            canvas_crop: CanvasCropState::default(),
        }
    }

    pub fn is_replay_mode(&self) -> bool {
        self.run_config.replay_input_path.is_some()
    }

    pub fn advance_epoch(&mut self) {
        self.epoch = EpochId(self.epoch.0.saturating_add(1));
    }

    pub fn canvas_crop_mode_active(&self) -> bool {
        self.overlay
            .as_ref()
            .is_some_and(|overlay| overlay.canvas_crop_mode_active())
    }

    pub fn canvas_crop_handle_hit(&self, screen_position: (f32, f32)) -> bool {
        let Some(overlay) = self.overlay.as_ref() else {
            return false;
        };
        let Some(handle_center) = overlay.canvas_crop_handle_center() else {
            return false;
        };
        handle_center.distance(Pos2::new(screen_position.0, screen_position.1)) <= 14.0
    }

    pub fn begin_canvas_crop_drag(&mut self) -> bool {
        let Some(integration) = self.integration.as_ref() else {
            return false;
        };
        self.canvas_crop.active_drag = true;
        self.canvas_crop.preview_size = Some(integration.document_size());
        true
    }

    pub fn update_canvas_crop_preview(&mut self, screen_position: (f32, f32)) -> bool {
        if !self.canvas_crop.active_drag {
            return false;
        }
        let Some(integration) = self.integration.as_ref() else {
            return false;
        };
        let (doc_x, doc_y) =
            integration.map_screen_to_document(screen_position.0, screen_position.1);
        let next_size = (crop_extent_to_size(doc_x), crop_extent_to_size(doc_y));
        if self.canvas_crop.preview_size == Some(next_size) {
            return false;
        }
        self.canvas_crop.preview_size = Some(next_size);
        true
    }

    pub fn commit_canvas_crop(&mut self) -> bool {
        if !self.canvas_crop.active_drag {
            return false;
        }
        self.canvas_crop.active_drag = false;
        let Some((width, height)) = self.canvas_crop.preview_size.take() else {
            return false;
        };
        let Some(integration) = self.integration.as_mut() else {
            return false;
        };
        match integration.resize_document_canvas_anchored_top_left(ImageLayout::new(width, height))
        {
            Ok(()) => {
                self.advance_epoch();
                if let Some(overlay) = self.overlay.as_mut() {
                    overlay.mark_document_dirty();
                    overlay.set_document_status(
                        format!("Canvas resized to {} x {}", width, height),
                        false,
                    );
                }
                true
            }
            Err(error) => {
                if let Some(overlay) = self.overlay.as_mut() {
                    overlay.set_document_status(format!("Canvas resize failed: {:?}", error), true);
                }
                false
            }
        }
    }

    pub fn cancel_canvas_crop_interaction(&mut self) {
        self.canvas_crop.active_drag = false;
        self.canvas_crop.preview_size = None;
    }

    pub fn render_frame(&mut self) {
        let mut overlay_actions = Vec::new();
        let mut clear_canvas_crop = false;
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
                let crop_mode_active = overlay.canvas_crop_mode_active();
                if crop_mode_active {
                    let preview_size = self
                        .canvas_crop
                        .preview_size
                        .unwrap_or(integration.document_size());
                    let outline = crop_outline_screen_points(integration, preview_size);
                    overlay.set_canvas_crop_overlay(
                        Some(outline),
                        Some(outline[2]),
                        self.canvas_crop.active_drag,
                    );
                } else {
                    clear_canvas_crop = true;
                    overlay.set_canvas_crop_overlay(None, None, false);
                }
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
        if clear_canvas_crop {
            self.cancel_canvas_crop_interaction();
        }
        let report = self.apply_overlay_actions(overlay_actions);
        for error in report.errors {
            eprintln!("overlay action errors: {}", error);
        }
        if report.effect.advance_epoch {
            self.advance_epoch();
        }
        if report.effect.request_redraw {
            if let Some(window) = &self.window {
                window.request_redraw();
            }
        }
    }

    fn apply_overlay_actions(&mut self, actions: Vec<UiCommand>) -> ApplyActionsReport {
        let mut effect = ApplyActionsEffect::default();
        let mut errors = Vec::new();
        for action in actions {
            match self.apply_single_ui_command(action) {
                Ok(action_effect) => effect.merge(action_effect),
                Err(error) => errors.push(error),
            }
        }
        ApplyActionsReport { effect, errors }
    }

    fn apply_single_ui_command(
        &mut self,
        action: UiCommand,
    ) -> Result<ApplyActionsEffect, AppActionError> {
        match action {
            UiCommand::BrushUpdated(brush_kind, values) => {
                self.apply_brush_action(brush_kind, &values)
            }
            UiCommand::LayerSelected(node_id) => self.apply_layer_select(node_id),
            UiCommand::LayerCreated(kind) => self.apply_layer_create(kind),
            UiCommand::GroupCreated => self.apply_group_create(),
            UiCommand::LayerMoved(node_id, target) => self.apply_layer_move(node_id, target),
            UiCommand::LayerVisibilityChanged(node_id, visible) => {
                self.apply_layer_visibility(node_id, visible)
            }
            UiCommand::LayerOpacityChanged(node_id, opacity) => {
                self.apply_layer_opacity(node_id, opacity)
            }
            UiCommand::LayerBlendModeChanged(node_id, blend_mode) => {
                self.apply_layer_blend_mode(node_id, blend_mode)
            }
            UiCommand::DocumentSaveRequested(path) => self.apply_document_save(path),
            UiCommand::DocumentLoadRequested(path) => self.apply_document_load(path),
            UiCommand::DocumentExportRequested(path) => self.apply_document_export(path),
            UiCommand::ExitConfirmed(action) => self.apply_exit_confirm(action),
            UiCommand::PathDialogCancelled => self.apply_path_dialog_cancel(),
        }
    }

    fn apply_brush_action(
        &mut self,
        brush_kind: BrushKind,
        values: &[brushes::BrushConfigValue],
    ) -> Result<ApplyActionsEffect, AppActionError> {
        match brush_kind {
            BrushKind::Round => match RoundBrush::from_config_values(values) {
                Ok(updated_brush) => {
                    let Some(integration) = self.integration.as_mut() else {
                        return Ok(ApplyActionsEffect::default());
                    };
                    integration
                        .update_brush(brush_kind.brush_id(), updated_brush)
                        .map_err(|e| AppActionError::BrushUpdate(format!("{:?}", e)))?;
                    Ok(ApplyActionsEffect {
                        advance_epoch: true,
                        request_redraw: true,
                    })
                }
                Err(error) => Err(AppActionError::BrushBuild(format!(
                    "round brush: {}",
                    error
                ))),
            },
            BrushKind::PixelRect => match PixelRectBrush::from_config_values(values) {
                Ok(updated_brush) => {
                    let Some(integration) = self.integration.as_mut() else {
                        return Ok(ApplyActionsEffect::default());
                    };
                    integration
                        .update_brush(brush_kind.brush_id(), updated_brush)
                        .map_err(|e| AppActionError::BrushUpdate(format!("{:?}", e)))?;
                    Ok(ApplyActionsEffect {
                        advance_epoch: true,
                        request_redraw: true,
                    })
                }
                Err(error) => Err(AppActionError::BrushBuild(format!(
                    "pixel rect brush: {}",
                    error
                ))),
            },
        }
    }

    fn apply_layer_select(
        &mut self,
        node_id: NodeId,
    ) -> Result<ApplyActionsEffect, AppActionError> {
        let Some(integration) = self.integration.as_mut() else {
            return Ok(ApplyActionsEffect::default());
        };
        integration
            .select_document_node(node_id)
            .then_some(ApplyActionsEffect {
                advance_epoch: true,
                request_redraw: true,
            })
            .ok_or(AppActionError::LayerSelectFailed(node_id))
    }

    fn apply_layer_create(
        &mut self,
        kind: document::NewLayerKind,
    ) -> Result<ApplyActionsEffect, AppActionError> {
        let Some(integration) = self.integration.as_mut() else {
            return Ok(ApplyActionsEffect::default());
        };
        integration
            .create_layer_above_active(kind)
            .inspect(|_| {
                if let Some(overlay) = self.overlay.as_mut() {
                    overlay.mark_document_dirty();
                }
            })
            .map(|()| ApplyActionsEffect {
                advance_epoch: true,
                request_redraw: true,
            })
            .map_err(|e| AppActionError::LayerCreate(format!("{:?}", e)))
    }

    fn apply_group_create(&mut self) -> Result<ApplyActionsEffect, AppActionError> {
        let Some(integration) = self.integration.as_mut() else {
            return Ok(ApplyActionsEffect::default());
        };
        integration
            .create_group_above_active()
            .inspect(|_| {
                if let Some(overlay) = self.overlay.as_mut() {
                    overlay.mark_document_dirty();
                }
            })
            .map(|()| ApplyActionsEffect {
                advance_epoch: true,
                request_redraw: true,
            })
            .map_err(|e| AppActionError::GroupCreate(format!("{:?}", e)))
    }

    fn apply_layer_move(
        &mut self,
        node_id: NodeId,
        target: document::LayerMoveTarget,
    ) -> Result<ApplyActionsEffect, AppActionError> {
        let Some(integration) = self.integration.as_mut() else {
            return Ok(ApplyActionsEffect::default());
        };
        integration
            .move_document_node(node_id, target)
            .inspect(|_| {
                if let Some(overlay) = self.overlay.as_mut() {
                    overlay.mark_document_dirty();
                }
            })
            .map(|()| ApplyActionsEffect {
                advance_epoch: true,
                request_redraw: true,
            })
            .map_err(|e| AppActionError::LayerMove(node_id, format!("{:?}", e)))
    }

    fn apply_layer_visibility(
        &mut self,
        node_id: NodeId,
        visible: bool,
    ) -> Result<ApplyActionsEffect, AppActionError> {
        let Some(integration) = self.integration.as_mut() else {
            return Ok(ApplyActionsEffect::default());
        };
        integration
            .set_document_node_visibility(node_id, visible)
            .inspect(|_| {
                if let Some(overlay) = self.overlay.as_mut() {
                    overlay.mark_document_dirty();
                }
            })
            .map(|()| ApplyActionsEffect {
                advance_epoch: true,
                request_redraw: true,
            })
            .map_err(|e| AppActionError::LayerVisibility(node_id, format!("{:?}", e)))
    }

    fn apply_layer_opacity(
        &mut self,
        node_id: NodeId,
        opacity: f32,
    ) -> Result<ApplyActionsEffect, AppActionError> {
        let Some(integration) = self.integration.as_mut() else {
            return Ok(ApplyActionsEffect::default());
        };
        integration
            .set_document_node_opacity(node_id, opacity)
            .inspect(|_| {
                if let Some(overlay) = self.overlay.as_mut() {
                    overlay.mark_document_dirty();
                }
            })
            .map(|()| ApplyActionsEffect {
                advance_epoch: true,
                request_redraw: true,
            })
            .map_err(|e| AppActionError::LayerOpacity(node_id, format!("{:?}", e)))
    }

    fn apply_layer_blend_mode(
        &mut self,
        node_id: NodeId,
        blend_mode: document::UiBlendMode,
    ) -> Result<ApplyActionsEffect, AppActionError> {
        let Some(integration) = self.integration.as_mut() else {
            return Ok(ApplyActionsEffect::default());
        };
        integration
            .set_document_node_blend_mode(node_id, blend_mode)
            .inspect(|_| {
                if let Some(overlay) = self.overlay.as_mut() {
                    overlay.mark_document_dirty();
                }
            })
            .map(|()| ApplyActionsEffect {
                advance_epoch: true,
                request_redraw: true,
            })
            .map_err(|e| AppActionError::LayerBlendMode(node_id, format!("{:?}", e)))
    }

    fn apply_document_save(
        &mut self,
        path: std::path::PathBuf,
    ) -> Result<ApplyActionsEffect, AppActionError> {
        let Some(integration) = self.integration.as_mut() else {
            return Ok(ApplyActionsEffect::default());
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
                if let Some(overlay) = self.overlay.as_mut() {
                    overlay.set_document_status(format!("Save failed: {}", error), true);
                }
                self.shutdown_after_save = false;
            })
            .map(|()| ApplyActionsEffect {
                advance_epoch: false,
                request_redraw: true,
            })
            .map_err(|error| AppActionError::DocumentSave(path, format!("{:?}", error)))
    }

    fn apply_document_load(
        &mut self,
        path: std::path::PathBuf,
    ) -> Result<ApplyActionsEffect, AppActionError> {
        let Some(integration) = self.integration.as_mut() else {
            return Ok(ApplyActionsEffect::default());
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
                if let Some(overlay) = self.overlay.as_mut() {
                    overlay.set_document_status(format!("Load failed: {}", error), true);
                }
            })
            .map(|()| ApplyActionsEffect {
                advance_epoch: true,
                request_redraw: true,
            })
            .map_err(|error| AppActionError::DocumentLoad(path, format!("{:?}", error)))
    }

    fn apply_document_export(
        &mut self,
        path: std::path::PathBuf,
    ) -> Result<ApplyActionsEffect, AppActionError> {
        let Some(integration) = self.integration.as_mut() else {
            return Ok(ApplyActionsEffect::default());
        };
        integration
            .export_document_jpeg(&path)
            .inspect(|_| {
                if let Some(overlay) = self.overlay.as_mut() {
                    overlay.set_document_status(format!("Exported {}", path.display()), false);
                }
            })
            .inspect_err(|error| {
                if let Some(overlay) = self.overlay.as_mut() {
                    overlay.set_document_status(format!("Export failed: {}", error), true);
                }
            })
            .map(|()| ApplyActionsEffect {
                advance_epoch: false,
                request_redraw: true,
            })
            .map_err(|error| AppActionError::DocumentExport(path, format!("{:?}", error)))
    }

    fn apply_exit_confirm(
        &mut self,
        action: ExitConfirmAction,
    ) -> Result<ApplyActionsEffect, AppActionError> {
        match action {
            ExitConfirmAction::SaveAndExit => {
                self.shutdown_after_save = true;
                if let Some(overlay) = self.overlay.as_mut() {
                    overlay.open_path_dialog(PathDialogAction::Save);
                }
            }
            ExitConfirmAction::DiscardAndExit => {
                self.deferred_shutdown_requested = true;
            }
            ExitConfirmAction::Cancel => {
                self.shutdown_after_save = false;
            }
        }
        Ok(ApplyActionsEffect {
            advance_epoch: false,
            request_redraw: true,
        })
    }

    fn apply_path_dialog_cancel(&mut self) -> Result<ApplyActionsEffect, AppActionError> {
        self.shutdown_after_save = false;
        Ok(ApplyActionsEffect {
            advance_epoch: false,
            request_redraw: true,
        })
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

fn crop_outline_screen_points(
    integration: &AppThreadIntegration,
    document_size: (u32, u32),
) -> [Pos2; 4] {
    let (width, height) = document_size;
    let corners = [
        integration.map_document_to_screen(0.0, 0.0),
        integration.map_document_to_screen(width as f32, 0.0),
        integration.map_document_to_screen(width as f32, height as f32),
        integration.map_document_to_screen(0.0, height as f32),
    ];
    corners.map(|(x, y)| Pos2::new(x, y))
}

fn crop_extent_to_size(value: f32) -> u32 {
    if !value.is_finite() {
        return 1;
    }
    value.max(1.0).ceil().min(u32::MAX as f32) as u32
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

        let mut surface = None;
        if self.integration.is_none() {
            let gpu_init_desc = GpuContextInitDescriptor::default();
            let instance = wgpu::Instance::new(&gpu_init_desc.instance);
            let init_surface = match instance.create_surface(window.clone()) {
                Ok(surface) => surface,
                Err(error) => {
                    eprintln!("failed to create surface: {}", error);
                    event_loop.exit();
                    return;
                }
            };
            let rt = match tokio::runtime::Runtime::new() {
                Ok(rt) => rt,
                Err(error) => {
                    eprintln!("failed to create tokio runtime: {}", error);
                    event_loop.exit();
                    return;
                }
            };
            let init_result = rt.block_on(async move {
                let gpu_context = GpuContext::init_with_instance_and_surface(
                    &gpu_init_desc,
                    instance,
                    Some(&init_surface),
                )
                .await
                .map(std::sync::Arc::new)
                .map_err(app::InitError::GpuContext)?;
                let adapter_info = gpu_context.adapter.get_info();
                eprintln!(
                    "Using GPU adapter: {} ({:?}, {:?})",
                    adapter_info.name, adapter_info.backend, adapter_info.device_type
                );
                let integration = AppThreadIntegration::new_with_gpu_context(
                    "test-document".to_string(),
                    ImageLayout::new(1024, 1024),
                    gpu_context,
                )
                .await?;
                Ok::<_, app::InitError>((integration, init_surface))
            });

            let (mut integration, init_surface) = match init_result {
                Ok(result) => result,
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
            surface = Some(init_surface);
        }

        if let Some(integration) = &mut self.integration {
            let gpu_context = integration.main_state().gpu_context().clone();
            let adapter = &gpu_context.adapter;
            let device = &gpu_context.device;
            let surface = match surface {
                Some(surface) => surface,
                None => match gpu_context.instance.create_surface(window.clone()) {
                    Ok(surface) => surface,
                    Err(error) => {
                        eprintln!("failed to create surface: {}", error);
                        event_loop.exit();
                        return;
                    }
                },
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
                event:
                    KeyEvent {
                        state,
                        logical_key,
                        repeat,
                        ..
                    },
                ..
            } => {
                if ui_event_consumed {
                    return;
                }
                if state == ElementState::Pressed && !repeat {
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
            _ => {
                let (result, needs_redraw) = handle_window_event(self, &event, ui_event_consumed);
                match result {
                    MouseInputResult::StrokeBegan => {
                        let mut effect = ApplyActionsEffect::default();
                        if let Some(overlay) = &mut self.overlay {
                            overlay.flush_selected_brush_if_dirty();
                            let overlay_actions = overlay.take_pending_actions();
                            let report = self.apply_overlay_actions(overlay_actions);
                            for error in report.errors {
                                eprintln!("overlay action errors: {}", error);
                            }
                            effect.merge(report.effect);
                        }
                        if effect.advance_epoch {
                            self.advance_epoch();
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
                    MouseInputResult::StrokeEnded => {
                        if let Some(window) = &self.window {
                            window.request_redraw();
                        }
                    }
                    MouseInputResult::PanStarted | MouseInputResult::PanEnded => {
                        if let Some(window) = &self.window {
                            window.request_redraw();
                        }
                    }
                    MouseInputResult::CanvasCropCommitted => {
                        if let Some(window) = &self.window {
                            window.request_redraw();
                        }
                    }
                    MouseInputResult::None => {
                        if needs_redraw {
                            if let Some(window) = &self.window {
                                window.request_redraw();
                            }
                        }
                    }
                }
            }
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
