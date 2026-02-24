---
name: glaphica-debugger
description: Debugging playbook for the Glaphica GUI drawing app across the whole codebase (not only renderer). Use when diagnosing any bug, crash, panic, performance regression, visual artifact, or input issue, especially when the CLI agent needs to collaborate with the user to collect context via logs and minimal repros, then convert observations into focused assertions and regression tests before attempting fixes.
---

# Glaphica Debugger

## Overview

Follow a repeatable workflow to turn a user-observed GUI issue into (1) a minimal reproducible scenario, (2) logs that identify the failing module, (3) a focused test that reproduces the issue, and only then (4) a small targeted fix.

Keep observability opt-in (env-gated logs) and prefer “panic fast” invariants over silent fallbacks.

## Workflow Decision Tree

Because this is a GUI drawing app, a CLI agent cannot independently observe the UI. Prefer collaboration-first debugging:

1. Ask the user for a minimal repro sequence and the exact expected vs actual behavior.
2. Turn on the smallest set of logs to identify *which module* is misbehaving.
3. Write a focused test to reproduce the issue (or a deterministic repro harness).
4. Fix only when the failing location and invariant are clear.

If the issue is likely renderer/wgpu related, converge in this order (high probability → low probability):

1. Input coordinate contract: does the driver receive `canvas` or `screen` coordinates?
2. Render tree semantics/revision: does semantic change always bump revision (and does stable semantics avoid rebinding)?
3. Dirty model correctness: are only the affected tiles marked dirty (not “visible region = dirty”)?
4. wgpu submit semantics: is the same buffer range overwritten multiple times before a single `queue.submit`?
5. Tile atlas boundaries: do writes cross slot/tile boundaries (gutter/stride bugs)?

Heuristic:
- “Content repeats but key/address does not”: suspect GPU state reuse / submit hazards.
- “Affected tiles don’t match cursor path”: suspect `screen -> canvas` transform chain.
- “Adding preview/group changes triggers artifacts”: suspect render-tree semantics or per-submit buffer overwrites.

## Step 0: Read the Local Playbooks (Once Per Debug Session)

Use the repo’s distilled experience as the baseline:
- `docs/Instructions/debug_playbook.md`
- `docs/debug/brush_merge_duplicate_tiles_2026-02-23.md`
- `docs/Instructions/wgpu.md`

## Step 1: Increase Observability (Don’t Guess)

Turn on only the signals you need (logs must be env-gated; default off):
- `GLAPHICA_BRUSH_TRACE=1`: pointer `screen -> canvas` mapping + brush/merge submission trace
- `GLAPHICA_RENDER_TREE_TRACE=1`: bind reasons, revision, semantic changes
- `GLAPHICA_RENDER_TREE_INVARIANTS=1`: fail-fast when semantics change without revision bump
- `GLAPHICA_PERF_LOG=1`: dirty polling + cache rebuild/copy performance hints
- `GLAPHICA_FRAME_SCHEDULER_TRACE=1`: brush hot-path activation/ticks

If the visual symptom is “too subtle”, increase signal strength first (example: use a bigger default brush radius while debugging tile boundaries).

### Collaboration Checklist (Ask the User)

Collect concrete context early:
- Exact steps to reproduce (as a numbered list).
- Whether it reproduces on a clean start / new document.
- Whether it depends on zoom/pan/rotation, brush settings, layer count, or canvas size.
- The first panic line + backtrace (if any), and the relevant log window around it.
- A screen recording or screenshot if the bug is visual.

## Step 2: Convert Hypotheses into Falsifiable Assertions

Do not rely on log-reading alone. For each hypothesis, add a hard assertion at a dataflow boundary:
- Resolver boundary: “different `TileKey` must not map to same `TileAddress`”
- Mapping boundary: “same `TileKey` must not bind to multiple tile coordinates”
- Plan boundary: “merge plan outputs must not contain duplicate destination coordinates”
- Draw boundary: “draw instances must not contain duplicate tile coords”
- Brush boundary: “dab write region must not cross the slot bounds”

Prefer to keep assertions in debug builds if they protect important invariants.

## Step 3: Turn “Random UI Bug” into a Minimal Regression Test

Prioritize a unit test before iterating on UI observation:
- Build the smallest synthetic input that exercises the suspected layer (renderer/merge/geometry/dirty).
- Assert a crisp invariant (pixel-exact readback, mapping uniqueness, or “does not panic”).

For GPU-ish tests:
- Run with `--test-threads=1` if you see instability.
- Mark heavy / environment-sensitive tests as `#[ignore]` and run explicitly.

### How Tests Are Organized in This Repo

Common pattern: crate unit tests live in `crates/<crate>/src/tests.rs`, and are wired from `crates/<crate>/src/lib.rs` via:
- `#[cfg(test)] mod tests;`

Practical workflow:
- Find the owning crate for the code under test.
- Add a focused `#[test]` in that crate’s `src/tests.rs` (or create it and add `mod tests;`).
- Run it with: `cargo test -p <crate> <test_name> -- --nocapture`

Integration-style tests may also live at repo root under `tests/` (use when cross-crate behavior matters).

## Step 4: Fix at the Correct Time Boundary

If a command queue exists, clean up state when the command is *consumed*, not when it is *enqueued*, unless you can prove no later command depends on the state.

## Safety Rule: Don’t Touch Core Logic Without a Pinpoint

Unless the failing invariant and location are very clear, do not change core logic “to try things”.
Prefer:
- adding logs/invariants to narrow the fault,
- adding a focused test repro,
- making the smallest fix that flips the test from red to green.

## References in This Skill

- `references/triage.md`: symptom → likely layer → next check
- `references/test-organization.md`: where to add tests + how to run them
- `references/wgpu-submit-hazards.md`: common CPU->GPU ordering pitfalls
