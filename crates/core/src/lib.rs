pub const ATLAS_TILE_SIZE: u32 = 64;
pub const GUTTER_SIZE: u32 = 1;
pub const IMAGE_TILE_SIZE: u32 = ATLAS_TILE_SIZE - 2 * GUTTER_SIZE;

mod color;
mod color_management;
mod vec2;

pub use crate::color::Color;
pub use crate::color_management::{
    AlphaMode, BlendMode, Chromaticity, ColorManagement, ColorManagementError, ColorProfile,
    CpuColorTransform, CpuTransformOptions, CustomRgbProfile, GpuColorSpace, GpuColorTransform,
    GpuColorTransformUniform, GpuTransferCurve, RenderingIntent, RgbPrimaries, SimpleTransferCurve,
};
pub use crate::vec2::{CanvasVec2, RadianVec2, ScreenVec2, Vec2};
