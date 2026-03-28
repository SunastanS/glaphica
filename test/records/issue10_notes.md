# Issue 10 Investigation Notes

Related issue: `https://github.com/SunastanS/glaphica/issues/10`

## Repro Status

- Replay input: `test/records/tile_update_omitted.json`
- Repro remains deterministic.
- Stable symptom:
  - exported/document state is stable
  - frontend screenshot still shows missing stroke tiles

## Confirmed Observations

- In replay trace frame 4, the stable missing tile indices are:
  - `59, 60, 61, 76, 77, 78, 93, 94, 95, 110, 111, 112`
- In the same frame:
  - `TileSlotKeyUpdate` includes those tiles
  - `WriteOp` exists for those tiles
  - but `DrawOp` coverage only includes:
    - `62, 63, 64, 65, 79, 80, 81, 82, 96, 97, 98, 99, 113, 114, 115, 116`
- Added trace fields confirm:
  - frame 4 has `missing_updated_tile_indices` matching the missing 12 tiles
  - a later frame includes both runtime events:
    - `ApplyVisibleUpdates`
    - `ProcessRenderComposite`
  - therefore this is not explained by a simple missing redraw trigger alone

## Added Instrumentation

- `app/src/trace.rs`
  - `tile_timeline`
  - `submit_render`
  - `runtime_tile_events`
  - `draw_compaction`
- plumbing in:
  - `app/src/integration.rs`
  - `app/src/main_thread.rs`

## Verified / Ruled Out So Far

- Sparse dirty tile mapping in `document/shared_tree` is not the cause.
  - covered by added unit test
- Frontend/export flush ordering variants tested so far did not fix this case.
- Forcing a stronger wait/flush path from `process_render()` when pending visible updates exist
  but dirty render work is still empty did not change the replay result.
- Atlas bind-group cache key layer distinction was added and verified by tests.
- Narrow single-layer atlas sampling view plus shader-local layer param adjustment in
  `gpu_runtime/wgpu_brush_executor.rs` did not change the replay result.
- Narrowing `app/src/screen_blitter.rs` atlas sampling views to the exact sampled layer also did
  not change the replay result.
- Removing the current `move_mergeable_writes_to_end` call in
  `app/src/integration.rs` did not change the replay result.

## Recent Repair Experiments

- Workspace checkpoint commit before repair attempts:
  - `84b7258 Add issue 10 timing diagnostics and replay artifacts`
- Experiment 1:
  - change: make `process_render()` flush pending visible tile updates before concluding there is
    no render work when dirty tiles are still empty
  - result: no change in frontend replay artifact; issue remained bit-identical
  - conclusion: the stable corruption is not fixed by simply collapsing the visible-update timing
    gap before render
- Experiment 2:
  - change: narrow `screen_blitter` atlas sampling view to a single array layer and sample that
    view with shader-local layer `0`
  - result: no change in frontend replay artifact; issue remained bit-identical
  - conclusion: the bug is not explained by `screen_blitter` sampling the wrong atlas layer range

## Additional Narrowing Since Then

- The new non-GUI issue10 tests confirm the failure without depending on `present()` / onscreen
  display.
- GPU execution trace shows draw dispatches for the nominally missing tile indices do reach the GPU
  path later in replay; for example the missing set `59, 60, 61, 76, 77, 78, 93, 94, 95, 110,
  111, 112` appears in actual `[gpu_exec_trace][draw]` events.
- Extended GPU execution trace also shows stroke-buffer writeback for that same missing set reaching
  the leaf atlas; the corresponding `WriteOp` events for destination slots `28..39` are present.
- After those missing-tile draw/write events, GPU execution trace still records a later
  `render_cmd dst_tiles=28` pass, so the data path progresses beyond leaf writeback.
- A new headless export test now confirms the root render image itself contains non-white content
  in the frontend-missing tile set, while the comparison tile set that trace `DrawOp` coverage
  highlighted for frame 4 is fully white in the final root export.
- That means the frame-level `TraceOutputFrame.commands` view is not enough to prove those draws
  were never executed; frame splitting there is at least partially misleading for root-cause work.
- Current tighter boundary:
  - not `present()` timing alone
  - not `screen_blitter` layer-view selection
  - not absence of GPU draw dispatch for the missing tiles
  - not absence of stroke-buffer writeback dispatch for the missing tiles
  - final root render content already contains the expected missing-stroke tiles
  - more likely in frontend-visible sampling/readback or in how the final frontend path differs
    from headless root export

## Replay Validation Status

- After restarting the external MCP bridge, replay results are still unchanged.
- Current replay outputs remain identical to the pre-restart outputs for:
  - frontend screenshot
  - output json
  - document bundle
- That means the latest replay validation is now considered trustworthy.

## Current Working Conclusion

- Root cause was not a missing `WriteOp`, missing GPU writeback, or a renderer-side stale atlas read.
- The critical bug was that stroke-buffer `WriteOp` updated the final leaf tile content without adding
  the owning `(node_id, tile_index)` to `image_dirty_tracker`.
- Because `FrameBatch::execute_render_commands()` only rebuilds render caches from
  `image_dirty_tracker`, the root/cache composite phase could skip tiles whose current-frame work
  arrived through `WriteOp` + `TileSlotKeyUpdate` but no same-frame `DrawOp` on the final image.
- This matched the stable issue10 split:
  - previous frame already had `DrawOp` coverage for the eventually missing tiles
  - failing frame had `WriteOp` and `TileSlotKeyUpdate` for those tiles
  - but the failing frame only collected dirty image tiles from the four groups that still carried
    `DrawOp`
- Fix direction implemented:
  - extend `thread_protocol::WriteOp` with `node_id` and `tile_index`
  - populate that metadata in `crates/brushes/src/engine_runtime.rs`
  - mark `image_dirty_tracker` from `WriteOp` in `crates/gpu_runtime/src/frame_batch.rs`
  - apply visible tile-key updates in the same `process_gpu_commands()` pass so render collection sees
    the new tile mapping immediately
- After that change, the root image already contains ink on the previously missing tiles during the
  formerly failing command iteration, and the frontend artifact is no longer reproducible.

## Suggested Next Step

- Keep the new `WriteOp` image-dirty metadata path covered by regression tests, especially for
  stroke-buffer brushes that separate `DrawOp` from final leaf-tile writes.
- Treat old pending-visible-update timing tests as historical diagnostics only; they described the
  previous staging model and are now ignored because visible tile-key updates are applied in-frame.
