# Architecture

**Analysis Date:** 2026-02-28

## Pattern Overview

**Overall:** Event-driven GUI application with tile-based rendering architecture

**Key Characteristics:**
- Rust workspace with 16 crates following strict separation of concerns
- Tile-based rendering with atlas texture management for efficient GPU compositing
- Multi-threaded brush execution with lock-free ring buffers for real-time input processing
- Render tree abstraction supporting layer hierarchy with blend modes
- Stroke lifecycle management with preview buffers and merge operations

## Layers

**Application Layer (`crates/glaphica/`):**
- Purpose: Main application binary, event loop, and orchestration
- Location: `crates/glaphica/src/main.rs`, `crates/glaphica/src/lib.rs`
- Contains: Winit event loop handling, GPU state facade, CLI argument parsing, input recording/replay
- Depends on: All other crates
- Used by: End users (binary entry point)

**GPU Runtime Layer (`crates/glaphica/src/runtime/`, `crates/renderer/`):**
- Purpose: GPU resource management, frame rendering, command submission
- Location: `crates/glaphica/src/runtime/`, `crates/renderer/src/lib.rs`
- Contains: `GpuRuntime`, `Renderer`, wgpu device/queue management, surface configuration
- Depends on: `render_protocol`, `tiles`, `model`, `view`
- Used by: `GpuState` for rendering operations

**Business Logic Layer (`crates/glaphica/src/app_core/`):**
- Purpose: Document state, merge orchestration, view transforms
- Location: `crates/glaphica/src/app_core/`
- Contains: `AppCore`, merge lifecycle management, preview buffer handling, tile GC
- Depends on: `document`, `render_protocol`, `tiles`, `renderer`
- Used by: `GpuState` as the business logic delegate

**Rendering Layer (`crates/renderer/`):**
- Purpose: GPU command encoding, render tree traversal, dirty rect propagation
- Location: `crates/renderer/src/lib.rs` and submodules
- Contains: Frame planning, composite pass execution, cache management, merge GPU operations
- Depends on: `render_protocol`, `tiles`, `model`
- Used by: `GpuRuntime` for frame rendering

**Document Layer (`crates/document/`):**
- Purpose: Layer tree management, tile image storage, render tree snapshot generation
- Location: `crates/document/src/lib.rs`
- Contains: `Document`, layer hierarchy, dirty history tracking, merge commit handling
- Depends on: `render_protocol`, `tiles`, `model`, `slotmap`
- Used by: `AppCore` for document state

**Protocol Layer (`crates/render_protocol/`, `crates/protocol/`, `crates/replay_protocol/`):**
- Purpose: Cross-module message types and data structures
- Location: `crates/render_protocol/src/lib.rs`
- Contains: Render tree snapshots, brush commands, merge receipts, render ops
- Depends on: `slotmap`
- Used by: All crates for type-safe communication

**Tile System Layer (`crates/tiles/`):**
- Purpose: Tile atlas allocation, GPU array management, merge engine
- Location: `crates/tiles/src/lib.rs` and submodules
- Contains: `TileAtlasStore`, `TileMergeEngine`, tile key encoding, dirty bitsets
- Depends on: `model`, `render_protocol`, `bitvec`
- Used by: `renderer`, `document`, `glaphica`

**Brush Execution Layer (`crates/brush_execution/`, `crates/driver/`):**
- Purpose: Real-time stroke processing, brush command generation
- Location: `crates/brush_execution/src/lib.rs`, `crates/driver/src/lib.rs`
- Contains: `BrushExecutionRuntime`, `DriverEngine`, input sampling algorithms
- Depends on: `render_protocol`, `driver`, `rtrb`
- Used by: Main app for stroke-to-render-command pipeline

**View/Transform Layer (`crates/view/`, `crates/model/`):**
- Purpose: Canvas transformations, tile image model
- Location: `crates/view/src/lib.rs`, `crates/model/src/lib.rs`
- Contains: `ViewTransform`, `TileImage`, tile layout constants
- Depends on: Minimal (model has no internal deps)
- Used by: All layers for coordinate transforms and tile representation

**Scheduler Layer (`crates/frame_scheduler/`):**
- Purpose: Adaptive frame scheduling based on brush activity
- Location: `crates/frame_scheduler/src/lib.rs`
- Contains: `FrameScheduler`, quota calculation
- Depends on: None
- Used by: Main app for frame rate control

**Engine/Threading Layer (`crates/engine/`):**
- Purpose: Cross-thread communication primitives for Phase 4 threaded execution
- Location: `crates/engine/src/lib.rs`
- Contains: `MainThreadChannels`, `EngineThreadChannels`, ring buffers
- Depends on: `protocol`, `crossbeam-*`, `rtrb`
- Used by: `GpuState` for threaded execution mode

## Data Flow

**Input-to-Pixel Flow:**

1. **Input Ingestion**: `winit` window events → `main.rs` event handler
2. **Driver Processing**: Raw pointer events → `DriverEngine` → `SampleChunk` stream
3. **Brush Execution**: `SampleChunk` → `BrushExecutionRuntime` (separate thread) → `BrushRenderCommand` stream
4. **Command Enqueue**: `BrushRenderCommand` → `Renderer::enqueue_brush_render_command()` → GPU command queue
5. **Frame Planning**: `Renderer::render()` → `FramePlan` construction based on dirty rects
6. **Render Tree Traversal**: `CompositeNodePlan` tree walk → bind group + pipeline selection
7. **Draw Instance Building**: `build_leaf_tile_draw_instances()` → GPU instance buffer
8. **Pass Execution**: Composite passes (content → slot → surface) → `queue.submit()`
9. **Merge Lifecycle**: `MergeBuffer` command → GPU merge ops → `MergeAck` → document commit

