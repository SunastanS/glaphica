use egui::Color32;

#[derive(Clone, Copy)]
pub struct Theme {
    pub bg_color: Color32,
    pub panel_color: Color32,
    pub accent_color: Color32,
    pub text_color: Color32,
    pub border_color: Color32,
    pub input_bg_color: Color32,
    pub hover_color: Color32,
    pub curve_bg: Color32,
    pub curve_grid: Color32,
    pub curve_line: Color32,
    pub curve_point_fill: Color32,
    pub curve_point_stroke: Color32,
    pub error_color: Color32,
}

impl Theme {
    pub fn dark() -> Self {
        Self {
            bg_color: Color32::from_rgb(51, 51, 51),
            panel_color: Color32::from_rgb(68, 68, 68),
            accent_color: Color32::from_rgb(74, 144, 226),
            text_color: Color32::from_rgb(220, 220, 220),
            border_color: Color32::from_rgb(48, 48, 48),
            input_bg_color: Color32::from_rgb(38, 38, 38),
            hover_color: Color32::from_rgb(80, 80, 80),
            curve_bg: Color32::from_rgb(18, 18, 18),
            curve_grid: Color32::from_rgb(48, 48, 48),
            curve_line: Color32::from_rgb(128, 214, 255),
            curve_point_fill: Color32::from_rgb(244, 174, 68),
            curve_point_stroke: Color32::BLACK,
            error_color: Color32::from_rgb(220, 90, 90),
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::dark()
    }
}
