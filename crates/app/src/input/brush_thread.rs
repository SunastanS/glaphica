use std::collections::VecDeque;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use brush::BrushId;
use brush::round::RoundBrushSettings;

use crate::AppBrushRegistry;
use crate::input::{
    ActiveTool, BrushThreadBrushInputProducer, BrushWorker, BrushWorkerError,
    MainBrushInputConsumer, ToolSet, create_brush_input_channels,
};

pub struct BrushThreadRuntime {
    tool_set: ToolSet,
    active_tool: Arc<Mutex<ActiveTool>>,
    command_queue: Arc<BrushThreadCommandQueue>,
    brush_input_consumer: MainBrushInputConsumer,
    stop_requested: Arc<AtomicBool>,
    command_epoch: Arc<AtomicU64>,
    finish_state: Arc<FinishSignal>,
    thread: Option<JoinHandle<()>>,
}

#[derive(Debug)]
pub enum BrushThreadCommand {
    Begin {
        epoch: u64,
        brush_id: BrushId,
    },
    Reset {
        epoch: u64,
        brush_id: BrushId,
    },
    CanvasInput {
        epoch: u64,
        input: crate::CanvasInput,
    },
    Finish {
        epoch: u64,
        request_generation: u64,
    },
    Cancel {
        epoch: u64,
        brush_id: BrushId,
    },
    UpdateRoundBrushSettings(RoundBrushSettings),
}

#[derive(Debug)]
struct BrushThreadCommandQueueState {
    queue: VecDeque<BrushThreadCommand>,
    dropped_inputs: u64,
}

#[derive(Debug)]
struct BrushThreadCommandQueue {
    max_pending_inputs: usize,
    state: Mutex<BrushThreadCommandQueueState>,
    changed: Condvar,
}

impl BrushThreadCommandQueue {
    fn new(capacity: usize) -> Self {
        assert!(
            capacity > 0,
            "brush command queue capacity must be greater than zero"
        );
        Self {
            max_pending_inputs: capacity,
            state: Mutex::new(BrushThreadCommandQueueState {
                queue: VecDeque::with_capacity(capacity),
                dropped_inputs: 0,
            }),
            changed: Condvar::new(),
        }
    }

    fn push(&self, command: BrushThreadCommand) {
        let mut state = self
            .state
            .lock()
            .expect("brush command queue should not be poisoned");
        if matches!(command, BrushThreadCommand::CanvasInput { .. }) {
            while pending_input_count(&state.queue) >= self.max_pending_inputs {
                if !drop_oldest_input(&mut state.queue) {
                    break;
                }
                state.dropped_inputs = state.dropped_inputs.saturating_add(1);
            }
        } else {
            state.queue.push_back(command);
            drop(state);
            self.changed.notify_one();
            return;
        }
        state.queue.push_back(command);
        drop(state);
        self.changed.notify_one();
    }

    fn drain_batch_with_wait(
        &self,
        output: &mut Vec<BrushThreadCommand>,
        max_items: usize,
        wait_timeout: Duration,
        stop_requested: &AtomicBool,
    ) {
        if max_items == 0 {
            return;
        }
        let mut state = self
            .state
            .lock()
            .expect("brush command queue should not be poisoned");
        if state.queue.is_empty()
            && !wait_timeout.is_zero()
            && !stop_requested.load(Ordering::Relaxed)
        {
            let (next_state, _) = self
                .changed
                .wait_timeout(state, wait_timeout)
                .expect("brush command queue wait should not be poisoned");
            state = next_state;
        }
        for _ in 0..max_items {
            let Some(command) = state.queue.pop_front() else {
                break;
            };
            output.push(command);
        }
    }

    fn notify_changed(&self) {
        self.changed.notify_all();
    }
}

fn drop_oldest_input(queue: &mut VecDeque<BrushThreadCommand>) -> bool {
    let Some(position) = queue
        .iter()
        .position(|command| matches!(command, BrushThreadCommand::CanvasInput { .. }))
    else {
        return false;
    };
    queue.remove(position);
    true
}

