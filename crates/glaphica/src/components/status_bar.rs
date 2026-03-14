use crate::theme::Theme;
use egui::{Frame, TopBottomPanel};

pub struct StatusBar;

impl StatusBar {
    pub fn render(ctx: &egui::Context, theme: &Theme) {
        TopBottomPanel::bottom("overlay-bottom-bar")
            .exact_height(48.0)
            .frame(Frame::default().fill(theme.bg_color))
            .show(ctx, |_ui| {});
    }
}
