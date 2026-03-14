use crate::theme::Theme;
use document::{UiLayerTreeItem, UiNodeKind};
use egui::{Align2, Color32, CornerRadius, FontId, Sense, Stroke, StrokeKind, Vec2};
use glaphica_core::NodeId;

pub struct LayerTree<'a> {
    items: &'a [UiLayerTreeItem],
    selected_node: Option<NodeId>,
    theme: &'a Theme,
}

impl<'a> LayerTree<'a> {
    pub fn new(
        items: &'a [UiLayerTreeItem],
        selected_node: Option<NodeId>,
        theme: &'a Theme,
    ) -> Self {
        Self {
            items,
            selected_node,
            theme,
        }
    }

    pub fn render(&self, ui: &mut egui::Ui, on_select: &mut impl FnMut(NodeId)) {
        if let [root] = self.items {
            if matches!(root.kind, UiNodeKind::Branch) {
                for child in root.children.iter().rev() {
                    self.render_item(ui, child, 0, on_select);
                }
                return;
            }
        }

        for item in self.items.iter().rev() {
            self.render_item(ui, item, 0, on_select);
        }
    }

    fn render_item(
        &self,
        ui: &mut egui::Ui,
        item: &UiLayerTreeItem,
        depth: usize,
        on_select: &mut impl FnMut(NodeId),
    ) {
        if self.render_row(ui, item, depth).clicked() {
            on_select(item.id);
        }

        if !item.children.is_empty() {
            for child in item.children.iter().rev() {
                self.render_item(ui, child, depth + 1, on_select);
            }
        }
    }

    fn render_row(&self, ui: &mut egui::Ui, item: &UiLayerTreeItem, depth: usize) -> egui::Response {
        let is_selected = self.selected_node == Some(item.id);
        let fill = if is_selected {
            self.theme.hover_color.linear_multiply(1.15)
        } else {
            self.theme.input_bg_color
        };
        let stroke = if is_selected {
            Stroke::new(1.0, self.theme.accent_color)
        } else {
            Stroke::new(1.0, self.theme.border_color)
        };
        let row_height = 38.0;
        let row_width = ui.available_width();
        let (rect, response) =
            ui.allocate_exact_size(Vec2::new(row_width, row_height), Sense::click());
        let painter = ui.painter();

        painter.rect(
            rect,
            CornerRadius::same(6),
            fill,
            stroke,
            StrokeKind::Middle,
        );

        let indent = depth as f32 * 14.0;
        let thumb_rect = egui::Rect::from_min_size(
            rect.min + Vec2::new(8.0 + indent, 7.0),
            Vec2::new(34.0, 24.0),
        );
        self.paint_thumbnail(ui, thumb_rect, item.kind);

        let label_pos = thumb_rect.right_top() + Vec2::new(8.0, 1.0);
        painter.text(
            label_pos,
            Align2::LEFT_TOP,
            &item.label,
            FontId::proportional(13.0),
            self.theme.text_color,
        );
        painter.text(
            label_pos + Vec2::new(0.0, 16.0),
            Align2::LEFT_TOP,
            self.kind_label(item.kind),
            FontId::proportional(11.0),
            self.theme.accent_color,
        );

        response
    }

    fn paint_thumbnail(&self, ui: &egui::Ui, rect: egui::Rect, kind: UiNodeKind) {
        let painter = ui.painter();
        painter.rect(
            rect,
            CornerRadius::same(4),
            self.theme.panel_color,
            Stroke::new(1.0, self.theme.border_color),
            StrokeKind::Middle,
        );

        match kind {
            UiNodeKind::Branch => {
                let top = egui::Rect::from_min_size(
                    rect.min + Vec2::new(4.0, 5.0),
                    Vec2::new(rect.width() - 8.0, 6.0),
                );
                let bottom = egui::Rect::from_min_size(
                    rect.min + Vec2::new(7.0, 13.0),
                    Vec2::new(rect.width() - 14.0, 5.0),
                );
                painter.rect_filled(top, 2.0, self.theme.accent_color);
                painter.rect_filled(bottom, 2.0, self.theme.hover_color);
            }
            UiNodeKind::RasterLayer => {
                let inner = rect.shrink2(Vec2::new(4.0, 4.0));
                painter.rect_filled(inner, 2.0, Color32::from_rgb(104, 138, 176));
                painter.line_segment(
                    [inner.left_top(), inner.right_bottom()],
                    Stroke::new(1.0, Color32::from_white_alpha(70)),
                );
            }
            UiNodeKind::SpecialLayer => {
                let inner = rect.shrink2(Vec2::new(4.0, 4.0));
                painter.rect_filled(inner, 2.0, Color32::from_rgb(214, 157, 86));
                painter.circle_filled(inner.center(), 4.0, Color32::from_white_alpha(120));
            }
        }
    }

    fn kind_label(&self, kind: UiNodeKind) -> &'static str {
        match kind {
            UiNodeKind::Branch => "Branch",
            UiNodeKind::RasterLayer => "Leaf",
            UiNodeKind::SpecialLayer => "Leaf / Special",
        }
    }
}
