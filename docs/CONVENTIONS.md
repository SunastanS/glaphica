# Coding Conventions

**Analysis Date:** 2026-03-05

## Naming Patterns

**Files:**
- Snake_case with `.rs` extension
- Descriptive names: `engine_thread.rs`, `input_processor.rs`, `stroke_input.rs`

**Functions:**
- Snake_case: `process_raw_input()`, `begin_stroke()`, `push_input_sample()`
- Getters: No `get_` prefix, e.g., `gpu_context()`, `document()`, `shared_tree()`
- Mut getters: `_mut()` suffix, e.g., `document_mut()`, `brush_runtime_mut()`

**Variables:**
- Snake_case: `input_samples`, `brush_inputs`, `gpu_commands`
- Short names in small scopes: `id`, `img`, `cmd`

**Types:**
- PascalCase for structs and enums: `MainThreadState`, `BrushEngineRuntime`, `GpuCmdMsg`
- PascalCase for traits: `BrushSpec`, `EngineBrushPipeline`, `TileSlotAllocator`
- Type aliases use PascalCase: `type AppMainThreadChannels = MainThreadChannels<...>`

**Constants:**
- SCREAMING_SNAKE_CASE: `ATLAS_TILE_SIZE`, `GUTTER_SIZE`, `IMAGE_TILE_SIZE`

**Newtype pattern:**
- Tuple structs with pub field: `BrushId(pub u64)`, `NodeId(pub u64)`, `TileKey(...)`

## Code Style

**Formatting:**
- Standard rustfmt (no custom config detected)
- Max line length: Default (100 chars)
- Indentation: 4 spaces

**Linting:**
- Standard Rust lints
- No custom clippy configuration detected
- Warning about unused variables addressed with `_` prefix

## Import Organization

**Order:**
1. Standard library imports: `use std::sync::Arc;`
2. External crates: `use wgpu::...;`, `use tokio::...;`
3. Internal crates: `use app::AppThreadIntegration;`
4. Current crate modules: `use crate::engine_thread::...;`

**Grouping:**
- Imports grouped by source with blank lines between groups
- Multiple items from same path combined: `use glaphica_core::{BrushId, CanvasVec2, ...};`

**Path Aliases:**
- None detected (uses full paths)

## Error Handling

**Patterns:**
- Custom error types per module with `std::error::Error` implementation
- Error propagation with `?` operator
- No `unwrap()` or `expect()` in production code (AGENTS.md guideline)
- `Result<T, E>` return types
- `Box<dyn std::error::Error + Send + Sync + 'static>` for trait objects

**Error type design:**
```rust
#[derive(Debug)]
pub enum WgpuBrushExecutorError {
    BrushIdOutOfRange { brush_id: BrushId },
    BrushNotConfigured { brush_id: BrushId },
    MissingTargetAtlasBackend { brush_id: BrushId },
    // ...
}

impl Display for WgpuBrushExecutorError { ... }
impl Error for WgpuBrushExecutorError { ... }
```

## Logging

**Framework:** Console output via `eprintln!`

**Patterns:**
- Structured prefixes for categorization: `[INPUT]`, `[BRUSH]`, `[ENGINE]`
- Debug logging for state transitions: `eprintln!("[INPUT] Mouse left button pressed");`
- Performance metrics: `eprintln!("[ENGINE_RX] Received {} input samples", count);`
- No production logging framework (eprintln only)

## Comments

**When to Comment:**
- Explain "why" for non-obvious logic
- Document thread safety guarantees
- Explain performance trade-offs
- No organizational comments (AGENTS.md guideline)

**JSDoc/TSDoc:**
- Rust doc comments (`///` and `//!`) for public APIs
- Example from `crates/document/src/lib.rs`:
```rust
/// Incrementally syncs tile keys from UiLayerTree to FlatRenderTree.
///
/// This method performs a lazy update: instead of rebuilding the entire
/// FlatRenderTree from UiLayerTree, it only updates the tile keys at
/// specified positions by querying UiLayerTree directly.
```

## Function Design

**Size:** Functions kept small and focused (most < 50 lines)

**Parameters:**
- Use references for large types: `&BrushInput`, `&Image`
- Mutable references for modification: `&mut self`, `&mut Image`
- Owned values for transfer: `StrokeId`, `TileKey`

**Return Values:**
- `Result<T, E>` for fallible operations
- `Option<T>` for nullable results
- `Vec<T>` for collections
- `()` for success-only operations

## Module Design

**Exports:**
- Explicit `pub use` for re-exports: `pub use brush_registry::{BrushRegistry, BrushRegistryError};`
- Private by default, explicit `pub` for API
- Module structure in `lib.rs`: `mod submodule; pub use submodule::...;`

**Barrel Files:**
- Each crate has a `lib.rs` that re-exports public API
- Submodules declared with `mod` and re-exported with `pub use`

## Variable Shadowing

**Pattern:** Use shadowing to scope clones in async/move contexts
```rust
executor.spawn({
    let task_ran = task_ran.clone();
    async move {
        *task_ran.borrow_mut() = true;
    }
});
```

**Purpose:** Minimize lifetime of borrowed references

## Key Guidelines from AGENTS.md

- No `unwrap()` or `expect()` - use `?` for error propagation
- No silent error discarding with `let _ =`
- Prioritize correctness and clarity over performance
- No organizational comments
- Treat keys and IDs seriously - no magic values
- Prefer index mapping over hash maps for performance

---

*Convention analysis: 2026-03-05*