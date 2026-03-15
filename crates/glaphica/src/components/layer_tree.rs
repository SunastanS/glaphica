use crate::theme::Theme;
use document::{LayerMoveTarget, UiLayerTreeItem, UiNodeKind};
use egui::{Align2, Color32, CornerRadius, FontId, Rect, Sense, Stroke, StrokeKind, Vec2};
use glaphica_core::NodeId;

const ROW_HEIGHT: f32 = 38.0;
const THUMB_SIZE: Vec2 = Vec2::new(34.0, 24.0);
const HANDLE_SIZE: Vec2 = Vec2::new(18.0, 24.0);

pub struct LayerTree<'a> {
    items: &'a [UiLayerTreeItem],
    selected_node: Option<NodeId>,
    theme: &'a Theme,
}

#[derive(Default)]
pub struct LayerTreeOutput {
    pub select_node: Option<NodeId>,
    pub move_node: Option<LayerTreeMove>,
}

#[derive(Clone, Copy)]
pub struct LayerTreeMove {
    pub node_id: NodeId,
    pub target: LayerMoveTarget,
}

#[derive(Clone)]
struct VisibleRow<'a> {
    item: &'a UiLayerTreeItem,
    parent_id: NodeId,
    sibling_index: usize,
    depth: usize,
    ancestors: Vec<NodeId>,
}

enum DropIndicator {
    InsertLine { x_start: f32, y: f32 },
    HighlightGroup { rect: Rect },
}

struct DropCandidate {
    target: LayerMoveTarget,
    indicator: DropIndicator,
}

struct RowRender {
    rect: Rect,
    row_response: egui::Response,
    handle_response: egui::Response,
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

    pub fn render(&self, ui: &mut egui::Ui) -> LayerTreeOutput {
        let mut output = LayerTreeOutput::default();
        let Some(root) = self.items.first() else {
            return output;
        };

        let mut rows = Vec::new();
        if matches!(root.kind, UiNodeKind::Branch) {
            let ancestors = Vec::new();
            for (index, child) in root.children.iter().enumerate().rev() {
                self.collect_rows(child, root.id, index, 0, &ancestors, &mut rows);
            }
        } else {
            for (index, item) in self.items.iter().enumerate().rev() {
                self.collect_rows(item, root.id, index, 0, &Vec::new(), &mut rows);
            }
        }

        let drag_id = ui.id().with("layer_tree_drag_node");
        let mut dragging_node = ui.ctx().data(|data| data.get_temp::<NodeId>(drag_id));
        let pointer_pos = ui.ctx().input(|input| input.pointer.hover_pos());
        let any_released = ui.ctx().input(|input| input.pointer.any_released());
        let mut active_drop: Option<DropCandidate> = None;

        for row in rows {
            let render = self.render_row(ui, &row, dragging_node);
            if render.row_response.clicked() {
                output.select_node = Some(row.item.id);
            }
            if render.handle_response.drag_started() {
                ui.ctx().data_mut(|data| data.insert_temp(drag_id, row.item.id));
                dragging_node = Some(row.item.id);
                output.select_node = Some(row.item.id);
            }
            if let (Some(node_id), Some(pointer_pos)) = (dragging_node, pointer_pos)
                && render.rect.contains(pointer_pos)
                && let Some(candidate) = self.drop_candidate(&row, render.rect, pointer_pos, node_id)
            {
                self.paint_drop_indicator(ui, &candidate.indicator);
                active_drop = Some(candidate);
            }
        }

        if let Some(node_id) = dragging_node
            && any_released
        {
            ui.ctx().data_mut(|data| data.remove::<NodeId>(drag_id));
            if let Some(candidate) = active_drop {
                output.select_node = Some(node_id);
                output.move_node = Some(LayerTreeMove {
                    node_id,
                    target: candidate.target,
                });
            }
        }

        output
    }

    fn collect_rows(
        &self,
        item: &'a UiLayerTreeItem,
        parent_id: NodeId,
        sibling_index: usize,
        depth: usize,
        ancestors: &[NodeId],
        rows: &mut Vec<VisibleRow<'a>>,
    ) {
        rows.push(VisibleRow {
            item,
            parent_id,
            sibling_index,
            depth,
            ancestors: ancestors.to_vec(),
        });

        if item.children.is_empty() {
            return;
        }

        let mut next_ancestors = ancestors.to_vec();
        next_ancestors.push(item.id);
        for (index, child) in item.children.iter().enumerate().rev() {
            self.collect_rows(child, item.id, index, depth + 1, &next_ancestors, rows);
        }
    }

