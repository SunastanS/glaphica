use std::ops::RangeInclusive;

use egui::{Color32, DragValue, Frame, Slider, Stroke, Ui, widgets::color_picker};

pub(crate) fn panel_frame(fill: Color32, border: Color32) -> Frame {
    Frame::default().fill(fill).stroke(Stroke::new(1.0, border))
}

pub(crate) fn labeled_f32_slider(
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
        clamp_f32_to_range(value, min, max);
    }

    changed
}

pub(crate) fn rgb_color_picker(ui: &mut Ui, label: &str, color_rgb: &mut [f32; 3]) -> bool {
    let mut changed = false;

    ui.vertical(|ui| {
        ui.label(label);
        let mut color = rgb_to_color32(*color_rgb);
        changed = color_picker::color_picker_color32(ui, &mut color, color_picker::Alpha::Opaque);
        if changed {
            *color_rgb = color32_to_rgb(color);
        }
    });

    changed
}

fn clamp_f32_to_range(value: &mut f32, min: f32, max: f32) {
    *value = value.clamp(min, max);
}

fn rgb_to_color32(rgb: [f32; 3]) -> Color32 {
    let to_u8 = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
    Color32::from_rgb(to_u8(rgb[0]), to_u8(rgb[1]), to_u8(rgb[2]))
}

fn color32_to_rgb(color: Color32) -> [f32; 3] {
    [
        f32::from(color.r()) / 255.0,
        f32::from(color.g()) / 255.0,
        f32::from(color.b()) / 255.0,
    ]
}

#[cfg(test)]
mod tests {
    use super::{clamp_f32_to_range, color32_to_rgb, panel_frame, rgb_to_color32};
    use egui::Color32;

    #[test]
    fn rgb_color_conversion_clamps_and_rounds_to_opaque_color32() {
        let color = rgb_to_color32([-1.0, 0.5, 2.0]);

        assert_eq!(color, Color32::from_rgb(0, 128, 255));
        assert_eq!(
            color32_to_rgb(Color32::from_rgb(51, 102, 255)),
            [0.2, 0.4, 1.0]
        );
    }

    #[test]
    fn slider_value_clamp_matches_range_contract() {
        let mut too_low = -5.0;
        let mut too_high = 12.0;
        let mut inside = 0.5;

        clamp_f32_to_range(&mut too_low, 0.0, 1.0);
        clamp_f32_to_range(&mut too_high, 0.0, 1.0);
        clamp_f32_to_range(&mut inside, 0.0, 1.0);

        assert_eq!(too_low, 0.0);
        assert_eq!(too_high, 1.0);
        assert_eq!(inside, 0.5);
    }

    #[test]
    fn panel_frame_preserves_fill_and_border_colors() {
        let fill = Color32::from_rgb(1, 2, 3);
        let border = Color32::from_rgb(4, 5, 6);

        let frame = panel_frame(fill, border);

        assert_eq!(frame.fill, fill);
        assert_eq!(frame.stroke.color, border);
        assert_eq!(frame.stroke.width, 1.0);
    }
}
