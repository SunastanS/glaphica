/// App Core module.
///
/// Manages application-level business logic: document, merge orchestration, brush state.
/// Does not directly hold GPU resources - communicates with GpuRuntime via commands.
use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::{Arc, RwLock};
use std::time::Instant;

use brush_execution::BrushExecutionMergeFeedback;
use document::{Document, DocumentMergeError, TileCoordinate};
use render_protocol::{BrushRenderCommand, StrokeExecutionReceiptId};
use renderer::BrushRenderEnqueueError;
use tiles::{
    BrushBufferTileRegistry, GenericR32FloatTileAtlasStore, MergePlanRequest, TileAtlasStore,
    TileMergeEngine, TileMergeError, TilesBusinessResult,
};
use view::ViewTransform;
use winit::dpi::PhysicalSize;

use crate::runtime::{GpuRuntime, RuntimeCommand, RuntimeError, RuntimeReceipt};
use render_protocol::RenderTreeSnapshot;

/// Stroke tile merge plan for building merge operations.
#[derive(Debug)]
pub struct StrokeTileMergePlan {
    pub layer_id: u64,
    pub tile_ops: Vec<tiles::MergePlanTileOp>,
}

/// Merge bridge errors.
#[derive(Debug)]
pub enum MergeBridgeError {
    RendererPoll(renderer::MergePollError),
    RendererAck(renderer::MergeAckError),
    RendererSubmit(renderer::MergeSubmitError),
    RendererFinalize(renderer::MergeFinalizeError),
    Tiles(TileMergeError),
    TileImageApply(tiles::TileImageApplyError),
    Document(DocumentMergeError),
    MissingRendererNotice {
        receipt_id: StrokeExecutionReceiptId,
        notice_id: tiles::TileMergeCompletionNoticeId,
    },
}

impl From<renderer::MergePollError> for MergeBridgeError {
    fn from(err: renderer::MergePollError) -> Self {
        MergeBridgeError::RendererPoll(err)
    }
}

impl From<renderer::MergeAckError> for MergeBridgeError {
    fn from(err: renderer::MergeAckError) -> Self {
        MergeBridgeError::RendererAck(err)
    }
}

impl From<renderer::MergeSubmitError> for MergeBridgeError {
    fn from(err: renderer::MergeSubmitError) -> Self {
        MergeBridgeError::RendererSubmit(err)
    }
}

impl From<renderer::MergeFinalizeError> for MergeBridgeError {
    fn from(err: renderer::MergeFinalizeError) -> Self {
        MergeBridgeError::RendererFinalize(err)
    }
}

impl From<TileMergeError> for MergeBridgeError {
    fn from(err: TileMergeError) -> Self {
        MergeBridgeError::Tiles(err)
    }
}

impl From<tiles::TileImageApplyError> for MergeBridgeError {
    fn from(err: tiles::TileImageApplyError) -> Self {
        MergeBridgeError::TileImageApply(err)
    }
}

impl From<DocumentMergeError> for MergeBridgeError {
    fn from(err: DocumentMergeError) -> Self {
        MergeBridgeError::Document(err)
    }
}

/// Merge stores for tile merge engine.
pub struct MergeStores {
    pub layer_store: Arc<TileAtlasStore>,
    pub stroke_store: Arc<GenericR32FloatTileAtlasStore>,
}

impl tiles::MergeTileStore for MergeStores {
    fn allocate(&self) -> Result<tiles::TileKey, tiles::TileAllocError> {
        self.layer_store.allocate()
    }

    fn release(&self, key: tiles::TileKey) -> bool {
        self.layer_store.release(key)
    }

    fn resolve(&self, key: tiles::TileKey) -> Option<tiles::TileAddress> {
        self.layer_store.resolve(key)
    }

    fn resolve_stroke(&self, key: tiles::TileKey) -> Option<tiles::TileAddress> {
        self.stroke_store.resolve(key)
    }

    fn mark_keys_active(&self, keys: &[tiles::TileKey]) {
        self.layer_store.mark_keys_active(keys)
    }

    fn retain_keys_new_batch(&self, keys: &[tiles::TileKey]) -> u64 {
        self.layer_store.retain_keys_new_batch(keys)
    }
}

/// App Core - manages business logic and orchestrates GPU runtime.
///
/// Holds document, merge engine, and brush state.
/// Communicates with GpuRuntime via command interface.
pub struct AppCore {
    /// Document data.
    document: Arc<RwLock<Document>>,

    /// Tile merge engine for merge business logic.
    tile_merge_engine: TileMergeEngine<MergeStores>,

    /// Brush buffer tile key registry.
    brush_buffer_tile_keys: Arc<RwLock<BrushBufferTileRegistry>>,

    /// Layer atlas store (CPU-side allocation).
    atlas_store: Arc<TileAtlasStore>,

    /// Brush buffer store (CPU-side allocation).
    brush_buffer_store: Arc<GenericR32FloatTileAtlasStore>,

    /// View transform state.
    view_transform: ViewTransform,

    /// Brush execution feedback queue.
    brush_execution_feedback_queue: VecDeque<BrushExecutionMergeFeedback>,

    /// Frame ID counter.
    next_frame_id: u64,

    /// GC statistics.
    gc_evicted_batches_total: u64,
    gc_evicted_keys_total: u64,

    /// Debug flag: disable merge for debugging.
    disable_merge_for_debug: bool,

    /// Performance logging enabled.
    perf_log_enabled: bool,

    /// Brush trace logging enabled.
    brush_trace_enabled: bool,

    /// Debug state: last bound render tree (debug assertions only).
    #[cfg(debug_assertions)]
    last_bound_render_tree: Option<(u64, u64)>,

    /// Channel sender for GPU commands (threaded mode only).
    /// When present, AppCore sends RuntimeCommand via this channel instead of calling runtime.execute() directly.
    #[cfg(feature = "true_threading")]
    gpu_command_sender: Option<rtrb::Producer<protocol::GpuCmdMsg<RuntimeCommand>>>,

    /// Channel receiver for GPU feedback (threaded mode only).
    /// When present, AppCore receives RuntimeReceipt/RuntimeError via this channel.
    #[cfg(feature = "true_threading")]
    gpu_feedback_receiver:
        Option<rtrb::Consumer<protocol::GpuFeedbackFrame<RuntimeReceipt, RuntimeError>>>,
}

