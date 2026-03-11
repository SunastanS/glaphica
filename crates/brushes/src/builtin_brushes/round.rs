use std::error::Error;
use std::f32::consts::{FRAC_PI_2, PI};
use std::fmt::{Display, Formatter};

use glaphica_core::{BackendKind, BrushInput, CanvasVec2, RadianVec2, TextureFormat, TileKey};
use thread_protocol::GpuCmdFrameMergeTag;

use crate::BrushPipelineError;
use crate::brush_spec::BrushSpec;
use crate::config::{
    BrushConfigItem, BrushConfigKind, BrushConfigValue, UnitIntervalPoint,
    eval_unit_interval_curve_polynomial,
};
use crate::draw_layout::{BrushDrawInputLayout, BrushDrawInputShape, BrushDrawKind};
use crate::engine_runtime::EngineBrushPipeline;
use crate::gpu_pipeline_spec::BrushGpuPipelineSpec;
use crate::resampler_distance::BrushResamplerDistancePolicy;

pub const ROUND_DRAW_LAYOUT: BrushDrawInputLayout = BrushDrawInputLayout::new(
    BrushDrawKind::Round,
    &[
        BrushDrawInputShape::Vec2F32,
        BrushDrawInputShape::F32,
        BrushDrawInputShape::F32,
        BrushDrawInputShape::F32,
        BrushDrawInputShape::F32,
    ],
);

const ROUND_STAGE_BUFFER_DAB: f32 = 0.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RoundDrawInput {
    pub center_x: f32,
    pub center_y: f32,
    pub radius_px: f32,
    pub hardness: f32,
    pub opacity: f32,
    pub stage: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoundDecodeError {
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

impl Display for RoundDecodeError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LayoutKindMismatch { provided } => {
                write!(
                    f,
                    "round layout kind mismatch, provided layout is {provided:?}"
                )
            }
            Self::InputLengthMismatch { expected, provided } => write!(
                f,
                "round draw input length mismatch (expected: {expected}, provided: {provided})"
            ),
            Self::MissingSlot { slot_index } => {
                write!(f, "round missing draw input slot {slot_index}")
            }
            Self::SlotShapeMismatch {
                slot_index,
                expected_lane_count,
                provided_lane_count,
            } => write!(
                f,
                "round input slot {} lane count mismatch (expected: {}, provided: {})",
                slot_index, expected_lane_count, provided_lane_count
            ),
        }
    }
}

impl Error for RoundDecodeError {}

pub fn decode_round_draw_input(
    layout: BrushDrawInputLayout,
    input: &[f32],
) -> Result<RoundDrawInput, RoundDecodeError> {
    if layout.kind() != BrushDrawKind::Round {
        return Err(RoundDecodeError::LayoutKindMismatch {
            provided: layout.kind(),
        });
    }
    if !layout.validate_input(input) {
        return Err(RoundDecodeError::InputLengthMismatch {
            expected: layout.total_f32_count(),
            provided: input.len(),
        });
    }

    let center_slot = layout
        .slot_slice(input, 0)
        .ok_or(RoundDecodeError::MissingSlot { slot_index: 0 })?;
    if center_slot.len() != 2 {
        return Err(RoundDecodeError::SlotShapeMismatch {
            slot_index: 0,
            expected_lane_count: 2,
            provided_lane_count: center_slot.len(),
        });
    }

    let radius_slot = layout
        .slot_slice(input, 1)
        .ok_or(RoundDecodeError::MissingSlot { slot_index: 1 })?;
    if radius_slot.len() != 1 {
        return Err(RoundDecodeError::SlotShapeMismatch {
            slot_index: 1,
            expected_lane_count: 1,
            provided_lane_count: radius_slot.len(),
        });
    }

    let hardness_slot = layout
        .slot_slice(input, 2)
        .ok_or(RoundDecodeError::MissingSlot { slot_index: 2 })?;
    if hardness_slot.len() != 1 {
        return Err(RoundDecodeError::SlotShapeMismatch {
            slot_index: 2,
            expected_lane_count: 1,
            provided_lane_count: hardness_slot.len(),
        });
    }

    let opacity_slot = layout
        .slot_slice(input, 3)
        .ok_or(RoundDecodeError::MissingSlot { slot_index: 3 })?;
    if opacity_slot.len() != 1 {
        return Err(RoundDecodeError::SlotShapeMismatch {
            slot_index: 3,
            expected_lane_count: 1,
            provided_lane_count: opacity_slot.len(),
        });
    }

    let stage_slot = layout
        .slot_slice(input, 4)
        .ok_or(RoundDecodeError::MissingSlot { slot_index: 4 })?;
    if stage_slot.len() != 1 {
        return Err(RoundDecodeError::SlotShapeMismatch {
            slot_index: 4,
            expected_lane_count: 1,
            provided_lane_count: stage_slot.len(),
        });
    }

    Ok(RoundDrawInput {
        center_x: center_slot[0],
        center_y: center_slot[1],
        radius_px: radius_slot[0],
        hardness: hardness_slot[0],
        opacity: opacity_slot[0],
        stage: stage_slot[0],
    })
}

