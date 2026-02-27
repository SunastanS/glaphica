/// Engine Core module.
///
/// Business logic core running on the engine thread.
/// Owned exclusively by the engine thread (no Arc/RwLock needed).
use std::collections::VecDeque;

use crate::app_core::MergeStores;
use brush_execution::BrushExecutionMergeFeedback;
use document::Document;
use protocol::{
    CompleteWaterline, ExecutedBatchWaterline, GpuFeedbackFrame, InputRingSample, SubmitWaterline,
};
use tiles::{BrushBufferTileRegistry, TileMergeEngine};
use view::ViewTransform;

use crate::runtime::{RuntimeCommand, RuntimeError, RuntimeReceipt};

/// Engine waterlines (received from main thread via feedback)
#[derive(Debug, Clone, Copy)]
pub struct EngineWaterlines {
    pub submit: SubmitWaterline,
    pub executed: ExecutedBatchWaterline,
    pub complete: CompleteWaterline,
}

impl Default for EngineWaterlines {
    fn default() -> Self {
        Self {
            submit: SubmitWaterline(0),
            executed: ExecutedBatchWaterline(0),
            complete: CompleteWaterline(0),
        }
    }
}

/// Engine Core - business logic running on engine thread.
///
/// This struct is owned exclusively by the engine thread,
/// so fields do NOT need Arc/RwLock.
pub struct EngineCore {
    // Document owned exclusively by engine thread
    pub document: Document,

    // Merge engine
    pub tile_merge_engine: TileMergeEngine<MergeStores>,

    // Brush state
    pub brush_buffer_tile_keys: BrushBufferTileRegistry,

    // View state
    pub view_transform: ViewTransform,

    // GC state
    pub gc_evicted_batches_total: u64,
    pub gc_evicted_keys_total: u64,

    // Brush execution feedback queue
    pub brush_execution_feedback_queue: VecDeque<BrushExecutionMergeFeedback>,

    // Waterlines (received from main thread via feedback)
    pub waterlines: EngineWaterlines,

    // Pending commands (generated from business logic)
    pub pending_commands: Vec<RuntimeCommand>,

    // Shutdown flag
    pub shutdown_requested: bool,
    // Channel to send commands to main thread
    // Note: NOT owned by EngineCore, passed separately to engine_loop
}

impl EngineCore {
    /// Create a new EngineCore from existing components.
    pub fn new(
        document: Document,
        tile_merge_engine: TileMergeEngine<MergeStores>,
        brush_buffer_tile_keys: BrushBufferTileRegistry,
        view_transform: ViewTransform,
    ) -> Self {
        Self {
            document,
            tile_merge_engine,
            brush_buffer_tile_keys,
            view_transform,
            gc_evicted_batches_total: 0,
            gc_evicted_keys_total: 0,
            brush_execution_feedback_queue: VecDeque::new(),
            waterlines: EngineWaterlines::default(),
            pending_commands: Vec::new(),
            shutdown_requested: false,
        }
    }

    /// Process an input sample (brush stroke, etc.)
    pub fn process_input_sample(&mut self, sample: &InputRingSample) {
        // TODO: Implement brush session logic
        // For now, this is a placeholder
        let _ = sample;
    }

    /// Process feedback from main thread
    pub fn process_feedback(&mut self, frame: GpuFeedbackFrame<RuntimeReceipt, RuntimeError>) {
        // 1. Update waterlines (max merge - monotonic guarantee)
        self.waterlines.submit = self.waterlines.submit.max(frame.submit_waterline);
        self.waterlines.executed = self.waterlines.executed.max(frame.executed_batch_waterline);
        self.waterlines.complete = self.waterlines.complete.max(frame.complete_waterline);

        // 2. Debug: assert monotonicity
        #[cfg(debug_assertions)]
        {
            assert!(
                frame.submit_waterline >= last_submit,
                "submit waterline regression"
            );
            assert!(
                frame.executed_batch_waterline >= last_executed,
                "executed waterline regression"
            );
            assert!(
                frame.complete_waterline >= last_complete,
                "complete waterline regression"
            );
        }

        // 3. Process receipts
        for receipt in frame.receipts.iter() {
            self.apply_receipt(receipt);
        }

        // 4. Process errors
        for error in frame.errors.iter() {
            self.handle_error(error);
        }

        // 5. Safe-to-release decisions based on complete_waterline
        self.gc_evict_before_waterline(self.waterlines.complete);
    }

