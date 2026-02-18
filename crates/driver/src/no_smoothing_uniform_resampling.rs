use crate::{
    InputSamplingAlgorithm, RawPointerInput, SampleEmitter, SampleProcessingError, StrokeContext,
    StrokeSample,
};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NoSmoothingUniformResamplingConfig {
    pub spacing_pixels: f32,
}

impl Default for NoSmoothingUniformResamplingConfig {
    fn default() -> Self {
        Self {
            spacing_pixels: 1.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct SamplePoint {
    timestamp_micros: u64,
    canvas_x: f32,
    canvas_y: f32,
    pressure: f32,
    tilt_x_degrees: f32,
    tilt_y_degrees: f32,
    twist_degrees: f32,
}

#[derive(Debug, Default)]
pub struct NoSmoothingUniformResampling {
    stroke_context: Option<StrokeContext>,
    spacing_pixels: f32,
    last_input: Option<SamplePoint>,
    last_emitted: Option<SamplePoint>,
    distance_since_last_sample: f32,
}

impl NoSmoothingUniformResampling {
    pub fn new() -> Self {
        Self::default()
    }

    fn emit_sample<E>(
        &mut self,
        point: SamplePoint,
        emitter: &mut E,
    ) -> Result<(), SampleProcessingError>
    where
        E: SampleEmitter,
    {
        let velocity_pixels_per_second = match self.last_emitted {
            Some(previous) => {
                let delta_time_micros = point
                    .timestamp_micros
                    .saturating_sub(previous.timestamp_micros);
                if delta_time_micros == 0 {
                    0.0
                } else {
                    let delta_x = point.canvas_x - previous.canvas_x;
                    let delta_y = point.canvas_y - previous.canvas_y;
                    let distance = (delta_x * delta_x + delta_y * delta_y).sqrt();
                    distance / (delta_time_micros as f32 / 1_000_000.0)
                }
            }
            None => 0.0,
        };

        let sample = StrokeSample {
            timestamp_micros: point.timestamp_micros,
            canvas_x: point.canvas_x,
            canvas_y: point.canvas_y,
            pressure: point.pressure,
            velocity_pixels_per_second,
            tilt_x_degrees: point.tilt_x_degrees,
            tilt_y_degrees: point.tilt_y_degrees,
            twist_degrees: point.twist_degrees,
        };

        emitter.emit_sample(sample)?;
        self.last_emitted = Some(point);
        Ok(())
    }
}

impl InputSamplingAlgorithm for NoSmoothingUniformResampling {
    type Config = NoSmoothingUniformResamplingConfig;

    fn begin_stroke(
        &mut self,
        context: StrokeContext,
        config: &Self::Config,
    ) -> Result<(), SampleProcessingError> {
        if !config.spacing_pixels.is_finite() || config.spacing_pixels <= 0.0 {
            return Err(SampleProcessingError::InvalidInput);
        }

        self.stroke_context = Some(context);
        self.spacing_pixels = config.spacing_pixels;
        self.last_input = None;
        self.last_emitted = None;
        self.distance_since_last_sample = 0.0;
        Ok(())
    }

    fn feed_input<E>(
        &mut self,
        input: RawPointerInput,
        emitter: &mut E,
    ) -> Result<(), SampleProcessingError>
    where
        E: SampleEmitter,
    {
        if self.stroke_context.is_none() {
            return Err(SampleProcessingError::InvalidInput);
        }

        let current = SamplePoint {
            timestamp_micros: input.timestamp_micros,
            canvas_x: input.screen_x,
            canvas_y: input.screen_y,
            pressure: input.pressure.unwrap_or(1.0),
            tilt_x_degrees: input.tilt_x_degrees.unwrap_or(0.0),
            tilt_y_degrees: input.tilt_y_degrees.unwrap_or(0.0),
            twist_degrees: input.twist_degrees.unwrap_or(0.0),
        };

        if let Some(previous_input) = self.last_input {
            if current.timestamp_micros < previous_input.timestamp_micros {
                return Err(SampleProcessingError::NonMonotonicTimestamp);
            }

            let mut segment_start = previous_input;
            let segment_end = current;
            let mut segment_dx = segment_end.canvas_x - segment_start.canvas_x;
            let mut segment_dy = segment_end.canvas_y - segment_start.canvas_y;
            let mut segment_length = (segment_dx * segment_dx + segment_dy * segment_dy).sqrt();

            while self.distance_since_last_sample + segment_length >= self.spacing_pixels {
                let distance_to_next_sample = self.spacing_pixels - self.distance_since_last_sample;
                let interpolation_t = if segment_length == 0.0 {
                    0.0
                } else {
                    distance_to_next_sample / segment_length
                };

                let timestamp_delta = segment_end
                    .timestamp_micros
                    .saturating_sub(segment_start.timestamp_micros);
                let next_sample = SamplePoint {
                    timestamp_micros: segment_start.timestamp_micros
                        + ((timestamp_delta as f64) * interpolation_t as f64).round() as u64,
                    canvas_x: segment_start.canvas_x
                        + (segment_end.canvas_x - segment_start.canvas_x) * interpolation_t,
                    canvas_y: segment_start.canvas_y
                        + (segment_end.canvas_y - segment_start.canvas_y) * interpolation_t,
                    pressure: segment_start.pressure
                        + (segment_end.pressure - segment_start.pressure) * interpolation_t,
                    tilt_x_degrees: segment_start.tilt_x_degrees
                        + (segment_end.tilt_x_degrees - segment_start.tilt_x_degrees)
                            * interpolation_t,
                    tilt_y_degrees: segment_start.tilt_y_degrees
                        + (segment_end.tilt_y_degrees - segment_start.tilt_y_degrees)
                            * interpolation_t,
                    twist_degrees: segment_start.twist_degrees
                        + (segment_end.twist_degrees - segment_start.twist_degrees)
                            * interpolation_t,
                };

                self.emit_sample(next_sample, emitter)?;
                self.distance_since_last_sample = 0.0;
                segment_start = next_sample;

                segment_dx = segment_end.canvas_x - segment_start.canvas_x;
                segment_dy = segment_end.canvas_y - segment_start.canvas_y;
                segment_length = (segment_dx * segment_dx + segment_dy * segment_dy).sqrt();
            }

            self.distance_since_last_sample += segment_length;
            self.last_input = Some(current);
            Ok(())
        } else {
            self.emit_sample(current, emitter)?;
            self.last_input = Some(current);
            self.distance_since_last_sample = 0.0;
            Ok(())
        }
    }

    fn end_stroke<E>(&mut self, _emitter: &mut E) -> Result<(), SampleProcessingError>
    where
        E: SampleEmitter,
    {
        self.stroke_context = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::{PointerDeviceKind, PointerEventPhase, SampleChunkBuilder};

    use super::*;

    fn pointer_input(timestamp_micros: u64, x: f32, y: f32) -> RawPointerInput {
        RawPointerInput {
            pointer_id: 1,
            device_kind: PointerDeviceKind::Mouse,
            phase: PointerEventPhase::Move,
            timestamp_micros,
            screen_x: x,
            screen_y: y,
            pressure: None,
            tilt_x_degrees: None,
            tilt_y_degrees: None,
            twist_degrees: None,
        }
    }

    fn chunk_builder() -> SampleChunkBuilder {
        SampleChunkBuilder::new(1, 1, true, false)
    }

    #[test]
    fn emits_first_point_and_uniform_resamples() {
        let mut algorithm = NoSmoothingUniformResampling::new();
        let mut output_chunk = chunk_builder();
        algorithm
            .begin_stroke(
                StrokeContext {
                    stroke_session_id: 1,
                    pointer_id: 1,
                },
                &NoSmoothingUniformResamplingConfig {
                    spacing_pixels: 3.0,
                },
            )
            .expect("begin stroke");

        algorithm
            .feed_input(pointer_input(0, 0.0, 0.0), &mut output_chunk)
            .expect("first input");
        algorithm
            .feed_input(pointer_input(10_000, 10.0, 0.0), &mut output_chunk)
            .expect("second input");

        let chunk = output_chunk.finish();
        assert_eq!(chunk.sample_count(), 4);
        assert_eq!(chunk.canvas_x(), &[0.0, 3.0, 6.0, 9.0]);
    }

    #[test]
    fn resampling_keeps_spacing_across_multiple_segments() {
        let mut algorithm = NoSmoothingUniformResampling::new();
        let mut output_chunk = chunk_builder();
        algorithm
            .begin_stroke(
                StrokeContext {
                    stroke_session_id: 1,
                    pointer_id: 1,
                },
                &NoSmoothingUniformResamplingConfig {
                    spacing_pixels: 3.0,
                },
            )
            .expect("begin stroke");

        algorithm
            .feed_input(pointer_input(0, 0.0, 0.0), &mut output_chunk)
            .expect("first input");
        algorithm
            .feed_input(pointer_input(10_000, 5.0, 0.0), &mut output_chunk)
            .expect("second input");
        algorithm
            .feed_input(pointer_input(20_000, 10.0, 0.0), &mut output_chunk)
            .expect("third input");

        let chunk = output_chunk.finish();
        assert_eq!(chunk.sample_count(), 4);
        assert_eq!(chunk.canvas_x(), &[0.0, 3.0, 6.0, 9.0]);
    }

    #[test]
    fn begin_stroke_rejects_non_positive_spacing() {
        let mut algorithm = NoSmoothingUniformResampling::new();
        let error = algorithm
            .begin_stroke(
                StrokeContext {
                    stroke_session_id: 1,
                    pointer_id: 1,
                },
                &NoSmoothingUniformResamplingConfig {
                    spacing_pixels: 0.0,
                },
            )
            .expect_err("invalid spacing should fail");
        assert_eq!(error, SampleProcessingError::InvalidInput);
    }

    #[test]
    fn rejects_non_monotonic_timestamp_input() {
        let mut algorithm = NoSmoothingUniformResampling::new();
        let mut output_chunk = chunk_builder();
        algorithm
            .begin_stroke(
                StrokeContext {
                    stroke_session_id: 1,
                    pointer_id: 1,
                },
                &NoSmoothingUniformResamplingConfig {
                    spacing_pixels: 2.0,
                },
            )
            .expect("begin stroke");

        algorithm
            .feed_input(pointer_input(10, 0.0, 0.0), &mut output_chunk)
            .expect("first input");
        let error = algorithm
            .feed_input(pointer_input(9, 1.0, 0.0), &mut output_chunk)
            .expect_err("non-monotonic timestamp should fail");

        assert_eq!(error, SampleProcessingError::NonMonotonicTimestamp);
    }
}
