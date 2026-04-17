use std::collections::VecDeque;
use std::fmt::{Display, Formatter};

use glaphica_core::{CanvasInput, CanvasVec2, RadianVec2};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CurveKnot {
    pub time_ns: u64,
    pub position: CanvasVec2,
    pub pressure: f32,
    pub tilt: RadianVec2,
    pub twist: f32,
    pub velocity: CanvasVec2,
    pub acceleration: CanvasVec2,
    pub cumulative_s: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CommittedCanvasSpan {
    pub start: CurveKnot,
    pub end: CurveKnot,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CommittedCanvasSample {
    pub time_ns: u64,
    pub position: CanvasVec2,
    pub pressure: f32,
    pub tilt: RadianVec2,
    pub twist: f32,
    pub velocity: CanvasVec2,
    pub acceleration: CanvasVec2,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StrokeSmootherError {
    NonMonotonicTime {
        previous_time_ns: u64,
        current_time_ns: u64,
    },
    InvalidInputValue {
        input_index: usize,
        value_index: usize,
    },
}

impl Display for StrokeSmootherError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NonMonotonicTime {
                previous_time_ns,
                current_time_ns,
            } => write!(
                f,
                "canvas input time moved backwards: previous {}, current {}",
                previous_time_ns, current_time_ns
            ),
            Self::InvalidInputValue {
                input_index,
                value_index,
            } => write!(
                f,
                "canvas input {} contains invalid value at index {}",
                input_index, value_index
            ),
        }
    }
}

pub trait StrokeSmoother: Send {
    fn clear(&mut self);

    fn push_canvas_input(&mut self, input: CanvasInput) -> Result<(), StrokeSmootherError>;

    fn push_canvas_inputs(&mut self, input: &[CanvasInput]) -> Result<(), StrokeSmootherError> {
        for &sample in input {
            self.push_canvas_input(sample)?;
        }
        Ok(())
    }

    fn finish_stroke(&mut self);

    fn pop_committed_spans(
        &mut self,
        output: &mut CommittedCanvasSpanBuffer,
    ) -> Result<usize, StrokeSmootherError>;
}

