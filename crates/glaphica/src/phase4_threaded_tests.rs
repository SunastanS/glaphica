//! Phase 4 Threaded Mode Tests
//!
//! These tests validate the threaded execution mode where EngineBridge
//! handles cross-thread communication between main thread and engine thread.

#[cfg(test)]
mod tests {
    use crate::engine_bridge::EngineBridge;
    use crate::engine_core::{EngineCore, engine_loop};
    use crate::runtime::{RuntimeCommand, RuntimeReceipt, RuntimeError};
    use crate::sample_source::NoOpSampleSource;
    use engine::create_thread_channels;
    
    /// Test: EngineBridge dispatch_frame with injected command
    #[test]
    fn test_threaded_dispatch_with_injected_command() {
        // Create channels
        let (main_channels, _engine_channels) = create_thread_channels::<
            RuntimeCommand, RuntimeReceipt, RuntimeError
        >(64, 16, 256, 64);
        
        // Create EngineBridge with dummy runtime (won't be used in this test)
        // We'll manually create a bridge for testing
        let mut bridge = EngineBridge {
            main_channels,
            gpu_runtime: None,
            waterlines: crate::engine_bridge::MainThreadWaterlines::default(),
            engine_thread: None,
            main_thread_injected_commands: std::collections::VecDeque::new(),
        };
        
        // Inject a command
        bridge.enqueue_main_thread_command(RuntimeCommand::Init {
            ack_sender: {
                let (tx, _rx) = std::sync::mpsc::channel();
                tx
            },
        });
        
        // Dispatch frame
        let result = bridge.dispatch_frame();
        
        // Should succeed and produce feedback
        assert!(result.is_ok(), "dispatch_frame should succeed");
        
        // Verify waterline advanced
        assert_eq!(bridge.waterlines.executed_batch_waterline.0, 1,
            "waterline should increment on dispatch_frame");
    }
    
    /// Test: EngineBridge feedback queue full in debug mode
    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "feedback queue full")]
    fn test_threaded_feedback_full_debug_panic() {
        let (main_channels, _engine_channels) = create_thread_channels::<
            RuntimeCommand, RuntimeReceipt, RuntimeError
        >(64, 16, 256, 1); // feedback_capacity = 1
        
        let mut bridge = EngineBridge {
            main_channels,
            gpu_runtime: None,
            waterlines: crate::engine_bridge::MainThreadWaterlines::default(),
            engine_thread: None,
            main_thread_injected_commands: std::collections::VecDeque::new(),
        };
        
        // First dispatch should succeed
        bridge.enqueue_main_thread_command(RuntimeCommand::Init {
            ack_sender: { let (tx, _rx) = std::sync::mpsc::channel(); tx },
        });
        let _ = bridge.dispatch_frame();
        
        // Second dispatch should panic (feedback queue full, debug mode)
        bridge.enqueue_main_thread_command(RuntimeCommand::Init {
            ack_sender: { let (tx, _rx) = std::sync::mpsc::channel(); tx },
        });
        let _ = bridge.dispatch_frame();
    }
    
    /// Test: EngineBridge shutdown command produces ShutdownAck receipt
    #[test]
    fn test_threaded_shutdown_produces_ack() {
        let (main_channels, _engine_channels) = create_thread_channels::<
            RuntimeCommand, RuntimeReceipt, RuntimeError
        >(64, 16, 256, 64);
        
        let mut bridge = EngineBridge {
            main_channels,
            gpu_runtime: None,
            waterlines: crate::engine_bridge::MainThreadWaterlines::default(),
            engine_thread: None,
            main_thread_injected_commands: std::collections::VecDeque::new(),
        };
        
        // Inject shutdown command
        bridge.enqueue_main_thread_command(RuntimeCommand::Shutdown {
            reason: "test shutdown".to_string(),
        });
        
        // Dispatch should return ShutdownRequested error
        let result = bridge.dispatch_frame();
        assert!(matches!(result, Err(RuntimeError::ShutdownRequested { .. })),
            "dispatch_frame should return ShutdownRequested");
        
        // But waterline should still have advanced
        assert_eq!(bridge.waterlines.executed_batch_waterline.0, 1,
            "waterline should increment even on shutdown");
    }
}
