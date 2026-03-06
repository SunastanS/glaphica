#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UnitIntervalPoint {
    pub x: f32,
    pub y: f32,
}

impl UnitIntervalPoint {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum BrushConfigKind {
    ScalarF32 { min: f32, max: f32 },
    UnitIntervalCurve,
}

#[derive(Debug, Clone, PartialEq)]
pub enum BrushConfigValue {
    ScalarF32(f32),
    UnitIntervalCurve(Vec<UnitIntervalPoint>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct BrushConfigItem {
    pub key: &'static str,
    pub label: &'static str,
    pub kind: BrushConfigKind,
    pub default_value: BrushConfigValue,
}

pub fn eval_unit_interval_curve_polynomial(points: &[UnitIntervalPoint], x: f32) -> Option<f32> {
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
