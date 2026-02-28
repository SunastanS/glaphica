# AppCore Error Handling Design

## Executive Summary

This document proposes a systematic error handling redesign for AppCore to replace excessive panic usage with structured error types. The design enables better debugging, incremental recovery, and clearer error classification while preserving debuggability for logic bugs.

---

## 1. Current State Analysis

### 1.1 Panic Locations in AppCore (9 total)

| Line | Location | Error Type | Classification | Recommendation |
|------|----------|------------|----------------|----------------|
| 314 | `resize()` | `RuntimeError` from resize command | **Recoverable** | Return `Result` |
| 335 | `render()` | Unexpected receipt type | **Logic Bug** | `debug_assert!` + structured error |
| 340 | `render()` | `PresentError::TileDrain` | **Unrecoverable** | Keep panic (GPU resource failure) |
| 345 | `render()` | Unexpected `RuntimeError` variant | **Logic Bug** | `debug_assert!` + structured error |
| 377 | `enqueue_brush_render_command()` | Poisoned lock | **Logic Bug** | Keep panic (thread safety violation) |
| 385-387 | `enqueue_brush_render_command()` | Tile allocation failure | **Logic Bug** | `debug_assert!` + structured error |
| 395 | `enqueue_brush_render_command()` | Poisoned lock | **Logic Bug** | Keep panic |
| 420 | `enqueue_brush_render_command()` | Poisoned lock | **Logic Bug** | Keep panic |
| 477 | `process_renderer_merge_completions()` | Unexpected receipt type | **Logic Bug** | `debug_assert!` + structured error |

### 1.2 Current Result-Returning Methods

```rust
// app_core/mod.rs
pub fn execute_runtime(&mut self, command: RuntimeCommand) -> Result<RuntimeReceipt, RuntimeError>
pub fn render(&mut self) -> Result<(), wgpu::SurfaceError>
pub fn enqueue_brush_render_command(&mut self, command: BrushRenderCommand) -> Result<(), BrushRenderEnqueueError>
pub fn process_renderer_merge_completions(&mut self, frame_id: u64) -> Result<(), MergeBridgeError>
```

**Issue:** `resize()` returns `()` but panics on error - inconsistent with error handling pattern.

### 1.3 Existing Error Type Hierarchy

```
MergeBridgeError (app_core/mod.rs)
├── RendererPoll(renderer::MergePollError)
├── RendererAck(renderer::MergeAckError)
├── RendererSubmit(renderer::MergeSubmitError)
├── RendererFinalize(renderer::MergeFinalizeError)
├── Tiles(TileMergeError)
├── TileImageApply(tiles::TileImageApplyError)
└── Document(DocumentMergeError)

RuntimeError (runtime/protocol.rs)
├── PresentError(renderer::PresentError)
├── SurfaceError(wgpu::SurfaceError)
├── ResizeError(String)
├── BrushEnqueueError(renderer::BrushRenderEnqueueError)
├── MergeSubmit(renderer::MergeSubmitError)
└── MergePoll(renderer::MergePollError)

External Errors:
├── BrushRenderEnqueueError (renderer)
├── wgpu::SurfaceError
├── TileMergeError (tiles)
└── DocumentMergeError (document)
```

**Issue:** No unified `AppCoreError` type - callers must handle multiple error types.

---

## 2. Proposed Error Type Design

### 2.1 AppCoreError Enum

