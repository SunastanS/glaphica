use crate::theme::Theme;
use app::AppStats;
use egui::{Align, Frame, Layout, RichText, TopBottomPanel};

pub struct StatusBar;

impl StatusBar {
    pub fn render(ctx: &egui::Context, theme: &Theme, stats: Option<&AppStats>) {
        TopBottomPanel::bottom("overlay-bottom-bar")
            .exact_height(48.0)
            .frame(Frame::default().fill(theme.bg_color))
            .show(ctx, |ui| {
                ui.add_space(8.0);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let undo_text = stats
                        .map(|stats| format!("Undo {}", stats.undo_stroke_count))
                        .unwrap_or_else(|| "Undo -".to_owned());
                    ui.label(RichText::new(undo_text).color(theme.text_color).monospace());

                    if let Some(stats) = stats {
                        for backend in stats.backend_tiles.iter().rev() {
                            ui.add_space(16.0);
                            ui.label(
                                RichText::new(format!(
                                    "B{} A:{} C:{} F:{}",
                                    backend.backend_id.raw(),
                                    backend.active,
                                    backend.cached,
                                    backend.free
                                ))
                                .color(theme.text_color)
                                .monospace(),
                            );
                        }
                    }
                });
            });
    }
}
