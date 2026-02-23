# Renderer Design Notes

This document tracks renderer-level design decisions and follow-up work for rendering multiple image formats (layer images vs brush buffer images) without runtime pipeline registration.

## Goal

Support rendering multiple tile atlas payload formats through a single render tree abstraction:

- Layer images: currently stored in `TileAtlasGpuArray` and sampled as `texture_2d_array<f32>` using the existing `tile_composite*.wgsl` pipelines.
- Brush buffer images: stored in `GenericR32FloatTileAtlasGpuArray` (R32Float tiles) and must be composited with non-filterable sampling and a different shader/pipeline family.

Constraints:

- Pipelines are hardcoded and created during renderer init.
- No runtime pipeline registration (too expensive/complex on hot paths).
- Fail fast: if an `ImageSource` is present but not supported end-to-end, panic instead of silently drawing wrong output.

## Current State (2026-02-23)

Protocol changes:

- `render_protocol::RenderNodeSnapshot::Leaf` now carries `image_source: render_protocol::ImageSource`.
- `ImageSource::LayerImage { image_handle }` is used by `document` today.
- `ImageSource::BrushBuffer { stroke_session_id }` exists for upcoming brush preview integration.

Renderer changes:

- Two hardcoded composite pipeline families exist:
  - RGBA (filterable): existing `tile_composite.wgsl` and `tile_composite_slot.wgsl`.
  - R32Float (non-filterable): new `tile_composite_r32float.wgsl` and `tile_composite_slot_r32float.wgsl`.
- Two atlas bind group layouts exist:
  - `renderer.atlas_layout` (filterable float) for RGBA.
  - `renderer.atlas_layout.nonfilterable` (non-filterable float) for R32Float.
- A bind group for sampling the brush buffer atlas exists (`brush_buffer_atlas_bind_group_nearest`) but is not wired into composite traversal yet.
- Leaf draw instance building currently panics if asked to build instances for `ImageSource::BrushBuffer` (intentional fail-fast).

## Next Steps (Minimum End-to-End Brush Preview Rendering)

### 1. Extend Resolver Surface Area

Renderer must be able to resolve tiles for both source kinds.

Preferred approach (keep renderer decoupled from `document` internals):

- Add new resolver methods to `renderer::RenderDataResolver`:
  - `visit_image_source_tiles(image_source, visitor)`
  - `visit_image_source_tiles_for_coords(image_source, coords, visitor)`
  - `resolve_image_source_tile_address(image_source, tile_key) -> Option<TileAddress>`

Rules:

- For `LayerImage`, these should map to the existing `visit_image_tiles` and `resolve_tile_address`.
- For `BrushBuffer`, the document-side resolver should consult brush buffer registries/stores for that `stroke_session_id`.
- If a source kind cannot be resolved, panic with enough context to debug (`stroke_session_id`, tile coords, tile_key).

This is a protocol-impacting refactor and will require updating `crates/glaphica/src/lib.rs` (`DocumentRenderDataResolver`) and any renderer tests that implement fake resolvers.

### 2. Wire ImageSource to Bind Group + Pipeline Set

Composite traversal needs to choose the correct atlas bind group + pipeline family per leaf:

- `ImageSource::LayerImage`: use `atlas_bind_group_linear` + `composite_pipelines_rgba`.
- `ImageSource::BrushBuffer`: use `brush_buffer_atlas_bind_group_nearest` + `composite_pipelines_r32float`.

Implementation direction:

- Add a helper in renderer composite code to create a `DrawPassContext` from `(image_source, viewport_mode, composite_space)`.
- Group rendering (group cache atlas) remains RGBA and continues using the group atlas bind groups.

### 3. Update Caches and Grouping Keys

Leaf caching must treat `ImageSource` as part of leaf draw semantics:

- `CachedLeafDraw` already stores `image_source`, but any additional caches keyed only by `layer_id` must be checked for collisions when preview is introduced.

Note:

- The current leaf cache map is keyed by `layer_id`. That is acceptable only if each `layer_id` maps to exactly one `image_source` per bound snapshot revision.
- If preview introduces transient extra leaves or per-layer mixed sources, the cache key needs to become `(layer_id, image_source_kind)` or a stable leaf node id.

### 4. Dirty/Version Semantics for Brush Buffer

Preview updates will happen frequently. For minimal correctness:

- Treat brush buffer leaves as always-dirty (force rebuild / force redraw) while a stroke is active.

For better performance:

- Expose `dirty_since(version)` + `version()` equivalents for brush buffers.
- Integrate them into `DirtyStateStore` and `mark_dirty_from_tile_history` so preview redraw is incremental.

### 5. Tests

Add at least:

- A renderer unit test that constructs a render tree containing a `BrushBuffer` leaf and asserts renderer selects the correct pipeline/bind group family (can be a pure logic test, no GPU required).
- A resolver test that ensures `BrushBuffer` tile iteration resolves unique addresses and rejects missing entries with a clear panic.

WGSL tests:

- `crates/renderer/src/wgsl_tests.rs` already enforces shader parse success; keep it passing for new WGSL sources.

## Known Environment Issue

In this environment, some wgpu-backed tests can emit driver/device permission warnings and parallel test execution may intermittently segfault.

Recommended local check for CI-like stability:

- Run tests single-threaded: `cargo test -- --test-threads=1`.

