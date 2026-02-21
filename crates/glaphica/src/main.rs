use std::path::PathBuf;
use std::sync::Arc;

use brush_execution::{
    BrushExecutionCommandReceiver, BrushExecutionConfig, BrushExecutionFeedbackSender,
    BrushExecutionRuntime, BrushExecutionSampleSender,
};
use driver::PointerEventPhase;
use frame_scheduler::{FrameScheduler, FrameSchedulerDecision, FrameSchedulerInput};
use glaphica::GpuState;
use glaphica::driver_bridge::{DriverUiBridge, FrameDrainResult, FrameDrainStats};
use render_protocol::{
    BrushControlAck, BrushControlCommand, BrushProgramActivation, BrushProgramUpsert,
    ReferenceLayerSelection, ReferenceSetUpsert,
};
use winit::application::ApplicationHandler;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowAttributes, WindowId};

const ROTATION_RADIANS_PER_PIXEL: f32 = 0.01;
const WHEEL_ZOOM_SPEED: f32 = 0.1;
const PIXELS_PER_SCROLL_LINE: f32 = 120.0;
const DRIVER_QUEUE_CAPACITY: usize = 64;
const DRIVER_RESAMPLE_SPACING_PIXELS: f32 = 2.0;
const BRUSH_EXECUTION_INPUT_QUEUE_CAPACITY: usize = 64;
const BRUSH_EXECUTION_OUTPUT_QUEUE_CAPACITY: usize = 256;
const BRUSH_BUFFER_TILE_SIZE: u32 = 128;
const BRUSH_MAX_AFFECTED_RADIUS: f32 = 64.0;
const LIFECYCLE_FLUSH_MAX_STEPS: usize = 64;
const LIFECYCLE_FLUSH_IDLE_BREAK_STEPS: usize = 2;
const DEFAULT_BRUSH_ID: u64 = 1;
const DEFAULT_PROGRAM_REVISION: u64 = 1;
const DEFAULT_REFERENCE_SET_ID: u64 = 1;
const DEFAULT_TARGET_LAYER_ID: u64 = 1;
const DEFAULT_PROGRAM_WGSL: &str = r#"
@compute @workgroup_size(1)
fn main() {
}
"#;

struct DriverDebugState {
    bridge: DriverUiBridge,
}

impl DriverDebugState {
    fn new() -> Self {
        let bridge = DriverUiBridge::new(DRIVER_QUEUE_CAPACITY, DRIVER_RESAMPLE_SPACING_PIXELS)
            .expect("create driver ui bridge");
        Self { bridge }
    }

    fn push_input(&mut self, phase: PointerEventPhase, x: f32, y: f32) {
        self.bridge
            .ingest_mouse_event(phase, x, y)
            .expect("ingest mouse event into driver bridge");
    }

    fn has_active_stroke(&self) -> bool {
        self.bridge.has_active_stroke()
    }

    fn drain_debug_output(
        &mut self,
        frame_sequence_id: u64,
    ) -> (FrameDrainStats, Vec<driver::FramedSampleChunk>) {
        let FrameDrainResult { stats, chunks } = self.bridge.drain_frame(frame_sequence_id);
        if stats.has_activity() {
            println!(
                "[driver] frame={} input(total={} down={} move={} up={} cancel={} hover={} handle_us={}) output(chunks={} samples={} discontinuity_chunks={} dropped_before={})",
                stats.frame_sequence_id,
                stats.input.total_events,
                stats.input.down_events,
                stats.input.move_events,
                stats.input.up_events,
                stats.input.cancel_events,
                stats.input.hover_events,
                stats.input.handle_time_micros_total,
                stats.output.chunk_count,
                stats.output.sample_count,
                stats.output.discontinuity_chunk_count,
                stats.output.dropped_chunk_count_before_total,
            );
        }
        for framed_chunk in &chunks {
            let chunk = &framed_chunk.chunk;
            let first_x = chunk.canvas_x().first().copied().unwrap_or(0.0);
            let first_y = chunk.canvas_y().first().copied().unwrap_or(0.0);
            let last_x = chunk.canvas_x().last().copied().unwrap_or(0.0);
            let last_y = chunk.canvas_y().last().copied().unwrap_or(0.0);
            println!(
                "[driver] chunk frame={} stroke={} samples={} start={} end={} discontinuity={} dropped_before={} first=({:.2},{:.2}) last=({:.2},{:.2})",
                framed_chunk.frame_sequence_id,
                chunk.stroke_session_id,
                chunk.sample_count(),
                chunk.starts_stroke,
                chunk.ends_stroke,
                chunk.discontinuity_before,
                chunk.dropped_chunk_count_before,
                first_x,
                first_y,
                last_x,
                last_y,
            );
        }
        (stats, chunks)
    }
}