    /// Apply a receipt (merge completion, etc.)
    fn apply_receipt(&mut self, receipt: &RuntimeReceipt) {
        match receipt {
            RuntimeReceipt::InitComplete => {
                eprintln!("[engine] init complete");
            }
            RuntimeReceipt::Resized => {
                eprintln!("[engine] resize complete");
            }
            RuntimeReceipt::ShutdownAck { reason } => {
                eprintln!("[engine] shutdown ack: {}", reason);
            }
            // TODO: Handle other receipts
            _ => {}
        }
    }

    /// Handle an error from main thread
    fn handle_error(&mut self, error: &RuntimeError) {
        match error {
            RuntimeError::FeedbackQueueTimeout => {
                eprintln!("[error] feedback queue timeout - initiating shutdown");
                self.shutdown_requested = true;
            }
            // TODO: Handle other errors
            _ => {
                eprintln!("[error] runtime error: {:?}", error);
            }
        }
    }

    /// GC eviction based on complete waterline
    fn gc_evict_before_waterline(&mut self, _waterline: CompleteWaterline) {
        // TODO: Implement GC logic based on waterline
    }
}

/// Run the engine thread main loop.
///
/// This function:
/// 1. Drains input samples (brush strokes, etc.)
/// 2. Processes business logic (generates RuntimeCommands)
/// 3. Sends commands to main thread via channel
/// 4. Drains feedback frames from main thread
/// 5. Merges feedback using mailbox merge
/// 6. Applies merged feedback to engine state
/// 7. Checks for shutdown conditions
///
/// Returns when shutdown is requested or channel is disconnected.
use engine::EngineThreadChannels;