```rust
/// AppCore operation errors.
/// 
/// Classification:
/// - `LogicBug` variants: Should never occur in production. Use `debug_assert!` 
///   in addition to returning the error for debuggability.
/// - `Recoverable` variants: Expected failures that callers can handle.
/// - `Unrecoverable` variants: Fatal errors where recovery is impossible.
#[derive(Debug)]
pub enum AppCoreError {
    // === Logic Bugs (indicate programming errors) ===
    
    /// Unexpected receipt type for command.
    /// Indicates mismatch between command and receipt handling.
    UnexpectedReceipt {
        command: &'static str,
        received_receipt: &'static str,
    },
    
    /// Unexpected error variant in error conversion.
    /// Indicates incomplete error handling.
    UnexpectedErrorVariant {
        context: &'static str,
        error: String,
    },
    
    /// Tile allocation failed due to logic error (not resource exhaustion).
    /// Indicates invariant violation in tile management.
    TileAllocationLogicError {
        stroke_session_id: u64,
        reason: String,
    },
    
    /// Renderer notice missing for completion.
    /// Indicates synchronization bug between renderer and tile engine.
    MissingRendererNotice {
        receipt_id: StrokeExecutionReceiptId,
        notice_id: TileMergeCompletionNoticeId,
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
    /// Cannot recover without restarting GPU context.
    PresentFatal {
        source: tiles::TileGpuDrainError,
    },
    
    /// Out of memory.
    OutOfMemory,
}

impl std::fmt::Display for AppCoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AppCoreError::UnexpectedReceipt { command, received_receipt } => {
                write!(f, "unexpected receipt '{}' for command '{}'", received_receipt, command)
            }
            AppCoreError::UnexpectedErrorVariant { context, error } => {
                write!(f, "unexpected error variant in {}: {}", context, error)
            }
            AppCoreError::TileAllocationLogicError { stroke_session_id, reason } => {
                write!(f, "tile allocation logic error for stroke {}: {}", stroke_session_id, reason)
            }
            AppCoreError::MissingRendererNotice { receipt_id, notice_id } => {
                write!(f, "missing renderer notice for receipt {:?} notice {:?}", receipt_id, notice_id)
            }
            AppCoreError::Runtime(err) => write!(f, "runtime error: {:?}", err),
            AppCoreError::BrushEnqueue(err) => write!(f, "brush enqueue error: {:?}", err),
            AppCoreError::Merge(err) => write!(f, "merge error: {:?}", err),
            AppCoreError::Surface(err) => write!(f, "surface error: {:?}", err),
            AppCoreError::Resize { width, height, reason } => {
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
impl Send for AppCoreError {}
impl Sync for AppCoreError {}
```

### 2.2 Error Conversion Implementations

```rust
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
```

### 2.3 Error Classification Guidelines

| Error Category | When to Use | Example | Action |
|---------------|-------------|---------|--------|
| **Logic Bug** | Invariant violation, unreachable code, type mismatches | `UnexpectedReceipt` | `debug_assert!(false)` + return error |
| **Recoverable** | Expected failures, external dependency errors | `SurfaceError::Lost`, `BrushEnqueue` | Return error, caller handles |
| **Unrecoverable** | Fatal resource failure, cannot continue | `OutOfMemory`, `PresentFatal` | Panic or propagate to top-level |

---

## 3. Migration Plan

### Phase 1: Foundation (Non-Breaking)

**Step 1.1:** Add `AppCoreError` type to `app_core/mod.rs`
- Add enum definition
- Add `From` implementations
- No changes to existing method signatures yet

**Step 1.2:** Add helper methods for logic bug classification
```rust
impl AppCore {
    /// Assert and return logic bug error.
    fn logic_bug_unexpected_receipt(
        command: &'static str,
        received: &RuntimeReceipt,
    ) -> AppCoreError {
        debug_assert!(false, "unexpected receipt for {}", command);
        AppCoreError::UnexpectedReceipt {
            command,
            received_receipt: std::any::type_name_of_val(received),
        }
    }
}
```

### Phase 2: Method Conversions (Breaking, Incremental)

**Step 2.1:** Convert `resize()` - Lowest Risk
```rust
// Before
pub fn resize(&mut self, new_size: PhysicalSize<u32>) {
    self.runtime
        .execute(RuntimeCommand::Resize { ... })
        .unwrap_or_else(|err| panic!("resize command failed: {err:?}"));
}

// After
pub fn resize(&mut self, new_size: PhysicalSize<u32>) -> Result<(), AppCoreError> {
    self.runtime
        .execute(RuntimeCommand::Resize { 
            width, 
            height, 
            view_transform: self.view_transform.clone(),
        })
        .map_err(|err| AppCoreError::Resize {
            width,
            height,
            reason: err.to_string(),
        })?;
    Ok(())
}
```

