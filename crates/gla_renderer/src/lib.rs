use atlas::TilePos;
use gla_color::BlendMode;

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
}

#[derive(Default)]
pub struct Renderer {
    passes: Vec<Pass>,
}

impl Renderer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn passes(&self) -> &[Pass] {
        &self.passes
    }

    pub fn into_passes(self) -> Vec<Pass> {
        self.passes
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renderer_records_tile_passes_in_order() {
        let src = TilePos::new(0, 1);
        let dst = TilePos::new(0, 2);
        let mut renderer = Renderer::new();

        renderer.clear(dst);
        renderer.copy(src, dst);
        renderer.render_to(src, dst, BlendMode::Multiply, 0.5);

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
            ]
        );
    }
}