**State Management:**
- Document state: `Arc<RwLock<Document>>` for concurrent read access
- GPU state: `GpuState` with `GpuExecMode` enum for single-threaded vs threaded execution
- Brush execution: Lock-free ring buffers (`rtrb`) for real-time sample/command queues
- Render tree: `Arc<RenderNodeSnapshot>` for immutable snapshots with revision tracking

## Key Abstractions

**Render Tree (`RenderTreeSnapshot`, `RenderNodeSnapshot`):**
- Purpose: Hierarchical composition description for a frame
- Examples: `crates/render_protocol/src/lib.rs` (types), `crates/document/src/lib.rs` (generation)
- Pattern: Immutable snapshot with revision number, tree of `Leaf`/`Group` nodes

**Tile Atlas (`TileAtlasStore`, `TileAtlasGpuArray`):**
- Purpose: GPU texture array management for tiled images
- Examples: `crates/tiles/src/lib.rs`, `crates/tiles/src/atlas/`
- Pattern: Slot-based allocation with generation IDs for safe reclamation

**Brush Command Protocol (`BrushRenderCommand`):**
- Purpose: Stroke lifecycle commands from execution to rendering
- Examples: `crates/render_protocol/src/lib.rs`
- Pattern: Enum with `BeginStroke`, `AllocateBufferTiles`, `PushDabChunkF32`, `MergeBuffer`, `EndStroke`

**Merge Receipt System (`StrokeExecutionReceipt`, `ReceiptTerminalState`):**
- Purpose: Track GPU merge operations from submission to finalization
- Examples: `crates/render_protocol/src/lib.rs`, `crates/renderer/src/renderer_merge.rs`
- Pattern: Receipt ID lifecycle: `Pending` → `Succeeded`/`Failed` → `Finalized`/`Aborted`

**View Transform (`ViewTransform`):**
- Purpose: Screen ↔ canvas coordinate conversion with pan/zoom/rotate
- Examples: `crates/view/src/lib.rs`
- Pattern: 4x4 matrix composition with anchor-point zoom

## Entry Points

**`main()` (`crates/glaphica/src/main.rs`):**
- Location: Line 1045
- Triggers: Application launch
- Responsibilities: CLI parsing, event loop creation, `App` initialization, runtime startup

**`GpuState::new()` (`crates/glaphica/src/lib.rs`):**
- Location: Line 979
- Triggers: Application resume (window creation)
- Responsibilities: wgpu instance/device setup, renderer init, document creation, initial render tree binding

**`Renderer::render()` (`crates/renderer/src/lib.rs` via `renderer_frame.rs`):**
- Location: Called from `GpuState::render()` → `AppCore::render()`
- Triggers: Every frame redraw
- Responsibilities: View op ingestion, dirty propagation, frame planning, pass execution, present

**`BrushExecutionRuntime::start()` (`crates/brush_execution/src/lib.rs`):**
- Location: Line 97
- Triggers: Application initialization
- Responsibilities: Spawn brush execution thread, create ring buffers, start command processing loop

## Error Handling

**Strategy:** Fail-fast with context-rich panics for logic bugs; graceful degradation for recoverable GPU errors

**Patterns:**
- **Poisoned locks**: `unwrap_or_else(|_| panic!("lock poisoned"))` for `RwLock`/`Mutex`
- **GPU surface errors**: Match on `wgpu::SurfaceError` with recovery attempts (resize, redraw request)
- **Fatal errors**: `OutOfMemory`, `PresentFatal` set `fatal_error_seen` flag and exit event loop
- **Protocol violations**: Panic with full context (IDs, expected vs actual) for invariant violations
- **Merge failures**: Log error, release stroke tiles, send `MergeFailed` feedback, continue

**Error types:**
- `AppCoreError`: Unified error enum for application-level errors
- `MergeBridgeError`: Bridge errors between tiles, renderer, document
- `BrushRenderEnqueueError`: Brush command validation errors
- `TileMergeError`: Tile system merge operation errors

## Cross-Cutting Concerns

**Logging:** Environment-gated trace flags (default off):
- `GLAPHICA_BRUSH_TRACE=1`: Stroke and brush command tracing
- `GLAPHICA_RENDER_TREE_TRACE=1`: Render tree binding and semantic hash logging
- `GLAPHICA_RENDER_TREE_INVARIANTS=1`: Semantic consistency checks (debug builds)
- `GLAPHICA_PERF_LOG=1`: Performance timing and merge audit logs
- `GLAPHICA_FRAME_SCHEDULER_TRACE=1`: Frame scheduling decisions
- `GLAPHICA_QUIET=1`: Global quiet mode, suppresses all business logs

**Validation:**
- Render tree validation: `RenderTreeSnapshot::validate_executable()` against support matrix
- Tile address uniqueness: Debug assertions for duplicate tile key resolution
- Merge plan validation: Duplicate output tile detection, stroke buffer key uniqueness

**Authentication:** Not applicable (desktop application)

**Observability Hooks:**
- `take_latest_gpu_timing_report()`: GPU timestamp query results
- `semantic_state_digest()`: Document/render tree revision + hash for replay comparison
- Output trace recording: `OutputTraceRecorder` for deterministic regression testing

---

*Architecture analysis: 2026-02-28*