impl Default for DriverDebugState {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Default)]
struct App {
    window: Option<Arc<Window>>,
    gpu: Option<GpuState>,
    startup_image_path: Option<PathBuf>,
    input_modifiers: InputModifierState,
    interaction_mode: InteractionMode,
    is_left_mouse_pressed: bool,
    last_cursor_position: Option<(f64, f64)>,
    driver_debug: DriverDebugState,
    brush_execution_runtime: Option<BrushExecutionRuntime>,
    brush_execution_sender: Option<BrushExecutionSampleSender>,
    brush_execution_feedback_sender: Option<BrushExecutionFeedbackSender>,
    brush_execution_receiver: Option<BrushExecutionCommandReceiver>,
    next_driver_frame_sequence_id: u64,
    frame_scheduler: FrameScheduler,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum InteractionMode {
    #[default]
    Draw,
    Pan,
    Rotate,
}

#[derive(Default)]
struct InputModifierState {
    is_space_pressed: bool,
    is_rotate_pressed: bool,
}

impl InputModifierState {
    fn interaction_mode(&self) -> InteractionMode {
        if self.is_space_pressed {
            InteractionMode::Pan
        } else if self.is_rotate_pressed {
            InteractionMode::Rotate
        } else {
            InteractionMode::Draw
        }
    }
}

impl App {
    fn window_id(&self) -> Option<WindowId> {
        self.window.as_ref().map(|w| w.id())
    }

    fn update_interaction_mode(&mut self) {
        self.interaction_mode = self.input_modifiers.interaction_mode();
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        event_loop.set_control_flow(ControlFlow::Wait);

        if self.window.is_some() {
            return;
        }

        let window = Arc::new(
            event_loop
                .create_window(
                    WindowAttributes::default()
                        .with_title("glaphica")
                        .with_inner_size(PhysicalSize::new(1280u32, 720u32)),
                )
                .expect("create window"),
        );

        let gpu = pollster::block_on(GpuState::new(
            window.clone(),
            self.startup_image_path.clone(),
        ));
        window.request_redraw();

        self.window = Some(window);
        self.gpu = Some(gpu);
        self.initialize_brush_execution();
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        if self.window_id() != Some(window_id) {
            return;
        }

        match &event {
            WindowEvent::CloseRequested => {
                self.flush_brush_pipeline_lifecycle();
                event_loop.exit();
            }
            WindowEvent::RedrawRequested => {
                self.render_frame(event_loop);
            }
            _ => {}
        }

        self.handle_input_event(&event);
        self.handle_pointer_event(&event);
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }
}

impl App {
    fn handle_input_event(&mut self, event: &WindowEvent) {
        match event {
            WindowEvent::KeyboardInput { event, .. } => {
                self.handle_keyboard_input(event);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                self.handle_mouse_wheel(delta);
            }
            WindowEvent::Resized(new_size) => {
                self.handle_resize(*new_size);
            }
            WindowEvent::Focused(has_focus) => {
                if !has_focus {
                    self.flush_brush_pipeline_lifecycle();
                }
            }
            _ => {}
        }
    }

    fn handle_pointer_event(&mut self, event: &WindowEvent) {
        match event {
            WindowEvent::MouseInput { state, button, .. } => {
                self.handle_mouse_input(*state, *button);
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.handle_cursor_moved(*position);
            }
            _ => {}
        }
    }

    fn handle_keyboard_input(&mut self, event: &winit::event::KeyEvent) {
        let is_pressed = event.state == ElementState::Pressed;
        match event.physical_key {
            PhysicalKey::Code(KeyCode::Space) => {
                self.input_modifiers.is_space_pressed = is_pressed;
                self.update_interaction_mode();
            }
            PhysicalKey::Code(KeyCode::KeyR) => {
                self.input_modifiers.is_rotate_pressed = is_pressed;
                self.update_interaction_mode();
            }
            _ => {}
        }
    }

