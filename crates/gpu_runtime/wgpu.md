# wgpu 28 API Notes

This document captures practical findings for `wgpu = 28.0.0`, focused on context creation and initialization in `gpu_runtime`.

## 1. Core Initialization Flow

1. `wgpu::Instance::new(&wgpu::InstanceDescriptor)` is synchronous.
2. `Instance::request_adapter(&wgpu::RequestAdapterOptions)` returns `Future<Output = Result<wgpu::Adapter, wgpu::RequestAdapterError>>`.
3. `Adapter::request_device(&wgpu::DeviceDescriptor)` returns `Future<Output = Result<(wgpu::Device, wgpu::Queue), wgpu::RequestDeviceError>>`.

Conclusion: adapter/device acquisition is async. If a synchronous API is needed, use a blocking wrapper (for example `pollster`).

## 2. `request_adapter` Notes

Key `RequestAdapterOptions` fields:

- `power_preference: wgpu::PowerPreference`
- `force_fallback_adapter: bool`
- `compatible_surface: Option<&wgpu::Surface<'_>>`

Findings:

- `compatible_surface` affects adapter filtering. For presentation paths, pass the surface whenever possible.
- `wgpu::util::initialize_adapter_from_env_or_default` supports `WGPU_ADAPTER_NAME` and falls back to a default adapter.

## 3. `DeviceDescriptor` Fields in wgpu 28

In `wgpu 28`, `DeviceDescriptor` includes:

- `label`
- `required_features`
- `required_limits`
- `experimental_features`
- `memory_hints`
- `trace`

Implication: initialization should explicitly model `experimental_features`, `memory_hints`, and `trace`, not only features/limits.

## 4. Pre-checks to Avoid Panics

`Adapter::request_device` documentation states panic cases (for example unsupported features/limits).

Recommended explicit validation before requesting device:

- Feature check: `requested_features.difference(adapter.features()).is_empty()`
- Limits check: `requested_limits.check_limits(&adapter.limits())`

This converts capability mismatches into controlled errors instead of runtime panics.

## 5. Defaults Recommended for This Repository

Suggested defaults:

- Use `AdapterSelection::EnvOrDefault` by default for easier local adapter selection via `WGPU_ADAPTER_NAME`.
- Default `required_features = wgpu::Features::empty()`.
- Default `required_limits = wgpu::Limits::default()`.
- Provide a stable default `device_label` for easier debugging/profiling.

## 6. Current `gpu_runtime` API Surface

Implementation file:

- `crates/gpu_runtime/src/context_and_init.rs`

Currently provided:

- `GpuContext`
- `GpuContextInitDescriptor`
- `GpuContextInitError`
- `GpuContext::init(...)`
- `GpuContext::init_with_surface(...)`
- `GpuContext::init_blocking(...)` (behind `blocking` feature)

## 7. Texture View Usage Scope

`wgpu` tracks usage hazards at the texture subresource scope covered by a bound view, not only by the texel region a shader happens to sample.

Practical rule for this repository:

- When a pass writes one atlas layer as `RENDER_ATTACHMENT` and also samples from the same atlas texture, the sampling view must be narrowed to the exact layer(s) being read.
- Do not create a `D2Array` sampling view that covers the whole atlas (`base_array_layer = 0`, `array_layer_count = None`) if the pass also writes another layer from that same texture.
- Prefer `base_array_layer = resolved.address.layer` and `array_layer_count = Some(1)` for atlas read views unless there is a concrete need to sample multiple layers in one pass.

Otherwise `wgpu` can report a conflicting `RESOURCE` + `COLOR_TARGET` usage even when the logical source tile and destination tile are on different layers.

## 8. Blend / Opacity Semantics

The render backend in this repository uses premultiplied-alpha storage for intermediate and final atlas tiles.

Practical invariants:

- Shader outputs written into atlas tiles must be premultiplied by the effective alpha they carry.
- `opacity` is part of the blend contribution. It must be applied exactly once to both alpha and RGB contribution, not only to alpha.
- When a shader needs unpremultiplied color for blend math (for example `multiply`), it may temporarily divide by source alpha, but the value written back to the target must be premultiplied again.

Examples:

- Normal source-over:
  `out_rgb = src_rgb + dst_rgb * (1 - src_a)`
  `out_a = src_a + dst_a * (1 - src_a)`
- Multiply source-over:
  `out_rgb = src_rgb * (1 - dst_a) + dst_rgb * (1 - src_a) + (dst_unpremul * src_unpremul) * (dst_a * src_a)`
  `out_a = src_a + dst_a * (1 - src_a)`

Notes for this codebase:

- `render_shader.wgsl` `fs_multiply` / `fs_image_multiply` must output premultiplied RGB. Returning unpremultiplied RGB there makes low-opacity multiply layers brighter than the base, which is incorrect.
- `render_composite_shader.wgsl` must preserve the original blend mode semantics of the overlay. A cached overlay rendered with `Multiply` cannot be recomposited with normal source-over logic without changing the visible result.
- Do not treat `opacity` as a preprocessing step that changes only alpha coverage. In this backend it is part of the final compositing weight.

## 9. Composite Path Semantics

There are two distinct composition stages in the backend and they must agree on blend semantics:

- Direct render pass composition in `render_executor.rs`, where leaf/image sources are drawn into a destination tile using `LeafBlendMode`.
- Explicit `CompositeOp` composition, where an already-rendered overlay tile is merged onto a base tile.

Rules:

- If a blend mode exists in both stages, both stages must implement the same visual result for the same premultiplied inputs and opacity.
- `CompositeOp` blend mode is its own semantic axis. Do not reuse write/erase enums for composite behavior just because the payload shape looks similar.
- When adding a new blend mode, update all of:
  `thread_protocol` command enum
  trace serialization in `app/src/trace.rs`
  composite shader entry points
  pipeline selection in `render_executor.rs`
  regression tests covering low-opacity behavior

Recommended regression cases:

- White `Multiply` overlay at 50% opacity over mid-gray must stay mid-gray, never brighten.
- Cached-overlay `Multiply` and direct-render `Multiply` should match for the same inputs.
