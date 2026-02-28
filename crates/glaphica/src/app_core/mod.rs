/// App Core module.
///
/// Manages application-level business logic: document, merge orchestration, brush state.
/// Does not directly hold GPU resources - communicates with GpuRuntime via commands.
use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::{Arc, RwLock};
use std::time::Instant;

use brush_execution::BrushExecutionMergeFeedback;
use document::{Document, DocumentMergeError};
use render_protocol::{BrushRenderCommand, StrokeExecutionReceiptId};
use renderer::BrushRenderEnqueueError;
use tiles::{
    BrushBufferTileRegistry, GenericR32FloatTileAtlasStore, TileAtlasStore, TileMergeEngine,
    TileMergeError, TilesBusinessResult,
};
use view::ViewTransform;
use winit::dpi::PhysicalSize;

use crate::runtime::{GpuRuntime, RuntimeCommand, RuntimeError, RuntimeReceipt};

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

    /// Debug state: last bound render tree (debug assertions only).
    #[cfg(debug_assertions)]
    last_bound_render_tree: Option<(u64, u64)>,
}

impl AppCore {
    /// Create a new AppCore with the given components.
    pub fn new(
        document: Arc<RwLock<Document>>,
        tile_merge_engine: TileMergeEngine<MergeStores>,
        brush_buffer_tile_keys: Arc<RwLock<BrushBufferTileRegistry>>,
        atlas_store: Arc<TileAtlasStore>,
        brush_buffer_store: Arc<GenericR32FloatTileAtlasStore>,
        view_transform: ViewTransform,
        disable_merge_for_debug: bool,
        perf_log_enabled: bool,
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
            #[cfg(debug_assertions)]
            last_bound_render_tree: None,
        }
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

    /// Get and increment the frame ID.
    pub fn get_next_frame_id(&mut self) -> u64 {
        let id = self.next_frame_id;
        self.next_frame_id = self
            .next_frame_id
            .checked_add(1)
            .expect("frame id overflow");
        id
    }

    /// Check if merge is disabled for debug.
    pub fn merge_disabled(&self) -> bool {
        self.disable_merge_for_debug
    }

    /// Check if performance logging is enabled.
    pub fn perf_log_enabled(&self) -> bool {
        self.perf_log_enabled
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
                // TODO: migrate set_preview_buffer_and_rebind to AppCore
                // For now, this method is not yet migrated

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
                // TODO: migrate drain_tile_gc_evictions to AppCore

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
                    // TODO: migrate clear_preview_buffer_and_rebind
                    self.brush_execution_feedback_queue.push_back(
                        BrushExecutionMergeFeedback::MergeApplied {
                            stroke_session_id: merge.stroke_session_id,
                        },
                    );
                } else {
                    // TODO: migrate enqueue_stroke_merge_submission
                    // For now, this path is not yet migrated
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
    pub fn process_renderer_merge_completions(
        &mut self,
        runtime: &mut GpuRuntime,
        frame_id: u64,
    ) -> Result<(), MergeBridgeError> {
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

        for notice in completion_notices {
            let notice_key = (notice.notice_id, notice.receipt_id);
            let _ = renderer_notice_by_key.remove(&notice_key).ok_or(
                MergeBridgeError::MissingRendererNotice {
                    receipt_id: notice.receipt_id,
                    notice_id: notice.notice_id,
                },
            )?;

            // TODO: migrate ack_merge_result to runtime command
            // For now, direct call is needed
            // self.renderer.ack_merge_result(renderer_notice)
            //     .map_err(MergeBridgeError::RendererAck)?;

            self.tile_merge_engine
                .ack_merge_result(notice.receipt_id, notice.notice_id)
                .map_err(MergeBridgeError::Tiles)?;
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

        // TODO: migrate apply_tiles_business_results to AppCore
        // TODO: migrate drain_tile_gc_evictions to AppCore

        if self.perf_log_enabled {
            eprintln!(
                "[merge_bridge_perf] frame_id={} process_merge_completions_cpu_ms={:.3}",
                frame_id,
                perf_started.elapsed().as_secs_f64() * 1_000.0,
            );
        }

        Ok(())
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

// Ensure error types are thread-safe for future threading model
unsafe impl Send for AppCoreError {}
unsafe impl Sync for AppCoreError {}

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
