use std::collections::HashMap;
use std::path::{Path, PathBuf};

use app::{AppStats, LayerPreviewBitmap};
use document::{LayerMoveTarget, NewLayerKind, UiBlendMode, UiLayerTreeItem};
use egui::Rect;
use egui_winit::EventResponse;
use glaphica_core::NodeId;
use winit::{
    event::WindowEvent,
    event_loop::ActiveEventLoop,
    window::Window,
};

use crate::brush_ui::state::{BrushKind, BrushUiState};
use crate::egui_renderer::EguiRenderer;
use crate::overlay::actions::{ExitConfirmAction, PathDialogAction, PendingActions};
use crate::overlay::panels::Panels;
use crate::overlay::texture_cache::LayerTextureCache;
use crate::theme::Theme;

pub struct EguiOverlay {
    pub ctx: egui::Context,
    pub state: egui_winit::State,
    pub renderer: EguiRenderer,
    pub theme: Theme,
    pub left_panel_collapsed: bool,
    pub right_panel_collapsed: bool,
    pub active_color_rgb: [f32; 3],
    pub left_panel_width: f32,
    pub right_panel_width: f32,
    pub brush_states: Vec<BrushUiState>,
    pub selected_brush_index: usize,
    pub layer_tree_items: Vec<UiLayerTreeItem>,
    pub selected_node: Option<NodeId>,
    pub texture_cache: LayerTextureCache,
    pub document_path: String,
    pub path_dialog_action: Option<PathDialogAction>,
    pub path_dialog_cancelled: bool,
    pub document_status_text: Option<String>,
    pub document_status_is_error: bool,
    pub document_dirty: bool,
    pub exit_confirm_open: bool,
    pub config_panel_rect: Option<Rect>,
    pub app_stats: Option<AppStats>,
}

