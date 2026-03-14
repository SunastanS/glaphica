use crate::components::LayerTree;
use crate::theme::Theme;
use document::{NewLayerKind, UiLayerTreeItem};
use egui::{Frame, SidePanel};
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
                .default_width(220.0)
                .min_width(180.0)
                .max_width(360.0)
                .frame(Frame::default().fill(theme.panel_color))
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.heading("Tools");
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("<").clicked() {
                                output.toggle_collapse = true;
                            }
                        });
                    });
                    ui.separator();

                    ui.horizontal(|ui| {
                        ui.label("Layers");
                        if ui.small_button("+ Raster").clicked() {
                            output.create_layer = Some(NewLayerKind::Raster);
                        }
                        if ui.small_button("+ Solid").clicked() {
                            output.create_layer = Some(NewLayerKind::SolidColor {
                                color: [1.0, 1.0, 1.0, 1.0],
                            });
                        }
                        if ui.small_button("+ Group").clicked() {
                            output.create_group = true;
                        }
                    });

                    ui.horizontal(|ui| {
                        if ui.small_button("Up").clicked() {
                            output.move_layer_up = true;
                        }
                        if ui.small_button("Down").clicked() {
                            output.move_layer_down = true;
                        }
                    });

                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .max_height(280.0)
                        .show(ui, |ui| {
                            let layer_tree =
                                LayerTree::new(self.layer_tree_items, self.selected_node);
                            layer_tree.render(ui, &mut |node_id| {
                                output.select_layer = Some(node_id);
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
    pub move_layer_up: bool,
    pub move_layer_down: bool,
}
