mod gpu;
mod texture;

use atlas::TilePos;
use gla_color::BlendMode;
use gla_draw_on::DrawOnInvocation;
use std::error::Error;

pub use crate::gpu::{GpuRenderer, GpuRendererError};

pub type RendererDrawOnInvocation = DrawOnInvocation<TilePos>;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PresentTileParams {
    pub target_min_px: [f32; 2],
    pub target_max_px: [f32; 2],
    pub source_width: u32,
    pub source_height: u32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PresentTile {
    pub src: TilePos,
    pub params: PresentTileParams,
}

#[derive(Clone, Copy, Debug)]
pub struct PresentTarget<'a> {
    pub view: &'a wgpu::TextureView,
    pub format: wgpu::TextureFormat,
    pub width: u32,
    pub height: u32,
    pub clear_color: wgpu::Color,
}

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