fn pending_input_count(queue: &VecDeque<BrushThreadCommand>) -> usize {
    queue
        .iter()
        .filter(|command| matches!(command, BrushThreadCommand::CanvasInput { .. }))
        .count()
}

struct FinishState {
    request_generation: u64,
    ack_generation: u64,
    worker_error: Option<BrushWorkerError>,
}

struct FinishSignal {
    state: Mutex<FinishState>,
    changed: Condvar,
}

impl FinishSignal {
    fn new() -> Self {
        Self {
            state: Mutex::new(FinishState {
                request_generation: 0,
                ack_generation: 0,
                worker_error: None,
            }),
            changed: Condvar::new(),
        }
    }

    fn request_finish(&self) -> u64 {
        let mut state = self
            .state
            .lock()
            .expect("finish state should not be poisoned");
        state.request_generation = state.request_generation.saturating_add(1);
        let request_generation = state.request_generation;
        self.changed.notify_all();
        request_generation
    }

    fn wait_for_ack_or_error(&self, request_generation: u64) -> Result<(), BrushWorkerError> {
        let mut state = self
            .state
            .lock()
            .expect("finish state should not be poisoned");
        loop {
            if state.ack_generation >= request_generation {
                return Ok(());
            }
            if let Some(error) = state.worker_error.take() {
                return Err(error);
            }
            state = self
                .changed
                .wait(state)
                .expect("finish state should not be poisoned");
        }
    }

    fn take_worker_error(&self) -> Option<BrushWorkerError> {
        self.state
            .lock()
            .expect("finish state should not be poisoned")
            .worker_error
            .take()
    }

    fn ack_finish(&self, request_generation: u64) {
        let mut state = self
            .state
            .lock()
            .expect("finish state should not be poisoned");
        state.ack_generation = request_generation;
        self.changed.notify_all();
    }

    fn store_worker_error(&self, error: BrushWorkerError) {
        let mut state = self
            .state
            .lock()
            .expect("finish state should not be poisoned");
        state.worker_error = Some(error);
        self.changed.notify_all();
    }

    fn notify_changed(&self) {
        self.changed.notify_all();
    }
}

#[derive(Debug)]
pub enum BrushThreadRuntimeError {
    Worker(BrushWorkerError),
    ActiveToolUnavailable(ActiveTool),
    SpawnThread(std::io::Error),
    ThreadPanicked,
}

impl Display for BrushThreadRuntimeError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Worker(error) => Display::fmt(error, f),
            Self::ActiveToolUnavailable(active_tool) => match active_tool {
                ActiveTool::Brush(brush_id) => {
                    write!(
                        f,
                        "active tool brush {} is not in the tool set",
                        brush_id.raw()
                    )
                }
            },
            Self::SpawnThread(error) => Display::fmt(error, f),
            Self::ThreadPanicked => f.write_str("brush thread panicked"),
        }
    }
}

impl Error for BrushThreadRuntimeError {}

impl From<BrushWorkerError> for BrushThreadRuntimeError {
    fn from(error: BrushWorkerError) -> Self {
        Self::Worker(error)
    }
}

impl BrushThreadRuntime {
    pub fn spawn(
        brushes: AppBrushRegistry,
        tool_set: ToolSet,
        active_tool: ActiveTool,
        canvas_input_capacity: usize,
        brush_input_capacity: usize,
        worker_batch_capacity: usize,
        worker_wait_timeout: Duration,
    ) -> Result<Self, BrushThreadRuntimeError> {
        if !tool_set.contains(active_tool.as_tool()) {
            return Err(BrushThreadRuntimeError::ActiveToolUnavailable(active_tool));
        }
        let active_brush_id = match active_tool {
            ActiveTool::Brush(brush_id) => brush_id,
        };
        let worker = BrushWorker::new(brushes, active_brush_id, worker_batch_capacity)?;
        let (brush_input_producer, brush_input_consumer) =
            create_brush_input_channels(brush_input_capacity);
        let active_tool = Arc::new(Mutex::new(active_tool));
        let command_queue = Arc::new(BrushThreadCommandQueue::new(canvas_input_capacity));
        let stop_requested = Arc::new(AtomicBool::new(false));
        let command_epoch = Arc::new(AtomicU64::new(0));
        let finish_state = Arc::new(FinishSignal::new());
        let thread_stop_requested = stop_requested.clone();
        let thread_command_queue = command_queue.clone();
        let thread_finish_state = finish_state.clone();
        let thread = thread::Builder::new()
            .name("glaphica-brush".to_string())
            .spawn(move || {
                run_brush_thread(
                    worker,
                    thread_command_queue,
                    brush_input_producer,
                    thread_stop_requested,
                    thread_finish_state,
                    worker_batch_capacity,
                    worker_wait_timeout,
                );
            })
            .map_err(BrushThreadRuntimeError::SpawnThread)?;

        Ok(Self {
            tool_set,
            active_tool,
            command_queue,
            brush_input_consumer,
            stop_requested,
            command_epoch,
            finish_state,
            thread: Some(thread),
        })
    }

