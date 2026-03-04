# Architecture

**Analysis Date:** 2026-03-05

## Pattern Overview

**Overall:** Multi-threaded pipeline with lock-free communication

**Key Characteristics:**
- Thread-per-concern model (main thread, engine thread)
- Lock-free ring buffers for high-frequency input data
- Bounded queues for control events
- GPU command batching and execution pipeline
- Render tree with dirty tracking

## Layers

**Application Layer:**
- Purpose: Window management, user input handling, presentation
- Location: `crates/glaphica/src/main.rs`
- Contains: Event loop, window creation, input routing
- Depends on: app crate, winit, tokio
- Used by: Entry point (main)

**Integration Layer:**
- Purpose: Thread coordination, state management, message routing
- Location: `crates/app/src/integration.rs`, `crates/app/src/main_thread.rs`, `crates/app/src/engine_thread.rs`
- Contains: `AppThreadIntegration`, `MainThreadState`, `EngineThreadState`
- Depends on: All core crates, thread_protocol
- Used by: Application layer

**Engine Layer:**
- Purpose: Stroke processing, brush execution, tile allocation
- Location: `crates/brushes/src/engine_runtime.rs`, `crates/stroke_input/src/`
- Contains: Brush engine runtime, input processing, stroke smoothing
- Depends on: glaphica_core, thread_protocol
- Used by: Integration layer

**GPU Runtime Layer:**
- Purpose: GPU command execution, atlas management, rendering
- Location: `crates/gpu_runtime/src/`
- Contains: Render executor, atlas runtime, brush runtime, surface runtime
- Depends on: wgpu, atlas, brushes
- Used by: Integration layer

**Document Layer:**
- Purpose: Document model, layer tree, render tree construction
- Location: `crates/document/src/lib.rs`, `crates/document/src/shared_tree.rs`
- Contains: Document, UiLayerTree, RenderLayerTree, FlatRenderTree
- Depends on: images, glaphica_core
- Used by: Integration layer, Engine layer

**Core Types Layer:**
- Purpose: Shared type definitions, constants, primitives
- Location: `crates/glaphica_core/src/lib.rs`
- Contains: BrushId, NodeId, TileKey, Vec2 types, dirty trackers, ID allocators
- Depends on: bitflags, arc-swap
- Used by: All layers

**Thread Protocol Layer:**
- Purpose: Thread communication primitives and message types
- Location: `crates/thread_protocol/src/lib.rs`, `crates/threads/src/lib.rs`
- Contains: Input ring samples, GPU commands, GPU feedback, channel primitives
- Depends on: glaphica_core, crossbeam, rtrb
- Used by: Integration layer, Engine layer

## Data Flow

**Input Processing Flow:**

1. User input (mouse/touch) → winit event
2. Main thread: `AppThreadIntegration.push_input_sample()` → Input ring buffer
3. Engine thread: `EngineInputRingConsumer.drain_batch_with_wait()` → Input samples
4. Engine thread: `StrokeInputProcessor.process_input()` → BrushInput
5. Engine thread: `BrushEngineRuntime.process_stroke_input()` → GpuCmdMsg (DrawOp, CopyOp, ClearOp)
6. Engine thread: GPU command sender → GPU command queue
7. Main thread: GPU command receiver → `MainThreadState.process_gpu_commands()`
8. Main thread: Render executor → GPU execution
9. Main thread: Surface presentation

**Render Tree Flow:**

1. Document modification → UiLayerTree update
2. `Document.build_flat_render_tree()` → FlatRenderTree
3. `SharedRenderTree` update → Arc-swap
4. Main thread: Render tree read → Render executor
5. Render executor: Batch tile rendering commands

**State Management:**
- Shared state via `Arc` and `arc-swap`
- Lock-free reads with atomic swaps for updates
- Thread-local state in MainThreadState and EngineThreadState

## Key Abstractions

**Tile System:**
- Purpose: Grid-based image decomposition for GPU efficiency
- Examples: `crates/glaphica_core/src/tiles.rs`, `crates/atlas/src/lib.rs`, `crates/images/src/image.rs`
- Pattern: 64x64 pixel tiles with 1-pixel gutters, allocated from atlas backends

**Brush Pipeline:**
- Purpose: Pluggable brush system with GPU shader support
- Examples: `crates/brushes/src/brush_spec.rs`, `crates/brushes/src/gpu_pipeline_spec.rs`
- Pattern: Trait-based brush definition (`BrushSpec`), registry pattern for brush lookup

**Input Ring Buffer:**
- Purpose: Lossy, high-frequency input sample transport
- Examples: `crates/threads/src/lib.rs` (SharedInputRing)
- Pattern: SPSC ring buffer with overwrite-on-full semantics

**Render Tree:**
- Purpose: Hierarchical document representation for rendering
- Examples: `crates/document/src/lib.rs` (UiLayerTree, RenderLayerTree, FlatRenderTree)
- Pattern: Tree transformation pipeline (UI → Render → Flat)

## Entry Points

**Application Entry:**
- Location: `crates/glaphica/src/main.rs`
- Triggers: Process start
- Responsibilities: Event loop creation, window management, AppThreadIntegration lifecycle

**GPU Context Initialization:**
- Location: `crates/gpu_runtime/src/context.rs`
- Triggers: `GpuContext::init()` during app startup
- Responsibilities: wgpu instance/adapter/device creation, async initialization

**Stroke Begin:**
- Location: `crates/app/src/integration.rs` (`AppThreadIntegration.begin_stroke()`)
- Triggers: Mouse button press
- Responsibilities: Stroke ID allocation, input processor initialization, control event push

**Frame Processing:**
- Location: `crates/app/src/integration.rs` (`AppThreadIntegration.process_engine_frame()`)
- Triggers: Event loop `about_to_wait`
- Responsibilities: Input sample drain, brush processing, GPU command routing

## Error Handling

**Strategy:** Typed errors with `std::error::Error` trait

**Patterns:**
- Custom error enums per module (e.g., `WgpuBrushExecutorError`, `EngineBrushDispatchError`)
- Error propagation via `Result<T, E>` and `?` operator
- `Box<dyn std::error::Error + Send + Sync>` for trait object errors
- Error context via `Display` implementations
- No panic propagation across threads (explicit error types)

## Cross-Cutting Concerns

**Logging:** Console output with structured prefixes (eprintln!)
**Validation:** Type-safe IDs (BrushId, NodeId, TileKey) prevent mixing
**Authentication:** None (single-user desktop app)
**Concurrency:** Lock-free SPSC/MPMC patterns, arc-swap for shared state

---

*Architecture analysis: 2026-03-05*