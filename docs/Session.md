# Session

This document defines the image-level session model.

The upper management layer owns the document tree, layer semantics, UI tool
names, and document edits. It is currently expected to be driven from Janet, but
that binding is not part of this document.

Rust owns the compiled image graph, image bindings, resource keys, and session
execution. A session works only with images, image commands, tile ranges, and
tile resources. It does not understand layer nodes, group nodes, masks as
document concepts, or brush names.

## Keys And Active State

Janet-visible IR names images with compact opaque `ImageId`s. `ImageId` is
business-layer identity. Display names and UI layer names are outside the
executor identity model, and image storage does not know `ImageId`.

Rust stores resources behind generation-checked keys:

```text
ImageKey              // image tile array / content version
RegistryGraphKey      // image declarations and derived command graph version
ImageBindingTableKey  // ImageId -> ImageKey content binding version
```

The active document state is:

```text
ActiveDocumentState {
  graph: RegistryGraphKey,
  bindings: ImageBindingTableKey,
  version: DocumentVersionId,
}
```

`DocumentVersionId` is the lightweight Janet-visible freshness check. Rust keys
do not leak to Janet-visible IR.

The registry graph stores the single document root:

```text
RegistryGraph {
  root: ImageId,
  declarations: ImageId -> ImageDeclaration,
  ...
}
```

The root may be a primitive or derived image. The binding table must cover every
image id in the graph. The root is not special in the binding table; the binding
table only answers which `ImageKey` currently backs an `ImageId`.

`ImageId -> ImageKey` binding tables are doc/session-layer state. Runtime role
lookup is keyed by `ImageKey`, so recursive `render(image_key, tile_index)` can
find `Primitive | Derived(command)` directly without reverse lookup through an
image id.

Doc-level `ImageId`s are never reused within one document lifetime. Session-local
images use the same `ImageId` type and may shadow doc ids within one draw
session.

The Rust module boundary follows that split. `gla_doc` owns `RegistryGraph`,
`ImageBindingTable`, active document snapshots, registry patch application, and
undo/redo transitions. `gla_session` is the app-loop entry point and owns the
local binding overlay and key-role index used to shadow document images during a
session.

## Registry Graph

Each document image declaration is one of:

```text
PrimitiveImage {
  format,
  layout,
}

DerivedImage {
  format,
  layout,
  command: GraphCommand,
}
```

`PrimitiveImage` is editable or externally supplied image content. It has no
graph command and may be declared `ReadWrite` by a draw session.

`DerivedImage` is a document-level cache or output image. It has exactly one
graph command and is not a direct draw target. The single-command rule is
intentional: a group cache must not have different update programs depending on
which child layer was edited.

Registry validation derives:

```text
writer_of: ImageId -> GraphCommandIndex
readers_by_image: ImageId -> [GraphCommandIndex]
topo_order: [GraphCommandIndex]
```

Validation rules:

- the graph has exactly one root;
- every remaining graph image is reachable from the root by walking derived
  command reads backwards from the root;
- every derived image has exactly one graph command;
- primitive images have no graph command;
- all graph command reads reference registered document images;
- graph commands form an acyclic image dependency graph;
- a graph command writes exactly one derived image;
- a graph command does not read its destination current image;
- each read edge has a coordinate mapping.

A command may read the same image multiple times with different ports or
mappings. Dirty fanout registers the command once per read image and unions the
affected destination tiles from every matching read edge.

## Registry Patch Sessions

A session task is either drawing work or registry management work. A single
session never mixes the two.

Registry management IR is an incremental patch. Rust applies it through the
same local-first registry overlay used for lookup, sweeps unreachable images
from that overlay, validates the whole resulting graph, and only then publishes
new graph and binding snapshots.

Patch operations:

```text
NewImage {
  id,
  format,
  layout,
  role: Primitive | Derived(GraphCommand),
}

SetPrimitive(id)
SetDerived(id, GraphCommand)
SetRoot(id)
```

