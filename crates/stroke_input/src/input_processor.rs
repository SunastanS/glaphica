use glaphica_core::{BrushInput, BrushInputFlags, CanvasVec2, MappedCursor, StrokeId};

use crate::resampler::{DistanceResampler, ResampleResult, ResamplerConfig};
use crate::smoother::{
    ExponentialMovingAverage, ExponentialMovingAverageConfig, SmoothingStrategy,
};

/// Configuration for input processing
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InputProcessingConfig {
    /// Smoothing configuration
    pub smoothing: ExponentialMovingAverageConfig,
    /// Resampling configuration
    pub resampling: ResamplerConfig,
    /// Velocity calculation window size (number of samples)
    pub velocity_window_size: usize,
    /// Curvature calculation window size (number of samples)
    pub curvature_window_size: usize,
}

impl Default for InputProcessingConfig {
    fn default() -> Self {
        Self {
            smoothing: ExponentialMovingAverageConfig::default(),
            resampling: ResamplerConfig::default(),
            velocity_window_size: crate::VELOCITY_WINDOW_SIZE,
            curvature_window_size: crate::CURVATURE_WINDOW_SIZE,
        }
    }
}

/// A processed sample with all derived values
#[derive(Debug, Clone, Copy)]
struct ProcessedSample {
    cursor: MappedCursor,
    timestamp_ns: u64,
    path_s: f32,
    delta_s: f32,
    dt_s: f32,
}

/// Main processor that converts raw cursor input to BrushInput
pub struct StrokeInputProcessor {
    config: InputProcessingConfig,
    smoother: ExponentialMovingAverage,
    resampler: DistanceResampler,
    stroke_id: Option<StrokeId>,
    /// History of processed samples for derivative calculations
    history: Vec<ProcessedSample>,
    /// Current path length
    total_path_s: f32,
    /// Last output timestamp for dt calculation
    last_output_time_ns: Option<u64>,
}

impl StrokeInputProcessor {
    pub fn new(config: InputProcessingConfig) -> Self {
        Self {
            smoother: ExponentialMovingAverage::new(config.smoothing),
            resampler: DistanceResampler::new(config.resampling),
            config,
            stroke_id: None,
            history: Vec::with_capacity(crate::config::HISTORY_CAPACITY),
            total_path_s: 0.0,
            last_output_time_ns: None,
        }
    }

    /// Begin a new stroke
    pub fn begin_stroke(&mut self, stroke_id: StrokeId) {
        self.stroke_id = Some(stroke_id);
        self.smoother.reset();
        self.resampler.reset();
        self.history.clear();
        self.total_path_s = 0.0;
        self.last_output_time_ns = None;
    }

    /// End the current stroke
    pub fn end_stroke(&mut self) {
        self.stroke_id = None;
        self.smoother.reset();
        self.resampler.reset();
        self.history.clear();
        self.total_path_s = 0.0;
        self.last_output_time_ns = None;
    }

    /// Process a raw cursor input and return BrushInput samples
    pub fn process_input(
        &mut self,
        stroke_id: StrokeId,
        cursor: MappedCursor,
        timestamp_ns: u64,
    ) -> Vec<BrushInput> {
        let smoothed = self.smoother.smooth(cursor);

        let resampled = match self.resampler.add_sample(smoothed, timestamp_ns) {
            ResampleResult::Rejected => {
                return Vec::new();
            }
            ResampleResult::Accepted(c) => {
                vec![(c, timestamp_ns)]
            }
            ResampleResult::Interpolated(points) => {
                points
            }
        };

        let result: Vec<BrushInput> = resampled
            .into_iter()
            .filter_map(|(cursor, ts)| self.convert_to_brush_input(stroke_id, cursor, ts))
            .collect();

        result
    }

    fn convert_to_brush_input(
        &mut self,
        stroke_id: StrokeId,
        cursor: MappedCursor,
        timestamp_ns: u64,
    ) -> Option<BrushInput> {
        let dt_s = match self.last_output_time_ns {
            None => 0.0,
            Some(last) => (timestamp_ns - last) as f32 / 1_000_000_000.0,
        };
        self.last_output_time_ns = Some(timestamp_ns);

        let delta_s = match self.history.last() {
            None => 0.0,
            Some(last) => {
                let dx = cursor.cursor.x - last.cursor.cursor.x;
                let dy = cursor.cursor.y - last.cursor.cursor.y;
                (dx * dx + dy * dy).sqrt()
            }
        };

        self.total_path_s += delta_s;

        let sample = ProcessedSample {
            cursor,
            timestamp_ns,
            path_s: self.total_path_s,
            delta_s,
            dt_s,
        };

        self.history.push(sample);

        // Keep history bounded
        let max_history = self
            .config
            .velocity_window_size
            .max(self.config.curvature_window_size)
            + 2;
        if self.history.len() > max_history {
            self.history.remove(0);
        }

        // Calculate derivatives
        let (vel, speed, tangent) = self.calculate_velocity();
        let (acc, accel, curvature) = self.calculate_acceleration_and_curvature();

        let mut flags = BrushInputFlags::PATH_S
            | BrushInputFlags::DELTA_S
            | BrushInputFlags::DT_S
            | BrushInputFlags::CONFIDENCE;

        if speed > 0.0 {
            flags |= BrushInputFlags::VEL | BrushInputFlags::SPEED | BrushInputFlags::TANGENT;
        }
        if accel > 0.0 {
            flags |= BrushInputFlags::ACC | BrushInputFlags::ACCEL | BrushInputFlags::CURVATURE;
        }

        Some(BrushInput {
            stroke: stroke_id,
            cursor,
            flags,
            path_s: self.total_path_s,
            delta_s,
            dt_s,
            vel,
            speed,
            tangent,
            acc,
            accel,
            curvature,
            confidence: 1.0,
        })
    }

