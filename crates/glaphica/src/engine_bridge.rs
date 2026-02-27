/// Engine Bridge module.
///
/// Manages cross-thread communication between main thread (GPU) and engine thread (business).
/// Owned by the main thread.

use std::thread::{self, JoinHandle};
use std::time::Duration;
use std::sync::Arc;

use crossbeam_channel::bounded;
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
}

impl Drop for EngineBridge {
    fn drop(&mut self) {
        // Abandonment-based shutdown:
        // - Dropping MainThreadChannels disconnects the command sender
        // - Engine thread detects disconnection and exits
        // NOTE: For explicit shutdown, call engine_core.shutdown_requested = true
        // before dropping EngineBridge
        if let Some(handle) = self.engine_thread.take() {
            handle.join()
                .unwrap_or_else(|err| eprintln!("[error] engine thread panic: {:?}", err));
        }
    }
}
