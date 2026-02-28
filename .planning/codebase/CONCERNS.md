# Codebase Concerns

**Analysis Date:** 2026-02-28

## Tech Debt

### Phase 4 Thread Migration Incomplete

**Issue:** The codebase is mid-migration from single-threaded to true cross-thread architecture. `EngineCore` exists but full threading is not wired.

**Files:** 
- `crates/glaphica/src/lib.rs` (1660 lines)
- `crates/glaphica/src/engine_core.rs` (511 lines)
- `crates/glaphica/src/engine_bridge.rs` (598 lines)
- `crates/glaphica/src/app_core/mod.rs` (1318 lines)

**Evidence:**
- `lib.rs:1175`: `panic!("process_renderer_merge_completions not yet implemented for threaded mode")`
- `lib.rs:997`: `// Phase 4 TODO: We shouldn't need runtime in bridge for threaded mode`
- `lib.rs:1016`: `// Phase 4 TODO: Drain feedback and wait for waterline to advance`
- `engine_core.rs:96-176`: Multiple `// TODO` placeholders for brush session logic, receipt handling, GC

**Impact:** 
- Cannot safely merge concurrent changes to threading model
- Current state blocks full Phase 4 completion
- Risk of deadlocks or race conditions during partial migration

**Fix approach:** 
- Complete Phase 4 implementation plan (`docs/Instructions/phase4_implementation_plan.md`)
- Finish wiring `EngineBridge` dispatcher loop on main thread
- Implement full feedback processing with mailbox merge
- Remove `AppCore` direct `GpuRuntime` coupling

---

### Excessive Panic Usage in Production Paths

**Issue:** Heavy use of `panic!()` and `unwrap_or_else(|_| panic!(...))` patterns throughout core modules, particularly for lock poisoning scenarios.

**Files:**
- `crates/glaphica/src/lib.rs`: ~30+ panic sites (lines 58, 69, 87, 101, 113, 145, 169, 183, 195, 225, 239, 249, 263, 275, 313, 320, 412, 425, 434, 548, 586, 590, 596, 609, 622, 631, 688, 699, 706, 886, 1067, 1075, 1082, 1095, 1133, 1175, 1220, 1227, 1239, 1246, 1268)
- `crates/glaphica/src/app_core/mod.rs`: ~25+ panic sites (lines 413, 428, 436, 446, 462, 476, 508, 523, 547, 556, 687, 691, 710, 727, 784, 793, 800, 821, 913, 938, 977, 1022, 1069, 1073, 1079, 1093, 1106, 1115)
- `crates/glaphica/src/main.rs`: ~20+ panic sites (lines 222, 225, 254, 284, 607, 609, 1160, 1311, 1320, 1329, 1338, 1347, 1354, 1364)

**Pattern:**
```rust
.unwrap_or_else(|_| panic!("document read lock poisoned"))
.unwrap_or_else(|_| panic!("brush buffer tile key registry write lock poisoned"))
```

**Impact:**
- Any thread panic on lock operations crashes entire application
- No graceful degradation or error recovery path
- Makes testing failure scenarios difficult

**Fix approach:**
- Replace lock poisoning panics with proper `Result` propagation where feasible
- Consider using `poison` handling to recover from poisoned locks
- Reserve panics for truly unrecoverable invariants only

---

### Tiles Crate GPU/CPU Coupling

**Issue:** `crates/tiles` mixes pure logic (tile allocation, addressing) with GPU-specific code (wgpu textures, merge submission), preventing independent testing and reuse.

**Files:**
- `crates/tiles/src/lib.rs` (885 lines)
- `crates/tiles/src/atlas/core.rs` (1080 lines)
- `crates/tiles/src/atlas/gpu.rs`
- `crates/tiles/src/atlas/format_gpu.rs` (479 lines)
- `crates/tiles/src/merge_submission.rs` (1766 lines)

**Impact:**
- Cannot test tile allocation logic without GPU context
- Blocks pure-CPU unit tests for core tile semantics
- Increases compile times (wgpu dependency pulls in entire GPU stack)