There is no `RemoveImage` operation. Removing an image means removing it from
all root-reachable derived commands. Rust then garbage-collects unreachable
images from the overlaid graph and binding table.

Patch semantics:

- `NewImage(..., Primitive)` allocates a full empty image with valid empty tile
  keys via `GlaImages` and `Tiles`.
- `NewImage(..., Derived(command))` allocates a derived cache image whose slots
  start as `TileKey::INVALID`.
- `SetDerived` on an existing derived image replaces its graph command. The new
  cache image starts invalid. If the replaced image is the current root, its old
  cache key is retained by the inverse patch as a root presentation cache;
  otherwise the old derived cache key is released after the patch publishes.
- `SetDerived` on an existing primitive image changes the role by allocating a
  new invalid derived cache image. The inverse patch retains the old primitive
  image key.
- `SetPrimitive` on an existing derived image is flatten/materialize. The
  current implementation requires the derived image to be fully materialized,
  then reclassifies that image key as the primitive backing image with the same
  format and layout. If the converted image is the current root, undo restores
  the same key as the root derived cache.
- `SetPrimitive` on an existing primitive image is a no-op unless other metadata
  changes.
- `SetRoot` changes the graph root. Removing the current root is forbidden; use
  `SetRoot`.

No-op patch items are skipped by comparing only the locally patched
declaration, graph command, or root value. Command equality is exact structural
equality, including read order.

Registry patches are conservative for repaint. A patch item that changes an
image declaration, derived command, or root role marks that image as full dirty.
For root changes, before and after dirty sources may differ.

```text
RegistryRecord {
  graph_before: RegistryGraphKey,
  graph_after: RegistryGraphKey,
  bindings_before: ImageBindingTableKey,
  bindings_after: ImageBindingTableKey,
  changed_before: [ImageId],  // each means Full in graph_before
  changed_after: [ImageId],   // each means Full in graph_after
  root_cache_before: ImageKey,
  root_cache_after: ImageKey,
}
```

Examples:

```text
SetDerived(group_cache, new_command):
  changed_before = [group_cache]
  changed_after  = [group_cache]

SetRoot(new_root):
  changed_before = [old_root]
  changed_after  = [new_root]

Move layer from group A to group B:
  SetDerived(group_a_cache, command_without_layer)
  SetDerived(group_b_cache, command_with_layer)
  changed_before = [group_a_cache, group_b_cache]
  changed_after  = [group_a_cache, group_b_cache]
```

New empty primitive images are not dirty sources by themselves. If adding a
layer changes a group command, the group cache is the full-dirty source.

Undo and redo restore graph and binding snapshots from records. They do not
re-execute registry patch commands.

## Draw Session IR

A draw session is a static per-stroke program. Runtime input samples are pushed
by the Rust app loop frame by frame; they are not stored in the static IR.

```text
DrawSessionIR {
  expected_document_version: DocumentVersionId,
  doc_images: [DocImageUse],
  session_images: [SessionImageDecl],
  draw_on: [DrawOnCommand],
  derive: [DeriveCommand],
}

DocImageUse {
  id: ImageId,
  access: Read | ReadWrite,
}
```

`ReadWrite` requires the doc image to be a `PrimitiveImage`. Derived document
images may be declared only as `Read`; their writes come from the registry graph,
not from draw IR.

`session_images` share one namespace for the whole draw session:

```text
SessionImageDecl =
  Primitive { id, format, layout }
  Derived { id, format, layout, command: SessionCommand }
```

Primitive session images start as full empty valid images. Derived session
images may start with `TileKey::INVALID` and have one local session command.
Session images are released at draw session end and do not enter the document
binding table.

`format` and `layout` may be explicit or `Like(image)`. `Like` may reference
only an image that already exists and already has concrete metadata at the point
of declaration: a doc image, or an earlier session image declaration. Session
image declarations are resolved in order; the initializer does not topologically
sort `Like` dependencies. After initialization, every session image has concrete
format and layout metadata.

