/// Phase 4 Test Utilities
///
/// Provides fake/mock implementations for integration testing.
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use protocol::{GpuCmdMsg, GpuFeedbackFrame, InputRingSample};

use crate::runtime::{RuntimeCommand, RuntimeError, RuntimeReceipt};
use crate::sample_source::SampleSource;

/// Fake GpuRuntime for testing - records executed commands without real GPU
pub struct FakeGpuRuntime {
    execute_count: Arc<AtomicUsize>,
    resize_count: Arc<AtomicUsize>,
    present_count: Arc<AtomicUsize>,
    should_fail: Arc<AtomicBool>,
    fail_message: String,
}

pub struct FakeGpuRuntimeStats {
    pub execute_count: Arc<AtomicUsize>,
    pub resize_count: Arc<AtomicUsize>,
    pub present_count: Arc<AtomicUsize>,
}

impl FakeGpuRuntime {
    pub fn new() -> (Self, FakeGpuRuntimeStats) {
        let exec = Arc::new(AtomicUsize::new(0));
        let resize = Arc::new(AtomicUsize::new(0));
        let present = Arc::new(AtomicUsize::new(0));
        let should_fail = Arc::new(AtomicBool::new(false));

        let runtime = Self {
            execute_count: exec.clone(),
            resize_count: resize.clone(),
            present_count: present.clone(),
            should_fail: should_fail.clone(),
            fail_message: String::from("injected failure"),
        };

        let stats = FakeGpuRuntimeStats {
            execute_count: exec,
            resize_count: resize,
            present_count: present,
        };

        (runtime, stats)
    }

    pub fn set_should_fail(&mut self, fail: bool) {
        self.should_fail.store(fail, Ordering::SeqCst);
    }

    pub fn execute(&mut self, cmd: RuntimeCommand) -> Result<RuntimeReceipt, RuntimeError> {
        self.execute_count.fetch_add(1, Ordering::SeqCst);

        if self.should_fail.load(Ordering::SeqCst) {
            return Err(RuntimeError::PresentError(renderer::PresentError::Surface(
                wgpu::SurfaceError::Lost,
            )));
        }

        match cmd {
            RuntimeCommand::PresentFrame { .. } => {
                self.present_count.fetch_add(1, Ordering::SeqCst);
                Ok(RuntimeReceipt::FramePresented {
                    executed_tile_count: 0,
                })
            }
            RuntimeCommand::Resize { .. } | RuntimeCommand::ResizeHandshake { .. } => {
                self.resize_count.fetch_add(1, Ordering::SeqCst);
                Ok(RuntimeReceipt::Resized)
            }
            RuntimeCommand::Init { .. } => Ok(RuntimeReceipt::InitComplete),
            RuntimeCommand::Shutdown { reason } => Ok(RuntimeReceipt::ShutdownAck { reason }),
            _ => Ok(RuntimeReceipt::InitComplete),
        }
    }

    pub fn drain_view_ops(&mut self) {}
}

/// Mock SampleSource for testing
pub struct MockSampleSource {
    samples: Vec<InputRingSample>,
    consumed_count: Arc<AtomicUsize>,
}

impl MockSampleSource {
    pub fn new() -> (Self, Arc<AtomicUsize>) {
        let consumed = Arc::new(AtomicUsize::new(0));
        (
            Self {
                samples: Vec::new(),
                consumed_count: consumed.clone(),
            },
            consumed,
        )
    }

    pub fn with_samples(samples: Vec<InputRingSample>) -> (Self, Arc<AtomicUsize>) {
        let consumed = Arc::new(AtomicUsize::new(0));
        (
            Self {
                samples,
                consumed_count: consumed.clone(),
            },
            consumed,
        )
    }

    pub fn add_sample(&mut self, sample: InputRingSample) {
        self.samples.push(sample);
    }

    pub fn consumed_count(&self) -> usize {
        self.consumed_count.load(Ordering::SeqCst)
    }
}

impl Default for MockSampleSource {
    fn default() -> Self {
        let (source, _) = Self::new();
        source
    }
}

impl SampleSource for MockSampleSource {
    fn drain_batch(&mut self, output: &mut Vec<InputRingSample>, budget: usize) {
        output.clear();
        let count = self.samples.len().min(budget);
        for _ in 0..count {
            if let Some(sample) = self.samples.pop() {
                output.push(sample);
                self.consumed_count.fetch_add(1, Ordering::SeqCst);
            }
        }
    }
}

/// Helper: Create test channels
pub fn create_test_channels<Command, Receipt, Error>(
    command_capacity: usize,
    feedback_capacity: usize,
) -> (
    engine::MainThreadChannels<Command, Receipt, Error>,
    engine::EngineThreadChannels<Command, Receipt, Error>,
)
where
    Command: Send + 'static,
    Receipt: Send + 'static,
    Error: Send + 'static,
{
    engine::create_thread_channels(64, 16, command_capacity, feedback_capacity)
}

