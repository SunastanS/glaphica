use std::error::Error;
use std::fmt::{Display, Formatter};

use glaphica_core::{BrushInput, CanvasVec2, TileKey};

use crate::brush_spec::BrushSpec;
use crate::draw_layout::{BrushDrawInputLayout, BrushDrawInputShape, BrushDrawKind};
use crate::engine_runtime::EngineBrushPipeline;
use crate::gpu_pipeline_spec::BrushGpuPipelineSpec;
use crate::resampler_distance::BrushResamplerDistancePolicy;
use crate::BrushPipelineError;

pub const PIXEL_RECT_DRAW_LAYOUT: BrushDrawInputLayout = BrushDrawInputLayout::new(
    BrushDrawKind::PixelRect,
    &[BrushDrawInputShape::Vec2F32, BrushDrawInputShape::F32],
);

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PixelRectDrawInput {
    pub center_x: f32,
    pub center_y: f32,
    pub radius_px: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelRectDecodeError {
    LayoutKindMismatch {
        provided: BrushDrawKind,
    },
    InputLengthMismatch {
        expected: usize,
        provided: usize,
    },
    MissingSlot {
        slot_index: usize,
    },
    SlotShapeMismatch {
        slot_index: usize,
        expected_lane_count: usize,
        provided_lane_count: usize,
    },
}

impl Display for PixelRectDecodeError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LayoutKindMismatch { provided } => {
                write!(
                    f,
                    "pixel_rect layout kind mismatch, provided layout is {provided:?}"
                )
            }
            Self::InputLengthMismatch { expected, provided } => write!(
                f,
                "pixel_rect draw input length mismatch (expected: {expected}, provided: {provided})"
            ),
            Self::MissingSlot { slot_index } => {
                write!(f, "pixel_rect missing draw input slot {slot_index}")
            }
            Self::SlotShapeMismatch {
                slot_index,
                expected_lane_count,
                provided_lane_count,
            } => write!(
                f,
                "pixel_rect input slot {} lane count mismatch (expected: {}, provided: {})",
                slot_index, expected_lane_count, provided_lane_count
            ),
        }
    }
}

impl Error for PixelRectDecodeError {}