impl AppCore {
    /// Create a new AppCore with the given components (single-threaded mode).
    pub fn new(
        document: Arc<RwLock<Document>>,
        tile_merge_engine: TileMergeEngine<MergeStores>,
        brush_buffer_tile_keys: Arc<RwLock<BrushBufferTileRegistry>>,
        atlas_store: Arc<TileAtlasStore>,
        brush_buffer_store: Arc<GenericR32FloatTileAtlasStore>,
        view_transform: ViewTransform,
        disable_merge_for_debug: bool,
        perf_log_enabled: bool,
        brush_trace_enabled: bool,
        next_frame_id: u64,
    ) -> Self {
        Self {
            document,
            tile_merge_engine,
            brush_buffer_tile_keys,
            atlas_store,
            brush_buffer_store,
            view_transform,
            brush_execution_feedback_queue: VecDeque::new(),
            next_frame_id,
            gc_evicted_batches_total: 0,
            gc_evicted_keys_total: 0,
            disable_merge_for_debug,
            perf_log_enabled,
            brush_trace_enabled,
            #[cfg(debug_assertions)]
            last_bound_render_tree: None,
            #[cfg(feature = "true_threading")]
            gpu_command_sender: None,
            #[cfg(feature = "true_threading")]
            gpu_feedback_receiver: None,
        }
    }

/// Create a placeholder AppCore for threaded mode.
    ///
    /// In threaded mode, the actual business logic lives in EngineCore on the engine thread.
    /// This placeholder exists only to satisfy the GpuState type - it should never be used
    /// for actual operations in threaded mode.
    ///
    /// # Safety
    /// This method creates a placeholder with dummy Arc references. It should ONLY be used
    /// when transitioning to threaded mode, where the placeholder will never be accessed.
    #[cfg(feature = "true_threading")]
    pub fn placeholder_for_threaded_mode() -> Self {
        // In threaded mode, AppCore is NOT used - all business logic is in EngineCore.
        // However, we need to satisfy the type system.
        //
        // The key insight is that in Threaded mode, all GpuState methods that access
        // self.core should panic or return early before touching core.
        //
        // We use Option<AppCore> internally and set it to None in threaded mode.
        // But for now, we create a minimal placeholder that will work if accidentally
        // accessed (though it shouldn't be).
        //
        // Note: This is a transitional solution. The proper fix is to change
        // GpuState to use Option<AppCore> or a different type for threaded mode.
        
        panic!(
            "AppCore::placeholder_for_threaded_mode() should never be called. \
             In threaded mode, GpuState should not hold an AppCore. \
             This indicates a bug in the architecture transition."
        )
    }

