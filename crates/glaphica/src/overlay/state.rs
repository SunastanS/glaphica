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
use crate::components::{ConfigPanel, Sidebar, StatusBar, TopBar};
use crate::egui_renderer::EguiRenderer;
use crate::overlay::actions::{ExitConfirmAction, PathDialogAction};
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
    // Pending action fields
    pending_brush_update: Option<(BrushKind, Vec<brushes::BrushConfigValue>)>,
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
    exit_confirm_action: Option<ExitConfirmAction>,
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
            // Initialize pending fields
            pending_brush_update: None,
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
            exit_confirm_action: None,
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
        self.pending_brush_update.take()
    }

    pub fn take_pending_layer_select(&mut self) -> Option<NodeId> {
        self.pending_layer_select.take()
    }

    pub fn take_pending_layer_create(&mut self) -> Option<NewLayerKind> {
        self.pending_layer_create.take()
    }

    pub fn take_pending_group_create(&mut self) -> bool {
        std::mem::take(&mut self.pending_group_create)
    }

    pub fn take_pending_layer_move(&mut self) -> Option<(NodeId, LayerMoveTarget)> {
        self.pending_layer_move.take()
    }

    pub fn take_pending_layer_visibility(&mut self) -> Option<(NodeId, bool)> {
        self.pending_layer_visibility.take()
    }

    pub fn take_pending_layer_opacity(&mut self) -> Option<(NodeId, f32)> {
        self.pending_layer_opacity.take()
    }

    pub fn take_pending_layer_blend_mode(&mut self) -> Option<(NodeId, UiBlendMode)> {
        self.pending_layer_blend_mode.take()
    }

    pub fn take_pending_document_save(&mut self) -> Option<PathBuf> {
        if std::mem::take(&mut self.pending_document_save) {
            let path = self.document_path.trim();
            if !path.is_empty() {
                return Some(PathBuf::from(path));
            }
        }
        None
    }

    pub fn take_pending_document_load(&mut self) -> Option<PathBuf> {
        if std::mem::take(&mut self.pending_document_load) {
            let path = self.document_path.trim();
            if !path.is_empty() {
                return Some(PathBuf::from(path));
            }
        }
        None
    }

    pub fn take_pending_document_export(&mut self) -> Option<PathBuf> {
        if std::mem::take(&mut self.pending_document_export) {
            let path = self.document_path.trim();
            if !path.is_empty() {
                return Some(PathBuf::from(path));
            }
        }
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
        self.exit_confirm_action.take()
    }

    pub fn flush_selected_brush_if_dirty(&mut self) {
        self.queue_brush_update_if_dirty(self.selected_brush_index);
    }

    fn queue_brush_update_if_dirty(&mut self, index: usize) {
        if self.pending_brush_update.is_some() {
            return;
        }
        if index >= self.brush_states.len() {
            return;
        }
        let brush_state = &mut self.brush_states[index];
        if !brush_state.dirty {
            return;
        }
        brush_state.dirty = false;
        self.pending_brush_update = Some((brush_state.kind, brush_state.values.clone()));
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
    ) {
        let raw_input = self.state.take_egui_input(window);
        let preview_texture_ids: HashMap<_, _> = self
            .texture_cache
            .ids()
            .iter()
            .map(|(node_id, texture)| (*node_id, texture.id()))
            .collect();

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
        let selected_node = &mut self.selected_node;
        let pending_layer_select = &mut self.pending_layer_select;
        let pending_layer_create = &mut self.pending_layer_create;
        let pending_group_create = &mut self.pending_group_create;
        let pending_layer_move = &mut self.pending_layer_move;
        let pending_layer_visibility = &mut self.pending_layer_visibility;
        let pending_layer_opacity = &mut self.pending_layer_opacity;
        let pending_layer_blend_mode = &mut self.pending_layer_blend_mode;
        let _pending_document_save = &mut self.pending_document_save;
        let _pending_document_load = &mut self.pending_document_load;
        let _pending_document_export = &mut self.pending_document_export;
        let exit_confirm_open = &mut self.exit_confirm_open;
        let exit_confirm_action = &mut self.exit_confirm_action;
        let mut config_panel_rect = self.config_panel_rect;
        let app_stats = self.app_stats.clone();
        let _document_dirty = self.document_dirty;
        let document_path = &mut self.document_path;
        let path_dialog_action = &mut self.path_dialog_action;
        let _path_dialog_cancelled = &mut self.path_dialog_cancelled;

        let panel_max_width = (target_width as f32 - 96.0)
            .max(0.0)
            .min(target_width as f32);

        let mut requested_path_dialog: Option<PathDialogAction> = None;
        let mut confirm_path_dialog_flag = false;
        let mut cancel_path_dialog_flag = false;

        let full_output = self.ctx.run(raw_input, |ctx| {
            // Top bar
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

            // Status bar
            StatusBar::render(ctx, &theme, app_stats.as_ref());

            // Sidebar (left panel)
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

            // Config panel (right panel)
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

            // Exit confirmation dialog
            if *exit_confirm_open {
                egui::Window::new("Unsaved Changes")
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .collapsible(false)
                    .resizable(false)
                    .show(ctx, |ui| {
                        ui.label("Save changes before exit?");
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            if ui.button("Save").clicked() {
                                *exit_confirm_open = false;
                                *exit_confirm_action = Some(ExitConfirmAction::SaveAndExit);
                            }
                            if ui.button("Discard").clicked() {
                                *exit_confirm_open = false;
                                *exit_confirm_action = Some(ExitConfirmAction::DiscardAndExit);
                            }
                            if ui.button("Cancel").clicked() {
                                *exit_confirm_open = false;
                                *exit_confirm_action = Some(ExitConfirmAction::Cancel);
                            }
                        });
                    });
            }

            // Path dialog
            if let Some(action) = *path_dialog_action {
                let (title, confirm_label, hint) = match action {
                    PathDialogAction::Save => ("Save Document", "Save", "Enter bundle output path"),
                    PathDialogAction::Load => ("Load Document", "Load", "Enter bundle input path"),
                    PathDialogAction::Export => ("Export JPEG", "Export", "Enter .jpg or .jpeg output path"),
                };
                egui::Window::new(title)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .collapsible(false)
                    .resizable(false)
                    .show(ctx, |ui| {
                        ui.label(hint);
                        ui.add_space(8.0);
                        ui.add(egui::TextEdit::singleline(document_path).desired_width(360.0));
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            if ui.button(confirm_label).clicked() {
                                confirm_path_dialog_flag = true;
                            }
                            if ui.button("Cancel").clicked() {
                                cancel_path_dialog_flag = true;
                            }
                        });
                    });
            }
        });

        // Handle path dialog actions after ctx.run()
        if let Some(action) = requested_path_dialog {
            self.open_path_dialog(action);
        }
        if confirm_path_dialog_flag {
            self.confirm_path_dialog();
        }
        if cancel_path_dialog_flag {
            self.path_dialog_action = None;
            self.path_dialog_cancelled = true;
        }
        self.config_panel_rect = config_panel_rect;

        self.state
            .handle_platform_output(window, full_output.platform_output);

        // Auto-flush brush update when pointer leaves config panel
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

    fn confirm_path_dialog(&mut self) {
        match self.path_dialog_action.take() {
            Some(PathDialogAction::Save) => self.pending_document_save = true,
            Some(PathDialogAction::Load) => self.pending_document_load = true,
            Some(PathDialogAction::Export) => self.pending_document_export = true,
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
