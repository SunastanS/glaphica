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

`ReadWrite` document images must be primitive images in the active document
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
`ImageEdit` inverse tile patches for document primitive images, not brush
replay.

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
  kernel: w(d, r) = max(0, 1 - d / max(r, 1px))
  input: center, radius, flow
  footprint: circle bounds in dst image space
  semantics: dst += kernel * flow, without clamping
```

The user brush or tool program owns spacing, interpolation, smoothing, jitter,
pressure mapping, and tool presets. The session receives input samples from the
Rust app loop, applies input mappings, invokes DrawOn primitives, records frame
dirty, uploads dirty through session/document graph edges on flush, and
materializes root repaint demand.

The first storage-backed implementation keeps that layering but uses a temporary
fallback mapper for `RadialKernel1D`: mapped canvas position becomes center,
`tool_params.radius` becomes dab radius with a 1px minimum, and pressure becomes
flow. This fallback is not the primitive contract. The primitive receives a
tool-specific engine input and the renderer pass only reads center, radius, and
flow after mapping.

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
from the document graph:

```text
base_paint -> paint_group_cache -> character_cache -> root_image
```

The session `doc_dirty` records dirty tiles for `base_paint`, not for
`coverage` or downstream caches. The durable draw history stores the committed
`ImageEdit` tile replacements for `base_paint`.

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

Root repaint is the union of uploading each dirty set through the active session
and document graph edges.

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

The IR admits `Identity`, affine `Matrix`, and the `Expand(px)` footprint
modifier. The current executable source-footprint path handles
`Identity + None` precisely; expanded and matrix source footprints remain TODOs.
Dirty upload for expanded and matrix paths currently falls back conservatively.

## Frame Flow

For a drawing session, each frame follows this image-level flow:

```text
1. accept source input groups under the app-loop FrameBudget
2. invoke DrawOn primitives and mark per-DrawOn frame dirty
3. flush frame dirty by uploading each DrawOn dirty set toward root
4. recursively render root demand through session and document commands
5. submit renderer passes
```

Frame budgeting gates only input/DrawOn acceptance. Once a source group is
accepted, downstream derive and render work is drained by `flush_frame`.

## Records

Tile commands and session images are execution details and are not stored in the
brush record.

A draw record stores:

```text
version
ImageId -> ImageEdit inverse patch for committed primitive document edits
```

Draw commit edits the tile slots of the current document bindings in place. It
does not replace `ImageId -> GlaImageKey` bindings. Derived document cache edits
are published as cache updates and are not retained by draw history.
