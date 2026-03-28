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
- Atlas bind-group cache key layer distinction was added and verified by tests.
- Narrow single-layer atlas sampling view plus shader-local layer param adjustment in
  `gpu_runtime/wgpu_brush_executor.rs` did not change the replay result.
- Removing the current `move_mergeable_writes_to_end` call in
  `app/src/integration.rs` did not change the replay result.

## Replay Validation Status

- After restarting the external MCP bridge, replay results are still unchanged.
- Current replay outputs remain identical to the pre-restart outputs for:
  - frontend screenshot
  - output json
  - document bundle
- That means the latest replay validation is now considered trustworthy.

## Current Working Conclusion

- The attempted fixes above are not root cause fixes for this issue.
- The issue is still most likely in or near:
  - `crates/brushes/src/engine_runtime.rs`
  - interaction between stroke-buffer command generation and later runtime/render handling
- The existing frame-level evidence is suggestive, but not yet sufficient to prove the exact bug,
  because transport/frame splitting can make same-frame command presence misleading.

## Suggested Next Step

- Add a non-GUI regression/debug path that exports both of these from the same process state:
  - root render image
  - frontend readback
- Use that to determine whether the corruption exists in:
  - shared-tree/render-cache state
  - or frontend/screen-blit path
