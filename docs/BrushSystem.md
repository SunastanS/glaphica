# Brush System

This document describes how user-facing brush tools compile into image-level
draw sessions.

The session executor does not know brush names, document nodes, layer trees, or
tile keys. A brush compiles to `DrawOn` and `Derive` commands over image ids.
Runtime execution rules are defined in `docs/Session.md`.

## Tool Layers

There are three distinct layers:

```text
User tool:
  PixelRoundBrush, WatercolorBrush, Eraser, etc.

DrawSessionIR:
  doc image access declarations,
  session images,
  DrawOn commands,
  Derive commands,
  input mappings.

Executor primitives:
  DrawRadialKernel1D, MergeColor, BlurCoverage, RenderGroup, etc.
```

User tool names are for UI, presets, registration, debugging, and errors. They
do not enter hot executor semantics. The executor sees primitive op ids,
command config, image ids, mappings, and tile ranges.

## Document Access

A draw session explicitly declares the document images it reads or writes:

```text
doc_images:
  paint_layer: ReadWrite
  source_group_cache: Read
```

`ReadWrite` document images must be primitive images in the active registry
graph. Derived document images may be read, but they cannot be direct draw
targets. Their writes come from graph commands.

`image.backup` is written explicitly at a command read site. It reads the
stroke-start document image key and never sees session-local images or
session-local derived commands.

## Session Images

Brush tools may declare session-local images:

```text
session_images:
  coverage Primitive D1 layout Like(paint_layer)
  soft_coverage Derived D1 layout Like(paint_layer)
```

Session images share one namespace for the whole draw session. They may shadow
document ids, but commit and cleanup use the IR declarations rather than id
equality. A session image id must not also be declared as a `ReadWrite`
document image in the same draw session.

`Like(image)` format or layout references must point to a doc image or an
earlier session image declaration. Session image declarations are resolved in
order.

Session images are released when the draw session ends. They are not stored in
the document binding table and are not retained for undo or replay. Undo uses
document image CoW versions, not brush replay.

## DrawOn And Derive

Brush work compiles to two executor forms:

```text
DrawOn:
  input-driven primitive invocation
  ordered by input samples
  may mutate or accumulate
  may write a document ReadWrite image or a session image

Derive:
  dirty-driven image command
  full-overwrite per affected tile
  may write a document ReadWrite image or a session image
  may read dst.backup, but not dst.current
```

Direct drawing and pigment-then-merge drawing differ only in the lifetime of the
image first modified by `DrawOn`.

```text
Direct draw:
  DrawOn -> paint_layer

Pigment merge:
  DrawOn -> coverage
  Derive(coverage, paint_layer.backup) -> paint_layer
```

An image may have only one writer in a draw session. A document image cannot be
both a `DrawOn` destination and a `Derive` destination in the same session.

## Source Primitives

A source primitive is lower-level than a user brush. For example, the pixel
round brush is not itself a primitive. Its coverage can be produced by:

```text
DrawRadialKernel1D:
  dst: coverage
  kernel: linear-clamped compact gaussian approximation
  input: center, radius, flow
  footprint: circle bounds in dst image space
  semantics: accumulate with zero initialization
```

The user brush or tool program owns spacing, interpolation, smoothing, jitter,
pressure mapping, and tool presets. The session receives input samples from the
Rust app loop, applies input mappings, invokes DrawOn primitives, marks dirty,
and drains downstream Derive and registry commands.

DrawOn primitives have one `ReadWrite` destination and no image read edges in
the first version. Current-reading stamp or smudge behavior that requires
source images, dab-level snapshots, or frame-level snapshots is not part of the
first version.

## Pixel Round Example

A simple pixel round brush can compile to:

```text
doc_images:
  base_paint: ReadWrite

session_images:
  coverage Primitive D1 layout Like(base_paint)

draw_on:
  DrawRadialKernel1D(input -> coverage)

derive:
  MergePixelRound(base_paint.backup, coverage) -> base_paint
```

The merge command is dirty-driven and full-overwrite for each affected
destination tile:

```text
base_paint[tile] =
  merge(base_paint.backup[tile], coverage[tile], frozen_brush_config)
```

The merge command does not copy backup into target first. It reads backup and
coverage and writes the current `base_paint` image.

Downstream group cache and root updates are not part of the brush IR. They come
from the registry graph:

```text
base_paint -> paint_group_cache -> character_cache -> root_image
```

The resulting `DrawRecord.doc_dirty` records dirty tiles for `base_paint`, not
for `coverage` or downstream caches.

## Watercolor Example

A more complex watercolor brush can compile to:

```text
doc_images:
  base_paint: ReadWrite

session_images:
  stroke_coverage  Primitive D1 layout Like(base_paint)
  stroke_wetness   Primitive D1 layout Like(base_paint)
  soft_coverage    Derived D1 layout Like(base_paint)
  edge_darkening   Derived D1 layout Like(base_paint)
  settled_pigment  Derived D4 layout Like(base_paint)

draw_on:
  DrawRadialKernel1D -> stroke_coverage
  DrawRadialKernel1D -> stroke_wetness

derive:
  BlurCoverage(stroke_coverage) -> soft_coverage
  BuildEdgeDarkening(stroke_coverage, stroke_wetness) -> edge_darkening
  SettlePigment(soft_coverage, stroke_wetness) -> settled_pigment
  MergeWatercolor(base_paint.backup, settled_pigment, edge_darkening)
    -> base_paint
```

`BlurCoverage` uses an identity read mapping with `Expand(radius_px)`. Rendering
one blur tile may therefore read a neighborhood of source tiles, such as 3 by 3.
The same edge mapping and modifier define dirty upload from `stroke_coverage` to
`soft_coverage`.

## Multiple Document Targets

A draw session may declare multiple `ReadWrite` document images. Each target has
its own input mapping, and dirty is recorded per target:

```text
doc_images:
  color_layer: ReadWrite
  wetness_layer: ReadWrite

doc_dirty:
  color_layer -> Tiles(...)
  wetness_layer -> Tiles(...)
```

Root repaint is the union of uploading each dirty set through the active
registry graph.

## Input Mapping

Every image has its own coordinate system. DrawOn input mapping maps app input
coordinates into the destination image coordinate system:

```text
input_mapping:
  Identity
  Matrix(canvas_to_draw_dst)
```

Read edges use mappings from derive destination space to read source space. The
same coordinate relationship is used for read footprints and dirty upload.

Stage 1 supports `Identity`, affine `Matrix`, and the `Expand(px)` footprint
modifier.

## Frame Flow

For a drawing session, each frame follows this image-level flow:

```text
1. accept source input groups under the DrawOn budget
2. invoke DrawOn primitives and mark written images dirty
3. drain session-local Derive commands
4. drain registry-derived cache and root commands
5. submit root repaint work
```

Frame budgeting gates only input/DrawOn acceptance. Once a source group is
accepted, all downstream derive and render work is drained in the same frame.

## Records

Tile commands and session images are execution details and are not stored in the
brush record.

A draw record stores:

```text
graph
bindings_before
bindings_after
doc_dirty for ReadWrite document images
root_cache_before
root_cache_after
```

If a draw session produces no document dirty, its dirty set is empty. The
session may still commit active-chain binding or cache replacements.
