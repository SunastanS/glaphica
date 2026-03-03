use core::marker::PhantomData;

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Vec2<S = ()> {
    pub x: f32,
    pub y: f32,
    _s: PhantomData<S>,
}

impl<S> Vec2<S> {
    #[inline]
    pub const fn new(x: f32, y: f32) -> Self {
        Self {
            x,
            y,
            _s: PhantomData,
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
