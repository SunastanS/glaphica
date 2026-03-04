use glaphica_core::{CanvasVec2, MappedCursor, RadianVec2};

/// Configuration for the resampler
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResamplerConfig {
    /// Minimum distance between samples (in canvas units)
    pub min_distance: f32,
    /// Maximum distance between samples (in canvas units)
    pub max_distance: f32,
    /// Minimum time between samples (in seconds)
    pub min_time_s: f32,
    /// Maximum time between samples (in seconds)
    pub max_time_s: f32,
}

impl Default for ResamplerConfig {
    fn default() -> Self {
        Self {
            min_distance: 0.5,  // Half a pixel minimum
            max_distance: 10.0, // Maximum 10 pixels between samples
            min_time_s: 0.001,  // 1ms minimum
            max_time_s: 0.05,   // 50ms maximum (20Hz minimum)
        }
    }
}

/// Result of attempting to add a sample
#[derive(Debug, Clone, PartialEq)]
pub enum ResampleResult {
    /// Sample was accepted as-is
    Accepted(MappedCursor),
    /// Sample was rejected (too close to previous)
    Rejected,
    /// Sample triggered interpolation (returning interpolated points with timestamps)
    Interpolated(Vec<(MappedCursor, u64)>),
}

/// Resampler that generates uniformly spaced samples
pub struct DistanceResampler {
    config: ResamplerConfig,
    last_sample: Option<(MappedCursor, u64)>, // (cursor, timestamp_ns)
}

impl DistanceResampler {
    pub fn new(config: ResamplerConfig) -> Self {
        Self {
            config,
            last_sample: None,
        }
    }

    /// Attempt to add a new sample
    pub fn add_sample(&mut self, cursor: MappedCursor, timestamp_ns: u64) -> ResampleResult {
        let Some((last_cursor, last_time)) = self.last_sample else {
            self.last_sample = Some((cursor, timestamp_ns));
            return ResampleResult::Accepted(cursor);
        };

        let delta_x = cursor.cursor.x - last_cursor.cursor.x;
        let delta_y = cursor.cursor.y - last_cursor.cursor.y;
        let distance = (delta_x * delta_x + delta_y * delta_y).sqrt();
        let delta_time_ns = timestamp_ns.saturating_sub(last_time);
        let delta_time_s = delta_time_ns as f32 / 1_000_000_000.0;

        // Check minimum constraints
        if distance < self.config.min_distance && delta_time_s < self.config.min_time_s {
            return ResampleResult::Rejected;
        }

        // If distance is acceptable, accept directly
        if distance <= self.config.max_distance && delta_time_s <= self.config.max_time_s {
            self.last_sample = Some((cursor, timestamp_ns));
            return ResampleResult::Accepted(cursor);
        }

        // Need interpolation
        let distance_segments = if self.config.max_distance > 0.0 {
            (distance / self.config.max_distance).ceil() as usize
        } else {
            1
        };
        let time_segments = if self.config.max_time_s > 0.0 {
            (delta_time_s / self.config.max_time_s).ceil() as usize
        } else {
            1
        };
        let segment_count = distance_segments.max(time_segments).max(1);
        let mut interpolated = Vec::with_capacity(segment_count);

        for i in 1..=segment_count {
            let t = i as f32 / segment_count as f32;
            let interpolated_cursor = Self::interpolate_cursor(last_cursor, cursor, t);
            let interpolated_time = last_time + (delta_time_ns as f32 * t) as u64;
            interpolated.push((interpolated_cursor, interpolated_time));
        }

        self.last_sample = Some((cursor, timestamp_ns));
        ResampleResult::Interpolated(interpolated)
    }

    fn interpolate_cursor(a: MappedCursor, b: MappedCursor, t: f32) -> MappedCursor {
        MappedCursor {
            cursor: CanvasVec2::new(
                a.cursor.x + (b.cursor.x - a.cursor.x) * t,
                a.cursor.y + (b.cursor.y - a.cursor.y) * t,
            ),
            pressure: a.pressure + (b.pressure - a.pressure) * t,
            tilt: RadianVec2::new(
                a.tilt.x + (b.tilt.x - a.tilt.x) * t,
                a.tilt.y + (b.tilt.y - a.tilt.y) * t,
            ),
            twist: a.twist + (b.twist - a.twist) * t,
        }
    }

    /// Reset the resampler state
    pub fn reset(&mut self) {
        self.last_sample = None;
    }
}

#[cfg(test)]
mod tests {
    use glaphica_core::{CanvasVec2, MappedCursor, RadianVec2};

    use super::{DistanceResampler, ResampleResult, ResamplerConfig};

    fn cursor(x: f32, y: f32) -> MappedCursor {
        MappedCursor {
            cursor: CanvasVec2::new(x, y),
            tilt: RadianVec2::new(0.0, 0.0),
            pressure: 1.0,
            twist: 0.0,
        }
    }

    #[test]
    fn interpolation_includes_current_endpoint() {
        let mut resampler = DistanceResampler::new(ResamplerConfig {
            min_distance: 0.0,
            max_distance: 10.0,
            min_time_s: 0.0,
            max_time_s: 1.0,
        });

        assert_eq!(
            resampler.add_sample(cursor(0.0, 0.0), 0),
            ResampleResult::Accepted(cursor(0.0, 0.0))
        );

        let interpolated = resampler.add_sample(cursor(30.0, 0.0), 30_000_000);
        let points = match interpolated {
            ResampleResult::Interpolated(points) => points,
            _ => panic!("expected interpolated points"),
        };
        assert_eq!(points.len(), 3);
        assert!((points[0].0.cursor.x - 10.0).abs() < 0.001);
        assert!((points[1].0.cursor.x - 20.0).abs() < 0.001);
        assert!((points[2].0.cursor.x - 30.0).abs() < 0.001);
        assert_eq!(points[2].1, 30_000_000);
    }
}