A session image id must not also be declared as a `ReadWrite` document image in
the same draw session. A session image may still shadow a read-only document id
in tool-local current lookup. Commit and cleanup use the IR declarations rather
than id equality.

Plain image references and backup image references are distinct at read sites:

```text
image        // current image in the current evaluation context
image.backup // stroke-start doc image key, read-only
```

`.backup` is allowed only for doc images declared in `doc_images`. It is
explicit at each read site. It is not a hidden access mode.

## DrawOn And Derive

The executor has two image modification forms:

```text
DrawOn:
  input-driven
  ordered by source invocation
  may mutate or accumulate into one ReadWrite destination
  has no image read edges in the first design stage

Derive:
  dirty-driven
  unordered per destination tile
  full-overwrite for affected destination tiles
  must not read destination current
  may read destination backup
```

Direct drawing and pigment-then-merge drawing use the same machinery:

```text
Direct:
  DrawOn(dst = doc base_paint)

Pigment merge:
  DrawOn(dst = session pigment)
  Derive(reads = [base_paint.backup, pigment], dst = doc base_paint)
```

One image may have only one writer in the merged current session graph. A doc
image cannot be both a `DrawOn` destination and a `Derive` destination in the
same draw session. Multiple dabs are multiple invocations of the same DrawOn
writer, not multiple writers.

Local/session derived commands may write session images or doc images declared
`ReadWrite`. They do not change the document registry graph. They are a
session-scoped way to produce the current value of a writable object.

Dirty-driven command reads are ordered. The read array index is the pipeline
port for the first implementation stage:

```text
GraphCommand {
  reads: [GraphRead],
  op: OpId,
  params: OpParams,
}

GraphRead {
  image: ImageId,
  mapping: Mapping,
  modifier: FootprintModifier,
}

SessionCommand {
  reads: [SessionRead],
  op: OpId,
  params: OpParams,
}

SessionRead {
  image: ImageId | ImageId.backup,
  mapping: Mapping,
  modifier: FootprintModifier,
}

DeriveCommand {
  dst: ImageId,
  command: SessionCommand,
}

DrawOnCommand {
  dst: ImageId,
  input_mapping: Mapping,
  op: OpId,
  params: OpParams,
}
```

Destinations are always plain current `ImageId`s. `.backup` is read-only syntax
and cannot appear as a command destination.

`GraphCommand` cannot express `.backup` reads. It belongs to the document graph
and is independent of any draw session. `SessionCommand` can express `.backup`
reads and appears only in session-local derived image declarations and explicit
draw-session `DeriveCommand`s.

`DrawOnCommand` is not a dirty-driven command body. It is an input-driven atomic
draw into one destination image. In the first design stage it has no read list;
source image reads for stamp/smudge-like drawing are a later design problem.

`OpId`s are compact Rust-side operation ids. Initial params can be strongly
typed Rust enums or structs; the graph semantics remain `op + params` so packed
parameter blocks can replace them later.

## Current, Backup, And Local Rows

The draw-session execution model uses binding tables plus role indexes:

```text
Doc binding:
  ImageId -> ImageKey

Doc role index:
  ImageKey -> Primitive | Derived(GraphCommand)

Local binding overlay:
  ImageId -> ImageKey

Local role index:
  ImageKey -> Primitive | Derived(DeriveCommand)
```

The doc binding and registry graph are document truth. The doc role index is a
derived lookup from the active graph and binding table. Local bindings shadow doc
bindings by id, while role lookup is direct by image key and does not shadow by
id. Current binding lookup is local-first, then doc. Backup lookup uses only the
session-start doc binding table. Backup evaluation never sees local rows, which
prevents a command such as:

```text
Merge(base_paint.backup, pigment) -> base_paint
```

from recursively resolving `base_paint` through its own current-session writer.

`DrawOn` is not an image role. It is a separate draw task with a resolved
destination key and decoded draw parameters. An image role has only two forms:

