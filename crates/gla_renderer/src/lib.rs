mod gpu;
mod texture;

use atlas::TilePos;
use gla_color::BlendMode;
use std::error::Error;
use std::fmt::{Display, Formatter};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RendererError {
    UnsupportedDrawRadialKernel1D,
}

impl Display for RendererError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedDrawRadialKernel1D => {
                write!(f, "renderer draw_radial_kernel_1d pass is not supported")
            }
        }
    }
}

impl Error for RendererError {}

#[derive(Default)]
pub struct Renderer {
    passes: Vec<Pass>,
    capabilities: RendererCapabilities,
}

impl Renderer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_capabilities(capabilities: RendererCapabilities) -> Self {
        Self {
            passes: Vec::new(),
            capabilities,
        }
    }

    pub fn capabilities(&self) -> RendererCapabilities {
        self.capabilities
    }

    pub fn supports_draw_radial_kernel_1d(&self) -> bool {
        self.capabilities.draw_radial_kernel_1d
    }

    pub fn passes(&self) -> &[Pass] {
        &self.passes
    }

    pub fn into_passes(self) -> Vec<Pass> {
        self.passes
    }

    pub fn clear_passes(&mut self) {
        self.passes.clear();
    }

    pub fn execute(&mut self, gpu: &mut GpuRenderer) -> Result<(), GpuRendererError> {
        gpu.execute_passes(&self.passes)?;
        self.passes.clear();
        Ok(())
    }

    pub fn clear(&mut self, dst: TilePos) {
        self.passes.push(Pass::Clear { dst });
    }

    /// This overwrites the dst tile with the src tile.
    pub fn copy(&mut self, src: TilePos, dst: TilePos) {
        self.passes.push(Pass::Copy { src, dst });
    }

    pub fn render_to(&mut self, src: TilePos, dst: TilePos, blend_mode: BlendMode, opacity: f32) {
        self.passes.push(Pass::RenderTo {
            src,
            dst,
            blend_mode,
            opacity,
        });
    }

    pub fn fix_gutter(&mut self, dst: TilePos) {
        self.passes.push(Pass::FixGutter { dst });
    }

    pub fn draw_radial_kernel_1d(
        &mut self,
        dst: TilePos,
        center_in_tile_x: f32,
        center_in_tile_y: f32,
        radius: f32,
        flow: f32,
    ) -> Result<(), RendererError> {
        if !self.supports_draw_radial_kernel_1d() {
            return Err(RendererError::UnsupportedDrawRadialKernel1D);
        }
        self.passes.push(Pass::DrawRadialKernel1D {
            dst,
            center_in_tile_x,
            center_in_tile_y,
            radius,
            flow,
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renderer_records_tile_passes_in_order() {
        let src = TilePos::new(0, 1);
        let dst = TilePos::new(0, 2);
        let mut renderer = Renderer::with_capabilities(RendererCapabilities {
            draw_radial_kernel_1d: true,
        });

        renderer.clear(dst);
        renderer.copy(src, dst);
        renderer.render_to(src, dst, BlendMode::Multiply, 0.5);
        renderer.render_to(src, dst, BlendMode::MaskAlpha, 1.0);
        renderer
            .draw_radial_kernel_1d(dst, 8.0, 8.0, 12.0, 0.75)
            .unwrap();

        assert_eq!(
            renderer.passes(),
            &[
                Pass::Clear { dst },
                Pass::Copy { src, dst },
                Pass::RenderTo {
                    src,
                    dst,
                    blend_mode: BlendMode::Multiply,
                    opacity: 0.5,
                },
                Pass::RenderTo {
                    src,
                    dst,
                    blend_mode: BlendMode::MaskAlpha,
                    opacity: 1.0,
                },
                Pass::DrawRadialKernel1D {
                    dst,
                    center_in_tile_x: 8.0,
                    center_in_tile_y: 8.0,
                    radius: 12.0,
                    flow: 0.75,
                },
            ]
        );
    }

    #[test]
    fn clear_passes_removes_recorded_work() {
        let mut renderer = Renderer::new();
        renderer.clear(TilePos::new(7, 0));

        assert_eq!(renderer.passes().len(), 1);
        renderer.clear_passes();
        assert!(renderer.passes().is_empty());
    }

    #[test]
    fn radial_kernel_pass_requires_capability() {
        let mut renderer = Renderer::new();
        let err = renderer
            .draw_radial_kernel_1d(TilePos::new(0, 0), 8.0, 8.0, 12.0, 0.75)
            .unwrap_err();

        assert_eq!(err, RendererError::UnsupportedDrawRadialKernel1D);
        assert!(renderer.passes().is_empty());
    }
}
