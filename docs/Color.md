# Color

This document records the first renderer-facing color model. It is deliberately
small, but every supported render path must have an explicit pixel
interpretation.

## Storage Format

`GlaFormat` describes storage only:

```text
ChannelCount: D1 | D2 | D4
ChannelType:  U8 | U32 | F32 | F64
```

Storage format does not by itself define color semantics.

## Default Pixel Interpretation

Stage 1 defaults are:

```text
D1 -> Value
D2 -> unsupported / uninterpreted
D4 -> premultiplied RGBA in LinearSrgb
```

`D4` is therefore treated as color only by convention at this stage. Future
metadata may override or refine this default, but renderer code must not infer
more than the active interpretation says.

## Supported Composite Kinds

Current renderer-supported composites are:

```text
D4 RGBA -> D4 RGBA:
  Multiply
  Overlay

D1 Value -> D4 RGBA:
  MaskAlpha
```

`MaskAlpha` applies the D1 value as a scalar mask to the premultiplied RGBA
destination tile:

```text
dst.rgba = dst.rgba * clamp(value * opacity, 0, 1)
```

Other combinations, including D2 inputs or RGBA blend modes applied to D1
sources, are unsupported until they receive an explicit interpretation and
blend definition.
