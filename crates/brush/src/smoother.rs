use std::collections::VecDeque;
use std::fmt::{Display, Formatter};
use std::sync::OnceLock;
use std::time::Duration;

use crate::sampler::span_arclength;
use glaphica_core::{CanvasInput, CanvasVec2, RadianVec2};

#[derive(Debug, Clone, Copy)]
struct BrushPerfTraceConfig {
    stderr_enabled: bool,
    slow_threshold: Duration,
    far_threshold: f32,
}

impl BrushPerfTraceConfig {
    fn global() -> &'static Self {
        static CONFIG: OnceLock<BrushPerfTraceConfig> = OnceLock::new();
        CONFIG.get_or_init(Self::from_env)
    }

    fn from_env() -> Self {
        Self {
            stderr_enabled: env_flag("GLAPHICA_BRUSH_PERF_TRACE_STDERR"),
            slow_threshold: env_millis("GLAPHICA_BRUSH_PERF_TRACE_SLOW_MS")
                .map(Duration::from_millis)
                .unwrap_or(Duration::from_millis(8)),
            far_threshold: env_f32("GLAPHICA_BRUSH_PERF_TRACE_FAR_PX").unwrap_or(8.0),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct BrushLatencyPoint {
    time_ns: u64,
    position: CanvasVec2,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct BrushLatencySnapshot {
    pub time_ns: u64,
    pub distance: f32,
    pub input: BrushLatencyPoint,
    pub draw: BrushLatencyPoint,
}

#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub(crate) struct BrushLatencyTraceState {
    latest_input: Option<BrushLatencyPoint>,
    latest_draw: Option<BrushLatencyPoint>,
    drain_seq: u64,
}

impl BrushLatencyTraceState {
    pub(crate) fn clear(&mut self) {
        self.latest_input = None;
        self.latest_draw = None;
        self.drain_seq = 0;
    }

    pub(crate) fn record_input(&mut self, input: CanvasInput) {
        self.latest_input = Some(BrushLatencyPoint {
            time_ns: input.time_ns,
            position: input.position,
        });
    }

    pub(crate) fn record_current_draw(&mut self, sample: CommittedCanvasSample) {
        self.latest_draw = Some(BrushLatencyPoint {
            time_ns: sample.time_ns,
            position: sample.position,
        });
    }

    pub(crate) fn snapshot(&self) -> Option<BrushLatencySnapshot> {
        let input = self.latest_input?;
        let draw = self.latest_draw?;
        Some(BrushLatencySnapshot {
            time_ns: input.time_ns.saturating_sub(draw.time_ns),
            distance: distance_between(input.position, draw.position),
            input,
            draw,
        })
    }

    pub(crate) fn trace_drain(&mut self, committed_spans: usize, emitted_dabs: usize) {
        let Some(snapshot) = self.snapshot() else {
            return;
        };
        let config = BrushPerfTraceConfig::global();
        let slow = Duration::from_nanos(snapshot.time_ns) >= config.slow_threshold;
        let far = snapshot.distance >= config.far_threshold;
        if !config.stderr_enabled || !(slow || far) {
            return;
        }
        self.drain_seq += 1;
        eprintln!(
            "[PERF][brush][drain={}] latency_ms={:.3} latency_px={:.3} emitted_dabs={} committed_spans={} current_draw=({:.3},{:.3}) cursor=({:.3},{:.3})",
            self.drain_seq,
            duration_ms(Duration::from_nanos(snapshot.time_ns)),
            snapshot.distance,
            emitted_dabs,
            committed_spans,
            snapshot.draw.position.x,
            snapshot.draw.position.y,
            snapshot.input.position.x,
            snapshot.input.position.y,
        );
    }
}

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

    fn current_drawing_sample(&self) -> Option<CommittedCanvasSample>;

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
    emitted_initial_knot: bool,
    emitted_arclength: f32,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct CommittedCanvasSpanBuffer {
    knots: Vec<CurveKnot>,
    global_s_start: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct HermiteTangents {
    pub(crate) start_delta: CanvasVec2,
    pub(crate) end_delta: CanvasVec2,
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
}

impl CommittedCanvasSpanBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clear(&mut self) {
        self.knots.clear();
        self.global_s_start = 0.0;
    }

    pub fn knot_count(&self) -> usize {
        self.knots.len()
    }

    pub fn span_count(&self) -> usize {
        self.knots.len().saturating_sub(1)
    }

    pub fn is_empty(&self) -> bool {
        self.knots.is_empty()
    }

    pub fn global_s_start(&self) -> f32 {
        self.global_s_start
    }

    pub fn knots(&self) -> &[CurveKnot] {
        &self.knots
    }

    pub fn push_knot(&mut self, knot: CurveKnot) {
        self.knots.push(knot);
    }

    pub fn span_at(&self, index: usize) -> Option<CommittedCanvasSpan> {
        Some(CommittedCanvasSpan {
            start: *self.knots.get(index)?,
            end: *self.knots.get(index + 1)?,
        })
    }

    pub fn spans_iter(&self) -> impl Iterator<Item = CommittedCanvasSpan> + '_ {
        self.knots.windows(2).map(|w| CommittedCanvasSpan {
            start: w[0],
            end: w[1],
        })
    }

    pub fn collect_spans(&self) -> Vec<CommittedCanvasSpan> {
        self.spans_iter().collect()
    }

    pub(crate) fn set_global_s_start(&mut self, global_s_start: f32) {
        self.global_s_start = global_s_start.max(0.0);
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
        puffin::profile_scope!("stroke_smoother_finish_stroke");
        self.finished = true;
        self.recompute_mutable_tail();
        self.advance_stable_end();
    }

    fn current_drawing_sample(&self) -> Option<CommittedCanvasSample> {
        if self.knots.is_empty() {
            return None;
        }
        if self.finished {
            return self
                .knots
                .back()
                .copied()
                .map(committed_sample_from_curve_knot);
        }
        let index = if self.stable_end == 0 {
            0
        } else {
            self.stable_end.saturating_sub(1)
        };
        self.knots
            .get(index)
            .copied()
            .map(committed_sample_from_curve_knot)
    }

    fn push_input(&mut self, input: CanvasInput) -> Result<(), StrokeSmootherError> {
        puffin::profile_scope!("stroke_smoother_push_input");
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
        puffin::profile_scope!("stroke_smoother_pop_stable_spans");
        output.clear();
        output.set_global_s_start(self.emitted_arclength);
        if self.knots.is_empty() {
            return 0;
        }

        let mut count = 0;

        if !self.initial_point_emitted && self.stable_end.saturating_sub(self.emitted_prefix) < 2 {
            if let Some(first) = self.knots.front().copied() {
                output.push_knot(first);
                self.initial_point_emitted = true;
                return 1;
            }
        }

        let knot_start = self.emitted_prefix.min(self.stable_end);

        if self.stable_end < knot_start + 2 {
            return 0;
        }

        for index in knot_start..self.stable_end {
            output.push_knot(self.knots[index]);
            if index > knot_start {
                let span = CommittedCanvasSpan {
                    start: self.knots[index - 1],
                    end: self.knots[index],
                };
                self.emitted_arclength += span_arclength(&span);
            }
            count += 1;
        }

        if self.stable_end > 0 {
            self.emitted_prefix = self.stable_end.saturating_sub(1);
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
        puffin::profile_scope!("stroke_smoother_advance_stable_end");
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
        puffin::profile_scope!("stroke_smoother_recompute_mutable_tail");
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
            emitted_initial_knot: false,
            emitted_arclength: 0.0,
        }
    }
}

impl StrokeSmoother for PassthroughStrokeSmoother {
    fn clear(&mut self) {
        self.knots.clear();
        self.next_input_index = 0;
        self.emitted_prefix = 0;
        self.emitted_initial_knot = false;
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

    fn current_drawing_sample(&self) -> Option<CommittedCanvasSample> {
        self.knots
            .back()
            .copied()
            .map(committed_sample_from_curve_knot)
    }

    fn pop_committed_spans(
        &mut self,
        output: &mut CommittedCanvasSpanBuffer,
    ) -> Result<usize, StrokeSmootherError> {
        output.clear();
        output.set_global_s_start(self.emitted_arclength);
        if self.knots.is_empty() {
            return Ok(0);
        }

        let end_index = self.knots.len();

        if !self.emitted_initial_knot && end_index < 2 {
            output.push_knot(self.knots[0]);
            self.emitted_initial_knot = true;
            return Ok(1);
        }

        let knot_start = self.emitted_prefix.min(end_index.saturating_sub(1));

        if end_index < knot_start + 2 {
            return Ok(0);
        }

        let mut count = 0;
        for index in knot_start..end_index {
            output.push_knot(self.knots[index]);
            if index > knot_start {
                let span = CommittedCanvasSpan {
                    start: self.knots[index - 1],
                    end: self.knots[index],
                };
                self.emitted_arclength += span_arclength(&span);
            }
            count += 1;
        }

        if count > 0 {
            self.emitted_initial_knot = true;
        }

        if end_index > 0 {
            self.emitted_prefix = end_index.saturating_sub(1);
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

    fn current_drawing_sample(&self) -> Option<CommittedCanvasSample> {
        self.curve.current_drawing_sample()
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

fn committed_sample_from_curve_knot(knot: CurveKnot) -> CommittedCanvasSample {
    CommittedCanvasSample {
        time_ns: knot.time_ns,
        position: knot.position,
        pressure: knot.pressure,
        tilt: knot.tilt,
        twist: knot.twist,
        velocity: knot.velocity,
        acceleration: knot.acceleration,
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

pub(crate) fn distance_between(lhs: CanvasVec2, rhs: CanvasVec2) -> f32 {
    let delta = subtract_canvas_vec2(lhs, rhs);
    (delta.x * delta.x + delta.y * delta.y).sqrt()
}

fn env_flag(name: &str) -> bool {
    std::env::var(name)
        .ok()
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(false)
}

fn env_millis(name: &str) -> Option<u64> {
    std::env::var(name).ok()?.parse::<u64>().ok()
}

fn env_f32(name: &str) -> Option<f32> {
    std::env::var(name).ok()?.parse::<f32>().ok()
}

fn duration_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

fn lerp_u64(start: u64, end: u64, t: f32) -> u64 {
    if end <= start {
        return start;
    }
    let delta = end - start;
    start.saturating_add((delta as f64 * f64::from(t.clamp(0.0, 1.0))).round() as u64)
}

pub(crate) fn subtract_canvas_vec2(lhs: CanvasVec2, rhs: CanvasVec2) -> CanvasVec2 {
    CanvasVec2::new(lhs.x - rhs.x, lhs.y - rhs.y)
}

pub(crate) fn span_hermite_tangents(
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

pub(crate) fn add_canvas_vec2(lhs: CanvasVec2, rhs: CanvasVec2) -> CanvasVec2 {
    CanvasVec2::new(lhs.x + rhs.x, lhs.y + rhs.y)
}

pub(crate) fn scale_canvas_vec2(value: CanvasVec2, scale: f32) -> CanvasVec2 {
    CanvasVec2::new(value.x * scale, value.y * scale)
}

pub(crate) fn vector_length(value: CanvasVec2) -> f32 {
    (value.x * value.x + value.y * value.y).sqrt()
}

pub(crate) fn clamp_canvas_vec2_length(value: CanvasVec2, max_length: f32) -> CanvasVec2 {
    let length = vector_length(value);
    if length <= max_length || length <= f32::EPSILON {
        return value;
    }
    scale_canvas_vec2(value, max_length / length)
}

#[cfg(test)]
mod tests {
    use super::{
        CommittedCanvasSpan, CommittedCanvasSpanBuffer, CurveKnot, DistanceOrTimeStrokeSmoother,
        PassthroughStrokeSmoother, StrokeSmoother, StrokeSmootherError, distance_between,
    };
    use glaphica_core::{CanvasInput, CanvasVec2, RadianVec2};

    #[test]
    fn passthrough_current_drawing_sample_tracks_latest_input() {
        let mut smoother = PassthroughStrokeSmoother::default();

        smoother
            .push_canvas_inputs(&[
                CanvasInput {
                    time_ns: 10,
                    position: CanvasVec2::new(1.0, 2.0),
                    pressure: 0.2,
                    tilt: RadianVec2::new(0.0, 0.0),
                    twist: 0.0,
                },
                CanvasInput {
                    time_ns: 20,
                    position: CanvasVec2::new(5.0, 8.0),
                    pressure: 0.7,
                    tilt: RadianVec2::new(0.1, 0.2),
                    twist: 0.3,
                },
            ])
            .expect("push inputs");

        let sample = smoother
            .current_drawing_sample()
            .expect("current drawing sample");
        assert_eq!(sample.time_ns, 20);
        assert_eq!(sample.position, CanvasVec2::new(5.0, 8.0));
        assert_eq!(sample.pressure, 0.7);
    }

    #[test]
    fn distance_smoother_current_drawing_sample_tracks_stable_point() {
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

        let sample = smoother
            .current_drawing_sample()
            .expect("current drawing sample");
        assert_eq!(sample.time_ns, 0);
        assert_eq!(sample.position, CanvasVec2::new(0.0, 0.0));

        smoother.finish_stroke();

        let sample = smoother
            .current_drawing_sample()
            .expect("finished drawing sample");
        smoother.pop_committed_spans(&mut spans).expect("pop spans");
        let last_span = spans
            .span_at(spans.span_count().saturating_sub(1))
            .expect("last span");
        assert_eq!(sample.time_ns, last_span.end.time_ns);
        assert_eq!(sample.position, last_span.end.position);
    }

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
        assert_eq!(spans.knot_count(), 1);
        assert_eq!(spans.knots()[0].position, CanvasVec2::new(0.0, 0.0));
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
            .knots()
            .last()
            .expect("at least one committed knot")
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
        assert_eq!(spans.knot_count(), 1);
        assert_eq!(spans.knots()[0].position, CanvasVec2::new(4.0, 5.0));
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

        assert_eq!(count, 2);
        let first_span = spans.span_at(0).expect("first span");
        assert_eq!(first_span.start.position, CanvasVec2::new(0.0, 0.0));
        assert_eq!(first_span.end.position, CanvasVec2::new(1.0, 0.0));
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
            .span_at(spans.span_count().saturating_sub(1))
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

        assert_eq!(count, 2);
        let first_span = spans.span_at(0).expect("first span");
        assert_ne!(first_span.start.time_ns, first_span.end.time_ns);
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

    #[test]
    fn smooth_pop_does_not_emit_boundary_knot_without_new_spans() {
        let mut smoother = DistanceOrTimeStrokeSmoother::new(5.0, u64::MAX);
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
                    position: CanvasVec2::new(2.0, 0.0),
                    pressure: 0.5,
                    tilt: RadianVec2::new(0.0, 0.0),
                    twist: 0.0,
                },
                CanvasInput {
                    time_ns: 20,
                    position: CanvasVec2::new(8.0, 0.0),
                    pressure: 0.5,
                    tilt: RadianVec2::new(0.0, 0.0),
                    twist: 0.0,
                },
            ])
            .expect("push inputs");

        let count = smoother.pop_committed_spans(&mut spans).expect("first pop");
        assert_eq!(count, 1);
        assert_eq!(spans.knot_count(), 1);

        let count = smoother.pop_committed_spans(&mut spans).expect("second pop");
        assert_eq!(count, 0);
        assert!(spans.is_empty());
    }

    #[test]
    fn passthrough_pop_does_not_re_emit_initial_knot() {
        let mut smoother = PassthroughStrokeSmoother::default();
        let mut spans = CommittedCanvasSpanBuffer::new();

        smoother
            .push_canvas_input(CanvasInput {
                time_ns: 0,
                position: CanvasVec2::new(0.0, 0.0),
                pressure: 0.5,
                tilt: RadianVec2::new(0.0, 0.0),
                twist: 0.0,
            })
            .expect("push input");

        let count = smoother.pop_committed_spans(&mut spans).expect("first pop");
        assert_eq!(count, 1);
        assert_eq!(spans.knot_count(), 1);

        let count = smoother.pop_committed_spans(&mut spans).expect("second pop");
        assert_eq!(count, 0);
        assert!(spans.is_empty());
    }

    #[test]
    fn passthrough_first_pop_with_two_knots_marks_initial_as_emitted() {
        let mut smoother = PassthroughStrokeSmoother::default();
        let mut spans = CommittedCanvasSpanBuffer::new();

        smoother
            .push_canvas_input(CanvasInput {
                time_ns: 0,
                position: CanvasVec2::new(0.0, 0.0),
                pressure: 0.5,
                tilt: RadianVec2::new(0.0, 0.0),
                twist: 0.0,
            })
            .expect("push first");
        smoother
            .push_canvas_input(CanvasInput {
                time_ns: 1,
                position: CanvasVec2::new(1.0, 0.0),
                pressure: 0.5,
                tilt: RadianVec2::new(0.0, 0.0),
                twist: 0.0,
            })
            .expect("push second");

        let count = smoother.pop_committed_spans(&mut spans).expect("first pop");
        assert_eq!(count, 2);
        assert_eq!(spans.span_count(), 1);

        let count = smoother.pop_committed_spans(&mut spans).expect("second pop");
        assert_eq!(count, 0);
        assert!(spans.is_empty());
    }
}
