use std::ffi::OsStr;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

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
    BrushRenderCommand, ReferenceLayerSelection, ReferenceSetUpsert,
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

#[derive(Debug, Clone)]
enum RecordedInputEventKind {
    MouseInput { pressed: bool },
    CursorMoved { x: f64, y: f64 },
    MouseWheelLine { vertical_lines: f32 },
    MouseWheelPixel { delta_y: f64 },
}

#[derive(Debug, Clone)]
struct RecordedInputEvent {
    elapsed_micros: u64,
    kind: RecordedInputEventKind,
}

struct InputTraceRecorder {
    started_at: Instant,
    writer: BufWriter<File>,
}

impl InputTraceRecorder {
    fn from_path(path: PathBuf) -> Self {
        let file = File::create(&path).unwrap_or_else(|error| {
            panic!("create input trace file '{}': {error}", path.display())
        });
        Self {
            started_at: Instant::now(),
            writer: BufWriter::new(file),
        }
    }

    fn record(&mut self, kind: RecordedInputEventKind) {
        let elapsed_micros = self.started_at.elapsed().as_micros();
        let elapsed_micros = u64::try_from(elapsed_micros)
            .unwrap_or_else(|_| panic!("input trace timestamp overflow"));
        let line = match kind {
            RecordedInputEventKind::MouseInput { pressed } => {
                format!(
                    "{elapsed_micros}\tmouse_input\t{}",
                    if pressed { "1" } else { "0" }
                )
            }
            RecordedInputEventKind::CursorMoved { x, y } => {
                format!("{elapsed_micros}\tcursor_moved\t{x}\t{y}")
            }
            RecordedInputEventKind::MouseWheelLine { vertical_lines } => {
                format!("{elapsed_micros}\tmouse_wheel_line\t{vertical_lines}")
            }
            RecordedInputEventKind::MouseWheelPixel { delta_y } => {
                format!("{elapsed_micros}\tmouse_wheel_pixel\t{delta_y}")
            }
        };
        writeln!(self.writer, "{line}")
            .unwrap_or_else(|error| panic!("write input trace event failed: {error}"));
        self.writer
            .flush()
            .unwrap_or_else(|error| panic!("flush input trace event failed: {error}"));
    }
}

struct InputTraceReplay {
    started_at: Instant,
    events: Vec<RecordedInputEvent>,
    next_event_index: usize,
    completion_logged: bool,
}

impl InputTraceReplay {
    fn from_path(path: PathBuf) -> Self {
        let file = File::open(&path)
            .unwrap_or_else(|error| panic!("open input trace file '{}': {error}", path.display()));
        let reader = BufReader::new(file);
        let mut events = Vec::new();
        for (line_index, line_result) in reader.lines().enumerate() {
            let line_number = line_index
                .checked_add(1)
                .unwrap_or_else(|| panic!("input trace line number overflow"));
            let line = line_result
                .unwrap_or_else(|error| panic!("read input trace line {line_number}: {error}"));
            if line.trim().is_empty() {
                continue;
            }
            events.push(parse_recorded_input_event(&line, line_number));
        }
        Self {
            started_at: Instant::now(),
            events,
            next_event_index: 0,
            completion_logged: false,
        }
    }

    fn take_ready_events(&mut self) -> Vec<RecordedInputEventKind> {
        let elapsed_micros = self.started_at.elapsed().as_micros();
        let elapsed_micros = u64::try_from(elapsed_micros)
            .unwrap_or_else(|_| panic!("input replay timestamp overflow"));
        let mut ready = Vec::new();
        while self.next_event_index < self.events.len()
            && self.events[self.next_event_index].elapsed_micros <= elapsed_micros
        {
            ready.push(self.events[self.next_event_index].kind.clone());
            self.next_event_index = self
                .next_event_index
                .checked_add(1)
                .unwrap_or_else(|| panic!("input replay index overflow"));
        }
        ready
    }

    fn is_exhausted(&self) -> bool {
        self.next_event_index >= self.events.len()
    }

    fn restart_clock(&mut self) {
        self.started_at = Instant::now();
    }
}

