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

Janet-visible IR names images with compact opaque `ImageId`s. Display names and
UI layer names are outside the executor identity model.

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
image id in the graph. The root is not stored in the binding table; the binding
table only answers which `ImageKey` currently backs an `ImageId`.

Doc-level `ImageId`s are never reused within one document lifetime. Session-local
images use the same `ImageId` type and may shadow doc ids within one draw
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
  build_command,
}
```

`PrimitiveImage` is editable or externally supplied image content. It has no
registry build command and may be declared `ReadWrite` by a draw session.

`DerivedImage` is a document-level cache or output image. It has exactly one
build command and is not a direct draw target. The single build-command rule is
intentional: a group cache must not have different update programs depending on
which child layer was edited.

Registry validation derives:

```text
writer_of: ImageId -> BuildCommandIndex
readers_by_image: ImageId -> [BuildCommandIndex]
topo_order: [BuildCommandIndex]
```

Validation rules:

- the graph has exactly one root;
- every remaining graph image is reachable from the root by walking derived
  command reads backwards from the root;
- every derived image has exactly one build command;
- primitive images have no build command;
- all build command reads reference registered document images;
- build commands form an acyclic image dependency graph;
- a build command writes exactly one derived image;
- a build command does not read its destination current image;
- each read edge has a coordinate mapping.

A command may read the same image multiple times with different ports or
mappings. Dirty fanout registers the command once per read image and unions the
affected destination tiles from every matching read edge.

## Registry Patch Sessions

A session task is either drawing work or registry management work. A single
session never mixes the two.

Registry management IR is an incremental patch. Rust applies it to a staging
copy, sweeps unreachable images, validates the whole resulting graph, and only
then publishes new graph and binding snapshots.

Patch operations:

```text
NewImage {
  id,
  format,
  layout,
  role: Primitive | Derived(build_command),
}

SetPrimitive(id)
SetDerived(id, build_command)
SetRoot(id)
```

There is no `RemoveImage` operation. Removing an image means removing it from
all root-reachable derived commands. Rust then garbage-collects unreachable
images from the staging graph and binding table.

Patch semantics:

- `NewImage(..., Primitive)` creates a full empty image with valid empty tile
  keys.
- `NewImage(..., Derived(cmd))` creates a derived cache image whose slots may
  start as `TileKey::INVALID`.
- `SetDerived` on an existing derived image replaces its command if the command
  is structurally different. The new cache image starts invalid.
- `SetDerived` on an existing primitive image is forbidden in normal patch
  execution.
- `SetPrimitive` on an existing derived image is flatten/materialize: first
  materialize the old derived image under the old graph, then make that
  materialized image the primitive backing image with the same format and
  layout. The implementation may reuse the materialized derived `ImageKey`
  directly. It does not need to copy every tile into a second image key unless
  another resource constraint requires a new backing image.
- `SetPrimitive` on an existing primitive image is a no-op unless other metadata
  changes.
- `SetRoot` changes the graph root. Removing the current root is forbidden; use
  `SetRoot`.

No-op patch items are skipped by comparing only the locally patched
declaration, command, or root value. Command equality is exact structural
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
  Derived { id, format, layout, build_command }
```

Primitive session images start as full empty valid images. Derived session
images may start with `TileKey::INVALID` and have one local build command.
Session images are released at draw session end and do not enter the document
binding table or session record.

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
  may mutate or accumulate
  may read destination current if the primitive supports one GPU pass

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

Command reads are ordered. The read array index is the pipeline port for the
first implementation stage:

```text
DeriveCommand {
  reads: [ImageRead],
  dst: ImageId,
  op: OpId,
  params: OpParams,
}

DrawOnCommand {
  reads: [ImageRead],
  dst: ImageId,
  input_mapping: Mapping,
  op: OpId,
  params: OpParams,
}

ImageRead {
  image: ImageId | ImageId.backup,
  mapping: Mapping,
  modifier: FootprintModifier,
}
```

Destinations are always plain current `ImageId`s. `.backup` is read-only syntax
and cannot appear as a command destination.

`OpId`s are compact Rust-side operation ids. Initial params can be strongly
typed Rust enums or structs; the graph semantics remain `op + params` so packed
parameter blocks can replace them later.

## Current And Backup Evaluation

Draw sessions use two evaluation contexts.

Current evaluation:

```text
tool/local command image lookup:
  local current table first
  then doc current table

registry build command image lookup:
  doc current table only

derive writer lookup:
  local/session derive table first
  then document registry graph
```

Backup evaluation:

```text
image key lookup:
  session-start document binding table only

derive writer lookup:
  document registry graph only
```