**Step 2.2:** Convert `render()` - Medium Risk
```rust
// Before
pub fn render(&mut self) -> Result<(), wgpu::SurfaceError> {
    match self.runtime.execute(RuntimeCommand::PresentFrame { frame_id }) {
        Ok(RuntimeReceipt::FramePresented { .. }) => Ok(()),
        Ok(_) => panic!("unexpected receipt for PresentFrame command"),
        Err(RuntimeError::PresentError(e)) => match e {
            PresentError::Surface(err) => Err(err),
            PresentError::TileDrain(error) => {
                panic!("tile atlas drain failed during present: {error}")
            }
        },
        Err(RuntimeError::SurfaceError(err)) => Err(err),
        Err(other) => panic!("unexpected runtime error during render: {other:?}"),
    }
}

// After
pub fn render(&mut self) -> Result<(), AppCoreError> {
    match self.runtime.execute(RuntimeCommand::PresentFrame { frame_id }) {
        Ok(RuntimeReceipt::FramePresented { .. }) => Ok(()),
        Ok(receipt) => Err(Self::logic_bug_unexpected_receipt("PresentFrame", &receipt)),
        Err(RuntimeError::PresentError(PresentError::Surface(err))) => Err(err.into()),
        Err(RuntimeError::PresentError(PresentError::TileDrain(error))) => {
            Err(AppCoreError::PresentFatal { source: error })
        }
        Err(RuntimeError::SurfaceError(err)) => Err(err.into()),
        Err(other) => Err(AppCoreError::UnexpectedErrorVariant {
            context: "render",
            error: format!("{:?}", other),
        }),
    }
}
```

**Step 2.3:** Convert `enqueue_brush_render_command()` - Medium Risk
- Keep poisoned lock panics (thread safety violations)
- Convert tile allocation failure to logic bug error

**Step 2.4:** Convert `process_renderer_merge_completions()` - High Risk
- Most complex error handling
- Multiple error conversion points

### Phase 3: Caller Updates

**Step 3.1:** Update `GpuState` wrapper methods in `lib.rs`
**Step 3.2:** Update `main.rs` call sites
**Step 3.3:** Add error handling in `render_frame()` and `flush_brush_pipeline_lifecycle()`

---

## 4. Code Examples: Before and After

### 4.1 resize() Method

**Before:**
```rust
pub fn resize(&mut self, new_size: PhysicalSize<u32>) {
    let width = new_size.width.max(1);
    let height = new_size.height.max(1);
    
    let current_size = self.runtime.surface_size();
    if current_size.width == width && current_size.height == height {
        return;
    }
    
    self.runtime
        .execute(RuntimeCommand::Resize {
            width,
            height,
            view_transform: self.view_transform.clone(),
        })
        .unwrap_or_else(|err| panic!("resize command failed: {err:?}"));
}
```

**After:**
```rust
pub fn resize(&mut self, new_size: PhysicalSize<u32>) -> Result<(), AppCoreError> {
    let width = new_size.width.max(1);
    let height = new_size.height.max(1);
    
    let current_size = self.runtime.surface_size();
    if current_size.width == width && current_size.height == height {
        return Ok(());
    }
    
    self.runtime
        .execute(RuntimeCommand::Resize {
            width,
            height,
            view_transform: self.view_transform.clone(),
        })
        .map_err(|err| AppCoreError::Resize {
            width,
            height,
            reason: err.to_string(),
        })?;
    
    Ok(())
}
```

### 4.2 render() Method

