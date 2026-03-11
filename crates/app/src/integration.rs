use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use brushes::{BrushResamplerDistance, BrushResamplerDistancePolicy, BrushSpec};
use document::{Document, SharedRenderTree};
use glaphica_core::{AtlasLayout, BrushId, NodeId, StrokeId};
use gpu_runtime::surface_runtime::SurfaceRuntime;
use images::layout::ImageLayout;
use thread_protocol::{
    DrawFrameMergePolicy, GpuCmdFrameMergeTag, GpuCmdMsg, GpuFeedbackFrame, InputControlEvent,
    InputControlOp, InputRingSample, MergeItem, MergeVecIndex, TileKey,
};
use threads::{EngineThreadChannels, MainThreadChannels, create_thread_channels};

use crate::trace::{TraceInputFrame, TraceIoError, TraceRecorder};
use crate::{BrushRegisterError, EngineThreadState, MainThreadState, config};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrokeControl {
    pub node_id: NodeId,
    pub begin: bool,
}

impl InputControlOp for StrokeControl {
    type Target = Option<NodeId>;

    fn apply(&self, target: &mut Self::Target) {
        if self.begin {
            *target = Some(self.node_id);
        } else {
            *target = None;
        }
    }

    fn undo(&self, target: &mut Self::Target) {
        if self.begin {
            *target = None;
        } else {
            *target = Some(self.node_id);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TileAllocReceipt {
    pub old_tile_key: TileKey,
    pub new_tile_key: TileKey,
}

impl MergeItem for TileAllocReceipt {
    type MergeKey = TileKey;

    fn merge_key(&self) -> Self::MergeKey {
        self.old_tile_key
    }

    fn merge_duplicate(existing: &mut Self, incoming: Self) {
        *existing = incoming;
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GpuError {
    pub key: u64,
    pub message: String,
}

impl MergeItem for GpuError {
    type MergeKey = u64;

    fn merge_key(&self) -> Self::MergeKey {
        self.key
    }

    fn merge_duplicate(_existing: &mut Self, _incoming: Self) {}
}

type AppMainThreadChannels = MainThreadChannels<StrokeControl, TileAllocReceipt, GpuError>;
type AppEngineThreadChannels = EngineThreadChannels<StrokeControl, TileAllocReceipt, GpuError>;
type AppGpuFeedbackFrame = GpuFeedbackFrame<TileAllocReceipt, GpuError>;

struct AppGpuFeedbackMergeState {
    receipt_index: MergeVecIndex<TileKey>,
    error_index: MergeVecIndex<u64>,
}

impl Default for AppGpuFeedbackMergeState {
    fn default() -> Self {
        Self {
            receipt_index: MergeVecIndex::default(),
            error_index: MergeVecIndex::default(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct PerfTraceConfig {
    enabled: bool,
    slow_threshold: Duration,
}

impl PerfTraceConfig {
    fn from_env() -> Self {
        let enabled = std::env::var("GLAPHICA_DEBUG_PIPELINE_TRACE")
            .ok()
            .is_some_and(|value| value != "0");
        let slow_threshold_ms = std::env::var("GLAPHICA_DEBUG_PIPELINE_SLOW_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(4);
        Self {
            enabled,
            slow_threshold: Duration::from_millis(slow_threshold_ms),
        }
    }
}

#[derive(Default)]
struct EngineFramePerf {
    input_sample: Duration,
    smooth_and_resample: Duration,
    brush_handling: Duration,
    send_to_app_thread: Duration,
    send_inline_submit: Duration,
    accept_by_app_thread: Duration,
    submit_to_gpu: Duration,
    sample_count: usize,
    brush_input_count: usize,
    generated_gpu_command_count: usize,
    inline_submitted_gpu_command_count: usize,
    inline_submit_batches: usize,
    pending_send_gpu_command_count: usize,
    gpu_command_count: usize,
}

pub struct AppThreadIntegration {
    main_state: MainThreadState,
    engine_state: EngineThreadState,
    main_channels: AppMainThreadChannels,
    engine_channels: AppEngineThreadChannels,
    input_controls: Vec<InputControlEvent<StrokeControl>>,
    input_samples: Vec<InputRingSample>,
    brush_inputs: Vec<glaphica_core::BrushInput>,
    gpu_commands: Vec<thread_protocol::GpuCmdMsg>,
    pending_send_gpu_commands: VecDeque<thread_protocol::GpuCmdMsg>,
    feedback_merge_state: AppGpuFeedbackMergeState,
    trace_recorder: Option<TraceRecorder>,
    active_stroke_node: Option<NodeId>,
    current_brush_id: Option<BrushId>,
    brush_resampler_distances: Vec<Option<BrushResamplerDistance>>,
    next_stroke_id: u64,
    perf_trace: PerfTraceConfig,
    perf_frame_seq: u64,
    document_layout: ImageLayout,
}

impl AppThreadIntegration {
    fn should_merge_draw_in_frame(draw_op: &thread_protocol::DrawOp) -> bool {
        draw_op.frame_merge == DrawFrameMergePolicy::KeepLastInFrameByNodeTileBrush
    }

    fn should_keep_first_copy_in_frame(copy_op: &thread_protocol::CopyOp) -> bool {
        copy_op.frame_merge == GpuCmdFrameMergeTag::KeepFirstInFrameByDstTile
    }

    fn should_keep_last_write_in_frame(write_op: &thread_protocol::WriteOp) -> bool {
        write_op.frame_merge == GpuCmdFrameMergeTag::KeepLastInFrameByDstTile
    }

    fn compact_frame_mergeable_draws(commands: &mut Vec<GpuCmdMsg>) {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        struct CompositeKey {
            node_id: NodeId,
            tile_index: usize,
            brush_id: BrushId,
        }

        let mut latest_composite_indices: HashMap<CompositeKey, usize> = HashMap::new();
        for (index, cmd) in commands.iter().enumerate() {
            let GpuCmdMsg::DrawOp(draw_op) = cmd else {
                continue;
            };
            if !Self::should_merge_draw_in_frame(draw_op) {
                continue;
            }
            latest_composite_indices.insert(
                CompositeKey {
                    node_id: draw_op.node_id,
                    tile_index: draw_op.tile_index,
                    brush_id: draw_op.brush_id,
                },
                index,
            );
        }

        if latest_composite_indices.is_empty() {
            return;
        }

        let mut compacted = Vec::with_capacity(commands.len());
        for (index, cmd) in commands.drain(..).enumerate() {
            let keep = match &cmd {
                GpuCmdMsg::DrawOp(draw_op) if Self::should_merge_draw_in_frame(draw_op) => {
                    latest_composite_indices
                        .get(&CompositeKey {
                            node_id: draw_op.node_id,
                            tile_index: draw_op.tile_index,
                            brush_id: draw_op.brush_id,
                        })
                        .copied()
                        == Some(index)
                }
                _ => true,
            };
            if keep {
                compacted.push(cmd);
            }
        }
        *commands = compacted;
    }

    fn compact_frame_mergeable_copy_write(commands: &mut Vec<GpuCmdMsg>) {
        let mut first_copy_indices: HashMap<TileKey, usize> = HashMap::new();
        let mut last_write_indices: HashMap<TileKey, usize> = HashMap::new();

        for (index, cmd) in commands.iter().enumerate() {
            match cmd {
                GpuCmdMsg::CopyOp(copy_op) if Self::should_keep_first_copy_in_frame(copy_op) => {
                    first_copy_indices
                        .entry(copy_op.dst_tile_key)
                        .or_insert(index);
                }
                GpuCmdMsg::WriteOp(write_op) if Self::should_keep_last_write_in_frame(write_op) => {
                    last_write_indices.insert(write_op.dst_tile_key, index);
                }
                _ => {}
            }
        }

        if first_copy_indices.is_empty() && last_write_indices.is_empty() {
            return;
        }

        let mut compacted = Vec::with_capacity(commands.len());
        for (index, cmd) in commands.drain(..).enumerate() {
            let keep = match &cmd {
                GpuCmdMsg::CopyOp(copy_op) if Self::should_keep_first_copy_in_frame(copy_op) => {
                    first_copy_indices.get(&copy_op.dst_tile_key).copied() == Some(index)
                }
                GpuCmdMsg::WriteOp(write_op) if Self::should_keep_last_write_in_frame(write_op) => {
                    last_write_indices.get(&write_op.dst_tile_key).copied() == Some(index)
                }
                _ => true,
            };
            if keep {
                compacted.push(cmd);
            }
        }
        *commands = compacted;
    }

    fn move_mergeable_writes_to_end(commands: &mut Vec<GpuCmdMsg>) {
        let mut non_writes = Vec::with_capacity(commands.len());
        let mut deferred_writes = Vec::new();

        for cmd in commands.drain(..) {
            match &cmd {
                // After frame compaction only the final write to each destination remains.
                // Delaying these writes preserves the final image while keeping the buffered
                // stroke draw phase contiguous enough for GPU batching.
                GpuCmdMsg::WriteOp(write_op) if Self::should_keep_last_write_in_frame(write_op) => {
                    deferred_writes.push(cmd);
                }
                _ => non_writes.push(cmd),
            }
        }

        if deferred_writes.is_empty() {
            *commands = non_writes;
            return;
        }

        non_writes.extend(deferred_writes);
        *commands = non_writes;
    }

    fn move_setup_ops_before_draws(commands: &mut Vec<GpuCmdMsg>) {
        let mut setup_ops = Vec::with_capacity(commands.len());
        let mut draw_ops = Vec::new();
        let mut other_ops = Vec::new();

        for cmd in commands.drain(..) {
            match &cmd {
                // Buffered stroke setup must finish before the batched draw phase starts, otherwise
                // buffer clears and origin copies would race with packed round draws.
                GpuCmdMsg::ClearOp(_) => setup_ops.push(cmd),
                GpuCmdMsg::CopyOp(copy_op) if Self::should_keep_first_copy_in_frame(copy_op) => {
                    setup_ops.push(cmd);
                }
                GpuCmdMsg::DrawOp(_) => draw_ops.push(cmd),
                _ => other_ops.push(cmd),
            }
        }

        if draw_ops.is_empty() {
            setup_ops.extend(other_ops);
            *commands = setup_ops;
            return;
        }

        setup_ops.extend(draw_ops);
        setup_ops.extend(other_ops);
        *commands = setup_ops;
    }

    fn move_metadata_updates_to_end(commands: &mut Vec<GpuCmdMsg>) {
        let mut gpu_commands = Vec::with_capacity(commands.len());
        let mut deferred_updates = Vec::new();

        for cmd in commands.drain(..) {
            match &cmd {
                GpuCmdMsg::TileSlotKeyUpdate(_) | GpuCmdMsg::RenderTreeUpdated(_) => {
                    deferred_updates.push(cmd);
                }
                _ => gpu_commands.push(cmd),
            }
        }

        if deferred_updates.is_empty() {
            *commands = gpu_commands;
            return;
        }

        gpu_commands.extend(deferred_updates);
        *commands = gpu_commands;
    }

    pub async fn new(document_name: String, layout: ImageLayout) -> Result<Self, crate::InitError> {
        let mut main_state = MainThreadState::init().await?;

        let document = Document::new(
            document_name,
            layout,
            glaphica_core::BackendId::new(0),
            glaphica_core::BackendId::new(1),
        )
        .map_err(crate::InitError::Document)?;

        let initial_tree = document
            .build_flat_render_tree(glaphica_core::RenderTreeGeneration(0))
            .map_err(crate::InitError::Document)?;
        let shared_tree = Arc::new(SharedRenderTree::new(initial_tree));

        main_state.set_shared_tree(shared_tree.clone());

        let mut engine_state = EngineThreadState::new(
            document,
            shared_tree,
            crate::config::brush_processing::MAX_BRUSHES,
        );

        // Add backends to the engine thread's backend manager to match main thread
        engine_state
            .backend_manager_mut()
            .add_backend(AtlasLayout::Small11)
            .expect("failed to add leaf backend to engine");
        engine_state
            .backend_manager_mut()
            .add_backend(AtlasLayout::Small11)
            .expect("failed to add branch cache backend to engine");

        let (main_channels, engine_channels) = create_thread_channels(
            config::thread_channels::MAIN_TO_ENGINE_INPUT_RING,
            config::thread_channels::ENGINE_TO_MAIN_INPUT_CONTROL,
            config::thread_channels::ENGINE_TO_MAIN_GPU_COMMAND,
            config::thread_channels::MAIN_TO_ENGINE_FEEDBACK,
        );

        Ok(Self {
            main_state,
            engine_state,
            main_channels,
            engine_channels,
            input_controls: Vec::with_capacity(config::batch_capacities::INPUT_SAMPLES),
            input_samples: Vec::with_capacity(config::batch_capacities::INPUT_SAMPLES),
            brush_inputs: Vec::with_capacity(config::batch_capacities::BRUSH_INPUTS),
            gpu_commands: Vec::with_capacity(config::batch_capacities::GPU_COMMANDS),
            pending_send_gpu_commands: VecDeque::with_capacity(
                config::batch_capacities::GPU_COMMANDS,
            ),
            feedback_merge_state: AppGpuFeedbackMergeState::default(),
            trace_recorder: None,
            active_stroke_node: None,
            current_brush_id: None,
            brush_resampler_distances: vec![None; config::brush_processing::MAX_BRUSHES],
            next_stroke_id: 1,
            perf_trace: PerfTraceConfig::from_env(),
            perf_frame_seq: 0,
            document_layout: layout,
        })
    }

    pub fn document_size(&self) -> (u32, u32) {
        (self.document_layout.size_x(), self.document_layout.size_y())
    }

    pub fn main_state(&self) -> &MainThreadState {
        &self.main_state
    }

    pub fn main_state_mut(&mut self) -> &mut MainThreadState {
        &mut self.main_state
    }

    pub fn engine_state(&self) -> &EngineThreadState {
        &self.engine_state
    }

    pub fn engine_state_mut(&mut self) -> &mut EngineThreadState {
        &mut self.engine_state
    }

    pub fn push_input_sample(&self, sample: InputRingSample) {
        self.main_channels.input_ring_producer.push(sample);
    }

    pub fn map_screen_to_document(&self, screen_x: f32, screen_y: f32) -> (f32, f32) {
        self.main_state
            .view()
            .screen_to_document(screen_x, screen_y)
    }

    pub fn pan_view(&mut self, dx: f32, dy: f32) {
        self.main_state.view_mut().pan(dx, dy);
    }

    pub fn zoom_view(&mut self, factor: f32, center_x: f32, center_y: f32) {
        self.main_state.view_mut().zoom(factor, center_x, center_y);
    }

    pub fn rotate_view(&mut self, delta_radians: f32, center_x: f32, center_y: f32) {
        self.main_state
            .view_mut()
            .rotate(delta_radians, center_x, center_y);
    }

    pub fn begin_stroke(&mut self, node_id: NodeId) {
        let stroke_id = StrokeId(self.next_stroke_id);
        self.next_stroke_id += 1;

        let control = StrokeControl {
            node_id,
            begin: true,
        };
        self.main_channels
            .input_control_queue
            .blocking_push(InputControlEvent::Control(control));
        self.active_stroke_node = Some(node_id);
        self.engine_state.begin_stroke(stroke_id);
    }

    pub fn set_active_brush(&mut self, brush_id: BrushId) {
        self.current_brush_id = Some(brush_id);
        let Some(brush_index) = usize::try_from(brush_id.0).ok() else {
            return;
        };
        let Some(Some(distance)) = self.brush_resampler_distances.get(brush_index).copied() else {
            return;
        };
        self.engine_state.set_resampler_distance(distance);
    }

    pub fn active_brush_id(&self) -> Option<BrushId> {
        self.current_brush_id
    }

    pub fn end_stroke(&mut self) {
        if let Some(node_id) = self.active_stroke_node {
            let control = StrokeControl {
                node_id,
                begin: false,
            };
            self.main_channels
                .input_control_queue
                .blocking_push(InputControlEvent::Control(control));
        }
        self.active_stroke_node = None;
        self.engine_state.end_stroke();
    }

    pub fn process_engine_frame(&mut self, wait_timeout: std::time::Duration) -> bool {
        let mut perf = EngineFramePerf::default();
        self.input_controls.clear();
        while let Ok(event) = self.engine_channels.input_control_queue.pop() {
            self.input_controls.push(event);
        }
        for event in &self.input_controls {
            event.apply(&mut self.active_stroke_node);
        }
        self.input_samples.clear();
        let input_sample_started = Instant::now();
        self.engine_channels
            .input_ring_consumer
            .drain_batch_with_wait(
                &mut self.input_samples,
                config::brush_processing::MAX_INPUT_BATCH_SIZE,
                wait_timeout,
            );
        self.normalize_input_sample_timestamps();
        perf.input_sample = input_sample_started.elapsed();
        perf.sample_count = self.input_samples.len();
        if let Some(trace_recorder) = &mut self.trace_recorder {
            trace_recorder.record_input_frame(&self.input_controls, &self.input_samples);
        }
        self.process_engine_frame_from_samples(Some(&mut perf))
    }

    pub fn process_replay_input_frame(&mut self, input_frame: &TraceInputFrame) -> bool {
        let mut perf = EngineFramePerf::default();
        let (controls, samples) = input_frame.to_runtime();
        self.input_controls = controls;
        self.input_samples = samples;
        perf.sample_count = self.input_samples.len();

        for event in &self.input_controls {
            let InputControlEvent::Control(control) = event;
            if control.begin {
                let stroke_id = StrokeId(self.next_stroke_id);
                self.next_stroke_id += 1;
                self.engine_state.begin_stroke(stroke_id);
            } else {
                self.engine_state.end_stroke();
            }
        }
        for event in &self.input_controls {
            event.apply(&mut self.active_stroke_node);
        }
        self.process_engine_frame_from_samples(Some(&mut perf))
    }

    fn process_engine_frame_from_samples(
        &mut self,
        mut perf: Option<&mut EngineFramePerf>,
    ) -> bool {
        let mut generated_gpu_command_count = 0usize;
        if !self.input_samples.is_empty()
            && let Some(node_id) = self.active_stroke_node
        {
            if let Some(brush_id) = self.current_brush_id {
                self.brush_inputs.clear();
                self.gpu_commands.clear();

                let smooth_and_resample_started = Instant::now();
                for sample in &self.input_samples {
                    let new_inputs = self
                        .engine_state
                        .process_raw_input(sample.cursor, sample.time_ns);
                    self.brush_inputs.extend(new_inputs);
                }
                if let Some(perf) = perf.as_deref_mut() {
                    perf.smooth_and_resample = smooth_and_resample_started.elapsed();
                    perf.brush_input_count = self.brush_inputs.len();
                }

                let brush_inputs = self.brush_inputs.clone();
                let brush_handling_started = Instant::now();
                for brush_input in &brush_inputs {
                    match self.engine_state.process_stroke_input(
                        brush_id,
                        brush_input,
                        node_id,
                        None,
                    ) {
                        Ok(cmds) => {
                            self.gpu_commands.extend(cmds);
                        }
                        Err(e) => {
                            eprintln!("Stroke processing failed: {e:?}");
                        }
                    }
                }
                if let Some(perf) = perf.as_deref_mut() {
                    perf.brush_handling = brush_handling_started.elapsed();
                }

                Self::compact_frame_mergeable_draws(&mut self.gpu_commands);
                Self::compact_frame_mergeable_copy_write(&mut self.gpu_commands);
                Self::move_setup_ops_before_draws(&mut self.gpu_commands);
                Self::move_mergeable_writes_to_end(&mut self.gpu_commands);
                Self::move_metadata_updates_to_end(&mut self.gpu_commands);

                let pending_gpu_cmds = std::mem::take(&mut self.gpu_commands);
                generated_gpu_command_count = pending_gpu_cmds.len();
                self.pending_send_gpu_commands.extend(pending_gpu_cmds);
            }
        }

        let send_to_app_thread_started = Instant::now();
        while self.engine_channels.gpu_command_sender.slots() > 0 {
            let Some(cmd) = self.pending_send_gpu_commands.front().cloned() else {
                break;
            };
            if let Err(e) = self.engine_channels.gpu_command_sender.push(cmd) {
                eprintln!("GPU command send failed: {e:?}");
                break;
            }
            self.pending_send_gpu_commands.pop_front();
        }
        if let Some(perf) = perf.as_deref_mut() {
            perf.send_to_app_thread = send_to_app_thread_started.elapsed();
            perf.send_inline_submit = Duration::ZERO;
            perf.generated_gpu_command_count = generated_gpu_command_count;
            perf.inline_submitted_gpu_command_count = 0;
            perf.inline_submit_batches = 0;
            perf.pending_send_gpu_command_count = self.pending_send_gpu_commands.len();
        }

        self.gpu_commands.clear();
        let accept_by_app_thread_started = Instant::now();
        while let Ok(cmd) = self.main_channels.gpu_command_receiver.pop() {
            self.gpu_commands.push(cmd);
        }
        if let Some(perf) = perf.as_deref_mut() {
            perf.accept_by_app_thread = accept_by_app_thread_started.elapsed();
            perf.gpu_command_count = self.gpu_commands.len();
        }

        if let Some(trace_recorder) = &mut self.trace_recorder {
            trace_recorder.record_output_frame(&self.gpu_commands);
        }

        let has_commands = !self.gpu_commands.is_empty();
        if has_commands {
            let submit_to_gpu_started = Instant::now();
            self.main_state.process_gpu_commands(&self.gpu_commands);
            if let Some(perf) = perf.as_deref_mut() {
                perf.submit_to_gpu = submit_to_gpu_started.elapsed();
            }
        }

        if let Some(perf) = perf {
            self.trace_engine_frame_perf(perf);
        }

        has_commands
    }

    fn trace_engine_frame_perf(&mut self, perf: &EngineFramePerf) {
        if !self.perf_trace.enabled {
            return;
        }
        let stages = [
            ("input_sample", perf.input_sample),
            ("smooth_and_resample", perf.smooth_and_resample),
            ("brush_handling", perf.brush_handling),
            ("send_to_app_thread", perf.send_to_app_thread),
            ("accept_by_app_thread", perf.accept_by_app_thread),
            ("submit_to_gpu", perf.submit_to_gpu),
        ];
        let total = stages
            .iter()
            .map(|(_, duration)| *duration)
            .fold(Duration::ZERO, |acc, item| acc + item);
        if total < self.perf_trace.slow_threshold {
            return;
        }
        let Some((bottleneck, bottleneck_duration)) =
            stages.iter().max_by_key(|(_, duration)| *duration)
        else {
            return;
        };
        self.perf_frame_seq += 1;
        eprintln!(
            "[PERF][pipeline][engine_frame={}] total_ms={:.3} bottleneck={} ({:.3}ms) samples={} brush_inputs={} gpu_cmds={} generated_gpu_cmds={} pending_send_gpu_cmds={} inline_submit_cmds={} inline_submit_batches={} inline_submit_ms={:.3} stages_ms={{input:{:.3}, smooth_resample:{:.3}, brush:{:.3}, send:{:.3}, accept:{:.3}, submit:{:.3}}}",
            self.perf_frame_seq,
            duration_ms(total),
            bottleneck,
            duration_ms(*bottleneck_duration),
            perf.sample_count,
            perf.brush_input_count,
            perf.gpu_command_count,
            perf.generated_gpu_command_count,
            perf.pending_send_gpu_command_count,
            perf.inline_submitted_gpu_command_count,
            perf.inline_submit_batches,
            duration_ms(perf.send_inline_submit),
            duration_ms(perf.input_sample),
            duration_ms(perf.smooth_and_resample),
            duration_ms(perf.brush_handling),
            duration_ms(perf.send_to_app_thread),
            duration_ms(perf.accept_by_app_thread),
            duration_ms(perf.submit_to_gpu),
        );
    }

    fn normalize_input_sample_timestamps(&mut self) {
        for sample in &mut self.input_samples {
            if sample.time_ns == 0 {
                sample.time_ns = current_time_ns();
            }
        }
    }

    pub fn enable_trace_recording(&mut self) {
        self.trace_recorder = Some(TraceRecorder::default());
    }

    pub fn save_trace_files(
        &self,
        input_path: Option<&std::path::Path>,
        output_path: Option<&std::path::Path>,
    ) -> Result<(), TraceIoError> {
        match &self.trace_recorder {
            Some(trace_recorder) => {
                if let Some(input_path) = input_path {
                    trace_recorder.save_input_file(input_path)?;
                }
                if let Some(output_path) = output_path {
                    trace_recorder.save_output_file(output_path)?;
                }
                Ok(())
            }
            None => Ok(()),
        }
    }

    pub fn process_main_render(&mut self) -> bool {
        if !self.perf_trace.enabled {
            return self.main_state.process_render();
        }
        let started = Instant::now();
        let has_work = self.main_state.process_render();
        let elapsed = started.elapsed();
        if elapsed >= self.perf_trace.slow_threshold {
            eprintln!(
                "[PERF][pipeline][submit_to_gpu_render] elapsed_ms={:.3} has_work={}",
                duration_ms(elapsed),
                has_work
            );
        }
        has_work
    }

    pub fn set_surface(&mut self, surface: SurfaceRuntime) {
        self.main_state.set_surface(surface);
    }

    pub fn resize_surface(&mut self, width: u32, height: u32) {
        self.main_state.resize_surface(width, height);
    }

    pub fn present_to_screen(&mut self) {
        let started = if self.perf_trace.enabled {
            Some(Instant::now())
        } else {
            None
        };
        if let Err(e) = self.main_state.present_to_screen() {
            eprintln!("Screen present failed: {e:?}");
        }
        if let Some(started) = started {
            let elapsed = started.elapsed();
            if elapsed >= self.perf_trace.slow_threshold {
                eprintln!(
                    "[PERF][pipeline][show_on_screen] elapsed_ms={:.3}",
                    duration_ms(elapsed)
                );
            }
        }
    }

    pub fn present_to_screen_with_overlay<F>(&mut self, overlay: F)
    where
        F: FnMut(
            &wgpu::Device,
            &wgpu::Queue,
            &mut wgpu::CommandEncoder,
            &wgpu::TextureView,
            wgpu::TextureFormat,
            u32,
            u32,
        ),
    {
        if let Err(e) = self.main_state.present_to_screen_with_overlay(overlay) {
            eprintln!("Screen present failed: {e:?}");
        }
    }

    pub fn save_screenshot(
        &mut self,
        output_path: &std::path::Path,
        width: u32,
        height: u32,
    ) -> Result<(), crate::ScreenshotError> {
        self.main_state.save_screenshot(output_path, width, height)
    }

    pub fn rebuild_render_tree(&mut self) -> Result<(), document::ImageCreateError> {
        let msg = self.engine_state.rebuild_render_tree()?;
        self.main_state
            .process_gpu_commands(&[thread_protocol::GpuCmdMsg::RenderTreeUpdated(msg)]);
        Ok(())
    }

    pub fn register_brush<S: BrushSpec + BrushResamplerDistancePolicy>(
        &mut self,
        brush_id: BrushId,
        brush: S,
    ) -> Result<(), BrushRegisterError> {
        let resampler_distance = brush.resampler_distance();
        let max_affected_radius_px = brush.max_affected_radius_px();

        let cache_backend_id = self.main_state.register_brush(brush_id, &brush)?;
        if let Some(cache_backend_id) = cache_backend_id {
            while self
                .engine_state
                .backend_manager()
                .backend(cache_backend_id)
                .is_none()
            {
                if self
                    .engine_state
                    .backend_manager_mut()
                    .add_backend(AtlasLayout::Small11)
                    .is_err()
                {
                    break;
                }
            }
        }

        self.engine_state
            .brush_runtime_mut()
            .register_pipeline_with_stroke_buffer_backend(
                brush_id,
                max_affected_radius_px,
                cache_backend_id,
                brush,
            )
            .map_err(BrushRegisterError::Engine)?;

        let Some(brush_index) = usize::try_from(brush_id.0).ok() else {
            return Ok(());
        };
        if let Some(slot) = self.brush_resampler_distances.get_mut(brush_index) {
            *slot = Some(resampler_distance);
        }

        Ok(())
    }

    pub fn update_brush<S: BrushSpec + BrushResamplerDistancePolicy>(
        &mut self,
        brush_id: BrushId,
        brush: S,
    ) -> Result<(), BrushRegisterError> {
        let resampler_distance = brush.resampler_distance();
        let max_affected_radius_px = brush.max_affected_radius_px();
        self.engine_state
            .brush_runtime_mut()
            .update_pipeline(brush_id, max_affected_radius_px, brush)
            .map_err(BrushRegisterError::Engine)?;

        let Some(brush_index) = usize::try_from(brush_id.0).ok() else {
            return Ok(());
        };
        if let Some(slot) = self.brush_resampler_distances.get_mut(brush_index) {
            *slot = Some(resampler_distance);
        }
        Ok(())
    }
}

fn duration_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

fn current_time_ns() -> u64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_nanos() as u64,
        Err(_) => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::AppThreadIntegration;
    use glaphica_core::{BrushId, NodeId, StrokeId, TileKey};
    use thread_protocol::{
        CopyOp, DrawBlendMode, DrawFrameMergePolicy, DrawOp, GpuCmdFrameMergeTag, GpuCmdMsg,
        WriteBlendMode, WriteOp,
    };

    #[test]
    fn compact_copy_and_write_keeps_first_copy_and_last_write() {
        let dst_tile = TileKey::from_parts(0, 0, 1);
        let buffer_tile = TileKey::from_parts(2, 0, 9);
        let copy = GpuCmdMsg::CopyOp(CopyOp {
            src_tile_key: TileKey::from_parts(0, 0, 7),
            dst_tile_key: dst_tile,
            frame_merge: GpuCmdFrameMergeTag::KeepFirstInFrameByDstTile,
        });
        let draw = |value| {
            GpuCmdMsg::DrawOp(DrawOp {
                node_id: NodeId(1),
                tile_index: 3,
                tile_key: buffer_tile,
                blend_mode: DrawBlendMode::Alpha,
                frame_merge: DrawFrameMergePolicy::None,
                origin_tile: TileKey::EMPTY,
                ref_image: None,
                input: vec![value],
                brush_id: BrushId(2),
                stroke_id: StrokeId(4),
            })
        };
        let write = |opacity| {
            GpuCmdMsg::WriteOp(WriteOp {
                src_tile_key: buffer_tile,
                dst_tile_key: dst_tile,
                blend_mode: WriteBlendMode::Normal,
                opacity,
                frame_merge: GpuCmdFrameMergeTag::KeepLastInFrameByDstTile,
            })
        };

        let mut commands = vec![copy, draw(1.0), write(0.2), draw(2.0), write(0.8)];
        AppThreadIntegration::compact_frame_mergeable_copy_write(&mut commands);

        assert_eq!(commands.len(), 4);
        assert!(matches!(commands[0], GpuCmdMsg::CopyOp(_)));
        assert!(matches!(commands[1], GpuCmdMsg::DrawOp(_)));
        assert!(matches!(commands[2], GpuCmdMsg::DrawOp(_)));
        let GpuCmdMsg::WriteOp(write_op) = &commands[3] else {
            panic!("expected final write op");
        };
        assert_eq!(write_op.opacity, 0.8);
    }

    #[test]
    fn move_mergeable_writes_and_updates_to_end_preserves_draw_phase() {
        let dst_tile = TileKey::from_parts(0, 0, 1);
        let buffer_tile = TileKey::from_parts(2, 0, 9);
        let mut commands = vec![
            GpuCmdMsg::CopyOp(CopyOp {
                src_tile_key: TileKey::from_parts(0, 0, 7),
                dst_tile_key: dst_tile,
                frame_merge: GpuCmdFrameMergeTag::KeepFirstInFrameByDstTile,
            }),
            GpuCmdMsg::DrawOp(DrawOp {
                node_id: NodeId(1),
                tile_index: 3,
                tile_key: buffer_tile,
                blend_mode: DrawBlendMode::Alpha,
                frame_merge: DrawFrameMergePolicy::None,
                origin_tile: TileKey::EMPTY,
                ref_image: None,
                input: vec![1.0],
                brush_id: BrushId(2),
                stroke_id: StrokeId(4),
            }),
            GpuCmdMsg::TileSlotKeyUpdate(thread_protocol::TileSlotKeyUpdateMsg {
                updates: vec![(NodeId(1), 3, dst_tile)],
            }),
            GpuCmdMsg::DrawOp(DrawOp {
                node_id: NodeId(2),
                tile_index: 4,
                tile_key: buffer_tile,
                blend_mode: DrawBlendMode::Alpha,
                frame_merge: DrawFrameMergePolicy::None,
                origin_tile: TileKey::EMPTY,
                ref_image: None,
                input: vec![2.0],
                brush_id: BrushId(2),
                stroke_id: StrokeId(4),
            }),
            GpuCmdMsg::WriteOp(WriteOp {
                src_tile_key: buffer_tile,
                dst_tile_key: dst_tile,
                blend_mode: WriteBlendMode::Normal,
                opacity: 0.8,
                frame_merge: GpuCmdFrameMergeTag::KeepLastInFrameByDstTile,
            }),
        ];

        AppThreadIntegration::move_mergeable_writes_to_end(&mut commands);
        AppThreadIntegration::move_metadata_updates_to_end(&mut commands);

        assert!(matches!(commands[0], GpuCmdMsg::CopyOp(_)));
        assert!(matches!(commands[1], GpuCmdMsg::DrawOp(_)));
        assert!(matches!(commands[2], GpuCmdMsg::DrawOp(_)));
        assert!(matches!(commands[3], GpuCmdMsg::WriteOp(_)));
        assert!(matches!(commands[4], GpuCmdMsg::TileSlotKeyUpdate(_)));
    }

    #[test]
    fn move_setup_ops_before_draws_builds_setup_draw_write_update_phases() {
        let dst_tile = TileKey::from_parts(0, 0, 1);
        let buffer_tile = TileKey::from_parts(2, 0, 9);
        let mut commands = vec![
            GpuCmdMsg::DrawOp(DrawOp {
                node_id: NodeId(1),
                tile_index: 3,
                tile_key: buffer_tile,
                blend_mode: DrawBlendMode::Alpha,
                frame_merge: DrawFrameMergePolicy::None,
                origin_tile: TileKey::EMPTY,
                ref_image: None,
                input: vec![1.0],
                brush_id: BrushId(2),
                stroke_id: StrokeId(4),
            }),
            GpuCmdMsg::CopyOp(CopyOp {
                src_tile_key: TileKey::from_parts(0, 0, 7),
                dst_tile_key: dst_tile,
                frame_merge: GpuCmdFrameMergeTag::KeepFirstInFrameByDstTile,
            }),
            GpuCmdMsg::ClearOp(thread_protocol::ClearOp {
                tile_key: buffer_tile,
            }),
            GpuCmdMsg::WriteOp(WriteOp {
                src_tile_key: buffer_tile,
                dst_tile_key: dst_tile,
                blend_mode: WriteBlendMode::Normal,
                opacity: 0.8,
                frame_merge: GpuCmdFrameMergeTag::KeepLastInFrameByDstTile,
            }),
            GpuCmdMsg::TileSlotKeyUpdate(thread_protocol::TileSlotKeyUpdateMsg {
                updates: vec![(NodeId(1), 3, dst_tile)],
            }),
        ];

        AppThreadIntegration::move_setup_ops_before_draws(&mut commands);
        AppThreadIntegration::move_mergeable_writes_to_end(&mut commands);
        AppThreadIntegration::move_metadata_updates_to_end(&mut commands);

        assert!(matches!(commands[0], GpuCmdMsg::CopyOp(_)));
        assert!(matches!(commands[1], GpuCmdMsg::ClearOp(_)));
        assert!(matches!(commands[2], GpuCmdMsg::DrawOp(_)));
        assert!(matches!(commands[3], GpuCmdMsg::WriteOp(_)));
        assert!(matches!(commands[4], GpuCmdMsg::TileSlotKeyUpdate(_)));
    }
}
