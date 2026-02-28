# Phase 4.3 Verification Report

**Date:** 2026-02-28
**Phase:** 04-03-appcore-migration
**Status:** GAP IDENTIFIED - Infrastructure complete, integration pending

---

## Executive Summary

Phase 4.3 planned to migrate AppCore to channel-based communication. Analysis reveals:

- **Infrastructure: COMPLETE** - Channels, EngineCore, engine_loop, EngineBridge all exist
- **Integration: NOT IMPLEMENTED** - AppCore still uses direct `runtime.execute()` calls
- **Tests: PASSING** - 31 glaphica tests, 48 renderer tests pass with `true_threading` feature

The gap is architectural: the plumbing exists but AppCore hasn't been connected to it.

---

## Must-Have Truths Assessment

| # | Must-Have Truth | Status | Evidence |
|---|-----------------|--------|----------|
| 1 | AppCore::render() sends RuntimeCommand::PresentFrame via channel | ❌ NOT IMPLEMENTED | `app_core/mod.rs:365` uses `runtime.execute()` directly |
| 2 | AppCore::resize() sends RuntimeCommand::Resize synchronously | ❌ NOT IMPLEMENTED | `app_core/mod.rs:338` uses `runtime.execute()` directly |
| 3 | AppCore::enqueue_brush_render_command() sends RuntimeCommand::EnqueueBrushCommands | ❌ NOT IMPLEMENTED | Multiple locations use `runtime.execute()` directly |
| 4 | EngineCore consumes GpuFeedbackFrame from channel | ✅ IMPLEMENTED | `engine_core.rs:233` drains `gpu_feedback_receiver` |
| 5 | Waterline tracking updated from feedback | ✅ IMPLEMENTED | `engine_core.rs:108-110` updates waterlines with max merge |

---

## Architecture Analysis

### Current State

```
┌─────────────────────────────────────────────────────────────────┐
│                      SINGLE-THREADED MODE                        │
│  (Current default, working)                                      │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│   GpuState                                                       │
│   ├── AppCore ──────────────► GpuRuntime.execute() ───► GPU     │
│   │   (business logic)        (direct call)                     │
│   │                                                              │
│   └── exec_mode = SingleThread { runtime }                       │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────────┐
│                       THREADED MODE                              │
│  (Infrastructure exists, but AppCore NOT integrated)             │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│   Main Thread                    Engine Thread                   │
│   ────────────                   ──────────────                  │
│   GpuState                       (EngineCore exists              │
│   └── EngineBridge ◄──────────►   but NOT connected             │
│       ├── GpuRuntime              to AppCore)                    │
│       └── channels                                               │
│                                                                  │
│   Problem: AppCore is NOT in engine thread!                      │
│   AppCore still lives in GpuState and makes direct calls.        │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
```

### Required State (Per Plan)

```
┌─────────────────────────────────────────────────────────────────┐
│                       THREADED MODE (Target)                     │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│   Main Thread                    Engine Thread                   │
│   ────────────                   ──────────────                  │
│   EngineBridge ◄────────────►   EngineCore                       │
│   ├── GpuRuntime                ├── AppCore (moved here!)        │
│   └── channels                  ├── channels                     │
│                                 └── engine_loop                   │
│                                                                  │
│   Flow:                                                          │
│   1. AppCore::render() sends PresentFrame via channel            │
│   2. Main thread receives, GpuRuntime.execute()                  │
│   3. Feedback flows back to EngineCore                           │
│   4. Waterlines track progress                                   │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
```

---

## Code Evidence

### AppCore Direct Calls (NOT using channels)

```rust
// app_core/mod.rs:365 - render()
match runtime.execute(RuntimeCommand::PresentFrame { frame_id }) {
    // Should be: self.gpu_command_sender.push(GpuCmdMsg::Command(...))
}

// app_core/mod.rs:338 - resize()
.execute(RuntimeCommand::Resize {
    // Should be: self.gpu_command_sender.push(GpuCmdMsg::Command(...))
})

// app_core/mod.rs:408, 457, 503, 518 - brush commands
.execute(RuntimeCommand::EnqueueBrushCommand { ... })
// Should batch and send via channel
```

