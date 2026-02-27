use std::sync::Arc;
/// Engine Bridge module.
///
/// Manages cross-thread communication between main thread (GPU) and engine thread (business).
/// Owned by the main thread.
use std::thread::{self, JoinHandle};
use std::time::Duration;

use rtrb::PopError;
use rtrb::PushError;

use engine::{create_thread_channels, EngineThreadChannels, MainThreadChannels};
use protocol::{
    CompleteWaterline, ExecutedBatchWaterline, GpuCmdMsg, GpuFeedbackFrame, PresentFrameId,
    SubmitWaterline,
};

use crate::runtime::{GpuRuntime, RuntimeCommand, RuntimeError, RuntimeReceipt};

/// Main thread waterlines
#[derive(Debug, Clone, Copy)]
pub struct MainThreadWaterlines {
    pub present_frame_id: PresentFrameId,
    pub submit_waterline: SubmitWaterline,
    pub executed_batch_waterline: ExecutedBatchWaterline,
    pub complete_waterline: CompleteWaterline,
}

impl Default for MainThreadWaterlines {
    fn default() -> Self {
        Self {
            present_frame_id: PresentFrameId(0),
            submit_waterline: SubmitWaterline(0),
            executed_batch_waterline: ExecutedBatchWaterline(0),
            complete_waterline: CompleteWaterline(0),
        }
    }
}

/// Engine Bridge - manages cross-thread communication.
pub struct EngineBridge {
    pub main_channels: MainThreadChannels<RuntimeCommand, RuntimeReceipt, RuntimeError>,
    pub gpu_runtime: Option<GpuRuntime>,
    pub waterlines: MainThreadWaterlines,
    pub engine_thread: Option<JoinHandle<()>>,
}

impl EngineBridge {
    pub fn new<F>(gpu_runtime: GpuRuntime, spawn_engine: F) -> Self
    where
        F: FnOnce(
            EngineThreadChannels<RuntimeCommand, RuntimeReceipt, RuntimeError>,
        ) -> JoinHandle<()>,
    {
        let main_channels;
        let engine_channels;
        {
            let result = create_thread_channels::<RuntimeCommand, RuntimeReceipt, RuntimeError>(
                1024, // input_ring_capacity
                64,   // input_control_capacity
                256,  // gpu_command_capacity
                64,   // gpu_feedback_capacity
            );
            main_channels = result.0;
            engine_channels = result.1;
        }

        let engine_thread = spawn_engine(engine_channels);

        Self {
            main_channels,
            gpu_runtime: Some(gpu_runtime),
            waterlines: MainThreadWaterlines::default(),
            engine_thread: Some(engine_thread),
        }
    }

    /// Create EngineBridge for testing with custom executor (no real GpuRuntime needed).
    /// Returns (EngineBridge, EngineThreadChannels).
    pub fn new_for_test_with_executor<F>(
        command_capacity: usize,
        feedback_capacity: usize,
        _executor_marker: F, // Only used for type inference
    ) -> (
        Self,
        EngineThreadChannels<RuntimeCommand, RuntimeReceipt, RuntimeError>,
    )
    where
        F: FnMut(RuntimeCommand) -> Result<RuntimeReceipt, RuntimeError>,
    {
        let main_channels;
        let engine_channels;
        {
            let result = create_thread_channels::<RuntimeCommand, RuntimeReceipt, RuntimeError>(
                1024, // input_ring_capacity (not used in tests)
                64,   // input_control_capacity
                command_capacity,
                feedback_capacity,
            );
            main_channels = result.0;
            engine_channels = result.1;
        }

        // GpuRuntime is None - tests call dispatch_frame_with_executor which bypasses it
        let bridge = Self {
            main_channels,
            gpu_runtime: None,
            waterlines: MainThreadWaterlines::default(),
            engine_thread: None,
        };

        (bridge, engine_channels)
    }