impl EguiOverlay {
    pub fn new(
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
            layer_tree_items: Vec::new(),
            selected_node: None,
            texture_cache: LayerTextureCache::new(),
            document_path,
            path_dialog_action: None,
            path_dialog_cancelled: false,
            document_status_text: None,
            document_status_is_error: false,
            document_dirty: false,
            exit_confirm_open: false,
            config_panel_rect: None,
            app_stats: None,
        }
    }

    pub fn set_app_stats(&mut self, stats: AppStats) {
        self.app_stats = Some(stats);
    }

    pub fn on_window_event(
        &mut self,
        window: &Window,
        event: &WindowEvent,
    ) -> EventResponse {
        self.state.on_window_event(window, event)
    }

    pub fn selected_brush_kind(&self) -> BrushKind {
        self.brush_states
            .get(self.selected_brush_index)
            .map(|state| state.kind)
            .unwrap_or(BrushKind::Round)
    }

    pub fn selected_brush_color_rgb(&self) -> [f32; 3] {
        self.active_color_rgb
    }

    pub fn selected_brush_erase(&self) -> bool {
        self.brush_states
            .get(self.selected_brush_index)
            .map(|state| state.eraser)
            .unwrap_or(false)
    }

    pub fn sync_layer_tree(
        &mut self,
        layer_tree_items: Vec<UiLayerTreeItem>,
        selected_node: Option<NodeId>,
        preview_updates: Vec<LayerPreviewBitmap>,
    ) {
        self.layer_tree_items = layer_tree_items;
        self.selected_node = selected_node;
        self.texture_cache.update(&self.ctx, preview_updates);
        let valid_ids = collect_layer_tree_ids(&self.layer_tree_items);
        self.texture_cache.retain(|node_id| valid_ids.contains(node_id));
    }

    pub fn take_pending_brush_update(&mut self) -> Option<(BrushKind, Vec<brushes::BrushConfigValue>)> {
        None
    }

    pub fn take_pending_layer_select(&mut self) -> Option<NodeId> {
        None
    }

    pub fn take_pending_layer_create(&mut self) -> Option<NewLayerKind> {
        None
    }

    pub fn take_pending_group_create(&mut self) -> bool {
        false
    }

    pub fn take_pending_layer_move(&mut self) -> Option<(NodeId, LayerMoveTarget)> {
        None
    }

    pub fn take_pending_layer_visibility(&mut self) -> Option<(NodeId, bool)> {
        None
    }

    pub fn take_pending_layer_opacity(&mut self) -> Option<(NodeId, f32)> {
        None
    }

    pub fn take_pending_layer_blend_mode(&mut self) -> Option<(NodeId, UiBlendMode)> {
        None
    }

    pub fn take_pending_document_save(&mut self) -> Option<PathBuf> {
        None
    }

    pub fn take_pending_document_load(&mut self) -> Option<PathBuf> {
        None
    }

    pub fn take_pending_document_export(&mut self) -> Option<PathBuf> {
        None
    }

    pub fn take_path_dialog_cancelled(&mut self) -> bool {
        std::mem::take(&mut self.path_dialog_cancelled)
    }

    pub fn set_document_status(&mut self, text: String, is_error: bool) {
        self.document_status_text = Some(text);
        self.document_status_is_error = is_error;
    }

    pub fn mark_document_dirty(&mut self) {
        self.document_dirty = true;
    }

    pub fn mark_document_clean(&mut self) {
        self.document_dirty = false;
    }

    pub fn take_exit_confirm_action(&mut self) -> Option<ExitConfirmAction> {
        None
    }

    pub fn flush_selected_brush_if_dirty(&mut self) {
        self.queue_brush_update_if_dirty(self.selected_brush_index);
    }

    fn queue_brush_update_if_dirty(&mut self, index: usize) {
        if index >= self.brush_states.len() {
            return;
        }
        let brush_state = &mut self.brush_states[index];
        if brush_state.dirty {
            brush_state.dirty = false;
        }
    }

    pub fn open_path_dialog(&mut self, action: PathDialogAction) {
        self.document_path = self.suggested_path_for_action(action);
        self.path_dialog_action = Some(action);
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
        let file_name = format!("{}.{}", file_stem, extension);
        match current.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent.join(file_name),
            _ => PathBuf::from(file_name),
        }
        .display()
        .to_string()
    }

    pub fn paint(
        &mut self,
        window: &Window,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target_view: &wgpu::TextureView,
        _target_format: wgpu::TextureFormat,
        target_width: u32,
        target_height: u32,
    ) -> PendingActions {
        let raw_input = self.state.take_egui_input(window);
        let preview_texture_ids: HashMap<_, _> = self
            .texture_cache
            .ids()
            .iter()
            .map(|(node_id, texture)| (*node_id, texture.id()))
            .collect();

        let mut exit_confirm_open = self.exit_confirm_open;
        let mut document_path = self.document_path.clone();
        let mut path_dialog_action = self.path_dialog_action;

        let panels = Panels {
            left_collapsed: self.left_panel_collapsed,
            right_collapsed: self.right_panel_collapsed,
            left_width: self.left_panel_width,
            right_width: self.right_panel_width,
            active_color_rgb: &mut self.active_color_rgb,
            brush_states: &mut self.brush_states,
            selected_brush_index: self.selected_brush_index,
            layer_tree_items: &self.layer_tree_items,
            selected_node: self.selected_node,
            preview_texture_ids: &preview_texture_ids,
            app_stats: self.app_stats.clone(),
            document_dirty: self.document_dirty,
        };

        let output = panels.render(
            &self.ctx,
            &self.theme,
            target_width,
            &mut document_path,
            &mut path_dialog_action,
            &mut exit_confirm_open,
        );

        self.left_panel_width = output.left_width;
        self.right_panel_width = output.right_width;
        self.config_panel_rect = output.config_panel_rect;
        self.document_path = document_path;
        self.path_dialog_action = path_dialog_action;
        self.exit_confirm_open = output.exit_confirm_open;

        if let Some(index) = output.new_selected_brush_index {
            self.selected_brush_index = index;
        }

        // Handle path dialog actions
        if let Some(action) = output.requested_path_dialog {
            self.open_path_dialog(action);
        }
        if output.confirm_path_dialog {
            self.confirm_path_dialog();
        }
        if output.cancel_path_dialog {
            self.path_dialog_action = None;
            self.path_dialog_cancelled = true;
        }

        // Auto-flush brush update when pointer leaves config panel
        let pointer_pos = self.ctx.input(|input| input.pointer.latest_pos());
        if output.pending.brush_update.is_none()
            && let (Some(rect), Some(pointer_pos)) = (output.config_panel_rect, pointer_pos)
            && !rect.contains(pointer_pos)
            && let Some(brush_state) = self.brush_states.get(self.selected_brush_index)
            && brush_state.dirty
        {
            let brush_state = &mut self.brush_states[self.selected_brush_index];
            brush_state.dirty = false;
        }

        let full_output = self
            .ctx
            .run(raw_input, |_ctx| {});

        self.state
            .handle_platform_output(window, full_output.platform_output);

        if target_width == 0 || target_height == 0 {
            return output.pending;
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

        output.pending
    }

    fn confirm_path_dialog(&mut self) {
        match self.path_dialog_action.take() {
            Some(PathDialogAction::Save) => {}
            Some(PathDialogAction::Load) => {}
            Some(PathDialogAction::Export) => {}
            None => {}
        }
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