    /// Create a placeholder AppCore for threaded mode (not supported without true_threading feature).
    #[cfg(not(feature = "true_threading"))]
    pub fn placeholder_for_threaded_mode() -> Self {
        panic!("placeholder_for_threaded_mode called without true_threading feature - threaded mode not supported")
    }
    }

    /// Create a placeholder AppCore for threaded mode (not supported without true_threading feature).
    #[cfg(not(feature = "true_threading"))]
    pub fn placeholder_for_threaded_mode() -> Self {
        panic!("placeholder_for_threaded_mode called without true_threading feature - threaded mode not supported")
    }

    /// Create AppCore with channel communication (threaded mode).
    #[cfg(feature = "true_threading")]
    pub fn new_with_channels(
        document: Arc<RwLock<Document>>,
        tile_merge_engine: TileMergeEngine<MergeStores>,
        brush_buffer_tile_keys: Arc<RwLock<BrushBufferTileRegistry>>,
        atlas_store: Arc<TileAtlasStore>,
        brush_buffer_store: Arc<GenericR32FloatTileAtlasStore>,
        view_transform: ViewTransform,
        disable_merge_for_debug: bool,
        perf_log_enabled: bool,
        brush_trace_enabled: bool,
        next_frame_id: u64,
        gpu_command_sender: rtrb::Producer<protocol::GpuCmdMsg<RuntimeCommand>>,
        gpu_feedback_receiver: rtrb::Consumer<
            protocol::GpuFeedbackFrame<RuntimeReceipt, RuntimeError>,
        >,
    ) -> Self {
        Self {
            document,
            tile_merge_engine,
            brush_buffer_tile_keys,
            atlas_store,
            brush_buffer_store,
            view_transform,
            brush_execution_feedback_queue: VecDeque::new(),
            next_frame_id,
            gc_evicted_batches_total: 0,
            gc_evicted_keys_total: 0,
            disable_merge_for_debug,
            perf_log_enabled,
            brush_trace_enabled,
            #[cfg(debug_assertions)]
            last_bound_render_tree: None,
            gpu_command_sender: Some(gpu_command_sender),
            gpu_feedback_receiver: Some(gpu_feedback_receiver),
        }
    }

    /// Check if channels are available (threaded mode).
    #[cfg(feature = "true_threading")]
    fn has_channels(&self) -> bool {
        self.gpu_command_sender.is_some() && self.gpu_feedback_receiver.is_some()
    }

    /// Get a reference to the document.
    pub fn document(&self) -> &Arc<RwLock<Document>> {
        &self.document
    }

    /// Get a mutable reference to the document.
    pub fn document_mut(&mut self) -> &mut Arc<RwLock<Document>> {
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
    pub fn brush_buffer_tile_keys(&self) -> &Arc<RwLock<BrushBufferTileRegistry>> {
        &self.brush_buffer_tile_keys
    }

    /// Get a mutable reference to the brush buffer tile keys.
    pub fn brush_buffer_tile_keys_mut(&mut self) -> &mut Arc<RwLock<BrushBufferTileRegistry>> {
        &mut self.brush_buffer_tile_keys
    }

    /// Get the brush execution feedback queue.
    pub fn brush_execution_feedback_queue_mut(
        &mut self,
    ) -> &mut VecDeque<BrushExecutionMergeFeedback> {
        &mut self.brush_execution_feedback_queue
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

    /// Get the atlas store from runtime.
    pub fn atlas_store(&self) -> &Arc<TileAtlasStore> {
        &self.atlas_store
    }

    /// Get the brush buffer store from runtime.
    pub fn brush_buffer_store(&self) -> &Arc<GenericR32FloatTileAtlasStore> {
        &self.brush_buffer_store
    }

    /// Check if merge work is pending.
    pub fn has_pending_merge_work(&self) -> bool {
        self.tile_merge_engine.has_pending_work()
    }

    /// Extract inner document from Arc<RwLock>.
    /// Panics if there are other references to the Arc.
    pub fn document_into_inner(self) -> Document {
        match Arc::try_unwrap(self.document) {
            Ok(rwlock) => rwlock.into_inner().expect("document RwLock poisoned"),
            Err(_) => panic!("document Arc has other references"),
        }
    }

    /// Extract inner brush buffer tile registry from Arc<RwLock>.
    /// Panics if there are other references to the Arc.
    pub fn brush_buffer_tile_keys_into_inner(self) -> BrushBufferTileRegistry {
        match Arc::try_unwrap(self.brush_buffer_tile_keys) {
            Ok(rwlock) => rwlock
                .into_inner()
                .expect("brush_buffer_tile_keys RwLock poisoned"),
            Err(_) => panic!("brush_buffer_tile_keys Arc has other references"),
        }
    }

    /// Consume AppCore and return components needed for EngineCore.
    ///
    /// This is used when transitioning from single-threaded to threaded mode.
    /// Panics if there are other Arc references to document or brush_buffer_tile_keys.
    pub fn into_engine_parts(
        self,
    ) -> (
        Document,
        TileMergeEngine<MergeStores>,
        BrushBufferTileRegistry,
        ViewTransform,
        Arc<TileAtlasStore>,
        Arc<GenericR32FloatTileAtlasStore>,
        bool, // disable_merge_for_debug
        bool, // perf_log_enabled
        bool, // brush_trace_enabled
        u64,  // next_frame_id
    ) {
        // Destructure self to avoid multiple moves
        let AppCore {
            document,
            tile_merge_engine,
            brush_buffer_tile_keys,
            view_transform,
            atlas_store,
            brush_buffer_store,
            disable_merge_for_debug,
            perf_log_enabled,
            brush_trace_enabled,
            next_frame_id,
            last_bound_render_tree: _,
            gc_evicted_batches_total: _,
            gc_evicted_keys_total: _,
            brush_execution_feedback_queue: _,
            #[cfg(feature = "true_threading")]
                gpu_command_sender: _,
            #[cfg(feature = "true_threading")]
                gpu_feedback_receiver: _,
        } = self;

        // Extract document and brush_buffer_tile_keys from Arc<RwLock>
        let document = match Arc::try_unwrap(document) {
            Ok(rwlock) => rwlock.into_inner().expect("document RwLock poisoned"),
            Err(_) => panic!("document Arc has other references"),
        };
        let brush_buffer_tile_keys = match Arc::try_unwrap(brush_buffer_tile_keys) {
            Ok(rwlock) => rwlock
                .into_inner()
                .expect("brush_buffer_tile_keys RwLock poisoned"),
            Err(_) => panic!("brush_buffer_tile_keys Arc has other references"),
        };

        (
            document,
            tile_merge_engine,
            brush_buffer_tile_keys,
            view_transform,
            atlas_store,
            brush_buffer_store,
            disable_merge_for_debug,
            perf_log_enabled,
            brush_trace_enabled,
            next_frame_id,
        )
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

    /// Get the total number of GC evicted batches.
    pub fn gc_evicted_batches_total(&self) -> u64 {
        self.gc_evicted_batches_total
    }

    /// Get a mutable reference to the total number of GC evicted batches.
    pub fn gc_evicted_batches_total_mut(&mut self) -> &mut u64 {
        &mut self.gc_evicted_batches_total
    }

    /// Get the total number of GC evicted keys.
    pub fn gc_evicted_keys_total(&self) -> u64 {
        self.gc_evicted_keys_total
    }

    /// Get a mutable reference to the total number of GC evicted keys.
    pub fn gc_evicted_keys_total_mut(&mut self) -> &mut u64 {
        &mut self.gc_evicted_keys_total
    }

    /// Check if merge is disabled for debugging.
    pub fn disable_merge_for_debug(&self) -> bool {
        self.disable_merge_for_debug
    }
    /// Resize the surface.
    ///
    /// Phase 2.5-B: Now returns Result for error propagation.
    pub fn resize(
        &mut self,
        runtime: &mut GpuRuntime,
        new_size: PhysicalSize<u32>,
    ) -> Result<(), AppCoreError> {
        let width = new_size.width.max(1);
        let height = new_size.height.max(1);

        // Skip if unchanged
        let current_size = runtime.surface_size();
        if current_size.width == width && current_size.height == height {
            return Ok(());
        }

        // Execute resize command via runtime
        runtime
            .execute(RuntimeCommand::Resize {
                width,
                height,
                view_transform: self.view_transform.clone(),
            })
            .map_err(|err| AppCoreError::Resize {
                width,
                height,
                reason: format!("{:?}", err),
            })?;

        Ok(())
    }

    /// Render a frame.
    ///
    /// This is the main render path, migrated to use the runtime command interface.
    ///
    /// Phase 2.5-B: Now returns Result<(), AppCoreError> for unified error handling.
    pub fn render(&mut self, runtime: &mut GpuRuntime) -> Result<(), AppCoreError> {
        // Drain view operations before presenting
        runtime.drain_view_ops();

        // Get next frame ID
        let frame_id = self.get_next_frame_id();

        // Execute present command via runtime
        match runtime.execute(RuntimeCommand::PresentFrame { frame_id }) {
            Ok(RuntimeReceipt::FramePresented { .. }) => Ok(()),
            Ok(_) => {
                // Logic bug: unexpected receipt
                debug_assert!(false, "unexpected receipt for PresentFrame command");
                Err(AppCoreError::UnexpectedReceipt {
                    command: "PresentFrame",
                    receipt_type: "non-FramePresented",
                    receipt_debug: None,
                })
            }
            Err(RuntimeError::PresentError(e)) => match e {
                renderer::PresentError::Surface(err) => Err(AppCoreError::Surface(err)),
                renderer::PresentError::TileDrain(error) => {
                    // Fatal: GPU resource failure
                    Err(AppCoreError::PresentFatal { source: error })
                }
            },
            Err(RuntimeError::SurfaceError(err)) => Err(AppCoreError::Surface(err)),
            Err(other) => {
                // Logic bug: unexpected error variant
                debug_assert!(false, "unexpected runtime error during render");
                Err(AppCoreError::UnexpectedErrorVariant {
                    context: "render",
                    error: other,
                })
            }
        }
    }

    /// Enqueue a brush render command.
    ///
    /// This is a partial migration: GPU enqueue goes through runtime,
    /// but business logic (tile allocation, merge orchestration) stays in AppCore.
    pub fn enqueue_brush_render_command(
        &mut self,
        runtime: &mut GpuRuntime,
        command: BrushRenderCommand,
    ) -> Result<(), BrushRenderEnqueueError> {
        match &command {
            BrushRenderCommand::BeginStroke(_begin) => {
                // GPU enqueue through runtime
                runtime
                    .execute(RuntimeCommand::EnqueueBrushCommand {
                        command: command.clone(),
                    })
                    .map_err(|e| {
                        e.into_brush_enqueue().unwrap_or_else(|other| {
                            panic!("unexpected runtime error in brush enqueue: {:?}", other)
                        })
                    })?;

                // Business logic: preview buffer management
                // This is now handled by GpuState calling core.set_preview_buffer()

                Ok(())
            }

            BrushRenderCommand::AllocateBufferTiles(allocate) => {
                // Tile allocation (AppCore business logic)
                self.brush_buffer_tile_keys
                    .write()
                    .unwrap_or_else(|_| {
                        panic!("brush buffer tile key registry write lock poisoned")
                    })
                    .allocate_tiles(
                        allocate.stroke_session_id,
                        allocate.tiles.clone(),
                        runtime.brush_buffer_store(),
                    )
                    .unwrap_or_else(|error| {
                        panic!(
                            "failed to allocate brush buffer tiles for stroke {}: {error}",
                            allocate.stroke_session_id
                        )
                    });

                // Get tile bindings
                let tile_bindings = self
                    .brush_buffer_tile_keys
                    .read()
                    .unwrap_or_else(|_| panic!("brush buffer tile key registry read lock poisoned"))
                    .tile_bindings_for_stroke(allocate.stroke_session_id);

                // Bind tiles through runtime
                runtime.bind_brush_buffer_tiles(allocate.stroke_session_id, tile_bindings);

                // GC eviction handling
                // This is now handled by GpuState calling core.drain_tile_gc_evictions()

                // GPU enqueue through runtime
                runtime
                    .execute(RuntimeCommand::EnqueueBrushCommand {
                        command: command.clone(),
                    })
                    .map_err(|e| {
                        e.into_brush_enqueue().unwrap_or_else(|other| {
                            panic!("unexpected runtime error in brush enqueue: {:?}", other)
                        })
                    })?;

                Ok(())
            }

            BrushRenderCommand::MergeBuffer(merge) => {
                // Merge orchestration (AppCore business logic)
                if self.disable_merge_for_debug {
                    // Debug mode: skip merge submission
                    self.brush_buffer_tile_keys
                        .write()
                        .unwrap_or_else(|_| {
                            panic!("brush buffer tile key registry write lock poisoned")
                        })
                        .release_stroke_on_merge_failed(
                            merge.stroke_session_id,
                            runtime.brush_buffer_store(),
                        );
                    // This is now handled by GpuState calling core.clear_preview_buffer()
                    self.brush_execution_feedback_queue.push_back(
                        BrushExecutionMergeFeedback::MergeApplied {
                            stroke_session_id: merge.stroke_session_id,
                        },
                    );
                } else {
                    // Submit stroke merge to tile merge engine and renderer
                    self.enqueue_stroke_merge_submission(
                        runtime,
                        merge.stroke_session_id,
                        merge.tx_token,
                        merge.target_layer_id,
                    )
                    .map_err(|e| BrushRenderEnqueueError::MergeError {
                        message: format!("{:?}", e),
                    })?;
                }

                // GPU enqueue through runtime
                runtime
                    .execute(RuntimeCommand::EnqueueBrushCommand {
                        command: command.clone(),
                    })
                    .map_err(|e| {
                        e.into_brush_enqueue().unwrap_or_else(|other| {
                            panic!("unexpected runtime error in brush enqueue: {:?}", other)
                        })
                    })?;

                Ok(())
            }

            // Other commands: direct passthrough to runtime
            _ => {
                runtime
                    .execute(RuntimeCommand::EnqueueBrushCommand {
                        command: command.clone(),
                    })
                    .map_err(|e| {
                        e.into_brush_enqueue().unwrap_or_else(|other| {
                            panic!("unexpected runtime error in brush enqueue: {:?}", other)
                        })
                    })?;
                Ok(())
            }
        }
    }

    /// Process renderer merge completions.
    ///
    /// This is the main merge processing path, migrated to use the runtime command interface.
    /// Returns the updated render tree snapshot if the render tree was modified.
    pub fn process_renderer_merge_completions(
        &mut self,
        runtime: &mut GpuRuntime,
        frame_id: u64,
    ) -> Result<Option<RenderTreeSnapshot>, MergeBridgeError> {
        let perf_started = Instant::now();

        // Step 1: GPU side - submit and poll via runtime command
        let receipt = runtime
            .execute(RuntimeCommand::ProcessMergeCompletions { frame_id })
            .map_err(|err| {
                MergeBridgeError::RendererSubmit(err.into_merge_submit().unwrap_or_else(|other| {
                    panic!("unexpected runtime error in merge submit: {:?}", other)
                }))
            })?;

        let RuntimeReceipt::MergeCompletionsProcessed {
            submission_receipt_ids: _,
            renderer_notices,
        } = receipt
        else {
            panic!("unexpected receipt for ProcessMergeCompletions command");
        };

        if self.perf_log_enabled {
            eprintln!(
                "[merge_bridge_perf] frame_id={} submitted_receipts={} renderer_notices={}",
                frame_id,
                renderer_notices.len(),
                renderer_notices.len(),
            );
        }

        // Step 2: Business side - process notices through tile_merge_engine
        let mut renderer_notice_by_key = HashMap::new();
        for renderer_notice in renderer_notices {
            let notice_key = (renderer_notice.notice_id, renderer_notice.receipt_id);

            self.tile_merge_engine
                .on_renderer_completion_signal(
                    renderer_notice.receipt_id,
                    renderer_notice.audit_meta,
                    renderer_notice.result.clone(),
                )
                .map_err(MergeBridgeError::Tiles)?;

            let previous = renderer_notice_by_key.insert(notice_key, renderer_notice);
            assert!(
                previous.is_none(),
                "renderer poll yielded duplicate merge notice key"
            );
        }

        let completion_notices = self.tile_merge_engine.poll_submission_results();
        if self.perf_log_enabled && !completion_notices.is_empty() {
            eprintln!(
                "[merge_bridge_perf] frame_id={} tile_engine_completion_notices={}",
                frame_id,
                completion_notices.len(),
            );
        }

        // Collect renderer notices to acknowledge
        let mut notices_to_ack = Vec::with_capacity(completion_notices.len());
        for notice in &completion_notices {
            let notice_key = (notice.notice_id, notice.receipt_id);
            let renderer_notice = renderer_notice_by_key.remove(&notice_key).ok_or(
                MergeBridgeError::MissingRendererNotice {
                    receipt_id: notice.receipt_id,
                    notice_id: notice.notice_id,
                },
            )?;

            // Convert RendererNotice back to MergeCompletionNotice for ack
            notices_to_ack.push(renderer::MergeCompletionNotice {
                receipt_id: renderer_notice.receipt_id,
                audit_meta: renderer_notice.audit_meta,
                result: renderer_notice.result,
            });

            self.tile_merge_engine
                .ack_merge_result(notice.receipt_id, notice.notice_id)
                .map_err(MergeBridgeError::Tiles)?;
        }

        // Acknowledge merge results through runtime command
        if !notices_to_ack.is_empty() {
            runtime
                .execute(RuntimeCommand::AckMergeResults {
                    notices: notices_to_ack,
                })
                .map_err(|e| match e {
                    RuntimeError::MergeAck(ack_err) => MergeBridgeError::RendererAck(ack_err),
                    _ => MergeBridgeError::RendererAck(renderer::MergeAckError::UnknownReceipt {
                        receipt_id: StrokeExecutionReceiptId(0),
                    }),
                })?;
        }

        let business_results = self.tile_merge_engine.drain_business_results();
        if self.perf_log_enabled && !business_results.is_empty() {
            let finalize_count = business_results
                .iter()
                .filter(|result| matches!(result, TilesBusinessResult::CanFinalize { .. }))
                .count();
            let abort_count = business_results.len().saturating_sub(finalize_count);
            let total_dirty_tiles: usize = business_results
                .iter()
                .map(|result| match result {
                    TilesBusinessResult::CanFinalize { dirty_tiles, .. } => dirty_tiles.len(),
                    TilesBusinessResult::RequiresAbort { .. } => 0,
                })
                .sum();
            eprintln!(
                "[merge_bridge_perf] frame_id={} business_results={} finalize={} abort={} dirty_tiles={}",
                frame_id,
                business_results.len(),
                finalize_count,
                abort_count,
                total_dirty_tiles,
            );
        }

        // Apply tiles business results
        let render_tree = self.apply_tiles_business_results(&business_results, frame_id)?;

        // Drain tile GC evictions
        self.drain_tile_gc_evictions();

        if self.perf_log_enabled {
            eprintln!(
                "[merge_bridge_perf] frame_id={} process_merge_completions_cpu_ms={:.3}",
                frame_id,
                perf_started.elapsed().as_secs_f64() * 1_000.0,
            );
        }

        Ok(render_tree)
    }

    /// Set the active preview buffer for a layer and return the updated render tree snapshot.
    ///
    /// Returns `Some(RenderTreeSnapshot)` if the render tree was dirty and needs to be rebound.
    /// Returns `None` if no rebind is needed.
    pub fn set_preview_buffer(
        &mut self,
        layer_id: u64,
        stroke_session_id: u64,
    ) -> Option<RenderTreeSnapshot> {
        let mut document = self
            .document
            .write()
            .unwrap_or_else(|_| panic!("document write lock poisoned"));
        document
            .set_active_preview_buffer(layer_id, stroke_session_id)
            .unwrap_or_else(|error| {
                panic!(
                    "set active preview buffer failed: layer_id={} stroke_session_id={} error={error:?}",
                    layer_id, stroke_session_id
                )
            });
        if !document.take_render_tree_cache_dirty() {
            return None;
        }
        Some(document.render_tree_snapshot())
    }

    /// Clear the active preview buffer and return the updated render tree snapshot.
    ///
    /// Returns `Some(RenderTreeSnapshot)` if the render tree was dirty and needs to be rebound.
    /// Returns `None` if no rebind is needed.
    pub fn clear_preview_buffer(&mut self, stroke_session_id: u64) -> Option<RenderTreeSnapshot> {
        let mut document = self
            .document
            .write()
            .unwrap_or_else(|_| panic!("document write lock poisoned"));
        let _ = document.clear_active_preview_buffer(stroke_session_id);
        if !document.take_render_tree_cache_dirty() {
            return None;
        }
        Some(document.render_tree_snapshot())
    }

    /// Drain tile GC evictions and apply them to the brush buffer tile key registry.
    ///
    /// This processes all evicted retain batches from the brush buffer store and
    /// updates the GC statistics.
    pub fn drain_tile_gc_evictions(&mut self) {
        let evicted_batches = self.brush_buffer_store.drain_evicted_retain_batches();
        for evicted_batch in evicted_batches {
            self.brush_buffer_tile_keys
                .write()
                .unwrap_or_else(|_| panic!("brush buffer tile key registry write lock poisoned"))
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

    /// Apply tiles business results to the document.
    ///
    /// This processes business results from the tile merge engine, applying
    /// finalized merges to the document or handling aborts.
    ///
    /// Returns `Ok(Some(RenderTreeSnapshot))` if the render tree was updated.
    /// Returns `Ok(None)` if no render tree update was needed.
    /// Returns `Err(MergeBridgeError)` if processing failed.
    pub fn apply_tiles_business_results(
        &mut self,
        business_results: &[TilesBusinessResult],
        _frame_id: u64,
    ) -> Result<Option<RenderTreeSnapshot>, MergeBridgeError> {
        use document::TileCoordinate;
        use std::collections::HashSet;
        use tiles::TileKeyMapping;

        let mut final_render_tree: Option<RenderTreeSnapshot> = None;

        for result in business_results {
            match result {
                TilesBusinessResult::CanFinalize {
                    receipt_id,
                    stroke_session_id,
                    layer_id,
                    new_key_mappings,
                    dirty_tiles,
                    ..
                } => {
                    let apply_started = Instant::now();

                    #[cfg(debug_assertions)]
                    {
                        let mut mapping_coords = HashSet::with_capacity(new_key_mappings.len());
                        for mapping in new_key_mappings {
                            if !mapping_coords.insert((mapping.tile_x, mapping.tile_y)) {
                                panic!(
                                    "duplicate tile coordinate in new_key_mappings: receipt_id={} stroke_session_id={} layer_id={} tile=({}, {})",
                                    receipt_id.0, stroke_session_id, layer_id, mapping.tile_x, mapping.tile_y
                                );
                            }
                        }
                        let mut dirty_coords = HashSet::with_capacity(dirty_tiles.len());
                        for (tile_x, tile_y) in dirty_tiles {
                            if !dirty_coords.insert((*tile_x, *tile_y)) {
                                panic!(
                                    "duplicate tile coordinate in dirty_tiles: receipt_id={} stroke_session_id={} layer_id={} tile=({}, {})",
                                    receipt_id.0, stroke_session_id, layer_id, tile_x, tile_y
                                );
                            }
                        }
                        if mapping_coords != dirty_coords {
                            panic!(
                                "dirty tile set does not match mapping tile set: receipt_id={} stroke_session_id={} layer_id={} mapping_count={} dirty_count={}",
                                receipt_id.0, stroke_session_id, layer_id, mapping_coords.len(), dirty_coords.len()
                            );
                        }
                    }

                    if self.perf_log_enabled {
                        eprintln!(
                            "[brush_trace] merge_finalize_prepare receipt_id={} stroke_session_id={} layer_id={} mappings={} dirty_tiles={}",
                            receipt_id.0, stroke_session_id, layer_id, new_key_mappings.len(), dirty_tiles.len(),
                        );
                    }

                    let document_apply_result: Result<
                        Option<RenderTreeSnapshot>,
                        MergeBridgeError,
                    > = (|| {
                        let mut document = self
                            .document
                            .write()
                            .unwrap_or_else(|_| panic!("document write lock poisoned"));
                        let expected_revision = document.revision();
                        document
                            .begin_merge(*layer_id, *stroke_session_id, expected_revision)
                            .map_err(MergeBridgeError::Document)?;
                        let image_handle = document
                            .leaf_image_handle(*layer_id, *stroke_session_id)
                            .map_err(MergeBridgeError::Document)?;
                        let existing_image =
                            document
                                .image(image_handle)
                                .ok_or(MergeBridgeError::Document(
                                    DocumentMergeError::LayerNotFoundInStrokeSession {
                                        layer_id: *layer_id,
                                        stroke_session_id: *stroke_session_id,
                                    },
                                ))?;
                        let mut updated_image = (*existing_image).clone();

                        // Apply tile key mappings from brush execution
                        for mapping in new_key_mappings {
                            updated_image
                                .set_tile_at(mapping.tile_x, mapping.tile_y, mapping.new_key)
                                .map_err(|error| {
                                    MergeBridgeError::TileImageApply(
                                        tiles::TileImageApplyError::TileOutOfBounds {
                                            tile_x: mapping.tile_x,
                                            tile_y: mapping.tile_y,
                                        },
                                    )
                                })?;
                        }

                        let layer_dirty_tiles: Vec<TileCoordinate> = dirty_tiles
                            .iter()
                            .map(|(tile_x, tile_y)| TileCoordinate {
                                tile_x: *tile_x,
                                tile_y: *tile_y,
                            })
                            .collect();

                        document
                            .apply_merge_image(
                                *layer_id,
                                *stroke_session_id,
                                updated_image,
                                &layer_dirty_tiles,
                                false,
                            )
                            .map_err(MergeBridgeError::Document)?;

                        Ok(if document.take_render_tree_cache_dirty() {
                            Some(document.render_tree_snapshot())
                        } else {
                            None
                        })
                    })();

                    let render_tree = match document_apply_result {
                        Ok(render_tree) => render_tree,
                        Err(error) => {
                            self.brush_execution_feedback_queue.push_back(
                                BrushExecutionMergeFeedback::MergeFailed {
                                    stroke_session_id: *stroke_session_id,
                                    message: format!("document merge apply failed: {error:?}"),
                                },
                            );
                            return Err(error);
                        }
                    };

                    if render_tree.is_some() {
                        final_render_tree = render_tree;
                    }

                    if self.perf_log_enabled {
                        eprintln!(
                            "[merge_bridge_perf] merge_finalize receipt_id={} stroke_session_id={} layer_id={} dirty_tiles={} cpu_apply_ms={:.3}",
                            receipt_id.0, stroke_session_id, layer_id, dirty_tiles.len(),
                            apply_started.elapsed().as_secs_f64() * 1000.0,
                        );
                    }

                    // Finalize the receipt in the merge engine
                    self.tile_merge_engine
                        .finalize_receipt(*receipt_id)
                        .map_err(MergeBridgeError::Tiles)?;

                    // Retain stroke tiles in brush buffer store
                    self.brush_buffer_tile_keys
                        .write()
                        .unwrap_or_else(|_| {
                            panic!("brush buffer tile key registry write lock poisoned")
                        })
                        .retain_stroke_tiles(*stroke_session_id, self.brush_buffer_store.as_ref());

                    self.brush_execution_feedback_queue.push_back(
                        BrushExecutionMergeFeedback::MergeApplied {
                            stroke_session_id: *stroke_session_id,
                        },
                    );
                }

                TilesBusinessResult::RequiresAbort {
                    receipt_id,
                    stroke_session_id,
                    layer_id,
                    message,
                    ..
                } => {
                    let document_abort_result: Result<
                        Option<RenderTreeSnapshot>,
                        MergeBridgeError,
                    > = (|| {
                        let mut document = self
                            .document
                            .write()
                            .unwrap_or_else(|_| panic!("document write lock poisoned"));
                        if document.has_active_merge(*layer_id, *stroke_session_id) {
                            document
                                .abort_merge(*layer_id, *stroke_session_id)
                                .map_err(MergeBridgeError::Document)?;
                        }
                        Ok(if document.take_render_tree_cache_dirty() {
                            Some(document.render_tree_snapshot())
                        } else {
                            None
                        })
                    })();

                    let render_tree = match document_abort_result {
                        Ok(render_tree) => render_tree,
                        Err(error) => {
                            self.brush_execution_feedback_queue.push_back(
                                BrushExecutionMergeFeedback::MergeFailed {
                                    stroke_session_id: *stroke_session_id,
                                    message: format!("document merge abort failed: {error:?}"),
                                },
                            );
                            return Err(error);
                        }
                    };

                    if render_tree.is_some() {
                        final_render_tree = render_tree;
                    }

                    // Abort the receipt in the merge engine
                    self.tile_merge_engine
                        .abort_receipt(*receipt_id)
                        .map_err(MergeBridgeError::Tiles)?;

                    // Release stroke tiles on merge failed
                    self.brush_buffer_tile_keys
                        .write()
                        .unwrap_or_else(|_| {
                            panic!("brush buffer tile key registry write lock poisoned")
                        })
                        .release_stroke_on_merge_failed(
                            *stroke_session_id,
                            self.brush_buffer_store.as_ref(),
                        );

                    // Clear preview buffer
                    if let Some(render_tree) = self.clear_preview_buffer(*stroke_session_id) {
                        final_render_tree = Some(render_tree);
                    }

                    self.brush_execution_feedback_queue.push_back(
                        BrushExecutionMergeFeedback::MergeFailed {
                            stroke_session_id: *stroke_session_id,
                            message: format!("merge requires abort: {message}"),
                        },
                    );
                }
            }
        }

        Ok(final_render_tree)
    }

    /// Enqueue a stroke merge submission.
    ///
    /// This builds a merge plan from the stroke buffer tiles and submits it
    /// to the tile merge engine for orchestration.
    fn enqueue_stroke_merge_submission(
        &mut self,
        runtime: &mut GpuRuntime,
        stroke_session_id: u64,
        tx_token: u64,
        layer_id: u64,
    ) -> Result<(), MergeBridgeError> {
        use render_protocol::BlendMode;
        use std::collections::{HashMap, HashSet};
        use tiles::{MergePlanRequest, MergePlanTileOp};

        let Some(merge_plan) = self.build_stroke_tile_merge_plan(stroke_session_id, layer_id)
        else {
            // No tiles to merge - release stroke and mark as applied
            self.brush_buffer_tile_keys
                .write()
                .unwrap_or_else(|_| panic!("brush buffer tile key registry write lock poisoned"))
                .release_stroke_on_merge_failed(
                    stroke_session_id,
                    self.brush_buffer_store.as_ref(),
                );
            self.brush_execution_feedback_queue
                .push_back(BrushExecutionMergeFeedback::MergeApplied { stroke_session_id });
            return Ok(());
        };

        let request =
            self.build_merge_plan_request_from_plan(stroke_session_id, tx_token, merge_plan);
        let submission = self
            .tile_merge_engine
            .submit_merge_plan(request)
            .map_err(MergeBridgeError::Tiles)?;

        // Enqueue planned merge to renderer via runtime command
        runtime
            .execute(RuntimeCommand::EnqueuePlannedMerge {
                receipt: submission.renderer_submit_payload.receipt,
                gpu_merge_ops: submission.renderer_submit_payload.gpu_merge_ops,
                meta: submission.renderer_submit_payload.meta,
            })
            .map_err(|e| match e {
                RuntimeError::MergeSubmit(submit_err) => {
                    MergeBridgeError::RendererSubmit(submit_err)
                }
                other => MergeBridgeError::RendererSubmit(renderer::MergeSubmitError::ZeroBudget),
            })?;

        Ok(())
    }

    /// Build a stroke tile merge plan from the stroke buffer tiles.
    fn build_stroke_tile_merge_plan(
        &self,
        stroke_session_id: u64,
        layer_id: u64,
    ) -> Option<StrokeTileMergePlan> {
        use render_protocol::BlendMode;
        use std::collections::{HashMap, HashSet};
        use tiles::MergePlanTileOp;

        let document = self
            .document
            .read()
            .unwrap_or_else(|_| panic!("document read lock poisoned"));
        let layer_image_handle = document
            .leaf_image_handle(layer_id, stroke_session_id)
            .unwrap_or_else(|error| {
                panic!(
                    "resolve leaf image handle for merge plan failed: layer_id={} stroke_session_id={} error={error:?}",
                    layer_id, stroke_session_id
                )
            });
        let layer_image = document.image(layer_image_handle).unwrap_or_else(|| {
            panic!(
                "layer image handle missing while building merge plan: layer_id={} stroke_session_id={} image_handle={:?}",
                layer_id, stroke_session_id, layer_image_handle
            )
        });
        let layer_tiles_per_row = layer_image.tiles_per_row();
        let layer_tiles_per_column = layer_image.tiles_per_column();
        let mut tile_ops = Vec::new();
        let mut op_trace_id = 0u64;
        let mut seen_output_tiles = HashSet::new();
        let mut stroke_tile_by_key = HashMap::new();
        let brush_buffer_tile_keys = self
            .brush_buffer_tile_keys
            .read()
            .unwrap_or_else(|_| panic!("brush buffer tile key registry read lock poisoned"));
        brush_buffer_tile_keys.visit_tiles(stroke_session_id, |tile_coordinate, stroke_buffer_key| {
            if tile_coordinate.tile_x < 0 || tile_coordinate.tile_y < 0 {
                return;
            }
            let tile_x = u32::try_from(tile_coordinate.tile_x)
                .expect("positive brush tile x must convert to u32");
            let tile_y = u32::try_from(tile_coordinate.tile_y)
                .expect("positive brush tile y must convert to u32");
            if tile_x >= layer_tiles_per_row || tile_y >= layer_tiles_per_column {
                return;
            }
            if !seen_output_tiles.insert((tile_x, tile_y)) {
                panic!(
                    "duplicate output tile in stroke merge plan: stroke_session_id={} layer_id={} tile=({}, {})",
                    stroke_session_id, layer_id, tile_x, tile_y
                );
            }
            if let Some((previous_tile_x, previous_tile_y)) =
                stroke_tile_by_key.insert(stroke_buffer_key, (tile_x, tile_y))
            {
                if (previous_tile_x, previous_tile_y) != (tile_x, tile_y) {
                    panic!(
                        "duplicate stroke buffer key in merge plan: stroke_session_id={} layer_id={} key={:?} first_tile=({}, {}) duplicate_tile=({}, {})",
                        stroke_session_id, layer_id, stroke_buffer_key, previous_tile_x, previous_tile_y, tile_x, tile_y
                    );
                }
            }
            let existing_layer_key = document.leaf_tile_key_at(layer_id, tile_x, tile_y);
            tile_ops.push(MergePlanTileOp {
                tile_x,
                tile_y,
                existing_layer_key,
                stroke_buffer_key,
                blend_mode: BlendMode::Normal,
                opacity: 1.0,
                op_trace_id,
            });
            op_trace_id = op_trace_id
                .checked_add(1)
                .expect("merge op index exceeds u64");
        });
        if tile_ops.is_empty() {
            return None;
        }
        Some(StrokeTileMergePlan { layer_id, tile_ops })
    }

    /// Build a merge plan request from a merge plan.
    fn build_merge_plan_request_from_plan(
        &self,
        stroke_session_id: u64,
        tx_token: u64,
        merge_plan: StrokeTileMergePlan,
    ) -> MergePlanRequest {
        tiles::MergePlanRequest {
            stroke_session_id,
            tx_token,
            program_revision: None,
            layer_id: merge_plan.layer_id,
            tile_ops: merge_plan.tile_ops,
        }
    }
}

/// AppCore operation errors.
///
/// Classification:
/// - **LogicBug** variants: Should never occur in production. Use `debug_assert!`
///   in addition to returning the error for debuggability.
/// - **Recoverable** variants: Expected failures that callers can handle.
/// - **Unrecoverable** variants: Fatal errors where recovery is impossible.
#[derive(Debug)]
pub enum AppCoreError {
    // === Logic Bugs (indicate programming errors) ===
    /// Unexpected receipt type for command.
    UnexpectedReceipt {
        command: &'static str,
        receipt_type: &'static str,
        /// Optional receipt debug payload (use format!("{:?}", receipt) at call site)
        receipt_debug: Option<String>,
    },

    /// Unexpected error variant in error conversion.
    UnexpectedErrorVariant {
        context: &'static str,
        error: RuntimeError,
    },

    /// Tile allocation failed due to logic error (not resource exhaustion).
    TileAllocationLogicError {
        stroke_session_id: u64,
        reason: String,
    },

    /// Renderer notice missing for completion.
    MissingRendererNotice {
        receipt_id: StrokeExecutionReceiptId,
        notice_id: tiles::TileMergeCompletionNoticeId,
    },

    // === Recoverable Errors ===
    /// Runtime command failed.
    Runtime(RuntimeError),

    /// Brush render command enqueue failed.
    BrushEnqueue(BrushRenderEnqueueError),

    /// Merge operation failed.
    Merge(MergeBridgeError),

    /// Surface operation failed (can be recovered by resize/recreate).
    Surface(wgpu::SurfaceError),

    /// Resize operation failed.
    Resize {
        width: u32,
        height: u32,
        reason: String,
    },

    // === Unrecoverable Errors ===
    /// GPU resource failure during present.
    PresentFatal { source: tiles::TileGpuDrainError },

    /// Out of memory.
    OutOfMemory,
}

impl std::fmt::Display for AppCoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AppCoreError::UnexpectedReceipt {
                command,
                receipt_type,
                receipt_debug,
            } => {
                write!(
                    f,
                    "unexpected receipt '{}' for command '{}': {}",
                    receipt_type,
                    command,
                    receipt_debug.as_deref().unwrap_or("no debug info")
                )
            }
            AppCoreError::UnexpectedErrorVariant { context, error } => {
                write!(f, "unexpected error variant in {}: {:?}", context, error)
            }
            AppCoreError::TileAllocationLogicError {
                stroke_session_id,
                reason,
            } => {
                write!(
                    f,
                    "tile allocation logic error for stroke {}: {}",
                    stroke_session_id, reason
                )
            }
            AppCoreError::MissingRendererNotice {
                receipt_id,
                notice_id,
            } => {
                write!(
                    f,
                    "missing renderer notice for receipt {:?} notice {:?}",
                    receipt_id, notice_id
                )
            }
            AppCoreError::Runtime(err) => write!(f, "runtime error: {:?}", err),
            AppCoreError::BrushEnqueue(err) => write!(f, "brush enqueue error: {:?}", err),
            AppCoreError::Merge(err) => write!(f, "merge error: {:?}", err),
            AppCoreError::Surface(err) => write!(f, "surface error: {:?}", err),
            AppCoreError::Resize {
                width,
                height,
                reason,
            } => {
                write!(f, "resize to {}x{} failed: {}", width, height, reason)
            }
            AppCoreError::PresentFatal { source } => {
                write!(f, "fatal present error: {:?}", source)
            }
            AppCoreError::OutOfMemory => write!(f, "out of memory"),
        }
    }
}

impl std::error::Error for AppCoreError {}

// From implementations for external errors
impl From<RuntimeError> for AppCoreError {
    fn from(err: RuntimeError) -> Self {
        AppCoreError::Runtime(err)
    }
}

impl From<BrushRenderEnqueueError> for AppCoreError {
    fn from(err: BrushRenderEnqueueError) -> Self {
        AppCoreError::BrushEnqueue(err)
    }
}

impl From<MergeBridgeError> for AppCoreError {
    fn from(err: MergeBridgeError) -> Self {
        AppCoreError::Merge(err)
    }
}

impl From<wgpu::SurfaceError> for AppCoreError {
    fn from(err: wgpu::SurfaceError) -> Self {
        match err {
            wgpu::SurfaceError::OutOfMemory => AppCoreError::OutOfMemory,
            other => AppCoreError::Surface(other),
        }
    }
}

// Compile-time verification that AppCoreError is Send + Sync
const _: () = {
    const fn assert_send<T: Send>() {}
    const fn assert_sync<T: Sync>() {}

    // This will fail to compile if AppCoreError is not Send + Sync
    const _: () = assert_send::<AppCoreError>();
    const _: () = assert_sync::<AppCoreError>();
};
