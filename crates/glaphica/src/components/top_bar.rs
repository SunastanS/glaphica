use crate::theme::Theme;
use egui::{Button, Frame, RichText, TextEdit, TopBottomPanel};

pub struct TopBar<'a> {
    document_path: &'a mut String,
}

impl<'a> TopBar<'a> {
    pub fn new(document_path: &'a mut String) -> Self {
        Self { document_path }
    }

    pub fn render(&mut self, ctx: &egui::Context, theme: &Theme) -> TopBarOutput {
        let mut output = TopBarOutput::default();
        TopBottomPanel::top("overlay-top-bar")
            .exact_height(38.0)
            .frame(Frame::default().fill(theme.bg_color))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Document").color(theme.text_color).strong());
                    let text_edit = TextEdit::singleline(self.document_path)
                        .desired_width((ui.available_width() - 160.0).max(120.0));
                    ui.add(text_edit);
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
                });
            });
        output
    }
}

#[derive(Default)]
pub struct TopBarOutput {
    pub save_clicked: bool,
    pub load_clicked: bool,
}
