use egui::{Color32, Context, Style, Visuals};

#[derive(Debug, Clone, Copy)]
pub struct UiTheme {
    pub panel_fill: Color32,
    pub panel_border: Color32,
    pub accent: Color32,
    pub subdued_text: Color32,
}

impl Default for UiTheme {
    fn default() -> Self {
        Self {
            panel_fill: Color32::from_rgba_unmultiplied(28, 31, 36, 220),
            panel_border: Color32::from_rgb(60, 66, 74),
            accent: Color32::from_rgb(109, 181, 255),
            subdued_text: Color32::from_rgb(170, 178, 188),
        }
    }
}

pub fn apply_theme(ctx: &Context, theme: UiTheme) {
    let mut style: Style = (*ctx.style()).clone();
    style.visuals = Visuals::dark();
    style.visuals.window_fill = theme.panel_fill;
    style.visuals.panel_fill = theme.panel_fill;
    style.visuals.widgets.hovered.bg_fill = theme.accent.linear_multiply(0.25);
    style.visuals.widgets.active.bg_fill = theme.accent.linear_multiply(0.35);
    style.visuals.widgets.active.bg_stroke.color = theme.accent;
    style.visuals.selection.bg_fill = theme.accent.linear_multiply(0.35);
    style.visuals.faint_bg_color = theme.panel_fill;
    ctx.set_style(style);
}
