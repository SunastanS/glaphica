mod gpu;
mod texture;

use atlas::TilePos;
use gla_color::BlendMode;
use std::error::Error;

pub use crate::gpu::{GpuRenderer, GpuRendererError};

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
    /// Accumulates a linear radial kernel into `dst`:
    /// `dst += max(0, 1 - d / max(radius, 1px)) * flow`.
    DrawRadialKernel1D {
        dst: TilePos,
        center_in_tile_x: f32,
        center_in_tile_y: f32,
        radius: f32,
        flow: f32,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RendererCapabilities {
    pub draw_radial_kernel_1d: bool,
}

pub trait RenderBackend {
    type Error: Error + 'static;

    fn submit(&mut self, passes: &[Pass]) -> Result<(), Self::Error>;
}