**Before:**
```rust
pub fn render(&mut self) -> Result<(), wgpu::SurfaceError> {
    self.runtime.drain_view_ops();
    let frame_id = self.get_next_frame_id();
    
    match self.runtime.execute(RuntimeCommand::PresentFrame { frame_id }) {
        Ok(RuntimeReceipt::FramePresented { .. }) => Ok(()),
        Ok(_) => panic!("unexpected receipt for PresentFrame command"),
        Err(RuntimeError::PresentError(e)) => match e {
            PresentError::Surface(err) => Err(err),
            PresentError::TileDrain(error) => {
                panic!("tile atlas drain failed during present: {error}")
            }
        },
        Err(RuntimeError::SurfaceError(err)) => Err(err),
        Err(other) => panic!("unexpected runtime error during render: {other:?}"),
    }
}
```

**After:**
```rust
pub fn render(&mut self) -> Result<(), AppCoreError> {
    self.runtime.drain_view_ops();
    let frame_id = self.get_next_frame_id();
    
    match self.runtime.execute(RuntimeCommand::PresentFrame { frame_id }) {
        Ok(RuntimeReceipt::FramePresented { .. }) => Ok(()),
        Ok(receipt) => {
            debug_assert!(false, "unexpected receipt for PresentFrame command");
            Err(AppCoreError::UnexpectedReceipt {
                command: "PresentFrame",
                received_receipt: std::any::type_name_of_val(&receipt),
            })
        }
        Err(RuntimeError::PresentError(PresentError::Surface(err))) => {
            Err(AppCoreError::Surface(err))
        }
        Err(RuntimeError::PresentError(PresentError::TileDrain(error))) => {
            // Fatal: GPU resource failure
            Err(AppCoreError::PresentFatal { source: error })
        }
        Err(RuntimeError::SurfaceError(err)) => {
            Err(AppCoreError::Surface(err))
        }
        Err(other) => {
            debug_assert!(false, "unexpected runtime error during render");
            Err(AppCoreError::UnexpectedErrorVariant {
                context: "render",
                error: format!("{:?}", other),
            })
        }
    }
}
```

### 4.3 Caller Updates in main.rs

**Before:**
```rust
let render_result = gpu.render();
gpu.process_renderer_merge_completions(frame_sequence_id)
    .expect("process renderer merge completions");

match render_result {
    Ok(()) => {}
    Err(wgpu::SurfaceError::Outdated | wgpu::SurfaceError::Lost) => {
        gpu.resize(window.inner_size());
        window.request_redraw();
    }
    Err(wgpu::SurfaceError::Timeout) => {
        window.request_redraw();
    }
    Err(wgpu::SurfaceError::OutOfMemory) => {
        event_loop.exit();
    }
    Err(_) => {
        window.request_redraw();
    }
}
```

**After:**
```rust
let render_result = gpu.render();
if let Err(err) = gpu.process_renderer_merge_completions(frame_sequence_id) {
    // Log merge completion error - may be recoverable
    eprintln!("[error] merge completion failed: {:?}", err);
    // Continue rendering to allow recovery
}

match render_result {
    Ok(()) => {}
    Err(AppCoreError::Surface(wgpu::SurfaceError::Outdated | wgpu::SurfaceError::Lost)) => {
        // Attempt recovery by resizing
        if let Err(err) = gpu.resize(window.inner_size()) {
            eprintln!("[error] resize recovery failed: {:?}", err);
        }
        window.request_redraw();
    }
    Err(AppCoreError::Surface(wgpu::SurfaceError::Timeout)) => {
        window.request_redraw();
    }
    Err(AppCoreError::OutOfMemory | AppCoreError::PresentFatal { .. }) => {
        // Fatal errors - exit
        event_loop.exit();
    }
    Err(AppCoreError::UnexpectedReceipt { .. } | AppCoreError::UnexpectedErrorVariant { .. }) => {
        // Logic bugs - log and continue for debugging
        eprintln!("[error] render logic bug: {:?}", render_result);
        window.request_redraw();
    }
    Err(other) => {
        eprintln!("[error] render failed: {:?}", other);
        window.request_redraw();
    }
}
```

---

## 5. Caller Impact Analysis

### 5.1 GpuState Wrapper (lib.rs)

**Affected Methods:**
- `GpuState::resize()` - Add `Result` return type
- `GpuState::render()` - Change return type to `AppCoreError`
- `GpuState::process_renderer_merge_completions()` - Change error type

