pub struct View {
    offset_x: f32,
    offset_y: f32,
    scale: f32,
}

impl View {
    pub fn new() -> Self {
        Self {
            offset_x: 0.0,
            offset_y: 0.0,
            scale: 1.0,
        }
    }

    pub fn offset(&self) -> (f32, f32) {
        (self.offset_x, self.offset_y)
    }

    pub fn scale(&self) -> f32 {
        self.scale
    }

    pub fn set_offset(&mut self, x: f32, y: f32) {
        self.offset_x = x;
        self.offset_y = y;
    }

    pub fn set_scale(&mut self, scale: f32) {
        self.scale = scale.clamp(0.01, 100.0);
    }

    pub fn pan(&mut self, dx: f32, dy: f32) {
        self.offset_x += dx;
        self.offset_y += dy;
    }

    pub fn zoom(&mut self, factor: f32, center_x: f32, center_y: f32) {
        let new_scale = (self.scale * factor).clamp(0.01, 100.0);
        let ratio = new_scale / self.scale;
        self.offset_x = center_x - (center_x - self.offset_x) * ratio;
        self.offset_y = center_y - (center_y - self.offset_y) * ratio;
        self.scale = new_scale;
    }

    pub fn document_to_screen(&self, doc_x: f32, doc_y: f32) -> (f32, f32) {
        (
            doc_x * self.scale + self.offset_x,
            doc_y * self.scale + self.offset_y,
        )
    }

    pub fn screen_to_document(&self, screen_x: f32, screen_y: f32) -> (f32, f32) {
        (
            (screen_x - self.offset_x) / self.scale,
            (screen_y - self.offset_y) / self.scale,
        )
    }
}

impl Default for View {
    fn default() -> Self {
        Self::new()
    }
}
