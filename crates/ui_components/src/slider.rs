use std::ops::RangeInclusive;

use egui::{DragValue, Slider, Ui};

pub fn labeled_f32_slider(
    ui: &mut Ui,
    label: &str,
    value: &mut f32,
    range: RangeInclusive<f32>,
) -> bool {
    let min = *range.start();
    let max = *range.end();
    let drag_speed = ((max - min).abs() / 200.0).max(0.001);
    let mut changed = false;

    ui.vertical(|ui| {
        ui.label(label);
        ui.horizontal(|ui| {
            changed |= ui
                .add(Slider::new(value, min..=max).show_value(false))
                .changed();
            changed |= ui.add(DragValue::new(value).speed(drag_speed)).changed();
        });
    });

    if changed {
        *value = value.clamp(min, max);
    }

    changed
}