pub type CurvePoint = UnitIntervalPoint;

#[derive(Debug, Clone, PartialEq)]
pub struct ModulationCurve {
    points: Vec<CurvePoint>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CurveValidationError {
    TooFewPoints,
    PointOutOfRange { index: usize },
    NotMonotonic { index: usize },
}

impl Display for CurveValidationError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooFewPoints => write!(f, "curve requires at least two control points"),
            Self::PointOutOfRange { index } => {
                write!(f, "curve point {index} is out of [0, 1] range")
            }
            Self::NotMonotonic { index } => write!(
                f,
                "curve points must be monotonically increasing, violated at index {index}"
            ),
        }
    }
}

impl Error for CurveValidationError {}

impl ModulationCurve {
    pub fn new(points: Vec<CurvePoint>) -> Result<Self, CurveValidationError> {
        Self::validate_points(&points)?;
        Ok(Self { points })
    }

    pub fn flat_one() -> Self {
        Self {
            points: vec![CurvePoint::new(0.0, 1.0), CurvePoint::new(1.0, 1.0)],
        }
    }

    pub fn sample(&self, x: f32) -> f32 {
        eval_unit_interval_curve_polynomial(&self.points, x).unwrap_or(1.0)
    }

    pub fn points(&self) -> &[CurvePoint] {
        &self.points
    }

