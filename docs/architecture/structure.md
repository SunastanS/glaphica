# Codebase Structure

**Analysis Date:** 2026-02-28

## Directory Layout

```
glaphica/
├── Cargo.toml                  # Workspace root (16 crates)
├── AGENTS.md                   # Agent coding guidelines
├── crates/
│   ├── glaphica/               # Main binary + app orchestration
│   │   ├── src/
│   │   │   ├── main.rs         # Application entry point
│   │   │   ├── lib.rs          # GpuState, AppCore bridge
│   │   │   ├── app_core/       # Business logic layer
│   │   │   ├── runtime/        # GPU runtime (single-threaded mode)
│   │   │   ├── engine_bridge.rs# Phase 4 threaded execution bridge
│   │   │   ├── driver_bridge.rs# Driver → brush execution bridge
│   │   │   └── ...
│   │   └── Cargo.toml
│   ├── renderer/               # GPU rendering engine
│   │   ├── src/
│   │   │   ├── lib.rs          # Renderer public API
│   │   │   ├── renderer_init.rs# GPU resource initialization
│   │   │   ├── renderer_frame.rs# Frame planning + execution
│   │   │   ├── renderer_composite.rs# Render tree traversal
│   │   │   ├── renderer_merge.rs# GPU merge operations
│   │   │   ├── renderer_cache_draw.rs# Leaf draw caching
│   │   │   ├── dirty.rs        # Dirty rect propagation
│   │   │   ├── planning.rs     # Frame/group decision engine
│   │   │   └── *.wgsl          # WGSL shaders
│   │   ├── DESIGN.md           # Renderer architecture doc
│   │   └── Cargo.toml
│   ├── render_protocol/        # Cross-module message types
│   │   ├── src/lib.rs          # Render ops, commands, receipts
│   │   └── Cargo.toml
│   ├── document/               # Document state management
│   │   ├── src/lib.rs          # Document, layer tree, dirty history
│   │   └── Cargo.toml
│   ├── tiles/                  # Tile atlas system
│   │   ├── src/
│   │   │   ├── lib.rs          # Tile allocation, addresses
│   │   │   ├── atlas/          # Atlas layout, tier management
│   │   │   ├── engine/         # Tile merge engine
│   │   │   └── execution/      # Tile execution helpers
│   │   └── Cargo.toml
│   ├── brush_execution/        # Real-time brush processing
│   │   ├── src/lib.rs          # BrushExecutionRuntime, command generation
│   │   └── Cargo.toml
│   ├── driver/                 # Input sampling pipeline
│   │   ├── src/
│   │   │   ├── lib.rs          # DriverEngine, stroke sessions
│   │   │   └── no_smoothing_uniform_resampling.rs
│   │   └── Cargo.toml
│   ├── frame_scheduler/        # Adaptive frame scheduling
│   │   ├── src/lib.rs          # FrameScheduler, quota calculation
│   │   └── Cargo.toml
│   ├── view/                   # Viewport transforms
│   │   ├── src/lib.rs          # ViewTransform (pan/zoom/rotate)
│   │   └── Cargo.toml
│   ├── model/                  # Core data structures
│   │   ├── src/lib.rs          # TileImage, ImageLayout, constants
│   │   └── Cargo.toml
│   ├── engine/                 # Phase 4 threading primitives
│   │   ├── src/lib.rs          # MainThreadChannels, EngineThreadChannels
│   │   └── Cargo.toml
│   ├── protocol/               # Low-level protocol types
│   │   ├── src/lib.rs          # GpuCmdMsg, InputRingSample
│   │   └── Cargo.toml
│   ├── replay_protocol/        # Output trace types
│   │   ├── src/lib.rs          # OutputPayload, OutputPhase
│   │   └── Cargo.toml
│   ├── window_replay/          # Input/output trace recording
│   │   ├── src/lib.rs          # InputTraceRecorder, OutputTraceRecorder
│   │   └── Cargo.toml
│   ├── code_analysis/          # Code analysis tools (binary)
│   │   ├── src/main.rs
│   │   └── Cargo.toml
│   └── AGENTS.md               # Per-crate agent instructions
├── docs/
│   ├── guides/               # Developer guides
│   │   ├── coding-guidelines.md
│   │   ├── debug-playbook.md
│   │   └── wgpu-guide.md
│   ├── architecture/         # Architecture documentation
│   ├── planning/             # Project planning docs
│   └── archive/              # Archived docs
├── tests/                      # Integration test resources
│   ├── resources/
│   └── records/
└── .github/
    └── workflows/
        └── ci.yml              # CI pipeline
```

