/// Engine Bridge module.
///
/// Manages cross-thread communication between main thread (GPU) and engine thread (business).
/// Owned by the main thread.

use std::thread::{self, JoinHandle};
use std::time::Duration;
use std::sync::Arc;

use rtrb::PopError;
use rtrb::PushError;

use protocol::{
    PresentFrameId, SubmitWaterline, ExecutedBatchWaterline, CompleteWaterline,
    GpuFeedbackFrame, GpuCmdMsg,
};
use engine::{create_thread_channels, MainThreadChannels, EngineThreadChannels};

use crate::runtime::{GpuRuntime, RuntimeCommand, RuntimeReceipt, RuntimeError};

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
    pub gpu_runtime: GpuRuntime,
    pub waterlines: MainThreadWaterlines,
    pub engine_thread: Option<JoinHandle<()>>,
}

impl EngineBridge {
    pub fn new<F>(
        gpu_runtime: GpuRuntime,
        spawn_engine: F,
    ) -> Self
    where
        F: FnOnce(EngineThreadChannels<RuntimeCommand, RuntimeReceipt, RuntimeError>) -> JoinHandle<()>,
    {
        let main_channels;
        let engine_channels;
        {
            let result = create_thread_channels::<RuntimeCommand, RuntimeReceipt, RuntimeError>(
                1024,  // input_ring_capacity
                64,    // input_control_capacity
                256,   // gpu_command_capacity
                64,    // gpu_feedback_capacity
            );
            main_channels = result.0;
            engine_channels = result.1;
        }
        
        let engine_thread = spawn_engine(engine_channels);
        
        Self {
            main_channels,
            gpu_runtime,
            waterlines: MainThreadWaterlines::default(),
            engine_thread: Some(engine_thread),
        }
    }
    
    /// Dispatch a frame of GPU commands (called from main thread event loop).
    /// 
    /// This function:
    /// 1. Drains commands from the command queue (with budget)
    /// 2. Executes each command on GpuRuntime
    /// 3. Collects receipts/errors
    /// 4. Updates waterlines
    /// 5. Pushes feedback frame (MUST SUCCEED - protocol invariant)
    pub fn dispatch_frame(&mut self) -> Result<(), RuntimeError> {
        let mut receipts = Vec::new();
        let mut errors = Vec::new();
        
        // 1. Drain commands (with budget)
        const COMMAND_BUDGET: usize = 256;
        for _ in 0..COMMAND_BUDGET {
            match self.main_channels.gpu_command_receiver.pop() {
                Ok(GpuCmdMsg::Command(cmd)) => {
                    self.execute_command(cmd, &mut receipts, &mut errors)?;
                }
                Err(PopError::Empty) => break,
            }
        }
        
        // 2. Update waterlines
        // SEMANTIC: executed_batch_waterline increments on EVERY dispatch_frame call,
        // regardless of whether any commands were executed. This ensures monotonic
        // progress tracking even for empty frames.
        self.waterlines.executed_batch_waterline.0 += 1;
        
        // 3. Push feedback frame (MUST SUCCEED - protocol invariant)
        self.push_feedback_frame(receipts, errors)?;
        
        Ok(())
    }
    
    /// Execute a single command and collect receipts/errors.
    fn execute_command(
        &mut self, 
        cmd: RuntimeCommand, 
        receipts: &mut Vec<RuntimeReceipt>,
        errors: &mut Vec<RuntimeError>,
    ) -> Result<(), RuntimeError> {
        match cmd {
            RuntimeCommand::Shutdown { reason } => {
                // Send feedback before shutdown to ensure receipts/errors are not lost
                receipts.push(RuntimeReceipt::ShutdownAck { reason: reason.clone() });
                eprintln!("[shutdown] {}", reason);
                // Push feedback with shutdown ack, then return error
                // Caller should handle ShutdownRequested by stopping render loop
                return Err(RuntimeError::ShutdownRequested { reason })
            }
            
            RuntimeCommand::ResizeHandshake { width, height, ack_sender } => {
                self.waterlines.submit_waterline.0 += 1;
                // For now, just acknowledge (execute will handle actual resize)
                let _ = ack_sender.send(Ok(()));
                receipts.push(RuntimeReceipt::ResizeHandshakeAck);
                Ok(())
            }
            
            RuntimeCommand::Resize { width, height, view_transform: _ } => {
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
            
            // TODO: Handle other commands
            RuntimeCommand::PresentFrame { frame_id } => {
                self.waterlines.present_frame_id = PresentFrameId(frame_id);
                let receipt = self.gpu_runtime.execute(cmd)?;
                receipts.push(receipt);
                Ok(())
            }
            
            _ => {
                let receipt = self.gpu_runtime.execute(cmd)?;
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
            self.main_channels.gpu_feedback_sender.push(frame)
                .expect("feedback queue full: protocol violation (receipts/errors must not be dropped)");
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
            handle.join()
                .unwrap_or_else(|err| eprintln!("[error] engine thread panic: {:?}", err));
        }
    }
}