    fn validate_points(points: &[CurvePoint]) -> Result<(), CurveValidationError> {
        if points.len() < 2 {
            return Err(CurveValidationError::TooFewPoints);
        }

        let mut prev_x = 0.0f32;
        let mut first = true;
        for (index, point) in points.iter().enumerate() {
            if !(0.0..=1.0).contains(&point.x) || !(0.0..=1.0).contains(&point.y) {
                return Err(CurveValidationError::PointOutOfRange { index });
            }
            if first {
                prev_x = point.x;
                first = false;
                continue;
            }
            if point.x <= prev_x {
                return Err(CurveValidationError::NotMonotonic { index });
            }
            prev_x = point.x;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RoundBrushCurves {
    pub pressure_to_radius: ModulationCurve,
    pub pressure_to_hardness: ModulationCurve,
    pub pressure_to_opacity: ModulationCurve,
    pub tilt_to_radius: ModulationCurve,
    pub tilt_to_hardness: ModulationCurve,
    pub tilt_to_opacity: ModulationCurve,
    pub twist_to_radius: ModulationCurve,
    pub twist_to_hardness: ModulationCurve,
    pub twist_to_opacity: ModulationCurve,
    pub speed_to_radius: ModulationCurve,
    pub speed_to_hardness: ModulationCurve,
    pub speed_to_opacity: ModulationCurve,
}

impl Default for RoundBrushCurves {
    fn default() -> Self {
        Self {
            pressure_to_radius: ModulationCurve::flat_one(),
            pressure_to_hardness: ModulationCurve::flat_one(),
            pressure_to_opacity: ModulationCurve::flat_one(),
            tilt_to_radius: ModulationCurve::flat_one(),
            tilt_to_hardness: ModulationCurve::flat_one(),
            tilt_to_opacity: ModulationCurve::flat_one(),
            twist_to_radius: ModulationCurve::flat_one(),
            twist_to_hardness: ModulationCurve::flat_one(),
            twist_to_opacity: ModulationCurve::flat_one(),
            speed_to_radius: ModulationCurve::flat_one(),
            speed_to_hardness: ModulationCurve::flat_one(),
            speed_to_opacity: ModulationCurve::flat_one(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RoundBrush {
    base_radius_px: f32,
    base_hardness: f32,
    base_opacity: f32,
    curves: RoundBrushCurves,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoundBrushConfigError {
    RadiusMustBePositive,
    HardnessOutOfRange,
    OpacityOutOfRange,
    InvalidConfigLength,
    ConfigTypeMismatch,
    CurveInvalid,
}

impl Display for RoundBrushConfigError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RadiusMustBePositive => write!(f, "round brush radius must be > 0"),
            Self::HardnessOutOfRange => write!(f, "round brush hardness must be in [0, 1]"),
            Self::OpacityOutOfRange => write!(f, "round brush opacity must be in [0, 1]"),
            Self::InvalidConfigLength => write!(f, "round brush config item count is invalid"),
            Self::ConfigTypeMismatch => write!(f, "round brush config value type mismatch"),
            Self::CurveInvalid => write!(f, "round brush curve config is invalid"),
        }
    }
}

impl Error for RoundBrushConfigError {}

impl RoundBrush {
    pub const CONSTANT_A: f32 = 1.2;
    pub const CONSTANT_B: f32 = 2.0 / 3.0;

    pub fn new(
        base_radius_px: f32,
        base_hardness: f32,
        curves: RoundBrushCurves,
    ) -> Result<Self, RoundBrushConfigError> {
        Self::new_with_opacity(base_radius_px, base_hardness, 1.0, curves)
    }

    pub fn new_with_opacity(
        base_radius_px: f32,
        base_hardness: f32,
        base_opacity: f32,
        curves: RoundBrushCurves,
    ) -> Result<Self, RoundBrushConfigError> {
        if base_radius_px <= 0.0 {
            return Err(RoundBrushConfigError::RadiusMustBePositive);
        }
        if !(0.0..=1.0).contains(&base_hardness) {
            return Err(RoundBrushConfigError::HardnessOutOfRange);
        }
        if !(0.0..=1.0).contains(&base_opacity) {
            return Err(RoundBrushConfigError::OpacityOutOfRange);
        }
        Ok(Self {
            base_radius_px,
            base_hardness,
            base_opacity,
            curves,
        })
    }

    pub fn with_default_curves(
        base_radius_px: f32,
        base_hardness: f32,
    ) -> Result<Self, RoundBrushConfigError> {
        Self::new(base_radius_px, base_hardness, RoundBrushCurves::default())
    }

    pub fn config_items(&self) -> Vec<BrushConfigItem> {
        vec![
            BrushConfigItem {
                key: "base_radius_px",
                label: "Base Radius",
                default_hidden: false,
                kind: BrushConfigKind::ScalarF32 {
                    min: 0.1,
                    max: 128.0,
                },
                default_value: BrushConfigValue::ScalarF32(self.base_radius_px),
            },
            BrushConfigItem {
                key: "base_hardness",
                label: "Base Hardness",
                default_hidden: false,
                kind: BrushConfigKind::ScalarF32 { min: 0.0, max: 1.0 },
                default_value: BrushConfigValue::ScalarF32(self.base_hardness),
            },
            BrushConfigItem {
                key: "base_opacity",
                label: "Base Opacity",
                default_hidden: false,
                kind: BrushConfigKind::ScalarF32 { min: 0.0, max: 1.0 },
                default_value: BrushConfigValue::ScalarF32(self.base_opacity),
            },
            curve_item(
                "pressure_to_radius",
                "Pressure -> Radius",
                &self.curves.pressure_to_radius,
            ),
            curve_item(
                "pressure_to_hardness",
                "Pressure -> Hardness",
                &self.curves.pressure_to_hardness,
            ),
            curve_item(
                "pressure_to_opacity",
                "Pressure -> Opacity",
                &self.curves.pressure_to_opacity,
            ),
            curve_item(
                "tilt_to_radius",
                "Tilt -> Radius",
                &self.curves.tilt_to_radius,
            ),
            curve_item(
                "tilt_to_hardness",
                "Tilt -> Hardness",
                &self.curves.tilt_to_hardness,
            ),
            curve_item(
                "tilt_to_opacity",
                "Tilt -> Opacity",
                &self.curves.tilt_to_opacity,
            ),
            curve_item(
                "twist_to_radius",
                "Twist -> Radius",
                &self.curves.twist_to_radius,
            ),
            curve_item(
                "twist_to_hardness",
                "Twist -> Hardness",
                &self.curves.twist_to_hardness,
            ),
            curve_item(
                "twist_to_opacity",
                "Twist -> Opacity",
                &self.curves.twist_to_opacity,
            ),
            curve_item(
                "speed_to_radius",
                "Speed -> Radius",
                &self.curves.speed_to_radius,
            ),
            curve_item(
                "speed_to_hardness",
                "Speed -> Hardness",
                &self.curves.speed_to_hardness,
            ),
            curve_item(
                "speed_to_opacity",
                "Speed -> Opacity",
                &self.curves.speed_to_opacity,
            ),
        ]
    }

    pub fn from_config_values(values: &[BrushConfigValue]) -> Result<Self, RoundBrushConfigError> {
        if values.len() != 15 {
            return Err(RoundBrushConfigError::InvalidConfigLength);
        }
        let base_radius_px = match values.first() {
            Some(BrushConfigValue::ScalarF32(value)) => *value,
            _ => return Err(RoundBrushConfigError::ConfigTypeMismatch),
        };
        let base_hardness = match values.get(1) {
            Some(BrushConfigValue::ScalarF32(value)) => *value,
            _ => return Err(RoundBrushConfigError::ConfigTypeMismatch),
        };
        let base_opacity = match values.get(2) {
            Some(BrushConfigValue::ScalarF32(value)) => *value,
            _ => return Err(RoundBrushConfigError::ConfigTypeMismatch),
        };
        let pressure_to_radius = curve_from_value(values.get(3))?;
        let pressure_to_hardness = curve_from_value(values.get(4))?;
        let pressure_to_opacity = curve_from_value(values.get(5))?;
        let tilt_to_radius = curve_from_value(values.get(6))?;
        let tilt_to_hardness = curve_from_value(values.get(7))?;
        let tilt_to_opacity = curve_from_value(values.get(8))?;
        let twist_to_radius = curve_from_value(values.get(9))?;
        let twist_to_hardness = curve_from_value(values.get(10))?;
        let twist_to_opacity = curve_from_value(values.get(11))?;
        let speed_to_radius = curve_from_value(values.get(12))?;
        let speed_to_hardness = curve_from_value(values.get(13))?;
        let speed_to_opacity = curve_from_value(values.get(14))?;
        Self::new_with_opacity(
            base_radius_px,
            base_hardness,
            base_opacity,
            RoundBrushCurves {
                pressure_to_radius,
                pressure_to_hardness,
                pressure_to_opacity,
                tilt_to_radius,
                tilt_to_hardness,
                tilt_to_opacity,
                twist_to_radius,
                twist_to_hardness,
                twist_to_opacity,
                speed_to_radius,
                speed_to_hardness,
                speed_to_opacity,
            },
        )
    }

    fn modulated_radius_px(&self, brush_input: &BrushInput) -> f32 {
        let pressure = clamp01(brush_input.cursor.pressure);
        let tilt = normalize_tilt(brush_input.cursor.tilt);
        let twist = normalize_twist(brush_input.cursor.twist);
        let speed = normalize_speed(brush_input.speed);
        self.base_radius_px
            * self.curves.pressure_to_radius.sample(pressure)
            * self.curves.tilt_to_radius.sample(tilt)
            * self.curves.twist_to_radius.sample(twist)
            * self.curves.speed_to_radius.sample(speed)
    }

    fn modulated_hardness(&self, brush_input: &BrushInput) -> f32 {
        let pressure = clamp01(brush_input.cursor.pressure);
        let tilt = normalize_tilt(brush_input.cursor.tilt);
        let twist = normalize_twist(brush_input.cursor.twist);
        let speed = normalize_speed(brush_input.speed);
        clamp01(
            self.base_hardness
                * self.curves.pressure_to_hardness.sample(pressure)
                * self.curves.tilt_to_hardness.sample(tilt)
                * self.curves.twist_to_hardness.sample(twist)
                * self.curves.speed_to_hardness.sample(speed),
        )
    }

    fn modulated_input_opacity(&self, brush_input: &BrushInput) -> f32 {
        let pressure = clamp01(brush_input.cursor.pressure);
        let tilt = normalize_tilt(brush_input.cursor.tilt);
        let twist = normalize_twist(brush_input.cursor.twist);
        let speed = normalize_speed(brush_input.speed);
        clamp01(
            self.curves.pressure_to_opacity.sample(pressure)
                * self.curves.tilt_to_opacity.sample(tilt)
                * self.curves.twist_to_opacity.sample(twist)
                * self.curves.speed_to_opacity.sample(speed),
        )
    }
}

impl BrushResamplerDistancePolicy for RoundBrush {
    fn brush_size(&self) -> u32 {
        self.base_radius_px.ceil().max(1.0) as u32
    }

    fn max_distance_rate(&self) -> f32 {
        Self::CONSTANT_A
    }

    fn min_distance_rate(&self) -> f32 {
        Self::CONSTANT_B
    }
}

impl EngineBrushPipeline for RoundBrush {
    fn uses_stroke_buffer(&self) -> bool {
        true
    }

    fn encode_draw_input(
        &mut self,
        brush_input: &BrushInput,
        _tile_key: TileKey,
        tile_canvas_origin: CanvasVec2,
    ) -> Result<Vec<f32>, BrushPipelineError> {
        let local_x = brush_input.cursor.cursor.x - tile_canvas_origin.x;
        let local_y = brush_input.cursor.cursor.y - tile_canvas_origin.y;
        let radius_px = self.modulated_radius_px(brush_input);
        let hardness = self.modulated_hardness(brush_input);
        let opacity = self.base_opacity * self.modulated_input_opacity(brush_input);
        Ok(vec![
            local_x,
            local_y,
            radius_px,
            hardness,
            opacity,
            ROUND_STAGE_BUFFER_DAB,
        ])
    }

    fn encode_stroke_buffer_dab_input(
        &mut self,
        brush_input: &BrushInput,
        _tile_key: TileKey,
        tile_canvas_origin: CanvasVec2,
    ) -> Result<Vec<f32>, BrushPipelineError> {
        let local_x = brush_input.cursor.cursor.x - tile_canvas_origin.x;
        let local_y = brush_input.cursor.cursor.y - tile_canvas_origin.y;
        let radius_px = self.modulated_radius_px(brush_input);
        let hardness = self.modulated_hardness(brush_input);
        // Stroke buffer stores cumulative optical thickness for this stroke+tile.
        // Runtime must restore origin A -> target B (Copy) before each Write(T(C)) to B,
        // otherwise rewriting onto an already written B+ would double-apply transmission.
        let opacity = self.modulated_input_opacity(brush_input);
        Ok(vec![
            local_x,
            local_y,
            radius_px,
            hardness,
            opacity,
            ROUND_STAGE_BUFFER_DAB,
        ])
    }

    fn stroke_buffer_write_opacity(
        &mut self,
        _brush_input: &BrushInput,
    ) -> Result<f32, BrushPipelineError> {
        Ok(self.base_opacity)
    }

    fn stroke_buffer_copy_frame_merge_tag(&self) -> GpuCmdFrameMergeTag {
        GpuCmdFrameMergeTag::KeepFirstInFrameByDstTile
    }

    fn stroke_buffer_write_frame_merge_tag(&self) -> GpuCmdFrameMergeTag {
        GpuCmdFrameMergeTag::KeepLastInFrameByDstTile
    }

    fn restore_origin_before_each_dab(&self) -> bool {
        true
    }
}

impl BrushSpec for RoundBrush {
    fn max_affected_radius_px(&self) -> u32 {
        self.base_radius_px.ceil().max(1.0) as u32
    }

    fn draw_input_layout(&self) -> BrushDrawInputLayout {
        ROUND_DRAW_LAYOUT
    }

    fn gpu_pipeline_spec(&self) -> BrushGpuPipelineSpec {
        BrushGpuPipelineSpec {
            label: "round-brush",
            wgsl_source: include_str!("round.wgsl"),
            vertex_entry: "vs_main",
            fragment_entry: "fs_main",
            uses_brush_cache_backend: true,
            cache_backend_format: Some(TextureFormat::Rgba16Float),
        }
    }

    fn cache_backend_kind(&self) -> Option<BackendKind> {
        Some(BackendKind::Leaf)
    }
}

fn clamp01(v: f32) -> f32 {
    v.clamp(0.0, 1.0)
}

fn normalize_tilt(tilt: RadianVec2) -> f32 {
    let magnitude = (tilt.x * tilt.x + tilt.y * tilt.y).sqrt();
    clamp01(magnitude / FRAC_PI_2)
}

fn normalize_twist(twist: f32) -> f32 {
    clamp01((twist + PI) / (2.0 * PI))
}

fn normalize_speed(speed: f32) -> f32 {
    const MAX_SPEED: f32 = 1000.0;
    clamp01(speed / MAX_SPEED)
}

fn curve_item(key: &'static str, label: &'static str, curve: &ModulationCurve) -> BrushConfigItem {
    BrushConfigItem {
        key,
        label,
        default_hidden: true,
        kind: BrushConfigKind::UnitIntervalCurve,
        default_value: BrushConfigValue::UnitIntervalCurve(curve.points().to_vec()),
    }
}

fn curve_from_value(
    value: Option<&BrushConfigValue>,
) -> Result<ModulationCurve, RoundBrushConfigError> {
    let points = match value {
        Some(BrushConfigValue::UnitIntervalCurve(points)) => points,
        _ => return Err(RoundBrushConfigError::ConfigTypeMismatch),
    };
    ModulationCurve::new(points.clone()).map_err(|_| RoundBrushConfigError::CurveInvalid)
}

#[cfg(test)]
mod tests {
    use std::f32::consts::{FRAC_PI_2, PI};

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

    use super::{
        CurvePoint, ModulationCurve, ROUND_DRAW_LAYOUT, RoundBrush, RoundBrushCurves,
        decode_round_draw_input,
    };

    fn build_input(center: CanvasVec2, pressure: f32, tilt: RadianVec2, twist: f32) -> BrushInput {
        BrushInput {
            stroke: StrokeId(9),
            cursor: MappedCursor {
                cursor: center,
                tilt,
                pressure,
                twist,
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
    fn decode_resolves_fields_from_layout() {
        let decoded = decode_round_draw_input(ROUND_DRAW_LAYOUT, &[1.0, 2.0, 3.0, 0.4, 0.9, 1.0]);
        assert!(decoded.is_ok());
        let decoded = match decoded {
            Ok(decoded) => decoded,
            Err(_) => return,
        };
        assert_eq!(decoded.center_x, 1.0);
        assert_eq!(decoded.center_y, 2.0);
        assert_eq!(decoded.radius_px, 3.0);
        assert_eq!(decoded.hardness, 0.4);
        assert_eq!(decoded.opacity, 0.9);
        assert_eq!(decoded.stage, 1.0);
    }

    #[test]
    fn decode_rejects_layout_without_hardness_field() {
        let layout = BrushDrawInputLayout::new(
            BrushDrawKind::Round,
            &[
                BrushDrawInputShape::Vec2F32,
                BrushDrawInputShape::F32,
                BrushDrawInputShape::F32,
            ],
        );
        let decoded = decode_round_draw_input(layout, &[1.0, 2.0, 3.0, 0.4]);
        assert!(decoded.is_err());
    }

    #[test]
    fn curve_sample_fits_line_from_two_points() {
        let curve =
            ModulationCurve::new(vec![CurvePoint::new(0.0, 0.2), CurvePoint::new(1.0, 1.0)]);
        assert!(curve.is_ok());
        let curve = match curve {
            Ok(curve) => curve,
            Err(_) => return,
        };
        let sampled = curve.sample(0.25);
        assert!((sampled - 0.4).abs() < 0.0001);
    }

    #[test]
    fn encode_draw_input_applies_modulation_curves() {
        let half_curve =
            ModulationCurve::new(vec![CurvePoint::new(0.0, 0.5), CurvePoint::new(1.0, 0.5)]);
        assert!(half_curve.is_ok());
        let half_curve = match half_curve {
            Ok(curve) => curve,
            Err(_) => return,
        };
        let curves = RoundBrushCurves {
            pressure_to_radius: half_curve.clone(),
            pressure_to_hardness: half_curve.clone(),
            pressure_to_opacity: half_curve.clone(),
            tilt_to_radius: half_curve.clone(),
            tilt_to_hardness: half_curve.clone(),
            tilt_to_opacity: half_curve.clone(),
            twist_to_radius: half_curve.clone(),
            twist_to_hardness: half_curve.clone(),
            twist_to_opacity: half_curve.clone(),
            speed_to_radius: half_curve.clone(),
            speed_to_hardness: half_curve,
            speed_to_opacity: ModulationCurve::flat_one(),
        };
        let brush = RoundBrush::new_with_opacity(8.0, 0.8, 0.9, curves);
        assert!(brush.is_ok());
        let mut brush = match brush {
            Ok(brush) => brush,
            Err(_) => return,
        };
        let input = build_input(
            CanvasVec2::new(100.0, 200.0),
            1.0,
            RadianVec2::new(FRAC_PI_2, 0.0),
            PI,
        );

        let encoded = brush.encode_draw_input(
            &input,
            TileKey::from_parts(0, 0, 0),
            CanvasVec2::new(64.0, 128.0),
        );
        assert!(encoded.is_ok());
        let encoded = match encoded {
            Ok(encoded) => encoded,
            Err(_) => return,
        };
        assert_eq!(encoded[0], 36.0);
        assert_eq!(encoded[1], 72.0);
        assert!((encoded[2] - 0.5).abs() < 0.0001);
        assert!((encoded[3] - 0.05).abs() < 0.0001);
        assert!((encoded[4] - 0.1125).abs() < 0.0001);
        assert_eq!(encoded[5], super::ROUND_STAGE_BUFFER_DAB);
    }

    #[test]
    fn brush_spec_registers_round_engine_and_layout() {
        let mut engine_runtime = BrushEngineRuntime::new(4);
        let mut layouts = BrushLayoutRegistry::new(4);
        let mut gpu_pipeline_registry = BrushGpuPipelineRegistry::new(4);
        let brush = RoundBrush::with_default_curves(5.0, 0.7);
        assert!(brush.is_ok());
        let brush = match brush {
            Ok(brush) => brush,
            Err(_) => return,
        };

        let register = brush.register(
            BrushId(1),
            &mut engine_runtime,
            &mut layouts,
            &mut gpu_pipeline_registry,
        );
        assert!(register.is_ok());
        assert_eq!(layouts.layout(BrushId(1)), Ok(ROUND_DRAW_LAYOUT));
        assert!(gpu_pipeline_registry.pipeline_spec(BrushId(1)).is_ok());
    }

    #[test]
    fn resampler_distance_scales_with_brush_size() {
        let brush = RoundBrush::with_default_curves(3.0, 0.8);
        assert!(brush.is_ok());
        let brush = match brush {
            Ok(brush) => brush,
            Err(_) => return,
        };
        let distance = brush.resampler_distance();
        assert!((distance.max_distance - 3.6).abs() < 0.0001);
        assert!((distance.min_distance - 2.0).abs() < 0.0001);
    }
}