```text
Primitive
Derived(command)
```

In the doc role index, `Derived(command)` stores the persistent id-level
`GraphCommand`. In the local role index, `Derived(command)` stores a key-level
`gla_image_command::DeriveCommand`. The key-level command contains its
destination `ImageKey`, destination layout, and source `ImageRef`s inline. Each
`ImageRef` has a source `ImageKey`, source layout, mapping, and footprint
modifier; there is no separate read table at this layer.

Session start creates all required local shadows before lowering commands.
After shadows exist, graph/session IR is lowered once into key-level commands.
This guarantees downstream refs point at the current shadow keys rather than at
old document cache keys. Key-level commands are session execution artifacts and
are not persisted.

## Session Initialization

Draw session initialization performs the non-frame work up front:

```text
1. Check expected_document_version.
2. Bind graph_before and bindings_before.
3. Validate doc_images and session_images.
4. Capture old keys for declared doc images, supporting .backup.
5. Compute write starts from DrawOn and session derive destinations.
6. Upload write starts through registry readers to compute active images.
7. Create local shadows for every active image.
8. Allocate session-only images.
9. Lower graph/session IR through the completed local-first table.
10. Build render indices and draw tasks.
```

The active image closure starts at images written by draw IR and follows
registry readers up to the root. In the current model this is normally the
active layer to root cache chain.

Local shadow creation follows one document-image rule:

```text
doc Primitive or doc Derived:
  shallow COW the old image tile array
  record the source document ImageKey in the local entry
```

The first write to a COW shadow tile compares the shadow slot with the source
slot. If the slot still aliases the source, the session allocates a fresh tile,
replaces the shadow slot, copies the source tile when it is valid, and writes the
fresh tile. Later writes to that shadow tile are in-place writes.

Shadowed, COWed, active, and commit-replace candidate are the same condition for
document images. Session-only images are also local rows, but they are discarded
at session end unless a registry patch explicitly promotes them.

Derived shadows are local only until commit. They do not update the document
binding table during the session. This keeps cancel/error handling local to the
draw session.

## Coordinate Mapping And Footprints

Every image has its own coordinate system. Read edge mappings are the single
truth source for render read footprints and dirty upload.

Stage 1 mapping primitives:

```text
Mapping =
  Identity
  Matrix(Affine2D)   // dst coordinate -> source coordinate

FootprintModifier =
  None
  Expand(px)
```

For a command tile, the executor computes source tiles by taking destination
tile bounds, applying the read edge mapping and modifier, and conservatively
covering source tiles.

The first executable path supports `Identity + None` as a single same-index
source tile. `Identity + Expand(px)` and `Matrix(Affine2D)` remain explicit
implementation TODOs in the executor until layout-aware footprint enumeration
and renderer sampling semantics are wired.

Dirty upload uses the same edge in the reverse conservative direction. `Full`
means all tiles of the current image. Uploading `Full` through a mapping does
not necessarily produce `Full` in the destination image; the result is clipped
to destination bounds and may become a sparse set.

Draw input mapping is a different use of the same coordinate relationship:

```text
input_mapping:
  canvas/root input coordinate -> DrawOn destination image coordinate
```

DrawOn has no image read edges in the first design stage. Stamp, smudge, or
other source-reading draw primitives need a separate design.

## Tile Sets

Tile dirty sets are represented semantically as:

```text
TileSet =
  Full
  Tiles([...]) // normalized unique tile indices
```

The internal representation may later use rectangles or bitsets. IR semantics
only require `Full` and normalized sparse sets.

## Tile Slot Values

An image tile slot may contain:

```text
TileKey::INVALID
valid TileKey with empty binding
valid TileKey with physical binding
```

`TileKey::INVALID` means the tile has no effective content yet. It must be
materialized by running the image's writer before it can be used as a read
operand.

A valid key with an empty binding is effective zero content. It is a complete
value and does not require physical allocation.

