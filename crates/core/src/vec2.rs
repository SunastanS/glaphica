use std::marker::PhantomData;

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Vec2<S = ()> {
    pub x: f32,
    pub y: f32,
    _space: PhantomData<S>,
}

impl<S> Vec2<S> {
    #[inline]
    pub const fn new(x: f32, y: f32) -> Self {
        Self {
            x,
            y,
            _space: PhantomData,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenSpace {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CanvasSpace {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RadianSpace {}

pub type ScreenVec2 = Vec2<ScreenSpace>;
pub type CanvasVec2 = Vec2<CanvasSpace>;
pub type RadianVec2 = Vec2<RadianSpace>;

#[cfg(test)]
mod tests {
    use super::{CanvasVec2, RadianVec2, ScreenVec2};

    #[test]
    fn typed_vectors_preserve_coordinates() {
        let screen = ScreenVec2::new(10.0, 20.0);
        let canvas = CanvasVec2::new(-3.5, 4.25);
        let radians = RadianVec2::new(1.0, -1.0);

        assert_eq!(screen.x, 10.0);
        assert_eq!(screen.y, 20.0);
        assert_eq!(canvas.x, -3.5);
        assert_eq!(canvas.y, 4.25);
        assert_eq!(radians.x, 1.0);
        assert_eq!(radians.y, -1.0);
    }
}
