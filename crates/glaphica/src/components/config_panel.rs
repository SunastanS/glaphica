use crate::theme::Theme;
use crate::{BrushKind, BrushUiState};
use brushes::{
    BrushConfigKind, BrushConfigValue, UnitIntervalPoint, eval_unit_interval_curve_polynomial,
};
use egui::{Color32, Frame, Rect, Sense, Shape, SidePanel, Stroke, vec2};

pub const RIGHT_PANEL_COMPACT_WIDTH: f32 = 160.0;
const RIGHT_PANEL_DRAG_MIN_WIDTH: f32 = 28.0;
const COLLAPSED_PANEL_WIDTH: f32 = 28.0;

pub struct ConfigPanel<'a> {
    collapsed: bool,
    width: f32,
    max_width: f32,
    active_color_rgb: &'a mut [f32; 3],
    brush_states: &'a mut [BrushUiState],
    selected_brush_index: usize,
}

impl<'a> ConfigPanel<'a> {
    pub fn new(
        collapsed: bool,
        width: f32,
        max_width: f32,
        active_color_rgb: &'a mut [f32; 3],
        brush_states: &'a mut [BrushUiState],
        selected_brush_index: usize,
    ) -> Self {
        Self {
            collapsed,
            width,
            max_width,
            active_color_rgb,
            brush_states,
            selected_brush_index,
        }
    }

    pub fn render(&mut self, ctx: &egui::Context, theme: &Theme) -> ConfigPanelOutput {
        let mut output = ConfigPanelOutput::default();
        let panel_fill = translucent_panel_fill(theme);

        if self.collapsed {
            SidePanel::right("overlay-right-panel-collapsed")
                .resizable(false)
                .exact_width(COLLAPSED_PANEL_WIDTH)
                .frame(Frame::default().fill(panel_fill))
                .show(ctx, |ui| {
                    if ui.button("<").clicked() {
                        output.toggle_collapse = true;
                    }
                });
            return output;
        }

        let panel = SidePanel::right("overlay-right-panel")
            .resizable(true)
            .default_width(self.width)
            .min_width(RIGHT_PANEL_DRAG_MIN_WIDTH)
            .max_width(self.max_width)
            .frame(Frame::default().fill(panel_fill))
            .show(ctx, |ui| {
                let compact = ui.available_width() <= RIGHT_PANEL_COMPACT_WIDTH;
                ui.horizontal(|ui| {
                    if ui.button(">").clicked() {
                        output.toggle_collapse = true;
                    }
                    if compact {
                        ui.menu_button(
                            self.brush_states
                                .get(self.selected_brush_index)
                                .map(|state| state.kind.label())
                                .unwrap_or("Brush"),
                            |ui| {
                                for kind in BrushKind::ALL {
                                    if let Some(index) = self
                                        .brush_states
                                        .iter()
                                        .position(|state| state.kind == kind)
                                        && ui
                                            .selectable_label(
                                                self.selected_brush_index == index,
                                                kind.label(),
                                            )
                                            .clicked()
                                    {
                                        self.selected_brush_index = index;
                                        output.brush_selection_changed = true;
                                        ui.close();
                                    }
                                }
                            },
                        );
                    } else {
                        ui.heading("Brush Config");
                    }
                });
                ui.separator();

                let previous_index = self.selected_brush_index;
                let selected_label = self
                    .brush_states
                    .get(self.selected_brush_index)
                    .map(|state| state.kind.label())
                    .unwrap_or("Unknown");

                if !compact {
                    egui::ComboBox::from_label("Engine")
                        .selected_text(selected_label)
                        .show_ui(ui, |ui| {
                            for kind in BrushKind::ALL {
                                if let Some(index) = self
                                    .brush_states
                                    .iter()
                                    .position(|state| state.kind == kind)
                                    && ui
                                        .selectable_label(
                                            self.selected_brush_index == index,
                                            kind.label(),
                                        )
                                        .clicked()
                                {
                                    self.selected_brush_index = index;
                                    output.brush_selection_changed = true;
                                }
                            }
                        });
                }

                if previous_index != self.selected_brush_index
                    && self
                        .brush_states
                        .get(previous_index)
                        .map(|state| state.dirty)
                        .unwrap_or(false)
                {
                    let previous = &mut self.brush_states[previous_index];
                    previous.dirty = false;
                    output.pending_brush_update = Some((previous.kind, previous.values.clone()));
                }

                render_color_section(ui, self.active_color_rgb, compact, theme);

                if let Some(brush_state) = self.brush_states.get_mut(self.selected_brush_index) {
                    ui.separator();
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            render_brush_params(ui, brush_state, theme, &mut output);
                            ui.add_space(8.0);
                            render_config_actions(ui, brush_state, &mut output);
                        });
                }
            });

        output.panel_rect = Some(panel.response.rect);
        output.new_selected_index = Some(self.selected_brush_index);
        output
    }
}

