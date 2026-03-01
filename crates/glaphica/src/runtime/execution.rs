use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use engine::MainThreadChannels;
use protocol::{GpuCmdMsg, GpuFeedbackFrame};
use rtrb::{Consumer, Producer, PushError};
use smallvec::SmallVec;

use super::{GpuRuntime, RuntimeCommand, RuntimeError, RuntimeReceipt};

pub struct GpuRuntimeThread {
    stop_requested: Arc<AtomicBool>,
}

impl GpuRuntimeThread {
    pub fn new() -> Self {
        Self {
            stop_requested: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn request_shutdown(&self) {
        self.stop_requested.store(true, Ordering::Release);
    }
}

impl Default for GpuRuntimeThread {
    fn default() -> Self {
        Self::new()
    }
}

pub fn run_runtime_loop(
    stop_requested: Arc<AtomicBool>,
    mut command_consumer: Consumer<GpuCmdMsg<RuntimeCommand>>,
    mut feedback_producer: Producer<GpuFeedbackFrame<RuntimeReceipt, RuntimeError>>,
    mut gpu_runtime: GpuRuntime,
) {
    const IDLE_SLEEP_DURATION: Duration = Duration::from_millis(1);

    let mut waterlines = RuntimeWaterlines::new();
    let mut receipts = SmallVec::<[RuntimeReceipt; 4]>::new();
    let mut errors = SmallVec::<[RuntimeError; 4]>::new();

    while !stop_requested.load(Ordering::Acquire) {
        const COMMAND_BUDGET: usize = 256;
        let mut shutdown_reason = None;

        for _ in 0..COMMAND_BUDGET {
            if shutdown_reason.is_some() {
                break;
            }

            match command_consumer.pop() {
                Ok(GpuCmdMsg::Command(cmd)) => {
                    match execute_command(
                        cmd,
                        &mut gpu_runtime,
                        &mut receipts,
                        &mut errors,
                        &mut waterlines,
                    ) {
                        Ok(()) => {}
                        Err(RuntimeError::ShutdownRequested { reason }) => {
                            shutdown_reason = Some(reason);
                        }
                        Err(e) => {
                            errors.push(e);
                        }
                    }
                }
                Err(rtrb::PopError::Empty) => {
                    break;
                }
            }
        }

        waterlines.executed_batch_waterline.0 += 1;

        if !receipts.is_empty() || !errors.is_empty() || shutdown_reason.is_some() {
            let frame = GpuFeedbackFrame {
                present_frame_id: waterlines.present_frame_id,
                submit_waterline: waterlines.submit_waterline,
                executed_batch_waterline: waterlines.executed_batch_waterline,
                complete_waterline: waterlines.complete_waterline,
                receipts,
                errors,
            };

            if let Err(e) = push_feedback(&mut feedback_producer, frame) {
                eprintln!("[gpu_runtime] feedback push error: {:?}", e);
            }

            receipts = SmallVec::new();
            errors = SmallVec::new();
        }

        if let Some(reason) = shutdown_reason {
            eprintln!("[gpu_runtime] shutdown requested: {}", reason);
            return;
        }

        thread::sleep(IDLE_SLEEP_DURATION);
    }
}

struct RuntimeWaterlines {
    present_frame_id: protocol::PresentFrameId,
    submit_waterline: protocol::SubmitWaterline,
    executed_batch_waterline: protocol::ExecutedBatchWaterline,
    complete_waterline: protocol::CompleteWaterline,
}

impl RuntimeWaterlines {
    fn new() -> Self {
        Self {
            present_frame_id: protocol::PresentFrameId(0),
            submit_waterline: protocol::SubmitWaterline(0),
            executed_batch_waterline: protocol::ExecutedBatchWaterline(0),
            complete_waterline: protocol::CompleteWaterline(0),
        }
    }
}

fn execute_command(
    cmd: RuntimeCommand,
    gpu_runtime: &mut GpuRuntime,
    receipts: &mut SmallVec<[RuntimeReceipt; 4]>,
    _errors: &mut SmallVec<[RuntimeError; 4]>,
    waterlines: &mut RuntimeWaterlines,
) -> Result<(), RuntimeError> {
    match &cmd {
        RuntimeCommand::PresentFrame { frame_id } => {
            waterlines.present_frame_id = protocol::PresentFrameId(*frame_id);
        }
        _ => {
            waterlines.submit_waterline.0 += 1;
        }
    }

    let receipt = gpu_runtime.execute(cmd)?;
    receipts.push(receipt);
    Ok(())
}

fn push_feedback(
    producer: &mut Producer<GpuFeedbackFrame<RuntimeReceipt, RuntimeError>>,
    frame: GpuFeedbackFrame<RuntimeReceipt, RuntimeError>,
) -> Result<(), RuntimeError> {
    #[cfg(debug_assertions)]
    {
        producer.push(frame).expect(
            "feedback queue full: protocol violation (receipts/errors must not be dropped)",
        );
    }

    #[cfg(not(debug_assertions))]
    {
        let timeout = Duration::from_millis(5);
        let start = std::time::Instant::now();
        let mut frame = frame;

        loop {
            match producer.push(frame) {
                Ok(()) => break,
                Err(PushError::Full(f)) => {
                    frame = f;
                    if start.elapsed() > timeout {
                        return Err(RuntimeError::FeedbackQueueTimeout);
                    }
                    thread::sleep(Duration::from_millis(1));
                }
            }
        }
    }

    Ok(())
}
