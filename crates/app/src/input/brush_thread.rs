use std::error::Error;
use std::fmt::{Display, Formatter};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::AppBrushRegistry;
use crate::input::{
    ActiveTool, BrushThreadBrushInputProducer, BrushThreadCanvasInputConsumer, BrushWorker,
    BrushWorkerError, MainBrushInputConsumer, MainCanvasInputProducer, ToolSet,
    create_brush_input_channels,
};

pub struct BrushThreadRuntime {
    tool_set: ToolSet,
    active_tool: Arc<Mutex<ActiveTool>>,
    canvas_input_producer: MainCanvasInputProducer,
    brush_input_consumer: MainBrushInputConsumer,
    stop_requested: Arc<AtomicBool>,
    stroke_generation: Arc<AtomicU64>,
    finish_state: Arc<FinishSignal>,
    worker_error: Arc<Mutex<Option<BrushWorkerError>>>,
    thread: Option<JoinHandle<()>>,
}

struct FinishState {
    request_generation: u64,
    ack_generation: u64,
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

    fn wait_for_ack_or_error(
        &self,
        request_generation: u64,
        worker_error: &Mutex<Option<BrushWorkerError>>,
    ) -> Result<(), BrushWorkerError> {
        let mut state = self
            .state
            .lock()
            .expect("finish state should not be poisoned");
        loop {
            if state.ack_generation >= request_generation {
                return Ok(());
            }
            if let Some(error) = worker_error
                .lock()
                .expect("brush worker error state should not be poisoned")
                .take()
            {
                return Err(error);
            }
            state = self
                .changed
                .wait(state)
                .expect("finish state should not be poisoned");
        }
    }

    fn request_generation(&self) -> u64 {
        self.state
            .lock()
            .expect("finish state should not be poisoned")
            .request_generation
    }

    fn ack_finish(&self, request_generation: u64) {
        let mut state = self
            .state
            .lock()
            .expect("finish state should not be poisoned");
        state.ack_generation = request_generation;
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
        let (
            canvas_input_producer,
            canvas_input_consumer,
            brush_input_producer,
            brush_input_consumer,
        ) = create_brush_input_channels(canvas_input_capacity, brush_input_capacity);
        let active_tool = Arc::new(Mutex::new(active_tool));
        let stop_requested = Arc::new(AtomicBool::new(false));
        let stroke_generation = Arc::new(AtomicU64::new(0));
        let finish_state = Arc::new(FinishSignal::new());
        let worker_error = Arc::new(Mutex::new(None));
        let thread_active_tool = active_tool.clone();
        let thread_stop_requested = stop_requested.clone();
        let thread_stroke_generation = stroke_generation.clone();
        let thread_finish_state = finish_state.clone();
        let thread_worker_error = worker_error.clone();
        let thread = thread::Builder::new()
            .name("glaphica-brush".to_string())
            .spawn(move || {
                run_brush_thread(
                    worker,
                    canvas_input_consumer,
                    brush_input_producer,
                    thread_active_tool,
                    thread_stop_requested,
                    thread_stroke_generation,
                    thread_finish_state,
                    thread_worker_error,
                    worker_batch_capacity,
                    worker_wait_timeout,
                );
            })
            .map_err(BrushThreadRuntimeError::SpawnThread)?;

        Ok(Self {
            tool_set,
            active_tool,
            canvas_input_producer,
            brush_input_consumer,
            stop_requested,
            stroke_generation,
            finish_state,
            worker_error,
            thread: Some(thread),
        })
    }

    pub fn canvas_input_producer(&self) -> &MainCanvasInputProducer {
        &self.canvas_input_producer
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
        self.worker_error
            .lock()
            .expect("brush worker error state should not be poisoned")
            .take()
    }

    pub fn reset_active_stroke_processing(&self) {
        self.stroke_generation.fetch_add(1, Ordering::Relaxed);
        self.canvas_input_producer.clear();
        self.brush_input_consumer.clear();
    }

    pub fn finish_active_stroke_processing(&self) -> Result<(), BrushThreadRuntimeError> {
        let request = self.finish_state.request_finish();
        self.finish_state
            .wait_for_ack_or_error(request, &self.worker_error)
            .map_err(BrushThreadRuntimeError::Worker)?;
        Ok(())
    }

    pub fn shutdown(mut self) -> Result<(), BrushThreadRuntimeError> {
        self.stop_requested.store(true, Ordering::Relaxed);
        self.finish_state.notify_changed();
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
    }
}

fn run_brush_thread(
    mut worker: BrushWorker,
    canvas_input_consumer: BrushThreadCanvasInputConsumer,
    brush_input_producer: BrushThreadBrushInputProducer,
    active_tool: Arc<Mutex<ActiveTool>>,
    stop_requested: Arc<AtomicBool>,
    stroke_generation: Arc<AtomicU64>,
    finish_state: Arc<FinishSignal>,
    worker_error: Arc<Mutex<Option<BrushWorkerError>>>,
    worker_batch_capacity: usize,
    worker_wait_timeout: Duration,
) {
    let mut seen_stroke_generation = stroke_generation.load(Ordering::Relaxed);
    let mut seen_finish_request_generation = finish_state.request_generation();
    while !stop_requested.load(Ordering::Relaxed) {
        let current_generation = stroke_generation.load(Ordering::Relaxed);
        if current_generation != seen_stroke_generation {
            worker.reset_active_stroke();
            seen_stroke_generation = current_generation;
        }
        let current_finish_request_generation = finish_state.request_generation();
        if current_finish_request_generation != seen_finish_request_generation {
            if let Err(error) = worker.finish_active_stroke(&brush_input_producer) {
                store_worker_error(&worker_error, &finish_state, error);
                break;
            }
            seen_finish_request_generation = current_finish_request_generation;
            finish_state.ack_finish(current_finish_request_generation);
            continue;
        }
        let brush_id = match *active_tool
            .lock()
            .expect("active tool state should not be poisoned")
        {
            ActiveTool::Brush(brush_id) => brush_id,
        };
        if let Err(error) = worker.set_active_brush(brush_id) {
            store_worker_error(&worker_error, &finish_state, error);
            break;
        }
        match worker.process_canvas_input(
            &canvas_input_consumer,
            &brush_input_producer,
            worker_batch_capacity,
            worker_wait_timeout,
        ) {
            Ok(_) => {}
            Err(error) => {
                store_worker_error(&worker_error, &finish_state, error);
                break;
            }
        }
    }
}

fn store_worker_error(
    worker_error: &Mutex<Option<BrushWorkerError>>,
    finish_state: &FinishSignal,
    error: BrushWorkerError,
) {
    let mut stored_error = worker_error
        .lock()
        .expect("brush worker error state should not be poisoned");
    *stored_error = Some(error);
    finish_state.notify_changed();
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

        runtime.canvas_input_producer().push(CanvasInput {
            time_ns: 1,
            position: CanvasVec2::new(4.0, 5.0),
            pressure: 0.6,
            tilt: RadianVec2::new(0.0, 0.0),
            twist: 0.0,
        });
        runtime.canvas_input_producer().push(CanvasInput {
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