    fn calculate_velocity(&self) -> (CanvasVec2, f32, CanvasVec2) {
        if self.history.len() < 2 {
            return (CanvasVec2::new(0.0, 0.0), 0.0, CanvasVec2::new(1.0, 0.0));
        }

        let window_size = self.config.velocity_window_size.min(self.history.len());
        let start_idx = self.history.len() - window_size;

        let mut total_vel = CanvasVec2::new(0.0, 0.0);
        let mut count = 0;

        for i in (start_idx + 1)..self.history.len() {
            let prev = &self.history[i - 1];
            let curr = &self.history[i];

            if curr.dt_s > 0.0 {
                let vx = (curr.cursor.cursor.x - prev.cursor.cursor.x) / curr.dt_s;
                let vy = (curr.cursor.cursor.y - prev.cursor.cursor.y) / curr.dt_s;
                total_vel = CanvasVec2::new(total_vel.x + vx, total_vel.y + vy);
                count += 1;
            }
        }

        if count == 0 {
            return (CanvasVec2::new(0.0, 0.0), 0.0, CanvasVec2::new(1.0, 0.0));
        }

        let avg_vel = CanvasVec2::new(total_vel.x / count as f32, total_vel.y / count as f32);
        let speed = (avg_vel.x * avg_vel.x + avg_vel.y * avg_vel.y).sqrt();

        let tangent = if speed > 0.0001 {
            CanvasVec2::new(avg_vel.x / speed, avg_vel.y / speed)
        } else {
            CanvasVec2::new(1.0, 0.0)
        };

        (avg_vel, speed, tangent)
    }

    fn calculate_acceleration_and_curvature(&self) -> (CanvasVec2, f32, f32) {
        if self.history.len() < 3 {
            return (CanvasVec2::new(0.0, 0.0), 0.0, 0.0);
        }

        let window_size = self.config.curvature_window_size.min(self.history.len());
        let start_idx = self.history.len().saturating_sub(window_size);

        let mut total_acc = CanvasVec2::new(0.0, 0.0);
        let mut count = 0;

        for i in (start_idx + 2)..self.history.len() {
            let prev = &self.history[i - 2];
            let curr = &self.history[i];

            let dt = (curr.timestamp_ns - prev.timestamp_ns) as f32 / 1_000_000_000.0;
            if dt > 0.0 {
                let prev_vel = if prev.dt_s > 0.0 {
                    let dx = curr.cursor.cursor.x - prev.cursor.cursor.x;
                    let dy = curr.cursor.cursor.y - prev.cursor.cursor.y;
                    CanvasVec2::new(dx / prev.dt_s, dy / prev.dt_s)
                } else {
                    CanvasVec2::new(0.0, 0.0)
                };

                let curr_vel = if curr.dt_s > 0.0 {
                    let dx = curr.cursor.cursor.x - prev.cursor.cursor.x;
                    let dy = curr.cursor.cursor.y - prev.cursor.cursor.y;
                    CanvasVec2::new(dx / curr.dt_s, dy / curr.dt_s)
                } else {
                    CanvasVec2::new(0.0, 0.0)
                };

                let acc_x = (curr_vel.x - prev_vel.x) / dt;
                let acc_y = (curr_vel.y - prev_vel.y) / dt;
                total_acc = CanvasVec2::new(total_acc.x + acc_x, total_acc.y + acc_y);
                count += 1;
            }
        }

        if count == 0 {
            return (CanvasVec2::new(0.0, 0.0), 0.0, 0.0);
        }

        let avg_acc = CanvasVec2::new(total_acc.x / count as f32, total_acc.y / count as f32);
        let accel = (avg_acc.x * avg_acc.x + avg_acc.y * avg_acc.y).sqrt();

        // Calculate curvature using velocity and acceleration
        let (vel, speed, _) = self.calculate_velocity();
        let curvature = if speed > 0.0001 {
            // κ = |v × a| / |v|³
            let cross_product = vel.x * avg_acc.y - vel.y * avg_acc.x;
            cross_product.abs() / (speed * speed * speed)
        } else {
            0.0
        };

        (avg_acc, accel, curvature)
    }
}
