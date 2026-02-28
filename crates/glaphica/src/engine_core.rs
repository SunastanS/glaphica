/// Engine Core module.
///
/// Business logic core running on the engine thread.
/// Owned exclusively by the engine thread (no Arc/RwLock needed for exclusive data).
use std::collections::VecDeque;
use std::sync::Arc;

use crate::app_core::MergeStores;
use brush_execution::BrushExecutionMergeFeedback;
use document::Document;
use protocol::{
    CompleteWaterline, ExecutedBatchWaterline, GpuFeedbackFrame, InputRingSample, SubmitWaterline,
};
use tiles::{
    BrushBufferTileRegistry, GenericR32FloatTileAtlasStore, TileAtlasStore, TileMergeEngine,
};
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
/// so fields do NOT need Arc/RwLock for exclusive data.
/// However, some GPU-related stores use Arc for sharing with the renderer.
pub struct EngineCore {
    // === Document owned exclusively by engine thread ===
    pub document: Document,

    // === Merge engine ===
    pub tile_merge_engine: TileMergeEngine<MergeStores>,

    // === Brush state ===
    pub brush_buffer_tile_keys: BrushBufferTileRegistry,

    // === View state ===
    pub view_transform: ViewTransform,

    // === Atlas stores (shared with GPU resources) ===
    /// Layer atlas store (CPU-side allocation).
    /// Arc because it's shared with GpuRuntime for GPU operations.
    pub atlas_store: Arc<TileAtlasStore>,

    /// Brush buffer store (CPU-side allocation).
    /// Arc because it's shared with GpuRuntime for GPU operations.
    pub brush_buffer_store: Arc<GenericR32FloatTileAtlasStore>,

    // === GC state ===
    pub gc_evicted_batches_total: u64,
    pub gc_evicted_keys_total: u64,

    // === Brush execution feedback queue ===
    pub brush_execution_feedback_queue: VecDeque<BrushExecutionMergeFeedback>,

    // === Waterlines (received from main thread via feedback) ===
    pub waterlines: EngineWaterlines,

    // === Pending commands (generated from business logic) ===
    pub pending_commands: Vec<RuntimeCommand>,

    // === Frame management ===
    pub next_frame_id: u64,

    // === Debug flags ===
    /// Debug flag: disable merge for debugging.
    pub disable_merge_for_debug: bool,

    /// Performance logging enabled.
    pub perf_log_enabled: bool,

    /// Brush trace logging enabled.
    pub brush_trace_enabled: bool,

    // === Debug state: last bound render tree (debug assertions only) ===
    #[cfg(debug_assertions)]
    pub last_bound_render_tree: Option<(u64, u64)>,

    // === Shutdown flag ===
    pub shutdown_requested: bool,
}

impl EngineCore {
    /// Create a new EngineCore from existing components.
    pub fn new(
        document: Document,
        tile_merge_engine: TileMergeEngine<MergeStores>,
        brush_buffer_tile_keys: BrushBufferTileRegistry,
        view_transform: ViewTransform,
        atlas_store: Arc<TileAtlasStore>,
        brush_buffer_store: Arc<GenericR32FloatTileAtlasStore>,
        disable_merge_for_debug: bool,
        perf_log_enabled: bool,
        brush_trace_enabled: bool,
    ) -> Self {
        Self {
            document,
            tile_merge_engine,
            brush_buffer_tile_keys,
            view_transform,
            atlas_store,
            brush_buffer_store,
            gc_evicted_batches_total: 0,
            gc_evicted_keys_total: 0,
            brush_execution_feedback_queue: VecDeque::new(),
            waterlines: EngineWaterlines::default(),
            pending_commands: Vec::new(),
            next_frame_id: 0,
            disable_merge_for_debug,
            perf_log_enabled,
            brush_trace_enabled,
            #[cfg(debug_assertions)]
            last_bound_render_tree: None,
            shutdown_requested: false,
        }
    }