fn parse_recorded_input_event(line: &str, line_number: usize) -> RecordedInputEvent {
    let fields = line.split('\t').collect::<Vec<_>>();
    assert!(
        fields.len() >= 3,
        "invalid input trace format at line {line_number}: expected at least 3 fields"
    );
    let elapsed_micros = fields[0].parse::<u64>().unwrap_or_else(|error| {
        panic!("invalid input trace timestamp at line {line_number}: {error}")
    });
    let kind = match fields[1] {
        "mouse_input" => {
            assert!(
                fields.len() == 3,
                "invalid mouse_input trace format at line {line_number}"
            );
            let pressed = match fields[2] {
                "1" => true,
                "0" => false,
                value => panic!("invalid mouse_input value '{value}' at line {line_number}"),
            };
            RecordedInputEventKind::MouseInput { pressed }
        }
        "cursor_moved" => {
            assert!(
                fields.len() == 4,
                "invalid cursor_moved trace format at line {line_number}"
            );
            let x = fields[2]
                .parse::<f64>()
                .unwrap_or_else(|error| panic!("invalid cursor x at line {line_number}: {error}"));
            let y = fields[3]
                .parse::<f64>()
                .unwrap_or_else(|error| panic!("invalid cursor y at line {line_number}: {error}"));
            RecordedInputEventKind::CursorMoved { x, y }
        }
        "mouse_wheel_line" => {
            assert!(
                fields.len() == 3,
                "invalid mouse_wheel_line trace format at line {line_number}"
            );
            let vertical_lines = fields[2].parse::<f32>().unwrap_or_else(|error| {
                panic!("invalid mouse wheel line delta at line {line_number}: {error}")
            });
            RecordedInputEventKind::MouseWheelLine { vertical_lines }
        }
        "mouse_wheel_pixel" => {
            assert!(
                fields.len() == 3,
                "invalid mouse_wheel_pixel trace format at line {line_number}"
            );
            let delta_y = fields[2].parse::<f64>().unwrap_or_else(|error| {
                panic!("invalid mouse wheel pixel delta at line {line_number}: {error}")
            });
            RecordedInputEventKind::MouseWheelPixel { delta_y }
        }
        kind => panic!("unknown input trace event kind '{kind}' at line {line_number}"),
    };
    RecordedInputEvent {
        elapsed_micros,
        kind,
    }
}

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
    input_trace_recorder: Option<InputTraceRecorder>,
    input_trace_replay: Option<InputTraceReplay>,
    brush_trace_enabled: bool,
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

    fn maybe_record_input(&mut self, kind: RecordedInputEventKind) {
        if let Some(recorder) = self.input_trace_recorder.as_mut() {
            recorder.record(kind);
        }
    }

    fn pump_input_replay_events(&mut self) {
        let Some(_) = self.input_trace_replay.as_ref() else {
            return;
        };
        let replay_events = {
            let replay = self
                .input_trace_replay
                .as_mut()
                .unwrap_or_else(|| panic!("input replay state missing"));
            replay.take_ready_events()
        };
        for replay_event in replay_events {
            match replay_event {
                RecordedInputEventKind::MouseInput { pressed } => {
                    let state = if pressed {
                        ElementState::Pressed
                    } else {
                        ElementState::Released
                    };
                    self.handle_mouse_input(state, MouseButton::Left);
                }
                RecordedInputEventKind::CursorMoved { x, y } => {
                    self.handle_cursor_moved(PhysicalPosition::new(x, y));
                }
                RecordedInputEventKind::MouseWheelLine { vertical_lines } => {
                    self.apply_mouse_wheel_delta(MouseScrollDelta::LineDelta(0.0, vertical_lines));
                }
                RecordedInputEventKind::MouseWheelPixel { delta_y } => {
                    self.apply_mouse_wheel_delta(MouseScrollDelta::PixelDelta(
                        PhysicalPosition::new(0.0, delta_y),
                    ));
                }
            }
        }
        let should_log_completion = {
            let replay = self
                .input_trace_replay
                .as_mut()
                .unwrap_or_else(|| panic!("input replay state missing"));
            if replay.is_exhausted() && !replay.completion_logged {
                replay.completion_logged = true;
                true
            } else {
                false
            }
        };
        if should_log_completion {
            println!("[input_replay] all recorded events have been replayed");
        }
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
        if let Some(replay) = self.input_trace_replay.as_mut() {
            replay.restart_clock();
            println!(
                "[input_replay] replay started with {} events",
                replay.events.len()
            );
        }
        if self.input_trace_recorder.is_some() {
            println!("[input_record] recording input trace events");
        }
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
        self.pump_input_replay_events();
        let Some(window) = self.window.as_ref() else {
            return;
        };
        let Some(gpu) = self.gpu.as_ref() else {
            return;
        };
        let pending_brush_commands =
            u32::try_from(gpu.pending_brush_command_count()).unwrap_or(u32::MAX);
        let should_continue_rendering = self.frame_scheduler.is_active()
            || self.driver_debug.has_active_stroke()
            || pending_brush_commands > 0
            || gpu.has_pending_merge_work();
        if should_continue_rendering {
            window.request_redraw();
        }
    }
}

