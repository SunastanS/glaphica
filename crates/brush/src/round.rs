use crate::{
    BrushId, BrushInput, BrushInputBlockList, BrushInputError, BrushInputProcessor,
    BrushShaderRegistration, BrushStrokeInputProcessor, BrushStrokeSampler, CommittedCanvasSample,
    CommittedCanvasSpanBuffer, DistanceOrTimeStrokeSmoother, EquidistantStrokeSampler,
    SmoothedBrushStrokeInputProcessor, StrokeSampler, StrokeSmoother,
};
use bytemuck::{Pod, Zeroable};
use glaphica_core::CanvasVec2;
use renderer::{BrushShaderSource, BrushShaderSpec};
use std::f32::consts::{FRAC_PI_2, PI};

pub const ROUND_BRUSH_ID: BrushId = BrushId::new(1);

pub const ROUND_APPLY_DAB_WGSL: &str = include_str!("round_apply_dab.wgsl");
pub const ROUND_MERGE_TILE_WGSL: &str = include_str!("round_merge_tile.wgsl");

pub const ROUND_SHADER_SPEC: BrushShaderSpec = BrushShaderSpec {
    apply_dab: BrushShaderSource {
        wgsl: ROUND_APPLY_DAB_WGSL,
        entry_point: "fs_apply_dab",
    },
    merge_tile: BrushShaderSource {
        wgsl: ROUND_MERGE_TILE_WGSL,
        entry_point: "fs_merge_tile",
    },
};

pub const ROUND_SHADER_REGISTRATION: BrushShaderRegistration = BrushShaderRegistration {
    brush_id: ROUND_BRUSH_ID,
    shader_spec: ROUND_SHADER_SPEC,
};

pub const ROUND_INPUT_BLOCK_LEN: usize = 11;
pub const ROUND_MERGE_LUT_LEN: usize = 128;

const ROUND_MIN_SPACING_RATIO: f32 = 0.05;
const ROUND_INPUT_MAX_SPEED_PX_PER_S: f32 = 1000.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CurvePoint {
    pub x: f32,
    pub y: f32,
}

