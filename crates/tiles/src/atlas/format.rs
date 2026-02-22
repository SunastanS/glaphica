#[path = "format_core.rs"]
mod format_core;
#[cfg(feature = "atlas-gpu")]
#[path = "format_gpu.rs"]
mod format_gpu;

pub use format_core::{
    R32FloatSpec, R8UintSpec, Rgba8Spec, Rgba8SrgbSpec, TileFormatSpec, TilePayloadSpec,
    TileUploadFormatSpec,
};
pub(crate) use format_core::rgba8_tile_len;
#[cfg(feature = "atlas-gpu")]
pub use format_gpu::{TileGpuCreateValidator, TileGpuOpAdapter};
