# Glaphica Debug Triage (Whole App)

Use this as a fast symptom classifier, then follow the repo playbooks:
- `docs/Instructions/debug_playbook.md`
- `docs/debug/brush_merge_duplicate_tiles_2026-02-23.md`
- `docs/Instructions/wgpu.md`

## Symptom → Likely Layer

### “I don’t know where it breaks”
- First goal: identify the failing module with logs.
- Ask the user for: repro steps, expected vs actual, and a short log excerpt around the first anomaly/panic.

### Tiles look repeated / “copied everywhere”
- **If `TileKey`/`TileAddress` are unique but content repeats**: suspect GPU state reuse (uniform/storage/instance overwrite before a single submit).
- **If keys/addresses collide**: suspect mapping/resolver bugs (key→address uniqueness).

### Affected tiles don’t match cursor path
- Suspect coordinate contract mismatch:
  - Driver consuming `screen` as `canvas`
  - Missing/incorrect `screen -> canvas` inverse transform

### Artifacts appear after adding live preview / extra group level
- Suspect render tree semantic/revision contract:
  - Semantics changed without revision bump
  - Semantic hash includes/excludes preview incorrectly
  - Cache key instability causing unintended rebuild/copy

### 1px bleed at tile borders
- Suspect tile atlas slot/gutter boundary:
  - Dab footprint crosses slot bounds
  - Shader sampling uses wrong stride/gutter assumptions

### Grey screen / “freezes then recovers”
- Suspect accidental full composite/copy in hot path:
  - Dirty model marks too much as dirty
  - Cache rebuild/copy spikes triggered by tree changes

### Crash/panic without obvious repro
- Collect the first panic line + backtrace (set `RUST_BACKTRACE=1`).
- Reduce the app steps until it reproduces reliably (new document, default settings).
- Convert the panic into a unit test if possible (“does not panic”) to prevent regressions.

## Recommended Debug Switches

Enable only what you need:
- `GLAPHICA_BRUSH_TRACE=1`
- `GLAPHICA_RENDER_TREE_TRACE=1`
- `GLAPHICA_RENDER_TREE_INVARIANTS=1`
- `GLAPHICA_PERF_LOG=1`
- `GLAPHICA_FRAME_SCHEDULER_TRACE=1`

## Convert a Guess into an Assertion

Prefer fail-fast invariants at boundaries:
- Mapping: a key maps to one coordinate; a coordinate maps to one key.
- Planning: merge output coordinates unique; dirty tiles match mappings.
- Render tree: semantic changes must bump revision.
- Atlas: per-dab writes must stay inside the intended slot bounds.