impl CurvePoint {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoundBrushInputFeature {
    Pressure,
    Tilt,
    Twist,
    Speed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoundBrushDabVariable {
    Radius,
    Flow,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RoundBrushVariableModulation {
    pressure: ModulationCurve,
    tilt: ModulationCurve,
    twist: ModulationCurve,
    speed: ModulationCurve,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RoundBrushModulationSet {
    radius: RoundBrushVariableModulation,
    flow: RoundBrushVariableModulation,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RoundBrushInputProcessor {
    base_radius_px: f32,
    spacing_ratio: f32,
    base_hardness: f32,
    base_flow: f32,
    base_opacity: f32,
    tint: [f32; 3],
    modulations: RoundBrushModulationSet,
    smoother_factory: fn() -> Box<dyn StrokeSmoother>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RoundBrushSettings {
    pub base_radius_px: f32,
    pub spacing_ratio: f32,
    pub base_hardness: f32,
    pub base_flow: f32,
    pub base_opacity: f32,
    pub tint: [f32; 3],
    pub modulations: RoundBrushModulationSet,
}

struct RoundBrushStrokeSampler {
    sampler: EquidistantStrokeSampler,
    base_radius_px: f32,
    spacing_ratio: f32,
    base_hardness: f32,
    base_flow: f32,
    base_opacity: f32,
    tint: [f32; 3],
    modulations: RoundBrushModulationSet,
    last_emitted_position: Option<CanvasVec2>,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Pod, Zeroable)]
struct RoundApplyPayload {
    center_local_x: f32,
    center_local_y: f32,
    radius_px: f32,
    flow: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RoundMergeSettings {
    pub tint: [f32; 3],
    pub opacity: f32,
    pub stroke_flow: f32,
    pub spacing_ratio: f32,
    pub hardness: f32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Pod, Zeroable)]
struct RoundMergePayload {
    tint_and_opacity: [f32; 4],
    lookup_params: [f32; 4],
    coverage_lut: [f32; ROUND_MERGE_LUT_LEN],
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct RoundDabParameters {
    radius_px: f32,
    flow: f32,
}

pub fn encode_round_apply_payload(center_local: [f32; 2], radius_px: f32, flow: f32) -> Vec<u8> {
    bytemuck::bytes_of(&RoundApplyPayload {
        center_local_x: center_local[0],
        center_local_y: center_local[1],
        radius_px,
        flow,
    })
    .to_vec()
}

pub fn encode_round_merge_payload(settings: RoundMergeSettings) -> Vec<u8> {
    let payload = build_round_merge_payload(settings);
    bytemuck::bytes_of(&payload).to_vec()
}

impl Default for RoundBrushInputProcessor {
    fn default() -> Self {
        Self::from_settings(RoundBrushSettings::default())
    }
}

impl Default for RoundBrushSettings {
    fn default() -> Self {
        Self {
            base_radius_px: 5.0,
            spacing_ratio: 1.0,
            base_hardness: 0.7,
            base_flow: 1.0,
            base_opacity: 1.0,
            tint: [0.0, 0.0, 1.0],
            modulations: RoundBrushModulationSet::default(),
        }
    }
}

impl RoundBrushSettings {
    pub fn with_base_radius_px(mut self, base_radius_px: f32) -> Self {
        self.base_radius_px = base_radius_px;
        self
    }

    pub fn with_spacing_ratio(mut self, spacing_ratio: f32) -> Self {
        self.spacing_ratio = spacing_ratio;
        self
    }

    pub fn with_base_hardness(mut self, base_hardness: f32) -> Self {
        self.base_hardness = base_hardness;
        self
    }

    pub fn with_base_flow(mut self, base_flow: f32) -> Self {
        self.base_flow = base_flow;
        self
    }

    pub fn with_base_opacity(mut self, base_opacity: f32) -> Self {
        self.base_opacity = base_opacity;
        self
    }

    pub fn with_tint(mut self, tint: [f32; 3]) -> Self {
        self.tint = tint;
        self
    }

    pub fn with_modulation_curve(
        mut self,
        variable: RoundBrushDabVariable,
        feature: RoundBrushInputFeature,
        curve: ModulationCurve,
    ) -> Self {
        self.modulations = self.modulations.with_curve(variable, feature, curve);
        self
    }

    pub fn with_modulations(mut self, modulations: RoundBrushModulationSet) -> Self {
        self.modulations = modulations;
        self
    }
}

impl From<RoundBrushSettings> for RoundBrushInputProcessor {
    fn from(settings: RoundBrushSettings) -> Self {
        Self::from_settings(settings)
    }
}

impl RoundBrushInputProcessor {
    pub fn from_settings(settings: RoundBrushSettings) -> Self {
        Self {
            base_radius_px: settings.base_radius_px,
            spacing_ratio: settings.spacing_ratio,
            base_hardness: settings.base_hardness,
            base_flow: settings.base_flow,
            base_opacity: settings.base_opacity,
            tint: settings.tint,
            modulations: settings.modulations,
            smoother_factory: default_smoother_factory,
        }
    }
}

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

    pub fn identity() -> Self {
        Self {
            points: vec![CurvePoint::new(0.0, 0.0), CurvePoint::new(1.0, 1.0)],
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

impl Default for RoundBrushVariableModulation {
    fn default() -> Self {
        Self {
            pressure: ModulationCurve::flat_one(),
            tilt: ModulationCurve::flat_one(),
            twist: ModulationCurve::flat_one(),
            speed: ModulationCurve::flat_one(),
        }
    }
}

impl RoundBrushVariableModulation {
    fn curve(&self, feature: RoundBrushInputFeature) -> &ModulationCurve {
        match feature {
            RoundBrushInputFeature::Pressure => &self.pressure,
            RoundBrushInputFeature::Tilt => &self.tilt,
            RoundBrushInputFeature::Twist => &self.twist,
            RoundBrushInputFeature::Speed => &self.speed,
        }
    }

    fn curve_mut(&mut self, feature: RoundBrushInputFeature) -> &mut ModulationCurve {
        match feature {
            RoundBrushInputFeature::Pressure => &mut self.pressure,
            RoundBrushInputFeature::Tilt => &mut self.tilt,
            RoundBrushInputFeature::Twist => &mut self.twist,
            RoundBrushInputFeature::Speed => &mut self.speed,
        }
    }

    fn sample_factor(&self, sample: CommittedCanvasSample) -> f32 {
        self.pressure.sample(normalized_round_input_feature(
            RoundBrushInputFeature::Pressure,
            sample,
        )) * self.tilt.sample(normalized_round_input_feature(
            RoundBrushInputFeature::Tilt,
            sample,
        )) * self.twist.sample(normalized_round_input_feature(
            RoundBrushInputFeature::Twist,
            sample,
        )) * self.speed.sample(normalized_round_input_feature(
            RoundBrushInputFeature::Speed,
            sample,
        ))
    }
}

impl Default for RoundBrushModulationSet {
    fn default() -> Self {
        let radius = RoundBrushVariableModulation::default();
        let mut flow = RoundBrushVariableModulation::default();
        *flow.curve_mut(RoundBrushInputFeature::Pressure) = ModulationCurve::identity();
        Self { radius, flow }
    }
}

impl RoundBrushModulationSet {
    pub fn with_curve(
        mut self,
        variable: RoundBrushDabVariable,
        feature: RoundBrushInputFeature,
        curve: ModulationCurve,
    ) -> Self {
        *self.variable_mut(variable).curve_mut(feature) = curve;
        self
    }

    fn variable(&self, variable: RoundBrushDabVariable) -> &RoundBrushVariableModulation {
        match variable {
            RoundBrushDabVariable::Radius => &self.radius,
            RoundBrushDabVariable::Flow => &self.flow,
        }
    }

    fn variable_mut(
        &mut self,
        variable: RoundBrushDabVariable,
    ) -> &mut RoundBrushVariableModulation {
        match variable {
            RoundBrushDabVariable::Radius => &mut self.radius,
            RoundBrushDabVariable::Flow => &mut self.flow,
        }
    }

    fn sample_factor(&self, variable: RoundBrushDabVariable, sample: CommittedCanvasSample) -> f32 {
        self.variable(variable).sample_factor(sample)
    }
}

fn default_smoother_factory() -> Box<dyn StrokeSmoother> {
    Box::new(DistanceOrTimeStrokeSmoother::default())
}

impl RoundBrushInputProcessor {
    fn dab_spacing_px(&self) -> f32 {
        self.base_radius_px * sanitized_spacing_ratio(self.spacing_ratio)
    }

    pub fn settings(&self) -> RoundBrushSettings {
        RoundBrushSettings {
            base_radius_px: self.base_radius_px,
            spacing_ratio: self.spacing_ratio,
            base_hardness: self.base_hardness,
            base_flow: self.base_flow,
            base_opacity: self.base_opacity,
            tint: self.tint,
            modulations: self.modulations.clone(),
        }
    }

    pub fn with_base_radius_px(mut self, base_radius_px: f32) -> Self {
        self.base_radius_px = base_radius_px;
        self
    }

    pub fn with_spacing_ratio(mut self, spacing_ratio: f32) -> Self {
        self.spacing_ratio = spacing_ratio;
        self
    }

    pub fn with_base_hardness(mut self, base_hardness: f32) -> Self {
        self.base_hardness = base_hardness;
        self
    }

    pub fn with_base_flow(mut self, base_flow: f32) -> Self {
        self.base_flow = base_flow;
        self
    }

    pub fn with_base_opacity(mut self, base_opacity: f32) -> Self {
        self.base_opacity = base_opacity;
        self
    }

    pub fn with_tint(mut self, tint: [f32; 3]) -> Self {
        self.tint = tint;
        self
    }

    pub fn with_modulation_curve(
        mut self,
        variable: RoundBrushDabVariable,
        feature: RoundBrushInputFeature,
        curve: ModulationCurve,
    ) -> Self {
        self.modulations = self.modulations.with_curve(variable, feature, curve);
        self
    }

    pub fn with_modulations(mut self, modulations: RoundBrushModulationSet) -> Self {
        self.modulations = modulations;
        self
    }

    pub fn with_smoother_factory(
        mut self,
        smoother_factory: fn() -> Box<dyn StrokeSmoother>,
    ) -> Self {
        self.smoother_factory = smoother_factory;
        self
    }
}

impl BrushInputProcessor for RoundBrushInputProcessor {
    fn begin_stroke(&self) -> Box<dyn BrushStrokeInputProcessor> {
        Box::new(SmoothedBrushStrokeInputProcessor::new(
            (self.smoother_factory)(),
            Box::new(RoundBrushStrokeSampler {
                sampler: EquidistantStrokeSampler::new(self.dab_spacing_px()),
                base_radius_px: self.base_radius_px,
                spacing_ratio: self.spacing_ratio,
                base_hardness: self.base_hardness,
                base_flow: self.base_flow,
                base_opacity: self.base_opacity,
                tint: self.tint,
                modulations: self.modulations.clone(),
                last_emitted_position: None,
            }),
        ))
    }

    fn max_affected_radius_px(&self) -> u32 {
        self.base_radius_px.ceil().max(1.0) as u32
    }

    fn block_center(
        &self,
        input: &BrushInput,
        block_index: usize,
    ) -> Result<CanvasVec2, BrushInputError> {
        if input.brush_id != ROUND_BRUSH_ID {
            return Err(BrushInputError::WrongBrush {
                expected: ROUND_BRUSH_ID,
                actual: input.brush_id,
            });
        }
        let values = input
            .blocks
            .blocks()
            .get(block_index)
            .ok_or(BrushInputError::InvalidBlockLength {
                brush_id: ROUND_BRUSH_ID,
                expected: block_index + 1,
                actual: input.blocks.blocks().len(),
            })?
            .values();
        if values.len() != ROUND_INPUT_BLOCK_LEN {
            return Err(BrushInputError::InvalidBlockLength {
                brush_id: ROUND_BRUSH_ID,
                expected: ROUND_INPUT_BLOCK_LEN,
                actual: values.len(),
            });
        }
        Ok(CanvasVec2::new(values[0], values[1]))
    }

    fn encode_apply_dab_payload(
        &self,
        input: &BrushInput,
        block_index: usize,
        tile_canvas_origin: CanvasVec2,
    ) -> Result<Vec<u8>, BrushInputError> {
        if input.brush_id != ROUND_BRUSH_ID {
            return Err(BrushInputError::WrongBrush {
                expected: ROUND_BRUSH_ID,
                actual: input.brush_id,
            });
        }
        let values = input
            .blocks
            .blocks()
            .get(block_index)
            .ok_or(BrushInputError::InvalidBlockLength {
                brush_id: ROUND_BRUSH_ID,
                expected: block_index + 1,
                actual: input.blocks.blocks().len(),
            })?
            .values();
        if values.len() != ROUND_INPUT_BLOCK_LEN {
            return Err(BrushInputError::InvalidBlockLength {
                brush_id: ROUND_BRUSH_ID,
                expected: ROUND_INPUT_BLOCK_LEN,
                actual: values.len(),
            });
        }
        for (value_index, value) in values.iter().copied().enumerate() {
            if !value.is_finite() {
                return Err(BrushInputError::InvalidBlockValue {
                    brush_id: ROUND_BRUSH_ID,
                    block_index,
                    value_index,
                });
            }
        }
        let center = CanvasVec2::new(values[0], values[1]);
        let local_center = [
            center.x - tile_canvas_origin.x,
            center.y - tile_canvas_origin.y,
        ];
        Ok(encode_round_apply_payload(
            local_center,
            values[2].max(0.0),
            values[3],
        ))
    }

    fn encode_merge_payload(&self, input: &BrushInput) -> Result<Vec<u8>, BrushInputError> {
        if input.brush_id != ROUND_BRUSH_ID {
            return Err(BrushInputError::WrongBrush {
                expected: ROUND_BRUSH_ID,
                actual: input.brush_id,
            });
        }
        let last = input
            .blocks
            .blocks()
            .last()
            .ok_or(BrushInputError::InvalidBlockLength {
                brush_id: ROUND_BRUSH_ID,
                expected: 1,
                actual: 0,
            })?
            .values();
        if last.len() != ROUND_INPUT_BLOCK_LEN {
            return Err(BrushInputError::InvalidBlockLength {
                brush_id: ROUND_BRUSH_ID,
                expected: ROUND_INPUT_BLOCK_LEN,
                actual: last.len(),
            });
        }
        for (value_index, value) in last.iter().copied().enumerate() {
            if !value.is_finite() {
                return Err(BrushInputError::InvalidBlockValue {
                    brush_id: ROUND_BRUSH_ID,
                    block_index: input.blocks.blocks().len().saturating_sub(1),
                    value_index,
                });
            }
        }
        Ok(encode_round_merge_payload(RoundMergeSettings {
            tint: [last[8], last[9], last[10]],
            opacity: last[7].clamp(0.0, 1.0),
            stroke_flow: last[4].max(0.0),
            spacing_ratio: last[5],
            hardness: last[6],
        }))
    }
}

impl BrushStrokeSampler for RoundBrushStrokeSampler {
    fn reset(&mut self) {
        self.sampler.reset();
        self.last_emitted_position = None;
    }

    fn sample_brush_input(
        &mut self,
        spans: &CommittedCanvasSpanBuffer,
    ) -> Result<Option<BrushInput>, BrushInputError> {
        self.sampler.set_spacing(self.dab_spacing_px());
        let mut samples = Vec::new();
        self.sampler.sample_committed_spans(spans, &mut samples);
        let mut blocks = BrushInputBlockList::new(ROUND_BRUSH_ID);
        for (block_index, sample) in samples.iter().copied().enumerate() {
            if self
                .last_emitted_position
                .is_some_and(|position| same_canvas_position(position, sample.position))
            {
                continue;
            }
            push_round_block(
                &mut blocks,
                block_index,
                sample,
                self.base_radius_px,
                self.spacing_ratio,
                self.base_hardness,
                self.base_flow,
                self.base_opacity,
                self.tint,
                &self.modulations,
            )?;
            self.last_emitted_position = Some(sample.position);
        }
        if blocks.blocks().is_empty() {
            return Ok(None);
        }

        Ok(Some(BrushInput {
            brush_id: ROUND_BRUSH_ID,
            blocks,
        }))
    }
}

impl RoundBrushStrokeSampler {
    fn dab_spacing_px(&self) -> f32 {
        self.base_radius_px * sanitized_spacing_ratio(self.spacing_ratio)
    }
}

fn same_canvas_position(lhs: CanvasVec2, rhs: CanvasVec2) -> bool {
    const EPSILON: f32 = 1e-5;
    (lhs.x - rhs.x).abs() <= EPSILON && (lhs.y - rhs.y).abs() <= EPSILON
}

fn push_round_block(
    blocks: &mut BrushInputBlockList,
    block_index: usize,
    sample: CommittedCanvasSample,
    base_radius_px: f32,
    spacing_ratio: f32,
    base_hardness: f32,
    base_flow: f32,
    base_opacity: f32,
    tint: [f32; 3],
    modulations: &RoundBrushModulationSet,
) -> Result<(), BrushInputError> {
    if !sample.position.x.is_finite() {
        return Err(BrushInputError::InvalidBlockValue {
            brush_id: ROUND_BRUSH_ID,
            block_index,
            value_index: 0,
        });
    }
    if !sample.position.y.is_finite() {
        return Err(BrushInputError::InvalidBlockValue {
            brush_id: ROUND_BRUSH_ID,
            block_index,
            value_index: 1,
        });
    }
    if !sample.pressure.is_finite() {
        return Err(BrushInputError::InvalidBlockValue {
            brush_id: ROUND_BRUSH_ID,
            block_index,
            value_index: 2,
        });
    }

    let dab = round_dab_parameters(sample, base_radius_px, base_flow, modulations);
    blocks.push_block(vec![
        sample.position.x,
        sample.position.y,
        dab.radius_px,
        dab.flow,
        base_flow,
        sanitized_spacing_ratio(spacing_ratio),
        base_hardness,
        base_opacity,
        tint[0],
        tint[1],
        tint[2],
    ]);
    Ok(())
}

fn round_dab_parameters(
    sample: CommittedCanvasSample,
    base_radius_px: f32,
    base_flow: f32,
    modulations: &RoundBrushModulationSet,
) -> RoundDabParameters {
    let radius_px = base_radius_px.max(0.0)
        * modulations
            .sample_factor(RoundBrushDabVariable::Radius, sample)
            .clamp(0.0, 1.0);
    let flow = base_flow.max(0.0)
        * modulations
            .sample_factor(RoundBrushDabVariable::Flow, sample)
            .clamp(0.0, 1.0);
    RoundDabParameters { radius_px, flow }
}

fn normalized_round_input_feature(
    feature: RoundBrushInputFeature,
    sample: CommittedCanvasSample,
) -> f32 {
    match feature {
        RoundBrushInputFeature::Pressure => sample.pressure.clamp(0.0, 1.0),
        RoundBrushInputFeature::Tilt => normalize_tilt(sample.tilt),
        RoundBrushInputFeature::Twist => normalize_twist(sample.twist),
        RoundBrushInputFeature::Speed => normalize_speed(sample.velocity),
    }
}

fn normalize_tilt(tilt: glaphica_core::RadianVec2) -> f32 {
    let magnitude = (tilt.x * tilt.x + tilt.y * tilt.y).sqrt();
    (magnitude / FRAC_PI_2).clamp(0.0, 1.0)
}

fn normalize_twist(twist: f32) -> f32 {
    ((twist + PI) / (2.0 * PI)).clamp(0.0, 1.0)
}

fn normalize_speed(velocity: CanvasVec2) -> f32 {
    let speed = (velocity.x * velocity.x + velocity.y * velocity.y).sqrt();
    (speed / ROUND_INPUT_MAX_SPEED_PX_PER_S).clamp(0.0, 1.0)
}

fn eval_unit_interval_curve_polynomial(points: &[CurvePoint], x: f32) -> Option<f32> {
    if points.len() < 2 {
        return None;
    }
    let x = x.clamp(0.0, 1.0);
    let mut y = 0.0f32;
    for (i, point_i) in points.iter().enumerate() {
        let mut basis = 1.0f32;
        for (j, point_j) in points.iter().enumerate() {
            if i == j {
                continue;
            }
            let denominator = point_i.x - point_j.x;
            if denominator.abs() <= f32::EPSILON {
                return None;
            }
            basis *= (x - point_j.x) / denominator;
        }
        y += point_i.y * basis;
    }
    Some(y.clamp(0.0, 1.0))
}

fn sanitized_spacing_ratio(spacing_ratio: f32) -> f32 {
    if spacing_ratio.is_finite() {
        spacing_ratio.max(ROUND_MIN_SPACING_RATIO)
    } else {
        ROUND_MIN_SPACING_RATIO
    }
}

fn round_hardness_threshold_source(stroke_flow: f32, hardness: f32) -> f32 {
    let stroke_flow = stroke_flow.max(0.0);
    let hardness = hardness.clamp(0.0, 1.0);
    if stroke_flow <= f32::EPSILON {
        return 0.0;
    }
    if hardness >= 1.0 {
        return 0.0;
    }
    (stroke_flow * (1.0 - hardness)).max(f32::EPSILON)
}

fn round_merge_coverage_for_source(
    source: f32,
    hardness_threshold_source: f32,
    hardness: f32,
) -> f32 {
    let source = source.max(0.0);
    if source <= 0.0 {
        return 0.0;
    }
    if hardness.clamp(0.0, 1.0) >= 1.0 {
        return 1.0;
    }
    if hardness_threshold_source <= f32::EPSILON {
        return 0.0;
    }
    (source / hardness_threshold_source).clamp(0.0, 1.0)
}

fn build_round_merge_payload(settings: RoundMergeSettings) -> RoundMergePayload {
    let mut coverage_lut = [0.0; ROUND_MERGE_LUT_LEN];
    let tint_and_opacity = [
        settings.tint[0],
        settings.tint[1],
        settings.tint[2],
        settings.opacity.clamp(0.0, 1.0),
    ];
    let stroke_flow = settings.stroke_flow.max(0.0);
    let hardness = settings.hardness.clamp(0.0, 1.0);
    let source_max = stroke_flow;
    let hardness_threshold_source = round_hardness_threshold_source(stroke_flow, hardness);
    let mut source_to_lut_scale = 0.0;
    if source_max > f32::EPSILON {
        source_to_lut_scale = (ROUND_MERGE_LUT_LEN.saturating_sub(1) as f32) / source_max;
        for (index, lut_value) in coverage_lut.iter_mut().enumerate() {
            let source = source_max * index as f32 / (ROUND_MERGE_LUT_LEN.saturating_sub(1) as f32);
            *lut_value =
                round_merge_coverage_for_source(source, hardness_threshold_source, hardness);
        }
    }
    RoundMergePayload {
        tint_and_opacity,
        lookup_params: [source_to_lut_scale, source_max, 0.0, 0.0],
        coverage_lut,
    }
}

#[cfg(test)]
mod tests {
    use std::f32::consts::PI;

    use crate::{
        BrushInput, BrushInputBlockList, BrushInputProcessor, CanvasInput,
        PassthroughStrokeSmoother, StrokeSmoother,
        round::{
            CurvePoint, ModulationCurve, ROUND_BRUSH_ID, RoundBrushDabVariable,
            RoundBrushInputFeature, RoundBrushInputProcessor, RoundMergeSettings,
            encode_round_merge_payload,
        },
    };
    use glaphica_core::CanvasVec2;

    fn passthrough_smoother_factory() -> Box<dyn StrokeSmoother> {
        Box::new(PassthroughStrokeSmoother::default())
    }

    #[test]
    fn round_processor_encodes_payloads_from_blocks() {
        let mut input = BrushInputBlockList::new(ROUND_BRUSH_ID);
        input.push_block(vec![10.0, 8.0, 6.0, 0.7, 0.9, 0.8, 0.4, 0.6, 0.2, 0.3, 0.4]);
        let input = BrushInput {
            brush_id: ROUND_BRUSH_ID,
            blocks: input,
        };

        let result = RoundBrushInputProcessor::default()
            .encode_apply_dab_payload(&input, 0, CanvasVec2::new(0.0, 0.0))
            .expect("processing should succeed");

        assert!(!result.is_empty());
        assert_eq!(
            RoundBrushInputProcessor::default()
                .encode_merge_payload(&input)
                .expect("merge payload"),
            encode_round_merge_payload(RoundMergeSettings {
                tint: [0.2, 0.3, 0.4],
                opacity: 0.6,
                stroke_flow: 0.9,
                spacing_ratio: 0.8,
                hardness: 0.4,
            })
        );
    }

    #[test]
    fn round_processor_produces_blocks_from_canvas_input() {
        let processor = RoundBrushInputProcessor::default();
        let mut stroke = processor.begin_stroke();
        let input = [
            CanvasInput {
                time_ns: 1,
                position: glaphica_core::CanvasVec2::new(11.0, 13.0),
                pressure: 0.5,
                tilt: glaphica_core::RadianVec2::new(0.0, 0.0),
                twist: 0.0,
            },
            CanvasInput {
                time_ns: 2,
                position: glaphica_core::CanvasVec2::new(17.0, 13.0),
                pressure: 0.5,
                tilt: glaphica_core::RadianVec2::new(0.0, 0.0),
                twist: 0.0,
            },
        ];

        stroke
            .push_canvas_inputs(&input)
            .expect("push should succeed");
        let result = stroke
            .drain_brush_input()
            .expect("drain should succeed")
            .expect("brush input should exist");

        assert_eq!(result.brush_id, ROUND_BRUSH_ID);
        assert_eq!(result.blocks.blocks().len(), 1);
        assert_eq!(result.blocks.blocks()[0].values()[0], 11.0);
        assert_eq!(result.blocks.blocks()[0].values()[1], 13.0);
        assert_eq!(result.blocks.blocks()[0].values()[2], 5.0);
        assert_eq!(result.blocks.blocks()[0].values()[3], 0.5);
        assert_eq!(result.blocks.blocks()[0].values()[4], 1.0);
        assert_eq!(result.blocks.blocks()[0].values()[5], 1.0);
        assert_eq!(result.blocks.blocks()[0].values()[6], 0.7);
        assert_eq!(result.blocks.blocks()[0].values()[7], 1.0);
    }

    #[test]
    fn round_processor_allows_custom_base_radius_and_hardness() {
        let processor = RoundBrushInputProcessor::default()
            .with_base_radius_px(20.0)
            .with_base_hardness(0.3);
        let mut stroke = processor.begin_stroke();
        let input = [
            CanvasInput {
                time_ns: 1,
                position: glaphica_core::CanvasVec2::new(11.0, 13.0),
                pressure: 0.5,
                tilt: glaphica_core::RadianVec2::new(0.0, 0.0),
                twist: 0.0,
            },
            CanvasInput {
                time_ns: 2,
                position: glaphica_core::CanvasVec2::new(17.0, 13.0),
                pressure: 0.5,
                tilt: glaphica_core::RadianVec2::new(0.0, 0.0),
                twist: 0.0,
            },
        ];

        stroke
            .push_canvas_inputs(&input)
            .expect("push should succeed");
        let result = stroke
            .drain_brush_input()
            .expect("drain should succeed")
            .expect("brush input should exist");

        assert_eq!(result.blocks.blocks().len(), 1);
        assert_eq!(result.blocks.blocks()[0].values()[2], 20.0);
        assert_eq!(result.blocks.blocks()[0].values()[6], 0.3);
        assert_eq!(processor.max_affected_radius_px(), 20);
    }

    #[test]
    fn round_processor_can_map_pressure_to_radius_per_dab() {
        let processor = RoundBrushInputProcessor::default()
            .with_smoother_factory(passthrough_smoother_factory)
            .with_modulation_curve(
                RoundBrushDabVariable::Radius,
                RoundBrushInputFeature::Pressure,
                ModulationCurve::identity(),
            );
        let mut stroke = processor.begin_stroke();

        stroke
            .push_canvas_inputs(&[CanvasInput {
                time_ns: 1,
                position: glaphica_core::CanvasVec2::new(11.0, 13.0),
                pressure: 0.5,
                tilt: glaphica_core::RadianVec2::new(0.0, 0.0),
                twist: 0.0,
            }])
            .expect("push should succeed");
        let result = stroke
            .drain_brush_input()
            .expect("drain should succeed")
            .expect("brush input should exist");

        assert_eq!(result.blocks.blocks().len(), 1);
        assert_eq!(result.blocks.blocks()[0].values()[2], 2.5);
    }

    #[test]
    fn round_processor_can_map_twist_to_flow_per_dab() {
        let processor = RoundBrushInputProcessor::default()
            .with_smoother_factory(passthrough_smoother_factory)
            .with_modulation_curve(
                RoundBrushDabVariable::Flow,
                RoundBrushInputFeature::Pressure,
                ModulationCurve::flat_one(),
            )
            .with_modulation_curve(
                RoundBrushDabVariable::Flow,
                RoundBrushInputFeature::Twist,
                ModulationCurve::new(vec![CurvePoint::new(0.0, 0.0), CurvePoint::new(1.0, 1.0)])
                    .expect("valid curve"),
            );
        let mut stroke = processor.begin_stroke();

        stroke
            .push_canvas_inputs(&[CanvasInput {
                time_ns: 1,
                position: glaphica_core::CanvasVec2::new(11.0, 13.0),
                pressure: 1.0,
                tilt: glaphica_core::RadianVec2::new(0.0, 0.0),
                twist: 0.0,
            }])
            .expect("push should succeed");
        let result = stroke
            .drain_brush_input()
            .expect("drain should succeed")
            .expect("brush input should exist");

        assert_eq!(result.blocks.blocks().len(), 1);
        assert!((result.blocks.blocks()[0].values()[3] - 0.5).abs() <= 1e-5);

        let mut full_twist = processor.begin_stroke();
        full_twist
            .push_canvas_inputs(&[CanvasInput {
                time_ns: 2,
                position: glaphica_core::CanvasVec2::new(21.0, 13.0),
                pressure: 1.0,
                tilt: glaphica_core::RadianVec2::new(0.0, 0.0),
                twist: PI,
            }])
            .expect("push should succeed");
        let full_twist_result = full_twist
            .drain_brush_input()
            .expect("drain should succeed")
            .expect("brush input should exist");

        assert_eq!(full_twist_result.blocks.blocks()[0].values()[3], 1.0);
    }

    #[test]
    fn round_processor_uses_uniform_sampling_for_first_point_with_passthrough_smoother() {
        let processor =
            RoundBrushInputProcessor::default().with_smoother_factory(passthrough_smoother_factory);
        let mut stroke = processor.begin_stroke();

        stroke
            .push_canvas_inputs(&[
                CanvasInput {
                    time_ns: 1,
                    position: glaphica_core::CanvasVec2::new(0.0, 0.0),
                    pressure: 0.5,
                    tilt: glaphica_core::RadianVec2::new(0.0, 0.0),
                    twist: 0.0,
                },
                CanvasInput {
                    time_ns: 2,
                    position: glaphica_core::CanvasVec2::new(12.0, 0.0),
                    pressure: 0.5,
                    tilt: glaphica_core::RadianVec2::new(0.0, 0.0),
                    twist: 0.0,
                },
            ])
            .expect("push should succeed");

        let result = stroke
            .drain_brush_input()
            .expect("drain should succeed")
            .expect("brush input should exist");
        let positions = result
            .blocks
            .blocks()
            .iter()
            .map(|block| (block.values()[0], block.values()[1]))
            .collect::<Vec<_>>();

        assert_eq!(positions, vec![(0.0, 0.0), (5.0, 0.0), (10.0, 0.0)]);
    }

    #[test]
    fn round_processor_does_not_repeat_first_center_after_small_initial_motion() {
        let processor = RoundBrushInputProcessor::default();
        let mut stroke = processor.begin_stroke();
        let mut emitted_positions = Vec::new();

        stroke
            .push_canvas_inputs(&[CanvasInput {
                time_ns: 0,
                position: glaphica_core::CanvasVec2::new(0.0, 0.0),
                pressure: 0.5,
                tilt: glaphica_core::RadianVec2::new(0.0, 0.0),
                twist: 0.0,
            }])
            .expect("press input");
        if let Some(input) = stroke.drain_brush_input().expect("press drain") {
            emitted_positions.extend(
                input
                    .blocks
                    .blocks()
                    .iter()
                    .map(|block| (block.values()[0], block.values()[1])),
            );
        }

        stroke
            .push_canvas_inputs(&[
                CanvasInput {
                    time_ns: 1,
                    position: glaphica_core::CanvasVec2::new(0.5, 0.0),
                    pressure: 0.5,
                    tilt: glaphica_core::RadianVec2::new(0.0, 0.0),
                    twist: 0.0,
                },
                CanvasInput {
                    time_ns: 2,
                    position: glaphica_core::CanvasVec2::new(1.0, 0.0),
                    pressure: 0.5,
                    tilt: glaphica_core::RadianVec2::new(0.0, 0.0),
                    twist: 0.0,
                },
                CanvasInput {
                    time_ns: 3,
                    position: glaphica_core::CanvasVec2::new(8.0, 0.0),
                    pressure: 0.5,
                    tilt: glaphica_core::RadianVec2::new(0.0, 0.0),
                    twist: 0.0,
                },
            ])
            .expect("motion inputs");
        if let Some(input) = stroke.drain_brush_input().expect("motion drain") {
            emitted_positions.extend(
                input
                    .blocks
                    .blocks()
                    .iter()
                    .map(|block| (block.values()[0], block.values()[1])),
            );
        }

        stroke.finish_stroke().expect("finish stroke");
        if let Some(input) = stroke.drain_brush_input().expect("finish drain") {
            emitted_positions.extend(
                input
                    .blocks
                    .blocks()
                    .iter()
                    .map(|block| (block.values()[0], block.values()[1])),
            );
        }

        let first_point_count = emitted_positions
            .iter()
            .filter(|&&(x, y)| x.abs() <= 1e-5 && y.abs() <= 1e-5)
            .count();
        assert_eq!(first_point_count, 1, "{emitted_positions:?}");
    }

    #[test]
    fn default_smoother_delays_second_center_after_small_initial_motion() {
        let processor = RoundBrushInputProcessor::default();
        let mut stroke = processor.begin_stroke();
        let mut emitted_positions = Vec::new();

        stroke
            .push_canvas_inputs(&[CanvasInput {
                time_ns: 0,
                position: glaphica_core::CanvasVec2::new(0.0, 0.0),
                pressure: 0.5,
                tilt: glaphica_core::RadianVec2::new(0.0, 0.0),
                twist: 0.0,
            }])
            .expect("press input");
        if let Some(input) = stroke.drain_brush_input().expect("press drain") {
            emitted_positions.extend(
                input
                    .blocks
                    .blocks()
                    .iter()
                    .map(|block| (block.values()[0], block.values()[1])),
            );
        }

        stroke
            .push_canvas_inputs(&[
                CanvasInput {
                    time_ns: 1,
                    position: glaphica_core::CanvasVec2::new(0.5, 0.0),
                    pressure: 0.5,
                    tilt: glaphica_core::RadianVec2::new(0.0, 0.0),
                    twist: 0.0,
                },
                CanvasInput {
                    time_ns: 2,
                    position: glaphica_core::CanvasVec2::new(1.0, 0.0),
                    pressure: 0.5,
                    tilt: glaphica_core::RadianVec2::new(0.0, 0.0),
                    twist: 0.0,
                },
                CanvasInput {
                    time_ns: 3,
                    position: glaphica_core::CanvasVec2::new(8.0, 0.0),
                    pressure: 0.5,
                    tilt: glaphica_core::RadianVec2::new(0.0, 0.0),
                    twist: 0.0,
                },
            ])
            .expect("motion inputs");
        if let Some(input) = stroke.drain_brush_input().expect("motion drain") {
            emitted_positions.extend(
                input
                    .blocks
                    .blocks()
                    .iter()
                    .map(|block| (block.values()[0], block.values()[1])),
            );
        }

        stroke.finish_stroke().expect("finish stroke");
        if let Some(input) = stroke.drain_brush_input().expect("finish drain") {
            emitted_positions.extend(
                input
                    .blocks
                    .blocks()
                    .iter()
                    .map(|block| (block.values()[0], block.values()[1])),
            );
        }

        assert_eq!(emitted_positions, vec![(0.0, 0.0)]);
    }

    #[test]
    fn passthrough_smoother_keeps_second_center_one_spacing_from_origin_after_small_initial_motion()
    {
        let processor =
            RoundBrushInputProcessor::default().with_smoother_factory(passthrough_smoother_factory);
        let mut stroke = processor.begin_stroke();
        let mut emitted_positions = Vec::new();

        stroke
            .push_canvas_inputs(&[CanvasInput {
                time_ns: 0,
                position: glaphica_core::CanvasVec2::new(0.0, 0.0),
                pressure: 0.5,
                tilt: glaphica_core::RadianVec2::new(0.0, 0.0),
                twist: 0.0,
            }])
            .expect("press input");
        if let Some(input) = stroke.drain_brush_input().expect("press drain") {
            emitted_positions.extend(
                input
                    .blocks
                    .blocks()
                    .iter()
                    .map(|block| (block.values()[0], block.values()[1])),
            );
        }

        stroke
            .push_canvas_inputs(&[
                CanvasInput {
                    time_ns: 1,
                    position: glaphica_core::CanvasVec2::new(0.5, 0.0),
                    pressure: 0.5,
                    tilt: glaphica_core::RadianVec2::new(0.0, 0.0),
                    twist: 0.0,
                },
                CanvasInput {
                    time_ns: 2,
                    position: glaphica_core::CanvasVec2::new(1.0, 0.0),
                    pressure: 0.5,
                    tilt: glaphica_core::RadianVec2::new(0.0, 0.0),
                    twist: 0.0,
                },
                CanvasInput {
                    time_ns: 3,
                    position: glaphica_core::CanvasVec2::new(8.0, 0.0),
                    pressure: 0.5,
                    tilt: glaphica_core::RadianVec2::new(0.0, 0.0),
                    twist: 0.0,
                },
            ])
            .expect("motion inputs");
        if let Some(input) = stroke.drain_brush_input().expect("motion drain") {
            emitted_positions.extend(
                input
                    .blocks
                    .blocks()
                    .iter()
                    .map(|block| (block.values()[0], block.values()[1])),
            );
        }

        stroke.finish_stroke().expect("finish stroke");
        if let Some(input) = stroke.drain_brush_input().expect("finish drain") {
            emitted_positions.extend(
                input
                    .blocks
                    .blocks()
                    .iter()
                    .map(|block| (block.values()[0], block.values()[1])),
            );
        }

        assert!(emitted_positions.len() >= 2, "{emitted_positions:?}");
        let second = emitted_positions[1];
        let distance_from_origin = (second.0 * second.0 + second.1 * second.1).sqrt();
        assert!(distance_from_origin >= 4.9, "{emitted_positions:?}");
    }

    #[test]
    fn round_processor_keeps_uniform_arclength_across_drains() {
        let processor = RoundBrushInputProcessor::default();
        let mut stroke = processor.begin_stroke();

        stroke
            .push_canvas_inputs(&[
                CanvasInput {
                    time_ns: 1,
                    position: glaphica_core::CanvasVec2::new(0.0, 0.0),
                    pressure: 0.5,
                    tilt: glaphica_core::RadianVec2::new(0.0, 0.0),
                    twist: 0.0,
                },
                CanvasInput {
                    time_ns: 2,
                    position: glaphica_core::CanvasVec2::new(6.0, 0.0),
                    pressure: 0.5,
                    tilt: glaphica_core::RadianVec2::new(0.0, 0.0),
                    twist: 0.0,
                },
            ])
            .expect("first push");
        let first = stroke
            .drain_brush_input()
            .expect("first drain")
            .expect("first input should exist");

        stroke
            .push_canvas_inputs(&[CanvasInput {
                time_ns: 3,
                position: glaphica_core::CanvasVec2::new(12.0, 0.0),
                pressure: 0.5,
                tilt: glaphica_core::RadianVec2::new(0.0, 0.0),
                twist: 0.0,
            }])
            .expect("second push");
        stroke
            .push_canvas_inputs(&[CanvasInput {
                time_ns: 4,
                position: glaphica_core::CanvasVec2::new(18.0, 0.0),
                pressure: 0.5,
                tilt: glaphica_core::RadianVec2::new(0.0, 0.0),
                twist: 0.0,
            }])
            .expect("third push");
        let second = stroke
            .drain_brush_input()
            .expect("second drain")
            .expect("second input should exist");
        stroke.finish_stroke().expect("finish stroke");
        let third = stroke
            .drain_brush_input()
            .expect("third drain")
            .expect("third input should exist");

        let mut positions = first
            .blocks
            .blocks()
            .iter()
            .chain(second.blocks.blocks().iter())
            .chain(third.blocks.blocks().iter())
            .map(|block| (block.values()[0], block.values()[1]))
            .collect::<Vec<_>>();
        positions.sort_by(|lhs, rhs| lhs.partial_cmp(rhs).expect("finite x compare"));

        assert_eq!(positions, vec![(0.0, 0.0), (5.0, 0.0), (10.0, 0.0)]);
    }

    #[test]
    fn round_processor_streaming_drains_match_finished_stroke_sampling_prefix() {
        let processor = RoundBrushInputProcessor::default();
        let inputs = (0..16)
            .map(|index| {
                let x = index as f32 * 12.0;
                let y = if index < 6 {
                    index as f32 * 6.0
                } else if index < 11 {
                    36.0 - (index as f32 - 6.0) * 4.0
                } else {
                    16.0 - (index as f32 - 11.0) * 3.0
                };
                CanvasInput {
                    time_ns: index as u64 * 1_000_000,
                    position: glaphica_core::CanvasVec2::new(x, y),
                    pressure: 0.5,
                    tilt: glaphica_core::RadianVec2::new(0.0, 0.0),
                    twist: 0.0,
                }
            })
            .collect::<Vec<_>>();

        let mut streamed = processor.begin_stroke();
        let mut streamed_positions = Vec::new();
        for chunk in inputs.chunks(2) {
            streamed
                .push_canvas_inputs(chunk)
                .expect("streaming push should succeed");
            if let Some(input) = streamed.drain_brush_input().expect("streaming drain") {
                streamed_positions.extend(
                    input
                        .blocks
                        .blocks()
                        .iter()
                        .map(|block| (block.values()[0], block.values()[1])),
                );
            }
        }
        streamed.finish_stroke().expect("finish streaming stroke");
        if let Some(input) = streamed.drain_brush_input().expect("final streaming drain") {
            streamed_positions.extend(
                input
                    .blocks
                    .blocks()
                    .iter()
                    .map(|block| (block.values()[0], block.values()[1])),
            );
        }

        let mut finished = processor.begin_stroke();
        finished
            .push_canvas_inputs(&inputs)
            .expect("finished push should succeed");
        finished.finish_stroke().expect("finish complete stroke");
        let final_input = finished
            .drain_brush_input()
            .expect("finished drain")
            .expect("finished stroke should emit input");
        let finished_positions = final_input
            .blocks
            .blocks()
            .iter()
            .map(|block| (block.values()[0], block.values()[1]))
            .collect::<Vec<_>>();

        let comparable_len = streamed_positions
            .len()
            .min(finished_positions.len())
            .saturating_sub(4);
        assert!(comparable_len > 0);
        assert_eq!(
            &streamed_positions[..comparable_len],
            &finished_positions[..comparable_len]
        );
    }

    #[test]
    fn merge_payload_lookup_table_is_monotonic() {
        let payload = super::build_round_merge_payload(RoundMergeSettings {
            tint: [0.1, 0.2, 0.3],
            opacity: 0.8,
            stroke_flow: 1.0,
            spacing_ratio: 0.7,
            hardness: 0.4,
        });

        assert_eq!(payload.coverage_lut[0], 0.0);
        assert!(payload.lookup_params[0] > 0.0);
        assert!(payload.coverage_lut[super::ROUND_MERGE_LUT_LEN - 1] > 0.99);
        for window in payload.coverage_lut.windows(2) {
            assert!(window[0] <= window[1], "{window:?}");
        }
    }

    #[test]
    fn harder_merge_payload_keeps_more_coverage_at_same_source() {
        let soft = super::build_round_merge_payload(RoundMergeSettings {
            tint: [0.1, 0.2, 0.3],
            opacity: 1.0,
            stroke_flow: 1.0,
            spacing_ratio: 0.8,
            hardness: 0.2,
        });
        let hard = super::build_round_merge_payload(RoundMergeSettings {
            tint: [0.1, 0.2, 0.3],
            opacity: 1.0,
            stroke_flow: 1.0,
            spacing_ratio: 0.8,
            hardness: 0.8,
        });

        let mid_index = super::ROUND_MERGE_LUT_LEN / 2;
        assert!(hard.coverage_lut[mid_index] >= soft.coverage_lut[mid_index]);
    }

    #[test]
    fn hardness_maps_to_image_space_clip_threshold() {
        let hardness = 0.3;
        let hardness_threshold_source = super::round_hardness_threshold_source(1.0, hardness);

        assert!((hardness_threshold_source - 0.7).abs() <= 1e-5);
        assert_eq!(
            super::round_merge_coverage_for_source(0.7, hardness_threshold_source, hardness),
            1.0
        );
        assert!(
            (super::round_merge_coverage_for_source(0.35, hardness_threshold_source, hardness,)
                - 0.5)
                .abs()
                <= 1e-5
        );
    }

    #[test]
    fn hardness_one_saturates_any_positive_source() {
        let hardness = 0.3;
        assert_eq!(super::round_merge_coverage_for_source(0.0, 0.0, 1.0), 0.0);
        assert_eq!(super::round_merge_coverage_for_source(0.1, 0.0, 1.0), 1.0);
        assert_eq!(super::round_merge_coverage_for_source(1.0, 0.0, 1.0), 1.0);
        assert_eq!(super::round_hardness_threshold_source(1.0, 1.0), 0.0);
        assert!(super::round_merge_coverage_for_source(0.25, 0.5, hardness) < 1.0);
    }

    #[test]
    fn merge_payload_is_independent_of_spacing_ratio() {
        let sparse = super::build_round_merge_payload(RoundMergeSettings {
            tint: [0.1, 0.2, 0.3],
            opacity: 1.0,
            stroke_flow: 1.0,
            spacing_ratio: 1.0,
            hardness: 0.3,
        });
        let dense = super::build_round_merge_payload(RoundMergeSettings {
            tint: [0.1, 0.2, 0.3],
            opacity: 1.0,
            stroke_flow: 1.0,
            spacing_ratio: 0.1,
            hardness: 0.3,
        });

        assert_eq!(sparse.lookup_params, dense.lookup_params);
        assert_eq!(sparse.coverage_lut, dense.coverage_lut);
    }
}