fn render_color_section(
    ui: &mut egui::Ui,
    active_color_rgb: &mut [f32; 3],
    compact: bool,
    theme: &Theme,
) {
    ui.group(|ui| {
        if compact {
            ui.horizontal(|ui| {
                ui.menu_button(color_swatch_text(*active_color_rgb), |ui| {
                    render_color_picker(ui, active_color_rgb);
                });
            });
        } else {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("Color")
                        .size(12.0)
                        .color(theme.text_color)
                        .strong(),
                );
            });
            ui.add_space(4.0);
            ui.scope(|ui| {
                ui.spacing_mut().slider_width = ui.available_width().max(48.0);
                render_color_picker(ui, active_color_rgb);
            });
        }
    });
}

fn render_color_picker(ui: &mut egui::Ui, color_rgb: &mut [f32; 3]) {
    let mut color = to_color32(*color_rgb);
    if egui::widgets::color_picker::color_picker_color32(
        ui,
        &mut color,
        egui::widgets::color_picker::Alpha::Opaque,
    ) {
        *color_rgb = [
            f32::from(color.r()) / 255.0,
            f32::from(color.g()) / 255.0,
            f32::from(color.b()) / 255.0,
        ];
    }
}

fn color_swatch_text(rgb: [f32; 3]) -> egui::RichText {
    egui::RichText::new("    ").background_color(to_color32(rgb))
}

fn to_color32(rgb: [f32; 3]) -> Color32 {
    let to_u8 = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
    Color32::from_rgb(to_u8(rgb[0]), to_u8(rgb[1]), to_u8(rgb[2]))
}

fn translucent_panel_fill(theme: &Theme) -> Color32 {
    Color32::from_rgba_unmultiplied(
        theme.panel_color.r(),
        theme.panel_color.g(),
        theme.panel_color.b(),
        208,
    )
}

#[derive(Default)]
pub struct ConfigPanelOutput {
    pub toggle_collapse: bool,
    pub pending_brush_update: Option<(BrushKind, Vec<BrushConfigValue>)>,
    pub brush_selection_changed: bool,
    pub new_selected_index: Option<usize>,
    pub panel_rect: Option<Rect>,
}

fn render_brush_params(
    ui: &mut egui::Ui,
    brush_state: &mut BrushUiState,
    theme: &Theme,
    _output: &mut ConfigPanelOutput,
) {
    ui.group(|ui| {
        ui.horizontal(|ui| {
            let eraser_button = egui::Button::new("␡").selected(brush_state.eraser);
            if ui.add(eraser_button).clicked() {
                brush_state.eraser = !brush_state.eraser;
                brush_state.dirty = true;
            }
        });

        ui.add_space(4.0);

        for index in 0..brush_state.items.len() {
            if !brush_state.visible.get(index).copied().unwrap_or(false) {
                continue;
            }

            let item = &brush_state.items[index];
            let item_key = item.key;
            let item_kind = item.kind.clone();

            match (&item_kind, &mut brush_state.values[index]) {
                (BrushConfigKind::ScalarF32 { min, max }, BrushConfigValue::ScalarF32(current)) => {
                    render_scalar_config(ui, item_key, current, *min, *max, &mut brush_state.dirty);
                }
                (
                    BrushConfigKind::UnitIntervalCurve,
                    BrushConfigValue::UnitIntervalCurve(points),
                ) => {
                    ui.add_space(4.0);
                    render_curve_config(ui, item_key, points, &mut brush_state.dirty, theme);
                }
                _ => {
                    ui.colored_label(theme.error_color, "Config type mismatch");
                }
            }
        }
    });
}

