use egui::{Color32, Ui, widgets::color_picker};

pub fn rgb_color_picker(ui: &mut Ui, label: &str, color_rgb: &mut [f32; 3]) -> bool {
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