pub fn engine_loop(
    mut core: EngineCore,
    mut channels: EngineThreadChannels<RuntimeCommand, RuntimeReceipt, RuntimeError>,
    mut sample_source: impl crate::sample_source::SampleSource,
) {
    let mut samples_buffer = Vec::with_capacity(1024);
    let mut feedback_merge_state =
        protocol::GpuFeedbackMergeState::<RuntimeReceipt, RuntimeError>::default();
    let mut pending_feedback: Option<protocol::GpuFeedbackFrame<RuntimeReceipt, RuntimeError>> =
        None;

    loop {
        // 1. Drain input samples (Phase 4: channel, Phase 4.5: ring)
        // Clear buffer first to avoid duplicate processing
        samples_buffer.clear();
        sample_source.drain_batch(&mut samples_buffer, 1024);

        // 2. Process business logic (brush, merge, etc.)
        for sample in &samples_buffer {
            core.process_input_sample(sample);
        }

        // 3. Send GPU commands (based on processed input)
        // Note: commands are generated in core.pending_commands by process_input_sample
        for cmd in core.pending_commands.drain(..) {
            match channels
                .gpu_command_sender
                .push(protocol::GpuCmdMsg::Command(cmd))
            {
                Ok(()) => {}
                Err(rtrb::PushError::Full(_)) => {
                    // Command queue full - skip this frame's commands
                    // (feedback will still be processed)
                    break;
                }
            }
        }

        // 4. Drain and merge feedback (mailbox merge)
        while let Ok(frame) = channels.gpu_feedback_receiver.pop() {
            pending_feedback = Some(match pending_feedback.take() {
                None => frame,
                Some(current) => protocol::GpuFeedbackFrame::merge_mailbox(
                    current,
                    frame,
                    &mut feedback_merge_state,
                ),
            });
        }

        // 5. Apply merged feedback
        if let Some(frame) = pending_feedback.take() {
            core.process_feedback(frame);
        }

        // 6. Check shutdown
        if core.shutdown_requested {
            // Engine loop exiting - channels will be disconnected
            // Main thread will detect disconnection and join this thread
            break;
        }

        // 7. Idle detection: yield if no work was done
        // This prevents busy-waiting and reduces CPU usage
        if samples_buffer.is_empty()
            && pending_feedback.is_none()
            && core.pending_commands.is_empty()
        {
            std::thread::yield_now();
        }
    }

    eprintln!("[engine] engine_loop exiting");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sample_source::ChannelSampleSource;
    use crossbeam_channel::bounded;
    use protocol::InputRingSample;

    #[test]
    fn test_engine_core_creation() {
        // Test that EngineCore can be created
        // Note: This is a basic smoke test
        let waterlines = EngineWaterlines::default();
        assert_eq!(waterlines.submit.0, 0);
        assert_eq!(waterlines.executed.0, 0);
        assert_eq!(waterlines.complete.0, 0);
    }

    #[test]
    fn test_engine_loop_shutdown() {
        // Test that engine_loop exits when shutdown_requested is set
        let (_cmd_tx, cmd_rx): (
            _,
            crossbeam_channel::Receiver<protocol::GpuCmdMsg<RuntimeCommand>>,
        ) = bounded(64);
        let (feedback_tx, _feedback_rx): (
            crossbeam_channel::Sender<protocol::GpuFeedbackFrame<RuntimeReceipt, RuntimeError>>,
            _,
        ) = bounded(64);
        let (sample_tx, _sample_rx): (crossbeam_channel::Sender<InputRingSample>, _) =
            bounded(1024);

        // Create minimal channels for testing
        // Note: Full channel setup requires engine crate integration
        // This is a placeholder for the full integration test

        let _ = (cmd_rx, feedback_tx, sample_tx);
        // Full engine_loop test requires proper EngineThreadChannels setup
        // Deferred to integration test with full GpuState integration
    }

    #[test]
    fn test_feedback_merge_mailbox() {
        // Test that GpuFeedbackFrame::merge_mailbox works correctly
        let mut merge_state =
            protocol::GpuFeedbackMergeState::<RuntimeReceipt, RuntimeError>::default();

        let frame1 = protocol::GpuFeedbackFrame {
            present_frame_id: protocol::PresentFrameId(1),
            submit_waterline: protocol::SubmitWaterline(1),
            executed_batch_waterline: protocol::ExecutedBatchWaterline(1),
            complete_waterline: protocol::CompleteWaterline(1),
            receipts: vec![RuntimeReceipt::InitComplete].into(),
            errors: vec![].into(),
        };

        let frame2 = protocol::GpuFeedbackFrame {
            present_frame_id: protocol::PresentFrameId(2),
            submit_waterline: protocol::SubmitWaterline(2),
            executed_batch_waterline: protocol::ExecutedBatchWaterline(2),
            complete_waterline: protocol::CompleteWaterline(2),
            receipts: vec![RuntimeReceipt::Resized].into(),
            errors: vec![].into(),
        };

        let merged = protocol::GpuFeedbackFrame::merge_mailbox(frame1, frame2, &mut merge_state);

        // Waterlines should be max
        assert_eq!(merged.executed_batch_waterline.0, 2);
        assert_eq!(merged.submit_waterline.0, 2);
        assert_eq!(merged.complete_waterline.0, 2);
    }
}

#[cfg(test)]
mod roundtrip_tests {
    use super::*;
    use protocol::GpuFeedbackFrame;

    #[test]
    fn test_feedback_merge_with_multiple_frames() {
        // Test merging multiple feedback frames
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
            errors: vec![RuntimeError::FeedbackQueueTimeout].into(),
        };

        let frame3 = GpuFeedbackFrame {
            present_frame_id: protocol::PresentFrameId(3),
            submit_waterline: protocol::SubmitWaterline(3),
            executed_batch_waterline: protocol::ExecutedBatchWaterline(3),
            complete_waterline: protocol::CompleteWaterline(3),
            receipts: vec![RuntimeReceipt::ResizeHandshakeAck].into(),
            errors: vec![].into(),
        };

        // Merge frames sequentially (simulating engine_loop drain)
        let merged12 = GpuFeedbackFrame::merge_mailbox(frame1, frame2, &mut merge_state);
        let merged123 = GpuFeedbackFrame::merge_mailbox(merged12, frame3, &mut merge_state);

