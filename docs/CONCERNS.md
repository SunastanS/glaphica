# Codebase Concerns

**Analysis Date:** 2026-03-05

## Tech Debt

**Debug Logging via eprintln!:**
- Issue: Using `eprintln!` for all logging instead of structured logging framework
- Files: Throughout codebase (82 instances across crates)
- Impact: No log levels, no filtering, no production-grade logging, console spam
- Fix approach: Integrate `tracing` or `log` crate with appropriate log levels

**Hardcoded Configuration:**
- Issue: Configuration values hardcoded as constants (e.g., brush cache backend IDs, atlas layouts)
- Files: `crates/app/src/main_thread.rs`, `crates/app/src/integration.rs`
- Impact: Requires code changes to modify behavior, no runtime configuration
- Fix approach: Extract to configuration file (TOML/JSON) or command-line arguments

**Magic Numbers in Tests:**
- Issue: Test data uses magic numbers without clear meaning
- Files: `crates/thread_protocol/src/lib.rs` (test module)
- Impact: Tests harder to understand and maintain
- Fix approach: Use named constants or builder patterns for test data

## Known Bugs

**No detected bug markers:**
- No TODO/FIXME/HACK/XXX comments found in codebase
- No known bug documentation

## Security Considerations

**No Security Concerns Detected:**
- Single-user desktop application
- No network communication
- No secrets management
- No authentication/authorization

## Performance Bottlenecks

**Potential GC Pressure from Arc Cloning:**
- Issue: Frequent `Arc::clone()` operations in hot paths (render tree, GPU context)
- Files: `crates/app/src/main_thread.rs`, `crates/document/src/shared_tree.rs`
- Impact: Reference counting overhead, potential cache misses
- Improvement path: Profile with `criterion`, consider `Arc::try_unwrap()` where possible

**Input Ring Buffer Dropping:**
- Issue: Input ring drops samples under high load (by design)
- Files: `crates/threads/src/lib.rs` (SharedInputRing)
- Impact: Loss of precision in stroke rendering, visible gaps
- Improvement path: Increase ring capacity, implement adaptive sampling

**Large Vec Allocations:**
- Issue: Pre-allocated Vecs with large capacity in integration layer
- Files: `crates/app/src/integration.rs` (input_samples, brush_inputs, gpu_commands)
- Impact: Memory overhead, potential fragmentation
- Improvement path: Pool allocation, capacity tuning based on profiling

## Fragile Areas

**Thread Communication Protocol:**
- Files: `crates/thread_protocol/src/lib.rs`, `crates/threads/src/lib.rs`
- Why fragile: Lock-free communication requires careful reasoning, race conditions hard to debug
- Safe modification: Extensive testing, use `loom` for concurrency testing
- Test coverage: Partial (merge logic tested, channels not fully tested)

**Render Tree Synchronization:**
- Files: `crates/document/src/lib.rs`, `crates/document/src/shared_tree.rs`
- Why fragile: Arc-swap semantics, multiple tree representations (UI/Render/Flat)
- Safe modification: Understand all tree transformation paths, test generation bumps
- Test coverage: Minimal (no tests for tree transformations)

**Brush Pipeline Configuration:**
- Files: `crates/brushes/src/engine_runtime.rs`, `crates/gpu_runtime/src/wgpu_brush_executor.rs`
- Why fragile: GPU shader configuration, pipeline state management, backend dependencies
- Safe modification: Test with actual GPU, validate shader compatibility
- Test coverage: None (requires GPU mocking)

## Scaling Limits

**Atlas Tile Capacity:**
- Current capacity: AtlasLayout::Small11 = 2^11 = 2048 tiles per backend
- Limit: Larger layouts available (up to 2^20 = 1M tiles)
- Scaling path: Increase atlas layout size, add more backends

**Brush Registry Size:**
- Current capacity: Hardcoded max_brushes parameter (16 in integration)
- Limit: Registry uses Vec-based storage
- Scaling path: Increase max_brushes parameter, or implement dynamic registry

**Thread Channel Sizes:**
- Current capacity: 256 (input ring), 64 (control), 1024 (GPU commands), 256 (feedback)
- Limit: Fixed-size ring buffers, no backpressure
- Scaling path: Tune based on profiling, implement dynamic sizing

## Dependencies at Risk

**wgpu Version Pinning:**
- Risk: Pinned to specific version 28.0.0
- Impact: May miss bug fixes, API changes in newer versions
- Migration plan: Regular dependency updates with testing

**Tokio Runtime for GPU Init Only:**
- Risk: Tokio used only for async GPU initialization, adds complexity
- Impact: Dependency overhead for limited async usage
- Migration plan: Consider `pollster` blocking executor (already feature-gated)

## Missing Critical Features

**No Undo/Redo System:**
- Problem: No undo/redo capability for stroke operations
- Blocks: User workflow, professional use cases

**No Document Persistence:**
- Problem: Documents only exist in memory
- Blocks: Saving/loading work, multi-session projects

**No UI/Tool Palette:**
- Problem: Hardcoded brush selection, no UI controls
- Blocks: User interaction, tool switching

**No Layer Management UI:**
- Problem: Layer operations not exposed to users
- Blocks: Document organization, non-destructive editing

## Test Coverage Gaps

**GPU Runtime Execution:**
- What's not tested: Render executor, atlas runtime, brush runtime, surface runtime
- Files: `crates/gpu_runtime/src/render_executor.rs`, `crates/gpu_runtime/src/atlas_runtime.rs`
- Risk: GPU command encoding errors, texture management bugs, rendering artifacts
- Priority: High (core rendering functionality)

**Stroke Input Processing:**
- What's not tested: Smoothing, resampling, velocity calculation, curvature calculation
- Files: `crates/stroke_input/src/input_processor.rs`, `crates/stroke_input/src/smoother.rs`
- Risk: Incorrect brush input generation, poor stroke quality
- Priority: High (affects user experience)

**Document Tree Transformations:**
- What's not tested: UiLayerTree → RenderLayerTree → FlatRenderTree conversions
- Files: `crates/document/src/lib.rs`
- Risk: Incorrect render tree structure, missing nodes, wrong ordering
- Priority: Medium (document correctness)

**Thread Coordination:**
- What's not tested: Input ring producer/consumer, control queue handling, GPU command routing
- Files: `crates/threads/src/lib.rs`, `crates/app/src/integration.rs`
- Risk: Race conditions, lost messages, deadlock
- Priority: High (thread safety)

**Brush Engine:**
- What's not tested: Brush registry, tile allocation, stroke processing
- Files: `crates/brushes/src/engine_runtime.rs`
- Risk: Incorrect tile allocation, missing stroke rendering
- Priority: High (core functionality)

---

*Concerns audit: 2026-03-05*