fn render_config_actions(
    ui: &mut egui::Ui,
    brush_state: &mut BrushUiState,
    _output: &mut ConfigPanelOutput,
) {
    ui.horizontal(|ui| {
        let hidden_items = brush_state
            .items
            .iter()
            .zip(brush_state.visible.iter())
            .enumerate()
            .filter(|(_, (item, visible))| item.default_hidden && !**visible)
            .map(|(index, (item, _))| (index, item.label))
            .collect::<Vec<_>>();

        ui.add_enabled_ui(!hidden_items.is_empty(), |ui| {
            ui.menu_button("+", |ui| {
                for (index, label) in &hidden_items {
                    if ui.button(*label).clicked() {
                        if let Some(visible) = brush_state.visible.get_mut(*index) {
                            *visible = true;
                            brush_state.dirty = true;
                        }
                        ui.close();
                    }
                }
            });
        });

        if ui.button("Reset").clicked() {
            brush_state.reset();
        }
    });
}

fn render_scalar_config(
    ui: &mut egui::Ui,
    key: &'static str,
    value: &mut f32,
    min: f32,
    max: f32,
    dirty: &mut bool,
) {
    const VALUE_FIELD_WIDTH: f32 = 72.0;
    let available_width = ui.available_width();
    let drag_speed = (max - min) * 0.01;
    let minimum_slider_width = ui.spacing().interact_size.x;
    let slider_width = (available_width - VALUE_FIELD_WIDTH - ui.spacing().item_spacing.x).max(0.0);
    let is_compact =
        available_width < RIGHT_PANEL_COMPACT_WIDTH || slider_width < minimum_slider_width;

    ui.push_id(key, |ui| {
        ui.add_space(2.0);

        if is_compact {
            ui.horizontal(|ui| {
                if ui
                    .add(
                        egui::DragValue::new(value)
                            .speed(drag_speed)
                            .range(min..=max),
                    )
                    .changed()
                {
                    *dirty = true;
                }
            });
        } else {
            ui.horizontal(|ui| {
                let slider_changed = ui
                    .scope(|ui| {
                        ui.spacing_mut().slider_width = slider_width;
                        ui.add(egui::Slider::new(value, min..=max).show_value(false))
                            .changed()
                    })
                    .inner;
                let value_changed = ui
                    .add_sized(
                        [VALUE_FIELD_WIDTH, 0.0],
                        egui::DragValue::new(value)
                            .speed(drag_speed)
                            .range(min..=max),
                    )
                    .changed();

                if slider_changed || value_changed {
                    *dirty = true;
                }
            });
        }
    });
}

