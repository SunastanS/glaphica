# Brush Pipeline Summary

This document summarizes the structure, registration flow, execution flow, and key constraints of the brush pipeline in this repository.

## 1. Design Goals

The current implementation splits the brush into three layers:

1. **Engine Layer (CPU)**
   - Encodes `DrawOp.input` (`Vec<f32>`) based on `BrushInput` and the target tile.
2. **Runtime Layer (GPU Dispatch)**
   - Validates input layout by `brush_id`.
3. **Executor Layer (Concrete Execution Backend)**
   - Supports a dynamic WGSL wgpu executor (`WgpuBrushExecutor`).

The core change is:
- Each brush defines and registers its own `BrushGpuPipelineSpec` (instead of selecting from hardcoded pipelines).

---

## 2. Core Data Structures

### 2.1 Brush Spec

- `BrushSpec` (`crates/brushes/src/brush_spec.rs`)
  - `max_affected_radius_px()`
  - `draw_input_layout()`
  - `gpu_pipeline_spec()`

### 2.2 GPU Pipeline Spec

- `BrushGpuPipelineSpec` (`crates/brushes/src/gpu_pipeline_spec.rs`)
  - `label`
  - `wgsl_source`
  - `vertex_entry`
  - `fragment_entry`
  - `uses_brush_cache_backend`
  - `cache_backend_format: Option<TextureFormat>` - the format of the brush's cache atlas

Note:
- When `uses_brush_cache_backend = true`, the executor requires a cache backend to be provided during `configure_brush`.

### 2.3 Texture Format

- `TextureFormat` (`crates/glaphica_core/src/texture_format.rs`)
  - Platform-agnostic texture format enum
  - Converted to `wgpu::TextureFormat` in `gpu_runtime`

### 2.4 Registry

Brush registration is divided into three types of registries:

1. `BrushEngineRuntime`: engine pipeline (encodes draw input)
2. `BrushLayoutRegistry`: draw input layout
3. `BrushGpuPipelineRegistry`: GPU pipeline spec (WGSL spec)

`BrushSpec::register(...)` completes all three registrations at once, with duplicate/out-of-bounds checks before registration.

---

## 3. Brush Context and Configuration

### 3.1 BrushContext

Each brush has a `BrushContext` that caches all GPU resources needed for execution:

```rust
struct BrushContext {
    spec: BrushGpuPipelineSpec,
    cache_backend_id: Option<u8>,
    pipeline: Option<wgpu::RenderPipeline>,
    atlas_bind_group: Option<wgpu::BindGroup>,
}
```

### 3.2 Configuration Flow

Before a brush can be executed, it must be configured:

1. `BrushSpec::register(...)` - registers the brush in all registries
2. `WgpuBrushExecutor::configure_brush(brush_id, spec, cache_backend_id)` - configures the brush context

### 3.3 Invariants

- `source_backend` (Image atlas) is global and unique per document
- Each brush's `cache_backend` is determined at configuration time
- `source_sample_type`, `cache_sample_type`, `target_format` are all determined by configuration

---

## 4. Execution Flow

### 4.1 Engine Phase

`BrushEngineRuntime` is responsible for:

1. Finding affected tiles based on the brush's affected radius.
2. Calling the brush's engine pipeline to encode `BrushInput` into `DrawOp.input`.
3. Producing `GpuCmdMsg::DrawOp`.

If `build_draw_ops_for_image_with_ref_image(...)` is called, the reference image tile is written to `DrawOp.ref_image`.

### 4.2 GPU Runtime Phase

`BrushGpuRuntime::apply_draw_op(...)` is responsible for:

1. Looking up layout by `brush_id`.
2. Validating that `DrawOp.input` conforms to the layout.
3. Passing `draw_op + layout` to the executor.

Note: The pipeline spec is retrieved from the cached `BrushContext`, not passed as a parameter.

---

## 5. WgpuBrushExecutor

File: `crates/gpu_runtime/src/wgpu_brush_executor.rs`

### 5.1 Architecture

```
WgpuBrushExecutor
├── brushes: Vec<Option<BrushContext>>  // indexed by brush_id
├── draw_bind_group_layout: Option<wgpu::BindGroupLayout>
├── atlas_bind_group_layouts: Vec<CachedAtlasBindGroupLayout>
├── atlas_sampler: Option<wgpu::Sampler>
└── dummy_cache_texture: Option<DummyCacheTexture>
```

### 5.2 Single-Layer Lookup

Execution flow:

1. `brushes[brush_id]` → `BrushContext`
2. Check if `pipeline` and `atlas_bind_group` are created (first time creates them)
3. Use cached resources directly

No nested key lookups - all resources are cached per brush context.

### 5.3 Resource Group Layout

Currently fixed to two groups:

1. `@group(0)` per-draw dynamic data
   - `binding(0)`: draw input storage buffer
   - `binding(1)`: params uniform buffer

2. `@group(1)` atlas resources (cacheable/reusable)
   - `binding(0)`: source atlas `texture_2d_array`
   - `binding(1)`: cache atlas `texture_2d_array` (or dummy)
   - `binding(2)`: non-filtering sampler

### 5.4 Source Atlas Selection (ref_image Supported)

Source tile selection rules:

- If `draw_op.ref_image.is_some()`: use `ref_image.tile_key` as source
- Otherwise: use `draw_op.tile_key` as source

The source tile address is written to params (`src_tile_origin_x/y/layer`) for the shader to read the correct reference source.

---

## 6. Atlas Backend Conventions

`AtlasStorageRuntime` now provides:

- `resolve(tile_key)`: target tile address (including layer/offset/format)
- `backend_resource(backend_id)`: backend-level texture resource (texture/format/layers)

Default atlas texture usage includes:

- `COPY_DST`
- `COPY_SRC`
- `TEXTURE_BINDING`
- `RENDER_ATTACHMENT`

---

## 7. Current WGSL Conventions (Executor Side)

The executor provides:

- `group(0)/binding(0)`: `array<f32>` draw input
- `group(0)/binding(1)`: params (including target/source tile addresses)
- `group(1)/binding(0..2)`: source/cache atlas + sampler

Note:
- Shaders must match this resource layout, otherwise pipeline creation or runtime validation will fail.

---

## 8. Known Limitations and Future Suggestions

1. `BrushShaderParams` already uses `#[repr(C)]`, but the encoding function is still manually implemented. Consider using `bytemuck` to simplify in the future.
2. `WgpuBrushExecutor` currently submits command encoder per-draw. Batching could reduce submission overhead.
3. If source backend format changes (e.g., document color space change), brush contexts would need to be invalidated and recreated.