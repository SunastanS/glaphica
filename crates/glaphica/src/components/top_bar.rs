use crate::theme::Theme;
use egui::{Button, Frame, RichText, TopBottomPanel};

pub struct TopBar;

impl TopBar {
    pub fn new() -> Self {
        Self
    }

    pub fn render(&mut self, ctx: &egui::Context, theme: &Theme) -> TopBarOutput {
        let mut output = TopBarOutput::default();
        TopBottomPanel::top("overlay-top-bar")
            .exact_height(38.0)
            .frame(Frame::default().fill(theme.bg_color))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Document").color(theme.text_color).strong());
                    ui.add_space((ui.available_width() - 200.0).max(0.0));
                    if ui
                        .add(Button::new("Save").fill(theme.input_bg_color))
                        .clicked()
                    {
                        output.save_clicked = true;
                    }
                    if ui
                        .add(Button::new("Load").fill(theme.input_bg_color))
                        .clicked()
                    {
                        output.load_clicked = true;
                    }
                    if ui
                        .add(Button::new("Export").fill(theme.input_bg_color))
                        .clicked()
                    {
                        output.export_clicked = true;
                    }
                });
            });
        output
    }
}

#[derive(Default)]
pub struct TopBarOutput {
    pub save_clicked: bool,
    pub load_clicked: bool,
    pub export_clicked: bool,
}