/// Helper: Assert closure completes within timeout
pub fn assert_within_timeout<F, R>(f: F, timeout: Duration, message: &str) -> R
where
    F: FnOnce() -> R,
{
    let start = std::time::Instant::now();
    let result = f();
    assert!(
        start.elapsed() < timeout,
        "Operation timed out: {} (took {:?}, limit {:?})",
        message,
        start.elapsed(),
        timeout
    );
    result
}

/// Test harness for dispatch_frame with FakeGpuRuntime
pub struct DispatchFrameTestHarness {
    pub main_channels: engine::MainThreadChannels<RuntimeCommand, RuntimeReceipt, RuntimeError>,
    pub engine_channels: engine::EngineThreadChannels<RuntimeCommand, RuntimeReceipt, RuntimeError>,
    pub waterlines: crate::engine_bridge::MainThreadWaterlines,
    pub fake_runtime: FakeGpuRuntime,
    pub stats: FakeGpuRuntimeStats,
}

impl DispatchFrameTestHarness {
    pub fn new(command_capacity: usize, feedback_capacity: usize) -> Self {
        let (fake_runtime, stats) = FakeGpuRuntime::new();
        let (main_channels, engine_channels) = create_test_channels::<
            RuntimeCommand,
            RuntimeReceipt,
            RuntimeError,
        >(command_capacity, feedback_capacity);

        Self {
            main_channels,
            engine_channels,
            waterlines: crate::engine_bridge::MainThreadWaterlines::default(),
            fake_runtime,
            stats,
        }
    }

    pub fn dispatch_frame(&mut self) -> Result<(), RuntimeError> {
        use rtrb::PopError;

        let mut receipts = Vec::new();
        let mut errors = Vec::new();

        const COMMAND_BUDGET: usize = 256;
        for _ in 0..COMMAND_BUDGET {
            match self.main_channels.gpu_command_receiver.pop() {
                Ok(GpuCmdMsg::Command(cmd)) => match self.fake_runtime.execute(cmd) {
                    Ok(receipt) => receipts.push(receipt),
                    Err(error) => errors.push(error),
                },
                Err(PopError::Empty) => break,
            }
        }

        self.waterlines.executed_batch_waterline.0 += 1;
        self.push_feedback_frame(receipts, errors)?;
        Ok(())
    }

    pub fn push_command(
        &mut self,
        cmd: RuntimeCommand,
    ) -> Result<(), rtrb::PushError<protocol::GpuCmdMsg<RuntimeCommand>>> {
        self.engine_channels
            .gpu_command_sender
            .push(GpuCmdMsg::Command(cmd))
    }

    pub fn pop_feedback(
        &mut self,
    ) -> Result<GpuFeedbackFrame<RuntimeReceipt, RuntimeError>, rtrb::PopError> {
        self.engine_channels.gpu_feedback_receiver.pop()
    }

    pub fn waterlines(&self) -> crate::engine_bridge::MainThreadWaterlines {
        self.waterlines
    }

