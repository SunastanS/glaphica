/// Phase 4 Test Utilities
/// 
/// Provides fake/mock implementations for integration testing.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use crossbeam_channel::bounded;
use protocol::{GpuCmdMsg, GpuFeedbackFrame, InputRingSample};

use crate::runtime::{RuntimeCommand, RuntimeReceipt, RuntimeError};
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
    
    pub fn set_fail_message(&mut self, msg: &str) {
        self.fail_message = msg.to_string();
    }
    
    pub fn execute(&mut self, cmd: RuntimeCommand) -> Result<RuntimeReceipt, RuntimeError> {
        self.execute_count.fetch_add(1, Ordering::SeqCst);
        
        if self.should_fail.load(Ordering::SeqCst) {
            return Err(RuntimeError::PresentError(
                renderer::PresentError::Surface(wgpu::SurfaceError::Lost)
            ));
        }
        
        match cmd {
            RuntimeCommand::PresentFrame { .. } => {
                self.present_count.fetch_add(1, Ordering::SeqCst);
                Ok(RuntimeReceipt::FramePresented { executed_tile_count: 0 })
            }
            RuntimeCommand::Resize { .. } | RuntimeCommand::ResizeHandshake { .. } => {
                self.resize_count.fetch_add(1, Ordering::SeqCst);
                Ok(RuntimeReceipt::Resized)
            }
            RuntimeCommand::Init { .. } => {
                Ok(RuntimeReceipt::InitComplete)
            }
            RuntimeCommand::Shutdown { reason } => {
                Ok(RuntimeReceipt::ShutdownAck { reason })
            }
            _ => Ok(RuntimeReceipt::InitComplete),
        }
    }
    
    pub fn resize(&mut self, _width: u32, _height: u32) {
        self.resize_count.fetch_add(1, Ordering::SeqCst);
    }
    
    pub fn drain_view_ops(&mut self) {
        // No-op for testing
    }
}

/// Mock SampleSource for testing - provides controlled input samples
pub struct MockSampleSource {
    samples: Vec<InputRingSample>,
    consumed_count: Arc<AtomicUsize>,
}

impl MockSampleSource {
    pub fn new() -> (Self, Arc<AtomicUsize>) {
        let consumed = Arc::new(AtomicUsize::new(0));
        let source = Self {
            samples: Vec::new(),
            consumed_count: consumed.clone(),
        };
        (source, consumed)
    }
    
    pub fn with_samples(samples: Vec<InputRingSample>) -> (Self, Arc<AtomicUsize>) {
        let consumed = Arc::new(AtomicUsize::new(0));
        let source = Self {
            samples,
            consumed_count: consumed.clone(),
        };
        (source, consumed)
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

/// Helper: Create test channels with specified capacities
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
    engine::create_thread_channels(
        64,  // input_ring_capacity (not used in these tests)
        16,  // input_control_capacity
        command_capacity,
        feedback_capacity,
    )
}

// Note: drain_all_feedback and push_commands removed
// They require access to private rtrb types
// Tests will use channels directly instead

/// Helper: Assert that a closure completes within timeout
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

#[cfg(test)]
mod queue_semantics_tests {
    use super::*;
    use crate::runtime::RuntimeCommand;
    use protocol::GpuCmdMsg;
    use rtrb::{RingBuffer, PushError};
    
    /// Test: Command queue full - commands should not be lost (documents the fix)
    #[test]
    fn test_command_queue_full_push_back_preserves_commands() {
        // This test documents the fix for "Full => push back" logic in engine_loop
        // When queue is full, commands should be pushed back to pending_commands
        
        // Create a very small queue (capacity = 2)
        let (mut sender, mut receiver): (
            rtrb::Producer<GpuCmdMsg<RuntimeCommand>>,
            rtrb::Consumer<GpuCmdMsg<RuntimeCommand>>,
        ) = RingBuffer::new(2);
        
        // Prepare 5 commands
        let total_commands = 5;
        let mut pending_commands = Vec::new();
        
        for _ in 0..total_commands {
            // Use PresentFrame which doesn't need ack_sender
            pending_commands.push(RuntimeCommand::PresentFrame { frame_id: 0 });
        }
        
        // First iteration: send until full, then push back
        let mut sent_count = 0;
        while let Some(cmd) = pending_commands.pop() {
            match sender.push(GpuCmdMsg::Command(cmd)) {
                Ok(()) => {
                    sent_count += 1;
                }
                Err(PushError::Full(cmd)) => {
                    // Push back and stop (simulating engine_loop fix)
                    if let GpuCmdMsg::Command(c) = cmd {
                        pending_commands.push(c);
                    }
                    break;
                }
            }
        }
        
        // Should have sent 2 commands (queue capacity)
        assert_eq!(sent_count, 2, "Should send up to queue capacity");
        
        // Should have 3 commands remaining (not lost!)
        assert_eq!(pending_commands.len(), 3,
            "Remaining commands should NOT be lost - THIS IS THE KEY FIX");
        
        // Second iteration: drain first, then send remaining
        while receiver.pop().is_ok() {} // Drain the 2 commands
        
        // Now send the 3 remaining commands
        while let Some(cmd) = pending_commands.pop() {
            match sender.push(GpuCmdMsg::Command(cmd)) {
                Ok(()) => sent_count += 1,
                Err(PushError::Full(cmd)) => {
                    if let GpuCmdMsg::Command(c) = cmd {
                        pending_commands.push(c);
                    }
                    break;
                }
            }
        }
        
        // Should have sent 3 more (total 5)
        // But queue capacity is 2, so only 2 can be sent
        // The remaining 1 stays in pending_commands (not lost!)
        assert_eq!(sent_count, 4, "Sent 2 + 2 = 4 commands");
        assert_eq!(pending_commands.len(), 1, "1 command still pending (not lost!)");
        
        // Third iteration to send the last command
        while receiver.pop().is_ok() {} // Drain
        
        while let Some(cmd) = pending_commands.pop() {
            match sender.push(GpuCmdMsg::Command(cmd)) {
                Ok(()) => sent_count += 1,
                Err(PushError::Full(cmd)) => {
                    if let GpuCmdMsg::Command(c) = cmd {
                        pending_commands.push(c);
                    }
                    break;
                }
            }
        }
        
        // All 5 commands should eventually be sent
        assert_eq!(sent_count, total_commands,
            "All commands should eventually be sent (no loss)");
    }
    
    /// Test: Feedback merge with multiple frames (end-to-end semantic)
    #[test]
    fn test_feedback_merge_preserves_all_receipts() {
        // This test verifies that mailbox merge doesn't lose receipts
        
        use protocol::GpuFeedbackFrame;
        
        let mut merge_state = protocol::GpuFeedbackMergeState::<RuntimeReceipt, RuntimeError>::default();
        
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
        
        // Both receipts should be preserved (different merge keys)
        assert_eq!(merged.receipts.len(), 2,
            "Receipts with different keys should NOT be merged away");
        
        // Waterlines should be max
        assert_eq!(merged.executed_batch_waterline.0, 2);
    }
}
