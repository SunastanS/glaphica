/// Engine Core module.
///
/// Business logic core running on the engine thread.
/// Owned exclusively by the engine thread (no Arc/RwLock needed).

use std::collections::VecDeque;
use std::sync::Arc;

use brush_execution::BrushExecutionMergeFeedback;
use document::Document;
use protocol::{InputRingSample, SubmitWaterline, ExecutedBatchWaterline, CompleteWaterline, GpuFeedbackFrame};
use tiles::{
    BrushBufferTileRegistry, GenericR32FloatTileAtlasStore, TileAtlasStore, TileMergeEngine,
    TileMergeError, TilesBusinessResult,
};
use crate::app_core::MergeStores;
use view::ViewTransform;

use crate::runtime::{RuntimeCommand, RuntimeReceipt, RuntimeError};

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
    pub fn process_feedback(
        &mut self, 
        frame: GpuFeedbackFrame<RuntimeReceipt, RuntimeError>
    ) {
        // Store last waterlines for monotonicity check
        let last_submit = self.waterlines.submit;
        let last_executed = self.waterlines.executed;
        let last_complete = self.waterlines.complete;
        
        // 1. Update waterlines (max merge - monotonic guarantee)
        self.waterlines.submit = self.waterlines.submit.max(frame.submit_waterline);
        self.waterlines.executed = self.waterlines.executed.max(frame.executed_batch_waterline);
        self.waterlines.complete = self.waterlines.complete.max(frame.complete_waterline);
        
        // 2. Debug: assert monotonicity
        #[cfg(debug_assertions)]
        {
            assert!(frame.submit_waterline >= last_submit, "submit waterline regression");
            assert!(frame.executed_batch_waterline >= last_executed, "executed waterline regression");
            assert!(frame.complete_waterline >= last_complete, "complete waterline regression");
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
    let mut feedback_merge_state = protocol::GpuFeedbackMergeState::<RuntimeReceipt, RuntimeError>::default();
    let mut pending_feedback: Option<protocol::GpuFeedbackFrame<RuntimeReceipt, RuntimeError>> = None;
    
    loop {
        // 1. Drain input samples (Phase 4: channel, Phase 4.5: ring)
        sample_source.drain_batch(&mut samples_buffer, 1024);
        
        // 2. Process business logic (brush, merge, etc.)
        for sample in &samples_buffer {
            core.process_input_sample(sample);
        }
        
        // 3. Send GPU commands (based on processed input)
        // Note: commands are generated in core.pending_commands by process_input_sample
        for cmd in core.pending_commands.drain(..) {
            match channels.gpu_command_sender.push(protocol::GpuCmdMsg::Command(cmd)) {
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
                Some(current) => {
                    protocol::GpuFeedbackFrame::merge_mailbox(current, frame, &mut feedback_merge_state)
                }
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
    }
    
    eprintln!("[engine] engine_loop exiting");
}