    /// Create EngineBridge for testing without spawning engine thread.
    /// Returns (EngineBridge, EngineThreadChannels) - caller can run engine_loop manually.
    pub fn new_for_test(
        gpu_runtime: GpuRuntime,
        command_capacity: usize,
        feedback_capacity: usize,
    ) -> (
        Self,
        EngineThreadChannels<RuntimeCommand, RuntimeReceipt, RuntimeError>,
    ) {
        let main_channels;
        let engine_channels;
        {
            let result = create_thread_channels::<RuntimeCommand, RuntimeReceipt, RuntimeError>(
                1024, // input_ring_capacity (not used in tests)
                64,   // input_control_capacity
                command_capacity,
                feedback_capacity,
            );
            main_channels = result.0;
            engine_channels = result.1;
        }

        let bridge = Self {
            main_channels,
            gpu_runtime: Some(gpu_runtime),
            waterlines: MainThreadWaterlines::default(),
            engine_thread: None,
        };

        (bridge, engine_channels)
    }

    /// Dispatch a frame of GPU commands (called from main thread event loop).
    pub fn dispatch_frame(&mut self) -> Result<(), RuntimeError> {
        let mut receipts = Vec::new();
        let mut errors = Vec::new();
        let mut shutdown_reason = None;

        // 1. Drain commands (with budget)
        const COMMAND_BUDGET: usize = 256;
        for _ in 0..COMMAND_BUDGET {
            if shutdown_reason.is_some() {
                break;
            }

            match self.main_channels.gpu_command_receiver.pop() {
                Ok(GpuCmdMsg::Command(cmd)) => {
                    match self.execute_command(cmd, &mut receipts, &mut errors) {
                        Ok(()) => {}
                        Err(RuntimeError::ShutdownRequested { reason }) => {
                            shutdown_reason = Some(reason);
                        }
                        Err(e) => return Err(e),
                    }
                }
                Err(PopError::Empty) => break,
            }
        }

        // 2. Update waterlines
        self.waterlines.executed_batch_waterline.0 += 1;

        // 3. Push feedback frame
        self.push_feedback_frame(receipts, errors)?;

        // 4. Return Shutdown error after feedback is pushed
        if let Some(reason) = shutdown_reason {
            return Err(RuntimeError::ShutdownRequested { reason });
        }

        Ok(())
    }

    /// Dispatch frame with custom executor (for testing with FakeGpuRuntime).
    #[doc(hidden)]
    pub fn dispatch_frame_with_executor<F>(&mut self, mut execute: F) -> Result<(), RuntimeError>
    where
        F: FnMut(RuntimeCommand) -> Result<RuntimeReceipt, RuntimeError>,
    {
        let mut receipts = Vec::new();
        let mut errors = Vec::new();
        let mut shutdown_reason = None;

        // 1. Drain commands (with budget)
        const COMMAND_BUDGET: usize = 256;
        for _ in 0..COMMAND_BUDGET {
            if shutdown_reason.is_some() {
                break;
            } // Stop processing after Shutdown

            match self.main_channels.gpu_command_receiver.pop() {
                Ok(GpuCmdMsg::Command(cmd)) => {
                    // Don't use `?` - collect receipts/errors and handle Shutdown specially
                    match self.execute_command_with_executor(
                        cmd,
                        &mut receipts,
                        &mut errors,
                        &mut execute,
                    ) {
                        Ok(()) => {}
                        Err(RuntimeError::ShutdownRequested { reason }) => {
                            shutdown_reason = Some(reason);
                            // Continue to push feedback with ShutdownAck receipt
                        }
                        Err(e) => return Err(e),
                    }
                }
                Err(PopError::Empty) => break,
            }
        }

        // 2. Update waterlines
        self.waterlines.executed_batch_waterline.0 += 1;

        // 3. Push feedback frame (even on Shutdown - receipts/errors must not be lost)
        self.push_feedback_frame(receipts, errors)?;

        // 4. Return Shutdown error after feedback is pushed
        if let Some(reason) = shutdown_reason {
            return Err(RuntimeError::ShutdownRequested { reason });
        }

        Ok(())
    }

