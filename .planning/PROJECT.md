# Glaphica Phase 4: Two-Thread Architecture

## What This Is

Phase 4 of the Tiles/Model/Runtime refactoring initiative for glaphica, a GPU-accelerated drawing application. This phase integrates the `engine + protocol` channel system to decouple the main app from the GPU runtime using a two-thread architecture.

## Core Value

Achieve clean separation between business logic (AppCore on engine thread) and GPU execution (GpuRuntime on main thread) through message-passing channels, maintaining correctness while enabling efficient parallel execution.

**Architecture:**
- **Main thread**: Runs `GpuRuntime` (GPU operations) - must remain lightweight
- **Engine thread**: Runs engine loop (AppCore, command processing, feedback production)

## Requirements

### Validated

- ✓ Phase 1: Model统一 - TILE_* constants consolidated to `model` crate
- ✓ Phase 2: GpuState拆分 - AppCore + GpuRuntime architecture established
- ✓ Phase 2.5: GpuState integration - facade delegation complete
- ✓ Phase 3: Code cleanup - hybrid paths migrated, dead code removed, runtime commands implemented

### Active

- [ ] **PH4-01**: Create thread channels using `engine::create_thread_channels<RuntimeCommand, RuntimeReceipt, RuntimeError>`
- [ ] **PH4-02**: Implement runtime execution loop that consumes commands and produces feedback (runs on main thread)
- [ ] **PH4-03**: Migrate AppCore to send commands via channel instead of direct calls
- [ ] **PH4-04**: Integrate `GpuFeedbackFrame` waterline with receipts/errors
- [ ] **PH4-05**: Ensure tile lifetime safety with completion notices and generation-based ABA prevention
- [ ] **PH4-06**: Add feature flag for single-threaded vs two-thread mode (rollback support)
- [ ] **PH4-07**: Verify all tests pass in two-thread mode
- [ ] **PH4-08**: Update documentation (debug_playbook.md, architecture docs)

### Out of Scope

- AppCore panic → Result error handling (separate PR)
- TileKey encoding integration (Phase 1 draft code, not required for threading)
- Performance optimization (correctness first, optimize later)

## Context

**Current Architecture (Phase 3 complete)**:
- `GpuState` (facade) → `AppCore` (business logic) → `GpuRuntime` (GPU execution)
- Communication via `RuntimeCommand` enum (PresentFrame, Resize, EnqueueBrushCommands, etc.)
- Single-threaded dispatcher (direct function calls, not true channels)
- 12 RuntimeCommand variants implemented, 0 TODOs remaining

**Phase 4 Goal**:
- Replace direct dispatcher with `engine + protocol` channels
- Engine thread (AppCore) sends commands via channel
- Main thread (GpuRuntime) executes commands and returns receipts
- Maintain tile lifetime invariants (completion notice → ack → finalize/abort)
- Feature flag for rollback to single-threaded mode
- Keep GpuRuntime lightweight for performance

**Key Invariants (from refactor guide §14.5)**:
1. Tile release不得早于 GPU work completion
2. Generation-based ABA prevention for slot reuse
3. Locks must not cross command boundaries
4. Token (frame_id/submission_id) must be monotonically increasing

## Constraints

- **Correctness**: Must pass all existing tests (47 renderer tests, tiles tests)
- **Safety**: Tile lifetime invariants must be preserved (no use-after-free)
- **Rollback**: Feature flag to switch back to single-threaded mode
- **Incremental**: Can run single-threaded first, then enable two-thread mode via feature
- **Performance**: GpuRuntime must remain lightweight (runs on main thread)

## Key Decisions

| Decision | Rationale | Outcome |
|----------|-----------|---------|
| Use `engine::create_thread_channels` | Reuses existing protocol primitives | — Pending |
| Feature flag for two-thread mode | Enables safe rollback and comparison | — Pending |
| GpuRuntime runs on main thread | wgpu requires GPU operations on main thread, keeps architecture simple | — Clarified |
| TileKey encoding draft not required | Phase 4 focuses on threading, not key scheme | — Pending |

---
*Last updated: 2026-02-28 after Phase 3 completion*
