use crate::components::{LayerTree, LayerTreeMove};
use crate::theme::Theme;
use document::{NewLayerKind, UiLayerTreeItem};
use egui::{Button, Color32, CornerRadius, Frame, Rect, RichText, SidePanel, Stroke};
use glaphica_core::NodeId;
use std::collections::HashMap;

pub const LEFT_PANEL_COMPACT_WIDTH: f32 = 120.0;
const LEFT_PANEL_DRAG_MIN_WIDTH: f32 = 28.0;
const COLLAPSED_PANEL_WIDTH: f32 = 28.0;

pub struct Sidebar<'a> {
    collapsed: bool,
    width: f32,
    max_width: f32,
    layer_tree_items: &'a [UiLayerTreeItem],
    selected_node: Option<NodeId>,
    preview_texture_ids: &'a HashMap<NodeId, egui::TextureId>,
}

impl<'a> Sidebar<'a> {
    pub fn new(
        collapsed: bool,
        width: f32,
        max_width: f32,
        layer_tree_items: &'a [UiLayerTreeItem],
        selected_node: Option<NodeId>,
        preview_texture_ids: &'a HashMap<NodeId, egui::TextureId>,
    ) -> Self {
        Self {
            collapsed,
            width,
            max_width,
            layer_tree_items,
            selected_node,
            preview_texture_ids,
        }
    }

    pub fn render(&self, ctx: &egui::Context, theme: &Theme) -> SidebarOutput {
        let mut output = SidebarOutput::default();
        let panel_fill = translucent_panel_fill(theme);

        if self.collapsed {
            SidePanel::left("overlay-left-panel-collapsed")
                .resizable(false)
                .exact_width(COLLAPSED_PANEL_WIDTH)
                .frame(Frame::default().fill(panel_fill))
                .show(ctx, |ui| {
                    if ui.button(">").clicked() {
                        output.toggle_collapse = true;
                    }
                });
        } else {
            let compact = self.width <= LEFT_PANEL_COMPACT_WIDTH;
            let panel = SidePanel::left("overlay-left-panel")
                .resizable(true)
                .default_width(self.width)
                .min_width(LEFT_PANEL_DRAG_MIN_WIDTH)
                .max_width(self.max_width)
                .frame(Frame::default().fill(panel_fill))
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        if !compact {
                            ui.heading("Layers");
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("<").clicked() {
                                output.toggle_collapse = true;
                            }
                            ui.menu_button(
                                RichText::new("+").size(16.0).color(theme.text_color),
                                |ui| {
                                    if ui
                                        .add_sized(
                                            [120.0, 26.0],
                                            Button::new("New Layer").fill(theme.input_bg_color),
                                        )
                                        .clicked()
                                    {
                                        output.create_layer = Some(NewLayerKind::Raster);
                                        ui.close();
                                    }
                                    if ui
                                        .add_sized(
                                            [120.0, 26.0],
                                            Button::new("Solid Layer").fill(theme.input_bg_color),
                                        )
                                        .clicked()
                                    {
                                        output.create_layer = Some(NewLayerKind::SolidColor {
                                            color: [1.0, 1.0, 1.0, 1.0],
                                        });
                                        ui.close();
                                    }
                                    if ui
                                        .add_sized(
                                            [120.0, 26.0],
                                            Button::new("Layer Group").fill(theme.input_bg_color),
                                        )
                                        .clicked()
                                    {
                                        output.create_group = true;
                                        ui.close();
                                    }
                                },
                            );
                        });
                    });
                    ui.add_space(8.0);

                    Frame::new()
                        .fill(Color32::TRANSPARENT)
                        .stroke(Stroke::new(1.0, theme.border_color))
                        .corner_radius(CornerRadius::same(6))
                        .inner_margin(egui::Margin::same(8))
                        .show(ui, |ui| {
                            egui::ScrollArea::vertical()
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    let layer_tree = LayerTree::new(
                                        self.layer_tree_items,
                                        self.selected_node,
                                        self.preview_texture_ids,
                                        theme,
                                        compact,
                                    );
                                    let tree_output = layer_tree.render(ui);
                                    if let Some(node_id) = tree_output.select_node {
                                        output.select_layer = Some(node_id);
                                    }
                                    output.move_layer = tree_output.move_node;
                                });
                        });
                });

            output.panel_rect = Some(panel.response.rect);
        }

        output
    }
}

#[derive(Default)]
pub struct SidebarOutput {
    pub toggle_collapse: bool,
    pub create_layer: Option<NewLayerKind>,
    pub create_group: bool,
    pub select_layer: Option<NodeId>,
    pub move_layer: Option<LayerTreeMove>,
    pub panel_rect: Option<Rect>,
}

fn translucent_panel_fill(theme: &Theme) -> Color32 {
    Color32::from_rgba_unmultiplied(
        theme.panel_color.r(),
        theme.panel_color.g(),
        theme.panel_color.b(),
        208,
    )
}
