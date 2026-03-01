/// Phase 4 Test Utilities
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use protocol::{GpuCmdMsg, GpuFeedbackFrame, InputRingSample};
use crate::runtime::{RuntimeCommand, RuntimeError, RuntimeReceipt};
use crate::sample_source::SampleSource;

pub struct FakeGpuRuntime {
    execute_count: Arc<AtomicUsize>,
    present_count: Arc<AtomicUsize>,
    should_fail: Arc<AtomicBool>,
    _fail_message: String,
}

pub struct FakeGpuRuntimeStats {
    pub execute_count: Arc<AtomicUsize>,
    pub present_count: Arc<AtomicUsize>,
}

impl FakeGpuRuntime {
    pub fn new() -> (Self, FakeGpuRuntimeStats) {
        let exec = Arc::new(AtomicUsize::new(0));
        let present = Arc::new(AtomicUsize::new(0));
        let should_fail = Arc::new(AtomicBool::new(false));
        let runtime = Self {
            execute_count: exec.clone(), present_count: present.clone(),
            should_fail: should_fail.clone(), _fail_message: String::from("injected failure"),
        };
        let stats = FakeGpuRuntimeStats { execute_count: exec, present_count: present };
        (runtime, stats)
    }

    pub fn execute(&mut self, cmd: RuntimeCommand) -> Result<RuntimeReceipt, RuntimeError> {
        self.execute_count.fetch_add(1, Ordering::SeqCst);
        if self.should_fail.load(Ordering::SeqCst) {
            return Err(RuntimeError::PresentError(renderer::PresentError::Surface(wgpu::SurfaceError::Lost)));
        }
        match cmd {
            RuntimeCommand::PresentFrame { .. } => {
                self.present_count.fetch_add(1, Ordering::SeqCst);
                Ok(RuntimeReceipt::FramePresented { executed_tile_count: 0 })
            }
            RuntimeCommand::Resize { .. } | RuntimeCommand::ResizeHandshake { .. } => Ok(RuntimeReceipt::Resized),
            RuntimeCommand::Init { .. } => Ok(RuntimeReceipt::InitComplete),
            RuntimeCommand::Shutdown { reason } => Ok(RuntimeReceipt::ShutdownAck { reason }),
            _ => Ok(RuntimeReceipt::InitComplete),
        }
    }
    pub fn drain_view_ops(&mut self) {}
}

pub struct MockSampleSource { samples: Vec<InputRingSample>, consumed_count: Arc<AtomicUsize> }
impl MockSampleSource {
    pub fn new() -> (Self, Arc<AtomicUsize>) {
        let consumed = Arc::new(AtomicUsize::new(0));
        (Self { samples: Vec::new(), consumed_count: consumed.clone() }, consumed)
    }
    pub fn with_samples(samples: Vec<InputRingSample>) -> (Self, Arc<AtomicUsize>) {
        let consumed = Arc::new(AtomicUsize::new(0));
        (Self { samples, consumed_count: consumed.clone() }, consumed)
    }
    pub fn add_sample(&mut self, sample: InputRingSample) { self.samples.push(sample); }
    pub fn consumed_count(&self) -> usize { self.consumed_count.load(Ordering::SeqCst) }
}
impl Default for MockSampleSource { fn default() -> Self { let (s, _) = Self::new(); s } }
impl SampleSource for MockSampleSource {
    fn drain_batch(&mut self, output: &mut Vec<InputRingSample>, budget: usize) {
        output.clear();
        for _ in 0..self.samples.len().min(budget) {
            if let Some(s) = self.samples.pop() { output.push(s); self.consumed_count.fetch_add(1, Ordering::SeqCst); }
        }
    }
}

pub fn create_test_channels<Command, Receipt, Error>(command_capacity: usize, feedback_capacity: usize)
    -> (engine::MainThreadChannels<Command, Receipt, Error>, engine::EngineThreadChannels<Command, Receipt, Error>)
where Command: Send + 'static, Receipt: Send + 'static, Error: Send + 'static,
{ engine::create_thread_channels(64, 16, command_capacity, feedback_capacity) }

pub struct DispatchFrameTestHarness {
    pub bridge: crate::engine_bridge::EngineBridge,
    pub engine_channels: engine::EngineThreadChannels<RuntimeCommand, RuntimeReceipt, RuntimeError>,
    pub fake_runtime: FakeGpuRuntime,
    pub stats: FakeGpuRuntimeStats,
}

impl DispatchFrameTestHarness {
    pub fn new(command_capacity: usize, feedback_capacity: usize) -> Self {
        let (fake_runtime, stats) = FakeGpuRuntime::new();
        let (bridge, engine_channels) = crate::engine_bridge::EngineBridge::new_for_test_with_executor(
            command_capacity, feedback_capacity, |_cmd| Ok(RuntimeReceipt::InitComplete));
        Self { bridge, engine_channels, fake_runtime, stats }
    }

    pub fn dispatch_frame(&mut self) -> Result<(), RuntimeError> {
        self.bridge.dispatch_frame_with_executor(|cmd| self.fake_runtime.execute(cmd))
    }

    pub fn push_command(&mut self, cmd: RuntimeCommand) -> Result<(), rtrb::PushError<protocol::GpuCmdMsg<RuntimeCommand>>> {
        self.engine_channels.gpu_command_sender.push(GpuCmdMsg::Command(cmd))
    }
    pub fn pop_feedback(&mut self) -> Result<GpuFeedbackFrame<RuntimeReceipt, RuntimeError>, rtrb::PopError> {
        self.engine_channels.gpu_feedback_receiver.pop()
    }
    pub fn waterlines(&self) -> crate::engine_bridge::MainThreadWaterlines { self.bridge.waterlines }
}