#[derive(Debug, Clone, PartialEq)]
pub struct PassthroughStrokeSmoother {
    knots: VecDeque<CurveKnot>,
    next_input_index: usize,
    emitted_prefix: usize,
    initial_point_emitted: bool,
    emitted_arclength: f32,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct CommittedCanvasSpanBuffer {
    spans: Vec<CommittedCanvasSpan>,
    global_s_start: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct SpanSampleCursor {
    span_index: usize,
    span_t: f32,
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

#[derive(Debug, Clone, Copy, PartialEq)]
struct HermiteTangents {
    start_delta: CanvasVec2,
    end_delta: CanvasVec2,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ArcLengthCursor {
    next_sample_s: f32,
}

impl ArcLengthCursor {
    pub fn new(next_sample_s: f32) -> Self {
        Self {
            next_sample_s: next_sample_s.max(0.0),
        }
    }

    pub fn next_sample_s(&self) -> f32 {
        self.next_sample_s
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct StrokeCurveBuffer {
    raw_inputs: VecDeque<CanvasInput>,
    knots: VecDeque<CurveKnot>,
    stable_end: usize,
    emitted_prefix: usize,
    initial_point_emitted: bool,
    next_input_index: usize,
    freeze_distance: f32,
    freeze_time_ns: u64,
    smoothing_radius_samples: usize,
    emitted_arclength: f32,
    finished: bool,
}

impl CommittedCanvasSpan {
    pub fn duration_ns(&self) -> u64 {
        self.end.time_ns.saturating_sub(self.start.time_ns)
    }

    pub fn arclength(&self) -> f32 {
        self.end.cumulative_s - self.start.cumulative_s
    }

    pub fn is_stationary(&self) -> bool {
        self.arclength() <= f32::EPSILON
    }

    pub fn sample(&self, t: f32) -> CommittedCanvasSample {
        let clamped_t = t.clamp(0.0, 1.0);
        let lerp_f32 = |start: f32, end: f32| start * (1.0 - clamped_t) + end * clamped_t;
        let lerp_vec2 = |start: CanvasVec2, end: CanvasVec2| {
            CanvasVec2::new(lerp_f32(start.x, end.x), lerp_f32(start.y, end.y))
        };
        let lerp_radian = |start: RadianVec2, end: RadianVec2| {
            RadianVec2::new(lerp_f32(start.x, end.x), lerp_f32(start.y, end.y))
        };
        let duration_s = self.duration_s();
        let tangents = self.hermite_tangents(duration_s);
        let (position, velocity, acceleration) = if duration_s <= f32::EPSILON {
            (
                lerp_vec2(self.start.position, self.end.position),
                lerp_vec2(self.start.velocity, self.end.velocity),
                lerp_vec2(self.start.acceleration, self.end.acceleration),
            )
        } else {
            (
                hermite_canvas_position(
                    self.start.position,
                    self.end.position,
                    tangents,
                    clamped_t,
                ),
                hermite_canvas_velocity(
                    self.start.position,
                    self.end.position,
                    tangents,
                    duration_s,
                    clamped_t,
                ),
                hermite_canvas_acceleration(
                    self.start.position,
                    self.end.position,
                    tangents,
                    duration_s,
                    clamped_t,
                ),
            )
        };

        CommittedCanvasSample {
            time_ns: lerp_u64(self.start.time_ns, self.end.time_ns, clamped_t),
            position,
            pressure: lerp_f32(self.start.pressure, self.end.pressure),
            tilt: lerp_radian(self.start.tilt, self.end.tilt),
            twist: lerp_f32(self.start.twist, self.end.twist),
            velocity,
            acceleration,
        }
    }

    fn duration_s(&self) -> f32 {
        self.duration_ns() as f32 * 1e-9
    }

    fn hermite_tangents(&self, duration_s: f32) -> HermiteTangents {
        span_hermite_tangents(
            self.start.position,
            self.start.velocity,
            self.end.position,
            self.end.velocity,
            duration_s,
        )
    }

    fn build_arclength_table(&self) -> SpanArcTable {
        SpanArcTable::from_span(self)
    }
}

impl CommittedCanvasSpanBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clear(&mut self) {
        self.spans.clear();
        self.global_s_start = 0.0;
    }

    pub fn len(&self) -> usize {
        self.spans.len()
    }

    pub fn is_empty(&self) -> bool {
        self.spans.is_empty()
    }

    pub fn global_s_start(&self) -> f32 {
        self.global_s_start
    }

    pub fn spans(&self) -> &[CommittedCanvasSpan] {
        &self.spans
    }

    pub fn push_span(&mut self, span: CommittedCanvasSpan) {
        self.spans.push(span);
    }

    fn set_global_s_start(&mut self, global_s_start: f32) {
        self.global_s_start = global_s_start.max(0.0);
    }

    pub fn sample_by_arclength_from(
        &self,
        spacing: f32,
        cursor: &mut ArcLengthCursor,
        output: &mut Vec<CommittedCanvasSample>,
    ) {
        if self.spans.is_empty() {
            return;
        }

        let step = spacing.max(f32::EPSILON);
        let arclength_tables = self
            .spans
            .iter()
            .map(CommittedCanvasSpan::build_arclength_table)
            .collect::<Vec<_>>();
        let span_global_starts = self.span_global_starts(&arclength_tables);
        let batch_end_s = self.batch_end_s(&span_global_starts, &arclength_tables);
        while cursor.next_sample_s <= batch_end_s {
            output.push(self.sample_at_global_s(
                &arclength_tables,
                &span_global_starts,
                cursor.next_sample_s,
            ));
            cursor.next_sample_s += step;
        }
    }

    pub fn sample_by_arclength(
        &self,
        spacing: f32,
        carry_distance: f32,
        emit_start: bool,
        output: &mut Vec<CommittedCanvasSample>,
    ) -> f32 {
        if self.spans.is_empty() {
            return carry_distance.max(0.0);
        }

        let step = spacing.max(f32::EPSILON);
        let carry = carry_distance.max(0.0) % step;

        if emit_start {
            output.push(self.spans[0].sample(0.0));
        }

        let initial_advance = if carry <= f32::EPSILON {
            step
        } else {
            step - carry
        };
        let mut cursor = ArcLengthCursor {
            next_sample_s: self.global_s_start + initial_advance,
        };
        let before = output.len();
        let arclength_tables = self
            .spans
            .iter()
            .map(CommittedCanvasSpan::build_arclength_table)
            .collect::<Vec<_>>();
        let span_global_starts = self.span_global_starts(&arclength_tables);
        let batch_end_s = self.batch_end_s(&span_global_starts, &arclength_tables);
        while cursor.next_sample_s <= batch_end_s {
            output.push(self.sample_at_global_s(
                &arclength_tables,
                &span_global_starts,
                cursor.next_sample_s,
            ));
            cursor.next_sample_s += step;
        }

        if output.len() == before && initial_advance > batch_end_s - self.global_s_start {
            return (carry + (batch_end_s - self.global_s_start)).min(step);
        }

        let overshoot = (cursor.next_sample_s - batch_end_s).clamp(0.0, step);
        if overshoot <= f32::EPSILON {
            0.0
        } else {
            step - overshoot
        }
    }

    pub fn sample_by_time(
        &self,
        step_ns: u64,
        carry_time_ns: u64,
        emit_start: bool,
        output: &mut Vec<CommittedCanvasSample>,
    ) -> u64 {
        if self.spans.is_empty() {
            return carry_time_ns;
        }

        let step = step_ns.max(1);
        let mut carry = carry_time_ns % step;
        let mut cursor = SpanSampleCursor {
            span_index: 0,
            span_t: 0.0,
        };

        if emit_start {
            output.push(self.spans[0].sample(0.0));
        }

        while let Some((next_cursor, sample)) =
            self.advance_by_time(cursor, step.saturating_sub(carry))
        {
            output.push(sample);
            cursor = next_cursor;
            carry = 0;
        }

        carry = carry.saturating_add(self.remaining_time_ns_from(cursor));
        if carry >= step {
            carry %= step;
        }
        carry
    }

    pub fn sample_stationary_only(
        &self,
        step_ns: u64,
        carry_time_ns: u64,
        output: &mut Vec<CommittedCanvasSample>,
    ) -> u64 {
        if self.spans.is_empty() {
            return carry_time_ns;
        }

        let step = step_ns.max(1);
        let mut carry = carry_time_ns % step;
        for span in &self.spans {
            if !span.is_stationary() {
                carry = 0;
                continue;
            }
            let duration = span.duration_ns();
            let mut next_sample_at = step.saturating_sub(carry);
            while next_sample_at <= duration {
                output.push(span.sample(ns_ratio(next_sample_at, duration)));
                next_sample_at = next_sample_at.saturating_add(step);
            }
            carry = duration.saturating_add(carry) % step;
        }

        carry
    }

    fn advance_by_time(
        &self,
        cursor: SpanSampleCursor,
        duration_ns: u64,
    ) -> Option<(SpanSampleCursor, CommittedCanvasSample)> {
        let mut remaining = duration_ns;
        let mut span_index = cursor.span_index;
        let mut span_t = cursor.span_t.clamp(0.0, 1.0);

        while let Some(span) = self.spans.get(span_index) {
            let span_duration_ns = span.duration_ns();
            let remaining_span_ns = remaining_span_ns(span_duration_ns, span_t);
            if remaining_span_ns == 0 {
                span_index += 1;
                span_t = 0.0;
                continue;
            }
            if remaining <= remaining_span_ns {
                let span_t_f64 = f64::from(span_t);
                let t = span_t_f64 + ns_ratio_f64(remaining, span_duration_ns) * (1.0 - span_t_f64);
                return Some((
                    SpanSampleCursor {
                        span_index,
                        span_t: t as f32,
                    },
                    span.sample(t as f32),
                ));
            }
            remaining = remaining.saturating_sub(remaining_span_ns);
            span_index += 1;
            span_t = 0.0;
        }

        None
    }

    fn span_global_starts(&self, arclength_tables: &[SpanArcTable]) -> Vec<f32> {
        let mut global_s = self.global_s_start;
        let mut starts = Vec::with_capacity(arclength_tables.len());
        for arclength_table in arclength_tables {
            starts.push(global_s);
            global_s += arclength_table.total_length();
        }
        starts
    }

    fn batch_end_s(&self, span_global_starts: &[f32], arclength_tables: &[SpanArcTable]) -> f32 {
        span_global_starts
            .last()
            .copied()
            .zip(arclength_tables.last())
            .map(|(start, table)| start + table.total_length())
            .unwrap_or(self.global_s_start)
    }

    fn sample_at_global_s(
        &self,
        arclength_tables: &[SpanArcTable],
        span_global_starts: &[f32],
        global_s: f32,
    ) -> CommittedCanvasSample {
        let span_index = self.span_index_for_global_s(span_global_starts, global_s);
        let span = self
            .spans
            .get(span_index)
            .copied()
            .unwrap_or_else(|| self.spans[self.spans.len().saturating_sub(1)]);
        let arclength_table = arclength_tables
            .get(span_index)
            .unwrap_or(&arclength_tables[arclength_tables.len().saturating_sub(1)]);
        let local_s =
            (global_s - span_global_starts[span_index]).clamp(0.0, arclength_table.total_length());
        let t = arclength_table.t_at_cumulative_s(local_s);
        span.sample(t)
    }

    fn span_index_for_global_s(&self, span_global_starts: &[f32], global_s: f32) -> usize {
        span_global_starts
            .partition_point(|&span_start| span_start <= global_s)
            .saturating_sub(1)
            .min(self.spans.len().saturating_sub(1))
    }

    fn remaining_time_ns_from(&self, cursor: SpanSampleCursor) -> u64 {
        let mut total = 0u64;
        for (index, span) in self.spans.iter().enumerate().skip(cursor.span_index) {
            let span_duration_ns = span.duration_ns();
            if index == cursor.span_index {
                total = total.saturating_add(remaining_span_ns(span_duration_ns, cursor.span_t));
            } else {
                total = total.saturating_add(span_duration_ns);
            }
        }
        total
    }
}

impl StrokeCurveBuffer {
    pub fn new(freeze_distance: f32, freeze_time_ns: u64) -> Self {
        Self {
            raw_inputs: VecDeque::new(),
            knots: VecDeque::new(),
            stable_end: 0,
            emitted_prefix: 0,
            initial_point_emitted: false,
            next_input_index: 0,
            freeze_distance: freeze_distance.max(0.0),
            freeze_time_ns,
            smoothing_radius_samples: 2,
            emitted_arclength: 0.0,
            finished: false,
        }
    }

    pub fn finish_stroke(&mut self) {
        self.finished = true;
        self.recompute_mutable_tail();
        self.advance_stable_end();
    }

    fn push_input(&mut self, input: CanvasInput) -> Result<(), StrokeSmootherError> {
        validate_canvas_input(input, self.next_input_index)?;
        if let Some(previous) = self.raw_inputs.back() {
            if input.time_ns < previous.time_ns {
                return Err(StrokeSmootherError::NonMonotonicTime {
                    previous_time_ns: previous.time_ns,
                    current_time_ns: input.time_ns,
                });
            }
        }

        let previous_knot = self.knots.back().copied();
        self.raw_inputs.push_back(input);
        self.knots
            .push_back(curve_knot_from_input(previous_knot, input));
        self.next_input_index += 1;
        self.finished = false;
        self.recompute_mutable_tail();
        self.advance_stable_end();
        Ok(())
    }

    fn pop_stable_spans(&mut self, output: &mut CommittedCanvasSpanBuffer) -> usize {
        output.clear();
        output.set_global_s_start(self.emitted_arclength);
        if self.knots.is_empty() {
            return 0;
        }

        let mut count = 0usize;
        if !self.initial_point_emitted && self.stable_end.saturating_sub(self.emitted_prefix) < 2 {
            if let Some(first) = self.knots.front().copied() {
                output.push_span(CommittedCanvasSpan {
                    start: first,
                    end: first,
                });
                self.initial_point_emitted = true;
                count += 1;
            }
        }

        let span_start = self.emitted_prefix.min(self.stable_end);
        if self.stable_end >= span_start + 2 {
            for index in span_start..(self.stable_end - 1) {
                let Some(start) = self.knots.get(index).copied() else {
                    break;
                };
                let Some(end) = self.knots.get(index + 1).copied() else {
                    break;
                };
                let span = CommittedCanvasSpan { start, end };
                self.emitted_arclength += span.build_arclength_table().total_length();
                output.push_span(span);
                count += 1;
            }
            self.emitted_prefix = self.stable_end - 1;
        }

        count
    }

    fn clear(&mut self) {
        self.raw_inputs.clear();
        self.knots.clear();
        self.stable_end = 0;
        self.emitted_prefix = 0;
        self.initial_point_emitted = false;
        self.next_input_index = 0;
        self.emitted_arclength = 0.0;
        self.finished = false;
    }

    fn advance_stable_end(&mut self) {
        if self.finished {
            self.stable_end = self.knots.len();
            return;
        }
        let Some(latest) = self.knots.back().copied() else {
            self.stable_end = 0;
            return;
        };
        let stable_limit = self
            .knots
            .len()
            .saturating_sub(self.smoothing_radius_samples);
        while self.stable_end < self.knots.len() {
            if self.stable_end >= stable_limit {
                break;
            }
            let Some(candidate) = self.knots.get(self.stable_end).copied() else {
                break;
            };
            let distance_ready =
                latest.cumulative_s - candidate.cumulative_s >= self.freeze_distance;
            let time_ready =
                latest.time_ns.saturating_sub(candidate.time_ns) >= self.freeze_time_ns;
            if !(distance_ready || time_ready) {
                break;
            }
            self.stable_end += 1;
        }
    }

    fn recompute_mutable_tail(&mut self) {
        if self.raw_inputs.is_empty() || self.knots.is_empty() {
            return;
        }

        let len = self.knots.len();
        let start = self.stable_end.min(len);
        for index in start..len {
            let input = self.raw_inputs[index];
            let position = self.smoothed_position(index);
            let pressure = self.smoothed_pressure(index);
            let tilt = self.smoothed_tilt(index);
            let twist = self.smoothed_twist(index);
            let previous_knot = if index > 0 {
                self.knots.get(index - 1).copied()
            } else {
                None
            };
            let velocity = velocity_from_previous(previous_knot, input.time_ns, position);
            let acceleration = acceleration_from_previous(previous_knot, input.time_ns, velocity);
            let cumulative_s = previous_knot
                .map(|previous_knot| {
                    previous_knot.cumulative_s + distance_between(previous_knot.position, position)
                })
                .unwrap_or(0.0);

            self.knots[index] = CurveKnot {
                time_ns: input.time_ns,
                position,
                pressure,
                tilt,
                twist,
                velocity,
                acceleration,
                cumulative_s,
            };
        }
    }

    fn smoothed_position(&self, center_index: usize) -> CanvasVec2 {
        if !self.should_smooth_center(center_index) {
            return self.raw_inputs[center_index].position;
        }
        let (x, y, total_weight) = self.window_indices(center_index).fold(
            (0.0, 0.0, 0.0),
            |(x, y, total_weight), index| {
                let weight = self.window_weight(center_index, index);
                (
                    x + self.raw_inputs[index].position.x * weight,
                    y + self.raw_inputs[index].position.y * weight,
                    total_weight + weight,
                )
            },
        );
        if total_weight <= f32::EPSILON {
            return self.raw_inputs[center_index].position;
        }
        CanvasVec2::new(x / total_weight, y / total_weight)
    }

    fn smoothed_pressure(&self, center_index: usize) -> f32 {
        self.window_weighted_scalar(center_index, |input| input.pressure)
    }

    fn smoothed_tilt(&self, center_index: usize) -> RadianVec2 {
        let x = self.window_weighted_scalar(center_index, |input| input.tilt.x);
        let y = self.window_weighted_scalar(center_index, |input| input.tilt.y);
        RadianVec2::new(x, y)
    }

    fn smoothed_twist(&self, center_index: usize) -> f32 {
        self.window_weighted_scalar(center_index, |input| input.twist)
    }

    fn window_weighted_scalar(
        &self,
        center_index: usize,
        project: impl Fn(CanvasInput) -> f32,
    ) -> f32 {
        if !self.should_smooth_center(center_index) {
            return project(self.raw_inputs[center_index]);
        }
        let (value, total_weight) =
            self.window_indices(center_index)
                .fold((0.0, 0.0), |(value, total_weight), index| {
                    let weight = self.window_weight(center_index, index);
                    (
                        value + project(self.raw_inputs[index]) * weight,
                        total_weight + weight,
                    )
                });
        if total_weight <= f32::EPSILON {
            return project(self.raw_inputs[center_index]);
        }
        value / total_weight
    }

    fn window_indices(&self, center_index: usize) -> impl Iterator<Item = usize> + '_ {
        let start = center_index.saturating_sub(self.smoothing_radius_samples);
        let end = (center_index + self.smoothing_radius_samples + 1).min(self.raw_inputs.len());
        start..end
    }

    fn window_weight(&self, center_index: usize, sample_index: usize) -> f32 {
        let distance = center_index.abs_diff(sample_index);
        if distance > self.smoothing_radius_samples {
            0.0
        } else {
            (self.smoothing_radius_samples + 1 - distance) as f32
        }
    }

    fn should_smooth_center(&self, center_index: usize) -> bool {
        if center_index == 0 {
            return false;
        }
        if self.finished {
            return self.window_indices(center_index).count() >= 3;
        }
        center_index + self.smoothing_radius_samples < self.raw_inputs.len()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DistanceOrTimeStrokeSmoother {
    curve: StrokeCurveBuffer,
}

impl Default for DistanceOrTimeStrokeSmoother {
    fn default() -> Self {
        Self::new(5.0, 16_000_000)
    }
}

impl Default for PassthroughStrokeSmoother {
    fn default() -> Self {
        Self {
            knots: VecDeque::new(),
            next_input_index: 0,
            emitted_prefix: 0,
            initial_point_emitted: false,
            emitted_arclength: 0.0,
        }
    }
}

impl StrokeSmoother for PassthroughStrokeSmoother {
    fn clear(&mut self) {
        self.knots.clear();
        self.next_input_index = 0;
        self.emitted_prefix = 0;
        self.initial_point_emitted = false;
        self.emitted_arclength = 0.0;
    }

    fn push_canvas_input(&mut self, input: CanvasInput) -> Result<(), StrokeSmootherError> {
        validate_canvas_input(input, self.next_input_index)?;
        if let Some(previous) = self.knots.back() {
            if input.time_ns < previous.time_ns {
                return Err(StrokeSmootherError::NonMonotonicTime {
                    previous_time_ns: previous.time_ns,
                    current_time_ns: input.time_ns,
                });
            }
        }

        let previous_knot = self.knots.back().copied();
        self.knots
            .push_back(curve_knot_from_input(previous_knot, input));
        self.next_input_index += 1;
        Ok(())
    }

    fn finish_stroke(&mut self) {}

    fn pop_committed_spans(
        &mut self,
        output: &mut CommittedCanvasSpanBuffer,
    ) -> Result<usize, StrokeSmootherError> {
        output.clear();
        output.set_global_s_start(self.emitted_arclength);
        if self.knots.is_empty() {
            return Ok(0);
        }

        let mut count = 0usize;
        if !self.initial_point_emitted {
            let Some(first) = self.knots.front().copied() else {
                return Ok(0);
            };
            output.push_span(CommittedCanvasSpan {
                start: first,
                end: first,
            });
            self.initial_point_emitted = true;
            count += 1;
        }

        let last_segment_start = self.knots.len().saturating_sub(1);
        let span_start = self.emitted_prefix.min(last_segment_start);
        if self.knots.len() >= span_start + 2 {
            for index in span_start..last_segment_start {
                let Some(start) = self.knots.get(index).copied() else {
                    break;
                };
                let Some(end) = self.knots.get(index + 1).copied() else {
                    break;
                };
                let span = CommittedCanvasSpan { start, end };
                self.emitted_arclength += span.build_arclength_table().total_length();
                output.push_span(span);
                count += 1;
            }
            self.emitted_prefix = last_segment_start;
        }

        Ok(count)
    }
}

impl DistanceOrTimeStrokeSmoother {
    pub fn new(freeze_distance: f32, freeze_time_ns: u64) -> Self {
        Self {
            curve: StrokeCurveBuffer::new(freeze_distance, freeze_time_ns),
        }
    }
}

impl StrokeSmoother for DistanceOrTimeStrokeSmoother {
    fn clear(&mut self) {
        self.curve.clear();
    }

    fn push_canvas_input(&mut self, input: CanvasInput) -> Result<(), StrokeSmootherError> {
        self.curve.push_input(input)
    }

    fn finish_stroke(&mut self) {
        self.curve.finish_stroke();
    }

    fn pop_committed_spans(
        &mut self,
        output: &mut CommittedCanvasSpanBuffer,
    ) -> Result<usize, StrokeSmootherError> {
        Ok(self.curve.pop_stable_spans(output))
    }
}

fn validate_canvas_input(
    input: CanvasInput,
    input_index: usize,
) -> Result<(), StrokeSmootherError> {
    let values = [
        input.position.x,
        input.position.y,
        input.pressure,
        input.tilt.x,
        input.tilt.y,
        input.twist,
    ];
    for (value_index, value) in values.into_iter().enumerate() {
        if !value.is_finite() {
            return Err(StrokeSmootherError::InvalidInputValue {
                input_index,
                value_index,
            });
        }
    }
    Ok(())
}

fn curve_knot_from_input(previous_knot: Option<CurveKnot>, input: CanvasInput) -> CurveKnot {
    let velocity = velocity_from_previous(previous_knot, input.time_ns, input.position);
    let acceleration = acceleration_from_previous(previous_knot, input.time_ns, velocity);
    let cumulative_s = previous_knot
        .map(|previous_knot| {
            previous_knot.cumulative_s + distance_between(previous_knot.position, input.position)
        })
        .unwrap_or(0.0);

    CurveKnot {
        time_ns: input.time_ns,
        position: input.position,
        pressure: input.pressure,
        tilt: input.tilt,
        twist: input.twist,
        velocity,
        acceleration,
        cumulative_s,
    }
}

fn velocity_from_previous(
    previous_knot: Option<CurveKnot>,
    time_ns: u64,
    position: CanvasVec2,
) -> CanvasVec2 {
    previous_knot
        .map(|previous_knot| {
            let duration_s = (time_ns.saturating_sub(previous_knot.time_ns)) as f32 * 1e-9;
            if duration_s <= f32::EPSILON {
                return CanvasVec2::new(0.0, 0.0);
            }
            let delta = subtract_canvas_vec2(position, previous_knot.position);
            CanvasVec2::new(delta.x / duration_s, delta.y / duration_s)
        })
        .unwrap_or(CanvasVec2::new(0.0, 0.0))
}

fn acceleration_from_previous(
    previous_knot: Option<CurveKnot>,
    time_ns: u64,
    velocity: CanvasVec2,
) -> CanvasVec2 {
    previous_knot
        .map(|previous_knot| {
            let duration_s = (time_ns.saturating_sub(previous_knot.time_ns)) as f32 * 1e-9;
            if duration_s <= f32::EPSILON {
                return CanvasVec2::new(0.0, 0.0);
            }
            let delta = subtract_canvas_vec2(velocity, previous_knot.velocity);
            CanvasVec2::new(delta.x / duration_s, delta.y / duration_s)
        })
        .unwrap_or(CanvasVec2::new(0.0, 0.0))
}

fn distance_between(lhs: CanvasVec2, rhs: CanvasVec2) -> f32 {
    let delta = subtract_canvas_vec2(lhs, rhs);
    (delta.x * delta.x + delta.y * delta.y).sqrt()
}

fn lerp_u64(start: u64, end: u64, t: f32) -> u64 {
    if end <= start {
        return start;
    }
    let delta = end - start;
    start.saturating_add((delta as f64 * f64::from(t.clamp(0.0, 1.0))).round() as u64)
}

fn subtract_canvas_vec2(lhs: CanvasVec2, rhs: CanvasVec2) -> CanvasVec2 {
    CanvasVec2::new(lhs.x - rhs.x, lhs.y - rhs.y)
}

fn ns_ratio(value_ns: u64, duration_ns: u64) -> f32 {
    if duration_ns == 0 {
        return 1.0;
    }
    (value_ns as f64 / duration_ns as f64) as f32
}

fn ns_ratio_f64(value_ns: u64, duration_ns: u64) -> f64 {
    if duration_ns == 0 {
        return 1.0;
    }
    value_ns as f64 / duration_ns as f64
}

fn remaining_span_ns(duration_ns: u64, span_t: f32) -> u64 {
    (duration_ns as f64 * (1.0 - f64::from(span_t.clamp(0.0, 1.0)))).round() as u64
}

fn span_hermite_tangents(
    start_position: CanvasVec2,
    start_velocity: CanvasVec2,
    end_position: CanvasVec2,
    end_velocity: CanvasVec2,
    duration_s: f32,
) -> HermiteTangents {
    if duration_s <= f32::EPSILON {
        return HermiteTangents {
            start_delta: CanvasVec2::new(0.0, 0.0),
            end_delta: CanvasVec2::new(0.0, 0.0),
        };
    }

    let chord = subtract_canvas_vec2(end_position, start_position);
    let chord_length = vector_length(chord);
    if chord_length <= f32::EPSILON {
        return HermiteTangents {
            start_delta: CanvasVec2::new(0.0, 0.0),
            end_delta: CanvasVec2::new(0.0, 0.0),
        };
    }

    let max_tangent_length = chord_length;
    HermiteTangents {
        start_delta: clamp_canvas_vec2_length(
            scale_canvas_vec2(start_velocity, duration_s),
            max_tangent_length,
        ),
        end_delta: clamp_canvas_vec2_length(
            scale_canvas_vec2(end_velocity, duration_s),
            max_tangent_length,
        ),
    }
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

        let duration_s = span.duration_s();
        if duration_s <= f32::EPSILON {
            samples.push(ArcLengthSample {
                t: 1.0,
                position: end_position,
                cumulative_s: distance_between(start_position, end_position),
            });
            return Self { samples };
        }

        let control_points = hermite_to_bezier_control_points(
            span.start.position,
            span.end.position,
            span.hermite_tangents(duration_s),
        );
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

fn hermite_canvas_position(
    start_position: CanvasVec2,
    end_position: CanvasVec2,
    tangents: HermiteTangents,
    t: f32,
) -> CanvasVec2 {
    let t2 = t * t;
    let t3 = t2 * t;
    let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
    let h10 = t3 - 2.0 * t2 + t;
    let h01 = -2.0 * t3 + 3.0 * t2;
    let h11 = t3 - t2;

    add_canvas_vec2(
        add_canvas_vec2(
            scale_canvas_vec2(start_position, h00),
            scale_canvas_vec2(tangents.start_delta, h10),
        ),
        add_canvas_vec2(
            scale_canvas_vec2(end_position, h01),
            scale_canvas_vec2(tangents.end_delta, h11),
        ),
    )
}

fn hermite_canvas_velocity(
    start_position: CanvasVec2,
    end_position: CanvasVec2,
    tangents: HermiteTangents,
    duration_s: f32,
    t: f32,
) -> CanvasVec2 {
    let t2 = t * t;
    let dh00 = 6.0 * t2 - 6.0 * t;
    let dh10 = 3.0 * t2 - 4.0 * t + 1.0;
    let dh01 = -6.0 * t2 + 6.0 * t;
    let dh11 = 3.0 * t2 - 2.0 * t;
    let derivative = add_canvas_vec2(
        add_canvas_vec2(
            scale_canvas_vec2(start_position, dh00),
            scale_canvas_vec2(tangents.start_delta, dh10),
        ),
        add_canvas_vec2(
            scale_canvas_vec2(end_position, dh01),
            scale_canvas_vec2(tangents.end_delta, dh11),
        ),
    );

    scale_canvas_vec2(derivative, 1.0 / duration_s)
}

fn hermite_canvas_acceleration(
    start_position: CanvasVec2,
    end_position: CanvasVec2,
    tangents: HermiteTangents,
    duration_s: f32,
    t: f32,
) -> CanvasVec2 {
    let d2h00 = 12.0 * t - 6.0;
    let d2h10 = 6.0 * t - 4.0;
    let d2h01 = -12.0 * t + 6.0;
    let d2h11 = 6.0 * t - 2.0;
    let second_derivative = add_canvas_vec2(
        add_canvas_vec2(
            scale_canvas_vec2(start_position, d2h00),
            scale_canvas_vec2(tangents.start_delta, d2h10),
        ),
        add_canvas_vec2(
            scale_canvas_vec2(end_position, d2h01),
            scale_canvas_vec2(tangents.end_delta, d2h11),
        ),
    );

    scale_canvas_vec2(second_derivative, 1.0 / (duration_s * duration_s))
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
    start_position: CanvasVec2,
    end_position: CanvasVec2,
    tangents: HermiteTangents,
) -> [CanvasVec2; 4] {
    [
        start_position,
        add_canvas_vec2(
            start_position,
            scale_canvas_vec2(tangents.start_delta, 1.0 / 3.0),
        ),
        subtract_canvas_vec2(
            end_position,
            scale_canvas_vec2(tangents.end_delta, 1.0 / 3.0),
        ),
        end_position,
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

fn add_canvas_vec2(lhs: CanvasVec2, rhs: CanvasVec2) -> CanvasVec2 {
    CanvasVec2::new(lhs.x + rhs.x, lhs.y + rhs.y)
}

fn scale_canvas_vec2(value: CanvasVec2, scale: f32) -> CanvasVec2 {
    CanvasVec2::new(value.x * scale, value.y * scale)
}

fn midpoint_canvas_vec2(lhs: CanvasVec2, rhs: CanvasVec2) -> CanvasVec2 {
    CanvasVec2::new((lhs.x + rhs.x) * 0.5, (lhs.y + rhs.y) * 0.5)
}

fn vector_length(value: CanvasVec2) -> f32 {
    (value.x * value.x + value.y * value.y).sqrt()
}

fn clamp_canvas_vec2_length(value: CanvasVec2, max_length: f32) -> CanvasVec2 {
    let length = vector_length(value);
    if length <= max_length || length <= f32::EPSILON {
        return value;
    }
    scale_canvas_vec2(value, max_length / length)
}

#[cfg(test)]
mod tests {
    use super::{
        ArcLengthCursor, CommittedCanvasSpan, CommittedCanvasSpanBuffer, CurveKnot,
        DistanceOrTimeStrokeSmoother, StrokeSmoother, StrokeSmootherError, distance_between,
    };
    use glaphica_core::{CanvasInput, CanvasVec2, RadianVec2};

    #[test]
    fn smoother_rejects_non_monotonic_time() {
        let mut smoother = DistanceOrTimeStrokeSmoother::new(8.0, 16_000_000);

        smoother
            .push_canvas_input(CanvasInput {
                time_ns: 10,
                position: CanvasVec2::new(0.0, 0.0),
                pressure: 0.5,
                tilt: RadianVec2::new(0.0, 0.0),
                twist: 0.0,
            })
            .expect("first input");
        let error = smoother
            .push_canvas_input(CanvasInput {
                time_ns: 9,
                position: CanvasVec2::new(1.0, 0.0),
                pressure: 0.5,
                tilt: RadianVec2::new(0.0, 0.0),
                twist: 0.0,
            })
            .expect_err("time should be monotonic");

        assert_eq!(
            error,
            StrokeSmootherError::NonMonotonicTime {
                previous_time_ns: 10,
                current_time_ns: 9,
            }
        );
    }

    #[test]
    fn smoother_stabilizes_after_distance_window() {
        let mut smoother = DistanceOrTimeStrokeSmoother::new(5.0, u64::MAX);
        let mut spans = CommittedCanvasSpanBuffer::new();

        smoother
            .push_canvas_inputs(&[
                CanvasInput {
                    time_ns: 0,
                    position: CanvasVec2::new(0.0, 0.0),
                    pressure: 0.2,
                    tilt: RadianVec2::new(0.0, 0.0),
                    twist: 0.0,
                },
                CanvasInput {
                    time_ns: 10,
                    position: CanvasVec2::new(2.0, 0.0),
                    pressure: 0.2,
                    tilt: RadianVec2::new(0.0, 0.0),
                    twist: 0.0,
                },
                CanvasInput {
                    time_ns: 20,
                    position: CanvasVec2::new(8.0, 0.0),
                    pressure: 0.2,
                    tilt: RadianVec2::new(0.0, 0.0),
                    twist: 0.0,
                },
            ])
            .expect("push inputs");

        let count = smoother.pop_committed_spans(&mut spans).expect("pop spans");

        assert_eq!(count, 1);
        assert_eq!(spans.spans()[0].start.position, CanvasVec2::new(0.0, 0.0));
        assert_eq!(spans.spans()[0].end.position, CanvasVec2::new(0.0, 0.0));
    }

    #[test]
    fn smoother_reestimates_mutable_tail_positions() {
        let mut smoother = DistanceOrTimeStrokeSmoother::new(5.0, u64::MAX);
        let mut spans = CommittedCanvasSpanBuffer::new();

        smoother
            .push_canvas_inputs(&[
                CanvasInput {
                    time_ns: 0,
                    position: CanvasVec2::new(0.0, 0.0),
                    pressure: 0.2,
                    tilt: RadianVec2::new(0.0, 0.0),
                    twist: 0.0,
                },
                CanvasInput {
                    time_ns: 10,
                    position: CanvasVec2::new(2.0, 6.0),
                    pressure: 0.2,
                    tilt: RadianVec2::new(0.0, 0.0),
                    twist: 0.0,
                },
                CanvasInput {
                    time_ns: 20,
                    position: CanvasVec2::new(8.0, 0.0),
                    pressure: 0.2,
                    tilt: RadianVec2::new(0.0, 0.0),
                    twist: 0.0,
                },
                CanvasInput {
                    time_ns: 30,
                    position: CanvasVec2::new(14.0, 0.0),
                    pressure: 0.2,
                    tilt: RadianVec2::new(0.0, 0.0),
                    twist: 0.0,
                },
            ])
            .expect("push inputs");

        let count = smoother.pop_committed_spans(&mut spans).expect("pop spans");

        assert!(count >= 1);
        let smoothed_middle = spans
            .spans()
            .last()
            .expect("at least one committed span")
            .end
            .position;
        assert!(smoothed_middle.x > 2.0);
        assert!(smoothed_middle.y < 6.0);
    }

    #[test]
    fn smoother_stabilizes_stationary_point_after_time_window() {
        let mut smoother = DistanceOrTimeStrokeSmoother::new(f32::MAX, 10);
        let mut spans = CommittedCanvasSpanBuffer::new();

        smoother
            .push_canvas_inputs(&[
                CanvasInput {
                    time_ns: 0,
                    position: CanvasVec2::new(4.0, 5.0),
                    pressure: 0.4,
                    tilt: RadianVec2::new(0.0, 0.0),
                    twist: 0.0,
                },
                CanvasInput {
                    time_ns: 20,
                    position: CanvasVec2::new(4.0, 5.0),
                    pressure: 0.4,
                    tilt: RadianVec2::new(0.0, 0.0),
                    twist: 0.0,
                },
            ])
            .expect("push inputs");

        let count = smoother.pop_committed_spans(&mut spans).expect("pop spans");

        assert_eq!(count, 1);
        assert_eq!(spans.spans()[0].start.position, CanvasVec2::new(4.0, 5.0));
        assert_eq!(spans.spans()[0].end.position, CanvasVec2::new(4.0, 5.0));
    }

    #[test]
    fn finish_stroke_flushes_remaining_tail() {
        let mut smoother = DistanceOrTimeStrokeSmoother::new(f32::MAX, u64::MAX);
        let mut spans = CommittedCanvasSpanBuffer::new();

        smoother
            .push_canvas_inputs(&[
                CanvasInput {
                    time_ns: 0,
                    position: CanvasVec2::new(0.0, 0.0),
                    pressure: 0.5,
                    tilt: RadianVec2::new(0.0, 0.0),
                    twist: 0.0,
                },
                CanvasInput {
                    time_ns: 1,
                    position: CanvasVec2::new(1.0, 0.0),
                    pressure: 0.5,
                    tilt: RadianVec2::new(0.0, 0.0),
                    twist: 0.0,
                },
            ])
            .expect("push inputs");
        assert_eq!(
            smoother.pop_committed_spans(&mut spans).expect("pop spans"),
            1
        );

        smoother.finish_stroke();
        let count = smoother.pop_committed_spans(&mut spans).expect("pop spans");

        assert_eq!(count, 1);
        assert_eq!(spans.spans()[0].start.position, CanvasVec2::new(0.0, 0.0));
        assert_eq!(spans.spans()[0].end.position, CanvasVec2::new(1.0, 0.0));
    }

    #[test]
    fn finish_stroke_smooths_terminal_knot_with_one_sided_window() {
        let mut smoother = DistanceOrTimeStrokeSmoother::new(f32::MAX, u64::MAX);
        let mut spans = CommittedCanvasSpanBuffer::new();

        smoother
            .push_canvas_inputs(&[
                CanvasInput {
                    time_ns: 0,
                    position: CanvasVec2::new(0.0, 0.0),
                    pressure: 0.5,
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
                CanvasInput {
                    time_ns: 20,
                    position: CanvasVec2::new(20.0, 0.0),
                    pressure: 0.5,
                    tilt: RadianVec2::new(0.0, 0.0),
                    twist: 0.0,
                },
                CanvasInput {
                    time_ns: 30,
                    position: CanvasVec2::new(100.0, 0.0),
                    pressure: 0.5,
                    tilt: RadianVec2::new(0.0, 0.0),
                    twist: 0.0,
                },
            ])
            .expect("push inputs");

        smoother.finish_stroke();
        smoother.pop_committed_spans(&mut spans).expect("pop spans");

        let end = spans
            .spans()
            .last()
            .expect("committed span after finish")
            .end
            .position;
        assert!(end.x < 100.0);
        assert!((end.x - 58.333332).abs() < 1e-4);
    }

    #[test]
    fn multi_point_stroke_does_not_emit_initial_zero_length_span() {
        let mut smoother = DistanceOrTimeStrokeSmoother::new(f32::MAX, u64::MAX);
        let mut spans = CommittedCanvasSpanBuffer::new();

        smoother
            .push_canvas_inputs(&[
                CanvasInput {
                    time_ns: 0,
                    position: CanvasVec2::new(0.0, 0.0),
                    pressure: 0.5,
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
        let count = smoother.pop_committed_spans(&mut spans).expect("pop spans");

        assert_eq!(count, 1);
        assert_ne!(spans.spans()[0].start.time_ns, spans.spans()[0].end.time_ns);
    }

    #[test]
    fn sample_by_arclength_normalizes_initial_carry() {
        let mut spans = CommittedCanvasSpanBuffer::new();
        let mut output = Vec::new();
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
                time_ns: 10,
                position: CanvasVec2::new(10.0, 0.0),
                pressure: 1.0,
                tilt: RadianVec2::new(0.0, 0.0),
                twist: 0.0,
                velocity: CanvasVec2::new(10.0, 0.0),
                acceleration: CanvasVec2::new(0.0, 0.0),
                cumulative_s: 10.0,
            },
        });

        let carry = spans.sample_by_arclength(4.0, 9.0, true, &mut output);

        assert_eq!(output[0].position, CanvasVec2::new(0.0, 0.0));
        assert!(output[1].position.x > 2.9);
        assert!(output[1].position.x < 3.1);
        assert!((carry - 3.0).abs() < 1e-5);
    }

    #[test]
    fn sample_by_time_normalizes_initial_carry() {
        let mut spans = CommittedCanvasSpanBuffer::new();
        let mut output = Vec::new();
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
                time_ns: 10,
                position: CanvasVec2::new(10.0, 0.0),
                pressure: 1.0,
                tilt: RadianVec2::new(0.0, 0.0),
                twist: 0.0,
                velocity: CanvasVec2::new(10.0, 0.0),
                acceleration: CanvasVec2::new(0.0, 0.0),
                cumulative_s: 10.0,
            },
        });

        let carry = spans.sample_by_time(4, 9, true, &mut output);

        assert_eq!(output[0].position, CanvasVec2::new(0.0, 0.0));
        assert_eq!(output[1].time_ns, 3);
        assert_eq!(carry, 3);
    }

    #[test]
    fn sample_by_time_interpolates_with_subspan_precision() {
        let mut spans = CommittedCanvasSpanBuffer::new();
        let mut output = Vec::new();
        spans.push_span(CommittedCanvasSpan {
            start: CurveKnot {
                time_ns: 1_000_000_000_000_000_000,
                position: CanvasVec2::new(0.0, 0.0),
                pressure: 1.0,
                tilt: RadianVec2::new(0.0, 0.0),
                twist: 0.0,
                velocity: CanvasVec2::new(1.0, 0.0),
                acceleration: CanvasVec2::new(0.0, 0.0),
                cumulative_s: 0.0,
            },
            end: CurveKnot {
                time_ns: 1_000_000_000_000_000_010,
                position: CanvasVec2::new(10.0, 0.0),
                pressure: 1.0,
                tilt: RadianVec2::new(0.0, 0.0),
                twist: 0.0,
                velocity: CanvasVec2::new(1.0, 0.0),
                acceleration: CanvasVec2::new(0.0, 0.0),
                cumulative_s: 10.0,
            },
        });

        let carry = spans.sample_by_time(1, 0, false, &mut output);

        assert_eq!(
            output.first().expect("first sampled point").time_ns,
            1_000_000_000_000_000_001
        );
        assert_eq!(carry, 0);
    }

    #[test]
    fn span_buffer_samples_by_arclength() {
        let mut smoother = DistanceOrTimeStrokeSmoother::new(5.0, u64::MAX);
        let mut spans = CommittedCanvasSpanBuffer::new();
        let mut output = Vec::new();

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

        let carry = spans.sample_by_arclength(4.0, 0.0, true, &mut output);

        assert_eq!(output[0].position, CanvasVec2::new(0.0, 0.0));
        assert_eq!(output[1].position, CanvasVec2::new(4.0, 0.0));
        assert_eq!(output[2].position, CanvasVec2::new(8.0, 0.0));
        assert!((carry - 2.0).abs() < 1e-5);
    }

    #[test]
    fn span_sample_uses_cubic_hermite_position_and_derivatives() {
        let span = CommittedCanvasSpan {
            start: CurveKnot {
                time_ns: 0,
                position: CanvasVec2::new(0.0, 0.0),
                pressure: 0.2,
                tilt: RadianVec2::new(0.0, 0.0),
                twist: 0.0,
                velocity: CanvasVec2::new(10.0, 0.0),
                acceleration: CanvasVec2::new(0.0, 0.0),
                cumulative_s: 0.0,
            },
            end: CurveKnot {
                time_ns: 1_000_000_000,
                position: CanvasVec2::new(10.0, 10.0),
                pressure: 0.8,
                tilt: RadianVec2::new(0.0, 0.0),
                twist: 1.0,
                velocity: CanvasVec2::new(0.0, 10.0),
                acceleration: CanvasVec2::new(0.0, 0.0),
                cumulative_s: 20.0,
            },
        };

        let sample = span.sample(0.5);

        assert!((sample.position.x - 6.25).abs() < 1e-5);
        assert!((sample.position.y - 3.75).abs() < 1e-5);
        assert!((sample.velocity.x - 12.5).abs() < 1e-5);
        assert!((sample.velocity.y - 12.5).abs() < 1e-5);
        assert!((sample.acceleration.x + 10.0).abs() < 1e-4);
        assert!((sample.acceleration.y - 10.0).abs() < 1e-4);
        assert_eq!(sample.time_ns, 500_000_000);
        assert!((sample.pressure - 0.5).abs() < 1e-5);
    }

    #[test]
    fn span_sample_clamps_runaway_tangents_to_local_chord_scale() {
        let span = CommittedCanvasSpan {
            start: CurveKnot {
                time_ns: 0,
                position: CanvasVec2::new(0.0, 0.0),
                pressure: 1.0,
                tilt: RadianVec2::new(0.0, 0.0),
                twist: 0.0,
                velocity: CanvasVec2::new(10_000.0, 0.0),
                acceleration: CanvasVec2::new(0.0, 0.0),
                cumulative_s: 0.0,
            },
            end: CurveKnot {
                time_ns: 1_000_000,
                position: CanvasVec2::new(1.0, 0.0),
                pressure: 1.0,
                tilt: RadianVec2::new(0.0, 0.0),
                twist: 0.0,
                velocity: CanvasVec2::new(10_000.0, 0.0),
                acceleration: CanvasVec2::new(0.0, 0.0),
                cumulative_s: 1.0,
            },
        };

        let sample = span.sample(0.5);

        assert!(sample.position.x >= 0.0);
        assert!(sample.position.x <= 1.0);
        assert!(sample.position.y.abs() <= 1e-5);
    }

    #[test]
    fn curved_span_arclength_sampling_advances_along_curve() {
        let mut spans = CommittedCanvasSpanBuffer::new();
        let mut output = Vec::new();
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

        let carry = spans.sample_by_arclength(4.0, 0.0, true, &mut output);

        assert_eq!(output.len(), 4);
        assert_eq!(output[0].position, CanvasVec2::new(0.0, 0.0));
        assert!(output[1].position.x > 3.0);
        assert!(output[1].position.y > 0.0);
        assert!(output[2].position.x > output[1].position.x);
        assert!(output[2].position.y > output[1].position.y);
        assert!(output[3].position.y > output[2].position.y);
        assert!(output[3].position.x < 10.0);
        assert!(carry < 4.0);
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

        let mut first_batch = CommittedCanvasSpanBuffer::new();
        first_batch.push_span(make_span(0.0, 6.0, 0.0, 6.0));

        let mut second_batch = CommittedCanvasSpanBuffer::new();
        second_batch.set_global_s_start(6.0);
        second_batch.push_span(make_span(6.0, 12.0, 6.0, 12.0));

        let mut cursor = ArcLengthCursor::new(4.0);
        let mut output = Vec::new();
        first_batch.sample_by_arclength_from(4.0, &mut cursor, &mut output);
        second_batch.sample_by_arclength_from(4.0, &mut cursor, &mut output);

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
        let table = span.build_arclength_table();
        let local_s = 4.9336395;
        let sample = span.sample(table.t_at_cumulative_s(local_s));
        let distance_from_start = distance_between(span.start.position, sample.position);

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
        let mut first_batch = CommittedCanvasSpanBuffer::new();
        let mut next_batch = CommittedCanvasSpanBuffer::new();
        let mut output = Vec::new();
        let mut cursor = ArcLengthCursor::default();

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
        first_batch.sample_by_arclength_from(5.0, &mut cursor, &mut output);

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
        next_batch.sample_by_arclength_from(5.0, &mut cursor, &mut output);

        smoother.finish_stroke();
        smoother
            .pop_committed_spans(&mut next_batch)
            .expect("finish batch");
        next_batch.sample_by_arclength_from(5.0, &mut cursor, &mut output);

        let origin_count = output
            .iter()
            .filter(|sample| sample.position.x.abs() <= 1e-5 && sample.position.y.abs() <= 1e-5)
            .count();
        assert_eq!(origin_count, 1, "{output:?}");
    }

    #[test]
    fn invalid_input_index_tracks_global_input_order_across_drains() {
        let mut smoother = DistanceOrTimeStrokeSmoother::new(0.0, 0);
        let mut spans = CommittedCanvasSpanBuffer::new();

        smoother
            .push_canvas_inputs(&[
                CanvasInput {
                    time_ns: 0,
                    position: CanvasVec2::new(0.0, 0.0),
                    pressure: 0.5,
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
                CanvasInput {
                    time_ns: 20,
                    position: CanvasVec2::new(20.0, 0.0),
                    pressure: 0.5,
                    tilt: RadianVec2::new(0.0, 0.0),
                    twist: 0.0,
                },
            ])
            .expect("push inputs");

        smoother.pop_committed_spans(&mut spans).expect("pop spans");

        let error = smoother
            .push_canvas_input(CanvasInput {
                time_ns: 30,
                position: CanvasVec2::new(f32::NAN, 0.0),
                pressure: 0.5,
                tilt: RadianVec2::new(0.0, 0.0),
                twist: 0.0,
            })
            .expect_err("invalid input should be rejected");

        assert_eq!(
            error,
            StrokeSmootherError::InvalidInputValue {
                input_index: 3,
                value_index: 0,
            }
        );
    }
}
