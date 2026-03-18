use std::collections::HashMap;

use egui::{Rect, TextureId};
use glaphica_core::NodeId;

use crate::brush_ui::state::BrushUiState;
use crate::components::{ConfigPanel, Sidebar, StatusBar, TopBar};
use crate::overlay::actions::PendingActions;
use crate::theme::Theme;
use document::UiLayerTreeItem;

pub struct Panels<'a> {
    pub left_collapsed: bool,
    pub right_collapsed: bool,
    pub left_width: f32,
    pub right_width: f32,
    pub active_color_rgb: &'a mut [f32; 3],
    pub brush_states: &'a mut [BrushUiState],
    pub selected_brush_index: usize,
    pub layer_tree_items: &'a [UiLayerTreeItem],
    pub selected_node: Option<NodeId>,
    pub preview_texture_ids: &'a HashMap<NodeId, TextureId>,
    pub app_stats: Option<app::AppStats>,
    pub document_dirty: bool,
}

pub struct PanelsOutput {
    pub left_width: f32,
    pub right_width: f32,
    pub config_panel_rect: Option<Rect>,
    pub pending: PendingActions,
    pub requested_path_dialog: Option<crate::overlay::actions::PathDialogAction>,
    pub confirm_path_dialog: bool,
    pub cancel_path_dialog: bool,
    pub exit_confirm_open: bool,
    pub new_selected_brush_index: Option<usize>,
}

impl<'a> Panels<'a> {
    pub fn render(
        self,
        ctx: &egui::Context,
        theme: &Theme,
        target_width: u32,
        document_path: &mut String,
        path_dialog_action: &mut Option<crate::overlay::actions::PathDialogAction>,
        exit_confirm_open: &mut bool,
    ) -> PanelsOutput {
        let mut output = PanelsOutput {
            left_width: self.left_width,
            right_width: self.right_width,
            config_panel_rect: None,
            pending: PendingActions::default(),
            requested_path_dialog: None,
            confirm_path_dialog: false,
            cancel_path_dialog: false,
            exit_confirm_open: *exit_confirm_open,
            new_selected_brush_index: None,
        };

        let panel_max_width = (target_width as f32 - 96.0)
            .max(0.0)
            .min(target_width as f32);

        // Top bar
        let mut top_bar = TopBar::new();
        let top_bar_output = top_bar.render(ctx, theme);
        if top_bar_output.save_clicked {
            output.requested_path_dialog = Some(crate::overlay::actions::PathDialogAction::Save);
        }
        if top_bar_output.load_clicked {
            output.requested_path_dialog = Some(crate::overlay::actions::PathDialogAction::Load);
        }
        if top_bar_output.export_clicked {
            output.requested_path_dialog = Some(crate::overlay::actions::PathDialogAction::Export);
        }

        // Status bar
        StatusBar::render(ctx, theme, self.app_stats.as_ref());

        // Sidebar (left panel)
        let sidebar = Sidebar::new(
            self.left_collapsed,
            self.left_width,
            panel_max_width,
            self.layer_tree_items,
            self.selected_node,
            self.preview_texture_ids,
        );
        let sidebar_output = sidebar.render(ctx, theme);

        if sidebar_output.toggle_collapse {
            output.pending.exit_confirm_action =
                Some(crate::overlay::actions::ExitConfirmAction::Cancel);
        }
        if let Some(kind) = sidebar_output.create_layer {
            output.pending.layer_create = Some(kind);
        }
        if sidebar_output.create_group {
            output.pending.group_create = true;
        }
        if let Some(node_id) = sidebar_output.select_layer {
            output.pending.layer_select = Some(node_id);
        }
        if let Some(layer_move) = sidebar_output.move_layer {
            output.pending.layer_move = Some((layer_move.node_id, layer_move.target));
        }
        if let Some((node_id, visible)) = sidebar_output.set_layer_visibility {
            output.pending.layer_visibility = Some((node_id, visible));
        }
        if let Some((node_id, opacity)) = sidebar_output.set_layer_opacity {
            output.pending.layer_opacity = Some((node_id, opacity));
        }
        if let Some((node_id, blend_mode)) = sidebar_output.set_layer_blend_mode {
            output.pending.layer_blend_mode = Some((node_id, blend_mode));
        }
        if let Some(rect) = sidebar_output.panel_rect {
            output.left_width = rect.width();
        }

        // Config panel (right panel)
        let mut config_panel = ConfigPanel::new(
            self.right_collapsed,
            self.right_width,
            panel_max_width,
            self.active_color_rgb,
            self.brush_states,
            self.selected_brush_index,
        );
        let config_output = config_panel.render(ctx, theme);

        if config_output.toggle_collapse {
            output.pending.exit_confirm_action =
                Some(crate::overlay::actions::ExitConfirmAction::Cancel);
        }
        if let Some((kind, values)) = config_output.pending_brush_update {
            output.pending.brush_update = Some((kind, values));
        }
        if config_output.brush_selection_changed {
            output.new_selected_brush_index = config_output.new_selected_index;
        }
        output.config_panel_rect = config_output.panel_rect;
        if let Some(rect) = config_output.panel_rect {
            output.right_width = rect.width();
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
                            output.pending.exit_confirm_action =
                                Some(crate::overlay::actions::ExitConfirmAction::SaveAndExit);
                        }
                        if ui.button("Discard").clicked() {
                            *exit_confirm_open = false;
                            output.pending.exit_confirm_action =
                                Some(crate::overlay::actions::ExitConfirmAction::DiscardAndExit);
                        }
                        if ui.button("Cancel").clicked() {
                            *exit_confirm_open = false;
                            output.pending.exit_confirm_action =
                                Some(crate::overlay::actions::ExitConfirmAction::Cancel);
                        }
                    });
                });
        }

        // Path dialog
        if let Some(action) = path_dialog_action {
            let (title, confirm_label, hint) = match action {
                crate::overlay::actions::PathDialogAction::Save => {
                    ("Save Document", "Save", "Enter bundle output path")
                }
                crate::overlay::actions::PathDialogAction::Load => {
                    ("Load Document", "Load", "Enter bundle input path")
                }
                crate::overlay::actions::PathDialogAction::Export => {
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
                            output.confirm_path_dialog = true;
                        }
                        if ui.button("Cancel").clicked() {
                            output.cancel_path_dialog = true;
                        }
                    });
                });
        }

        output
    }
}
