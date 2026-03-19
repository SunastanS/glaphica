use std::collections::HashMap;
use std::path::{Path, PathBuf};

use app::{AppStats, LayerPreviewBitmap};
use document::UiLayerTreeItem;
use egui::{Color32, Pos2, Rect, Stroke, StrokeKind, Vec2};
use egui_winit::EventResponse;
use glaphica_core::NodeId;
use winit::{event::WindowEvent, event_loop::ActiveEventLoop, window::Window};

use crate::brush_ui::state::{BrushKind, BrushUiState};
use crate::components::{ConfigPanel, Sidebar, StatusBar, TopBar};
use crate::egui_renderer::EguiRenderer;
use crate::overlay::actions::{ExitConfirmAction, PathDialogAction, UiCommand};
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
    pub document_status_text: Option<String>,
    pub document_status_is_error: bool,
    pub document_dirty: bool,
    pub exit_confirm_open: bool,
    pub config_panel_rect: Option<Rect>,
    pub app_stats: Option<AppStats>,
    pub canvas_crop_mode_active: bool,
    pub canvas_crop_outline: Option<[Pos2; 4]>,
    pub canvas_crop_handle_center: Option<Pos2>,
    pub canvas_crop_dragging: bool,
    pending_actions: Vec<UiCommand>,
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
            document_status_text: None,
            document_status_is_error: false,
            document_dirty: false,
            exit_confirm_open: false,
            config_panel_rect: None,
            app_stats: None,
            canvas_crop_mode_active: false,
            canvas_crop_outline: None,
            canvas_crop_handle_center: None,
            canvas_crop_dragging: false,
            pending_actions: Vec::new(),
        }
    }

    pub fn set_app_stats(&mut self, stats: AppStats) {
        self.app_stats = Some(stats);
    }

    pub fn on_window_event(&mut self, window: &Window, event: &WindowEvent) -> EventResponse {
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
        self.texture_cache
            .retain(|node_id| valid_ids.contains(node_id));
    }

    pub fn take_pending_actions(&mut self) -> Vec<UiCommand> {
        std::mem::take(&mut self.pending_actions)
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

    pub fn canvas_crop_mode_active(&self) -> bool {
        self.canvas_crop_mode_active
    }

    pub fn set_canvas_crop_overlay(
        &mut self,
        outline: Option<[Pos2; 4]>,
        handle_center: Option<Pos2>,
        dragging: bool,
    ) {
        self.canvas_crop_outline = outline;
        self.canvas_crop_handle_center = handle_center;
        self.canvas_crop_dragging = dragging;
    }

    pub fn canvas_crop_handle_center(&self) -> Option<Pos2> {
        self.canvas_crop_handle_center
    }

    pub fn flush_selected_brush_if_dirty(&mut self) {
        self.queue_brush_update_if_dirty(self.selected_brush_index);
    }

    fn queue_brush_update_if_dirty(&mut self, index: usize) {
        if index >= self.brush_states.len() {
            return;
        }
        let brush_state = &mut self.brush_states[index];
        if !brush_state.dirty {
            return;
        }
        brush_state.dirty = false;
        self.pending_actions.push(UiCommand::BrushUpdated(
            brush_state.kind,
            brush_state.values.clone(),
        ));
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
        let pending_actions = &mut self.pending_actions;
        let layer_tree_items = &self.layer_tree_items;
        let selected_node = &mut self.selected_node;
        let exit_confirm_open = &mut self.exit_confirm_open;
        let mut config_panel_rect = self.config_panel_rect;
        let app_stats = self.app_stats.clone();
        let _document_dirty = self.document_dirty;
        let document_path = &mut self.document_path;
        let path_dialog_action = &mut self.path_dialog_action;

        let panel_max_width = (target_width as f32 - 96.0)
            .max(0.0)
            .min(target_width as f32);

        let mut requested_path_dialog: Option<PathDialogAction> = None;
        let mut confirm_path_dialog_flag = false;
        let mut cancel_path_dialog_flag = false;

        let full_output = self.ctx.run(raw_input, |ctx| {
            // Top bar
            let mut top_bar = TopBar::new();
            let top_bar_output = top_bar.render(ctx, &theme, self.canvas_crop_mode_active);
            if top_bar_output.toggle_canvas_crop_mode {
                self.canvas_crop_mode_active = !self.canvas_crop_mode_active;
            }
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
                pending_actions.push(UiCommand::LayerCreated(kind));
            }
            if sidebar_output.create_group {
                pending_actions.push(UiCommand::GroupCreated);
            }
            if let Some(node_id) = sidebar_output.select_layer {
                pending_actions.push(UiCommand::LayerSelected(node_id));
            }
            if let Some(layer_move) = sidebar_output.move_layer {
                pending_actions.push(UiCommand::LayerMoved(layer_move.node_id, layer_move.target));
            }
            if let Some((node_id, visible)) = sidebar_output.set_layer_visibility {
                pending_actions.push(UiCommand::LayerVisibilityChanged(node_id, visible));
            }
            if let Some((node_id, opacity)) = sidebar_output.set_layer_opacity {
                pending_actions.push(UiCommand::LayerOpacityChanged(node_id, opacity));
            }
            if let Some((node_id, blend_mode)) = sidebar_output.set_layer_blend_mode {
                pending_actions.push(UiCommand::LayerBlendModeChanged(node_id, blend_mode));
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
                pending_actions.push(UiCommand::BrushUpdated(kind, values));
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
                                pending_actions
                                    .push(UiCommand::ExitConfirmed(ExitConfirmAction::SaveAndExit));
                            }
                            if ui.button("Discard").clicked() {
                                *exit_confirm_open = false;
                                pending_actions.push(UiCommand::ExitConfirmed(
                                    ExitConfirmAction::DiscardAndExit,
                                ));
                            }
                            if ui.button("Cancel").clicked() {
                                *exit_confirm_open = false;
                                pending_actions
                                    .push(UiCommand::ExitConfirmed(ExitConfirmAction::Cancel));
                            }
                        });
                    });
            }

            // Path dialog
            if let Some(action) = *path_dialog_action {
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
            self.pending_actions.push(UiCommand::PathDialogCancelled);
        }
        self.config_panel_rect = config_panel_rect;

        self.state
            .handle_platform_output(window, full_output.platform_output);

        if self.canvas_crop_mode_active {
            self.paint_canvas_crop_overlay();
        }

        // Auto-flush brush update when pointer leaves config panel
        let pointer_pos = self.ctx.input(|input| input.pointer.latest_pos());
        if let (Some(rect), Some(pointer_pos)) = (config_panel_rect, pointer_pos)
            && !rect.contains(pointer_pos)
            && let Some(brush_state) = self.brush_states.get(self.selected_brush_index)
            && brush_state.dirty
        {
            self.queue_brush_update_if_dirty(self.selected_brush_index);
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

    fn paint_canvas_crop_overlay(&self) {
        let Some(outline) = self.canvas_crop_outline else {
            return;
        };
        let layer = egui::LayerId::new(egui::Order::Foreground, egui::Id::new("canvas-crop"));
        let painter = self.ctx.layer_painter(layer);
        let outer_color = self.theme.accent_color.linear_multiply(0.18);
        let inner_color = if self.canvas_crop_dragging {
            Color32::from_rgb(124, 196, 255)
        } else {
            self.theme.accent_color
        };

        for index in 0..outline.len() {
            let start = outline[index];
            let end = outline[(index + 1) % outline.len()];
            painter.line_segment([start, end], Stroke::new(10.0, outer_color));
            painter.line_segment([start, end], Stroke::new(2.5, inner_color));
        }

        let Some(handle_center) = self.canvas_crop_handle_center else {
            return;
        };
        let handle_rect = Rect::from_center_size(handle_center, Vec2::splat(16.0));
        painter.rect_filled(handle_rect, 4.0, inner_color);
        painter.rect_stroke(
            handle_rect.expand(3.0),
            6.0,
            Stroke::new(4.0, outer_color),
            StrokeKind::Outside,
        );
    }

    fn confirm_path_dialog(&mut self) {
        let path = self.document_path.trim();
        if path.is_empty() {
            self.path_dialog_action = None;
            return;
        }
        match self.path_dialog_action.take() {
            Some(PathDialogAction::Save) => self
                .pending_actions
                .push(UiCommand::DocumentSaveRequested(PathBuf::from(path))),
            Some(PathDialogAction::Load) => self
                .pending_actions
                .push(UiCommand::DocumentLoadRequested(PathBuf::from(path))),
            Some(PathDialogAction::Export) => self
                .pending_actions
                .push(UiCommand::DocumentExportRequested(PathBuf::from(path))),
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
