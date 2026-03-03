use std::error::Error;
use std::fmt::{Display, Formatter};

use brushes::builtin_brushes::pixel_rect::decode_pixel_rect_draw_input;
use brushes::{BrushDrawInputLayout, BrushDrawKind, BrushGpuPipelineSpec, BrushPipelineError};
use glaphica_core::TileKey;
use thread_protocol::DrawOp;

use crate::brush_runtime::BrushDrawExecutor;

pub trait GenericBrushTarget {
    fn width(&self) -> u32;
    fn height(&self) -> u32;
    fn write_pixel(&mut self, x: u32, y: u32);
    fn sample_ref_pixel(&self, tile_key: TileKey, x: u32, y: u32) -> Option<[f32; 4]>;
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct GenericBrushExecutor;

#[derive(Debug)]
enum GenericBrushExecutorError {
    PixelRectDecode(brushes::builtin_brushes::pixel_rect::PixelRectDecodeError),
}

impl Display for GenericBrushExecutorError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PixelRectDecode(err) => {
                write!(f, "failed to decode pixel_rect draw input: {err}")
            }
        }
    }
}

impl Error for GenericBrushExecutorError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::PixelRectDecode(err) => Some(err),
        }
    }
}

impl<Target> BrushDrawExecutor<Target> for GenericBrushExecutor
where
    Target: GenericBrushTarget,
{
    fn execute_draw(
        &mut self,
        target: &mut Target,
        draw_op: &thread_protocol::DrawOp,
        layout: BrushDrawInputLayout,
        _pipeline_spec: BrushGpuPipelineSpec,
    ) -> Result<(), BrushPipelineError> {
        match layout.kind() {
            BrushDrawKind::PixelRect => {
                let decoded = decode_pixel_rect_draw_input(layout, &draw_op.input)
                    .map_err(GenericBrushExecutorError::PixelRectDecode)?;
                draw_square(
                    target,
                    decoded.center_x.round() as i64,
                    decoded.center_y.round() as i64,
                    decoded.radius_px.round().max(0.0) as i64,
                );
                Ok(())
            }
        }
    }
}

pub fn sample_ref_image_pixel<Target>(
    target: &Target,
    draw_op: &DrawOp,
    x: u32,
    y: u32,
) -> Option<[f32; 4]>
where
    Target: GenericBrushTarget,
{
    let ref_image = draw_op.ref_image?;
    target.sample_ref_pixel(ref_image.tile_key, x, y)
}

fn draw_square<Target>(target: &mut Target, center_x: i64, center_y: i64, radius: i64)
where
    Target: GenericBrushTarget,
{
    let width = i64::from(target.width());
    let height = i64::from(target.height());
    if width == 0 || height == 0 {
        return;
    }

    let min_x = center_x - radius;
    let max_x = center_x + radius;
    let min_y = center_y - radius;
    let max_y = center_y + radius;

    let max_pixel_x = width - 1;
    let max_pixel_y = height - 1;
    if max_x < 0 || max_y < 0 || min_x > max_pixel_x || min_y > max_pixel_y {
        return;
    }

    let clamped_min_x = min_x.clamp(0, max_pixel_x) as u32;
    let clamped_max_x = max_x.clamp(0, max_pixel_x) as u32;
    let clamped_min_y = min_y.clamp(0, max_pixel_y) as u32;
    let clamped_max_y = max_y.clamp(0, max_pixel_y) as u32;

    for y in clamped_min_y..=clamped_max_y {
        for x in clamped_min_x..=clamped_max_x {
            target.write_pixel(x, y);
        }
    }
}

#[cfg(test)]
mod tests {
    use brushes::builtin_brushes::pixel_rect::PixelRectBrush;
    use brushes::{BrushEngineRuntime, BrushGpuPipelineRegistry, BrushLayoutRegistry, BrushSpec};
    use glaphica_core::{BrushId, TileKey};
    use thread_protocol::{DrawOp, RefImage};

    use crate::brush_runtime::BrushGpuRuntime;

    use super::{GenericBrushExecutor, GenericBrushTarget};

    struct MaskTarget {
        width: u32,
        height: u32,
        pixels: Vec<u8>,
    }

    impl MaskTarget {
        fn new(width: u32, height: u32) -> Self {
            Self {
                width,
                height,
                pixels: vec![0; (width * height) as usize],
            }
        }

        fn is_set(&self, x: u32, y: u32) -> bool {
            self.pixels[(y * self.width + x) as usize] == 1
        }
    }

    impl GenericBrushTarget for MaskTarget {
        fn width(&self) -> u32 {
            self.width
        }

        fn height(&self) -> u32 {
            self.height
        }

        fn write_pixel(&mut self, x: u32, y: u32) {
            let index = (y * self.width + x) as usize;
            self.pixels[index] = 1;
        }

        fn sample_ref_pixel(&self, tile_key: TileKey, x: u32, y: u32) -> Option<[f32; 4]> {
            if tile_key == TileKey::from_parts(9, 9, 9) && x == 4 && y == 3 {
                return Some([0.2, 0.3, 0.4, 0.5]);
            }
            None
        }
    }

    #[test]
    fn generic_executor_draws_pixel_rect_square() {
        let mut runtime = BrushGpuRuntime::new(GenericBrushExecutor);
        let mut engine_runtime = BrushEngineRuntime::new(4);
        let mut layouts = BrushLayoutRegistry::new(4);
        let mut pipeline_registry = BrushGpuPipelineRegistry::new(4);
        let register = PixelRectBrush::new(1).register(
            BrushId(1),
            &mut engine_runtime,
            &mut layouts,
            &mut pipeline_registry,
        );
        assert!(register.is_ok());

        let draw_op = DrawOp {
            tile_key: TileKey::from_parts(0, 0, 0),
            ref_image: None,
            input: vec![4.0, 3.0, 1.0],
            brush_id: BrushId(1),
        };
        let mut target = MaskTarget::new(9, 7);
        let apply = runtime.apply_draw_op(&mut target, &draw_op, &layouts, &pipeline_registry);
        assert!(apply.is_ok());

        for y in 0..7 {
            for x in 0..9 {
                let expected = (3..=5).contains(&x) && (2..=4).contains(&y);
                assert_eq!(target.is_set(x, y), expected);
            }
        }
    }

    #[test]
    fn sample_ref_image_pixel_reads_from_ref_tile_key_when_present() {
        let target = MaskTarget::new(2, 2);
        let draw_op = DrawOp {
            tile_key: TileKey::from_parts(1, 1, 1),
            ref_image: Some(RefImage {
                tile_key: TileKey::from_parts(9, 9, 9),
            }),
            input: vec![0.0, 0.0, 0.0],
            brush_id: BrushId(1),
        };
        let sampled = super::sample_ref_image_pixel(&target, &draw_op, 4, 3);
        assert_eq!(sampled, Some([0.2, 0.3, 0.4, 0.5]));
    }
}
