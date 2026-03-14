use document::{UiLayerTreeItem, UiNodeKind};
use glaphica_core::NodeId;

pub struct LayerTree<'a> {
    items: &'a [UiLayerTreeItem],
    selected_node: Option<NodeId>,
}

impl<'a> LayerTree<'a> {
    pub fn new(items: &'a [UiLayerTreeItem], selected_node: Option<NodeId>) -> Self {
        Self {
            items,
            selected_node,
        }
    }

    pub fn render(&self, ui: &mut egui::Ui, on_select: &mut impl FnMut(NodeId)) {
        for item in self.items.iter().rev() {
            self.render_item(ui, item, on_select);
        }
    }

    fn render_item(
        &self,
        ui: &mut egui::Ui,
        item: &UiLayerTreeItem,
        on_select: &mut impl FnMut(NodeId),
    ) {
        let prefix = match item.kind {
            UiNodeKind::Branch => "[G] ",
            UiNodeKind::RasterLayer => "",
            UiNodeKind::SpecialLayer => "[S] ",
        };
        let label = format!("{prefix}{}", item.label);

        if item.children.is_empty() {
            if ui
                .selectable_label(self.selected_node == Some(item.id), label)
                .clicked()
            {
                on_select(item.id);
            }
            return;
        }

        egui::CollapsingHeader::new(label)
            .default_open(true)
            .show(ui, |ui| {
                if ui
                    .selectable_label(self.selected_node == Some(item.id), "Select Group")
                    .clicked()
                {
                    on_select(item.id);
                }
                for child in &item.children {
                    self.render_item(ui, child, on_select);
                }
            });
    }
}
