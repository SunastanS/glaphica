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