    fn push_feedback_frame(
        &mut self,
        receipts: Vec<RuntimeReceipt>,
        errors: Vec<RuntimeError>,
    ) -> Result<(), RuntimeError> {
        use rtrb::PushError;

        let frame = GpuFeedbackFrame {
            present_frame_id: self.waterlines.present_frame_id,
            submit_waterline: self.waterlines.submit_waterline,
            executed_batch_waterline: self.waterlines.executed_batch_waterline,
            complete_waterline: self.waterlines.complete_waterline,
            receipts: receipts.into(),
            errors: errors.into(),
        };

        #[cfg(debug_assertions)]
        {
            self.main_channels
                .gpu_feedback_sender
                .push(frame)
                .expect("feedback queue full: protocol violation");
        }

        #[cfg(not(debug_assertions))]
        {
            let timeout = Duration::from_millis(5);
            let start = std::time::Instant::now();
            let mut frame = frame;

            loop {
                match self.main_channels.gpu_feedback_sender.push(frame) {
                    Ok(()) => break,
                    Err(PushError::Full(f)) => {
                        frame = f;
                        if start.elapsed() > timeout {
                            return Err(RuntimeError::FeedbackQueueTimeout);
                        }
                        std::thread::sleep(Duration::from_millis(1));
                    }
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod dispatch_frame_e2e_tests {
    use super::*;

    #[test]
    fn test_dispatch_frame_e2e_with_fake_runtime() {
        let mut harness = DispatchFrameTestHarness::new(256, 64);

        let (init_tx, _init_rx) = std::sync::mpsc::channel();
        harness
            .push_command(RuntimeCommand::Init {
                ack_sender: init_tx,
            })
            .unwrap();
        harness
            .push_command(RuntimeCommand::PresentFrame { frame_id: 1 })
            .unwrap();
        harness
            .push_command(RuntimeCommand::Shutdown {
                reason: "test".to_string(),
            })
            .unwrap();

        // Dispatch returns Ok because FakeGpuRuntime handles Shutdown as Ok
        // (EngineBridge::execute_command handles Shutdown specially)
        let result = harness.dispatch_frame();
        assert!(result.is_ok());
        assert_eq!(harness.stats.execute_count.load(Ordering::SeqCst), 3);
        assert_eq!(harness.waterlines().executed_batch_waterline.0, 1);

        let feedback = harness.pop_feedback();
        assert!(feedback.is_ok());
        let fb = feedback.unwrap();
        assert_eq!(fb.receipts.len(), 3);
        assert_eq!(fb.executed_batch_waterline.0, 1);
    }

    #[test]
    fn test_dispatch_frame_empty_still_increments_waterline() {
        let mut harness = DispatchFrameTestHarness::new(256, 64);
        let result = harness.dispatch_frame();
        assert!(result.is_ok());
        assert_eq!(harness.waterlines().executed_batch_waterline.0, 1);

        let feedback = harness.pop_feedback();
        assert!(feedback.is_ok());
        let fb = feedback.unwrap();
        assert_eq!(fb.receipts.len(), 0);
        assert_eq!(fb.executed_batch_waterline.0, 1);
    }
}

#[cfg(test)]
mod feedback_queue_full_tests {
    use super::*;

    #[test]
    fn test_feedback_queue_full_timeout_behavior() {
        let mut harness = DispatchFrameTestHarness::new(256, 1);
        assert!(harness.dispatch_frame().is_ok());

        #[cfg(not(debug_assertions))]
        {
            let result = harness.dispatch_frame();
            assert!(matches!(result, Err(RuntimeError::FeedbackQueueTimeout)));
        }
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "feedback queue full")]
    fn test_feedback_queue_full_debug_panic() {
        let mut harness = DispatchFrameTestHarness::new(256, 1);
        let _ = harness.dispatch_frame();
        let _ = harness.dispatch_frame();
    }

    #[test]
    fn test_feedback_queue_recovers_after_drain() {
        let mut harness = DispatchFrameTestHarness::new(256, 2);
        let _ = harness.dispatch_frame();
        let _ = harness.dispatch_frame();
        let _ = harness.pop_feedback();
        let _ = harness.pop_feedback();
        assert!(harness.dispatch_frame().is_ok());
    }
}

#[cfg(test)]
mod queue_semantics_tests {
    use super::*;
    use protocol::GpuFeedbackFrame;

    #[test]
    fn test_feedback_merge_preserves_all_receipts() {
        let mut merge_state =
            protocol::GpuFeedbackMergeState::<RuntimeReceipt, RuntimeError>::default();

        let frame1 = GpuFeedbackFrame {
            present_frame_id: protocol::PresentFrameId(1),
            submit_waterline: protocol::SubmitWaterline(1),
            executed_batch_waterline: protocol::ExecutedBatchWaterline(1),
            complete_waterline: protocol::CompleteWaterline(1),
            receipts: vec![RuntimeReceipt::InitComplete].into(),
            errors: vec![].into(),
        };

        let frame2 = GpuFeedbackFrame {
            present_frame_id: protocol::PresentFrameId(2),
            submit_waterline: protocol::SubmitWaterline(2),
            executed_batch_waterline: protocol::ExecutedBatchWaterline(2),
            complete_waterline: protocol::CompleteWaterline(2),
            receipts: vec![RuntimeReceipt::Resized].into(),
            errors: vec![].into(),
        };

        let merged = GpuFeedbackFrame::merge_mailbox(frame1, frame2, &mut merge_state);
        assert_eq!(merged.receipts.len(), 2);
        assert_eq!(merged.executed_batch_waterline.0, 2);
    }

    #[test]
    fn test_receipt_merge_key_stability() {
        let mut merge_state =
            protocol::GpuFeedbackMergeState::<RuntimeReceipt, RuntimeError>::default();

        let frame1 = GpuFeedbackFrame {
            present_frame_id: protocol::PresentFrameId(1),
            submit_waterline: protocol::SubmitWaterline(1),
            executed_batch_waterline: protocol::ExecutedBatchWaterline(1),
            complete_waterline: protocol::CompleteWaterline(1),
            receipts: vec![RuntimeReceipt::InitComplete].into(),
            errors: vec![].into(),
        };

        let frame2 = GpuFeedbackFrame {
            present_frame_id: protocol::PresentFrameId(2),
            submit_waterline: protocol::SubmitWaterline(2),
            executed_batch_waterline: protocol::ExecutedBatchWaterline(2),
            complete_waterline: protocol::CompleteWaterline(2),
            receipts: vec![RuntimeReceipt::InitComplete].into(),
            errors: vec![].into(),
        };

        let merged = GpuFeedbackFrame::merge_mailbox(frame1, frame2, &mut merge_state);
        assert_eq!(merged.receipts.len(), 1);
        assert_eq!(merged.executed_batch_waterline.0, 2);
    }
}
