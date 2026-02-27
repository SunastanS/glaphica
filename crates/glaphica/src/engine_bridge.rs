/// Engine Bridge module.
///
/// Manages cross-thread communication between main thread (GPU) and engine thread (business).
/// Owned by the main thread.

use std::thread::{self, JoinHandle};
use std::time::Duration;
use std::sync::Arc;

use crossbeam_channel::{self as crossbeam_channel, bounded};
use crossbeam_queue;
use rtrb;

use crate::protocol::{
    PresentFrameId, SubmitWaterline, ExecutedBatchWaterline, CompleteWaterline,
    GpuFeedbackFrame, GpuCmdMsg,
};
use crate::engine::{create_thread_channels, MainThreadChannels, EngineThreadChannels};

use crate::runtime::{GpuRuntime, RuntimeCommand, RuntimeReceipt, RuntimeError};

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