    pub fn brush_input_consumer(&self) -> &MainBrushInputConsumer {
        &self.brush_input_consumer
    }

    pub fn tool_set(&self) -> &ToolSet {
        &self.tool_set
    }

    pub fn active_tool(&self) -> ActiveTool {
        *self
            .active_tool
            .lock()
            .expect("active tool state should not be poisoned")
    }

    pub fn set_active_tool(&self, active_tool: ActiveTool) -> Result<(), BrushThreadRuntimeError> {
        if !self.tool_set.contains(active_tool.as_tool()) {
            return Err(BrushThreadRuntimeError::ActiveToolUnavailable(active_tool));
        }
        let mut stored = self
            .active_tool
            .lock()
            .expect("active tool state should not be poisoned");
        *stored = active_tool;
        Ok(())
    }

    pub fn take_worker_error(&self) -> Option<BrushWorkerError> {
        self.finish_state.take_worker_error()
    }

    pub fn reset_active_stroke_processing(&self) {
        let brush_id = self.active_brush_id();
        let epoch = self.next_epoch();
        self.command_queue
            .push(BrushThreadCommand::Reset { epoch, brush_id });
        self.brush_input_consumer.clear();
    }

    pub fn begin_active_stroke_processing(&self) {
        let brush_id = self.active_brush_id();
        let epoch = self.next_epoch();
        self.command_queue
            .push(BrushThreadCommand::Begin { epoch, brush_id });
        self.brush_input_consumer.clear();
    }

    pub fn cancel_active_stroke_processing(&self) {
        let brush_id = self.active_brush_id();
        let epoch = self.next_epoch();
        self.command_queue
            .push(BrushThreadCommand::Cancel { epoch, brush_id });
        self.brush_input_consumer.clear();
    }

    pub fn push_canvas_input(&self, input: crate::CanvasInput) {
        let epoch = self.command_epoch.load(Ordering::Relaxed);
        self.command_queue
            .push(BrushThreadCommand::CanvasInput { epoch, input });
    }

    pub fn update_round_brush_settings(&self, settings: RoundBrushSettings) {
        self.command_queue
            .push(BrushThreadCommand::UpdateRoundBrushSettings(settings));
    }

    pub fn finish_active_stroke_processing(&self) -> Result<(), BrushThreadRuntimeError> {
        let request = self.finish_state.request_finish();
        let epoch = self.command_epoch.load(Ordering::Relaxed);
        self.command_queue.push(BrushThreadCommand::Finish {
            epoch,
            request_generation: request,
        });
        self.finish_state
            .wait_for_ack_or_error(request)
            .map_err(BrushThreadRuntimeError::Worker)?;
        Ok(())
    }

    pub fn shutdown(mut self) -> Result<(), BrushThreadRuntimeError> {
        self.stop_requested.store(true, Ordering::Relaxed);
        self.finish_state.notify_changed();
        self.command_queue.notify_changed();
        let Some(thread) = self.thread.take() else {
            return Ok(());
        };
        match thread.join() {
            Ok(()) => {
                if let Some(error) = self.take_worker_error() {
                    return Err(BrushThreadRuntimeError::Worker(error));
                }
                Ok(())
            }
            Err(_) => Err(BrushThreadRuntimeError::ThreadPanicked),
        }
    }
}

