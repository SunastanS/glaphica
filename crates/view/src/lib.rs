#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewTransform {
    zoom: f32,
    offset_x: f32,
    offset_y: f32,
    roll_radians: f32,
    mirror_x: bool,
    mirror_y: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewTransformError {
    InvalidZoom,
    InvalidViewport,
    NonFiniteValue,
}

impl Default for ViewTransform {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            offset_x: 0.0,
            offset_y: 0.0,
            roll_radians: 0.0,
            mirror_x: false,
            mirror_y: false,
        }
    }
}

impl ViewTransform {
    pub fn zoom(&self) -> f32 {
        self.zoom
    }

    pub fn offset_x(&self) -> f32 {
        self.offset_x
    }

    pub fn offset_y(&self) -> f32 {
        self.offset_y
    }

    pub fn roll_radians(&self) -> f32 {
        self.roll_radians
    }

    pub fn mirror_x(&self) -> bool {
        self.mirror_x
    }

    pub fn mirror_y(&self) -> bool {
        self.mirror_y
    }

    pub fn set_zoom(&mut self, zoom: f32) -> Result<(), ViewTransformError> {
        if !zoom.is_finite() || zoom <= 0.0 {
            return Err(ViewTransformError::InvalidZoom);
        }
        self.zoom = zoom;
        Ok(())
    }

    pub fn zoom_about_point(
        &mut self,
        zoom_factor: f32,
        point_x: f32,
        point_y: f32,
    ) -> Result<(), ViewTransformError> {
        if !zoom_factor.is_finite() || zoom_factor <= 0.0 {
            return Err(ViewTransformError::InvalidZoom);
        }
        if !point_x.is_finite() || !point_y.is_finite() {
            return Err(ViewTransformError::NonFiniteValue);
        }

        let next_zoom = checked_mul(self.zoom, zoom_factor)?;
        if next_zoom <= 0.0 {
            return Err(ViewTransformError::InvalidZoom);
        }

        let keep_anchor_scale = checked_add(1.0, -zoom_factor)?;
        let scaled_offset_x = checked_mul(self.offset_x, zoom_factor)?;
        let scaled_offset_y = checked_mul(self.offset_y, zoom_factor)?;
        let anchor_x_contribution = checked_mul(point_x, keep_anchor_scale)?;
        let anchor_y_contribution = checked_mul(point_y, keep_anchor_scale)?;

        self.offset_x = checked_add(scaled_offset_x, anchor_x_contribution)?;
        self.offset_y = checked_add(scaled_offset_y, anchor_y_contribution)?;
        self.zoom = next_zoom;
        Ok(())
    }

    pub fn pan_by(&mut self, delta_x: f32, delta_y: f32) -> Result<(), ViewTransformError> {
        self.offset_x = checked_add(self.offset_x, delta_x)?;
        self.offset_y = checked_add(self.offset_y, delta_y)?;
        Ok(())
    }

    pub fn rotate_by(&mut self, delta_roll: f32) -> Result<(), ViewTransformError> {
        self.roll_radians = checked_add(self.roll_radians, delta_roll)?;
        Ok(())
    }

    pub fn set_mirror(&mut self, mirror_x: bool, mirror_y: bool) {
        self.mirror_x = mirror_x;
        self.mirror_y = mirror_y;
    }

    pub fn flip_along_screen_x_axis(&mut self) -> Result<(), ViewTransformError> {
        self.roll_radians = checked_neg(self.roll_radians)?;
        self.mirror_y = !self.mirror_y;
        Ok(())
    }

    pub fn flip_along_screen_y_axis(&mut self) -> Result<(), ViewTransformError> {
        self.roll_radians = checked_neg(self.roll_radians)?;
        self.mirror_x = !self.mirror_x;
        Ok(())
    }

    pub fn to_matrix3x3(&self) -> [[f32; 3]; 3] {
        let mirror_x_scale = if self.mirror_x { -1.0 } else { 1.0 };
        let mirror_y_scale = if self.mirror_y { -1.0 } else { 1.0 };
        let sine = self.roll_radians.sin();
        let cosine = self.roll_radians.cos();

        let m00 = self.zoom * cosine * mirror_x_scale;
        let m01 = self.zoom * -sine * mirror_y_scale;
        let m10 = self.zoom * sine * mirror_x_scale;
        let m11 = self.zoom * cosine * mirror_y_scale;

        [
            [m00, m01, self.offset_x],
            [m10, m11, self.offset_y],
            [0.0, 0.0, 1.0],
        ]
    }