### EngineCore Channel Usage (IMPLEMENTED)

```rust
// engine_core.rs:220 - sends commands
gpu_command_sender.push(protocol::GpuCmdMsg::Command(cmd))

// engine_core.rs:233 - receives feedback
while let Ok(frame) = channels.gpu_feedback_receiver.pop() {
    pending_feedback = Some(...);
}

// engine_core.rs:246 - processes feedback
core.process_feedback(frame);
```

---

## What Was Completed (Phases 4.1 & 4.2)

### Phase 4.1: Channel Infrastructure ✅
- `engine::create_thread_channels()` - SPSC queue factory
- `EngineThreadChannels<Command, Receipt, Error>` - channel bundle
- `GpuCmdMsg` and `GpuFeedbackFrame` message types
- `RuntimeCommand`, `RuntimeReceipt`, `RuntimeError` enums

### Phase 4.2: Runtime Thread Loop ✅
- `EngineCore` struct with waterlines and feedback processing
- `engine_loop()` function for engine thread
- `GpuRuntimeThread` with shutdown signaling
- `run_runtime_loop()` for main thread
- Mailbox merge for feedback frames
- Waterline monotonicity enforcement

---

## Gap Closure Requirements

To complete Phase 4.3, the following changes are needed:

### 1. Add Channel Fields to AppCore
```rust
pub struct AppCore {
    // ... existing fields ...
    
    // Phase 4.3: Channel communication
    pub gpu_command_sender: GpuCommandSender<RuntimeCommand>,
    pub gpu_feedback_receiver: GpuFeedbackReceiver<RuntimeReceipt, RuntimeError>,
}
```

### 2. Refactor AppCore Methods
- `render()` → send `PresentFrame` via channel, wait for feedback
- `resize()` → send `Resize` via channel with handshake pattern
- Brush operations → batch and send via `EnqueueBrushCommands`
- Merge operations → use `ProcessMergeCompletions` command

### 3. Move AppCore to Engine Thread
- `GpuState::into_threaded()` should create `EngineCore` from `AppCore`
- Engine thread runs `engine_loop(engine_core, channels, ...)`
- Main thread only holds `EngineBridge`

### 4. Update GpuState Threading Mode
```rust
pub fn into_threaded<F>(self, spawn_engine: F) -> Self {
    // Create EngineCore from AppCore components
    let engine_core = EngineCore::new(
        self.core.document,  // Move document
        self.core.tile_merge_engine,
        // ... other components
    );
    
    // Spawn engine thread with engine_loop
    // Main thread keeps EngineBridge with GpuRuntime
}
```

---

## Impact Assessment

### Risk: Low
- Infrastructure is solid and tested
- Single-threaded mode continues to work
- Changes are additive (channels) before being replacement

### Effort: Medium-High
- ~10-15 file modifications
- AppCore refactoring touches most methods
- Integration testing required

### Dependencies
- Phase 4.1 ✅
- Phase 4.2 ✅
- No blockers

---

## Recommendations

1. **Update Plan** - Mark Phase 4.3 as requiring new implementation approach
2. **Split into Smaller Plans** - Consider breaking into:
   - 4.3a: Add channel fields to AppCore
   - 4.3b: Migrate render/resize paths
   - 4.3c: Migrate brush/merge paths
   - 4.3d: Integration testing

3. **Alternative Approach** - Consider if full migration is needed now, or if single-threaded mode suffices for current use cases

---

## Test Verification

```
$ cargo test -p glaphica --features true_threading
test result: ok. 31 passed; 0 failed; 0 ignored

$ cargo test -p renderer --features true_threading  
test result: ok. 48 passed; 0 failed; 2 ignored

All infrastructure tests pass. Gap is in AppCore integration only.
```

---

*Generated: 2026-02-28 by GSD Execute-Phase Orchestrator*