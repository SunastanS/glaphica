use gla_color::apply_value_mask_to_premultiplied_rgba;
use gla_core::{CanvasCoordF, CanvasInput};

use crate::{ActiveTool, BrushId, BrushSettings, ReplaceCircleStrokeSample, ToolSet};

const MIN_SPACING_RATIO: f32 = 0.05;
const MIN_SPACING_PX: f32 = 1.0;
const SAME_POSITION_EPSILON: f32 = 1e-5;

#[derive(Debug)]
pub(crate) struct BrushWorker {
    tool_set: ToolSet,
    active_tool: ActiveTool,
    brush_settings: BrushSettings,
    active_stroke: Option<ActiveRootStroke>,
}

#[derive(Debug)]
pub(crate) struct ActiveRootStroke {
    brush_id: BrushId,
    brush_settings: BrushSettings,
    inputs: Vec<CanvasInput>,
}

#[derive(Debug)]
pub(crate) struct FinishedRootStroke {
    stroke: ActiveRootStroke,
}

impl BrushWorker {
    pub(crate) fn new(
        tool_set: ToolSet,
        active_tool: ActiveTool,
        brush_settings: BrushSettings,
    ) -> Self {
        Self {
            tool_set,
            active_tool,
            brush_settings,
            active_stroke: None,
        }
    }

    pub(crate) fn active_brush_id(&self) -> Option<BrushId> {
        self.tool_set
            .contains(self.active_tool.as_tool())
            .then_some(self.active_tool.brush_id())?
    }

    pub(crate) fn begin_active_stroke(&mut self) -> bool {
        let Some(brush_id) = self.active_brush_id() else {
            self.active_stroke = None;
            return false;
        };
        self.active_stroke = Some(ActiveRootStroke::new(brush_id, self.brush_settings));
        true
    }

    pub(crate) fn push_canvas_input(&mut self, input: CanvasInput) {
        if let Some(stroke) = self.active_stroke.as_mut() {
            stroke.push_input(input);
        }
    }

    pub(crate) fn finish_active_stroke(&mut self) -> Option<FinishedRootStroke> {
        let stroke = self.active_stroke.take()?;
        (!stroke.is_empty()).then_some(FinishedRootStroke { stroke })
    }

    pub(crate) fn restore_active_stroke(&mut self, stroke: FinishedRootStroke) {
        self.active_stroke = Some(stroke.stroke);
    }

    pub(crate) fn cancel_active_stroke(&mut self) -> bool {
        self.active_stroke.take().is_some()
    }

    pub(crate) fn has_active_stroke(&self) -> bool {
        self.active_stroke.is_some()
    }

    pub(crate) fn active_stroke(&self) -> Option<&ActiveRootStroke> {
        self.active_stroke.as_ref()
    }

    pub(crate) fn update_brush_settings(&mut self, brush_settings: BrushSettings) {
        self.brush_settings = brush_settings;
    }
}

impl ActiveRootStroke {
    fn new(brush_id: BrushId, brush_settings: BrushSettings) -> Self {
        Self {
            brush_id,
            brush_settings,
            inputs: Vec::new(),
        }
    }

    pub(crate) fn brush_id(&self) -> BrushId {
        self.brush_id
    }

    pub(crate) fn inputs(&self) -> &[CanvasInput] {
        &self.inputs
    }

    fn push_input(&mut self, input: CanvasInput) {
        self.inputs.push(input);
    }

    fn is_empty(&self) -> bool {
        self.inputs.is_empty()
    }
}

impl FinishedRootStroke {
    pub(crate) fn replace_circle_samples(&self) -> Vec<ReplaceCircleStrokeSample> {
        self.stroke.replace_circle_samples()
    }

    pub(crate) fn brush_id(&self) -> BrushId {
        self.stroke.brush_id()
    }
}

impl ActiveRootStroke {
    fn replace_circle_samples(&self) -> Vec<ReplaceCircleStrokeSample> {
        sample_canvas_inputs(&self.inputs, self.brush_settings)
            .into_iter()
            .map(|input| ReplaceCircleStrokeSample {
                center: input.position,
                radius_px: self.brush_settings.radius_px,
                color: apply_value_mask_to_premultiplied_rgba(
                    self.brush_settings.color,
                    self.brush_settings.flow * input.pressure.clamp(0.0, 1.0),
                    self.brush_settings.opacity,
                ),
            })
            .collect()
    }
}