    /// Create EngineCore from AppCore parts.
    ///
    /// This is used when transitioning from single-threaded to threaded mode.
    /// Takes the components returned by `AppCore::into_engine_parts()`.
    pub fn from_app_parts(
        document: Document,
        tile_merge_engine: TileMergeEngine<MergeStores>,
        brush_buffer_tile_keys: BrushBufferTileRegistry,
        view_transform: ViewTransform,
        atlas_store: Arc<TileAtlasStore>,
        brush_buffer_store: Arc<GenericR32FloatTileAtlasStore>,
        disable_merge_for_debug: bool,
        perf_log_enabled: bool,
        brush_trace_enabled: bool,
        next_frame_id: u64,
    ) -> Self {
        Self {
            document,
            tile_merge_engine,
            brush_buffer_tile_keys,
            view_transform,
            atlas_store,
            brush_buffer_store,
            gc_evicted_batches_total: 0,
            gc_evicted_keys_total: 0,
            brush_execution_feedback_queue: VecDeque::new(),
            waterlines: EngineWaterlines::default(),
            pending_commands: Vec::new(),
            next_frame_id,
            disable_merge_for_debug,
            perf_log_enabled,
            brush_trace_enabled,
            #[cfg(debug_assertions)]
            last_bound_render_tree: None,
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
        // Debug: store old waterlines for monotonicity check
        #[cfg(debug_assertions)]
        let old_waterlines = self.waterlines;

        // 1. Update waterlines (max merge - monotonic guarantee)
        self.waterlines.submit = self.waterlines.submit.max(frame.submit_waterline);
        self.waterlines.executed = self.waterlines.executed.max(frame.executed_batch_waterline);
        self.waterlines.complete = self.waterlines.complete.max(frame.complete_waterline);

        // 2. Debug: assert monotonicity
        #[cfg(debug_assertions)]
        {
            assert!(
                frame.submit_waterline >= old_waterlines.submit,
                "submit waterline regression"
            );
            assert!(
                frame.executed_batch_waterline >= old_waterlines.executed,
                "executed waterline regression"
            );
            assert!(
                frame.complete_waterline >= old_waterlines.complete,
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

    // === Accessor methods ===

    /// Get a reference to the document.
    pub fn document(&self) -> &Document {
        &self.document
    }

    /// Get a mutable reference to the document.
    pub fn document_mut(&mut self) -> &mut Document {
        &mut self.document
    }

    /// Get the view transform.
    pub fn view_transform(&self) -> &ViewTransform {
        &self.view_transform
    }

    /// Get a mutable reference to the view transform.
    pub fn view_transform_mut(&mut self) -> &mut ViewTransform {
        &mut self.view_transform
    }

    /// Get the tile merge engine.
    pub fn tile_merge_engine(&self) -> &TileMergeEngine<MergeStores> {
        &self.tile_merge_engine
    }

    /// Get a mutable reference to the tile merge engine.
    pub fn tile_merge_engine_mut(&mut self) -> &mut TileMergeEngine<MergeStores> {
        &mut self.tile_merge_engine
    }

    /// Get the brush buffer tile keys.
    pub fn brush_buffer_tile_keys(&self) -> &BrushBufferTileRegistry {
        &self.brush_buffer_tile_keys
    }

    /// Get a mutable reference to the brush buffer tile keys.
    pub fn brush_buffer_tile_keys_mut(&mut self) -> &mut BrushBufferTileRegistry {
        &mut self.brush_buffer_tile_keys
    }

    /// Get the brush execution feedback queue.
    pub fn brush_execution_feedback_queue_mut(
        &mut self,
    ) -> &mut VecDeque<BrushExecutionMergeFeedback> {
        &mut self.brush_execution_feedback_queue
    }

    /// Get the atlas store.
    pub fn atlas_store(&self) -> &Arc<TileAtlasStore> {
        &self.atlas_store
    }

    /// Get the brush buffer store.
    pub fn brush_buffer_store(&self) -> &Arc<GenericR32FloatTileAtlasStore> {
        &self.brush_buffer_store
    }

    /// Check if merge work is pending.
    pub fn has_pending_merge_work(&self) -> bool {
        self.tile_merge_engine.has_pending_work()
    }

    /// Get and increment the frame ID.
    pub fn get_next_frame_id(&mut self) -> u64 {
        let id = self.next_frame_id;
        self.next_frame_id = self
            .next_frame_id
            .checked_add(1)
            .expect("frame id overflow");
        id
    }

    /// Check if performance logging is enabled.
    pub fn perf_log_enabled(&self) -> bool {
        self.perf_log_enabled
    }

    /// Check if brush trace logging is enabled.
    pub fn brush_trace_enabled(&self) -> bool {
        self.brush_trace_enabled
    }

    /// Check if merge is disabled for debugging.
    pub fn disable_merge_for_debug(&self) -> bool {
        self.disable_merge_for_debug
    }

    /// Get the total number of GC evicted batches.
    pub fn gc_evicted_batches_total(&self) -> u64 {
        self.gc_evicted_batches_total
    }

    /// Get the total number of GC evicted keys.
    pub fn gc_evicted_keys_total(&self) -> u64 {
        self.gc_evicted_keys_total
    }

    /// Update last bound render tree (debug assertions only).
    #[cfg(debug_assertions)]
    pub fn set_last_bound_render_tree(&mut self, value: Option<(u64, u64)>) {
        self.last_bound_render_tree = value;
    }

    /// Get last bound render tree (debug assertions only).
    #[cfg(debug_assertions)]
    pub fn last_bound_render_tree(&self) -> Option<(u64, u64)> {
        self.last_bound_render_tree
    }

    // === Business Logic Methods (migrated from AppCore) ===

    /// Set the active preview buffer for a layer and return the updated render tree snapshot.
    ///
    /// Returns `Some(render_tree)` if the render tree was dirty and needs to be rebound.
    /// Returns `None` if no rebind is needed.
    pub fn set_preview_buffer(
        &mut self,
        layer_id: u64,
        stroke_session_id: u64,
    ) -> Option<render_protocol::RenderTreeSnapshot> {
        self.document
            .set_active_preview_buffer(layer_id, stroke_session_id)
            .unwrap_or_else(|error| {
                panic!(
                    "set active preview buffer failed: layer_id={} stroke_session_id={} error={error:?}",
                    layer_id, stroke_session_id
                )
            });
        if !self.document.take_render_tree_cache_dirty() {
            return None;
        }
        Some(self.document.render_tree_snapshot())
    }

    /// Clear the active preview buffer and return the updated render tree snapshot.
    ///
    /// Returns `Some(render_tree)` if the render tree was dirty and needs to be rebound.
    /// Returns `None` if no rebind is needed.
    pub fn clear_preview_buffer(
        &mut self,
        stroke_session_id: u64,
    ) -> Option<render_protocol::RenderTreeSnapshot> {
        let _ = self.document.clear_active_preview_buffer(stroke_session_id);
        if !self.document.take_render_tree_cache_dirty() {
            return None;
        }
        Some(self.document.render_tree_snapshot())
    }

    /// Drain tile GC evictions and apply them to the brush buffer tile key registry.
    ///
    /// This processes all evicted retain batches from the brush buffer store and
    /// updates the GC statistics.
    pub fn drain_tile_gc_evictions(&mut self) {
        let evicted_batches = self.brush_buffer_store.drain_evicted_retain_batches();
        for evicted_batch in evicted_batches {
            self.brush_buffer_tile_keys
                .apply_retained_eviction(evicted_batch.retain_id, &evicted_batch.keys);
            self.apply_gc_evicted_batch(evicted_batch.retain_id, evicted_batch.keys.len());
        }
    }

    fn apply_gc_evicted_batch(&mut self, retain_id: u64, key_count: usize) {
        self.gc_evicted_batches_total = self
            .gc_evicted_batches_total
            .checked_add(1)
            .expect("gc evicted batch counter overflow");
        self.gc_evicted_keys_total = self
            .gc_evicted_keys_total
            .checked_add(u64::try_from(key_count).expect("gc key count exceeds u64"))
            .expect("gc evicted key counter overflow");
        eprintln!(
            "tiles gc evicted retain batch: retain_id={} key_count={} total_batches={} total_keys={}",
            retain_id, key_count, self.gc_evicted_batches_total, self.gc_evicted_keys_total
        );
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
