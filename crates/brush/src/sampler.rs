use glaphica_core::CanvasVec2;

use crate::smoother::{
    CommittedCanvasSample, CommittedCanvasSpan, CommittedCanvasSpanBuffer, add_canvas_vec2,
    distance_between, scale_canvas_vec2, span_hermite_tangents, subtract_canvas_vec2,
    vector_length,
};

pub trait StrokeSampler: Send {
    fn reset(&mut self);

    fn sample_committed_spans(
        &mut self,
        spans: &CommittedCanvasSpanBuffer,
        output: &mut Vec<CommittedCanvasSample>,
    );
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct EquidistantSamplerCursor {
    next_sample_s: f32,
}

impl EquidistantSamplerCursor {
    pub fn new(next_sample_s: f32) -> Self {
        Self {
            next_sample_s: next_sample_s.max(0.0),
        }
    }

    pub fn next_sample_s(&self) -> f32 {
        self.next_sample_s
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EquidistantCurveSampler {
    spacing: f32,
}

impl EquidistantCurveSampler {
    pub fn new(spacing: f32) -> Self {
        Self {
            spacing: spacing.max(f32::EPSILON),
        }
    }

    pub fn spacing(&self) -> f32 {
        self.spacing
    }

    pub fn sample_spans(
        &self,
        spans: &CommittedCanvasSpanBuffer,
        cursor: &mut EquidistantSamplerCursor,
        output: &mut Vec<CommittedCanvasSample>,
    ) {
        if spans.is_empty() {
            return;
        }

        let arclength_tables = spans
            .spans()
            .iter()
            .map(SpanArcTable::from_span)
            .collect::<Vec<_>>();
        let span_global_starts = span_global_starts(spans.global_s_start(), &arclength_tables);
        let batch_end_s = batch_end_s(
            spans.global_s_start(),
            &span_global_starts,
            &arclength_tables,
        );
        while cursor.next_sample_s <= batch_end_s {
            output.push(sample_at_global_s(
                spans.spans(),
                &arclength_tables,
                &span_global_starts,
                cursor.next_sample_s,
            ));
            cursor.next_sample_s += self.spacing;
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct EquidistantStrokeSampler {
    curve_sampler: EquidistantCurveSampler,
    cursor: EquidistantSamplerCursor,
}

impl EquidistantStrokeSampler {
    pub fn new(spacing: f32) -> Self {
        Self {
            curve_sampler: EquidistantCurveSampler::new(spacing),
            cursor: EquidistantSamplerCursor::default(),
        }
    }

    pub fn spacing(&self) -> f32 {
        self.curve_sampler.spacing()
    }

    pub fn set_spacing(&mut self, spacing: f32) {
        self.curve_sampler = EquidistantCurveSampler::new(spacing);
    }
}

impl StrokeSampler for EquidistantStrokeSampler {
    fn reset(&mut self) {
        self.cursor = EquidistantSamplerCursor::default();
    }

    fn sample_committed_spans(
        &mut self,
        spans: &CommittedCanvasSpanBuffer,
        output: &mut Vec<CommittedCanvasSample>,
    ) {
        self.curve_sampler
            .sample_spans(spans, &mut self.cursor, output);
    }
}

pub(crate) fn span_arclength(span: &CommittedCanvasSpan) -> f32 {
    SpanArcTable::from_span(span).total_length()
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct ArcLengthSample {
    t: f32,
    position: CanvasVec2,
    cumulative_s: f32,
}

#[derive(Debug, Clone, PartialEq)]
struct SpanArcTable {
    samples: Vec<ArcLengthSample>,
}

impl SpanArcTable {
    fn from_span(span: &CommittedCanvasSpan) -> Self {
        const FLATNESS_TOLERANCE_PX: f32 = 0.25;
        const MAX_SUBDIVISION_DEPTH: u8 = 8;
        const MAX_ARCLENGTH_SAMPLE_T_STEP: f32 = 1.0 / 32.0;

        let start_position = span.sample(0.0).position;
        let end_position = span.sample(1.0).position;
        let mut samples = Vec::with_capacity(9);
        samples.push(ArcLengthSample {
            t: 0.0,
            position: start_position,
            cumulative_s: 0.0,
        });

        if distance_between(start_position, end_position) <= f32::EPSILON && span.is_stationary() {
            samples.push(ArcLengthSample {
                t: 1.0,
                position: end_position,
                cumulative_s: 0.0,
            });
            return Self { samples };
        }

        let duration_s = span.duration_ns() as f32 * 1e-9;
        if duration_s <= f32::EPSILON {
            samples.push(ArcLengthSample {
                t: 1.0,
                position: end_position,
                cumulative_s: distance_between(start_position, end_position),
            });
            return Self { samples };
        }

        let control_points = hermite_to_bezier_control_points(span, duration_s);
        let mut stack = vec![BezierSegment::new(control_points, 0.0, 1.0, 0)];
        let mut cumulative_s = 0.0;
        let mut previous_position = start_position;

        while let Some(segment) = stack.pop() {
            if segment.should_subdivide(FLATNESS_TOLERANCE_PX, MAX_SUBDIVISION_DEPTH) {
                let (left, right) = segment.split();
                stack.push(right);
                stack.push(left);
                continue;
            }

            cumulative_s += distance_between(previous_position, segment.control_points[3]);
            previous_position = segment.control_points[3];
            samples.push(ArcLengthSample {
                t: segment.t_end,
                position: segment.control_points[3],
                cumulative_s,
            });
        }

        if samples
            .last()
            .is_none_or(|sample| sample.t < 1.0 || sample.position != end_position)
        {
            cumulative_s += distance_between(previous_position, end_position);
            samples.push(ArcLengthSample {
                t: 1.0,
                position: end_position,
                cumulative_s,
            });
        }

        Self {
            samples: densify_arclength_samples(span, &samples, MAX_ARCLENGTH_SAMPLE_T_STEP),
        }
    }

    fn total_length(&self) -> f32 {
        self.samples
            .last()
            .map(|sample| sample.cumulative_s)
            .unwrap_or(0.0)
    }

    fn t_at_cumulative_s(&self, cumulative_s: f32) -> f32 {
        let clamped_s = cumulative_s.clamp(0.0, self.total_length());
        let Some((start, end)) = self.segment_for_cumulative_s(clamped_s) else {
            return 0.0;
        };
        interpolate_segment_scalar(
            start.cumulative_s,
            end.cumulative_s,
            clamped_s,
            start.t,
            end.t,
        )
    }

    fn segment_for_cumulative_s(
        &self,
        cumulative_s: f32,
    ) -> Option<(ArcLengthSample, ArcLengthSample)> {
        if self.samples.len() < 2 {
            return None;
        }

        for window in self.samples.windows(2) {
            let start = window[0];
            let end = window[1];
            if cumulative_s <= end.cumulative_s {
                return Some((start, end));
            }
        }

        self.samples
            .get(self.samples.len().saturating_sub(2))
            .copied()
            .zip(self.samples.last().copied())
    }
}

fn span_global_starts(global_s_start: f32, arclength_tables: &[SpanArcTable]) -> Vec<f32> {
    let mut global_s = global_s_start;
    let mut starts = Vec::with_capacity(arclength_tables.len());
    for arclength_table in arclength_tables {
        starts.push(global_s);
        global_s += arclength_table.total_length();
    }
    starts
}

fn batch_end_s(
    global_s_start: f32,
    span_global_starts: &[f32],
    arclength_tables: &[SpanArcTable],
) -> f32 {
    span_global_starts
        .last()
        .copied()
        .zip(arclength_tables.last())
        .map(|(start, table)| start + table.total_length())
        .unwrap_or(global_s_start)
}

fn sample_at_global_s(
    spans: &[CommittedCanvasSpan],
    arclength_tables: &[SpanArcTable],
    span_global_starts: &[f32],
    global_s: f32,
) -> CommittedCanvasSample {
    let span_index = span_index_for_global_s(spans.len(), span_global_starts, global_s);
    let span = spans
        .get(span_index)
        .copied()
        .unwrap_or_else(|| spans[spans.len().saturating_sub(1)]);
    let arclength_table = arclength_tables
        .get(span_index)
        .unwrap_or(&arclength_tables[arclength_tables.len().saturating_sub(1)]);
    let local_s =
        (global_s - span_global_starts[span_index]).clamp(0.0, arclength_table.total_length());
    let t = arclength_table.t_at_cumulative_s(local_s);
    span.sample(t)
}

fn span_index_for_global_s(span_count: usize, span_global_starts: &[f32], global_s: f32) -> usize {
    span_global_starts
        .partition_point(|&span_start| span_start <= global_s)
        .saturating_sub(1)
        .min(span_count.saturating_sub(1))
}

fn densify_arclength_samples(
    span: &CommittedCanvasSpan,
    coarse_samples: &[ArcLengthSample],
    max_t_step: f32,
) -> Vec<ArcLengthSample> {
    if coarse_samples.len() < 2 {
        return coarse_samples.to_vec();
    }

    let clamped_max_t_step = max_t_step.clamp(f32::EPSILON, 1.0);
    let mut densified = Vec::new();
    let mut previous_position = coarse_samples[0].position;
    let mut cumulative_s = 0.0;
    densified.push(ArcLengthSample {
        t: coarse_samples[0].t,
        position: coarse_samples[0].position,
        cumulative_s,
    });

    for window in coarse_samples.windows(2) {
        let start = window[0];
        let end = window[1];
        let subdivisions = ((end.t - start.t) / clamped_max_t_step).ceil().max(1.0) as usize;
        for subdivision_index in 1..=subdivisions {
            let interpolation_t = subdivision_index as f32 / subdivisions as f32;
            let sample_t = if subdivision_index == subdivisions {
                end.t
            } else {
                start.t + (end.t - start.t) * interpolation_t
            };
            let position = if subdivision_index == subdivisions {
                end.position
            } else {
                span.sample(sample_t).position
            };
            cumulative_s += distance_between(previous_position, position);
            densified.push(ArcLengthSample {
                t: sample_t,
                position,
                cumulative_s,
            });
            previous_position = position;
        }
    }

    densified
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct BezierSegment {
    control_points: [CanvasVec2; 4],
    t_start: f32,
    t_end: f32,
    depth: u8,
}

impl BezierSegment {
    fn new(control_points: [CanvasVec2; 4], t_start: f32, t_end: f32, depth: u8) -> Self {
        Self {
            control_points,
            t_start,
            t_end,
            depth,
        }
    }

    fn should_subdivide(&self, flatness_tolerance_px: f32, max_depth: u8) -> bool {
        self.depth < max_depth && cubic_bezier_flatness(self.control_points) > flatness_tolerance_px
    }

    fn split(self) -> (Self, Self) {
        let p01 = midpoint_canvas_vec2(self.control_points[0], self.control_points[1]);
        let p12 = midpoint_canvas_vec2(self.control_points[1], self.control_points[2]);
        let p23 = midpoint_canvas_vec2(self.control_points[2], self.control_points[3]);
        let p012 = midpoint_canvas_vec2(p01, p12);
        let p123 = midpoint_canvas_vec2(p12, p23);
        let p0123 = midpoint_canvas_vec2(p012, p123);
        let t_mid = (self.t_start + self.t_end) * 0.5;
        let next_depth = self.depth.saturating_add(1);

        (
            Self::new(
                [self.control_points[0], p01, p012, p0123],
                self.t_start,
                t_mid,
                next_depth,
            ),
            Self::new(
                [p0123, p123, p23, self.control_points[3]],
                t_mid,
                self.t_end,
                next_depth,
            ),
        )
    }
}

fn hermite_to_bezier_control_points(
    span: &CommittedCanvasSpan,
    duration_s: f32,
) -> [CanvasVec2; 4] {
    let tangents = span_hermite_tangents(
        span.start.position,
        span.start.velocity,
        span.end.position,
        span.end.velocity,
        duration_s,
    );
    [
        span.start.position,
        add_canvas_vec2(
            span.start.position,
            scale_canvas_vec2(tangents.start_delta, 1.0 / 3.0),
        ),
        subtract_canvas_vec2(
            span.end.position,
            scale_canvas_vec2(tangents.end_delta, 1.0 / 3.0),
        ),
        span.end.position,
    ]
}

fn cubic_bezier_flatness(control_points: [CanvasVec2; 4]) -> f32 {
    let chord = subtract_canvas_vec2(control_points[3], control_points[0]);
    let chord_length = vector_length(chord);
    if chord_length <= f32::EPSILON {
        return vector_length(subtract_canvas_vec2(control_points[1], control_points[0])).max(
            vector_length(subtract_canvas_vec2(control_points[2], control_points[0])),
        );
    }

    let inv_chord_length = 1.0 / chord_length;
    let distance_1 = point_line_distance(
        control_points[1],
        control_points[0],
        chord,
        inv_chord_length,
    );
    let distance_2 = point_line_distance(
        control_points[2],
        control_points[0],
        chord,
        inv_chord_length,
    );
    distance_1.max(distance_2)
}

fn point_line_distance(
    point: CanvasVec2,
    line_origin: CanvasVec2,
    line_direction: CanvasVec2,
    inv_line_length: f32,
) -> f32 {
    let offset = subtract_canvas_vec2(point, line_origin);
    ((offset.x * line_direction.y - offset.y * line_direction.x).abs()) * inv_line_length
}

fn interpolate_segment_scalar(
    start_domain: f32,
    end_domain: f32,
    value: f32,
    start_range: f32,
    end_range: f32,
) -> f32 {
    let domain_extent = end_domain - start_domain;
    if domain_extent.abs() <= f32::EPSILON {
        return end_range;
    }
    let t = ((value - start_domain) / domain_extent).clamp(0.0, 1.0);
    start_range * (1.0 - t) + end_range * t
}

fn midpoint_canvas_vec2(lhs: CanvasVec2, rhs: CanvasVec2) -> CanvasVec2 {
    CanvasVec2::new((lhs.x + rhs.x) * 0.5, (lhs.y + rhs.y) * 0.5)
}

#[cfg(test)]
mod tests {
    use glaphica_core::RadianVec2;

    use super::{EquidistantCurveSampler, EquidistantSamplerCursor, SpanArcTable, span_arclength};
    use crate::{
        CommittedCanvasSpan, CommittedCanvasSpanBuffer, CurveKnot, DistanceOrTimeStrokeSmoother,
        StrokeSmoother,
    };
    use glaphica_core::{CanvasInput, CanvasVec2};

    #[test]
    fn span_buffer_samples_by_arclength() {
        let mut smoother = DistanceOrTimeStrokeSmoother::new(5.0, u64::MAX);
        let mut spans = CommittedCanvasSpanBuffer::new();
        let mut output = Vec::new();
        let sampler = EquidistantCurveSampler::new(4.0);
        let mut cursor = EquidistantSamplerCursor::default();

        smoother
            .push_canvas_inputs(&[
                CanvasInput {
                    time_ns: 0,
                    position: CanvasVec2::new(0.0, 0.0),
                    pressure: 0.1,
                    tilt: RadianVec2::new(0.0, 0.0),
                    twist: 0.0,
                },
                CanvasInput {
                    time_ns: 10,
                    position: CanvasVec2::new(10.0, 0.0),
                    pressure: 0.5,
                    tilt: RadianVec2::new(0.0, 0.0),
                    twist: 0.0,
                },
            ])
            .expect("push inputs");
        smoother.finish_stroke();
        smoother.pop_committed_spans(&mut spans).expect("pop spans");

        output.push(spans.spans()[0].sample(0.0));
        cursor = EquidistantSamplerCursor::new(4.0);
        sampler.sample_spans(&spans, &mut cursor, &mut output);

        assert_eq!(output[0].position, CanvasVec2::new(0.0, 0.0));
        assert_eq!(output[1].position, CanvasVec2::new(4.0, 0.0));
        assert_eq!(output[2].position, CanvasVec2::new(8.0, 0.0));
        assert!((cursor.next_sample_s() - 12.0).abs() < 1e-5);
    }

    #[test]
    fn curved_span_arclength_sampling_advances_along_curve() {
        let mut spans = CommittedCanvasSpanBuffer::new();
        let mut output = Vec::new();
        let sampler = EquidistantCurveSampler::new(4.0);
        let mut cursor = EquidistantSamplerCursor::new(4.0);
        spans.push_span(CommittedCanvasSpan {
            start: CurveKnot {
                time_ns: 0,
                position: CanvasVec2::new(0.0, 0.0),
                pressure: 1.0,
                tilt: RadianVec2::new(0.0, 0.0),
                twist: 0.0,
                velocity: CanvasVec2::new(10.0, 0.0),
                acceleration: CanvasVec2::new(0.0, 0.0),
                cumulative_s: 0.0,
            },
            end: CurveKnot {
                time_ns: 1_000_000_000,
                position: CanvasVec2::new(10.0, 10.0),
                pressure: 1.0,
                tilt: RadianVec2::new(0.0, 0.0),
                twist: 0.0,
                velocity: CanvasVec2::new(0.0, 10.0),
                acceleration: CanvasVec2::new(0.0, 0.0),
                cumulative_s: 20.0,
            },
        });

        output.push(spans.spans()[0].sample(0.0));
        sampler.sample_spans(&spans, &mut cursor, &mut output);

        assert_eq!(output.len(), 4);
        assert_eq!(output[0].position, CanvasVec2::new(0.0, 0.0));
        assert!(output[1].position.x > 3.0);
        assert!(output[1].position.y > 0.0);
        assert!(output[2].position.x > output[1].position.x);
        assert!(output[2].position.y > output[1].position.y);
        assert!(output[3].position.y > output[2].position.y);
        assert!(output[3].position.x < 10.0);
        assert!(cursor.next_sample_s() < 20.0);
    }

    #[test]
    fn global_arclength_cursor_stays_uniform_across_batches() {
        let make_span = |start_x: f32, end_x: f32, start_s: f32, end_s: f32| CommittedCanvasSpan {
            start: CurveKnot {
                time_ns: start_x as u64,
                position: CanvasVec2::new(start_x, 0.0),
                pressure: 1.0,
                tilt: RadianVec2::new(0.0, 0.0),
                twist: 0.0,
                velocity: CanvasVec2::new(1.0, 0.0),
                acceleration: CanvasVec2::new(0.0, 0.0),
                cumulative_s: start_s,
            },
            end: CurveKnot {
                time_ns: end_x as u64,
                position: CanvasVec2::new(end_x, 0.0),
                pressure: 1.0,
                tilt: RadianVec2::new(0.0, 0.0),
                twist: 0.0,
                velocity: CanvasVec2::new(1.0, 0.0),
                acceleration: CanvasVec2::new(0.0, 0.0),
                cumulative_s: end_s,
            },
        };

        let sampler = EquidistantCurveSampler::new(4.0);
        let mut first_batch = CommittedCanvasSpanBuffer::new();
        first_batch.push_span(make_span(0.0, 6.0, 0.0, 6.0));

        let mut second_batch = CommittedCanvasSpanBuffer::new();
        second_batch.set_global_s_start(6.0);
        second_batch.push_span(make_span(6.0, 12.0, 6.0, 12.0));

        let mut cursor = EquidistantSamplerCursor::new(4.0);
        let mut output = Vec::new();
        sampler.sample_spans(&first_batch, &mut cursor, &mut output);
        sampler.sample_spans(&second_batch, &mut cursor, &mut output);

        assert_eq!(output.len(), 3);
        assert_eq!(output[0].position, CanvasVec2::new(4.0, 0.0));
        assert_eq!(output[1].position, CanvasVec2::new(8.0, 0.0));
        assert_eq!(output[2].position, CanvasVec2::new(12.0, 0.0));
        assert!((cursor.next_sample_s() - 16.0).abs() < 1e-5);
    }

    #[test]
    fn arclength_lookup_stays_close_to_requested_distance_for_parametrically_uneven_line_span() {
        let span = CommittedCanvasSpan {
            start: CurveKnot {
                time_ns: 0,
                position: CanvasVec2::new(249.09917, 359.9114),
                pressure: 1.0,
                tilt: RadianVec2::new(0.0, 0.0),
                twist: 0.0,
                velocity: CanvasVec2::new(1656.6141, 63.716),
                acceleration: CanvasVec2::new(0.0, 0.0),
                cumulative_s: 0.0,
            },
            end: CurveKnot {
                time_ns: 1_213,
                position: CanvasVec2::new(287.98526, 362.07175),
                pressure: 1.0,
                tilt: RadianVec2::new(0.0, 0.0),
                twist: 0.0,
                velocity: CanvasVec2::new(32_057_784.0, 1_780_988.8),
                acceleration: CanvasVec2::new(0.0, 0.0),
                cumulative_s: 38.946056,
            },
        };
        let table = SpanArcTable::from_span(&span);
        let local_s = 4.9336395;
        let sample = span.sample(table.t_at_cumulative_s(local_s));
        let distance_from_start =
            crate::smoother::distance_between(span.start.position, sample.position);

        assert!(
            distance_from_start >= 4.0,
            "distance_from_start={} local_s={}",
            distance_from_start,
            local_s
        );
    }

    #[test]
    fn arclength_cursor_does_not_resample_origin_after_initial_press_batch() {
        let mut smoother = DistanceOrTimeStrokeSmoother::new(0.0, 0);
        let sampler = EquidistantCurveSampler::new(5.0);
        let mut first_batch = CommittedCanvasSpanBuffer::new();
        let mut next_batch = CommittedCanvasSpanBuffer::new();
        let mut output = Vec::new();
        let mut cursor = EquidistantSamplerCursor::default();

        smoother
            .push_canvas_input(CanvasInput {
                time_ns: 0,
                position: CanvasVec2::new(0.0, 0.0),
                pressure: 0.5,
                tilt: RadianVec2::new(0.0, 0.0),
                twist: 0.0,
            })
            .expect("press input");
        smoother
            .pop_committed_spans(&mut first_batch)
            .expect("first batch");
        sampler.sample_spans(&first_batch, &mut cursor, &mut output);

        smoother
            .push_canvas_inputs(&[
                CanvasInput {
                    time_ns: 1,
                    position: CanvasVec2::new(0.5, 0.0),
                    pressure: 0.5,
                    tilt: RadianVec2::new(0.0, 0.0),
                    twist: 0.0,
                },
                CanvasInput {
                    time_ns: 2,
                    position: CanvasVec2::new(1.0, 0.0),
                    pressure: 0.5,
                    tilt: RadianVec2::new(0.0, 0.0),
                    twist: 0.0,
                },
                CanvasInput {
                    time_ns: 3,
                    position: CanvasVec2::new(8.0, 0.0),
                    pressure: 0.5,
                    tilt: RadianVec2::new(0.0, 0.0),
                    twist: 0.0,
                },
            ])
            .expect("motion inputs");
        smoother
            .pop_committed_spans(&mut next_batch)
            .expect("second batch");
        sampler.sample_spans(&next_batch, &mut cursor, &mut output);

        smoother.finish_stroke();
        smoother
            .pop_committed_spans(&mut next_batch)
            .expect("finish batch");
        sampler.sample_spans(&next_batch, &mut cursor, &mut output);

        let origin_count = output
            .iter()
            .filter(|sample| sample.position.x.abs() <= 1e-5 && sample.position.y.abs() <= 1e-5)
            .count();
        assert_eq!(origin_count, 1, "{output:?}");
    }

    #[test]
    fn span_arclength_matches_sampler_batch_progress() {
        let span = CommittedCanvasSpan {
            start: CurveKnot {
                time_ns: 0,
                position: CanvasVec2::new(0.0, 0.0),
                pressure: 1.0,
                tilt: RadianVec2::new(0.0, 0.0),
                twist: 0.0,
                velocity: CanvasVec2::new(10.0, 0.0),
                acceleration: CanvasVec2::new(0.0, 0.0),
                cumulative_s: 0.0,
            },
            end: CurveKnot {
                time_ns: 10,
                position: CanvasVec2::new(10.0, 0.0),
                pressure: 1.0,
                tilt: RadianVec2::new(0.0, 0.0),
                twist: 0.0,
                velocity: CanvasVec2::new(10.0, 0.0),
                acceleration: CanvasVec2::new(0.0, 0.0),
                cumulative_s: 10.0,
            },
        };

        assert!((span_arclength(&span) - 10.0).abs() < 1e-5);
    }
}
