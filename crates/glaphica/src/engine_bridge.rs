/// Engine Bridge module.
///
/// Manages cross-thread communication between main thread (GPU) and engine thread (business).
/// Owned by the main thread.

use std::thread::{self, JoinHandle};
use std::time::Duration;

use crossbeam_channel::bounded;
use crossbeam_queue::PopError;
use protocol::{
    PresentFrameId, SubmitWaterline, ExecutedBatchWaterline, CompleteWaterline,
    GpuFeedbackFrame, GpuCmdMsg,
};
use rtrb::{Producer, Consumer, PushError};

use crate::runtime::{GpuRuntime, RuntimeCommand, RuntimeReceipt, RuntimeError};
use engine::{create_thread_channels, MainThreadChannels, EngineThreadChannels};

/// Main thread waterlines
#[derive(Debug, Clone, Copy, Default)]
pub struct MainThreadWaterlines {
    pub present_frame_id: PresentFrameId,
    pub submit_waterline: SubmitWaterline,
    pub executed_batch_waterline: ExecutedBatchWaterline,
    pub complete_waterline: CompleteWaterline,
}

/// Engine Bridge - manages cross-thread communication.
/// 
/// This struct is owned by the main thread and manages:
/// - Channel endpoints for main thread
/// - GpuRuntime (GPU executor)
/// - Engine thread lifecycle
pub struct EngineBridge {
    // Main thread channels
    pub main_channels: MainThreadChannels<RuntimeCommand, RuntimeReceipt, RuntimeError>,
    
    // GPU runtime (main thread only)
    pub gpu_runtime: GpuRuntime,
    
    // Main thread waterlines
    pub waterlines: MainThreadWaterlines,
    
    // Engine thread handle
    pub engine_thread: Option<JoinHandle<()>>,
}

impl EngineBridge {
    /// Create a new EngineBridge with the given GPU runtime and spawn the engine thread.
    pub fn new<F>(
        gpu_runtime: GpuRuntime,
        spawn_engine: F,
    ) -> Self
    where
        F: FnOnce(EngineThreadChannels<RuntimeCommand, RuntimeReceipt, RuntimeError>) -> JoinHandle<()>,
    {
        // Create channels
        let (main_channels, engine_channels) = create_thread_channels(
            input_ring_capacity: 1024,
            input_control_capacity: 64,
            gpu_command_capacity: 256,
            gpu_feedback_capacity: 64,
        );
        
        // Spawn engine thread
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
                Err(PopError::Disconnected) => {
                    return Err(RuntimeError::EngineThreadDisconnected);
                }
            }
        }
        
        // 2. Update waterlines
        self.waterlines.executed_batch_waterline.0 += 1;
        
        // 3. Push feedback frame (MUST SUCCEED - protocol invariant)
        self.push_feedback_frame_all(receipts, errors)?;
        
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
                // Send final feedback before shutdown
                receipts.push(RuntimeReceipt::ShutdownAck { reason: reason.clone() });
                self.push_feedback_frame_all(receipts, errors)?;
                eprintln!("[shutdown] {}", reason);
                Err(RuntimeError::ShutdownRequested { reason })
            }
            
            RuntimeCommand::Init { ack_sender } => {
                let result = self.gpu_runtime.initialize();
                let _ = ack_sender.send(result.clone());
                if result.is_ok() {
                    receipts.push(RuntimeReceipt::InitComplete);
                }
                Ok(())
            }
            
            RuntimeCommand::Resize { width, height, ack_sender } => {
                let result = self.gpu_runtime.resize(width, height);
                let _ = ack_sender.send(result.clone());
                if result.is_ok() {
                    receipts.push(RuntimeReceipt::ResizeComplete);
                }
                result.map_err(RuntimeError::from)
            }
            
            // TODO: Handle other commands
            _ => {
                let receipt = self.gpu_runtime.execute(cmd)?;
                receipts.push(receipt);
                Ok(())
            }
        }
    }
    
    /// Push a feedback frame to the engine thread.
    /// 
    /// Phase 4 Q2 strategy:
    /// - Debug: panic on full (fail-fast)
    /// - Release: timeout blocking with graceful degradation
    fn push_feedback_frame_all(
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
        
        // Q2 strategy: Debug panic + Release timeout
        #[cfg(debug_assertions)]
        {
            self.main_channels.gpu_feedback_sender.push(frame)
                .expect("feedback queue full: protocol violation");
        }
        
        #[cfg(not(debug_assertions))]
        {
            // Release: timeout blocking (5ms)
            let timeout = Duration::from_millis(5);
            loop {
                match self.main_channels.gpu_feedback_sender.push(frame.clone()) {
                    Ok(()) => break,
                    Err(PushError::Full(_)) => {
                        thread::sleep(timeout);
                        // After timeout, trigger shutdown
                        return Err(RuntimeError::FeedbackQueueTimeout);
                    }
                }
            }
        }
        
        Ok(())
    }
}

impl Drop for EngineBridge {
    fn drop(&mut self) {
        // 1. Send shutdown command
        let _ = self.main_channels.gpu_command_sender.push(
            GpuCmdMsg::Command(RuntimeCommand::Shutdown { 
                reason: "EngineBridge dropped".to_string() 
            })
        );
        
        // 2. Join engine thread
        if let Some(handle) = self.engine_thread.take() {
            handle.join()
                .unwrap_or_else(|err| eprintln!("[error] engine thread panic: {:?}", err));
        }
    }
}
