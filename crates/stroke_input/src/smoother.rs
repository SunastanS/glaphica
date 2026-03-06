use glaphica_core::{CanvasVec2, MappedCursor, RadianVec2};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExponentialMovingAverageConfig {
    pub position_alpha: f32,
    pub pressure_alpha: f32,
    pub tilt_alpha: f32,
    pub twist_alpha: f32,
}

impl Default for ExponentialMovingAverageConfig {
    fn default() -> Self {
        Self {
            position_alpha: 0.3,
            pressure_alpha: 0.5,
            tilt_alpha: 0.3,
            twist_alpha: 0.3,
        }
    }
}

pub trait SmoothingStrategy {
    fn smooth(&mut self, sample: MappedCursor) -> MappedCursor;
    fn reset(&mut self);
}

pub struct ExponentialMovingAverage {
    config: ExponentialMovingAverageConfig,
    last_smoothed: Option<MappedCursor>,
}

impl ExponentialMovingAverage {
    pub fn new(config: ExponentialMovingAverageConfig) -> Self {
        Self {
            config,
            last_smoothed: None,
        }
    }

    fn lerp_f32(a: f32, b: f32, t: f32) -> f32 {
        a + (b - a) * t
    }

    fn lerp_vec2(a: CanvasVec2, b: CanvasVec2, t: f32) -> CanvasVec2 {
        CanvasVec2::new(Self::lerp_f32(a.x, b.x, t), Self::lerp_f32(a.y, b.y, t))
    }

    fn lerp_radian(a: RadianVec2, b: RadianVec2, t: f32) -> RadianVec2 {
        RadianVec2::new(Self::lerp_f32(a.x, b.x, t), Self::lerp_f32(a.y, b.y, t))
    }
}

impl SmoothingStrategy for ExponentialMovingAverage {
    fn smooth(&mut self, sample: MappedCursor) -> MappedCursor {
        let smoothed = match self.last_smoothed {
            None => sample,
            Some(last) => MappedCursor {
                cursor: Self::lerp_vec2(last.cursor, sample.cursor, self.config.position_alpha),
                pressure: Self::lerp_f32(
                    last.pressure,
                    sample.pressure,
                    self.config.pressure_alpha,
                ),
                tilt: Self::lerp_radian(last.tilt, sample.tilt, self.config.tilt_alpha),
                twist: Self::lerp_f32(last.twist, sample.twist, self.config.twist_alpha),
            },
        };
        self.last_smoothed = Some(smoothed);
        smoothed
    }

    fn reset(&mut self) {
        self.last_smoothed = None;
    }
}

pub struct NoSmoothing;

impl SmoothingStrategy for NoSmoothing {
    fn smooth(&mut self, sample: MappedCursor) -> MappedCursor {
        sample
    }

    fn reset(&mut self) {}
}