impl App {
    fn push_draw_pointer_input(&mut self, phase: PointerEventPhase, screen_x: f32, screen_y: f32) {
        let Some(gpu) = self.gpu.as_ref() else {
            return;
        };
        let (canvas_x, canvas_y) = gpu.screen_to_canvas_point(screen_x, screen_y);
        self.driver_debug.push_input(phase, canvas_x, canvas_y);
        if self.brush_trace_enabled {
            eprintln!(
                "[brush_trace] pointer_event phase={:?} screen=({:.2},{:.2}) canvas=({:.2},{:.2})",
                phase, screen_x, screen_y, canvas_x, canvas_y
            );
        }
    }

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
        self.maybe_record_input(RecordedInputEventKind::MouseInput {
            pressed: state == ElementState::Pressed,
        });

        self.is_left_mouse_pressed = state == ElementState::Pressed;
        if self.is_left_mouse_pressed {
            if self.interaction_mode == InteractionMode::Draw {
                if let Some((cursor_x, cursor_y)) = self.last_cursor_position {
                    self.push_draw_pointer_input(
                        PointerEventPhase::Down,
                        cursor_x as f32,
                        cursor_y as f32,
                    );
                }
            }
        } else {
            if self.driver_debug.has_active_stroke() {
                if let Some((cursor_x, cursor_y)) = self.last_cursor_position {
                    self.push_draw_pointer_input(
                        PointerEventPhase::Up,
                        cursor_x as f32,
                        cursor_y as f32,
                    );
                }
            }
            self.last_cursor_position = None;
        }
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    fn handle_cursor_moved(&mut self, position: PhysicalPosition<f64>) {
        self.maybe_record_input(RecordedInputEventKind::CursorMoved {
            x: position.x,
            y: position.y,
        });
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
                            self.push_draw_pointer_input(
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
        match delta {
            MouseScrollDelta::LineDelta(_, vertical_lines) => {
                self.maybe_record_input(RecordedInputEventKind::MouseWheelLine {
                    vertical_lines: *vertical_lines,
                });
            }
            MouseScrollDelta::PixelDelta(physical_position) => {
                self.maybe_record_input(RecordedInputEventKind::MouseWheelPixel {
                    delta_y: physical_position.y,
                });
            }
        }
        self.apply_mouse_wheel_delta(*delta);
    }

    fn apply_mouse_wheel_delta(&mut self, delta: MouseScrollDelta) {
        let scroll_lines = match delta {
            MouseScrollDelta::LineDelta(_, vertical_lines) => vertical_lines,
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
        brush_trace_enabled: bool,
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
            let mut drained_command_count = 0u32;
            while let Some(command) = receiver.pop_command() {
                if brush_trace_enabled {
                    println!(
                        "[brush_trace] frame={} recv_cmd={} stroke={}",
                        frame_sequence_id,
                        brush_command_kind(&command),
                        brush_command_stroke_session_id(&command),
                    );
                }
                gpu.enqueue_brush_render_command(command)
                    .expect("enqueue brush render command");
                drained_command_count = drained_command_count
                    .checked_add(1)
                    .expect("drained brush command count overflow");
            }
            if brush_trace_enabled && drained_command_count > 0 {
                println!(
                    "[brush_trace] frame={} drained_commands={}",
                    frame_sequence_id, drained_command_count
                );
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
            self.brush_trace_enabled,
        );
        let pending_brush_commands =
            u32::try_from(gpu.pending_brush_command_count()).unwrap_or(u32::MAX);
        let brush_hot_path_active = self.driver_debug.has_active_stroke()
            || frame_stats.input.total_events > 0
            || frame_stats.output.chunk_count > 0
            || pending_brush_commands > 0;
        let scheduler_decision = self.frame_scheduler.schedule_frame(FrameSchedulerInput {
            frame_sequence_id,
            brush_hot_path_active,
            pending_brush_commands,
            previous_frame_gpu_micros,
        });
        apply_frame_scheduler_decision(gpu, scheduler_decision);
        let render_result = gpu.render();
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

        match render_result {
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
                self.brush_trace_enabled,
            );
            let render_result = gpu.render();
            if let Err(error) = render_result {
                match error {
                    wgpu::SurfaceError::OutOfMemory => {
                        panic!("out of memory while flushing brush pipeline lifecycle")
                    }
                    wgpu::SurfaceError::Outdated
                    | wgpu::SurfaceError::Lost
                    | wgpu::SurfaceError::Timeout
                    | wgpu::SurfaceError::Other => {}
                }
            }
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
                || gpu.pending_brush_command_count() > 0;

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
    let cli_options = parse_cli_options();
    let event_loop = EventLoop::new().expect("create event loop");
    let mut app = App {
        startup_image_path: cli_options.startup_image_path,
        driver_debug: DriverDebugState::new(),
        frame_scheduler: FrameScheduler::default(),
        input_trace_recorder: cli_options
            .input_record_path
            .map(InputTraceRecorder::from_path),
        input_trace_replay: cli_options
            .input_replay_path
            .map(InputTraceReplay::from_path),
        brush_trace_enabled: brush_trace_enabled(),
        ..App::default()
    };
    event_loop.run_app(&mut app).expect("run app");
}

fn apply_frame_scheduler_decision(gpu: &GpuState, decision: FrameSchedulerDecision) {
    let Some(max_commands) = decision.brush_commands_to_render else {
        return;
    };
    gpu.set_brush_command_quota(max_commands);
    let trace_enabled =
        std::env::var_os("GLAPHICA_FRAME_SCHEDULER_TRACE").is_some_and(|value| value != "0");
    let should_log = trace_enabled
        || matches!(
            decision.update_reason,
            Some(frame_scheduler::SchedulerUpdateReason::BrushHotPathActivated)
                | Some(frame_scheduler::SchedulerUpdateReason::BrushHotPathDeactivated)
        )
        || max_commands > 0;
    if should_log {
        println!(
            "[frame_scheduler] frame={} active={} brush_commands={} reason={:?}",
            decision.frame_sequence_id,
            decision.scheduler_active,
            max_commands,
            decision.update_reason,
        );
    }
}

fn brush_trace_enabled() -> bool {
    std::env::var_os("GLAPHICA_BRUSH_TRACE").is_some_and(|value| value != "0")
}

fn brush_command_kind(command: &BrushRenderCommand) -> &'static str {
    match command {
        BrushRenderCommand::BeginStroke(_) => "BeginStroke",
        BrushRenderCommand::AllocateBufferTiles(_) => "AllocateBufferTiles",
        BrushRenderCommand::PushDabChunkF32(_) => "PushDabChunkF32",
        BrushRenderCommand::EndStroke(_) => "EndStroke",
        BrushRenderCommand::MergeBuffer(_) => "MergeBuffer",
    }
}

fn brush_command_stroke_session_id(command: &BrushRenderCommand) -> u64 {
    match command {
        BrushRenderCommand::BeginStroke(command) => command.stroke_session_id,
        BrushRenderCommand::AllocateBufferTiles(command) => command.stroke_session_id,
        BrushRenderCommand::PushDabChunkF32(command) => command.stroke_session_id,
        BrushRenderCommand::EndStroke(command) => command.stroke_session_id,
        BrushRenderCommand::MergeBuffer(command) => command.stroke_session_id,
    }
}

struct CliOptions {
    startup_image_path: Option<PathBuf>,
    input_record_path: Option<PathBuf>,
    input_replay_path: Option<PathBuf>,
}

fn parse_cli_options() -> CliOptions {
    let usage = "usage: glaphica [--image <path>] [--record-input <path>] [--replay-input <path>] | [<path>]";
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();

    let mut startup_image_path = None;
    let mut positional_image_path = None;
    let mut input_record_path = None;
    let mut input_replay_path = None;
    let mut index = 0usize;

    while index < args.len() {
        let current = &args[index];
        if current == OsStr::new("--image") {
            index = index
                .checked_add(1)
                .unwrap_or_else(|| panic!("argument index overflow"));
            assert!(
                index < args.len(),
                "missing image path after --image; {usage}"
            );
            startup_image_path = Some(PathBuf::from(&args[index]));
        } else if current == OsStr::new("--record-input") {
            index = index
                .checked_add(1)
                .unwrap_or_else(|| panic!("argument index overflow"));
            assert!(
                index < args.len(),
                "missing path after --record-input; {usage}"
            );
            input_record_path = Some(PathBuf::from(&args[index]));
        } else if current == OsStr::new("--replay-input") {
            index = index
                .checked_add(1)
                .unwrap_or_else(|| panic!("argument index overflow"));
            assert!(
                index < args.len(),
                "missing path after --replay-input; {usage}"
            );
            input_replay_path = Some(PathBuf::from(&args[index]));
        } else if current.to_string_lossy().starts_with("--") {
            panic!("unknown option '{}'; {usage}", current.to_string_lossy());
        } else {
            assert!(
                positional_image_path.is_none(),
                "too many positional arguments; {usage}"
            );
            positional_image_path = Some(PathBuf::from(current));
        }
        index = index
            .checked_add(1)
            .unwrap_or_else(|| panic!("argument index overflow"));
    }

    assert!(
        startup_image_path.is_none() || positional_image_path.is_none(),
        "cannot use positional image path together with --image; {usage}"
    );
    assert!(
        !(input_record_path.is_some() && input_replay_path.is_some()),
        "cannot use --record-input and --replay-input together; choose one mode"
    );

    CliOptions {
        startup_image_path: startup_image_path.or(positional_image_path),
        input_record_path,
        input_replay_path,
    }
}
