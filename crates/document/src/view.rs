pub struct View {
    offset_x: f32,
    offset_y: f32,
    scale: f32,
    rotation: f32,
}

impl View {
    pub fn new() -> Self {
        Self {
            offset_x: 0.0,
            offset_y: 0.0,
            scale: 1.0,
            rotation: 0.0,
        }
    }

    pub fn offset(&self) -> (f32, f32) {
        (self.offset_x, self.offset_y)
    }

    pub fn scale(&self) -> f32 {
        self.scale
    }

    pub fn rotation(&self) -> f32 {
        self.rotation
    }

    pub fn set_offset(&mut self, x: f32, y: f32) {
        self.offset_x = x;
        self.offset_y = y;
    }

    pub fn set_scale(&mut self, scale: f32) {
        self.scale = scale.clamp(0.01, 100.0);
    }

    pub fn set_rotation(&mut self, rotation: f32) {
        self.rotation = rotation;
    }

    pub fn pan(&mut self, dx: f32, dy: f32) {
        self.offset_x += dx;
        self.offset_y += dy;
    }

    pub fn zoom(&mut self, factor: f32, center_x: f32, center_y: f32) {
        let (anchor_doc_x, anchor_doc_y) = self.screen_to_document(center_x, center_y);
        self.scale = (self.scale * factor).clamp(0.01, 100.0);
        self.keep_screen_anchor(anchor_doc_x, anchor_doc_y, center_x, center_y);
    }

    pub fn rotate(&mut self, delta_radians: f32, center_x: f32, center_y: f32) {
        let (anchor_doc_x, anchor_doc_y) = self.screen_to_document(center_x, center_y);
        self.rotation += delta_radians;
        self.keep_screen_anchor(anchor_doc_x, anchor_doc_y, center_x, center_y);
    }

    fn keep_screen_anchor(
        &mut self,
        anchor_doc_x: f32,
        anchor_doc_y: f32,
        screen_anchor_x: f32,
        screen_anchor_y: f32,
    ) {
        let (screen_x, screen_y) = self.document_to_screen(anchor_doc_x, anchor_doc_y);
        self.offset_x += screen_anchor_x - screen_x;
        self.offset_y += screen_anchor_y - screen_y;
    }

    fn rotated_scaled_doc(&self, doc_x: f32, doc_y: f32) -> (f32, f32) {
        let cos_theta = self.rotation.cos();
        let sin_theta = self.rotation.sin();
        let rotated_x = cos_theta * doc_x - sin_theta * doc_y;
        let rotated_y = sin_theta * doc_x + cos_theta * doc_y;
        (rotated_x * self.scale, rotated_y * self.scale)
    }

    pub fn document_to_screen(&self, doc_x: f32, doc_y: f32) -> (f32, f32) {
        let (scaled_x, scaled_y) = self.rotated_scaled_doc(doc_x, doc_y);
        (scaled_x + self.offset_x, scaled_y + self.offset_y)
    }

    pub fn screen_to_document(&self, screen_x: f32, screen_y: f32) -> (f32, f32) {
        let dx = (screen_x - self.offset_x) / self.scale;
        let dy = (screen_y - self.offset_y) / self.scale;
        let cos_theta = self.rotation.cos();
        let sin_theta = self.rotation.sin();
        (
            cos_theta * dx + sin_theta * dy,
            -sin_theta * dx + cos_theta * dy,
        )
    }
}

impl Default for View {
    fn default() -> Self {
        Self::new()
    }
}
