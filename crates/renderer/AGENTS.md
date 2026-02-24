# AGENTS.md (crates/renderer)

## Scope

These instructions apply to everything under `crates/renderer/`.

## Debugging Strategy (Collaboration-First)

This is a GUI drawing app. A CLI agent cannot independently observe most rendering bugs.
Prefer to collaborate with the user to collect context, then narrow the fault with logs and tests:

1. Get a minimal repro sequence + expected vs actual behavior.
2. Use env-gated logs to identify the failing subsystem (do not add always-on hot-path logs).
3. Convert the repro into a focused regression test (or the smallest deterministic harness).
4. Only then apply the smallest fix that flips the test from red to green.

Unless the failing location is very clear, do not modify core render logic “to try things”.

## Observability Rules

- Logs must be gated behind existing env switches (or add a new one); default must be off.
- Prefer fail-fast invariants (`panic!` with context) over silent fallbacks.

Common switches (enable only what you need):
- `GLAPHICA_BRUSH_TRACE=1`
- `GLAPHICA_RENDER_TREE_TRACE=1`
- `GLAPHICA_RENDER_TREE_INVARIANTS=1`
- `GLAPHICA_PERF_LOG=1`
- `GLAPHICA_FRAME_SCHEDULER_TRACE=1`

## wgpu / GPU Semantics (High-Risk Area)

Be extremely cautious with CPU→GPU ordering:
- Avoid overwriting the same buffer range multiple times before a single `queue.submit` if multiple passes may read it.
- If correctness is uncertain, prefer the safe (possibly slower) ordering first, then optimize.

Reference docs to read when touching GPU submission or merge:
- `docs/Instructions/debug_playbook.md`
- `docs/Instructions/wgpu.md`
- `crates/renderer/DESIGN.md`
- `crates/renderer/docs/merge_ack_integration.md`

## Command-Queue Time Boundaries

If a renderer command is enqueued then consumed later, state cleanup must happen at the correct time boundary:
- Only clean up state at enqueue time if no later queued command can depend on it.
- Otherwise, clean up when the corresponding command is consumed.

## Tests (Preferred Fix Vehicle)

- Unit tests live in `crates/renderer/src/tests.rs` and are wired from `crates/renderer/src/lib.rs` via `mod tests;`.
- Run focused tests with: `cargo test -p renderer <test_name> -- --nocapture`
- For GPU-related tests, consider: `--test-threads=1`
- If the test is environment-sensitive, mark it `#[ignore]` and document the intended run command in the test name or message.