    pub fn to_matrix4x4(&self) -> [f32; 16] {
        let matrix = self.to_matrix3x3();
        [
            matrix[0][0],
            matrix[1][0],
            0.0,
            0.0,
            matrix[0][1],
            matrix[1][1],
            0.0,
            0.0,
            0.0,
            0.0,
            1.0,
            0.0,
            matrix[0][2],
            matrix[1][2],
            0.0,
            1.0,
        ]
    }

    pub fn to_clip_matrix4x4(
        &self,
        viewport_width: f32,
        viewport_height: f32,
    ) -> Result<[f32; 16], ViewTransformError> {
        if !viewport_width.is_finite()
            || !viewport_height.is_finite()
            || viewport_width <= 0.0
            || viewport_height <= 0.0
        {
            return Err(ViewTransformError::InvalidViewport);
        }

        let matrix = self.to_matrix3x3();
        let scale_x = 2.0 / viewport_width;
        let scale_y = -2.0 / viewport_height;

        let clip_m00 = matrix[0][0] * scale_x;
        let clip_m01 = matrix[0][1] * scale_x;
        let clip_m10 = matrix[1][0] * scale_y;
        let clip_m11 = matrix[1][1] * scale_y;
        let clip_tx = matrix[0][2] * scale_x - 1.0;
        let clip_ty = matrix[1][2] * scale_y + 1.0;

        Ok([
            clip_m00, clip_m10, 0.0, 0.0, clip_m01, clip_m11, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0,
            clip_tx, clip_ty, 0.0, 1.0,
        ])
    }

    pub fn screen_to_canvas_point(
        &self,
        screen_x: f32,
        screen_y: f32,
    ) -> Result<(f32, f32), ViewTransformError> {
        if !screen_x.is_finite() || !screen_y.is_finite() {
            return Err(ViewTransformError::NonFiniteValue);
        }

        let matrix = self.to_matrix3x3();
        let m00 = matrix[0][0];
        let m01 = matrix[0][1];
        let m10 = matrix[1][0];
        let m11 = matrix[1][1];
        let tx = matrix[0][2];
        let ty = matrix[1][2];

        let det = m00 * m11 - m01 * m10;
        if !det.is_finite() || det.abs() <= f32::EPSILON {
            return Err(ViewTransformError::NonFiniteValue);
        }

        let inv00 = m11 / det;
        let inv01 = -m01 / det;
        let inv10 = -m10 / det;
        let inv11 = m00 / det;

        let rhs_x = screen_x - tx;
        let rhs_y = screen_y - ty;
        let canvas_x = inv00 * rhs_x + inv01 * rhs_y;
        let canvas_y = inv10 * rhs_x + inv11 * rhs_y;
        if !canvas_x.is_finite() || !canvas_y.is_finite() {
            return Err(ViewTransformError::NonFiniteValue);
        }
        Ok((canvas_x, canvas_y))
    }
}

fn checked_add(current: f32, delta: f32) -> Result<f32, ViewTransformError> {
    if !delta.is_finite() {
        return Err(ViewTransformError::NonFiniteValue);
    }
    let next = current + delta;
    if !next.is_finite() {
        return Err(ViewTransformError::NonFiniteValue);
    }
    Ok(next)
}

fn checked_mul(left: f32, right: f32) -> Result<f32, ViewTransformError> {
    if !left.is_finite() || !right.is_finite() {
        return Err(ViewTransformError::NonFiniteValue);
    }
    let next = left * right;
    if !next.is_finite() {
        return Err(ViewTransformError::NonFiniteValue);
    }
    Ok(next)
}

fn checked_neg(value: f32) -> Result<f32, ViewTransformError> {
    if !value.is_finite() {
        return Err(ViewTransformError::NonFiniteValue);
    }
    let next = -value;
    if !next.is_finite() {
        return Err(ViewTransformError::NonFiniteValue);
    }
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zoom_about_point_keeps_anchor_screen_position() {
        let mut transform = ViewTransform::default();
        transform.pan_by(20.0, -10.0).expect("pan");

        transform
            .zoom_about_point(2.0, 100.0, 50.0)
            .expect("zoom about point");

        assert!((transform.zoom() - 2.0).abs() < 1e-6);
        assert!((transform.offset_x() + 60.0).abs() < 1e-6);
        assert!((transform.offset_y() + 70.0).abs() < 1e-6);
    }

    #[test]
    fn zoom_about_point_rejects_invalid_inputs() {
        let mut transform = ViewTransform::default();
        assert_eq!(
            transform.zoom_about_point(0.0, 10.0, 20.0),
            Err(ViewTransformError::InvalidZoom)
        );
        assert_eq!(
            transform.zoom_about_point(1.2, f32::NAN, 20.0),
            Err(ViewTransformError::NonFiniteValue)
        );
    }
}
