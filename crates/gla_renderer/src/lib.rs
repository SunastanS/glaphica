mod gpu;
mod texture;

use atlas::TilePos;
use gla_color::BlendMode;
use gla_draw_on::DrawOnInvocation;
use std::error::Error;

pub use crate::gpu::{GpuRenderer, GpuRendererError};

pub type RendererDrawOnInvocation = DrawOnInvocation<TilePos>;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Pass {
    Clear {
        dst: TilePos,
    },
    Copy {
        src: TilePos,
        dst: TilePos,
    },
    RenderTo {
        src: TilePos,
        dst: TilePos,
        blend_mode: BlendMode,
        opacity: f32,
    },
    FixGutter {
        dst: TilePos,
    },
    DrawOn(RendererDrawOnInvocation),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RendererCapabilities {
    pub draw_radial_kernel_1d: bool,
    pub replace_circle_4d: bool,
}

pub trait RenderBackend {
    type Error: Error + 'static;

    fn submit(&mut self, passes: &[Pass]) -> Result<(), Self::Error>;
}