## Directory Purposes

**`crates/glaphica/`:**
- Purpose: Main application binary and orchestration layer
- Contains: Event loop, GPU state facade, CLI parsing, input recording/replay
- Key files: `src/main.rs` (binary entry), `src/lib.rs` (GpuState, AppCore)

**`crates/renderer/`:**
- Purpose: GPU rendering engine with wgpu
- Contains: Frame planning, composite traversal, dirty propagation, merge execution
- Key files: `src/lib.rs` (public API), `src/renderer_frame.rs` (frame pipeline), `src/renderer_merge.rs` (merge logic)

**`crates/render_protocol/`:**
- Purpose: Shared message types for rendering, brush, merge communication
- Contains: `RenderOp`, `BrushRenderCommand`, `RenderTreeSnapshot`, merge receipts
- Key files: `src/lib.rs` (all protocol types)
- **Collaboration Rule**: Receiver/executor side may implement first; initiator/caller side must report first

**`crates/document/`:**
- Purpose: Document state, layer hierarchy, render tree generation
- Contains: `Document`, layer tree, dirty history, merge commit handling
- Key files: `src/lib.rs` (Document implementation)

**`crates/tiles/`:**
- Purpose: Tile atlas allocation and GPU array management
- Contains: `TileAtlasStore`, `TileMergeEngine`, tile key encoding
- Key files: `src/lib.rs` (tile allocation), `src/engine/` (merge planning)

**`crates/brush_execution/`:**
- Purpose: Real-time stroke processing on dedicated thread
- Contains: `BrushExecutionRuntime`, tile allocation logic, command generation
- Key files: `src/lib.rs` (runtime loop)

**`crates/driver/`:**
- Purpose: Pointer input sampling and chunking
- Contains: `DriverEngine`, `StrokeChunkSplitter`, resampling algorithms
- Key files: `src/lib.rs` (driver pipeline), `src/no_smoothing_uniform_resampling.rs`

**`crates/model/`:**
- Purpose: Foundational data structures (no internal dependencies)
- Contains: `TileImage<K>`, `ImageLayout`, tile constants (`TILE_IMAGE`, `TILE_STRIDE`)
- Key files: `src/lib.rs` (tile image with dirty bit tracking)

**`crates/view/`:**
- Purpose: Canvas view transformations
- Contains: `ViewTransform` (pan, zoom about point, rotate)
- Key files: `src/lib.rs`

**`crates/frame_scheduler/`:**
- Purpose: Adaptive frame rate control based on brush activity
- Contains: `FrameScheduler`, quota calculation
- Key files: `src/lib.rs`

**`crates/engine/`:**
- Purpose: Cross-thread communication for Phase 4 threaded execution
- Contains: Channel types, ring buffer wrappers
- Key files: `src/lib.rs`

## Key File Locations

**Entry Points:**
- `crates/glaphica/src/main.rs`: Application entry point, CLI parsing, event loop
- `crates/glaphica/src/lib.rs::GpuState::new()`: GPU initialization
- `crates/brush_execution/src/lib.rs::BrushExecutionRuntime::start()`: Brush thread spawn

**Configuration:**
- `Cargo.toml` (workspace root): Workspace members, resolver = "3", edition = "2024"
- `crates/*/Cargo.toml`: Per-crate dependencies

**Core Logic:**
- `crates/glaphica/src/app_core/`: Business logic (merge, preview, GC)
- `crates/renderer/src/renderer_frame.rs`: Frame pipeline construction
- `crates/document/src/lib.rs`: Document state management
- `crates/tiles/src/engine/`: Merge planning engine

**Testing:**
- `crates/renderer/src/tests.rs`: Renderer unit tests (wgpu-backed)
- `crates/renderer/src/wgsl_tests.rs`: WGSL shader parse tests
- `crates/*/src/lib.rs`: Inline `#[cfg(test)]` modules