impl Drop for BrushThreadRuntime {
    fn drop(&mut self) {
        self.stop_requested.store(true, Ordering::Relaxed);
        self.finish_state.notify_changed();
        self.command_queue.notify_changed();
    }
}

impl BrushThreadRuntime {
    fn next_epoch(&self) -> u64 {
        self.command_epoch
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1)
    }

    fn active_brush_id(&self) -> BrushId {
        match *self
            .active_tool
            .lock()
            .expect("active tool state should not be poisoned")
        {
            ActiveTool::Brush(brush_id) => brush_id,
        }
    }
}

fn run_brush_thread(
    mut worker: BrushWorker,
    command_queue: Arc<BrushThreadCommandQueue>,
    brush_input_producer: BrushThreadBrushInputProducer,
    stop_requested: Arc<AtomicBool>,
    finish_state: Arc<FinishSignal>,
    worker_batch_capacity: usize,
    worker_wait_timeout: Duration,
) {
    let mut command_batch = Vec::with_capacity(worker_batch_capacity);
    let mut canvas_batch = Vec::with_capacity(worker_batch_capacity);
    let mut stroke_state = WorkerStrokeState::Idle;
    while !stop_requested.load(Ordering::Relaxed) {
        command_batch.clear();
        command_queue.drain_batch_with_wait(
            &mut command_batch,
            worker_batch_capacity,
            worker_wait_timeout,
            &stop_requested,
        );
        if command_batch.is_empty() {
            continue;
        }
        for command in command_batch.drain(..) {
            match command {
                BrushThreadCommand::CanvasInput { epoch, input } => {
                    if stroke_state.accepts(epoch) {
                        canvas_batch.push(input);
                    }
                }
                BrushThreadCommand::Begin { epoch, brush_id } => {
                    canvas_batch.clear();
                    if let Err(error) = worker.set_active_brush(brush_id) {
                        finish_state.store_worker_error(error);
                        return;
                    }
                    worker.reset_active_stroke();
                    stroke_state = WorkerStrokeState::Active { epoch };
                }
                BrushThreadCommand::Reset {
                    epoch: _epoch,
                    brush_id,
                } => {
                    canvas_batch.clear();
                    if let Err(error) = worker.set_active_brush(brush_id) {
                        finish_state.store_worker_error(error);
                        return;
                    }
                    worker.reset_active_stroke();
                    stroke_state = WorkerStrokeState::Idle;
                }
                BrushThreadCommand::Cancel {
                    epoch: _epoch,
                    brush_id,
                } => {
                    canvas_batch.clear();
                    if let Err(error) = worker.set_active_brush(brush_id) {
                        finish_state.store_worker_error(error);
                        return;
                    }
                    worker.reset_active_stroke();
                    stroke_state = WorkerStrokeState::Idle;
                }
                BrushThreadCommand::Finish {
                    epoch,
                    request_generation,
                } => {
                    if stroke_state.accepts(epoch) {
                        if let Err(error) = flush_canvas_batch(
                            &mut worker,
                            &brush_input_producer,
                            &mut canvas_batch,
                        ) {
                            finish_state.store_worker_error(error);
                            return;
                        }
                        if let Err(error) = worker.finish_active_stroke(&brush_input_producer) {
                            finish_state.store_worker_error(error);
                            return;
                        }
                        worker.reset_active_stroke();
                        stroke_state = WorkerStrokeState::Idle;
                    }
                    finish_state.ack_finish(request_generation);
                }
                BrushThreadCommand::UpdateRoundBrushSettings(settings) => {
                    if let Err(error) =
                        flush_canvas_batch(&mut worker, &brush_input_producer, &mut canvas_batch)
                    {
                        finish_state.store_worker_error(error);
                        return;
                    }
                    if let Err(error) = worker.update_round_brush_settings(settings) {
                        finish_state.store_worker_error(error);
                        return;
                    }
                    stroke_state = WorkerStrokeState::Idle;
                }
            }
        }
        if let Err(error) =
            flush_canvas_batch(&mut worker, &brush_input_producer, &mut canvas_batch)
        {
            finish_state.store_worker_error(error);
            break;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkerStrokeState {
    Idle,
    Active { epoch: u64 },
}

impl WorkerStrokeState {
    fn accepts(self, epoch: u64) -> bool {
        matches!(self, Self::Active { epoch: active_epoch } if active_epoch == epoch)
    }
}

fn flush_canvas_batch(
    worker: &mut BrushWorker,
    brush_input_producer: &BrushThreadBrushInputProducer,
    canvas_batch: &mut Vec<crate::CanvasInput>,
) -> Result<(), BrushWorkerError> {
    if !canvas_batch.is_empty() {
        worker.process_canvas_inputs(canvas_batch, brush_input_producer)?;
        canvas_batch.clear();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use atlas::{AtlasLayout, Backend, BackendId};
    use brush::round::ROUND_BRUSH_ID;
    use glaphica_core::{CanvasInput, CanvasVec2, RadianVec2};

    use crate::{ActiveTool, BrushThreadRuntime, Tool, ToolSet};

    #[test]
    fn spawned_runtime_processes_canvas_input_in_background() {
        let backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(51));
        let brushes = crate::AppBrushRegistry::with_builtin_round(backend);
        let runtime = BrushThreadRuntime::spawn(
            brushes,
            ToolSet::new(vec![Tool::Brush(ROUND_BRUSH_ID)]),
            ActiveTool::Brush(ROUND_BRUSH_ID),
            8,
            8,
            16,
            Duration::from_millis(1),
        )
        .expect("spawn runtime");
        let mut brush_inputs = Vec::new();

        runtime.begin_active_stroke_processing();
        runtime.push_canvas_input(CanvasInput {
            time_ns: 1,
            position: CanvasVec2::new(4.0, 5.0),
            pressure: 0.6,
            tilt: RadianVec2::new(0.0, 0.0),
            twist: 0.0,
        });
        runtime.push_canvas_input(CanvasInput {
            time_ns: 2,
            position: CanvasVec2::new(10.0, 5.0),
            pressure: 0.6,
            tilt: RadianVec2::new(0.0, 0.0),
            twist: 0.0,
        });

        runtime.brush_input_consumer().drain_batch_with_wait(
            &mut brush_inputs,
            1,
            Duration::from_millis(20),
        );

        assert_eq!(brush_inputs.len(), 1);
        assert_eq!(brush_inputs[0].brush_id, ROUND_BRUSH_ID);
        assert!(!brush_inputs[0].blocks.blocks().is_empty());
        assert_eq!(brush_inputs[0].blocks.blocks()[0].values()[0], 4.0);

        runtime.shutdown().expect("shutdown runtime");
    }

    #[test]
    fn runtime_preserves_begin_input_finish_command_order() {
        let backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(53));
        let brushes = crate::AppBrushRegistry::with_builtin_round(backend);
        let runtime = BrushThreadRuntime::spawn(
            brushes,
            ToolSet::new(vec![Tool::Brush(ROUND_BRUSH_ID)]),
            ActiveTool::Brush(ROUND_BRUSH_ID),
            8,
            8,
            16,
            Duration::from_millis(20),
        )
        .expect("spawn runtime");

        runtime.begin_active_stroke_processing();
        runtime.push_canvas_input(CanvasInput {
            time_ns: 1,
            position: CanvasVec2::new(11.0, 13.0),
            pressure: 0.6,
            tilt: RadianVec2::new(0.0, 0.0),
            twist: 0.0,
        });
        runtime
            .finish_active_stroke_processing()
            .expect("finish stroke");

        let mut brush_inputs = Vec::new();
        runtime
            .brush_input_consumer()
            .drain_batch_with_wait(&mut brush_inputs, 8, Duration::ZERO);

        assert_eq!(brush_inputs.len(), 1);
        assert_eq!(brush_inputs[0].brush_id, ROUND_BRUSH_ID);
        let blocks = brush_inputs[0].blocks.blocks();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].values()[0], 11.0);
        assert_eq!(blocks[0].values()[1], 13.0);

        runtime.shutdown().expect("shutdown runtime");
    }

    #[test]
    fn reset_and_cancel_ignore_old_epoch_inputs() {
        let backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(54));
        let brushes = crate::AppBrushRegistry::with_builtin_round(backend);
        let worker = crate::BrushWorker::new(brushes, ROUND_BRUSH_ID, 16).expect("worker");
        let command_queue = std::sync::Arc::new(super::BrushThreadCommandQueue::new(16));
        let (brush_producer, brush_consumer) = crate::create_brush_input_channels(8);
        let stop_requested = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let finish_state = std::sync::Arc::new(super::FinishSignal::new());
        let request = finish_state.request_finish();

        command_queue.push(super::BrushThreadCommand::Begin {
            epoch: 1,
            brush_id: ROUND_BRUSH_ID,
        });
        command_queue.push(super::BrushThreadCommand::CanvasInput {
            epoch: 1,
            input: CanvasInput {
                time_ns: 1,
                position: CanvasVec2::new(1.0, 1.0),
                pressure: 0.6,
                tilt: RadianVec2::new(0.0, 0.0),
                twist: 0.0,
            },
        });
        command_queue.push(super::BrushThreadCommand::Reset {
            epoch: 2,
            brush_id: ROUND_BRUSH_ID,
        });
        command_queue.push(super::BrushThreadCommand::Cancel {
            epoch: 3,
            brush_id: ROUND_BRUSH_ID,
        });
        command_queue.push(super::BrushThreadCommand::CanvasInput {
            epoch: 1,
            input: CanvasInput {
                time_ns: 2,
                position: CanvasVec2::new(2.0, 2.0),
                pressure: 0.6,
                tilt: RadianVec2::new(0.0, 0.0),
                twist: 0.0,
            },
        });
        command_queue.push(super::BrushThreadCommand::CanvasInput {
            epoch: 3,
            input: CanvasInput {
                time_ns: 3,
                position: CanvasVec2::new(9.0, 9.0),
                pressure: 0.6,
                tilt: RadianVec2::new(0.0, 0.0),
                twist: 0.0,
            },
        });
        command_queue.push(super::BrushThreadCommand::Finish {
            epoch: 3,
            request_generation: request,
        });

        let thread_command_queue = command_queue.clone();
        let thread_stop_requested = stop_requested.clone();
        let thread_finish_state = finish_state.clone();
        let handle = std::thread::spawn(move || {
            super::run_brush_thread(
                worker,
                thread_command_queue,
                brush_producer,
                thread_stop_requested,
                thread_finish_state,
                16,
                Duration::from_millis(20),
            );
        });

        finish_state
            .wait_for_ack_or_error(request)
            .expect("finish ack");
        stop_requested.store(true, std::sync::atomic::Ordering::Relaxed);
        command_queue.notify_changed();
        handle.join().expect("brush thread");

        let mut brush_inputs = Vec::new();
        brush_consumer.drain_batch_with_wait(&mut brush_inputs, 8, Duration::ZERO);
        assert!(brush_inputs.is_empty());
    }

    #[test]
    fn stale_input_before_first_begin_is_ignored_even_when_batch_capacity_is_one() {
        let commands = vec![
            super::BrushThreadCommand::CanvasInput {
                epoch: 0,
                input: canvas_input(1, 1.0, 1.0),
            },
            super::BrushThreadCommand::Begin {
                epoch: 1,
                brush_id: ROUND_BRUSH_ID,
            },
            super::BrushThreadCommand::CanvasInput {
                epoch: 1,
                input: canvas_input(2, 9.0, 9.0),
            },
        ];
        let brush_inputs = run_commands_until_finish(commands, 1, 1);

        assert_eq!(brush_inputs.len(), 1);
        assert_first_block_position(&brush_inputs[0], 9.0, 9.0);
    }

    #[test]
    fn cancel_transitions_worker_to_idle_and_rejects_same_epoch_input() {
        let commands = vec![
            super::BrushThreadCommand::Begin {
                epoch: 1,
                brush_id: ROUND_BRUSH_ID,
            },
            super::BrushThreadCommand::CanvasInput {
                epoch: 1,
                input: canvas_input(1, 1.0, 1.0),
            },
            super::BrushThreadCommand::Cancel {
                epoch: 2,
                brush_id: ROUND_BRUSH_ID,
            },
            super::BrushThreadCommand::CanvasInput {
                epoch: 2,
                input: canvas_input(2, 9.0, 9.0),
            },
        ];
        let brush_inputs = run_commands_until_finish(commands, 2, 16);

        assert!(brush_inputs.is_empty());
    }

    #[test]
    fn finish_transitions_worker_to_idle_and_rejects_same_epoch_input_before_reset() {
        let backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(56));
        let brushes = crate::AppBrushRegistry::with_builtin_round(backend);
        let worker = crate::BrushWorker::new(brushes, ROUND_BRUSH_ID, 16).expect("worker");
        let command_queue = std::sync::Arc::new(super::BrushThreadCommandQueue::new(16));
        let (brush_producer, brush_consumer) = crate::create_brush_input_channels(8);
        let stop_requested = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let finish_state = std::sync::Arc::new(super::FinishSignal::new());
        let first_finish = finish_state.request_finish();
        let second_finish = finish_state.request_finish();

        command_queue.push(super::BrushThreadCommand::Begin {
            epoch: 1,
            brush_id: ROUND_BRUSH_ID,
        });
        command_queue.push(super::BrushThreadCommand::CanvasInput {
            epoch: 1,
            input: canvas_input(1, 1.0, 1.0),
        });
        command_queue.push(super::BrushThreadCommand::Finish {
            epoch: 1,
            request_generation: first_finish,
        });
        command_queue.push(super::BrushThreadCommand::CanvasInput {
            epoch: 1,
            input: canvas_input(2, 9.0, 9.0),
        });
        command_queue.push(super::BrushThreadCommand::Finish {
            epoch: 1,
            request_generation: second_finish,
        });

        let thread_command_queue = command_queue.clone();
        let thread_stop_requested = stop_requested.clone();
        let thread_finish_state = finish_state.clone();
        let handle = std::thread::spawn(move || {
            super::run_brush_thread(
                worker,
                thread_command_queue,
                brush_producer,
                thread_stop_requested,
                thread_finish_state,
                16,
                Duration::from_millis(20),
            );
        });

        finish_state
            .wait_for_ack_or_error(second_finish)
            .expect("finish ack");
        stop_requested.store(true, std::sync::atomic::Ordering::Relaxed);
        command_queue.notify_changed();
        handle.join().expect("brush thread");

        let mut brush_inputs = Vec::new();
        brush_consumer.drain_batch_with_wait(&mut brush_inputs, 8, Duration::ZERO);

        assert_eq!(brush_inputs.len(), 1);
        assert_first_block_position(&brush_inputs[0], 1.0, 1.0);
    }

    #[test]
    fn command_queue_drops_oldest_inputs_instead_of_growing_unbounded() {
        let queue = super::BrushThreadCommandQueue::new(2);
        let stop_requested = std::sync::atomic::AtomicBool::new(false);
        for time_ns in 1..=3 {
            queue.push(super::BrushThreadCommand::CanvasInput {
                epoch: 0,
                input: CanvasInput {
                    time_ns,
                    position: CanvasVec2::new(time_ns as f32, 0.0),
                    pressure: 0.6,
                    tilt: RadianVec2::new(0.0, 0.0),
                    twist: 0.0,
                },
            });
        }

        let mut commands = Vec::new();
        queue.drain_batch_with_wait(&mut commands, 8, Duration::ZERO, &stop_requested);

        let times = commands
            .into_iter()
            .map(|command| match command {
                super::BrushThreadCommand::CanvasInput { input, .. } => input.time_ns,
                _ => panic!("expected only canvas input commands"),
            })
            .collect::<Vec<_>>();
        assert_eq!(times, vec![2, 3]);
    }

    #[test]
    fn command_queue_capacity_limits_inputs_not_control_commands() {
        let queue = super::BrushThreadCommandQueue::new(1);
        let stop_requested = std::sync::atomic::AtomicBool::new(false);

        queue.push(super::BrushThreadCommand::Begin {
            epoch: 1,
            brush_id: ROUND_BRUSH_ID,
        });
        queue.push(super::BrushThreadCommand::CanvasInput {
            epoch: 1,
            input: canvas_input(1, 4.0, 5.0),
        });

        let mut commands = Vec::new();
        queue.drain_batch_with_wait(&mut commands, 8, Duration::ZERO, &stop_requested);

        assert_eq!(commands.len(), 2);
        assert!(matches!(
            commands[0],
            super::BrushThreadCommand::Begin {
                epoch: 1,
                brush_id: ROUND_BRUSH_ID
            }
        ));
        match &commands[1] {
            super::BrushThreadCommand::CanvasInput { epoch, input } => {
                assert_eq!(*epoch, 1);
                assert_eq!(input.time_ns, 1);
                assert_eq!(input.position, CanvasVec2::new(4.0, 5.0));
            }
            _ => panic!("expected canvas input after begin"),
        }
    }

    fn run_commands_until_finish(
        mut commands: Vec<super::BrushThreadCommand>,
        finish_epoch: u64,
        worker_batch_capacity: usize,
    ) -> Vec<brush::BrushInput> {
        let backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(55));
        let brushes = crate::AppBrushRegistry::with_builtin_round(backend);
        let worker = crate::BrushWorker::new(brushes, ROUND_BRUSH_ID, 16).expect("worker");
        let command_queue = std::sync::Arc::new(super::BrushThreadCommandQueue::new(32));
        let (brush_producer, brush_consumer) = crate::create_brush_input_channels(8);
        let stop_requested = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let finish_state = std::sync::Arc::new(super::FinishSignal::new());
        let request = finish_state.request_finish();

        for command in commands.drain(..) {
            command_queue.push(command);
        }
        command_queue.push(super::BrushThreadCommand::Finish {
            epoch: finish_epoch,
            request_generation: request,
        });

        let thread_command_queue = command_queue.clone();
        let thread_stop_requested = stop_requested.clone();
        let thread_finish_state = finish_state.clone();
        let handle = std::thread::spawn(move || {
            super::run_brush_thread(
                worker,
                thread_command_queue,
                brush_producer,
                thread_stop_requested,
                thread_finish_state,
                worker_batch_capacity,
                Duration::from_millis(20),
            );
        });

        finish_state
            .wait_for_ack_or_error(request)
            .expect("finish ack");
        stop_requested.store(true, std::sync::atomic::Ordering::Relaxed);
        command_queue.notify_changed();
        handle.join().expect("brush thread");

        let mut brush_inputs = Vec::new();
        brush_consumer.drain_batch_with_wait(&mut brush_inputs, 8, Duration::ZERO);
        brush_inputs
    }

    fn canvas_input(time_ns: u64, x: f32, y: f32) -> CanvasInput {
        CanvasInput {
            time_ns,
            position: CanvasVec2::new(x, y),
            pressure: 0.6,
            tilt: RadianVec2::new(0.0, 0.0),
            twist: 0.0,
        }
    }

    fn assert_first_block_position(brush_input: &brush::BrushInput, x: f32, y: f32) {
        let blocks = brush_input.blocks.blocks();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].values()[0], x);
        assert_eq!(blocks[0].values()[1], y);
    }

    #[test]
    fn runtime_rejects_active_tool_outside_tool_set() {
        let backend = Backend::new(AtlasLayout::Tiny8, BackendId::new(52));
        let brushes = crate::AppBrushRegistry::with_builtin_round(backend);

        let error = match BrushThreadRuntime::spawn(
            brushes,
            ToolSet::new(vec![Tool::Brush(ROUND_BRUSH_ID)]),
            ActiveTool::Brush(brush::BrushId::new(99)),
            8,
            8,
            16,
            Duration::from_millis(1),
        ) {
            Ok(_) => panic!("expected invalid active tool"),
            Err(error) => error,
        };

        assert!(matches!(
            error,
            crate::BrushThreadRuntimeError::ActiveToolUnavailable(ActiveTool::Brush(brush_id))
                if brush_id == brush::BrushId::new(99)
        ));
    }
}
