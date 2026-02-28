# Requirements: Glaphica Phase 4 Refactoring

**Defined:** 2026-02-28
**Last Updated:** 2026-02-28 — Mapped to sub-phases 4.1–4.4
**Core Value:** Achieve clean separation between business logic (AppCore) and GPU execution (GpuRuntime) through message-passing channels, enabling future parallelism while maintaining correctness.

## v1 Requirements

### Channel Infrastructure (Phase 4.1)

- [ ] **CHAN-01**: Add `engine` and `protocol` dependencies to `crates/glaphica/Cargo.toml`
- [ ] **CHAN-02**: Define `RuntimeReceipt` enum with variants for each command response
- [ ] **CHAN-03**: Define `RuntimeError` enum with error types for each failure mode
- [ ] **CHAN-04**: Instantiate channels in `GpuState::new()` using `engine::create_thread_channels()`
- [ ] **CHAN-05**: Add feature flag `true_threading` to switch between single-threaded and multi-threaded mode

### Runtime Thread Loop (Phase 4.2)

- [ ] **LOOP-01**: Create runtime thread spawn function in `GpuRuntime`
- [ ] **LOOP-02**: Implement command consumption loop that reads from `gpu_command_receiver`
- [ ] **LOOP-03**: Implement feedback production that writes to `gpu_feedback_sender`
- [ ] **LOOP-04**: Handle `GpuCmdMsg::Command(RuntimeCommand)` variants
- [ ] **LOOP-05**: Implement graceful shutdown mechanism

### AppCore Command Integration (Phase 4.3)

- [ ] **CMD-01**: Migrate `AppCore::render()` to send `RuntimeCommand::PresentFrame` via channel
- [ ] **CMD-02**: Migrate `AppCore::resize()` to send `RuntimeCommand::Resize` via channel
- [ ] **CMD-03**: Migrate `AppCore::enqueue_brush_render_command()` to send `RuntimeCommand::EnqueueBrushCommands`
- [ ] **CMD-04**: Migrate merge polling to send `RuntimeCommand::PollMergeNotices`
- [ ] **CMD-05**: Implement feedback processing to consume `RuntimeReceipt` from channel

### Tile Lifetime Safety (Phase 4.4)

- [ ] **SAFE-01**: Ensure `completion_waterline` is checked before tile release
- [ ] **SAFE-02**: Verify generation-based ABA prevention in `resolve()` operations
- [ ] **SAFE-03**: Add debug assertions for lock lifetime (no cross-command holding)
- [ ] **SAFE-04**: Validate monotonically increasing `frame_id` / `submission_id`

### Testing & Validation (Phase 4.1, 4.3, 4.4)

- [ ] **TEST-01**: All 47 renderer tests pass in single-threaded mode (Phase 4.1)
- [ ] **TEST-02**: All 47 renderer tests pass in true-threaded mode (Phase 4.3)
- [ ] **TEST-03**: Add stress test for concurrent command/feedback (Phase 4.4)
- [ ] **TEST-04**: Verify no deadlocks under sustained load (Phase 4.4)

## v2 Requirements

### Performance Optimization

- **PERF-01**: Benchmark single-threaded vs true-threaded mode
- **PERF-02**: Tune channel capacities based on profiling data
- **PERF-03**: Optimize feedback merge frequency

### Enhanced Error Handling

- **ERR-01**: AppCore panic → Result error handling
- **ERR-02**: Graceful degradation on GPU errors
- **ERR-03**: Error telemetry and logging

## Out of Scope

| Feature | Reason |
|---------|--------|
| TileKey encoding integration | Phase 1 draft code, not required for threading correctness |
| Performance optimization | Correctness first, optimize in Phase 5+ |
| AppCore panic → Result | Separate PR, too large for Phase 4 |
| Multi-GPU support | Future enhancement |

## Traceability

### Phase 4.1: Channel Infrastructure
| Requirement | Status | Plans |
|-------------|--------|-------|
| CHAN-01 | Pending | 04-01 |
| CHAN-02 | Pending | 04-01 |
| CHAN-03 | Pending | 04-01 |
| CHAN-04 | Pending | 04-02 |
| CHAN-05 | Pending | 04-02 |
| TEST-01 | Pending | 04-01/04-02 |

### Phase 4.2: Runtime Thread Loop
| Requirement | Status |
|-------------|--------|
| LOOP-01 | Pending |
| LOOP-02 | Pending |
| LOOP-03 | Pending |
| LOOP-04 | Pending |
| LOOP-05 | Pending |

### Phase 4.3: AppCore Migration
| Requirement | Status |
|-------------|--------|
| CMD-01 | Pending |
| CMD-02 | Pending |
| CMD-03 | Pending |
| CMD-04 | Pending |
| CMD-05 | Pending |
| TEST-02 | Pending |

### Phase 4.4: Safety & Validation
| Requirement | Status |
|-------------|--------|
| SAFE-01 | Pending |
| SAFE-02 | Pending |
| SAFE-03 | Pending |
| SAFE-04 | Pending |
| TEST-03 | Pending |
| TEST-04 | Pending |

**Coverage:**
- v1 requirements: 23 total
- Mapped to sub-phases: 23
- Unmapped: 0 ✓

---

*Requirements defined: 2026-02-28*
*Last updated: 2026-02-28 — Mapped to sub-phases 4.1–4.4*
