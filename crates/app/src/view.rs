use std::error::Error;
use std::fmt::{Display, Formatter};

use glaphica_core::{CanvasVec2, ScreenVec2};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AppView {
    document_to_screen: [f32; 6],
    screen_to_document: [f32; 6],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppViewMatrixError {
    NonInvertible,
}

impl Display for AppViewMatrixError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NonInvertible => f.write_str("view matrix is not invertible"),
        }
    }
}

impl Error for AppViewMatrixError {}

impl Default for AppView {
    fn default() -> Self {
        Self::identity()
    }
}

impl AppView {
    pub fn identity() -> Self {
        Self {
            document_to_screen: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            screen_to_document: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
        }
    }

    pub fn new(document_to_screen: [f32; 6]) -> Result<Self, AppViewMatrixError> {
        let screen_to_document =
            invert_affine(document_to_screen).ok_or(AppViewMatrixError::NonInvertible)?;
        Ok(Self {
            document_to_screen,
            screen_to_document,
        })
    }

    pub fn from_scale_rotation_translation(
        scale_x: f32,
        scale_y: f32,
        rotation_radians: f32,
        translate_x: f32,
        translate_y: f32,
    ) -> Result<Self, AppViewMatrixError> {
        let cos_theta = rotation_radians.cos();
        let sin_theta = rotation_radians.sin();
        Self::new([
            cos_theta * scale_x,
            sin_theta * scale_x,
            -sin_theta * scale_y,
            cos_theta * scale_y,
            translate_x,
            translate_y,
        ])
    }

    pub fn set_document_to_screen(
        &mut self,
        document_to_screen: [f32; 6],
    ) -> Result<(), AppViewMatrixError> {
        let screen_to_document =
            invert_affine(document_to_screen).ok_or(AppViewMatrixError::NonInvertible)?;
        self.document_to_screen = document_to_screen;
        self.screen_to_document = screen_to_document;
        Ok(())
    }

    pub fn document_to_screen_matrix(&self) -> [f32; 6] {
        self.document_to_screen
    }

    pub fn screen_to_document_matrix(&self) -> [f32; 6] {
        self.screen_to_document
    }

    pub fn document_to_screen_point(&self, point: CanvasVec2) -> ScreenVec2 {
        transform_point(self.document_to_screen, point.x, point.y)
    }

    pub fn screen_to_document_point(&self, point: ScreenVec2) -> CanvasVec2 {
        transform_point(self.screen_to_document, point.x, point.y)
    }
}

fn transform_point<S>(matrix: [f32; 6], x: f32, y: f32) -> glaphica_core::Vec2<S> {
    glaphica_core::Vec2::new(
        matrix[0] * x + matrix[2] * y + matrix[4],
        matrix[1] * x + matrix[3] * y + matrix[5],
    )
}

fn invert_affine(matrix: [f32; 6]) -> Option<[f32; 6]> {
    let determinant = matrix[0] * matrix[3] - matrix[1] * matrix[2];
    if !determinant.is_finite() || determinant.abs() <= f32::EPSILON {
        return None;
    }
    let inverse_det = 1.0 / determinant;
    let inv_a = matrix[3] * inverse_det;
    let inv_b = -matrix[1] * inverse_det;
    let inv_c = -matrix[2] * inverse_det;
    let inv_d = matrix[0] * inverse_det;
    let inv_tx = -(inv_a * matrix[4] + inv_c * matrix[5]);
    let inv_ty = -(inv_b * matrix[4] + inv_d * matrix[5]);
    Some([inv_a, inv_b, inv_c, inv_d, inv_tx, inv_ty])
}

#[cfg(test)]
mod tests {
    use glaphica_core::{CanvasVec2, ScreenVec2};

    use super::{AppView, AppViewMatrixError};

    #[test]
    fn identity_maps_points_without_change() {
        let view = AppView::identity();
        assert_eq!(
            view.document_to_screen_point(CanvasVec2::new(12.0, -3.0)),
            ScreenVec2::new(12.0, -3.0)
        );
    }

    #[test]
    fn inverse_maps_back_to_document_space() {
        let view = AppView::new([2.0, 0.0, 0.0, 3.0, 10.0, -5.0]).unwrap();
        let screen = view.document_to_screen_point(CanvasVec2::new(4.0, 6.0));
        let canvas = view.screen_to_document_point(screen);
        assert_eq!(canvas, CanvasVec2::new(4.0, 6.0));
    }

    #[test]
    fn non_invertible_matrix_is_rejected() {
        let result = AppView::new([1.0, 2.0, 2.0, 4.0, 0.0, 0.0]);
        assert_eq!(result, Err(AppViewMatrixError::NonInvertible));
    }
}