fn render_curve_config(
    ui: &mut egui::Ui,
    key: &'static str,
    points: &mut Vec<UnitIntervalPoint>,
    dirty: &mut bool,
    theme: &Theme,
) {
    let available_width = ui.available_width();
    let is_compact = available_width < RIGHT_PANEL_COMPACT_WIDTH;

    ui.push_id(key, |ui| {
        ui.horizontal(|ui| {
            if ui.small_button("+ point").clicked() {
                insert_curve_point(points);
                *dirty = true;
            }
            if points.len() > 2 && ui.small_button("- point").clicked() {
                points.remove(points.len().saturating_sub(2));
                *dirty = true;
            }
        });

        let desired_size = if is_compact {
            vec2(available_width, 100.0)
        } else {
            vec2(ui.available_width(), 160.0)
        };
        let (rect, response) = ui.allocate_exact_size(desired_size, Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        paint_curve_editor(&painter, rect, points, theme);
        interact_with_curve(ui, key, rect, &response, points, dirty);
    });
}

fn insert_curve_point(points: &mut Vec<UnitIntervalPoint>) {
    if points.len() < 2 {
        points.push(UnitIntervalPoint::new(1.0, 1.0));
        return;
    }

    let mut insert_index = 1usize;
    let mut max_gap = 0.0f32;
    for index in 0..points.len() - 1 {
        let gap = points[index + 1].x - points[index].x;
        if gap > max_gap {
            max_gap = gap;
            insert_index = index + 1;
        }
    }

    let prev = points[insert_index - 1];
    let next = points[insert_index];
    points.insert(
        insert_index,
        UnitIntervalPoint::new((prev.x + next.x) * 0.5, (prev.y + next.y) * 0.5),
    );
}

fn paint_curve_editor(
    painter: &egui::Painter,
    rect: Rect,
    points: &[UnitIntervalPoint],
    theme: &Theme,
) {
    painter.rect_filled(rect, 6.0, theme.curve_bg);
    painter.rect_stroke(
        rect,
        6.0,
        Stroke::new(1.0, theme.curve_grid),
        egui::StrokeKind::Inside,
    );

    for step in 1..4 {
        let t = step as f32 / 4.0;
        let x = egui::lerp(rect.left()..=rect.right(), t);
        let y = egui::lerp(rect.bottom()..=rect.top(), t);
        painter.line_segment(
            [
                egui::Pos2::new(x, rect.top()),
                egui::Pos2::new(x, rect.bottom()),
            ],
            Stroke::new(1.0, theme.curve_grid),
        );
        painter.line_segment(
            [
                egui::Pos2::new(rect.left(), y),
                egui::Pos2::new(rect.right(), y),
            ],
            Stroke::new(1.0, theme.curve_grid),
        );
    }

    let mut curve = Vec::with_capacity(65);
    for step in 0..=64 {
        let x = step as f32 / 64.0;
        let y = eval_unit_interval_curve_polynomial(points, x).unwrap_or(0.0);
        curve.push(curve_pos(rect, x, y));
    }
    painter.add(Shape::line(curve, Stroke::new(2.0, theme.curve_line)));

    for point in points {
        painter.circle(
            curve_pos(rect, point.x, point.y),
            5.0,
            theme.curve_point_fill,
            Stroke::new(1.0, theme.curve_point_stroke),
        );
    }
}

fn interact_with_curve(
    ui: &mut egui::Ui,
    key: &'static str,
    rect: Rect,
    response: &egui::Response,
    points: &mut [UnitIntervalPoint],
    dirty: &mut bool,
) {
    let drag_id = ui.id().with(key).with("curve_drag_index");

    if response.drag_started()
        && let Some(pointer_pos) = response.interact_pointer_pos()
    {
        let closest = points
            .iter()
            .enumerate()
            .map(|(index, point)| {
                (
                    index,
                    curve_pos(rect, point.x, point.y).distance(pointer_pos),
                )
            })
            .min_by(|(_, lhs), (_, rhs)| lhs.total_cmp(rhs));

        if let Some((index, distance)) = closest
            && distance <= 14.0
        {
            ui.memory_mut(|memory| memory.data.insert_temp(drag_id, index));
        }
    }

    if response.dragged()
        && let Some(pointer_pos) = response.interact_pointer_pos()
        && let Some(index) = ui.memory(|memory| memory.data.get_temp::<usize>(drag_id))
    {
        let mut x = ((pointer_pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
        let y = ((rect.bottom() - pointer_pos.y) / rect.height()).clamp(0.0, 1.0);

        if index == 0 {
            x = 0.0;
        } else if index + 1 == points.len() {
            x = 1.0;
        } else {
            let min_x = points[index - 1].x + 0.01;
            let max_x = points[index + 1].x - 0.01;
            x = x.clamp(min_x, max_x);
        }

        points[index] = UnitIntervalPoint::new(x, y);
        *dirty = true;
    }

    if response.drag_stopped() {
        ui.memory_mut(|memory| memory.data.remove::<usize>(drag_id));
    }
}

fn curve_pos(rect: Rect, x: f32, y: f32) -> egui::Pos2 {
    egui::Pos2::new(
        egui::lerp(rect.left()..=rect.right(), x),
        egui::lerp(rect.bottom()..=rect.top(), y),
    )
}