fn sample_canvas_inputs(inputs: &[CanvasInput], settings: BrushSettings) -> Vec<CanvasInput> {
    let Some(&first) = inputs.first() else {
        return Vec::new();
    };
    let mut output = vec![first];
    let spacing = dab_spacing_px(settings);
    let mut next_sample_distance = spacing;
    let mut segment_start_distance = 0.0;

    for pair in inputs.windows(2) {
        let start = pair[0];
        let end = pair[1];
        let segment_length = distance_between(start.position, end.position);
        if segment_length <= f32::EPSILON {
            continue;
        }

        let segment_end_distance = segment_start_distance + segment_length;
        while next_sample_distance <= segment_end_distance {
            let t = (next_sample_distance - segment_start_distance) / segment_length;
            output.push(interpolate_input(start, end, t));
            next_sample_distance += spacing;
        }
        segment_start_distance = segment_end_distance;
    }

    if let Some(&last) = inputs.last()
        && output
            .last()
            .is_none_or(|sample| !same_position(sample.position, last.position))
    {
        output.push(last);
    }
    output
}

fn dab_spacing_px(settings: BrushSettings) -> f32 {
    settings.radius_px.max(MIN_SPACING_PX) * sanitized_spacing_ratio(settings.spacing_ratio)
}

fn sanitized_spacing_ratio(spacing_ratio: f32) -> f32 {
    if spacing_ratio.is_finite() {
        spacing_ratio.max(MIN_SPACING_RATIO)
    } else {
        MIN_SPACING_RATIO
    }
}

fn distance_between(lhs: CanvasCoordF, rhs: CanvasCoordF) -> f32 {
    (lhs.x - rhs.x).hypot(lhs.y - rhs.y)
}

fn same_position(lhs: CanvasCoordF, rhs: CanvasCoordF) -> bool {
    (lhs.x - rhs.x).abs() <= SAME_POSITION_EPSILON && (lhs.y - rhs.y).abs() <= SAME_POSITION_EPSILON
}

fn interpolate_input(start: CanvasInput, end: CanvasInput, t: f32) -> CanvasInput {
    let t = t.clamp(0.0, 1.0);
    CanvasInput {
        time_ns: lerp_u64(start.time_ns, end.time_ns, t),
        position: CanvasCoordF::new(
            lerp_f32(start.position.x, end.position.x, t),
            lerp_f32(start.position.y, end.position.y, t),
        ),
        pressure: lerp_f32(start.pressure, end.pressure, t),
        tilt: (
            lerp_f32(start.tilt.0, end.tilt.0, t),
            lerp_f32(start.tilt.1, end.tilt.1, t),
        ),
        twist: lerp_f32(start.twist, end.twist, t),
    }
}

fn lerp_f32(start: f32, end: f32, t: f32) -> f32 {
    start * (1.0 - t) + end * t
}

fn lerp_u64(start: u64, end: u64, t: f32) -> u64 {
    (start as f64 * (1.0 - t as f64) + end as f64 * t as f64).round() as u64
}

#[cfg(test)]
mod tests {
    use super::{ActiveRootStroke, BrushWorker};
    use crate::{ActiveTool, BrushId, BrushSettings, ToolSet};
    use gla_core::{CanvasCoordF, CanvasInput};

    fn canvas_input(time_ns: u64, x: f32, y: f32, pressure: f32) -> CanvasInput {
        CanvasInput {
            time_ns,
            position: CanvasCoordF::new(x, y),
            pressure,
            tilt: (0.0, 0.0),
            twist: 0.0,
        }
    }

    #[test]
    fn replace_circle_samples_are_inserted_at_brush_spacing() {
        let mut settings = BrushSettings::default();
        settings.radius_px = 10.0;
        settings.spacing_ratio = 1.0;
        let mut stroke = ActiveRootStroke::new(BrushId::DEFAULT, settings);

        stroke.push_input(canvas_input(0, 0.0, 0.0, 1.0));
        stroke.push_input(canvas_input(30, 30.0, 0.0, 1.0));

        let samples = stroke.replace_circle_samples();
        let centers = samples
            .iter()
            .map(|sample| sample.center)
            .collect::<Vec<_>>();
        assert_eq!(
            centers,
            vec![
                CanvasCoordF::new(0.0, 0.0),
                CanvasCoordF::new(10.0, 0.0),
                CanvasCoordF::new(20.0, 0.0),
                CanvasCoordF::new(30.0, 0.0),
            ]
        );
    }