**Fix approach:**
- Execute planned split from `crates/tiles/docs/TODO.md`:
  - Create `tiles_core` (pure logic, no wgpu)
  - Create `tiles_gpu` (GPU runtime, depends on tiles_core)
  - Keep `tiles` as facade crate
- Define clear trait boundaries: `TileAtlasBackend`, `TileAtlasGpu`

---

### Large Monolithic Files

**Issue:** Several files exceed 1000+ lines, making them difficult to review and maintain.

**Files:**
- `crates/renderer/src/tests.rs`: 2527 lines
- `crates/document/src/lib.rs`: 1780 lines
- `crates/tiles/src/merge_submission.rs`: 1766 lines
- `crates/glaphica/src/lib.rs`: 1660 lines
- `crates/glaphica/src/app_core/mod.rs`: 1318 lines
- `crates/renderer/src/renderer_frame.rs`: 1580 lines
- `crates/renderer/src/renderer_merge.rs`: 1514 lines
- `crates/glaphica/src/main.rs`: 1387 lines

**Impact:**
- High cognitive load for modifications
- Difficult to isolate concerns
- Merge conflicts more likely

**Fix approach:**
- Extract logical modules from `app_core/mod.rs` (brush sessions, merge orchestration, GC)
- Split `merge_submission.rs` into smaller, testable units
- Consider feature-based module organization

---

## Known Bugs

### Threaded Mode Merge Completions Unimplemented

**Issue:** `process_renderer_merge_completions` explicitly panics in threaded mode.

**Files:** `crates/glaphica/src/lib.rs:1175`

**Symptoms:** 
- Application will panic if merge completions are processed in threaded execution mode
- Currently blocks full Phase 4 rollout

**Trigger:** Call `process_renderer_merge_completions()` when `GpuExecMode::Threaded`

**Workaround:** Use single-threaded mode until Phase 4 is complete

---

### Brush Session Logic Not Implemented

**Issue:** `EngineCore::process_input_sample()` is a stub with no actual brush session handling.

**Files:** `crates/glaphica/src/engine_core.rs:96-99`

**Evidence:**
```rust
pub fn process_input_sample(&mut self, sample: &InputRingSample) {
    // TODO: Implement brush session logic
    // For now, this is a placeholder
    let _ = sample;
}
```

**Impact:** Input samples are silently ignored in engine thread mode

---

## Security Considerations

### Environment Variable Secret Exposure Risk

**Risk:** `.env` files or environment variables may contain sensitive configuration (API keys, credentials).

**Files:** Environment configuration (existence confirmed, contents not inspected)

**Current mitigation:** None detected - no `.gitignore` review performed

**Recommendations:**
- Ensure `.env` is in `.gitignore`
- Document required environment variables in `README.md` or `.env.example`
- Add secret scanning to CI pipeline

---

### Unsafe Code Usage

**Risk:** Potential unsafe blocks in GPU/wgpu integration layer.

**Files:** Requires deeper inspection of `crates/renderer/` and `crates/tiles/src/atlas/gpu.rs`

**Current mitigation:** Fail-fast design with debug assertions

**Recommendations:**
- Audit all `unsafe` blocks
- Add safety invariants as comments
- Consider adding `unsafe` code review checklist

---

## Performance Bottlenecks

### Lock Contention on Shared State

**Problem:** Heavy use of `RwLock` with frequent write operations creates contention points.

**Files:**
- `crates/glaphica/src/lib.rs`: Multiple `RwLock<Document>`, `RwLock<BrushBufferTileRegistry>`
- `crates/glaphica/src/app_core/mod.rs`: Same patterns

**Cause:**
- Write locks held during complex operations
- No lock-free data structures for hot paths
- Potential for priority inversion in threaded mode

**Improvement path:**
- Profile lock contention with `parking_lot` features
- Consider actor model for document state (already partially implemented with `EngineCore`)
- Use finer-grained locking or lock-free queues where possible

---

