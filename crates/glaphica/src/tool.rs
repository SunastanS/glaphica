use gla_color::PremultipliedRgbaF32;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(transparent)]
pub struct BrushId(u64);

impl BrushId {
    pub const DEFAULT: Self = Self::new(1);

    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn value(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Tool {
    Brush(BrushId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActiveTool {
    Brush(BrushId),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolSet {
    tools: Vec<Tool>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BrushSettings {
    pub radius_px: f32,
    pub color: PremultipliedRgbaF32,
    pub spacing_ratio: f32,
    pub hardness: f32,
    pub flow: f32,
    pub opacity: f32,
    pub modulations: RoundBrushModulationSet,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RoundBrushSettings {
    pub base_radius_px: f32,
    pub spacing_ratio: f32,
    pub base_hardness: f32,
    pub base_flow: f32,
    pub base_opacity: f32,
    pub tint: [f32; 3],
    #[serde(default, skip_serializing_if = "RoundBrushModulationSet::is_default")]
    pub modulations: RoundBrushModulationSet,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct CurvePoint {
    pub x: f32,
    pub y: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModulationCurve {
    points: Vec<CurvePoint>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CurveValidationError {
    TooFewPoints,
    PointOutOfRange { index: usize },
    NotMonotonic { index: usize },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RoundBrushInputFeature {
    Pressure,
    Tilt,
    Twist,
    Speed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RoundBrushDabVariable {
    Radius,
    Flow,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RoundBrushVariableModulation {
    pub pressure: ModulationCurve,
    pub tilt: ModulationCurve,
    pub twist: ModulationCurve,
    pub speed: ModulationCurve,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RoundBrushModulationSet {
    pub radius: RoundBrushVariableModulation,
    pub flow: RoundBrushVariableModulation,
}

impl Tool {
    pub fn brush_id(self) -> Option<BrushId> {
        match self {
            Self::Brush(brush_id) => Some(brush_id),
        }
    }
}

impl ActiveTool {
    pub fn brush_id(self) -> Option<BrushId> {
        match self {
            Self::Brush(brush_id) => Some(brush_id),
        }
    }

    pub fn as_tool(self) -> Tool {
        match self {
            Self::Brush(brush_id) => Tool::Brush(brush_id),
        }
    }
}

impl ToolSet {
    pub fn new(tools: Vec<Tool>) -> Self {
        Self { tools }
    }

    pub fn default_brush() -> Self {
        Self::new(vec![Tool::Brush(BrushId::DEFAULT)])
    }

    pub fn tools(&self) -> &[Tool] {
        &self.tools
    }

    pub fn contains(&self, tool: Tool) -> bool {
        self.tools.contains(&tool)
    }
}

impl Default for ToolSet {
    fn default() -> Self {
        Self::default_brush()
    }
}

impl Default for BrushSettings {
    fn default() -> Self {
        Self {
            radius_px: 10.0,
            color: PremultipliedRgbaF32::new(0.95, 0.17, 0.10, 1.0),
            spacing_ratio: 1.0,
            hardness: 0.7,
            flow: 1.0,
            opacity: 1.0,
            modulations: RoundBrushModulationSet::default(),
        }
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

impl CurvePoint {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

impl ModulationCurve {
    pub fn new(points: Vec<CurvePoint>) -> Result<Self, CurveValidationError> {
        validate_curve_points(&points)?;
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
        eval_unit_interval_curve(&self.points, x).unwrap_or(1.0)
    }

    pub fn points(&self) -> &[CurvePoint] {
        &self.points
    }

    pub fn is_flat_one(&self) -> bool {
        *self == Self::flat_one()
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
    pub fn sample_factor(&self, pressure: f32, tilt: f32, twist: f32, speed: f32) -> f32 {
        self.pressure.sample(pressure)
            * self.tilt.sample(tilt)
            * self.twist.sample(twist)
            * self.speed.sample(speed)
    }

    fn curve_mut(&mut self, feature: RoundBrushInputFeature) -> &mut ModulationCurve {
        match feature {
            RoundBrushInputFeature::Pressure => &mut self.pressure,
            RoundBrushInputFeature::Tilt => &mut self.tilt,
            RoundBrushInputFeature::Twist => &mut self.twist,
            RoundBrushInputFeature::Speed => &mut self.speed,
        }
    }
}

impl Default for RoundBrushModulationSet {
    fn default() -> Self {
        let radius = RoundBrushVariableModulation::default();
        let mut flow = RoundBrushVariableModulation::default();
        flow.pressure = ModulationCurve::identity();
        Self { radius, flow }
    }
}

impl RoundBrushModulationSet {
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }

    pub fn with_curve(
        mut self,
        variable: RoundBrushDabVariable,
        feature: RoundBrushInputFeature,
        curve: ModulationCurve,
    ) -> Self {
        *self.variable_mut(variable).curve_mut(feature) = curve;
        self
    }

    pub fn sample_factor(
        &self,
        variable: RoundBrushDabVariable,
        pressure: f32,
        tilt: f32,
        twist: f32,
        speed: f32,
    ) -> f32 {
        let modulation = match variable {
            RoundBrushDabVariable::Radius => &self.radius,
            RoundBrushDabVariable::Flow => &self.flow,
        };
        modulation.sample_factor(pressure, tilt, twist, speed)
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
}

impl From<RoundBrushSettings> for BrushSettings {
    fn from(settings: RoundBrushSettings) -> Self {
        Self {
            radius_px: settings.base_radius_px,
            color: PremultipliedRgbaF32::new(
                settings.tint[0],
                settings.tint[1],
                settings.tint[2],
                1.0,
            ),
            spacing_ratio: settings.spacing_ratio,
            hardness: settings.base_hardness,
            flow: settings.base_flow,
            opacity: settings.base_opacity,
            modulations: settings.modulations,
        }
    }
}

impl BrushSettings {
    pub fn from_round_brush(settings: RoundBrushSettings) -> Self {
        settings.into()
    }
}

fn validate_curve_points(points: &[CurvePoint]) -> Result<(), CurveValidationError> {
    if points.len() < 2 {
        return Err(CurveValidationError::TooFewPoints);
    }

    let mut previous_x = 0.0f32;
    for (index, point) in points.iter().enumerate() {
        if !(0.0..=1.0).contains(&point.x) || !(0.0..=1.0).contains(&point.y) {
            return Err(CurveValidationError::PointOutOfRange { index });
        }
        if index > 0 && point.x <= previous_x {
            return Err(CurveValidationError::NotMonotonic { index });
        }
        previous_x = point.x;
    }
    Ok(())
}

fn eval_unit_interval_curve(points: &[CurvePoint], x: f32) -> Option<f32> {
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

#[cfg(test)]
mod tests {
    use super::{
        ActiveTool, BrushId, BrushSettings, CurvePoint, CurveValidationError, ModulationCurve,
        RoundBrushDabVariable, RoundBrushInputFeature, RoundBrushModulationSet, RoundBrushSettings,
        Tool, ToolSet,
    };

    #[test]
    fn active_tool_round_trips_into_tool_set_membership() {
        let active_tool = ActiveTool::Brush(BrushId::new(5));
        let tool_set = ToolSet::new(vec![Tool::Brush(BrushId::new(5))]);

        assert!(tool_set.contains(active_tool.as_tool()));
        assert_eq!(active_tool.brush_id(), Some(BrushId::new(5)));
    }

    #[test]
    fn default_tool_set_contains_default_brush() {
        let tool_set = ToolSet::default();

        assert_eq!(tool_set.tools(), &[Tool::Brush(BrushId::DEFAULT)]);
    }

    #[test]
    fn default_brush_settings_reserve_round_brush_controls() {
        let settings = BrushSettings::default();

        assert_eq!(settings.radius_px, 10.0);
        assert_eq!(
            settings.color,
            gla_color::PremultipliedRgbaF32::new(0.95, 0.17, 0.10, 1.0)
        );
        assert_eq!(settings.spacing_ratio, 1.0);
        assert_eq!(settings.hardness, 0.7);
        assert_eq!(settings.flow, 1.0);
        assert_eq!(settings.opacity, 1.0);
        assert_eq!(settings.modulations, RoundBrushModulationSet::default());
    }

    #[test]
    fn round_brush_settings_match_dev_field_contract() {
        let settings = RoundBrushSettings::default();

        assert_eq!(settings.base_radius_px, 5.0);
        assert_eq!(settings.spacing_ratio, 1.0);
        assert_eq!(settings.base_hardness, 0.7);
        assert_eq!(settings.base_flow, 1.0);
        assert_eq!(settings.base_opacity, 1.0);
        assert_eq!(settings.tint, [0.0, 0.0, 1.0]);
        assert_eq!(settings.modulations, RoundBrushModulationSet::default());
        assert_eq!(
            settings
                .modulations
                .sample_factor(RoundBrushDabVariable::Radius, 0.25, 0.0, 0.5, 0.5),
            1.0
        );
        assert_eq!(
            settings
                .modulations
                .sample_factor(RoundBrushDabVariable::Flow, 0.25, 0.0, 0.5, 0.5),
            0.25
        );
    }

    #[test]
    fn modulation_curve_validates_and_samples_unit_interval_points() {
        let curve =
            ModulationCurve::new(vec![CurvePoint::new(0.0, 0.2), CurvePoint::new(1.0, 0.8)])
                .unwrap();

        assert!((curve.sample(0.5) - 0.5).abs() < f32::EPSILON);
        assert_eq!(
            ModulationCurve::new(vec![CurvePoint::new(0.0, 1.0)]).unwrap_err(),
            CurveValidationError::TooFewPoints
        );
        assert_eq!(
            ModulationCurve::new(vec![CurvePoint::new(0.5, 1.0), CurvePoint::new(0.5, 0.0)])
                .unwrap_err(),
            CurveValidationError::NotMonotonic { index: 1 }
        );
    }

    #[test]
    fn round_brush_settings_convert_to_runtime_brush_settings() {
        let settings = BrushSettings::from_round_brush(RoundBrushSettings {
            base_radius_px: 12.0,
            spacing_ratio: 0.5,
            base_hardness: 0.4,
            base_flow: 0.75,
            base_opacity: 0.6,
            tint: [0.1, 0.2, 0.3],
            modulations: RoundBrushModulationSet::default().with_curve(
                RoundBrushDabVariable::Radius,
                RoundBrushInputFeature::Pressure,
                ModulationCurve::identity(),
            ),
        });

        assert_eq!(settings.radius_px, 12.0);
        assert_eq!(
            settings.color,
            gla_color::PremultipliedRgbaF32::new(0.1, 0.2, 0.3, 1.0)
        );
        assert_eq!(settings.spacing_ratio, 0.5);
        assert_eq!(settings.hardness, 0.4);
        assert_eq!(settings.flow, 0.75);
        assert_eq!(settings.opacity, 0.6);
        assert_eq!(
            settings
                .modulations
                .sample_factor(RoundBrushDabVariable::Radius, 0.4, 0.0, 0.5, 0.5),
            0.4
        );
    }
}