    fn render_row(
        &self,
        ui: &mut egui::Ui,
        row: &VisibleRow<'_>,
        dragging_node: Option<NodeId>,
    ) -> RowRender {
        let is_selected = self.selected_node == Some(row.item.id);
        let is_dragging = dragging_node == Some(row.item.id);
        let fill = if is_selected {
            self.theme.hover_color.linear_multiply(1.15)
        } else if is_dragging {
            self.theme.input_bg_color.linear_multiply(0.8)
        } else {
            self.theme.input_bg_color
        };
        let stroke = if is_selected {
            Stroke::new(1.0, self.theme.accent_color)
        } else {
            Stroke::new(1.0, self.theme.border_color)
        };
        let row_width = ui.available_width();
        let (rect, row_response) =
            ui.allocate_exact_size(Vec2::new(row_width, ROW_HEIGHT), Sense::click());
        let painter = ui.painter();
        painter.rect(
            rect,
            CornerRadius::same(6),
            fill,
            stroke,
            StrokeKind::Middle,
        );

        let indent = row.depth as f32 * 14.0;
        let thumb_rect = Rect::from_min_size(rect.min + Vec2::new(8.0 + indent, 7.0), THUMB_SIZE);
        self.paint_thumbnail(ui, thumb_rect, row.item.kind);

        let handle_rect = Rect::from_min_size(
            rect.right_top() - Vec2::new(HANDLE_SIZE.x + 8.0, -7.0),
            HANDLE_SIZE,
        );
        let handle_response = ui.interact(
            handle_rect,
            ui.id().with(("layer-tree-handle", row.item.id.0)),
            Sense::click_and_drag(),
        );
        self.paint_handle(ui, handle_rect, handle_response.hovered() || is_dragging);

        let label_pos = thumb_rect.right_top() + Vec2::new(8.0, 1.0);
        painter.text(
            label_pos,
            Align2::LEFT_TOP,
            &row.item.label,
            FontId::proportional(13.0),
            self.theme.text_color,
        );
        painter.text(
            label_pos + Vec2::new(0.0, 16.0),
            Align2::LEFT_TOP,
            self.kind_label(row.item.kind),
            FontId::proportional(11.0),
            self.theme.accent_color,
        );

        RowRender {
            rect,
            row_response,
            handle_response,
        }
    }

    fn drop_candidate(
        &self,
        row: &VisibleRow<'_>,
        rect: Rect,
        pointer_pos: egui::Pos2,
        dragging_node: NodeId,
    ) -> Option<DropCandidate> {
        if dragging_node == row.item.id || row.ancestors.contains(&dragging_node) {
            return None;
        }

        let top_threshold = rect.top() + ROW_HEIGHT * 0.3;
        let bottom_threshold = rect.bottom() - ROW_HEIGHT * 0.3;
        let line_x = rect.left() + 8.0 + row.depth as f32 * 14.0;

        if pointer_pos.y <= top_threshold {
            return Some(DropCandidate {
                target: LayerMoveTarget {
                    parent_id: row.parent_id,
                    index: row.sibling_index + 1,
                },
                indicator: DropIndicator::InsertLine {
                    x_start: line_x,
                    y: rect.top(),
                },
            });
        }

        if pointer_pos.y >= bottom_threshold {
            return Some(DropCandidate {
                target: LayerMoveTarget {
                    parent_id: row.parent_id,
                    index: row.sibling_index,
                },
                indicator: DropIndicator::InsertLine {
                    x_start: line_x,
                    y: rect.bottom(),
                },
            });
        }

        if matches!(row.item.kind, UiNodeKind::Branch) {
            return Some(DropCandidate {
                target: LayerMoveTarget {
                    parent_id: row.item.id,
                    index: row.item.children.len(),
                },
                indicator: DropIndicator::HighlightGroup { rect },
            });
        }

        Some(DropCandidate {
            target: LayerMoveTarget {
                parent_id: row.parent_id,
                index: row.sibling_index,
            },
            indicator: DropIndicator::InsertLine {
                x_start: line_x,
                y: rect.bottom(),
            },
        })
    }

    fn paint_drop_indicator(&self, ui: &egui::Ui, indicator: &DropIndicator) {
        let painter = ui.painter();
        match indicator {
            DropIndicator::InsertLine { x_start, y } => {
                painter.line_segment(
                    [egui::pos2(*x_start, *y), egui::pos2(ui.max_rect().right() - 10.0, *y)],
                    Stroke::new(2.0, self.theme.accent_color),
                );
            }
            DropIndicator::HighlightGroup { rect } => {
                painter.rect_stroke(
                    rect.shrink(1.0),
                    CornerRadius::same(6),
                    Stroke::new(2.0, self.theme.accent_color),
                    StrokeKind::Middle,
                );
            }
        }
    }

    fn paint_handle(&self, ui: &egui::Ui, rect: Rect, active: bool) {
        let painter = ui.painter();
        let fill = if active {
            self.theme.hover_color
        } else {
            self.theme.panel_color
        };
        painter.rect(
            rect,
            CornerRadius::same(4),
            fill,
            Stroke::new(1.0, self.theme.border_color),
            StrokeKind::Middle,
        );

        let center_x = rect.center().x;
        for offset in [-4.0_f32, 0.0, 4.0] {
            let x = center_x + offset;
            painter.line_segment(
                [
                    egui::pos2(x, rect.top() + 5.0),
                    egui::pos2(x, rect.bottom() - 5.0),
                ],
                Stroke::new(1.0, self.theme.text_color),
            );
        }
    }

    fn paint_thumbnail(&self, ui: &egui::Ui, rect: Rect, kind: UiNodeKind) {
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
                let top = Rect::from_min_size(
                    rect.min + Vec2::new(4.0, 5.0),
                    Vec2::new(rect.width() - 8.0, 6.0),
                );
                let bottom = Rect::from_min_size(
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