### GPU Command Batching Efficiency

**Problem:** No explicit batching strategy detected for GPU command submission.

**Files:** `crates/glaphica/src/engine_bridge.rs`, `crates/renderer/src/lib.rs`

**Cause:** Commands may be submitted individually rather than batched, increasing CPU overhead

**Improvement path:**
- Implement command batching with configurable budget
- Use `rtrb` ring buffer semantics for efficient batching (already in `engine` crate)
- Profile submit frequency vs. frame time

---

### Test Execution Stability Issues

**Problem:** GPU-backed tests can be unstable in parallel execution.

**Files:** `crates/renderer/src/tests.rs`, `crates/tiles/src/tests.rs`

**Evidence:** From `crates/renderer/DESIGN.md:109-110`:
```
Recommended local check for CI-like stability:
- Run tests single-threaded: `cargo test -- --test-threads=1`.
```

**Impact:** 
- CI may experience flaky test failures
- Slower test execution (single-threaded)

**Improvement path:**
- Mark GPU tests with `#[ignore]` and document run command
- Add test isolation for GPU resource cleanup
- Consider headless GPU testing infrastructure

---

## Fragile Areas

### Render Tree Revision Semantics

**Files:** `crates/renderer/src/renderer_frame.rs`, `crates/renderer/src/renderer_cache_draw.rs`

**Why fragile:** 
- Revision bumps must be perfectly synchronized with semantic changes
- Off-by-one errors cause silent rendering bugs or unnecessary redraws
- Complex interaction between dirty tracking and cache invalidation

**Safe modification:**
- Always run with `GLAPHICA_RENDER_TREE_TRACE=1 GLAPHICA_RENDER_TREE_INVARIANTS=1`
- Add assertion: semantic changes without revision bump should panic
- Follow debug playbook: `docs/Instructions/debug_playbook.md`

**Test coverage:** Partial - renderer tests exist but may not cover all revision scenarios

---

### Waterline Monotonicity Invariants

**Files:** 
- `crates/protocol/src/lib.rs` (feedback frame merge)
- `crates/glaphica/src/engine_core.rs:106-128`

**Why fragile:**
- Mailbox merge relies on monotonic waterline progression
- Non-monotonic updates break "max-merge" semantics
- Silent data loss if receipts/errors are dropped

**Safe modification:**
- Keep debug assertions for monotonicity (already present)
- Never drop feedback frames without merging
- Test out-of-order frame delivery explicitly

**Test coverage:** Debug-only assertions present

---

### wgpu CPU→GPU Ordering

**Files:** `crates/renderer/`, `crates/tiles/src/atlas/gpu.rs`

**Why fragile:**
- `queue.write_buffer()` semantics are subtle
- Multiple writes to same buffer range before `submit()` can cause race conditions
- Platform-specific behavior (macOS/Metal has stricter main-thread requirements)

**Safe modification:**
- Follow `docs/Instructions/wgpu.md` strictly
- Prefer safe (slower) ordering first, optimize later
- Add invariants: single writer per buffer range per submit

**Test coverage:** Partial - WGSL parse tests in `wgsl_tests.rs`

---

### Thread Channel Endpoint Ownership

**Files:** `crates/engine/src/lib.rs`, `crates/glaphica/src/engine_bridge.rs`

**Why fragile:**
- `rtrb` SPSC endpoints are `Send` but not `Sync`
- Accidental `Clone` or shared reference could cause data races
- Drop semantics must be explicit for clean shutdown

**Safe modification:**
- Keep channel endpoints private within bridge structs
- Never expose raw `Sender`/`Receiver` outside bridge
- Follow `EngineThread` / `EngineBridge` ownership pattern

**Test coverage:** Limited - threaded tests in `phase4_threaded_tests.rs`

---

## Scaling Limits

### Feedback Queue Capacity

**Current capacity:** 64 frames (from `phase4_implementation_plan.md`)

**Limit:** Queue overflow would drop correctness-critical receipts/errors

