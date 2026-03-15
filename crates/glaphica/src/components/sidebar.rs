use crate::components::{LayerTree, LayerTreeMove};
use crate::theme::Theme;
use document::{NewLayerKind, UiLayerTreeItem};
use egui::{Button, CornerRadius, Frame, RichText, SidePanel, Stroke};
use glaphica_core::NodeId;

pub struct Sidebar<'a> {
    collapsed: bool,
    layer_tree_items: &'a [UiLayerTreeItem],
    selected_node: Option<NodeId>,
}

impl<'a> Sidebar<'a> {
    pub fn new(
        collapsed: bool,
        layer_tree_items: &'a [UiLayerTreeItem],
        selected_node: Option<NodeId>,
    ) -> Self {
        Self {
            collapsed,
            layer_tree_items,
            selected_node,
        }
    }

    pub fn render(&self, ctx: &egui::Context, theme: &Theme) -> SidebarOutput {
        let mut output = SidebarOutput::default();

        if self.collapsed {
            SidePanel::left("overlay-left-panel-collapsed")
                .resizable(false)
                .exact_width(28.0)
                .frame(Frame::default().fill(theme.panel_color))
                .show(ctx, |ui| {
                    if ui.button(">").clicked() {
                        output.toggle_collapse = true;
                    }
                });
        } else {
            SidePanel::left("overlay-left-panel")
                .resizable(true)
                .default_width(280.0)
                .min_width(220.0)
                .max_width(420.0)
                .frame(Frame::default().fill(theme.panel_color))
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.heading("Layers");
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("<").clicked() {
                                output.toggle_collapse = true;
                            }
                        });
                    });
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new("Top-most layers stay at the top of the tree.")
                            .size(11.0)
                            .color(theme.text_color),
                    );
                    ui.add_space(10.0);

                    Frame::new()
                        .fill(theme.bg_color)
                        .stroke(Stroke::new(1.0, theme.border_color))
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(egui::Margin::same(10))
                        .show(ui, |ui| {
                            ui.label(
                                RichText::new("Add")
                                    .size(12.0)
                                    .color(theme.accent_color)
                                    .strong(),
                            );
                            ui.add_space(6.0);
                            ui.horizontal(|ui| {
                                if ui
                                    .add_sized(
                                        [72.0, 26.0],
                                        Button::new("Raster").fill(theme.input_bg_color),
                                    )
                                    .clicked()
                                {
                                    output.create_layer = Some(NewLayerKind::Raster);
                                }
                                if ui
                                    .add_sized(
                                        [72.0, 26.0],
                                        Button::new("Solid").fill(theme.input_bg_color),
                                    )
                                    .clicked()
                                {
                                    output.create_layer = Some(NewLayerKind::SolidColor {
                                        color: [1.0, 1.0, 1.0, 1.0],
                                    });
                                }
                                if ui
                                    .add_sized(
                                        [72.0, 26.0],
                                        Button::new("Group").fill(theme.input_bg_color),
                                    )
                                    .clicked()
                                {
                                    output.create_group = true;
                                }
                            });

                            ui.add_space(10.0);
                            ui.add_space(4.0);
                            ui.label(
                                RichText::new("Drag the handle on the right to reorder or regroup.")
                                    .size(11.0)
                                    .color(theme.text_color),
                            );
                        });

                    ui.add_space(10.0);

                    Frame::new()
                        .fill(theme.bg_color)
                        .stroke(Stroke::new(1.0, theme.border_color))
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(egui::Margin::same(10))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new("Tree")
                                        .size(12.0)
                                        .color(theme.accent_color)
                                        .strong(),
                                );
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        ui.label(
                                            RichText::new("Leaf / Branch thumbnails reserved")
                                                .size(11.0)
                                                .color(theme.text_color),
                                        );
                                    },
                                );
                            });
                            ui.add_space(8.0);

                            egui::ScrollArea::vertical()
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    let layer_tree = LayerTree::new(
                                        self.layer_tree_items,
                                        self.selected_node,
                                        theme,
                                    );
                                    let tree_output = layer_tree.render(ui);
                                    if let Some(node_id) = tree_output.select_node {
                                        output.select_layer = Some(node_id);
                                    }
                                    output.move_layer = tree_output.move_node;
                                });
                        });
                });
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
}