**Migration Strategy:**
```rust
// Temporary compatibility wrapper during migration
pub fn resize(&mut self, new_size: PhysicalSize<u32>) {
    self.core.resize(new_size)
        .expect("resize failed") // TODO: propagate error to caller
}
```

### 5.2 main.rs Call Sites

**Affected Locations:**
| Line | Method | Current Handling | Required Change |
|------|--------|------------------|-----------------|
| 569 | `gpu.resize()` | Direct call | Handle `Result` |
| 693 | `gpu.render()` | Match on `SurfaceError` | Match on `AppCoreError` |
| 700 | `process_renderer_merge_completions()` | `.expect()` | Handle errors gracefully |
| 812 | `gpu.render()` in flush | Match on `SurfaceError` | Match on `AppCoreError` |
| 820 | `process_renderer_merge_completions()` in flush | `.expect()` | Handle errors |

### 5.3 Backward Compatibility

**Strategy:**
1. Keep `MergeBridgeError` - still used by renderer direct calls
2. Add `AppCoreError` as unified type for AppCore public API
3. Use intermediate compatibility wrappers during migration
4. Update callers incrementally, one method at a time

---

## 6. Testing Strategy

### 6.1 Unit Tests

```rust
#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_app_core_error_display() {
        let err = AppCoreError::UnexpectedReceipt {
            command: "PresentFrame",
            received_receipt: "Resized",
        };
        assert!(err.to_string().contains("PresentFrame"));
        assert!(err.to_string().contains("Resized"));
    }
    
    #[test]
    fn test_app_core_error_is_send_sync() {
        fn assert_send<T: Send>() {}
        fn assert_sync<T: Sync>() {}
        assert_send::<AppCoreError>();
        assert_sync::<AppCoreError>();
    }
}
```

### 6.2 Integration Tests

- Test error propagation from `resize()` through `GpuState` to main
- Test recovery paths for `SurfaceError::Lost` and `SurfaceError::Outdated`
- Test logic bug detection with `debug_assert!` enabled

---

## 7. Implementation Checklist

### Phase 1 (Foundation)
- [ ] Add `AppCoreError` enum to `app_core/mod.rs`
- [ ] Implement `Display` and `Error` traits
- [ ] Add `Send + Sync` marker implementations
- [ ] Add `From` implementations for external errors
- [ ] Add helper methods for logic bug errors

### Phase 2 (Method Conversions)
- [ ] Convert `resize()` to return `Result<(), AppCoreError>`
- [ ] Convert `render()` to return `Result<(), AppCoreError>`
- [ ] Update `enqueue_brush_render_command()` error handling
- [ ] Update `process_renderer_merge_completions()` to use `AppCoreError`

### Phase 3 (Caller Updates)
- [ ] Update `GpuState::resize()` wrapper
- [ ] Update `GpuState::render()` wrapper
- [ ] Update `main.rs` render loop error handling
- [ ] Update `main.rs` flush lifecycle error handling
- [ ] Add error logging for graceful degradation

### Phase 4 (Cleanup)
- [ ] Remove temporary compatibility wrappers
- [ ] Review and remove any remaining unwrapped panics
- [ ] Add comprehensive error documentation
- [ ] Update coding guidelines with error handling patterns

---

## 8. Future Considerations

### 8.1 Threading Model
- Error types are `Send + Sync` ready for multi-threaded execution
- Consider adding error context for async stack traces

### 8.2 Error Reporting
- Add structured error logging (JSON format for telemetry)
- Consider error codes for categorization

### 8.3 Recovery Strategies
- Implement automatic recovery for `SurfaceError::Lost`
- Add fallback rendering paths for degraded modes

---

## 9. References

- [`coding-guidelines.md`](./coding-guidelines.md) - Error propagation guidelines
- `crates/renderer/AGENTS.md` - Renderer error handling patterns
- `crates/renderer/docs/merge_ack_integration.md` - Merge error flow
- Phase 2 Review Notes - Panic reduction requirements