**Scaling path:**
- Monitor queue depth in production
- Add backpressure mechanism if consistently near capacity
- Consider dynamic sizing based on frame rate

---

### Tile Atlas Memory Growth

**Current capacity:** Not explicitly bounded

**Limit:** Long-running sessions with heavy brush usage could exhaust GPU memory

**Scaling path:**
- Implement GC based on waterline (planned in `EngineCore::gc_evict_before_waterline`)
- Add memory pressure monitoring
- Consider LRU eviction policy for tile cache

---

## Dependencies at Risk

### wgpu Version Lock

**Risk:** wgpu 28.0.0 (from `crates/tiles/Cargo.toml`) may have breaking changes in future versions

**Impact:** 
- Upgrade path may require significant WGSL/backend changes
- Platform-specific bugs may be version-dependent

**Migration plan:**
- Pin to specific wgpu version
- Abstract wgpu usage behind traits where possible
- Monitor wgpu release notes for breaking changes

---

### Crossbeam Channel Dependencies

**Risk:** `engine` crate uses `crossbeam` for channels; version mismatch could cause issues

**Impact:** Limited - channel types are internal to `engine` crate

**Mitigation:** Workspace-level dependency management

---

## Test Coverage Gaps

### Threaded Mode Integration Tests

**What's not tested:** Full end-to-end threaded execution with real GPU commands

**Files:** `crates/glaphica/src/phase4_threaded_tests.rs` exists but limited scope

**Risk:** Thread interaction bugs, deadlocks, or race conditions discovered late

**Priority:** **High** - Phase 4 completion blocker

---

### GC / Waterline-Based Eviction

**What's not tested:** `EngineCore::gc_evict_before_waterline()` is unimplemented

**Files:** `crates/glaphica/src/engine_core.rs:174-177`

**Risk:** Memory leaks in long-running sessions

**Priority:** **Medium** - impacts sustained usage scenarios

---

### Error Receipt Handling

**What's not tested:** Full error propagation from GPU thread to engine thread

**Files:** `crates/glaphica/src/engine_core.rs:160-171` (only handles `FeedbackQueueTimeout`)

**Evidence:**
```rust
// TODO: Handle other errors
_ => {
    eprintln!("[error] runtime error: {:?}", error);
}
```

**Risk:** Silent failures or incomplete error recovery

**Priority:** **Medium** - affects debugging and resilience

---

### Brush Execution Feedback Integration

**What's not tested:** End-to-end brush merge feedback loop in threaded mode

**Files:** `crates/glaphica/src/lib.rs`, `crates/brush_execution/`

**Risk:** Feedback may not be correctly applied to brush state

**Priority:** **High** - core feature correctness

---

## CI/CD Gaps

### Minimal CI Configuration

**Issue:** CI only runs clippy and basic tests (` .github/workflows/ci.yml`)

**Missing:**
- GPU test isolation (no `--test-threads=1`)
- Environment variable validation
- Build artifact caching beyond Cargo
- Platform-specific testing (macOS/Metal constraints)

**Risk:** Platform-specific bugs only discovered by users

**Recommendations:**
- Add matrix builds for Linux/macOS/Windows
- Add GPU test workflow with single-threaded execution
- Add clippy pedantic lints for critical crates

---

## Documentation Gaps

### Phase 4 State Tracking

**Issue:** Documentation (`phase4_implementation_plan.md`) is ahead of code implementation

**Files:** 
- `docs/Instructions/phase4_implementation_plan.md` (858 lines, comprehensive plan)
- Actual implementation: partial

**Risk:** Confusion about current state vs. target state

**Recommendation:** 
- Add `PHASE4_STATUS.md` with current completion checklist
- Mark TODOs with issue tracker references

---

### Crate API Documentation

**Issue:** Limited inline documentation for public APIs

**Files:** Most `lib.rs` files have minimal doc comments

**Recommendation:**
- Add `#![warn(missing_docs)]` to critical crates
- Generate docs with `cargo doc --no-deps`

---

*Concerns audit: 2026-02-28*
