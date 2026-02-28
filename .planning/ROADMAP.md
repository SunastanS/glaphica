# Roadmap: Glaphica Phase 4 Refactoring

**Project:** Glaphica Tiles/Model/Runtime Refactoring
**Phase 4 Goal:** Integrate `engine + protocol` channels for true multi-threaded execution
**Created:** 2026-02-28
**Last Updated:** 2026-02-28 — Phase 4.1 COMPLETE

## Overview

**Phase 4** with **4 sub-phases** | **23 requirements mapped** | All v1 requirements covered ✓

| # | Sub-Phase | Goal | Requirements | Success Criteria |
|---|-----------|------|--------------|------------------|
| 4.1 | Channel Infrastructure | Set up channel primitives and types | CHAN-01..05, TEST-01 | Channels instantiated, feature flag works |
| 4.2 | Runtime Thread Loop | Implement command consumer and feedback producer | LOOP-01..05 | Runtime thread processes all commands |
| 4.3 | AppCore Migration | Migrate AppCore to channel-based communication | CMD-01..05, TEST-02 | All commands sent via channel, feedback consumed |
| 4.4 | Safety & Validation | Ensure tile lifetime safety and run tests | SAFE-01..04, TEST-03..04 | All tests pass, no deadlocks, invariants hold |

---

## Phase Details

### Phase 4.1: Channel Infrastructure

**Goal:** Establish channel infrastructure with proper types and feature flags

**Plans:** 2 plans

Plans:
- [x] 04-01-PLAN.md — Add dependencies and define RuntimeReceipt/RuntimeError types
- [x] 04-02-PLAN.md — Instantiate channels in GpuState::new()

**Requirements:**
- [x] CHAN-01: Add engine/protocol dependencies
- [x] CHAN-02: Define RuntimeReceipt enum
- [x] CHAN-03: Define RuntimeError enum  
- [x] CHAN-04: Instantiate channels in GpuState::new()
- [x] CHAN-05: Add true_threading feature flag

**Success Criteria:**
1. ✓ `Cargo.toml` includes `engine` and `protocol` dependencies
2. ✓ `RuntimeReceipt` enum has variants matching each command type
3. ✓ `RuntimeError` enum covers all failure modes
4. ✓ Channels created with appropriate capacities (input_ring=1024, input_control=256, gpu_command=1024, gpu_feedback=256)
5. ✓ Feature flag `true_threading` gates threaded vs single-threaded mode
6. ✓ Code compiles with both feature configurations

**Implementation Notes:**
- Channel capacities: start conservative (e.g., 1024 for commands, 256 for feedback)
- Use `engine::create_thread_channels::<RuntimeCommand, RuntimeReceipt, RuntimeError>()`
- Feature flag in `crates/glaphica/Cargo.toml` and conditional compilation in `mod.rs`

**Phase 4.1 Status: COMPLETE (2026-02-28)**
- All 5 requirements implemented (CHAN-01 through CHAN-05)
- 2 plans completed with SUMMARY.md created
- Channel infrastructure ready for Phase 4.2

---

### Phase 4.2: Runtime Thread Loop

**Goal:** Implement runtime thread that consumes commands and produces feedback

**Requirements:**
- LOOP-01: Create runtime thread spawn function
- LOOP-02: Implement command consumption loop
- LOOP-03: Implement feedback production
- LOOP-04: Handle all RuntimeCommand variants
- LOOP-05: Implement graceful shutdown

**Success Criteria:**
1. `GpuRuntime::spawn_runtime_thread()` creates dedicated thread
2. Command loop uses `gpu_command_receiver.pop()` or drain batch
3. Feedback sent via `gpu_feedback_sender.push()`
4. All 12 RuntimeCommand variants handled (PresentFrame, Resize, EnqueueBrushCommands, PollMergeNotices, AckMergeResults, BindRenderTree, etc.)
5. Shutdown mechanism via InputControlEvent or channel close
6. Runtime thread panics are caught and reported

