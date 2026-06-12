# ADR 001: DrawOn Primitive Execution

## Status

Accepted.

## Context

DrawOn is the executor-level primitive invocation layer. It must not depend on
user-facing brush names, raw stylus fields, or stroke-level tool state. The
session layer only needs enough information to allocate writable tiles, record
dirty tiles, and preserve execution order before downstream derive/render work.

## Decision

`DrawOnCommand` declares the writable destination and `DrawOnToolKind`.
Runtime execution uses typed `DrawOnInput` values. Each input declares:

- `center_x`, `center_y`
- `footprint_radius_px`
- a tool-specific input payload

The session uses the public center and footprint radius to enumerate affected
tiles and record dirty. Tool-specific fields are executor/shader payload. The
framework does not try to prove that a shader stays inside the declared
footprint; this is a tool contract.

The first built-in DrawOn tools are:

- `RadialKernel1D`: writes `D1/F32`, using `radius_px` and positive finite
  `amplitude`; it accumulates `dst += kernel * amplitude`.
- `ReplaceCircle4D`: writes `D4/F32`, using `radius_px` and a
  `PremultipliedRgbaF32`; matching pixels are directly replaced.

The frame exposes `DrawFrame::route_draw_targets` for shown-image coordinate
routing and `DrawFrame::draw_on` for typed DrawOn execution. `CanvasInput`
fallback mapping remains temporarily for the existing
`draw_dab(shown_image, input)` path.

GPU execution uses compute pipelines over atlas layer views. The renderer
encodes continuous DrawOn blocks, groups work by atlas layer and tool, builds
per-tile ordered programs, and dispatches tool-specific compute shaders.
`FixGutter` remains an explicit renderer pass emitted after DrawOn mutation by
the session/frame layer.

## Consequences

- `DrawOnCommand` no longer stores brush or tool parameters.
- DrawOn target format mismatches are rejected during session initialization and
  again defensively by the renderer.
- `RadialKernel1D` requires native read-write storage texture support for
  `R32Float`.
- Renderer initialization can be based on the registered/required
  `DrawOnToolKind` set; unregistered DrawOn passes fail at submit time.
- Upper layers remain free to implement pressure, spacing, smoothing, jitter,
  and dynamic size by producing different typed inputs per invocation.
- Session dirty scheduling is driven by declared input footprints.