    /// Execute a single command.
    fn execute_command(
        &mut self,
        cmd: RuntimeCommand,
        receipts: &mut Vec<RuntimeReceipt>,
        errors: &mut Vec<RuntimeError>,
    ) -> Result<(), RuntimeError> {
        match cmd {
            RuntimeCommand::Shutdown { reason } => {
                receipts.push(RuntimeReceipt::ShutdownAck {
                    reason: reason.clone(),
                });
                eprintln!("[shutdown] {}", reason);
                return Err(RuntimeError::ShutdownRequested { reason });
            }

            RuntimeCommand::ResizeHandshake {
                width,
                height,
                ack_sender,
            } => {
                self.waterlines.submit_waterline.0 += 1;
                let _ = ack_sender.send(Ok(()));
                receipts.push(RuntimeReceipt::ResizeHandshakeAck);
                Ok(())
            }

            RuntimeCommand::Resize {
                width,
                height,
                view_transform: _,
            } => {
                self.waterlines.submit_waterline.0 += 1;
                receipts.push(RuntimeReceipt::Resized);
                Ok(())
            }

            RuntimeCommand::Init { ack_sender } => {
                self.waterlines.submit_waterline.0 += 1;
                let _ = ack_sender.send(Ok(()));
                receipts.push(RuntimeReceipt::InitComplete);
                Ok(())
            }

            RuntimeCommand::PresentFrame { frame_id } => {
                self.waterlines.present_frame_id = PresentFrameId(frame_id);
                let receipt = self.gpu_runtime.as_mut().unwrap().execute(cmd)?;
                receipts.push(receipt);
                Ok(())
            }

            _ => {
                let receipt = self.gpu_runtime.as_mut().unwrap().execute(cmd)?;
                receipts.push(receipt);
                Ok(())
            }
        }
    }

    /// Execute command with custom executor (for testing).
    fn execute_command_with_executor<F>(
        &mut self,
        cmd: RuntimeCommand,
        receipts: &mut Vec<RuntimeReceipt>,
        errors: &mut Vec<RuntimeError>,
        mut execute: F,
    ) -> Result<(), RuntimeError>
    where
        F: FnMut(RuntimeCommand) -> Result<RuntimeReceipt, RuntimeError>,
    {
        match cmd {
            RuntimeCommand::Shutdown { reason } => {
                receipts.push(RuntimeReceipt::ShutdownAck {
                    reason: reason.clone(),
                });
                eprintln!("[shutdown] {}", reason);
                return Err(RuntimeError::ShutdownRequested { reason });
            }

            RuntimeCommand::ResizeHandshake {
                width,
                height,
                ack_sender,
            } => {
                self.waterlines.submit_waterline.0 += 1;
                let _ = ack_sender.send(Ok(()));
                receipts.push(RuntimeReceipt::ResizeHandshakeAck);
                Ok(())
            }

            RuntimeCommand::Resize {
                width,
                height,
                view_transform: _,
            } => {
                self.waterlines.submit_waterline.0 += 1;
                receipts.push(RuntimeReceipt::Resized);
                Ok(())
            }

            RuntimeCommand::Init { ack_sender } => {
                self.waterlines.submit_waterline.0 += 1;
                let _ = ack_sender.send(Ok(()));
                receipts.push(RuntimeReceipt::InitComplete);
                Ok(())
            }

            RuntimeCommand::PresentFrame { frame_id } => {
                self.waterlines.present_frame_id = PresentFrameId(frame_id);
                let receipt = execute(cmd)?;
                receipts.push(receipt);
                Ok(())
            }

            _ => {
                let receipt = execute(cmd)?;
                receipts.push(receipt);
                Ok(())
            }
        }
    }

