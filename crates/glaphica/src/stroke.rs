use gla_color::apply_value_mask_to_premultiplied_rgba;
use gla_core::{CanvasCoordF, CanvasInput};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, TrySendError, sync_channel};
use std::thread::{self, JoinHandle};

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

pub(crate) struct BrushThreadRuntime {
    tool_set: ToolSet,
    active_tool: ActiveTool,
    command_sender: Option<SyncSender<BrushThreadCommand>>,
    active: bool,
    command_epoch: u64,
    thread: Option<JoinHandle<()>>,
}

#[derive(Debug)]
enum BrushThreadCommand {
    Begin {
        epoch: u64,
    },
    CanvasInput {
        epoch: u64,
        input: CanvasInput,
    },
    Finish {
        epoch: u64,
        response: SyncSender<Option<FinishedRootStroke>>,
    },
    Restore {
        epoch: u64,
        stroke: FinishedRootStroke,
    },
    Cancel {
        epoch: u64,
    },
    SetActiveTool(ActiveTool),
    UpdateBrushSettings(BrushSettings),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkerStrokeState {
    Idle,
    Active { epoch: u64 },
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

impl BrushThreadRuntime {
    pub(crate) fn spawn(
        tool_set: ToolSet,
        active_tool: ActiveTool,
        brush_settings: BrushSettings,
        command_capacity: usize,
    ) -> Self {
        let capacity = command_capacity.max(1);
        let (command_sender, command_receiver) = sync_channel(capacity);
        let worker = BrushWorker::new(tool_set.clone(), active_tool, brush_settings);
        let thread = thread::Builder::new()
            .name("glaphica-brush".to_owned())
            .spawn(move || run_brush_thread(worker, command_receiver, capacity))
            .expect("brush thread should spawn");
        Self {
            tool_set,
            active_tool,
            command_sender: Some(command_sender),
            active: false,
            command_epoch: 0,
            thread: Some(thread),
        }
    }

    pub(crate) fn active_brush_id(&self) -> Option<BrushId> {
        self.tool_set
            .contains(self.active_tool.as_tool())
            .then_some(self.active_tool.brush_id())?
    }

    pub(crate) fn set_active_tool(&mut self, active_tool: ActiveTool) -> bool {
        if !self.tool_set.contains(active_tool.as_tool()) {
            return false;
        }
        self.next_epoch();
        let sent = self.send_control(BrushThreadCommand::SetActiveTool(active_tool));
        if sent {
            self.active_tool = active_tool;
            self.active = false;
        }
        sent
    }

    pub(crate) fn begin_active_stroke(&mut self) -> bool {
        if self.active_brush_id().is_none() {
            self.active = false;
            return false;
        }
        let epoch = self.next_epoch();
        let sent = self.send_control(BrushThreadCommand::Begin { epoch });
        self.active = sent;
        sent
    }

    pub(crate) fn push_canvas_input(&mut self, input: CanvasInput) {
        if !self.active {
            return;
        }
        let Some(sender) = self.command_sender.as_ref() else {
            self.active = false;
            return;
        };
        let command = BrushThreadCommand::CanvasInput {
            epoch: self.command_epoch,
            input,
        };
        match sender.try_send(command) {
            Ok(()) | Err(TrySendError::Full(BrushThreadCommand::CanvasInput { .. })) => {}
            Err(TrySendError::Full(command)) => {
                if sender.send(command).is_err() {
                    self.active = false;
                }
            }
            Err(TrySendError::Disconnected(_)) => {
                self.active = false;
            }
        }
    }

    pub(crate) fn finish_active_stroke(&mut self) -> Option<FinishedRootStroke> {
        if !self.active {
            return None;
        }
        let Some(sender) = self.command_sender.as_ref() else {
            self.active = false;
            return None;
        };
        let (response_sender, response_receiver) = sync_channel(0);
        if sender
            .send(BrushThreadCommand::Finish {
                epoch: self.command_epoch,
                response: response_sender,
            })
            .is_err()
        {
            self.active = false;
            return None;
        }
        self.active = false;
        response_receiver.recv().ok().flatten()
    }

    pub(crate) fn restore_active_stroke(&mut self, stroke: FinishedRootStroke) {
        let epoch = self.next_epoch();
        self.active = self.send_control(BrushThreadCommand::Restore { epoch, stroke });
    }

    pub(crate) fn cancel_active_stroke(&mut self) -> bool {
        if !self.active {
            return false;
        }
        self.active = false;
        let epoch = self.next_epoch();
        self.send_control(BrushThreadCommand::Cancel { epoch })
    }

    pub(crate) fn has_active_stroke(&self) -> bool {
        self.active
    }

    pub(crate) fn update_brush_settings(&mut self, brush_settings: BrushSettings) {
        self.next_epoch();
        self.active = false;
        let _ = self.send_control(BrushThreadCommand::UpdateBrushSettings(brush_settings));
    }

    fn next_epoch(&mut self) -> u64 {
        self.command_epoch = self.command_epoch.saturating_add(1);
        self.command_epoch
    }

    fn send_control(&self, command: BrushThreadCommand) -> bool {
        let Some(sender) = self.command_sender.as_ref() else {
            return false;
        };
        sender.send(command).is_ok()
    }
}

impl Drop for BrushThreadRuntime {
    fn drop(&mut self) {
        drop(self.command_sender.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
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

    pub(crate) fn set_active_tool(&mut self, active_tool: ActiveTool) -> bool {
        if !self.tool_set.contains(active_tool.as_tool()) {
            return false;
        }
        self.active_tool = active_tool;
        self.active_stroke = None;
        true
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

    pub(crate) fn push_canvas_inputs(&mut self, inputs: &[CanvasInput]) {
        if let Some(stroke) = self.active_stroke.as_mut() {
            stroke.push_inputs(inputs);
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

    fn push_inputs(&mut self, inputs: &[CanvasInput]) {
        self.inputs.extend_from_slice(inputs);
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

    pub(crate) fn inputs(&self) -> &[CanvasInput] {
        self.stroke.inputs()
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

impl WorkerStrokeState {
    fn accepts(self, epoch: u64) -> bool {
        matches!(self, Self::Active { epoch: active_epoch } if active_epoch == epoch)
    }
}

fn run_brush_thread(
    mut worker: BrushWorker,
    command_receiver: Receiver<BrushThreadCommand>,
    batch_capacity: usize,
) {
    let batch_capacity = batch_capacity.max(1);
    let mut command_batch = Vec::with_capacity(batch_capacity);
    let mut canvas_batch = Vec::with_capacity(batch_capacity);
    let mut stroke_state = WorkerStrokeState::Idle;

    while receive_command_batch(&command_receiver, &mut command_batch, batch_capacity) {
        for command in command_batch.drain(..) {
            match command {
                BrushThreadCommand::Begin { epoch } => {
                    canvas_batch.clear();
                    worker.begin_active_stroke();
                    stroke_state = WorkerStrokeState::Active { epoch };
                }
                BrushThreadCommand::CanvasInput { epoch, input } => {
                    if stroke_state.accepts(epoch) {
                        canvas_batch.push(input);
                    }
                }
                BrushThreadCommand::Finish { epoch, response } => {
                    let finished = if stroke_state.accepts(epoch) {
                        flush_canvas_batch(&mut worker, &mut canvas_batch);
                        worker.finish_active_stroke()
                    } else {
                        None
                    };
                    stroke_state = WorkerStrokeState::Idle;
                    let _ = response.send(finished);
                }
                BrushThreadCommand::Restore { epoch, stroke } => {
                    canvas_batch.clear();
                    worker.restore_active_stroke(stroke);
                    stroke_state = WorkerStrokeState::Active { epoch };
                }
                BrushThreadCommand::Cancel { epoch: _ } => {
                    canvas_batch.clear();
                    worker.cancel_active_stroke();
                    stroke_state = WorkerStrokeState::Idle;
                }
                BrushThreadCommand::SetActiveTool(active_tool) => {
                    canvas_batch.clear();
                    worker.set_active_tool(active_tool);
                    stroke_state = WorkerStrokeState::Idle;
                }
                BrushThreadCommand::UpdateBrushSettings(settings) => {
                    canvas_batch.clear();
                    worker.cancel_active_stroke();
                    worker.update_brush_settings(settings);
                    stroke_state = WorkerStrokeState::Idle;
                }
            }
        }
        flush_canvas_batch(&mut worker, &mut canvas_batch);
    }
}

fn receive_command_batch(
    command_receiver: &Receiver<BrushThreadCommand>,
    output: &mut Vec<BrushThreadCommand>,
    max_items: usize,
) -> bool {
    output.clear();
    let Ok(first) = command_receiver.recv() else {
        return false;
    };
    output.push(first);
    for _ in 1..max_items {
        match command_receiver.try_recv() {
            Ok(command) => output.push(command),
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => break,
        }
    }
    true
}

fn flush_canvas_batch(worker: &mut BrushWorker, canvas_batch: &mut Vec<CanvasInput>) {
    if canvas_batch.is_empty() {
        return;
    }
    worker.push_canvas_inputs(canvas_batch);
    canvas_batch.clear();
}

#[cfg(test)]
mod tests {
    use super::{ActiveRootStroke, BrushThreadCommand, BrushThreadRuntime, BrushWorker};
    use crate::{ActiveTool, BrushId, BrushSettings, Tool, ToolSet};
    use gla_core::{CanvasCoordF, CanvasInput};
    use std::sync::mpsc::sync_channel;

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
    fn brush_worker_switches_registered_active_tool() {
        let second_brush = BrushId::new(2);
        let mut worker = BrushWorker::new(
            ToolSet::new(vec![
                Tool::Brush(BrushId::DEFAULT),
                Tool::Brush(second_brush),
            ]),
            ActiveTool::Brush(BrushId::DEFAULT),
            BrushSettings::default(),
        );

        assert!(worker.begin_active_stroke());
        worker.push_canvas_input(canvas_input(0, 0.0, 0.0, 1.0));
        assert!(worker.set_active_tool(ActiveTool::Brush(second_brush)));

        assert_eq!(worker.active_brush_id(), Some(second_brush));
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

    #[test]
    fn brush_thread_runtime_finishes_inputs_in_background() {
        let mut runtime = BrushThreadRuntime::spawn(
            ToolSet::default_brush(),
            ActiveTool::Brush(BrushId::DEFAULT),
            BrushSettings::default(),
            8,
        );

        assert!(runtime.begin_active_stroke());
        runtime.push_canvas_input(canvas_input(0, 1.0, 2.0, 1.0));
        runtime.push_canvas_input(canvas_input(1, 3.0, 4.0, 1.0));
        let finished = runtime.finish_active_stroke().unwrap();

        assert_eq!(finished.brush_id(), BrushId::DEFAULT);
        assert_eq!(finished.inputs().len(), 2);
        assert_eq!(finished.inputs()[0].position, CanvasCoordF::new(1.0, 2.0));
        assert_eq!(finished.inputs()[1].position, CanvasCoordF::new(3.0, 4.0));
        assert!(!runtime.has_active_stroke());
    }

    #[test]
    fn brush_thread_batch_clears_queued_inputs_on_cancel() {
        let (command_sender, command_receiver) = sync_channel(16);
        let (response_sender, response_receiver) = sync_channel(1);
        command_sender
            .send(BrushThreadCommand::Begin { epoch: 1 })
            .unwrap();
        command_sender
            .send(BrushThreadCommand::CanvasInput {
                epoch: 1,
                input: canvas_input(1, 1.0, 1.0, 1.0),
            })
            .unwrap();
        command_sender
            .send(BrushThreadCommand::Cancel { epoch: 2 })
            .unwrap();
        command_sender
            .send(BrushThreadCommand::CanvasInput {
                epoch: 1,
                input: canvas_input(2, 2.0, 2.0, 1.0),
            })
            .unwrap();
        command_sender
            .send(BrushThreadCommand::Begin { epoch: 3 })
            .unwrap();
        command_sender
            .send(BrushThreadCommand::CanvasInput {
                epoch: 3,
                input: canvas_input(3, 3.0, 3.0, 1.0),
            })
            .unwrap();
        command_sender
            .send(BrushThreadCommand::Finish {
                epoch: 3,
                response: response_sender,
            })
            .unwrap();
        drop(command_sender);

        super::run_brush_thread(
            BrushWorker::new(
                ToolSet::default_brush(),
                ActiveTool::Brush(BrushId::DEFAULT),
                BrushSettings::default(),
            ),
            command_receiver,
            16,
        );
        let finished = response_receiver.recv().unwrap().unwrap();

        assert_eq!(finished.inputs().len(), 1);
        assert_eq!(finished.inputs()[0].position, CanvasCoordF::new(3.0, 3.0));
    }

    #[test]
    fn brush_thread_batch_applies_settings_after_clearing_active_input() {
        let (command_sender, command_receiver) = sync_channel(16);
        let (response_sender, response_receiver) = sync_channel(1);
        let mut settings = BrushSettings::default();
        settings.radius_px = 4.0;

        command_sender
            .send(BrushThreadCommand::Begin { epoch: 1 })
            .unwrap();
        command_sender
            .send(BrushThreadCommand::CanvasInput {
                epoch: 1,
                input: canvas_input(1, 1.0, 1.0, 1.0),
            })
            .unwrap();
        command_sender
            .send(BrushThreadCommand::UpdateBrushSettings(settings))
            .unwrap();
        command_sender
            .send(BrushThreadCommand::CanvasInput {
                epoch: 1,
                input: canvas_input(2, 2.0, 2.0, 1.0),
            })
            .unwrap();
        command_sender
            .send(BrushThreadCommand::Begin { epoch: 2 })
            .unwrap();
        command_sender
            .send(BrushThreadCommand::CanvasInput {
                epoch: 2,
                input: canvas_input(3, 3.0, 3.0, 1.0),
            })
            .unwrap();
        command_sender
            .send(BrushThreadCommand::Finish {
                epoch: 2,
                response: response_sender,
            })
            .unwrap();
        drop(command_sender);

        super::run_brush_thread(
            BrushWorker::new(
                ToolSet::default_brush(),
                ActiveTool::Brush(BrushId::DEFAULT),
                BrushSettings::default(),
            ),
            command_receiver,
            16,
        );
        let finished = response_receiver.recv().unwrap().unwrap();
        let samples = finished.replace_circle_samples();

        assert_eq!(finished.inputs().len(), 1);
        assert_eq!(finished.inputs()[0].position, CanvasCoordF::new(3.0, 3.0));
        assert_eq!(samples[0].radius_px, 4.0);
    }

    #[test]
    fn brush_thread_runtime_can_restore_finished_stroke() {
        let mut runtime = BrushThreadRuntime::spawn(
            ToolSet::default_brush(),
            ActiveTool::Brush(BrushId::DEFAULT),
            BrushSettings::default(),
            8,
        );

        assert!(runtime.begin_active_stroke());
        runtime.push_canvas_input(canvas_input(0, 5.0, 6.0, 1.0));
        let finished = runtime.finish_active_stroke().unwrap();
        runtime.restore_active_stroke(finished);
        let restored = runtime.finish_active_stroke().unwrap();

        assert_eq!(restored.inputs().len(), 1);
        assert_eq!(restored.inputs()[0].position, CanvasCoordF::new(5.0, 6.0));
    }

    #[test]
    fn brush_thread_runtime_switches_registered_active_tool() {
        let second_brush = BrushId::new(2);
        let mut runtime = BrushThreadRuntime::spawn(
            ToolSet::new(vec![
                Tool::Brush(BrushId::DEFAULT),
                Tool::Brush(second_brush),
            ]),
            ActiveTool::Brush(BrushId::DEFAULT),
            BrushSettings::default(),
            8,
        );

        assert!(runtime.set_active_tool(ActiveTool::Brush(second_brush)));
        assert_eq!(runtime.active_brush_id(), Some(second_brush));
        assert!(runtime.begin_active_stroke());
        runtime.push_canvas_input(canvas_input(0, 1.0, 2.0, 1.0));
        let finished = runtime.finish_active_stroke().unwrap();

        assert_eq!(finished.brush_id(), second_brush);
    }

    #[test]
    fn brush_thread_runtime_updates_settings_before_next_stroke() {
        let mut runtime = BrushThreadRuntime::spawn(
            ToolSet::default_brush(),
            ActiveTool::Brush(BrushId::DEFAULT),
            BrushSettings::default(),
            8,
        );
        let mut settings = BrushSettings::default();
        settings.radius_px = 4.0;
        runtime.update_brush_settings(settings);

        assert!(runtime.begin_active_stroke());
        runtime.push_canvas_input(canvas_input(0, 0.0, 0.0, 1.0));
        let samples = runtime
            .finish_active_stroke()
            .unwrap()
            .replace_circle_samples();

        assert_eq!(samples[0].radius_px, 4.0);
    }
}
