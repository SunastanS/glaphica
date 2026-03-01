# AGENTS.md

## Working Strategy

- Explicitly report when facing a task too large to finish in a roll
- Rectify the user when you doubt they have a wrong assumption of current code base
- Stop early when you need more context and references, maybe docs, examples
- Never write a fallback, make the program panic fast
- If a roll of developing didn't reach its original target, report next steps
- Do not run test automatically unless in debug tracing work

**Read `docs/guides/coding-guidelines.md` before coding.**

The gsd-tools is at /home/sunastans/.config/opencode/get-shit-done/bin/gsd-tools.cjs

## Crate-Specific Guidelines

### render_protocol (`crates/render_protocol/`)
- Defines cross-module message/data types for rendering, brush, merge, and render-tree communication.
- **Collaboration Rule**: Receiver/executor side may implement first and report. Initiator/caller side must report first and only modify after approval.
- Prefer additive changes (new fields/types) over breaking changes.
- Do not "guess" a protocol change to fix a bug; localize with logs/tests first.
- Run tests: `cargo test -p render_protocol -- --nocapture`

### renderer (`crates/renderer/`)
- **Debugging**: This is a GUI drawing app. CLI agents cannot independently observe most rendering bugs. Collaborate with user to collect context.
- **Observability**: Logs must be gated behind environment switches (default off):
  - `GLAPHICA_BRUSH_TRACE=1`
  - `GLAPHICA_RENDER_TREE_TRACE=1`
  - `GLAPHICA_RENDER_TREE_INVARIANTS=1`
  - `GLAPHICA_PERF_LOG=1`
  - `GLAPHICA_FRAME_SCHEDULER_TRACE=1`
  - `GLAPHICA_QUIET=1` (global quiet mode; when set, suppresses business logs even if trace switches are enabled)
- **GPU/wgpu**: Be extremely cautious with CPU→GPU ordering. Prefer safe ordering first, optimize after correctness.
- **Command-Queue**: Clean up state at the correct time boundary - only at enqueue time if no later command depends on it.

### All Crates
- Unit tests typically live in `src/tests.rs` or inline under `#[cfg(test)]`.
- Mark environment-sensitive tests with `#[ignore]` and document the intended run command.

---

## Project Structure

```
glaphica/
├── Cargo.toml          # Workspace root
├── crates/
│   ├── brush_execution/
│   ├── code_analysis/
│   ├── document/
│   ├── driver/
│   ├── frame_scheduler/
│   ├── glaphica/       # Main binary
│   ├── renderer_protocol/
│   ├── tiles/
│   ├── render│   └── view/
├── docs/
│   ├── guides/         # Developer guides
│   ├── architecture/   # Architecture docs
│   ├── planning/       # Project planning
│   └── debug/          # Debug records
└── .github/workflows/
    └── ci.yml
```

---

## Additional Resources

- `docs/guides/debug-playbook.md` - Debugging strategies
- `docs/guides/wgpu-guide.md` - GPU-specific guidance
- `crates/renderer/DESIGN.md` - Renderer architecture
- `crates/renderer/docs/merge_ack_integration.md` - Merge integration details
