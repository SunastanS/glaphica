pub const ATLAS_TILE_SIZE: u32 = 64;
pub const GUTTER_SIZE: u32 = 1;
pub const IMAGE_TILE_SIZE: u32 = ATLAS_TILE_SIZE - 2 * GUTTER_SIZE;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(transparent)]
pub struct BrushId(u64);

impl BrushId {
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CanvasInput {
    pub time_ns: u64,
    pub position: CanvasVec2,
    pub pressure: f32,
    pub tilt: RadianVec2,
    pub twist: f32,
}

mod color;
mod color_management;
pub mod tile_command;
mod vec2;

pub use crate::color::Color;
pub use crate::color_management::{
    AlphaMode, BlendMode, Chromaticity, ColorManagement, ColorManagementError, ColorProfile,
    CpuColorTransform, CpuTransformOptions, CustomRgbProfile, GpuColorSpace, GpuColorTransform,
    GpuColorTransformUniform, GpuTransferCurve, RenderingIntent, RgbPrimaries, SimpleTransferCurve,
};
pub use crate::tile_command::CopyTileCommand;
pub use crate::vec2::{CanvasVec2, RadianVec2, ScreenVec2, Vec2};
