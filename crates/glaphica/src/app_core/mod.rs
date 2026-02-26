/// App Core module.
///
/// Manages application-level business logic: document, merge orchestration, brush state.
/// Does not directly hold GPU resources - communicates with GpuRuntime via commands.
use std::collections::VecDeque;
use std::sync::{Arc, RwLock};

use brush_execution::BrushExecutionMergeFeedback;
use document::{Document, DocumentMergeError};
use renderer::MergeCompletionNotice;
use tiles::{
    BrushBufferTileRegistry, GenericR32FloatTileAtlasStore, MergePlanTileOp, TileAtlasStore,
    TileKey, TileMergeEngine, TileMergeError,
};
use view::ViewTransform;
use winit::dpi::PhysicalSize;

use crate::runtime::{GpuRuntime, RuntimeCommand, RuntimeError, RuntimeReceipt};

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

    /// View transform state.
    view_transform: ViewTransform,

    /// Brush execution feedback queue.
    brush_execution_feedback_queue: VecDeque<BrushExecutionMergeFeedback>,

    /// GC statistics.
    gc_evicted_batches_total: u64,
    gc_evicted_keys_total: u64,

    /// GPU runtime (command interface).
    runtime: GpuRuntime,

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
        view_transform: ViewTransform,
        runtime: GpuRuntime,
        disable_merge_for_debug: bool,
        perf_log_enabled: bool,
    ) -> Self {
        Self {
            document,
            tile_merge_engine,
            brush_buffer_tile_keys,
            view_transform,
            runtime,
            brush_execution_feedback_queue: VecDeque::new(),
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

    /// Get the GPU runtime.
    pub fn runtime(&self) -> &GpuRuntime {
        &self.runtime
    }

    /// Get a mutable reference to the GPU runtime.
    pub fn runtime_mut(&mut self) -> &mut GpuRuntime {
        &mut self.runtime
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

    /// Get the atlas store from runtime.
    pub fn atlas_store(&self) -> &Arc<TileAtlasStore> {
        self.runtime.atlas_store()
    }

    /// Get the brush buffer store from runtime.
    pub fn brush_buffer_store(&self) -> &Arc<GenericR32FloatTileAtlasStore> {
        self.runtime.brush_buffer_store()
    }

    /// Check if merge work is pending.
    pub fn has_pending_merge_work(&self) -> bool {
        self.tile_merge_engine.has_pending_work()
    }

    /// Get the next frame ID from runtime.
    pub fn next_frame_id(&mut self) -> u64 {
        self.runtime.next_frame_id()
    }

    /// Get the surface size from runtime.
    pub fn surface_size(&self) -> PhysicalSize<u32> {
        self.runtime.surface_size()
    }

    /// Execute a runtime command.
    pub fn execute_runtime(
        &mut self,
        command: RuntimeCommand,
    ) -> Result<RuntimeReceipt, RuntimeError> {
        self.runtime.execute(command)
    }

    /// Check if merge is disabled for debug.
    pub fn merge_disabled(&self) -> bool {
        self.disable_merge_for_debug
    }

    /// Check if performance logging is enabled.
    pub fn perf_log_enabled(&self) -> bool {
        self.perf_log_enabled
    }
}
