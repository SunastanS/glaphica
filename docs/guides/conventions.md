# Coding Conventions

**Analysis Date:** 2026-02-28

## Naming Patterns

**Files:**
- Snake case for module files: `renderer_frame.rs`, `tile_key_encoding.rs`
- Test files: `tests.rs` (centralized per crate) or inline `#[cfg(test)]` modules
- WGSL shaders: descriptive names like `tile_composite.wgsl`, `brush_dab_write.wgsl`

**Functions:**
- Snake case: `allocate_tile_keys()`, `build_leaf_tile_draw_instances()`
- Builder pattern: `with_config()`, `new()`, `default()`
- Predicate functions: `is_allocated()`, `should_rebuild()`

**Variables:**
- Full words only (no abbreviations): `tile_stride`, `atlas_layer`, `document_x`
- Snake case throughout: `brush_buffer_store`, `layer_dirty_rect_masks`
- Type-indicating suffixes when helpful: `_buffer`, `_store`, `_gpu`

**Types:**
- Pascal case for structs/enums: `TileAtlasConfig`, `RenderTreeNode`, `BrushProgramKey`
- Clear domain naming: `TileKey`, `TileAddress`, `ImageHandle`, `LayerId`

## Code Style

**Formatting:**
- No explicit `rustfmt.toml` found - using Rust defaults
- 4-space indentation (Rust standard)
- Line length ~100-120 characters observed
- Trailing commas in multi-line structs/arrays

**Linting:**
- No `.clippy.toml` detected
- Standard Rust clippy warnings expected to pass
- `debug_assert!` used for invariant checks in debug builds

## Import Organization

**Order within files:**
1. `std` library imports first
2. External crate imports (alphabetically)
3. Internal crate imports (by module hierarchy)
4. Relative imports last

**Example from `crates/renderer/src/lib.rs`:**
```rust
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock};
use std::{fs::OpenOptions, io::Write};

use model::{TILE_IMAGE, TileImage};
use render_protocol::{
    BlendMode, BrushId, BrushProgramKey, BrushRenderCommand, BufferTileCoordinate, ImageHandle,
    ImageSource, LayerId, ProgramRevision, ReferenceLayerSelection, ReferenceSetId, RenderOp,
    RenderTreeSnapshot, TransformMatrix4x4, Viewport,
};
use tiles::{
    DirtySinceResult, GenericR32FloatTileAtlasGpuArray, GenericR32FloatTileAtlasStore,
    GroupTileAtlasGpuArray, GroupTileAtlasStore, TILE_GUTTER, TILE_STRIDE, TileAddress,
    TileAtlasGpuArray, TileAtlasLayout, TileGpuDrainError, TileKey,
};
```

**Path Aliases:**
- None detected - direct paths used throughout workspace

## Error Handling

**Patterns:**
- `Result<T, E>` propagated with `?` operator
- Custom error types per crate: `TileAllocError`, `TileSetError`, `AppCoreError`
- `.expect()` with descriptive messages for initialization/setup code:
  ```rust
  .expect("request wgpu adapter")
  .expect("TileAtlasStore::with_config")
  ```
- `.unwrap_or_else()` with panic for unrecoverable states:
  ```rust
  .unwrap_or_else(|_| panic!("brush buffer tile key registry write lock poisoned"))
  ```
- `match` for explicit error variant handling

**Error type naming:**
- `{Domain}Error` pattern: `TileAtlasCreateError`, `BrushRenderEnqueueError`
- Variants describe specific failure modes: `AtlasFull`, `DuplicateTileKey`, `MissingCopyDstUsage`

**From AGENTS.md guidelines:**
- Avoid `.unwrap()` - prefer `?` propagation
- Never silently discard errors with `let _ =`
- Use `.log_err()` pattern when ignoring errors (visibility required)
- Async errors must propagate to UI layer for user feedback

## Logging

**Framework:** No formal logging framework - uses `eprintln!` and `println!` for diagnostics