pub fn decode_pixel_rect_draw_input(
    layout: BrushDrawInputLayout,
    input: &[f32],
) -> Result<PixelRectDrawInput, PixelRectDecodeError> {
    if layout.kind() != BrushDrawKind::PixelRect {
        return Err(PixelRectDecodeError::LayoutKindMismatch {
            provided: layout.kind(),
        });
    }
    if !layout.validate_input(input) {
        return Err(PixelRectDecodeError::InputLengthMismatch {
            expected: layout.total_f32_count(),
            provided: input.len(),
        });
    }
    let center_slot = layout
        .slot_slice(input, 0)
        .ok_or(PixelRectDecodeError::MissingSlot { slot_index: 0 })?;
    if center_slot.len() != 2 {
        return Err(PixelRectDecodeError::SlotShapeMismatch {
            slot_index: 0,
            expected_lane_count: 2,
            provided_lane_count: center_slot.len(),
        });
    }

    let radius_slot = layout
        .slot_slice(input, 1)
        .ok_or(PixelRectDecodeError::MissingSlot { slot_index: 1 })?;
    if radius_slot.len() != 1 {
        return Err(PixelRectDecodeError::SlotShapeMismatch {
            slot_index: 1,
            expected_lane_count: 1,
            provided_lane_count: radius_slot.len(),
        });
    }

    Ok(PixelRectDrawInput {
        center_x: center_slot[0],
        center_y: center_slot[1],
        radius_px: radius_slot[0],
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PixelRectBrush {
    radius_px: u32,
}

impl PixelRectBrush {
    pub const CONSTANT_A: f32 = 1.2;
    pub const CONSTANT_B: f32 = 2.0 / 3.0;

    pub const fn new(radius_px: u32) -> Self {
        Self { radius_px }
    }

    pub const fn radius_px(self) -> u32 {
        self.radius_px
    }
}

impl BrushResamplerDistancePolicy for PixelRectBrush {
    fn brush_size(&self) -> u32 {
        self.radius_px
    }

    fn max_distance_rate(&self) -> f32 {
        Self::CONSTANT_A
    }

    fn min_distance_rate(&self) -> f32 {
        Self::CONSTANT_B
    }
}

impl EngineBrushPipeline for PixelRectBrush {
    fn encode_draw_input(
        &mut self,
        brush_input: &BrushInput,
        _tile_key: TileKey,
        tile_canvas_origin: CanvasVec2,
    ) -> Result<Vec<f32>, BrushPipelineError> {
        let local_x = brush_input.cursor.cursor.x - tile_canvas_origin.x;
        let local_y = brush_input.cursor.cursor.y - tile_canvas_origin.y;
        Ok(vec![local_x, local_y, self.radius_px as f32])
    }
}

impl BrushSpec for PixelRectBrush {
    fn max_affected_radius_px(&self) -> u32 {
        self.radius_px
    }

    fn draw_input_layout(&self) -> BrushDrawInputLayout {
        PIXEL_RECT_DRAW_LAYOUT
    }

    fn gpu_pipeline_spec(&self) -> BrushGpuPipelineSpec {
        BrushGpuPipelineSpec {
            label: "pixel-rect-brush",
            wgsl_source: include_str!("pixel_rect.wgsl"),
            vertex_entry: "vs_main",
            fragment_entry: "fs_main",
            uses_brush_cache_backend: false,
            cache_backend_format: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use glaphica_core::{
        BrushId, BrushInput, BrushInputFlags, CanvasVec2, MappedCursor, RadianVec2, StrokeId,
        TileKey,
    };

    use crate::brush_spec::BrushSpec;
    use crate::draw_layout::{BrushDrawInputLayout, BrushDrawInputShape, BrushDrawKind};
    use crate::resampler_distance::BrushResamplerDistancePolicy;
    use crate::{
        BrushEngineRuntime, BrushGpuPipelineRegistry, BrushLayoutRegistry, EngineBrushPipeline,
    };

    use super::{decode_pixel_rect_draw_input, PixelRectBrush, PIXEL_RECT_DRAW_LAYOUT};

    fn build_input(center: CanvasVec2) -> BrushInput {
        BrushInput {
            stroke: StrokeId(9),
            cursor: MappedCursor {
                cursor: center,
                tilt: RadianVec2::new(0.0, 0.0),
                pressure: 1.0,
                twist: 0.0,
            },
            flags: BrushInputFlags::empty(),
            path_s: 0.0,
            delta_s: 0.0,
            dt_s: 0.0,
            vel: CanvasVec2::new(0.0, 0.0),
            speed: 0.0,
            tangent: CanvasVec2::new(0.0, 0.0),
            acc: CanvasVec2::new(0.0, 0.0),
            accel: 0.0,
            curvature: 0.0,
            confidence: 1.0,
        }
    }

    #[test]
    fn encode_draw_input_carries_center_and_radius() {
        let mut brush = PixelRectBrush::new(5);
        let input = build_input(CanvasVec2::new(12.0, 7.0));
        let encoded = brush.encode_draw_input(
            &input,
            TileKey::from_parts(0, 0, 0),
            CanvasVec2::new(0.0, 0.0),
        );
        assert!(encoded.is_ok());
        let encoded = match encoded {
            Ok(encoded) => encoded,
            Err(_) => return,
        };
        assert_eq!(encoded, vec![12.0, 7.0, 5.0]);
    }

    #[test]
    fn encode_draw_input_converts_to_local_coords() {
        let mut brush = PixelRectBrush::new(5);
        let input = build_input(CanvasVec2::new(100.0, 200.0));
        let tile_origin = CanvasVec2::new(64.0, 128.0);
        let encoded = brush.encode_draw_input(&input, TileKey::from_parts(0, 0, 0), tile_origin);
        assert!(encoded.is_ok());
        let encoded = match encoded {
            Ok(encoded) => encoded,
            Err(_) => return,
        };
        assert_eq!(encoded, vec![36.0, 72.0, 5.0]);
    }

    #[test]
    fn decode_resolves_fields_from_layout() {
        let decoded = decode_pixel_rect_draw_input(PIXEL_RECT_DRAW_LAYOUT, &[1.0, 2.0, 3.0]);
        assert!(decoded.is_ok());
        let decoded = match decoded {
            Ok(decoded) => decoded,
            Err(_) => return,
        };
        assert_eq!(decoded.center_x, 1.0);
        assert_eq!(decoded.center_y, 2.0);
        assert_eq!(decoded.radius_px, 3.0);
    }

    #[test]
    fn decode_rejects_layout_without_radius_field() {
        let layout =
            BrushDrawInputLayout::new(BrushDrawKind::PixelRect, &[BrushDrawInputShape::Vec2F32]);
        let decoded = decode_pixel_rect_draw_input(layout, &[1.0, 2.0]);
        assert!(decoded.is_err());
    }

    #[test]
    fn brush_spec_registers_pixel_rect_engine_and_layout() {
        let mut engine_runtime = BrushEngineRuntime::new(4);
        let mut layouts = BrushLayoutRegistry::new(4);
        let mut gpu_pipeline_registry = BrushGpuPipelineRegistry::new(4);
        let register = PixelRectBrush::new(3).register(
            BrushId(1),
            &mut engine_runtime,
            &mut layouts,
            &mut gpu_pipeline_registry,
        );
        assert!(register.is_ok());
        assert_eq!(layouts.layout(BrushId(1)), Ok(PIXEL_RECT_DRAW_LAYOUT));
        assert!(gpu_pipeline_registry.pipeline_spec(BrushId(1)).is_ok());
    }

    #[test]
    fn resampler_distance_scales_with_brush_size() {
        let brush = PixelRectBrush::new(3);
        let distance = brush.resampler_distance();
        assert!((distance.max_distance - 3.6).abs() < 0.0001);
        assert!((distance.min_distance - 2.0).abs() < 0.0001);
    }
}