    fn handle_mouse_input(&mut self, state: ElementState, button: MouseButton) {
        if button != MouseButton::Left {
            return;
        }

        self.is_left_mouse_pressed = state == ElementState::Pressed;
        if self.is_left_mouse_pressed {
            if self.interaction_mode == InteractionMode::Draw {
                if let Some((cursor_x, cursor_y)) = self.last_cursor_position {
                    self.driver_debug.push_input(
                        PointerEventPhase::Down,
                        cursor_x as f32,
                        cursor_y as f32,
                    );
                }
            }
        } else {
            if self.driver_debug.has_active_stroke() {
                if let Some((cursor_x, cursor_y)) = self.last_cursor_position {
                    self.driver_debug.push_input(
                        PointerEventPhase::Up,
                        cursor_x as f32,
                        cursor_y as f32,
                    );
                }
            }
            self.last_cursor_position = None;
        }
    }

    fn handle_cursor_moved(&mut self, position: PhysicalPosition<f64>) {
        if self.is_left_mouse_pressed {
            if let Some((last_x, last_y)) = self.last_cursor_position {
                let delta_x = (position.x - last_x) as f32;
                let delta_y = (position.y - last_y) as f32;

                if let Some(gpu) = self.gpu.as_mut() {
                    match self.interaction_mode {
                        InteractionMode::Pan => {
                            gpu.pan_canvas(delta_x, delta_y);
                        }
                        InteractionMode::Rotate => {
                            gpu.rotate_canvas(delta_x * ROTATION_RADIANS_PER_PIXEL);
                        }
                        InteractionMode::Draw => {
                            self.driver_debug.push_input(
                                PointerEventPhase::Move,
                                position.x as f32,
                                position.y as f32,
                            );
                        }
                    }
                }
            }
        }
        self.last_cursor_position = Some((position.x, position.y));

        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    fn handle_mouse_wheel(&mut self, delta: &MouseScrollDelta) {
        let scroll_lines = match delta {
            MouseScrollDelta::LineDelta(_, vertical_lines) => *vertical_lines,
            MouseScrollDelta::PixelDelta(physical_position) => {
                (physical_position.y as f32) / PIXELS_PER_SCROLL_LINE
            }
        };
        let zoom_factor = (scroll_lines * WHEEL_ZOOM_SPEED).exp();
        let (anchor_x, anchor_y) = if let Some((cursor_x, cursor_y)) = self.last_cursor_position {
            (cursor_x as f32, cursor_y as f32)
        } else if let Some(window) = self.window.as_ref() {
            let size = window.inner_size();
            (size.width as f32 * 0.5, size.height as f32 * 0.5)
        } else {
            (0.0, 0.0)
        };
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.zoom_canvas_about_viewport_point(zoom_factor, anchor_x, anchor_y);
        }
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    fn handle_resize(&mut self, new_size: PhysicalSize<u32>) {
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.resize(new_size);
        }
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    fn drain_brush_pipeline(
        driver_debug: &mut DriverDebugState,
        brush_execution_sender: &mut Option<BrushExecutionSampleSender>,
        brush_execution_receiver: &mut Option<BrushExecutionCommandReceiver>,
        frame_sequence_id: u64,
        gpu: &mut GpuState,
    ) -> FrameDrainStats {
        let (frame_stats, sample_chunks) = driver_debug.drain_debug_output(frame_sequence_id);
        if let Some(sender) = brush_execution_sender.as_mut() {
            for sample_chunk in sample_chunks {
                sender
                    .push_chunk(sample_chunk)
                    .expect("push sample chunk into brush execution");
            }
        }
        if let Some(receiver) = brush_execution_receiver.as_mut() {
            while let Some(command) = receiver.pop_command() {
                gpu.enqueue_brush_render_command(command)
                    .expect("enqueue brush render command");
            }
        }
        frame_stats
    }

    fn render_frame(&mut self, event_loop: &ActiveEventLoop) {
        let Some(gpu) = self.gpu.as_mut() else {
            return;
        };

        let frame_sequence_id = self.next_driver_frame_sequence_id;
        let previous_frame_gpu_micros = gpu
            .take_latest_gpu_timing_report()
            .map(|report| report.gpu_time_micros);
        let frame_stats = Self::drain_brush_pipeline(
            &mut self.driver_debug,
            &mut self.brush_execution_sender,
            &mut self.brush_execution_receiver,
            frame_sequence_id,
            gpu,
        );
        let brush_hot_path_active = self.driver_debug.has_active_stroke()
            || frame_stats.input.total_events > 0
            || frame_stats.output.chunk_count > 0;
        let pending_brush_commands =
            u32::try_from(gpu.pending_brush_dab_count()).unwrap_or(u32::MAX);
        let scheduler_decision = self.frame_scheduler.schedule_frame(FrameSchedulerInput {
            frame_sequence_id,
            brush_hot_path_active,
            pending_brush_commands,
            previous_frame_gpu_micros,
        });
        apply_frame_scheduler_decision(gpu, scheduler_decision);
        gpu.process_renderer_merge_completions(frame_sequence_id)
            .expect("process renderer merge completions");
        if let Some(feedback_sender) = self.brush_execution_feedback_sender.as_mut() {
            for feedback in gpu.drain_brush_execution_merge_feedbacks() {
                feedback_sender
                    .push_feedback(feedback)
                    .expect("send merge feedback into brush execution");
            }
        }
        if let Some(receiver) = self.brush_execution_receiver.as_mut() {
            while let Some(command) = receiver.pop_command() {
                gpu.enqueue_brush_render_command(command)
                    .expect("enqueue brush render command after merge feedback");
            }
        }
        self.next_driver_frame_sequence_id = self
            .next_driver_frame_sequence_id
            .checked_add(1)
            .expect("driver frame sequence id overflow");

        match gpu.render() {
            Ok(()) => {}
            Err(wgpu::SurfaceError::Outdated | wgpu::SurfaceError::Lost) => {
                if let Some(window) = self.window.as_ref() {
                    gpu.resize(window.inner_size());
                    window.request_redraw();
                }
            }
            Err(wgpu::SurfaceError::Timeout) => {
                if let Some(window) = self.window.as_ref() {
                    window.request_redraw();
                }
            }
            Err(wgpu::SurfaceError::OutOfMemory) => {
                event_loop.exit();
            }
            Err(_) => {
                if let Some(window) = self.window.as_ref() {
                    window.request_redraw();
                }
            }
        }
    }

    fn flush_brush_pipeline_lifecycle(&mut self) {
        let Some(gpu) = self.gpu.as_mut() else {
            return;
        };

        let mut consecutive_idle_steps = 0usize;
        for _ in 0..LIFECYCLE_FLUSH_MAX_STEPS {
            let frame_sequence_id = self.next_driver_frame_sequence_id;

            let frame_stats = Self::drain_brush_pipeline(
                &mut self.driver_debug,
                &mut self.brush_execution_sender,
                &mut self.brush_execution_receiver,
                frame_sequence_id,
                gpu,
            );
            gpu.process_renderer_merge_completions(frame_sequence_id)
                .expect("process renderer merge completions during lifecycle flush");
            let mut drained_merge_feedback_count = 0u32;
            if let Some(feedback_sender) = self.brush_execution_feedback_sender.as_mut() {
                for feedback in gpu.drain_brush_execution_merge_feedbacks() {
                    drained_merge_feedback_count = drained_merge_feedback_count
                        .checked_add(1)
                        .expect("lifecycle flush drained merge feedback count overflow");
                    feedback_sender
                        .push_feedback(feedback)
                        .expect("send merge feedback during lifecycle flush");
                }
            }

            let mut drained_commands_after_feedback = 0u32;
            if let Some(receiver) = self.brush_execution_receiver.as_mut() {
                while let Some(command) = receiver.pop_command() {
                    drained_commands_after_feedback = drained_commands_after_feedback
                        .checked_add(1)
                        .expect("lifecycle flush drained command count overflow");
                    gpu.enqueue_brush_render_command(command)
                        .expect("enqueue brush render command during lifecycle flush");
                }
            }

            let has_activity = frame_stats.has_activity()
                || drained_merge_feedback_count > 0
                || drained_commands_after_feedback > 0
                || gpu.pending_brush_dab_count() > 0;

            self.next_driver_frame_sequence_id = self
                .next_driver_frame_sequence_id
                .checked_add(1)
                .expect("driver frame sequence id overflow");
            if has_activity {
                consecutive_idle_steps = 0;
            } else {
                consecutive_idle_steps = consecutive_idle_steps
                    .checked_add(1)
                    .expect("lifecycle flush idle step overflow");
                if consecutive_idle_steps >= LIFECYCLE_FLUSH_IDLE_BREAK_STEPS {
                    break;
                }
            }
        }
    }

    fn initialize_brush_execution(&mut self) {
        let Some(gpu) = self.gpu.as_mut() else {
            return;
        };
        let upsert_ack = gpu
            .apply_brush_control_command(BrushControlCommand::UpsertBrushProgram(
                BrushProgramUpsert {
                    brush_id: DEFAULT_BRUSH_ID,
                    program_revision: DEFAULT_PROGRAM_REVISION,
                    payload_hash: fxhash(DEFAULT_PROGRAM_WGSL),
                    wgsl_source: Arc::<str>::from(DEFAULT_PROGRAM_WGSL),
                },
            ))
            .expect("prepare default brush program");
        assert!(
            matches!(
                upsert_ack,
                BrushControlAck::Prepared | BrushControlAck::AlreadyPrepared
            ),
            "unexpected upsert ack: {upsert_ack:?}"
        );
        let activate_ack = gpu
            .apply_brush_control_command(BrushControlCommand::ActivateBrushProgram(
                BrushProgramActivation {
                    brush_id: DEFAULT_BRUSH_ID,
                    program_revision: DEFAULT_PROGRAM_REVISION,
                },
            ))
            .expect("activate default brush program");
        assert_eq!(activate_ack, BrushControlAck::Activated);
        let reference_ack = gpu
            .apply_brush_control_command(BrushControlCommand::UpsertReferenceSet(
                ReferenceSetUpsert {
                    reference_set_id: DEFAULT_REFERENCE_SET_ID,
                    selection: ReferenceLayerSelection::CurrentLayer,
                },
            ))
            .expect("upsert default reference set");
        assert_eq!(reference_ack, BrushControlAck::ReferenceSetUpserted);

        let (runtime, sender, feedback_sender, receiver) = BrushExecutionRuntime::start(
            BrushExecutionConfig {
                brush_id: DEFAULT_BRUSH_ID,
                program_revision: DEFAULT_PROGRAM_REVISION,
                reference_set_id: DEFAULT_REFERENCE_SET_ID,
                target_layer_id: DEFAULT_TARGET_LAYER_ID,
                buffer_tile_size: BRUSH_BUFFER_TILE_SIZE,
                max_affected_radius: BRUSH_MAX_AFFECTED_RADIUS,
            },
            BRUSH_EXECUTION_INPUT_QUEUE_CAPACITY,
            BRUSH_EXECUTION_OUTPUT_QUEUE_CAPACITY,
        )
        .expect("start brush execution runtime");
        self.brush_execution_runtime = Some(runtime);
        self.brush_execution_sender = Some(sender);
        self.brush_execution_feedback_sender = Some(feedback_sender);
        self.brush_execution_receiver = Some(receiver);
    }
}

fn fxhash(input: &str) -> u64 {
    use std::hash::{Hash, Hasher};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    input.hash(&mut hasher);
    hasher.finish()
}

fn main() {
    let event_loop = EventLoop::new().expect("create event loop");
    let mut app = App {
        startup_image_path: parse_startup_image_path(),
        driver_debug: DriverDebugState::new(),
        frame_scheduler: FrameScheduler::default(),
        ..App::default()
    };
    event_loop.run_app(&mut app).expect("run app");
}

fn apply_frame_scheduler_decision(gpu: &GpuState, decision: FrameSchedulerDecision) {
    let Some(max_commands) = decision.brush_commands_to_render else {
        return;
    };
    gpu.set_brush_command_quota(max_commands);
    println!(
        "[frame_scheduler] frame={} active={} brush_commands={} reason={:?}",
        decision.frame_sequence_id, decision.scheduler_active, max_commands, decision.update_reason,
    );
}

fn parse_startup_image_path() -> Option<PathBuf> {
    let mut args = std::env::args_os();
    let _program = args.next();

    let Some(first_arg) = args.next() else {
        return None;
    };

    if first_arg == "--image" {
        let image_path = args.next().unwrap_or_else(|| {
            panic!("missing image path after --image; usage: glaphica [--image <path>] | [<path>]")
        });
        assert!(
            args.next().is_none(),
            "too many arguments; usage: glaphica [--image <path>] | [<path>]"
        );
        return Some(PathBuf::from(image_path));
    }

    assert!(
        args.next().is_none(),
        "too many arguments; usage: glaphica [--image <path>] | [<path>]"
    );
    Some(PathBuf::from(first_arg))
}