#[cfg(test)]
mod dispatch_frame_e2e_tests {
    use super::*;
    #[test]
    #[test]
    fn test_dispatch_frame_e2e_with_fake_runtime() {
        let mut h = DispatchFrameTestHarness::new(256, 64);
        let (tx, _rx) = std::sync::mpsc::channel();
        h.push_command(RuntimeCommand::Init { ack_sender: tx }).unwrap();
        h.push_command(RuntimeCommand::PresentFrame { frame_id: 1 }).unwrap();
        h.push_command(RuntimeCommand::Shutdown { reason: "test".into() }).unwrap();
        assert!(matches!(h.dispatch_frame(), Err(RuntimeError::ShutdownRequested { .. })));
        // Only PresentFrame calls the fake runtime executor (Init/Shutdown handled by EngineBridge)
        assert_eq!(h.stats.execute_count.load(Ordering::SeqCst), 1);
        assert_eq!(h.waterlines().executed_batch_waterline.0, 1);
        let fb = h.pop_feedback().unwrap();
        // Should have Init, PresentFrame, and ShutdownAck receipts
        assert_eq!(fb.receipts.len(), 3);
        assert_eq!(fb.executed_batch_waterline.0, 1);
    }
    #[test]
    fn test_dispatch_frame_empty_still_increments_waterline() {
        let mut h = DispatchFrameTestHarness::new(256, 64);
        assert!(h.dispatch_frame().is_ok());
        assert_eq!(h.waterlines().executed_batch_waterline.0, 1);
        let fb = h.pop_feedback().unwrap();
        assert_eq!(fb.receipts.len(), 0);
        assert_eq!(fb.executed_batch_waterline.0, 1);
    }
    #[test]
    fn test_dispatch_frame_multiple_calls_monotonic_waterline() {
        let mut h = DispatchFrameTestHarness::new(256, 64);
        for i in 1..=5 { assert!(h.dispatch_frame().is_ok()); assert_eq!(h.waterlines().executed_batch_waterline.0, i); }
    }
}

#[cfg(test)]
mod feedback_queue_full_tests {
    use super::*;
    #[test]
    fn test_feedback_queue_full_timeout_behavior() {
        let mut h = DispatchFrameTestHarness::new(256, 1);
        assert!(h.dispatch_frame().is_ok());
        #[cfg(not(debug_assertions))] { assert!(matches!(h.dispatch_frame(), Err(RuntimeError::FeedbackQueueTimeout))); }
    }
    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "feedback queue full")]
    fn test_feedback_queue_full_debug_panic() {
        let mut h = DispatchFrameTestHarness::new(256, 1);
        let _ = h.dispatch_frame(); let _ = h.dispatch_frame();
    }
    #[test]
    fn test_feedback_queue_recovers_after_drain() {
        let mut h = DispatchFrameTestHarness::new(256, 2);
        let _ = h.dispatch_frame(); let _ = h.dispatch_frame();
        let _ = h.pop_feedback(); let _ = h.pop_feedback();
        assert!(h.dispatch_frame().is_ok());
    }
}

#[cfg(test)]
mod queue_semantics_tests {
    use super::*; use protocol::GpuFeedbackFrame;
    #[test]
    fn test_feedback_merge_preserves_all_receipts() {
        let mut state = protocol::GpuFeedbackMergeState::<RuntimeReceipt, RuntimeError>::default();
        let f1 = GpuFeedbackFrame { present_frame_id: protocol::PresentFrameId(1), submit_waterline: protocol::SubmitWaterline(1),
            executed_batch_waterline: protocol::ExecutedBatchWaterline(1), complete_waterline: protocol::CompleteWaterline(1),
            receipts: vec![RuntimeReceipt::InitComplete].into(), errors: vec![].into() };
        let f2 = GpuFeedbackFrame { present_frame_id: protocol::PresentFrameId(2), submit_waterline: protocol::SubmitWaterline(2),
            executed_batch_waterline: protocol::ExecutedBatchWaterline(2), complete_waterline: protocol::CompleteWaterline(2),
            receipts: vec![RuntimeReceipt::Resized].into(), errors: vec![].into() };
        let m = GpuFeedbackFrame::merge_mailbox(f1, f2, &mut state);
        assert_eq!(m.receipts.len(), 2); assert_eq!(m.executed_batch_waterline.0, 2);
    }
    #[test]
    fn test_receipt_merge_key_stability() {
        let mut state = protocol::GpuFeedbackMergeState::<RuntimeReceipt, RuntimeError>::default();
        let f1 = GpuFeedbackFrame { present_frame_id: protocol::PresentFrameId(1), submit_waterline: protocol::SubmitWaterline(1),
            executed_batch_waterline: protocol::ExecutedBatchWaterline(1), complete_waterline: protocol::CompleteWaterline(1),
            receipts: vec![RuntimeReceipt::InitComplete].into(), errors: vec![].into() };
        let f2 = GpuFeedbackFrame { present_frame_id: protocol::PresentFrameId(2), submit_waterline: protocol::SubmitWaterline(2),
            executed_batch_waterline: protocol::ExecutedBatchWaterline(2), complete_waterline: protocol::CompleteWaterline(2),
            receipts: vec![RuntimeReceipt::InitComplete].into(), errors: vec![].into() };
        let m = GpuFeedbackFrame::merge_mailbox(f1, f2, &mut state);
        assert_eq!(m.receipts.len(), 1); assert_eq!(m.executed_batch_waterline.0, 2);
    }
}