Backup evaluation does not see session-local images or session-local derived
commands. This prevents a tool command such as:

```text
Merge(base_paint.backup, pigment) -> base_paint
```

from recursively resolving `base_paint` through its own current-session writer.

Draw sessions keep three conceptual tables:

```text
doc_start:
  session-start document bindings, used by .backup

doc_current:
  current document bindings after pre-COW

local:
  session-created images and their lifecycle ownership
```

An implementation may also materialize a merged current lookup table for
tool-local commands. In that case, local image keys can appear both in the
merged current table and in the local table. This is not a commit ambiguity:
only `ReadWrite` document declarations and the computed doc write closure update
`bindings_after`.

Registry build commands always resolve document images through `doc_current`
and never through the local table or a merged local-first current table.

The session-start document table is the `bindings_before` snapshot. Lookup and
commit decisions do not rely on id equality or id shadowing. They use the IR
declarations and the computed write closure.

Draw session validation builds a normalized key-level current dependency graph
for derive commands:

```text
edge = current_read_image_key -> dst_image_key
```

Backup reads are excluded. DrawOn commands are not cache-repair writers and do
not enter this derive cycle graph. The graph includes local/session derive
commands and the reachable registry derived commands under local writer overlay
priority.

Local derive commands may shadow only session images and doc images declared
`ReadWrite`. They may not shadow a doc-level derived image's registry writer.

## Session Initialization

Draw session initialization performs the non-frame work up front:

```text
1. Check expected_document_version.
2. Bind graph_before and bindings_before.
3. Validate doc_images and session_images.
4. Capture old keys for declared doc images, supporting .backup.
5. Compute the doc write closure.
6. Pre-COW every doc image in that write closure once.
7. Allocate session images.
8. Build doc current, local, writer indices, reader indices, and topo order.
```

The write closure starts at doc images that can be written by draw IR and then
follows registry readers up to the root. In the current model this is normally
the active layer to root cache chain. Pre-COW is whole-image shallow copy: the
new image initially has the same tile key array as the old image.

Pre-COW avoids hot-path "has this image been COW'd" branches. If a pre-COW image
does not receive dirty writes by commit time, its binding is reverted and the
unused new image is released.

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

Dirty upload uses the same edge in the reverse conservative direction. `Full`
means all tiles of the current image. Uploading `Full` through a mapping does
not necessarily produce `Full` in the destination image; the result is clipped
to destination bounds and may become a sparse set.

Draw input mapping is a different use of the same coordinate relationship:

```text
input_mapping:
  canvas/root input coordinate -> DrawOn destination image coordinate
```

If a DrawOn primitive reads another image, that read still uses a normal read
edge mapping from DrawOn destination space to read image space.

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

Dirty-driven rewrites and DrawOn writes are effective content changes. They
write only to pre-COW current image keys for doc images.

## Dirty And Execution

Dirty has exactly one meaning: image content changed.

```text
mark_dirty(image, tiles):
  for reader in readers_by_image[image]:
    dst_tiles = reader.affected_dst(image, tiles)
    pending_by_command[reader] += dst_tiles
```

Pending work is stored at image-command granularity:

```text
pending_by_command: CommandIndex -> TileSet
```

A pending item is only `(command, dst_tile)`. It does not cache read tile keys,
read positions, or lowered tile commands.

Processing a command tile:

```text
process(command, dst_tile):
  consume pending_by_command[command][dst_tile] if present
  resolve every read tile in the command's evaluation context
  acquire or create the destination tile key
  lower and execute the tile command
  mark_dirty(command.dst, dst_tile)
```

When resolving a read tile:

- a valid tile can be read;
- an `INVALID` tile is materialized by running its writer in the current
  evaluation context;
- reading an `INVALID` primitive image tile is an error.

Current dirty upload to root is the root repaint demand. Stage 1 does not try to
tree-shake dirty through visibility, opacity, or GPU content checks.

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
  if old.tiles[i] != new.tiles[i]:
    release old.tiles[i]
  else:
    keep it, because the new image still references it
```

If format or layout changed, there is no same-index successor relationship; a
non-root derived old image is released as cache. Primitive old images are still
history truth and follow the record lifetime.

Primitive old images are document truth and are retained through
`bindings_before`. Non-root derived old images are cache and are released at
session end by the diff rule. The root old image is stored as a presentation
cache accelerator in the session record.

Session images are released at draw session end.

## Session Records

Session records are an enum because draw and registry sessions have different
semantics.

```text
SessionRecord =
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

If a draw session produces no `doc_dirty`, it produces no record and active
bindings remain unchanged.

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