    #[test]
    fn replace_circle_samples_keep_short_stroke_endpoint() {
        let mut settings = BrushSettings::default();
        settings.radius_px = 10.0;
        settings.spacing_ratio = 1.0;
        let mut stroke = ActiveRootStroke::new(BrushId::DEFAULT, settings);

        stroke.push_input(canvas_input(0, 0.0, 0.0, 1.0));
        stroke.push_input(canvas_input(3, 3.0, 0.0, 1.0));

        let samples = stroke.replace_circle_samples();
        assert_eq!(samples.len(), 2);
        assert_eq!(samples[0].center, CanvasCoordF::new(0.0, 0.0));
        assert_eq!(samples[1].center, CanvasCoordF::new(3.0, 0.0));
    }

    #[test]
    fn replace_circle_samples_interpolate_pressure_for_flow() {
        let mut settings = BrushSettings::default();
        settings.radius_px = 10.0;
        settings.spacing_ratio = 0.5;
        settings.flow = 0.5;
        settings.opacity = 0.8;
        settings.color = gla_color::PremultipliedRgbaF32::new(1.0, 0.5, 0.25, 1.0);
        let mut stroke = ActiveRootStroke::new(BrushId::DEFAULT, settings);

        stroke.push_input(canvas_input(0, 0.0, 0.0, 1.0));
        stroke.push_input(canvas_input(10, 10.0, 0.0, 0.5));

        let samples = stroke.replace_circle_samples();
        assert_eq!(samples.len(), 3);
        assert!(samples.iter().all(|sample| sample.radius_px == 10.0));
        assert_eq!(
            samples[0].color,
            gla_color::PremultipliedRgbaF32::new(0.4, 0.2, 0.1, 0.4)
        );
        assert_eq!(
            samples[1].color,
            gla_color::PremultipliedRgbaF32::new(0.3, 0.15, 0.075, 0.3)
        );
        assert_eq!(
            samples[2].color,
            gla_color::PremultipliedRgbaF32::new(0.2, 0.1, 0.05, 0.2)
        );
    }

    #[test]
    fn brush_worker_finishes_active_stroke_into_samples() {
        let mut settings = BrushSettings::default();
        settings.radius_px = 10.0;
        settings.spacing_ratio = 1.0;
        let mut worker = BrushWorker::new(
            ToolSet::default_brush(),
            ActiveTool::Brush(BrushId::DEFAULT),
            settings,
        );

        assert!(worker.begin_active_stroke());
        worker.push_canvas_input(canvas_input(0, 0.0, 0.0, 1.0));
        worker.push_canvas_input(canvas_input(30, 30.0, 0.0, 1.0));
        let finished = worker.finish_active_stroke().unwrap();

        assert_eq!(finished.brush_id(), BrushId::DEFAULT);
        assert!(!worker.has_active_stroke());
        assert_eq!(finished.replace_circle_samples().len(), 4);
    }

    #[test]
    fn brush_worker_can_restore_uncommitted_finished_stroke() {
        let mut worker = BrushWorker::new(
            ToolSet::default_brush(),
            ActiveTool::Brush(BrushId::DEFAULT),
            BrushSettings::default(),
        );

        assert!(worker.begin_active_stroke());
        worker.push_canvas_input(canvas_input(0, 1.0, 2.0, 1.0));
        let finished = worker.finish_active_stroke().unwrap();
        worker.restore_active_stroke(finished);

        let active = worker.active_stroke().unwrap();
        assert_eq!(active.inputs().len(), 1);
        assert_eq!(active.inputs()[0].position, CanvasCoordF::new(1.0, 2.0));
    }

    #[test]
    fn brush_worker_rejects_unregistered_active_brush() {
        let mut worker = BrushWorker::new(
            ToolSet::default_brush(),
            ActiveTool::Brush(BrushId::new(99)),
            BrushSettings::default(),
        );

        assert_eq!(worker.active_brush_id(), None);
        assert!(!worker.begin_active_stroke());
        assert!(!worker.has_active_stroke());
    }

    #[test]
    fn brush_worker_setting_updates_apply_to_next_stroke() {
        let mut worker = BrushWorker::new(
            ToolSet::default_brush(),
            ActiveTool::Brush(BrushId::DEFAULT),
            BrushSettings::default(),
        );
        let mut settings = BrushSettings::default();
        settings.radius_px = 3.0;
        worker.update_brush_settings(settings);

        assert!(worker.begin_active_stroke());
        worker.push_canvas_input(canvas_input(0, 0.0, 0.0, 1.0));
        let samples = worker
            .finish_active_stroke()
            .unwrap()
            .replace_circle_samples();

        assert_eq!(samples[0].radius_px, 3.0);
    }
}
