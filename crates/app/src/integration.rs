use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use brushes::BrushSpec;
use document::{Document, SharedRenderTree};
use glaphica_core::{AtlasLayout, BrushId, NodeId, StrokeId};
use gpu_runtime::surface_runtime::SurfaceRuntime;
use images::layout::ImageLayout;
use thread_protocol::{
    GpuFeedbackFrame, InputControlEvent, InputControlOp, InputRingSample, MergeItem,
    MergeVecIndex, TileKey,
};
use threads::{create_thread_channels, EngineThreadChannels, MainThreadChannels};

use crate::trace::{TraceInputFrame, TraceIoError, TraceRecorder};
use crate::{config, BrushRegisterError, EngineThreadState, MainThreadState};

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

pub struct AppThreadIntegration {
    main_state: MainThreadState,
    engine_state: EngineThreadState,
    main_channels: AppMainThreadChannels,
    engine_channels: AppEngineThreadChannels,
    input_controls: Vec<InputControlEvent<StrokeControl>>,
    input_samples: Vec<InputRingSample>,
    brush_inputs: Vec<glaphica_core::BrushInput>,
    gpu_commands: Vec<thread_protocol::GpuCmdMsg>,
    feedback_merge_state: AppGpuFeedbackMergeState,
    trace_recorder: Option<TraceRecorder>,
    active_stroke_node: Option<NodeId>,
    current_brush_id: Option<BrushId>,
    next_stroke_id: u64,
}

impl AppThreadIntegration {
    pub async fn new(
        document_name: String,
        layout: ImageLayout,
    ) -> Result<Self, crate::InitError> {
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

        let mut engine_state = EngineThreadState::new(document, shared_tree, crate::config::brush_processing::MAX_BRUSHES);

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
            feedback_merge_state: AppGpuFeedbackMergeState::default(),
            trace_recorder: None,
            active_stroke_node: None,
            current_brush_id: None,
            next_stroke_id: 1,
        })
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
        self.input_controls.clear();
        while let Ok(event) = self.engine_channels.input_control_queue.pop() {
            self.input_controls.push(event);
        }
        for event in &self.input_controls {
            event.apply(&mut self.active_stroke_node);
        }
        self.input_samples.clear();
        self.engine_channels.input_ring_consumer.drain_batch_with_wait(
            &mut self.input_samples,
            config::brush_processing::MAX_INPUT_BATCH_SIZE,
            wait_timeout,
        );
        self.normalize_input_sample_timestamps();
        if let Some(trace_recorder) = &mut self.trace_recorder {
            trace_recorder.record_input_frame(&self.input_controls, &self.input_samples);
        }
        self.process_engine_frame_from_samples()
    }

    pub fn process_replay_input_frame(&mut self, input_frame: &TraceInputFrame) -> bool {
        let (controls, samples) = input_frame.to_runtime();
        self.input_controls = controls;
        self.input_samples = samples;
        for event in &self.input_controls {
            event.apply(&mut self.active_stroke_node);
        }
        self.process_engine_frame_from_samples()
    }

    fn process_engine_frame_from_samples(&mut self) -> bool {
        if !self.input_samples.is_empty() && let Some(node_id) = self.active_stroke_node {
            if let Some(brush_id) = self.current_brush_id {
                self.brush_inputs.clear();

                for sample in &self.input_samples {
                    let new_inputs = self
                        .engine_state
                        .process_raw_input(sample.cursor, sample.time_ns);
                    self.brush_inputs.extend(new_inputs);
                }

                for brush_input in &self.brush_inputs {
                    match self.engine_state.process_stroke_input(
                        brush_id,
                        brush_input,
                        node_id,
                        None,
                    ) {
                        Ok(cmds) => {
                            for cmd in cmds {
                                if let Err(e) = self.engine_channels.gpu_command_sender.push(cmd) {
                                    eprintln!("GPU command send failed: {e:?}");
                                }
                            }
                        }
                        Err(e) => {
                            eprintln!("Stroke processing failed: {e:?}");
                        }
                    }
                }
            }
        }

        self.gpu_commands.clear();
        while let Ok(cmd) = self.main_channels.gpu_command_receiver.pop() {
            self.gpu_commands.push(cmd);
        }

        if let Some(trace_recorder) = &mut self.trace_recorder {
            trace_recorder.record_output_frame(&self.gpu_commands);
        }

        let has_commands = !self.gpu_commands.is_empty();
        if has_commands {
            self.main_state.process_gpu_commands(&self.gpu_commands);
        }

        has_commands
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
        self.main_state.process_render()
    }

    pub fn set_surface(&mut self, surface: SurfaceRuntime) {
        self.main_state.set_surface(surface);
    }

    pub fn resize_surface(&mut self, width: u32, height: u32) {
        self.main_state.resize_surface(width, height);
    }

    pub fn present_to_screen(&mut self) {
        if let Err(e) = self.main_state.present_to_screen() {
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
        self.main_state.process_gpu_commands(&[thread_protocol::GpuCmdMsg::RenderTreeUpdated(msg)]);
        Ok(())
    }

    pub fn register_brush<S: BrushSpec>(
        &mut self,
        brush_id: BrushId,
        brush: S,
    ) -> Result<(), BrushRegisterError> {
        let max_affected_radius_px = brush.max_affected_radius_px();
        
        self.main_state.register_brush(brush_id, &brush)?;
        
        self.engine_state
            .brush_runtime_mut()
            .register_pipeline(brush_id, max_affected_radius_px, brush)
            .map_err(BrushRegisterError::Engine)?;
        
        Ok(())
    }
}

fn current_time_ns() -> u64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_nanos() as u64,
        Err(_) => 0,
    }
}
