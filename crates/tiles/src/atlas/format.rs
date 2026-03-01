#[path = "format_core.rs"]
mod format_core;
#[cfg(feature = "atlas-gpu")]
#[path = "format_gpu.rs"]
mod format_gpu;

pub(crate) use format_core::rgba8_tile_len;
pub use format_core::{
    Bgra8Spec, Bgra8SrgbSpec, R32FloatSpec, R8UintSpec, Rgba8Spec, Rgba8SrgbSpec, TileFormatSpec,
    TilePayloadSpec, TileUploadFormatSpec,
};
#[cfg(feature = "atlas-gpu")]
pub use format_gpu::{TileGpuCreateValidator, TileGpuOpAdapter};
