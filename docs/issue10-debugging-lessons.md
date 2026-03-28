# Issue 10 Debugging Lessons

## What Actually Broke

- The visible corruption looked like a renderer/front-end bug, but the decisive fault was earlier in
  the pipeline: `WriteOp` updated final leaf tiles without contributing to image-level dirty
  tracking.
- `FrameBatch::execute_render_commands()` only rebuilds branch/root caches from
  `image_dirty_tracker`, so any tile that only arrived through `WriteOp` could be missing from the
  same-frame cache rebuild.

## Why The Earlier Hypotheses Were Plausible

- Replay traces showed `TileSlotKeyUpdate`, `WriteOp`, and later GPU execution for the missing tiles,
  which made the issue look like stale presentation, bad readback, or a timing gap.
- The custom `GpuCmdMsg` channel plus frame-merge policy also split logical stroke work across output
  frames, which made frame-local command presence misleading.

## What Narrowed It Down

- Compare multiple artifacts, not just one:
  - trace json for command sequencing
  - headless root export for actual composited result
  - frontend-facing replay output for user-visible failure
- Track the same tiles across stages:
  - `DrawOp`
  - `WriteOp`
  - `TileSlotKeyUpdate`
  - dirty collection
  - render-cache composite
- Add tests that validate the first violated invariant, not just the final screenshot.

## Concrete Lessons For This Codebase

- For buffered brushes, `DrawOp` and `WriteOp` do not mean the same thing.
  - `DrawOp` may only touch stroke-buffer tiles.
  - `WriteOp` may be the first operation that actually changes the final leaf tile.
- Dirty tracking must follow semantic image updates, not just GPU work submission.
- Reorder/merge logic is dangerous when protocol messages carry different semantic phases.
  - moving metadata to the end can make traces look like updates happened "late"
  - channel chunking can split one logical stroke update across multiple observed frames
- A command-trace regression can become obsolete after a pipeline fix if the real user-visible
  invariant changes. Keep historical diagnostics, but promote image-visible regressions as the main
  acceptance tests.

## Recommended Workflow For Similar Bugs

- First reproduce with a deterministic replay.
- Then add tile-level assertions for each semantic stage.
- Distinguish carefully between:
  - GPU tile writes
  - visible tile-key updates
  - image dirty collection
  - cache composite
  - final readback/present
- When a brush has staging/buffered passes, always ask which command actually mutates the final leaf
  image, and make sure that command participates in dirty tracking.
