use egui::{Color32, Frame, Stroke};

pub fn panel_frame(fill: Color32, border: Color32) -> Frame {
    Frame::default().fill(fill).stroke(Stroke::new(1.0, border))
}