Primitive images, document or session-local, must never contain
`TileKey::INVALID`. Derived images and session-local derived images may contain
`TileKey::INVALID`.

Materializing an invalid cache tile does not mark dirty and does not trigger
image-level COW. It is cache residency, not effective content change. The new
tile key belongs to the owner `ImageKey` and is released when that image is
released.

Repairing an unshadowed document derived cache is allowed to write the existing
document cache `ImageKey` directly. It uses that image's existing graph command
and existing document binding, does not enter history, and is not
undone or redone. This is equivalent to completing previously deferred cache
work for the same document state.

Dirty-driven rewrites and DrawOn writes are effective content changes. For
document images, they write only to local shadow keys created during session
initialization.

## Dirty, Demand Render, And Cache Repair

Dirty has exactly one meaning: effective image content changed. `DrawOn` writes
and session derived rewrites mark dirty. Materializing an invalid cache tile
does not mark dirty.

The frame loop uploads dirty from the written image toward root to compute root
repaint demand. The repaint then renders from root downward by demand:

```text
render(image_key, tile_index):
  if tile is valid:
    return tile key
  if image row is Primitive:
    error
  render the row's Derived command for tile_index
  return the now-valid tile key
```

`gla_image_command::DeriveCommand` is an ordered list of key-level ops such as
`Clear`, `Copy`, and `RenderTo`, plus the destination `ImageKey` and destination
layout. Ops that read source images carry their own inline `ImageRef`. During
execution, the command asks the session for its writable destination tile and
asks `render(image_ref.key, source_tile_index)` before using each returned source
tile. This keeps recursive dependency rendering in pre-order: a parent tile is
written only after every required source tile has been materialized.

`render` is not a renderer call. It is the recursive materialization entry for
read dependencies. The same execution context also exposes tile-key acquisition
and a `gla_renderer` pass queue. After `render` returns a readable `TileKey`,
the current command resolves source and destination tile positions and appends
`Clear`, `Copy`, or `RenderTo` tile passes directly to the renderer.

Because `render` may inspect the same derived-command binding table that led to
the current command, session code clones the command before entering
`exec_tile`. The command is then owned by the stack frame, and recursive lookups
do not alias a borrowed table entry.

There are two cache materialization paths:

```text
local/shadowed derived:
  write the local shadow cache key
  publish it at commit if the session commits

unshadowed doc derived:
  write the existing doc cache key
  do not record history
  do not undo/redo
```

The unshadowed path is cache repair for an unchanged document state. It uses
the existing document key and existing graph command, so filling an invalid tile
is equivalent to completing deferred work from an earlier session.

Stage 1 does not try to tree-shake dirty through visibility, opacity, or GPU
content checks.

## Source Work And Frames

The frame loop accepts source input groups and invokes DrawOn commands. Frame
budgeting gates only DrawOn/input acceptance. The budget cost model is
replaceable.

Once a source group is accepted:

```text
1. DrawOn invocations run and mark dirty.
2. All downstream local derive and registry derive/render work is drained.
3. Root repaint commands for the resulting dirty are submitted.
```

Accepted source groups are atomic. The session does not leave partially drained
derive/render work for the next frame.

## Image And Tile Resource Cleanup

Tile write paths do not record discarded tile keys. Tile-level code should not
know whether a tile belongs to primitive history, derived cache, root cache, or
session scratch.

Resource cleanup happens at image/session commit time.

The core invariant is that long-lived images do not share tile ownership across
different ids. If the same content must appear in multiple places, it should be
rendered into multiple derived images rather than sharing tile keys as a
long-lived ownership model.

For a replaced non-root derived image, cleanup compares the old and new image of
the same id by tile index:

```text
for each tile index i:
  if new.tiles[i] == INVALID and old.tiles[i] is valid:
    new.tiles[i] = old.tiles[i]
  if old.tiles[i] != new.tiles[i]:
    release old.tiles[i]
  else:
    keep it, because the new image still references it
```