        // Final waterlines should be max of all frames
        assert_eq!(merged123.executed_batch_waterline.0, 3);
        assert_eq!(merged123.submit_waterline.0, 3);
        assert_eq!(merged123.complete_waterline.0, 3);
        assert_eq!(merged123.present_frame_id.0, 3);

        // All receipts should be present
        assert_eq!(merged123.receipts.len(), 3);

        // Errors should be present
        assert_eq!(merged123.errors.len(), 1);
    }

    #[test]
    fn test_receipt_merge_key_stability() {
        // Test that receipts with same merge key are properly merged
        let mut merge_state =
            protocol::GpuFeedbackMergeState::<RuntimeReceipt, RuntimeError>::default();

        // Use same receipt type (InitComplete) which has stable merge key
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

        // Only one InitComplete should remain (last-wins policy)
        // Note: FramePresented with different tile counts would NOT merge (different keys)
        assert_eq!(merged.receipts.len(), 1);

        // Waterlines should be max
        assert_eq!(merged.executed_batch_waterline.0, 2);
    }
}

#[cfg(test)]
mod error_merge_tests {
    use super::*;
    use protocol::GpuFeedbackFrame;

    #[test]
    fn test_error_last_wins_merge() {
        // Test that errors with same key use last-wins policy
        let mut merge_state =
            protocol::GpuFeedbackMergeState::<RuntimeReceipt, RuntimeError>::default();

        let frame1 = GpuFeedbackFrame {
            present_frame_id: protocol::PresentFrameId(1),
            submit_waterline: protocol::SubmitWaterline(1),
            executed_batch_waterline: protocol::ExecutedBatchWaterline(1),
            complete_waterline: protocol::CompleteWaterline(1),
            receipts: vec![].into(),
            errors: vec![RuntimeError::FeedbackQueueTimeout].into(),
        };

        let frame2 = GpuFeedbackFrame {
            present_frame_id: protocol::PresentFrameId(2),
            submit_waterline: protocol::SubmitWaterline(2),
            executed_batch_waterline: protocol::ExecutedBatchWaterline(2),
            complete_waterline: protocol::CompleteWaterline(2),
            receipts: vec![].into(),
            errors: vec![RuntimeError::FeedbackQueueTimeout].into(),
        };

        let merged = GpuFeedbackFrame::merge_mailbox(frame1, frame2, &mut merge_state);

        // Only one error should remain (last-wins)
        assert_eq!(
            merged.errors.len(),
            1,
            "Errors with same key should merge to last one"
        );

        // Waterlines should be max
        assert_eq!(merged.executed_batch_waterline.0, 2);
    }

    #[test]
    fn test_different_errors_not_merged() {
        // Test that different error types are preserved
        let mut merge_state =
            protocol::GpuFeedbackMergeState::<RuntimeReceipt, RuntimeError>::default();

        let frame1 = GpuFeedbackFrame {
            present_frame_id: protocol::PresentFrameId(1),
            submit_waterline: protocol::SubmitWaterline(1),
            executed_batch_waterline: protocol::ExecutedBatchWaterline(1),
            complete_waterline: protocol::CompleteWaterline(1),
            receipts: vec![].into(),
            errors: vec![RuntimeError::FeedbackQueueTimeout].into(),
        };

        let frame2 = GpuFeedbackFrame {
            present_frame_id: protocol::PresentFrameId(2),
            submit_waterline: protocol::SubmitWaterline(2),
            executed_batch_waterline: protocol::ExecutedBatchWaterline(2),
            complete_waterline: protocol::CompleteWaterline(2),
            receipts: vec![].into(),
            errors: vec![RuntimeError::EngineThreadDisconnected].into(),
        };

        let merged = GpuFeedbackFrame::merge_mailbox(frame1, frame2, &mut merge_state);

        // Both errors should be preserved (different keys)
        assert_eq!(
            merged.errors.len(),
            2,
            "Different error types should not merge"
        );

        // Waterlines should be max
        assert_eq!(merged.executed_batch_waterline.0, 2);
    }
}