**Environment-gated logging:**
- All diagnostic logs gated behind environment variables (default off)
- Common switches:
  - `GLAPHICA_BRUSH_TRACE=1` - brush execution tracing
  - `GLAPHICA_RENDER_TREE_TRACE=1` - render tree operations
  - `GLAPHICA_RENDER_TREE_INVARIANTS=1` - invariant checking
  - `GLAPHICA_PERF_LOG=1` - performance logging
  - `GLAPHICA_FRAME_SCHEDULER_TRACE=1` - frame scheduling
  - `GLAPHICA_QUIET=1` - global quiet mode (suppresses business logs)
  - `GLAPHICA_PERF_JSONL=<path>` - JSONL perf output path
  - `GLAPHICA_DISABLE_MERGE=1` - skip merge submission

**Pattern from `crates/renderer/src/lib.rs`:**
```rust
fn perf_log_enabled() -> bool {
    *ENABLED.get_or_init(|| std::env::var_os("GLAPHICA_PERF_LOG").is_some_and(|value| value != "0"))
}
```

**Log prefixes:**
- Component tagging: `[engine]`, `[error]`, `[startup]`, `[shutdown]`
- Example: `eprintln!("[engine] init complete");`

## Comments

**Style:**
- `//` for inline comments explaining "why" (not "what")
- `//!` for crate-level documentation
- `///` for public API documentation (minimal usage observed)

**From AGENTS.md:**
> "Do not write organizational or comments that summarize the code. Comments should only be written in order to explain 'why' the code is written in some way in the case there is a reason that is tricky / non-obvious."

**Examples:**
```rust
// Instance data is uploaded via `queue.write_buffer`. If we overwrite the same buffer range
// multiple times while encoding a single command buffer, earlier passes will read the latest
// contents at execution time. Treat the instance buffer as an append-only arena per submit.
```

```rust
// Reserved hook for future special node kinds (for example filter-driven layers that
// expand dirty regions and may not map to a direct image handle). The final propagation
// model is still being designed, so renderer currently uses this default identity behavior.
```

## Function Design

**Size:**
- Focused, single-responsibility functions
- Helper functions extracted for clarity: `tile_origin()`, `source_texel()`

**Parameters:**
- Group related parameters into structs when 4+ parameters: `TileAtlasConfig`, `TileAtlasLayout`
- Use `&self` for read-only operations, `&mut self` for mutations

**Return Values:**
- `Result<T, E>` for fallible operations
- `Option<T>` for potentially absent values
- Explicit boolean functions named as predicates: `is_allocated()`, `should_rebuild()`

## Module Design

**Visibility:**
- `mod` for private submodules
- `pub mod` for public crate API surface
- Internal modules prefixed with `renderer_`: `renderer_frame`, `renderer_composite`, `renderer_merge`

**Module organization from `crates/renderer/src/lib.rs`:**
```rust
mod dirty;
mod planning;
mod render_tree;
mod geometry;
mod renderer_cache_draw;
mod renderer_init;
mod renderer_frame;
mod renderer_composite;
mod renderer_draw_builders;
mod renderer_pipeline;
mod renderer_view_ops;
mod renderer_merge;
mod tests;           // Test module
mod wgsl_tests;      // WGSL shader tests
```

**Test modules:**
- `#[cfg(test)]` for inline tests
- Separate `tests.rs` file for larger test suites
- Test helpers in dedicated modules: `phase4_test_utils.rs`

## Async Patterns

**Clone-for-async pattern from AGENTS.md:**
```rust
executor.spawn({
    let task_ran = task_ran.clone();
    async move {
        *task_ran.borrow_mut() = true;
    }
});
```

**Variable shadowing for scope-limited clones:**
- Prefer shadowing to minimize borrow lifetimes in async contexts

## GPU/wgpu Conventions

**From AGENTS.md:**
- Be extremely cautious with CPU→GPU ordering
- Prefer safe ordering first, optimize after correctness
- Avoid overwriting same buffer range multiple times before `queue.submit`
- Treat instance buffers as append-only arenas per submit

**Resource naming:**
- Labels for wgpu resources: `label: Some("tiles tests")`
- Descriptive names: `"renderer.test.quadrant.layer_atlas"`

---

*Convention analysis: 2026-02-28*