If format or layout changed, there is no same-index successor relationship.
Primitive old images are still history truth and follow the record lifetime.
Non-root derived old images are cache and may be released once a valid
replacement is published.

Registry patches retain primitive image keys needed to restore document truth.
They also retain the derived cache key for the root image as a presentation
cache accelerator across undo/redo. Other derived images are not history-owned;
when they are replaced or swept, their cache keys and valid tile keys may be
released and later rebuilt from their graph commands.

Session images are released at draw session end.

## Document Records

`gla_doc` is a document model. It records committed model transitions, but it
does not execute draw sessions. `gla_session` builds and executes local state,
then commits an `ImageId -> ImageKey` binding patch and dirty sources into
`gla_doc`.

Draw-session commit is an id-key patch. It does not modify registry IR or the
doc role source. `gla_doc` applies the patch to the active binding table and
stores the before/after binding snapshots in the record. For a primitive
document image, the committed key fully replaces the previous key for that
`ImageId`. For a derived document image, the session first backfills any
`TileKey::INVALID` slots in the new cache from the old cache where the old tile
is valid, then replaces the binding. The derived role remains the existing
`GraphCommand` in the registry graph.

```text
DocRecord =
  DrawRecord {
    graph: RegistryGraphKey,
    bindings_before: ImageBindingTableKey,
    bindings_after: ImageBindingTableKey,
    doc_dirty: [(ImageId, TileSet)],
    root_cache_before: ImageKey,
    root_cache_after: ImageKey,
  }

  RegistryRecord {
    graph_before: RegistryGraphKey,
    graph_after: RegistryGraphKey,
    bindings_before: ImageBindingTableKey,
    bindings_after: ImageBindingTableKey,
    changed_before: [ImageId],
    changed_after: [ImageId],
    root_cache_before: ImageKey,
    root_cache_after: ImageKey,
  }
```

`DrawRecord.doc_dirty` records non-empty dirty sets for doc images declared
`ReadWrite` by the draw session. It does not record session-local dirty or
downstream registry derived dirty.

A draw session with empty `doc_dirty` may still commit active-chain binding
replacements and bump the document version. Empty `doc_dirty` means there is no
direct document dirty source to upload for repaint; it is not a no-record
sentinel.

`RegistryRecord.changed_before` and `changed_after` are full-dirty source image
ids. Registry records do not store tile sets because registry changes are
conservatively full dirty at their source images.

`root_cache_before` and `root_cache_after` are presentation accelerators for
fast undo/redo preview. They are not the document truth source. The truth source
is the graph and binding snapshots.

If the document root is a primitive image and a draw session writes that root
directly, the record intentionally degenerates: `bindings_before/after` already
contain the root image keys, and `root_cache_before/after` may be the same keys.
This redundancy keeps undo/redo handling uniform with the derived-root case.

Undo and redo restore snapshots from records and repaint by uploading
`doc_dirty` or `changed_*` through the restored graph. They do not replay DrawOn
input and do not re-execute registry patch commands.

## Example

A document tree may compile to this registry:

```text
PrimitiveImage:
  bg
  shadow
  base_paint
  detail
  lines
  overlay
  paint_mask

DerivedImage:
  paint_group_cache =
    RenderPaintGroup(base_paint, detail, paint_mask)

  character_cache =
    RenderCharacterGroup(shadow, paint_group_cache, lines)

  root_image =
    RenderRoot(bg, character_cache, overlay)
```

A watercolor draw session can declare:

```text
doc_images:
  base_paint: ReadWrite

session_images:
  stroke_coverage  Primitive D1
  stroke_wetness   Primitive D1
  soft_coverage    Derived D1
  edge_darkening   Derived D1
  settled_pigment  Derived D4

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

When `base_paint` becomes dirty, the registry continues the chain:

```text
base_paint
  -> paint_group_cache
  -> character_cache
  -> root_image
```

The draw IR does not include those document render commands; they are already in
the registry graph.