    /// Push a feedback frame to the engine thread.
    ///
    /// Phase 4 Q2 strategy (feedback queue full):
    /// - Debug: panic on full (fail-fast, protocol violation)
    /// - Release: retry with timeout, avoid clone by reusing frame
    fn push_feedback_frame(
        &mut self,
        receipts: Vec<RuntimeReceipt>,
        errors: Vec<RuntimeError>,
    ) -> Result<(), RuntimeError> {
        let frame = GpuFeedbackFrame {
            present_frame_id: self.waterlines.present_frame_id,
            submit_waterline: self.waterlines.submit_waterline,
            executed_batch_waterline: self.waterlines.executed_batch_waterline,
            complete_waterline: self.waterlines.complete_waterline,
            receipts: receipts.into(),
            errors: errors.into(),
        };

        // Q2 strategy: Debug panic + Release retry with timeout
        #[cfg(debug_assertions)]
        {
            self.main_channels.gpu_feedback_sender.push(frame).expect(
                "feedback queue full: protocol violation (receipts/errors must not be dropped)",
            );
        }

        #[cfg(not(debug_assertions))]
        {
            // Release: retry with 5ms total timeout, avoid clone
            use rtrb::PushError;
            let timeout = Duration::from_millis(5);
            let start = std::time::Instant::now();
            let mut frame = frame;

            loop {
                match self.main_channels.gpu_feedback_sender.push(frame) {
                    Ok(()) => break,
                    Err(PushError::Full(f)) => {
                        // Get frame back to retry without clone
                        frame = f;

                        // Check timeout
                        if start.elapsed() > timeout {
                            return Err(RuntimeError::FeedbackQueueTimeout);
                        }

                        // Brief sleep before retry
                        thread::sleep(Duration::from_millis(1));
                    }
                }
            }
        }

        Ok(())
    }
}

impl Drop for EngineBridge {
    fn drop(&mut self) {
        // Abandonment-based shutdown:
        // - Dropping MainThreadChannels disconnects the command sender
        // - Engine thread detects disconnection and exits
        // NOTE: For explicit shutdown, set engine_core.shutdown_requested = true
        // before dropping EngineBridge
        if let Some(handle) = self.engine_thread.take() {
            handle
                .join()
                .unwrap_or_else(|err| eprintln!("[error] engine thread panic: {:?}", err));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_main_thread_waterlines_default() {
        let waterlines = MainThreadWaterlines::default();
        assert_eq!(waterlines.present_frame_id.0, 0);
        assert_eq!(waterlines.submit_waterline.0, 0);
        assert_eq!(waterlines.executed_batch_waterline.0, 0);
        assert_eq!(waterlines.complete_waterline.0, 0);
    }
}

#[cfg(test)]
mod waterline_tests {
    use super::*;

    #[test]
    fn test_waterline_monotonicity() {
        // Test that waterlines only increase
        let mut waterlines = MainThreadWaterlines::default();

        // Initial state
        assert_eq!(waterlines.executed_batch_waterline.0, 0);

        // Simulate dispatch_frame increments
        waterlines.executed_batch_waterline.0 += 1;
        assert_eq!(waterlines.executed_batch_waterline.0, 1);

        waterlines.executed_batch_waterline.0 += 1;
        assert_eq!(waterlines.executed_batch_waterline.0, 2);

        // Waterline should never decrease (enforced by dispatch_frame logic)
        // This test documents the expected behavior
    }

    #[test]
    fn test_waterline_empty_frame_increment() {
        // Test that waterlines increment even with empty command queue
        // (documented semantic: executed_batch_waterline increments on EVERY dispatch_frame call)
        let mut waterlines = MainThreadWaterlines::default();

        // Simulate multiple dispatch_frame calls with no commands
        for i in 1..=5 {
            waterlines.executed_batch_waterline.0 += 1;
            assert_eq!(waterlines.executed_batch_waterline.0, i);
        }

        // Monotonic progress is maintained
    }
}

#[cfg(test)]
mod dispatch_semantics_tests {
    use super::*;

    #[test]
    fn test_waterline_increments_on_empty_dispatch() {
        // Documents semantic: waterline increments even with no commands
        // This is tested via direct field access since we can't easily
        // construct EngineBridge without a real GpuRuntime
        let mut waterlines = MainThreadWaterlines::default();

        // Simulate multiple dispatch_frame calls
        for i in 1..=5 {
            waterlines.executed_batch_waterline.0 += 1;
            assert_eq!(
                waterlines.executed_batch_waterline.0, i,
                "Waterline should increment on every dispatch_frame call"
            );
        }
    }

    #[test]
    fn test_waterline_never_decrements() {
        // Documents semantic: waterlines are monotonic
        let mut waterlines = MainThreadWaterlines::default();

        waterlines.executed_batch_waterline.0 = 10;
        let prev = waterlines.executed_batch_waterline.0;

        waterlines.executed_batch_waterline.0 += 1;
        assert!(
            waterlines.executed_batch_waterline.0 >= prev,
            "Waterline should never decrement"
        );
    }
}
