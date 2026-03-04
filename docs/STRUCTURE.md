# Codebase Structure

**Analysis Date:** 2026-03-05

## Directory Layout

```
glaphica/
├── crates/                 # Workspace member crates
│   ├── glaphica/          # Application entry point
│   ├── app/               # Thread integration and coordination
│   ├── glaphica_core/     # Shared types and primitives
│   ├── thread_protocol/   # Thread communication protocol
│   ├── threads/           # Thread communication primitives
│   ├── gpu_runtime/       # GPU command execution
│   ├── document/          # Document model and render trees
│   ├── brushes/           # Brush system and registry
│   ├── atlas/             # Atlas tile allocation
│   ├── images/            # Image and tile management
│   ├── stroke_input/      # Input processing and smoothing
│   └── frame_scheduler/   # Frame budget management
├── docs/                  # Documentation
├── .cargo/                # Cargo configuration
├── Cargo.toml             # Workspace manifest
├── Cargo.lock             # Dependency lockfile
├── AGENTS.md              # Development guidelines
└── README.md              # Project readme
```

## Directory Purposes

**`crates/glaphica/`:**
- Purpose: Application entry point and window management
- Contains: `main.rs` with event loop, winit integration
- Key files: `src/main.rs`

**`crates/app/`:**
- Purpose: Thread coordination and state management
- Contains: MainThreadState, EngineThreadState, AppThreadIntegration
- Key files: `src/integration.rs`, `src/main_thread.rs`, `src/engine_thread.rs`

**`crates/glaphica_core/`:**
- Purpose: Core type definitions and shared primitives
- Contains: IDs, Vec2 types, dirty trackers, allocators
- Key files: `src/lib.rs`, `src/tiles.rs`, `src/vec2.rs`, `src/dirty.rs`

**`crates/thread_protocol/`:**
- Purpose: Thread communication message types
- Contains: Input ring samples, GPU commands, GPU feedback
- Key files: `src/lib.rs`, `src/gpu_command.rs`, `src/gpu_feedback.rs`

**`crates/threads/`:**
- Purpose: Thread communication channel primitives
- Contains: Ring buffers, control queues, channel creation
- Key files: `src/lib.rs`

**`crates/gpu_runtime/`:**
- Purpose: GPU context and command execution
- Contains: GpuContext, RenderExecutor, AtlasRuntime, BrushRuntime, SurfaceRuntime
- Key files: `src/context.rs`, `src/render_executor.rs`, `src/atlas_runtime.rs`

**`crates/document/`:**
- Purpose: Document model and render tree
- Contains: Document, UiLayerTree, RenderLayerTree, FlatRenderTree
- Key files: `src/lib.rs`, `src/shared_tree.rs`, `src/view.rs`

**`crates/brushes/`:**
- Purpose: Brush system and GPU pipeline management
- Contains: BrushRegistry, BrushSpec, BrushEngineRuntime, built-in brushes
- Key files: `src/lib.rs`, `src/engine_runtime.rs`, `src/brush_spec.rs`

**`crates/atlas/`:**
- Purpose: Atlas tile allocation and backend management
- Contains: BackendManager, tile allocation
- Key files: `src/lib.rs`

**`crates/images/`:**
- Purpose: Image and tile management
- Contains: Image struct, ImageLayout
- Key files: `src/lib.rs`, `src/image.rs`, `src/layout.rs`

**`crates/stroke_input/`:**
- Purpose: Input processing and stroke smoothing
- Contains: StrokeInputProcessor, Smoother, Resampler
- Key files: `src/input_processor.rs`, `src/smoother.rs`, `src/resampler.rs`

**`crates/frame_scheduler/`:**
- Purpose: Frame budget and command scheduling
- Contains: FrameBudget, FrameScheduler
- Key files: `src/lib.rs`

## Key File Locations

**Entry Points:**
- `crates/glaphica/src/main.rs`: Application entry, event loop

**Configuration:**
- `Cargo.toml`: Workspace configuration
- `.cargo/config.toml`: Cargo settings

**Core Logic:**
- `crates/app/src/integration.rs`: Thread coordination, message routing
- `crates/gpu_runtime/src/render_executor.rs`: GPU command batching
- `crates/brushes/src/engine_runtime.rs`: Brush execution pipeline
- `crates/document/src/lib.rs`: Document and render tree model

**Testing:**
- Inline `#[cfg(test)]` modules within source files
- No separate test directory

## Naming Conventions

**Files:**
- Snake_case: `engine_thread.rs`, `input_processor.rs`
- Mod.rs pattern not used (explicit module declarations)

**Directories:**
- Snake_case: `glaphica_core/`, `stroke_input/`
- Plural for collections: `threads/`, `images/`, `brushes/`

**Crate names:**
- Snake_case: `glaphica`, `gpu_runtime`, `thread_protocol`
- Descriptive of purpose

## Where to Add New Code

**New Feature:**
- Primary code: Depends on layer (e.g., new brush → `crates/brushes/src/builtin_brushes/`)
- Tests: Inline `#[cfg(test)]` module in the same file

**New Component/Module:**
- Implementation: Appropriate crate based on responsibility
  - Core types → `crates/glaphica_core/src/`
  - Thread messages → `crates/thread_protocol/src/`
  - GPU operations → `crates/gpu_runtime/src/`
  - Document features → `crates/document/src/`

**New Brush:**
- Implementation: `crates/brushes/src/builtin_brushes/`
- Registry: Update `crates/brushes/src/builtin_brushes/mod.rs`
- Registration: In `crates/glaphica/src/main.rs` or integration layer

**Utilities:**
- Shared helpers: `crates/glaphica_core/src/` if widely used
- Module-specific: In respective crate

## Special Directories

**`target/`:**
- Purpose: Build artifacts
- Generated: Yes (by Cargo)
- Committed: No (in .gitignore)

**`docs/`:**
- Purpose: Design documentation
- Contains: `pipeline.md` (rendering pipeline documentation)
- Generated: No
- Committed: Yes

**`.planning/`:**
- Purpose: GSD planning documents
- Contains: Phase plans, research, codebase analysis
- Generated: Yes (by GSD commands)
- Committed: Yes (for now, may change)

---

*Structure analysis: 2026-03-05*