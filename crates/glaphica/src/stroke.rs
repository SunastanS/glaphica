use bytemuck::{Pod, Zeroable};
use gla_color::{PremultipliedRgbaF32, apply_value_mask_to_premultiplied_rgba};
use gla_core::{CanvasCoordF, CanvasInput};
use std::error::Error;
use std::f32::consts::{FRAC_PI_2, PI};
use std::fmt::{Display, Formatter};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, TrySendError, sync_channel};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::{
    ActiveTool, BrushId, BrushSettings, ModulationCurve, OverwriteRingConsumer,
    OverwriteRingProducer, ReplaceCircleStrokeSample, RoundBrushDabVariable,
    RoundBrushInputFeature, RoundBrushSettings, ToolSet, create_overwrite_ring,
};

const MIN_SPACING_RATIO: f32 = 0.05;
const MIN_SPACING_PX: f32 = 1.0;
const SAME_POSITION_EPSILON: f32 = 1e-5;
const REPLACE_CIRCLE_BLOCK_VALUE_COUNT: usize = 7;
pub const ROUND_BRUSH_INPUT_BLOCK_VALUE_COUNT: usize = 11;
const ROUND_INPUT_MAX_SPEED_PX_PER_S: f32 = 1000.0;
const ROUND_DAB_KERNEL_A: f32 = 1.5;

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
    overflow_input_producer: Option<OverwriteRingProducer<QueuedCanvasInput>>,
    active: bool,
    command_epoch: u64,
    thread: Option<JoinHandle<()>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BrushThreadRuntimeError {
    ActiveToolUnavailable(ActiveTool),
    ThreadStopped,
    ThreadPanicked,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BrushInputBlock {
    values: Vec<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BrushInputBlockList {
    brush_id: BrushId,
    blocks: Vec<BrushInputBlock>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BrushInput {
    pub brush_id: BrushId,
    pub blocks: BrushInputBlockList,
}

pub trait BrushInputProcessor: Send + Sync {
    fn begin_stroke(&self) -> Box<dyn BrushStrokeInputProcessor>;

    fn max_affected_radius_px(&self) -> u32;

    fn block_center(
        &self,
        input: &BrushInput,
        block_index: usize,
    ) -> Result<CanvasCoordF, BrushInputError>;

    fn encode_apply_dab_payload(
        &self,
        input: &BrushInput,
        block_index: usize,
        slot_canvas_origin: CanvasCoordF,
    ) -> Result<Vec<u8>, BrushInputError>;

    fn merge_payload(&self) -> Vec<u8>;
}

pub trait BrushStrokeInputProcessor: Send {
    fn reset(&mut self);

    fn push_canvas_inputs(&mut self, canvas_input: &[CanvasInput]) -> Result<(), BrushInputError>;

    fn finish_stroke(&mut self) -> Result<(), BrushInputError>;

    fn current_drawing_sample(&self) -> Option<FrozenCanvasSample>;

    fn drain_brush_input(&mut self) -> Result<Option<BrushInput>, BrushInputError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrushInputError {
    WrongBrush {
        expected: BrushId,
        actual: BrushId,
    },
    InvalidBlockLength {
        brush_id: BrushId,
        block_index: usize,
        expected: usize,
        actual: usize,
    },
    InvalidBlockValue {
        brush_id: BrushId,
        block_index: usize,
        value_index: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrozenCanvasSample {
    pub time_ns: u64,
    pub position: CanvasCoordF,
    pub pressure: f32,
    pub tilt: (f32, f32),
    pub twist: f32,
    pub velocity: CanvasCoordF,
    pub acceleration: CanvasCoordF,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RoundBrushInputProcessor {
    settings: RoundBrushSettings,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RoundMergeSettings {
    pub tint: [f32; 3],
    pub opacity: f32,
    pub stroke_flow: f32,
    pub spacing_ratio: f32,
    pub hardness: f32,
}

#[derive(Debug, Clone)]
struct RoundBrushStrokeInputProcessor {
    settings: RoundBrushSettings,
    inputs: Vec<CanvasInput>,
    emitted_sample_count: usize,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Pod, Zeroable)]
struct RoundApplyPayload {
    center_local_x: f32,
    center_local_y: f32,
    radius_px: f32,
    flow: f32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Pod, Zeroable)]
struct RoundMergePayload {
    tint_and_opacity: [f32; 4],
    coverage_params: [f32; 4],
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct RoundDabParameters {
    radius_px: f32,
    flow: f32,
}

#[derive(Debug, Clone, Copy)]
struct QueuedCanvasInput {
    epoch: u64,
    input: CanvasInput,
}

#[derive(Debug)]
enum BrushThreadCommand {
    Begin {
        epoch: u64,
    },
    Reset {
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
    brush_input: BrushInput,
}

#[derive(Debug, Clone)]
pub(crate) struct ReplaceCircleSampleCache {
    brush_settings: BrushSettings,
    sampled_inputs: Vec<CanvasInput>,
    path_distance: f32,
    next_sample_distance: f32,
    trailing_endpoint: bool,
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
        let (overflow_input_producer, overflow_input_consumer) = create_overwrite_ring(capacity);
        let worker = BrushWorker::new(tool_set.clone(), active_tool, brush_settings);
        let thread = thread::Builder::new()
            .name("glaphica-brush".to_owned())
            .spawn(move || {
                run_brush_thread(worker, command_receiver, overflow_input_consumer, capacity)
            })
            .expect("brush thread should spawn");
        Self {
            tool_set,
            active_tool,
            command_sender: Some(command_sender),
            overflow_input_producer: Some(overflow_input_producer),
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

    pub(crate) fn set_active_tool(
        &mut self,
        active_tool: ActiveTool,
    ) -> Result<(), BrushThreadRuntimeError> {
        if !self.tool_set.contains(active_tool.as_tool()) {
            return Err(BrushThreadRuntimeError::ActiveToolUnavailable(active_tool));
        }
        self.next_epoch();
        self.clear_overflow_inputs();
        self.send_control(BrushThreadCommand::SetActiveTool(active_tool))?;
        self.active_tool = active_tool;
        self.active = false;
        Ok(())
    }

    pub(crate) fn reset_active_stroke_processing(&mut self) -> Result<(), BrushThreadRuntimeError> {
        self.active = false;
        let epoch = self.next_epoch();
        self.clear_overflow_inputs();
        self.send_control(BrushThreadCommand::Reset { epoch })
    }

    pub(crate) fn begin_active_stroke_processing(&mut self) -> Result<(), BrushThreadRuntimeError> {
        if self.active_brush_id().is_none() {
            self.active = false;
            return Err(BrushThreadRuntimeError::ActiveToolUnavailable(
                self.active_tool,
            ));
        }
        let epoch = self.next_epoch();
        self.clear_overflow_inputs();
        match self.send_control(BrushThreadCommand::Begin { epoch }) {
            Ok(()) => {
                self.active = true;
                Ok(())
            }
            Err(error) => {
                self.active = false;
                Err(error)
            }
        }
    }

    pub(crate) fn begin_active_stroke(&mut self) -> bool {
        self.begin_active_stroke_processing().is_ok()
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
            Ok(()) => {}
            Err(TrySendError::Full(BrushThreadCommand::CanvasInput { epoch, input })) => {
                self.push_overflow_input(QueuedCanvasInput { epoch, input });
            }
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

    pub(crate) fn finish_active_stroke_processing(
        &mut self,
    ) -> Result<Option<FinishedRootStroke>, BrushThreadRuntimeError> {
        if !self.active {
            return Ok(None);
        }
        let Some(sender) = self.command_sender.as_ref() else {
            self.active = false;
            return Err(BrushThreadRuntimeError::ThreadStopped);
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
            return Err(BrushThreadRuntimeError::ThreadStopped);
        }
        self.active = false;
        response_receiver
            .recv()
            .map_err(|_| BrushThreadRuntimeError::ThreadStopped)
    }

    pub(crate) fn finish_active_stroke(&mut self) -> Option<FinishedRootStroke> {
        self.finish_active_stroke_processing().ok().flatten()
    }

    pub(crate) fn restore_active_stroke(&mut self, stroke: FinishedRootStroke) {
        let epoch = self.next_epoch();
        self.clear_overflow_inputs();
        self.active = self
            .send_control(BrushThreadCommand::Restore { epoch, stroke })
            .is_ok();
    }

    pub(crate) fn cancel_active_stroke_processing(
        &mut self,
    ) -> Result<(), BrushThreadRuntimeError> {
        self.active = false;
        let epoch = self.next_epoch();
        self.clear_overflow_inputs();
        self.send_control(BrushThreadCommand::Cancel { epoch })
    }

    pub(crate) fn cancel_active_stroke(&mut self) -> bool {
        if !self.active {
            return false;
        }
        self.cancel_active_stroke_processing().is_ok()
    }

    pub(crate) fn has_active_stroke(&self) -> bool {
        self.active
    }

    pub(crate) fn update_brush_settings(&mut self, brush_settings: BrushSettings) {
        self.next_epoch();
        self.active = false;
        self.clear_overflow_inputs();
        let _ = self.send_control(BrushThreadCommand::UpdateBrushSettings(brush_settings));
    }

    fn next_epoch(&mut self) -> u64 {
        self.command_epoch = self.command_epoch.saturating_add(1);
        self.command_epoch
    }

    fn send_control(&self, command: BrushThreadCommand) -> Result<(), BrushThreadRuntimeError> {
        let sender = self
            .command_sender
            .as_ref()
            .ok_or(BrushThreadRuntimeError::ThreadStopped)?;
        sender
            .send(command)
            .map_err(|_| BrushThreadRuntimeError::ThreadStopped)
    }

    fn push_overflow_input(&mut self, input: QueuedCanvasInput) {
        let Some(producer) = self.overflow_input_producer.as_ref() else {
            return;
        };
        producer.push(input);
    }

    fn clear_overflow_inputs(&self) {
        if let Some(producer) = self.overflow_input_producer.as_ref() {
            producer.clear();
        }
    }

    #[allow(dead_code)]
    pub(crate) fn shutdown(mut self) -> Result<(), BrushThreadRuntimeError> {
        drop(self.overflow_input_producer.take());
        drop(self.command_sender.take());
        let Some(thread) = self.thread.take() else {
            return Ok(());
        };
        thread
            .join()
            .map_err(|_| BrushThreadRuntimeError::ThreadPanicked)
    }
}

impl Display for BrushThreadRuntimeError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ActiveToolUnavailable(ActiveTool::Brush(brush_id)) => {
                write!(
                    formatter,
                    "active brush {} is not in the tool set",
                    brush_id.value()
                )
            }
            Self::ThreadStopped => formatter.write_str("brush thread stopped"),
            Self::ThreadPanicked => formatter.write_str("brush thread panicked"),
        }
    }
}

impl Error for BrushThreadRuntimeError {}

impl BrushInputBlock {
    pub fn new(values: Vec<f32>) -> Self {
        Self { values }
    }

    pub fn values(&self) -> &[f32] {
        &self.values
    }

    fn from_replace_circle_sample(sample: ReplaceCircleStrokeSample) -> Self {
        Self::new(vec![
            sample.center.x,
            sample.center.y,
            sample.radius_px,
            sample.color.r,
            sample.color.g,
            sample.color.b,
            sample.color.a,
        ])
    }

    fn replace_circle_sample(
        &self,
        brush_id: BrushId,
        block_index: usize,
    ) -> Result<ReplaceCircleStrokeSample, BrushInputError> {
        if self.values.len() != REPLACE_CIRCLE_BLOCK_VALUE_COUNT {
            return Err(BrushInputError::InvalidBlockLength {
                brush_id,
                block_index,
                expected: REPLACE_CIRCLE_BLOCK_VALUE_COUNT,
                actual: self.values.len(),
            });
        }
        for (value_index, value) in self.values.iter().enumerate() {
            if !value.is_finite() {
                return Err(BrushInputError::InvalidBlockValue {
                    brush_id,
                    block_index,
                    value_index,
                });
            }
        }
        Ok(ReplaceCircleStrokeSample::new(
            self.values[0],
            self.values[1],
            self.values[2],
            PremultipliedRgbaF32::new(
                self.values[3],
                self.values[4],
                self.values[5],
                self.values[6],
            ),
        ))
    }
}

impl BrushInputBlockList {
    pub fn new(brush_id: BrushId) -> Self {
        Self {
            brush_id,
            blocks: Vec::new(),
        }
    }

    pub fn brush_id(&self) -> BrushId {
        self.brush_id
    }

    pub fn blocks(&self) -> &[BrushInputBlock] {
        &self.blocks
    }

    pub fn push_block(&mut self, values: Vec<f32>) {
        self.blocks.push(BrushInputBlock::new(values));
    }

    pub fn push_replace_circle_sample(&mut self, sample: ReplaceCircleStrokeSample) {
        self.blocks
            .push(BrushInputBlock::from_replace_circle_sample(sample));
    }

    fn from_replace_circle_samples(
        brush_id: BrushId,
        samples: impl IntoIterator<Item = ReplaceCircleStrokeSample>,
    ) -> Self {
        let mut blocks = Self::new(brush_id);
        for sample in samples {
            blocks.push_replace_circle_sample(sample);
        }
        blocks
    }

    fn replace_circle_samples(&self) -> Result<Vec<ReplaceCircleStrokeSample>, BrushInputError> {
        self.blocks
            .iter()
            .enumerate()
            .map(|(block_index, block)| block.replace_circle_sample(self.brush_id, block_index))
            .collect()
    }
}

impl BrushInput {
    pub fn new(blocks: BrushInputBlockList) -> Self {
        Self {
            brush_id: blocks.brush_id(),
            blocks,
        }
    }

    pub fn from_replace_circle_samples(
        brush_id: BrushId,
        samples: impl IntoIterator<Item = ReplaceCircleStrokeSample>,
    ) -> Self {
        Self::new(BrushInputBlockList::from_replace_circle_samples(
            brush_id, samples,
        ))
    }

    pub fn replace_circle_samples(
        &self,
    ) -> Result<Vec<ReplaceCircleStrokeSample>, BrushInputError> {
        let actual = self.blocks.brush_id();
        if actual != self.brush_id {
            return Err(BrushInputError::WrongBrush {
                expected: self.brush_id,
                actual,
            });
        }
        self.blocks.replace_circle_samples()
    }
}

pub fn encode_round_apply_payload(center_local: [f32; 2], radius_px: f32, flow: f32) -> Vec<u8> {
    bytemuck::bytes_of(&RoundApplyPayload {
        center_local_x: center_local[0],
        center_local_y: center_local[1],
        radius_px,
        flow,
    })
    .to_vec()
}

pub fn encode_round_merge_payload(settings: RoundMergeSettings) -> Vec<u8> {
    bytemuck::bytes_of(&build_round_merge_payload(settings)).to_vec()
}

impl Default for RoundBrushInputProcessor {
    fn default() -> Self {
        Self::from_settings(RoundBrushSettings::default())
    }
}

impl From<RoundBrushSettings> for RoundBrushInputProcessor {
    fn from(settings: RoundBrushSettings) -> Self {
        Self::from_settings(settings)
    }
}

impl RoundBrushInputProcessor {
    pub fn from_settings(settings: RoundBrushSettings) -> Self {
        Self { settings }
    }

    pub fn settings(&self) -> RoundBrushSettings {
        self.settings.clone()
    }

    pub fn with_base_radius_px(mut self, base_radius_px: f32) -> Self {
        self.settings.base_radius_px = base_radius_px;
        self
    }

    pub fn with_spacing_ratio(mut self, spacing_ratio: f32) -> Self {
        self.settings.spacing_ratio = spacing_ratio;
        self
    }

    pub fn with_base_hardness(mut self, base_hardness: f32) -> Self {
        self.settings.base_hardness = base_hardness;
        self
    }

    pub fn with_base_flow(mut self, base_flow: f32) -> Self {
        self.settings.base_flow = base_flow;
        self
    }

    pub fn with_base_opacity(mut self, base_opacity: f32) -> Self {
        self.settings.base_opacity = base_opacity;
        self
    }

    pub fn with_tint(mut self, tint: [f32; 3]) -> Self {
        self.settings.tint = tint;
        self
    }

    pub fn with_modulation_curve(
        mut self,
        variable: RoundBrushDabVariable,
        feature: RoundBrushInputFeature,
        curve: ModulationCurve,
    ) -> Self {
        self.settings.modulations = self
            .settings
            .modulations
            .with_curve(variable, feature, curve);
        self
    }

    pub fn with_modulations(mut self, modulations: crate::RoundBrushModulationSet) -> Self {
        self.settings.modulations = modulations;
        self
    }
}

impl BrushInputProcessor for RoundBrushInputProcessor {
    fn begin_stroke(&self) -> Box<dyn BrushStrokeInputProcessor> {
        Box::new(RoundBrushStrokeInputProcessor {
            settings: self.settings.clone(),
            inputs: Vec::new(),
            emitted_sample_count: 0,
        })
    }

    fn max_affected_radius_px(&self) -> u32 {
        self.settings.base_radius_px.ceil().max(1.0) as u32
    }

    fn block_center(
        &self,
        input: &BrushInput,
        block_index: usize,
    ) -> Result<CanvasCoordF, BrushInputError> {
        let values = round_brush_block_values(input, block_index)?;
        Ok(CanvasCoordF::new(values[0], values[1]))
    }

    fn encode_apply_dab_payload(
        &self,
        input: &BrushInput,
        block_index: usize,
        slot_canvas_origin: CanvasCoordF,
    ) -> Result<Vec<u8>, BrushInputError> {
        let values = round_brush_block_values(input, block_index)?;
        Ok(encode_round_apply_payload(
            [
                values[0] - slot_canvas_origin.x,
                values[1] - slot_canvas_origin.y,
            ],
            values[2].max(0.0),
            values[3],
        ))
    }

    fn merge_payload(&self) -> Vec<u8> {
        encode_round_merge_payload(RoundMergeSettings {
            tint: self.settings.tint,
            opacity: self.settings.base_opacity,
            stroke_flow: self.settings.base_flow,
            spacing_ratio: self.settings.spacing_ratio,
            hardness: self.settings.base_hardness,
        })
    }
}

impl BrushStrokeInputProcessor for RoundBrushStrokeInputProcessor {
    fn reset(&mut self) {
        self.inputs.clear();
        self.emitted_sample_count = 0;
    }

    fn push_canvas_inputs(&mut self, canvas_input: &[CanvasInput]) -> Result<(), BrushInputError> {
        self.inputs.extend_from_slice(canvas_input);
        Ok(())
    }

    fn finish_stroke(&mut self) -> Result<(), BrushInputError> {
        Ok(())
    }

    fn current_drawing_sample(&self) -> Option<FrozenCanvasSample> {
        let (previous, current) = match self.inputs.as_slice() {
            [] => return None,
            [current] => (None, *current),
            inputs => (
                inputs.get(inputs.len().saturating_sub(2)).copied(),
                *inputs.last()?,
            ),
        };
        Some(frozen_canvas_sample(previous, current))
    }

    fn drain_brush_input(&mut self) -> Result<Option<BrushInput>, BrushInputError> {
        let brush_settings = BrushSettings::from_round_brush(self.settings.clone());
        let sampled_inputs = sample_canvas_inputs(&self.inputs, &brush_settings);
        if self.emitted_sample_count >= sampled_inputs.len() {
            return Ok(None);
        }

        let mut blocks = BrushInputBlockList::new(BrushId::DEFAULT);
        for absolute_index in self.emitted_sample_count..sampled_inputs.len() {
            let input = sampled_inputs[absolute_index];
            let block_index = blocks.blocks().len();
            push_round_brush_block(
                &mut blocks,
                block_index,
                input,
                previous_input(&sampled_inputs, absolute_index),
                &self.settings,
            )?;
        }
        self.emitted_sample_count = sampled_inputs.len();

        if blocks.blocks().is_empty() {
            Ok(None)
        } else {
            Ok(Some(BrushInput::new(blocks)))
        }
    }
}

impl Display for BrushInputError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WrongBrush { expected, actual } => write!(
                formatter,
                "brush input is for brush {}, expected brush {}",
                actual.value(),
                expected.value()
            ),
            Self::InvalidBlockLength {
                brush_id,
                block_index,
                expected,
                actual,
            } => write!(
                formatter,
                "brush {} input block {} length mismatch: expected {}, got {}",
                brush_id.value(),
                block_index,
                expected,
                actual
            ),
            Self::InvalidBlockValue {
                brush_id,
                block_index,
                value_index,
            } => write!(
                formatter,
                "brush {} input block {} value {} is not finite",
                brush_id.value(),
                block_index,
                value_index
            ),
        }
    }
}

impl Error for BrushInputError {}

impl Drop for BrushThreadRuntime {
    fn drop(&mut self) {
        drop(self.overflow_input_producer.take());
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
        self.active_stroke = Some(ActiveRootStroke::new(brush_id, self.brush_settings.clone()));
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
        (!stroke.is_empty()).then(|| {
            let brush_input = stroke.brush_input();
            FinishedRootStroke {
                stroke,
                brush_input,
            }
        })
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
    pub(crate) fn brush_input(&self) -> &BrushInput {
        &self.brush_input
    }

    pub(crate) fn replace_circle_samples(&self) -> Vec<ReplaceCircleStrokeSample> {
        self.brush_input
            .replace_circle_samples()
            .expect("finished root stroke should contain valid replace-circle brush input")
    }

    pub(crate) fn brush_id(&self) -> BrushId {
        self.stroke.brush_id()
    }

    pub(crate) fn inputs(&self) -> &[CanvasInput] {
        self.stroke.inputs()
    }
}

impl ReplaceCircleSampleCache {
    pub(crate) fn new(brush_settings: BrushSettings) -> Self {
        let next_sample_distance = dab_spacing_px(&brush_settings);
        Self {
            brush_settings,
            sampled_inputs: Vec::new(),
            path_distance: 0.0,
            next_sample_distance,
            trailing_endpoint: false,
        }
    }

    pub(crate) fn push_input(&mut self, input: CanvasInput) {
        let Some(&previous) = self.sampled_inputs.last() else {
            self.sampled_inputs.push(input);
            return;
        };

        if self.trailing_endpoint {
            self.sampled_inputs.pop();
            self.trailing_endpoint = false;
        }

        let segment_length = distance_between(previous.position, input.position);
        if segment_length > f32::EPSILON {
            let segment_end_distance = self.path_distance + segment_length;
            while self.next_sample_distance <= segment_end_distance {
                let t = (self.next_sample_distance - self.path_distance) / segment_length;
                self.sampled_inputs
                    .push(interpolate_input(previous, input, t));
                self.next_sample_distance += dab_spacing_px(&self.brush_settings);
            }
            self.path_distance = segment_end_distance;
        }

        if self
            .sampled_inputs
            .last()
            .is_none_or(|sample| !same_position(sample.position, input.position))
        {
            self.sampled_inputs.push(input);
            self.trailing_endpoint = true;
        }
    }

    pub(crate) fn replace_circle_samples(&self) -> Vec<ReplaceCircleStrokeSample> {
        replace_circle_samples_for_sampled_inputs(&self.sampled_inputs, &self.brush_settings)
    }
}

impl ActiveRootStroke {
    fn brush_input(&self) -> BrushInput {
        BrushInput::from_replace_circle_samples(self.brush_id, self.replace_circle_samples())
    }

    fn replace_circle_samples(&self) -> Vec<ReplaceCircleStrokeSample> {
        replace_circle_samples_for_inputs(&self.inputs, &self.brush_settings)
    }
}

pub(crate) fn replace_circle_samples_for_inputs(
    inputs: &[CanvasInput],
    brush_settings: &BrushSettings,
) -> Vec<ReplaceCircleStrokeSample> {
    let sampled_inputs = sample_canvas_inputs(inputs, brush_settings);
    replace_circle_samples_for_sampled_inputs(&sampled_inputs, brush_settings)
}

fn replace_circle_samples_for_sampled_inputs(
    inputs: &[CanvasInput],
    brush_settings: &BrushSettings,
) -> Vec<ReplaceCircleStrokeSample> {
    inputs
        .iter()
        .copied()
        .enumerate()
        .map(|(index, input)| {
            replace_circle_sample_for_input(input, previous_input(inputs, index), brush_settings)
        })
        .collect()
}

fn replace_circle_sample_for_input(
    input: CanvasInput,
    previous_input: Option<CanvasInput>,
    brush_settings: &BrushSettings,
) -> ReplaceCircleStrokeSample {
    let pressure = input.pressure.clamp(0.0, 1.0);
    let tilt = normalize_tilt(input.tilt);
    let twist = normalize_twist(input.twist);
    let speed = normalized_input_speed(previous_input, input);
    let radius_factor = brush_settings
        .modulations
        .sample_factor(RoundBrushDabVariable::Radius, pressure, tilt, twist, speed)
        .clamp(0.0, 1.0);
    let flow_factor = brush_settings
        .modulations
        .sample_factor(RoundBrushDabVariable::Flow, pressure, tilt, twist, speed)
        .clamp(0.0, 1.0);
    ReplaceCircleStrokeSample {
        center: input.position,
        radius_px: brush_settings.radius_px * radius_factor,
        color: apply_value_mask_to_premultiplied_rgba(
            brush_settings.color,
            brush_settings.flow * flow_factor,
            brush_settings.opacity,
        ),
    }
}

fn push_round_brush_block(
    blocks: &mut BrushInputBlockList,
    block_index: usize,
    input: CanvasInput,
    previous_input: Option<CanvasInput>,
    settings: &RoundBrushSettings,
) -> Result<(), BrushInputError> {
    let dab = round_dab_parameters(input, previous_input, settings);
    let values = vec![
        input.position.x,
        input.position.y,
        dab.radius_px,
        dab.flow,
        settings.base_flow,
        sanitized_spacing_ratio(settings.spacing_ratio),
        settings.base_hardness,
        settings.base_opacity,
        settings.tint[0],
        settings.tint[1],
        settings.tint[2],
    ];

    for (value_index, value) in values.iter().copied().enumerate() {
        if !value.is_finite() {
            return Err(BrushInputError::InvalidBlockValue {
                brush_id: BrushId::DEFAULT,
                block_index,
                value_index,
            });
        }
    }

    blocks.push_block(values);
    Ok(())
}

fn round_dab_parameters(
    input: CanvasInput,
    previous_input: Option<CanvasInput>,
    settings: &RoundBrushSettings,
) -> RoundDabParameters {
    let pressure = input.pressure.clamp(0.0, 1.0);
    let tilt = normalize_tilt(input.tilt);
    let twist = normalize_twist(input.twist);
    let speed = normalized_input_speed(previous_input, input);
    let radius_factor = settings
        .modulations
        .sample_factor(RoundBrushDabVariable::Radius, pressure, tilt, twist, speed)
        .clamp(0.0, 1.0);
    let flow_factor = settings
        .modulations
        .sample_factor(RoundBrushDabVariable::Flow, pressure, tilt, twist, speed)
        .clamp(0.0, 1.0);

    RoundDabParameters {
        radius_px: settings.base_radius_px.max(0.0) * radius_factor,
        flow: settings.base_flow.max(0.0) * flow_factor,
    }
}

fn round_brush_block_values(
    input: &BrushInput,
    block_index: usize,
) -> Result<&[f32], BrushInputError> {
    if input.brush_id != BrushId::DEFAULT {
        return Err(BrushInputError::WrongBrush {
            expected: BrushId::DEFAULT,
            actual: input.brush_id,
        });
    }
    if input.blocks.brush_id() != input.brush_id {
        return Err(BrushInputError::WrongBrush {
            expected: input.brush_id,
            actual: input.blocks.brush_id(),
        });
    }

    let Some(block) = input.blocks.blocks().get(block_index) else {
        return Err(BrushInputError::InvalidBlockLength {
            brush_id: BrushId::DEFAULT,
            block_index,
            expected: block_index + 1,
            actual: input.blocks.blocks().len(),
        });
    };
    if block.values().len() != ROUND_BRUSH_INPUT_BLOCK_VALUE_COUNT {
        return Err(BrushInputError::InvalidBlockLength {
            brush_id: BrushId::DEFAULT,
            block_index,
            expected: ROUND_BRUSH_INPUT_BLOCK_VALUE_COUNT,
            actual: block.values().len(),
        });
    }
    for (value_index, value) in block.values().iter().copied().enumerate() {
        if !value.is_finite() {
            return Err(BrushInputError::InvalidBlockValue {
                brush_id: BrushId::DEFAULT,
                block_index,
                value_index,
            });
        }
    }
    Ok(block.values())
}

fn build_round_merge_payload(settings: RoundMergeSettings) -> RoundMergePayload {
    let tint_and_opacity = [
        settings.tint[0],
        settings.tint[1],
        settings.tint[2],
        settings.opacity.clamp(0.0, 1.0),
    ];
    let stroke_flow = settings.stroke_flow.max(0.0);
    let spacing_ratio = sanitized_spacing_ratio(settings.spacing_ratio);
    let hardness = settings.hardness.clamp(0.0, 1.0);
    let hardness_threshold_source =
        round_hardness_threshold_source(stroke_flow, spacing_ratio, hardness);
    RoundMergePayload {
        tint_and_opacity,
        coverage_params: [hardness_threshold_source, hardness, 0.0, 0.0],
    }
}

fn round_hardness_threshold_source(stroke_flow: f32, spacing_ratio: f32, hardness: f32) -> f32 {
    let stroke_flow = stroke_flow.max(0.0);
    let hardness = hardness.clamp(0.0, 1.0);
    if stroke_flow <= f32::EPSILON {
        return 0.0;
    }
    if hardness >= 1.0 {
        return 0.0;
    }
    round_stroke_source_at_distance(stroke_flow, spacing_ratio, hardness).max(f32::EPSILON)
}

fn round_stroke_source_at_distance(
    stroke_flow: f32,
    spacing_ratio: f32,
    normalized_distance: f32,
) -> f32 {
    let stroke_flow = stroke_flow.max(0.0);
    if stroke_flow <= f32::EPSILON {
        return 0.0;
    }
    let spacing_ratio = sanitized_spacing_ratio(spacing_ratio);
    let normalized_distance = normalized_distance.max(0.0);
    if normalized_distance >= 1.0 {
        return 0.0;
    }

    let mut source = round_dab_kernel(normalized_distance) * stroke_flow;
    let mut offset = spacing_ratio;
    while offset < 1.0 {
        let previous_dab_source = round_dab_kernel(normalized_distance.hypot(offset)) * stroke_flow;
        source += previous_dab_source * 2.0;
        offset += spacing_ratio;
    }
    source
}

fn round_dab_kernel(relative_distance: f32) -> f32 {
    if relative_distance >= 1.0 {
        return 0.0;
    }
    let r2 = relative_distance * relative_distance;
    (1.0 - r2).powf(ROUND_DAB_KERNEL_A)
}

fn sample_canvas_inputs(inputs: &[CanvasInput], settings: &BrushSettings) -> Vec<CanvasInput> {
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

fn dab_spacing_px(settings: &BrushSettings) -> f32 {
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

fn previous_input(inputs: &[CanvasInput], index: usize) -> Option<CanvasInput> {
    index
        .checked_sub(1)
        .and_then(|previous| inputs.get(previous))
        .copied()
}

fn frozen_canvas_sample(previous: Option<CanvasInput>, input: CanvasInput) -> FrozenCanvasSample {
    let velocity = input_velocity(previous, input);
    FrozenCanvasSample {
        time_ns: input.time_ns,
        position: input.position,
        pressure: input.pressure,
        tilt: input.tilt,
        twist: input.twist,
        velocity,
        acceleration: CanvasCoordF::new(0.0, 0.0),
    }
}

fn input_velocity(previous: Option<CanvasInput>, input: CanvasInput) -> CanvasCoordF {
    let Some(previous) = previous else {
        return CanvasCoordF::new(0.0, 0.0);
    };
    let delta_time_ns = input.time_ns.saturating_sub(previous.time_ns);
    if delta_time_ns == 0 {
        return CanvasCoordF::new(0.0, 0.0);
    }
    let seconds = delta_time_ns as f32 * 1e-9;
    if seconds <= f32::EPSILON {
        return CanvasCoordF::new(0.0, 0.0);
    }
    CanvasCoordF::new(
        (input.position.x - previous.position.x) / seconds,
        (input.position.y - previous.position.y) / seconds,
    )
}

fn normalize_tilt(tilt: (f32, f32)) -> f32 {
    let magnitude = (tilt.0 * tilt.0 + tilt.1 * tilt.1).sqrt();
    (magnitude / FRAC_PI_2).clamp(0.0, 1.0)
}

fn normalize_twist(twist: f32) -> f32 {
    ((twist + PI) / (2.0 * PI)).clamp(0.0, 1.0)
}

fn normalized_input_speed(previous: Option<CanvasInput>, input: CanvasInput) -> f32 {
    let Some(previous) = previous else {
        return 0.0;
    };
    let delta_time_ns = input.time_ns.saturating_sub(previous.time_ns);
    if delta_time_ns == 0 {
        return 0.0;
    }
    let distance = distance_between(previous.position, input.position);
    let seconds = delta_time_ns as f32 * 1e-9;
    if seconds <= f32::EPSILON {
        return 0.0;
    }
    (distance / seconds / ROUND_INPUT_MAX_SPEED_PX_PER_S).clamp(0.0, 1.0)
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
    overflow_input_consumer: OverwriteRingConsumer<QueuedCanvasInput>,
    batch_capacity: usize,
) {
    let batch_capacity = batch_capacity.max(1);
    let mut command_batch = Vec::with_capacity(batch_capacity);
    let mut canvas_batch = Vec::with_capacity(batch_capacity);
    let mut overflow_batch = Vec::with_capacity(batch_capacity);
    let mut stroke_state = WorkerStrokeState::Idle;

    while receive_command_batch(&command_receiver, &mut command_batch, batch_capacity) {
        for command in command_batch.drain(..) {
            match command {
                BrushThreadCommand::Begin { epoch } => {
                    overflow_input_consumer.clear();
                    canvas_batch.clear();
                    worker.begin_active_stroke();
                    stroke_state = WorkerStrokeState::Active { epoch };
                }
                BrushThreadCommand::Reset { epoch } => {
                    let _ = epoch;
                    overflow_input_consumer.clear();
                    canvas_batch.clear();
                    worker.cancel_active_stroke();
                    stroke_state = WorkerStrokeState::Idle;
                }
                BrushThreadCommand::CanvasInput { epoch, input } => {
                    if stroke_state.accepts(epoch) {
                        canvas_batch.push(input);
                    }
                }
                BrushThreadCommand::Finish { epoch, response } => {
                    drain_overflow_inputs(
                        &overflow_input_consumer,
                        &mut overflow_batch,
                        batch_capacity,
                        stroke_state,
                        &mut canvas_batch,
                    );
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
                    overflow_input_consumer.clear();
                    canvas_batch.clear();
                    worker.restore_active_stroke(stroke);
                    stroke_state = WorkerStrokeState::Active { epoch };
                }
                BrushThreadCommand::Cancel { epoch } => {
                    let _ = epoch;
                    overflow_input_consumer.clear();
                    canvas_batch.clear();
                    worker.cancel_active_stroke();
                    stroke_state = WorkerStrokeState::Idle;
                }
                BrushThreadCommand::SetActiveTool(active_tool) => {
                    overflow_input_consumer.clear();
                    canvas_batch.clear();
                    worker.set_active_tool(active_tool);
                    stroke_state = WorkerStrokeState::Idle;
                }
                BrushThreadCommand::UpdateBrushSettings(settings) => {
                    overflow_input_consumer.clear();
                    canvas_batch.clear();
                    worker.cancel_active_stroke();
                    worker.update_brush_settings(settings);
                    stroke_state = WorkerStrokeState::Idle;
                }
            }
        }
        drain_overflow_inputs(
            &overflow_input_consumer,
            &mut overflow_batch,
            batch_capacity,
            stroke_state,
            &mut canvas_batch,
        );
        flush_canvas_batch(&mut worker, &mut canvas_batch);
    }
}

fn drain_overflow_inputs(
    overflow_input_consumer: &OverwriteRingConsumer<QueuedCanvasInput>,
    output: &mut Vec<QueuedCanvasInput>,
    max_items: usize,
    stroke_state: WorkerStrokeState,
    canvas_batch: &mut Vec<CanvasInput>,
) {
    overflow_input_consumer.drain_batch_with_wait(output, max_items, Duration::ZERO);
    for queued in output.drain(..) {
        if stroke_state.accepts(queued.epoch) {
            canvas_batch.push(queued.input);
        }
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
    use super::{
        ActiveRootStroke, BrushInput, BrushInputBlockList, BrushInputError, BrushInputProcessor,
        BrushThreadCommand, BrushThreadRuntime, BrushThreadRuntimeError, BrushWorker,
        QueuedCanvasInput, ROUND_BRUSH_INPUT_BLOCK_VALUE_COUNT, ReplaceCircleSampleCache,
        RoundBrushInputProcessor, RoundMergeSettings, encode_round_apply_payload,
        encode_round_merge_payload,
    };
    use crate::create_overwrite_ring;
    use crate::{
        ActiveTool, BrushId, BrushSettings, CurvePoint, ModulationCurve, RoundBrushDabVariable,
        RoundBrushInputFeature, RoundBrushModulationSet, Tool, ToolSet,
    };
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
    fn replace_circle_samples_apply_round_brush_pressure_modulation() {
        let mut settings = BrushSettings::default();
        settings.radius_px = 10.0;
        settings.flow = 1.0;
        settings.opacity = 1.0;
        settings.modulations = RoundBrushModulationSet::default()
            .with_curve(
                RoundBrushDabVariable::Radius,
                RoundBrushInputFeature::Pressure,
                ModulationCurve::new(vec![CurvePoint::new(0.0, 0.5), CurvePoint::new(1.0, 1.0)])
                    .unwrap(),
            )
            .with_curve(
                RoundBrushDabVariable::Flow,
                RoundBrushInputFeature::Pressure,
                ModulationCurve::flat_one(),
            );
        let mut stroke = ActiveRootStroke::new(BrushId::DEFAULT, settings);

        stroke.push_input(canvas_input(0, 0.0, 0.0, 0.5));

        let samples = stroke.replace_circle_samples();

        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].radius_px, 7.5);
        assert_eq!(samples[0].color.a, 1.0);
    }

    #[test]
    fn replace_circle_sample_cache_matches_batch_sampling_incrementally() {
        let mut settings = BrushSettings::default();
        settings.radius_px = 10.0;
        settings.spacing_ratio = 1.0;
        settings.flow = 0.5;
        settings.opacity = 0.8;
        settings.color = gla_color::PremultipliedRgbaF32::new(1.0, 0.5, 0.25, 1.0);
        let inputs = [
            canvas_input(0, 0.0, 0.0, 1.0),
            canvas_input(1, 3.0, 0.0, 0.8),
            canvas_input(2, 3.0, 0.0, 0.6),
            canvas_input(3, 12.0, 0.0, 0.4),
            canvas_input(4, 30.0, 0.0, 0.2),
            canvas_input(5, 30.0, 0.0, 0.1),
            canvas_input(6, 35.0, 5.0, 1.0),
        ];
        let mut cache = ReplaceCircleSampleCache::new(settings.clone());
        let mut seen_inputs = Vec::new();

        for input in inputs {
            seen_inputs.push(input);
            cache.push_input(input);

            assert_eq!(
                cache.replace_circle_samples(),
                super::replace_circle_samples_for_inputs(&seen_inputs, &settings)
            );
        }
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
    fn finished_stroke_exports_replace_circle_brush_input_blocks() {
        let mut worker = BrushWorker::new(
            ToolSet::default_brush(),
            ActiveTool::Brush(BrushId::DEFAULT),
            BrushSettings::default(),
        );

        assert!(worker.begin_active_stroke());
        worker.push_canvas_input(canvas_input(0, 10.0, 20.0, 0.5));
        let finished = worker.finish_active_stroke().unwrap();
        let samples = finished.replace_circle_samples();
        let brush_input = finished.brush_input();

        assert_eq!(brush_input.brush_id, BrushId::DEFAULT);
        assert_eq!(brush_input.blocks.brush_id(), BrushId::DEFAULT);
        assert_eq!(brush_input.blocks.blocks().len(), samples.len());
        assert_eq!(brush_input.replace_circle_samples().unwrap(), samples);
    }

    #[test]
    fn round_brush_processor_produces_dev_style_blocks_from_canvas_input() {
        let processor = RoundBrushInputProcessor::default();
        let mut stroke = processor.begin_stroke();

        stroke
            .push_canvas_inputs(&[
                canvas_input(0, 0.0, 0.0, 0.5),
                canvas_input(1_000_000, 12.0, 0.0, 0.5),
            ])
            .unwrap();
        stroke.finish_stroke().unwrap();
        let brush_input = stroke.drain_brush_input().unwrap().unwrap();

        assert_eq!(brush_input.brush_id, BrushId::DEFAULT);
        assert!(!brush_input.blocks.blocks().is_empty());
        let values = brush_input.blocks.blocks()[0].values();
        assert_eq!(values.len(), ROUND_BRUSH_INPUT_BLOCK_VALUE_COUNT);
        assert_eq!(
            &values[..],
            &[0.0, 0.0, 5.0, 0.5, 1.0, 1.0, 0.7, 1.0, 0.0, 0.0, 1.0]
        );
    }

    #[test]
    fn round_brush_processor_maps_modulation_and_encodes_apply_payload() {
        let processor = RoundBrushInputProcessor::default().with_modulation_curve(
            RoundBrushDabVariable::Radius,
            RoundBrushInputFeature::Pressure,
            ModulationCurve::identity(),
        );
        let mut stroke = processor.begin_stroke();

        stroke
            .push_canvas_inputs(&[canvas_input(0, 8.0, 9.0, 0.5)])
            .unwrap();
        let brush_input = stroke.drain_brush_input().unwrap().unwrap();
        let values = brush_input.blocks.blocks()[0].values();

        assert_eq!(values[2], 2.5);
        assert_eq!(values[3], 0.5);
        assert_eq!(
            processor.block_center(&brush_input, 0).unwrap(),
            CanvasCoordF::new(8.0, 9.0)
        );
        assert_eq!(
            processor
                .encode_apply_dab_payload(&brush_input, 0, CanvasCoordF::new(4.0, 7.0))
                .unwrap(),
            encode_round_apply_payload([4.0, 2.0], 2.5, 0.5)
        );
    }

    #[test]
    fn round_brush_processor_merge_payload_uses_settings() {
        let processor = RoundBrushInputProcessor::default()
            .with_base_flow(0.9)
            .with_base_hardness(0.4)
            .with_base_opacity(0.6)
            .with_spacing_ratio(0.8)
            .with_tint([0.2, 0.3, 0.4]);

        assert_eq!(
            processor.merge_payload(),
            encode_round_merge_payload(RoundMergeSettings {
                tint: [0.2, 0.3, 0.4],
                opacity: 0.6,
                stroke_flow: 0.9,
                spacing_ratio: 0.8,
                hardness: 0.4,
            })
        );
    }

    #[test]
    fn round_brush_processor_current_sample_reports_velocity() {
        let processor = RoundBrushInputProcessor::default();
        let mut stroke = processor.begin_stroke();

        stroke
            .push_canvas_inputs(&[
                canvas_input(0, 1.0, 2.0, 0.5),
                canvas_input(1_000_000_000, 11.0, 2.0, 0.5),
            ])
            .unwrap();

        let sample = stroke.current_drawing_sample().unwrap();

        assert_eq!(sample.position, CanvasCoordF::new(11.0, 2.0));
        assert_eq!(sample.velocity, CanvasCoordF::new(10.0, 0.0));
        assert_eq!(sample.acceleration, CanvasCoordF::new(0.0, 0.0));
    }

    #[test]
    fn brush_input_reports_invalid_replace_circle_blocks() {
        let mut blocks = BrushInputBlockList::new(BrushId::DEFAULT);
        blocks.push_block(vec![1.0, 2.0]);
        let error = BrushInput::new(blocks)
            .replace_circle_samples()
            .unwrap_err();

        assert!(matches!(
            error,
            BrushInputError::InvalidBlockLength {
                brush_id: BrushId::DEFAULT,
                block_index: 0,
                expected: 7,
                actual: 2
            }
        ));

        let mut blocks = BrushInputBlockList::new(BrushId::DEFAULT);
        blocks.push_block(vec![1.0, f32::NAN, 8.0, 1.0, 0.0, 0.0, 1.0]);
        let error = BrushInput::new(blocks)
            .replace_circle_samples()
            .unwrap_err();

        assert!(matches!(
            error,
            BrushInputError::InvalidBlockValue {
                brush_id: BrushId::DEFAULT,
                block_index: 0,
                value_index: 1
            }
        ));
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
    fn brush_thread_runtime_processing_methods_finish_inputs() {
        let mut runtime = BrushThreadRuntime::spawn(
            ToolSet::default_brush(),
            ActiveTool::Brush(BrushId::DEFAULT),
            BrushSettings::default(),
            8,
        );

        runtime.begin_active_stroke_processing().unwrap();
        runtime.push_canvas_input(canvas_input(0, 7.0, 8.0, 1.0));
        let finished = runtime.finish_active_stroke_processing().unwrap().unwrap();

        assert_eq!(finished.brush_id(), BrushId::DEFAULT);
        assert_eq!(finished.inputs().len(), 1);
        assert_eq!(finished.inputs()[0].position, CanvasCoordF::new(7.0, 8.0));
        assert!(!runtime.has_active_stroke());
    }

    #[test]
    fn brush_thread_runtime_reset_processing_drops_active_inputs() {
        let mut runtime = BrushThreadRuntime::spawn(
            ToolSet::default_brush(),
            ActiveTool::Brush(BrushId::DEFAULT),
            BrushSettings::default(),
            8,
        );

        runtime.begin_active_stroke_processing().unwrap();
        runtime.push_canvas_input(canvas_input(0, 1.0, 1.0, 1.0));
        runtime.reset_active_stroke_processing().unwrap();
        assert!(!runtime.has_active_stroke());
        runtime.push_canvas_input(canvas_input(1, 2.0, 2.0, 1.0));
        runtime.begin_active_stroke_processing().unwrap();
        runtime.push_canvas_input(canvas_input(2, 3.0, 3.0, 1.0));
        let finished = runtime.finish_active_stroke_processing().unwrap().unwrap();

        assert_eq!(finished.inputs().len(), 1);
        assert_eq!(finished.inputs()[0].position, CanvasCoordF::new(3.0, 3.0));
    }

    #[test]
    fn brush_thread_runtime_processing_reports_unregistered_active_brush() {
        let missing_brush = BrushId::new(99);
        let mut runtime = BrushThreadRuntime::spawn(
            ToolSet::default_brush(),
            ActiveTool::Brush(missing_brush),
            BrushSettings::default(),
            8,
        );

        let error = runtime.begin_active_stroke_processing().unwrap_err();

        assert_eq!(
            error,
            BrushThreadRuntimeError::ActiveToolUnavailable(ActiveTool::Brush(missing_brush))
        );
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
        let (_overflow_producer, overflow_consumer) = create_overwrite_ring(16);

        super::run_brush_thread(
            BrushWorker::new(
                ToolSet::default_brush(),
                ActiveTool::Brush(BrushId::DEFAULT),
                BrushSettings::default(),
            ),
            command_receiver,
            overflow_consumer,
            16,
        );
        let finished = response_receiver.recv().unwrap().unwrap();

        assert_eq!(finished.inputs().len(), 1);
        assert_eq!(finished.inputs()[0].position, CanvasCoordF::new(3.0, 3.0));
    }

    #[test]
    fn brush_thread_batch_clears_queued_inputs_on_reset() {
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
            .send(BrushThreadCommand::Reset { epoch: 2 })
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
        let (_overflow_producer, overflow_consumer) = create_overwrite_ring(16);

        super::run_brush_thread(
            BrushWorker::new(
                ToolSet::default_brush(),
                ActiveTool::Brush(BrushId::DEFAULT),
                BrushSettings::default(),
            ),
            command_receiver,
            overflow_consumer,
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
        let (_overflow_producer, overflow_consumer) = create_overwrite_ring(16);

        super::run_brush_thread(
            BrushWorker::new(
                ToolSet::default_brush(),
                ActiveTool::Brush(BrushId::DEFAULT),
                BrushSettings::default(),
            ),
            command_receiver,
            overflow_consumer,
            16,
        );
        let finished = response_receiver.recv().unwrap().unwrap();
        let samples = finished.replace_circle_samples();

        assert_eq!(finished.inputs().len(), 1);
        assert_eq!(finished.inputs()[0].position, CanvasCoordF::new(3.0, 3.0));
        assert_eq!(samples[0].radius_px, 4.0);
    }

    #[test]
    fn brush_thread_overflow_input_ring_keeps_newest_inputs() {
        let (overflow_producer, overflow_consumer) = create_overwrite_ring(2);
        let mut overflow_batch = Vec::new();
        let mut canvas_batch = Vec::new();

        overflow_producer.push(QueuedCanvasInput {
            epoch: 1,
            input: canvas_input(1, 1.0, 1.0, 1.0),
        });
        overflow_producer.push(QueuedCanvasInput {
            epoch: 1,
            input: canvas_input(2, 2.0, 2.0, 1.0),
        });
        overflow_producer.push(QueuedCanvasInput {
            epoch: 1,
            input: canvas_input(3, 3.0, 3.0, 1.0),
        });
        super::drain_overflow_inputs(
            &overflow_consumer,
            &mut overflow_batch,
            16,
            super::WorkerStrokeState::Active { epoch: 1 },
            &mut canvas_batch,
        );

        assert_eq!(overflow_producer.pushed_items(), 3);
        assert_eq!(overflow_producer.dropped_items(), 1);
        assert_eq!(canvas_batch.len(), 2);
        assert_eq!(canvas_batch[0].position, CanvasCoordF::new(2.0, 2.0));
        assert_eq!(canvas_batch[1].position, CanvasCoordF::new(3.0, 3.0));
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

        runtime
            .set_active_tool(ActiveTool::Brush(second_brush))
            .unwrap();
        assert_eq!(runtime.active_brush_id(), Some(second_brush));
        assert!(runtime.begin_active_stroke());
        runtime.push_canvas_input(canvas_input(0, 1.0, 2.0, 1.0));
        let finished = runtime.finish_active_stroke().unwrap();

        assert_eq!(finished.brush_id(), second_brush);
    }

    #[test]
    fn brush_thread_runtime_rejects_unregistered_tool_switch() {
        let missing_brush = BrushId::new(99);
        let mut runtime = BrushThreadRuntime::spawn(
            ToolSet::default_brush(),
            ActiveTool::Brush(BrushId::DEFAULT),
            BrushSettings::default(),
            8,
        );

        let error = runtime
            .set_active_tool(ActiveTool::Brush(missing_brush))
            .unwrap_err();

        assert_eq!(
            error,
            BrushThreadRuntimeError::ActiveToolUnavailable(ActiveTool::Brush(missing_brush))
        );
        assert_eq!(runtime.active_brush_id(), Some(BrushId::DEFAULT));
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

    #[test]
    fn brush_thread_runtime_shutdown_joins_worker_thread() {
        let runtime = BrushThreadRuntime::spawn(
            ToolSet::default_brush(),
            ActiveTool::Brush(BrushId::DEFAULT),
            BrushSettings::default(),
            8,
        );

        runtime.shutdown().unwrap();
    }
}