**Implementation Notes:**
- Thread loop structure: `loop { match command_receiver.pop() { ... } }`
- Consider using `recv_timeout()` for shutdown detection
- Feedback frame waterline management (submit, executed_batch, complete)
- Each command execution should produce corresponding receipt or error

---

### Phase 4.3: AppCore Migration

**Goal:** Migrate AppCore to send commands via channel and consume feedback

**Requirements:**
- CMD-01: Migrate render()/present() path
- CMD-02: Migrate resize() path
- CMD-03: Migrate brush enqueue path
- CMD-04: Migrate merge polling path
- CMD-05: Implement feedback processing
- TEST-02: All tests pass in true-threaded mode

**Success Criteria:**
1. `AppCore::render()` sends `RuntimeCommand::PresentFrame` and waits for feedback
2. `AppCore::resize()` sends `RuntimeCommand::Resize` synchronously
3. `AppCore::enqueue_brush_render_command()` sends `RuntimeCommand::EnqueueBrushCommands`
4. Merge polling uses `RuntimeCommand::PollMergeNotices` with feedback consumption
5. Feedback channel consumed each frame: `gpu_feedback_receiver.pop()` or drain
6. Waterline tracking updated from feedback (submit, executed_batch, complete)
7. All 47 renderer tests pass in true-threaded mode

**Implementation Notes:**
- Synchronous commands (resize) vs async commands (render, brush)
- Feedback processing in main loop: drain feedback queue before next frame
- Maintain existing behavior: no functional changes, only communication mechanism changes
- Feature flag allows fallback to single-threaded dispatcher

---

### Phase 4.4: Safety & Validation

**Goal:** Ensure tile lifetime safety invariants and comprehensive testing

**Requirements:**
- SAFE-01: completion_waterline check before tile release
- SAFE-02: Generation-based ABA prevention
- SAFE-03: Lock lifetime assertions
- SAFE-04: Monotonically increasing frame_id validation
- TEST-03: Stress test for concurrent command/feedback
- TEST-04: Deadlock verification

**Success Criteria:**
1. Tile release checks `complete_waterline >= submission_token`
2. `resolve()` operations validate generation matches expected value
3. Debug assertions verify locks not held across command boundaries
4. Frame ID monotonically increasing (debug_assert in render path)
5. Stress test runs 1000+ frames without deadlock or panic
6. All tests pass: renderer (47), tiles, document, integration
7. No new dead_code warnings introduced

**Implementation Notes:**
- Use `submission_id` or `frame_id` as token for completion tracking
- Generation validation in `TileAtlasStore::resolve()`
- Lock guard scope should not cross `.pop()` / `.push()` boundaries
- Stress test: rapid resize + render + brush commands concurrently

---

### Phase 4.5: Cleanup (Future)

**Goal:** Remove feature flag, optimize, and finalize documentation

**Not in current scope** - will be planned after Phase 4.4 completion.

**Expected Requirements:**
- Remove `true_threading` feature flag (make default)
- Remove single-threaded dispatcher code
- Performance tuning based on profiling
- Update debug_playbook.md with threading architecture
- Document tile lifetime invariants in architecture docs

---

## Risk Mitigation

| Risk | Mitigation |
|------|------------|
| Tile use-after-free | completion_waterline checks, generation validation |
| Deadlock | Lock lifetime assertions, stress testing |
| Performance regression | Feature flag for rollback, benchmarking |
| Feedback lost | Waterline-based tracking, non-lossy receipts |
| Shutdown race | Graceful shutdown via control channel |

---

## Dependencies

**Phase 4.1** → No dependencies (infrastructure only)

**Phase 4.2** → Depends on Phase 4.1 (channel types)

**Phase 4.3** → Depends on Phase 4.2 (runtime loop ready)

**Phase 4.4** → Depends on Phase 4.3 (migration complete, testing possible)

---

*Roadmap created: 2026-02-28*
*Restructured: 2026-02-28 — Phase 4 sub-phases (4.1–4.4)*
*Updated: 2026-02-28 — Plan 04-01 complete (CHAN-01, CHAN-02, CHAN-03, CHAN-05)*