**Shaders:**
- `crates/renderer/src/tile_composite.wgsl`: RGBA tile composite
- `crates/renderer/src/tile_composite_r32float.wgsl`: R32Float brush buffer composite
- `crates/renderer/src/brush_dab_write.wgsl`: Brush compute shader
- `crates/renderer/src/merge_tile.wgsl`: GPU merge operations

## Naming Conventions

**Files:**
- `snake_case.rs` for modules: `renderer_frame.rs`, `dirty.rs`
- `lib.rs` for crate roots
- `main.rs` for binaries
- `*.wgsl` for WGSL shaders

**Types:**
- `PascalCase` for structs/enums: `GpuState`, `RenderTreeSnapshot`
- `snake_case` for functions/methods: `enqueue_brush_render_command()`
- Trait names descriptive: `RenderDataResolver`, `SampleEmitter`

**Modules:**
- Private modules: `mod renderer_frame;` (in `lib.rs`)
- Public re-exports: `pub use renderer_merge::{...};`

**Constants:**
- `SCREAMING_SNAKE_CASE`: `TILE_IMAGE`, `BRUSH_COMMAND_BATCH_CAPACITY`
- Located in defining crate's `lib.rs` or dedicated `constants.rs`

## Where to Add New Code

**New Brush Program/Shader:**
- WGSL shader: `crates/renderer/src/your_shader.wgsl`
- Pipeline creation: `crates/renderer/src/renderer_init.rs`
- Protocol types: `crates/render_protocol/src/lib.rs` (follow collaboration rule)

**New Render Node Type:**
- Protocol definition: `crates/render_protocol/src/lib.rs::RenderNodeSnapshot`
- Composite traversal: `crates/renderer/src/renderer_composite.rs`
- Dirty propagation: `crates/renderer/src/dirty.rs`

**New Tile Atlas Format:**
- Store type: `crates/tiles/src/` (new module or extend existing)
- GPU array: `crates/tiles/src/engine/` or `crates/renderer/src/renderer_init.rs`
- Shader update: `crates/renderer/src/your_format_composite.wgsl`

**New Input Sampling Algorithm:**
- Algorithm impl: `crates/driver/src/your_algorithm.rs`
- Trait: Implement `InputSamplingAlgorithm`
- Integration: `crates/glaphica/src/main.rs` (DriverEngine construction)

**New Document Operation:**
- Document methods: `crates/document/src/lib.rs`
- Merge handling: Add to `DocumentMergeError` if needed
- Dirty tracking: Update `LayerDirtyHistory` if operation affects tiles

**Utilities:**
- Shared helpers: `crates/model/src/lib.rs` (if widely used) or crate-local `utils.rs`
- Test helpers: `crates/tiles/src/lib.rs` with `#[cfg(feature = "test-helpers")]`

## Special Directories

**`crates/glaphica/src/app_core/`:**
- Purpose: Business logic extracted from GpuState (Phase 2.5 refactor)
- Contains: Merge bridge, preview buffer management, tile GC
- Generated: No
- Committed: Yes

**`crates/renderer/src/` (submodules):**
- Purpose: Renderer compartmentalized by frame pipeline stage
- Contains: `dirty.rs`, `planning.rs`, `render_tree.rs`, `geometry.rs`, `renderer_*.rs`
- Note: All submodules are `pub(crate)` or private, only `Renderer` struct is public

**`crates/tiles/src/atlas/`, `crates/tiles/src/engine/`, `crates/tiles/src/execution/`:**
- Purpose: Tile system subdomains
- Contains: Atlas layout, merge engine, execution helpers
- Note: Most types re-exported from `lib.rs`

**`docs/guides/`:**
- Purpose: Developer guides for debugging, wgpu, coding standards
- Contains: `debug-playbook.md`, `wgpu-guide.md`, `coding-guidelines.md`
- Read before: Making significant changes

**`tests/resources/`, `tests/records/`:**
- Purpose: Integration test fixtures and recorded traces
- Contains: Sample images, input trace files for replay testing
- Used by: `--record-input`, `--replay-input`, `--record-output` CLI flags

---

*Structure analysis: 2026-02-28*
