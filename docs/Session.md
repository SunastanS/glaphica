# Session

This document defines the image-level draw session model.

The upper management layer owns the document tree, layer semantics, UI tool
names, and document edits. Rust owns the compiled image graph, image bindings,
resource keys, and draw-session execution. A session works only with images,
image commands, tile ranges, and tile resources.

## Document State

Janet-visible IR names images with compact opaque `ImageId`s. `ImageId` is
business-layer identity. Image storage does not know `ImageId`.

The current Rust document model is:

```text
Document {
  root: ImageId,
  roles: ImageId -> ImageRole,
  bindings: ImageId -> GlaImageKey,
  version: DocumentVersionId,
}

ImageRole =
  Primitive
  Derived(GraphCommand)
```

`Primitive` is editable or externally supplied document content. It has no graph
command and may be declared `ReadWrite` by a draw session.

`Derived(GraphCommand)` is document cache or output content. It has exactly one
graph command, is not a direct DrawOn target, and may contain
`TileKey::INVALID` cache slots.

The graph has exactly one root. Every image reachable from the root must have a
binding, and every binding must refer to a declared image id. Graph validation
also rejects missing reads, cycles, and graph commands that read their own
destination current image.

`gla_doc` is deliberately pure in this round: it validates and exposes the
document state and owns the version counter. Draw commit, draw history, and
undo/redo are owned by `gla_session`.

## Draw Session IR

A draw session is a static per-stroke program. Runtime input samples are pushed
by the app loop frame by frame; they are not stored in the static IR.

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

`ReadWrite` requires the doc image to be `Primitive`. Derived document images
may be read by IR and may be shadowed as active-chain caches, but they cannot be
explicit DrawOn or session-derive destinations.

Session images share one namespace for the whole draw session:

```text
SessionImageDecl =
  Primitive { id, format, layout }
  Derived { id, format, layout, command: SessionCommand }
```

Session image declarations are private to the draw session. They may shadow a
read-only document id in current lookup, but they must not reuse an id declared
as a `ReadWrite` document image in the same session.

`format` and `layout` may be explicit or `Like(image)`. `Like` may reference
only an image that already exists and already has concrete metadata at the point
of declaration: a doc image or an earlier session image declaration.

## Current And Backup Reads

Plain image references and backup image references are distinct at read sites:

```text
image         // current image in the current evaluation context
image.backup  // stroke-start doc image key, read-only
```

Current lookup is local-first, then document. Backup lookup uses only the
session-start document bindings and never sees session-local rows. This prevents
a command such as:

```text
Merge(base_paint.backup, pigment) -> base_paint
```

from recursively resolving `base_paint` through its own current-session writer.

`.backup` is allowed only for doc images declared in `doc_images`. It is
explicit at each read site and cannot appear as a command destination.

## Session Key Space

Image command execution uses a key wrapper:

```text
SessionImageKey =
  Doc(GlaImageKey)
  Local(GlaLocalImageKey)
```

`Doc` keys point at existing document image rows. `Local` keys point at rows in
the draw session's local table:

```text
SessionImage =
  Raw { format, layout, tiles }
  Edit { format, layout, source, edits }
```

`Raw` is used for session-created private images. Both session primitive and
session derived declarations allocate `Raw` rows with full valid empty tile
keys. This avoids a hot-path branch for local invalid tiles; unwritten local
tiles are still valid zero content.

`Edit` is used for document shadows. It contains a source `GlaImageKey` and a
sorted list of modified tile slots:

```text
ImageEdit {
  source: GlaImageKey,
  edits: Vec<(u32, TileKey)>,
}
```

Reads check `edits` first and fall back to `source`. Writes check `edits`
first; if the tile has not been touched, the session allocates a new tile,
copies the source tile when the source is valid, records the new tile in
`edits`, and writes in place from then on.

If the source tile is `TileKey::INVALID`, the first-write copy is skipped. This
is correct only because derived commands fully overwrite destination tiles and a
doc derived image is not shadowed as a DrawOn primitive. If those invariants
change, this write path must materialize or initialize the target differently.

## Session Initialization

Draw session initialization performs the non-frame work up front:

```text
1. Check expected_document_version.
2. Snapshot document roles and bindings.
3. Validate doc_images and session_images.
4. Resolve session image metadata.
5. Compute write starts from DrawOn destinations, session derived declarations,
   and explicit session Derive destinations.
6. Walk document graph readers from those write starts to the root to find the
   active document chain.
7. Create an Edit shadow for every active document image.
8. Allocate Raw rows for session-only images.
9. Lower graph and session commands through the completed local-first table into
   DeriveCommand<SessionImageKey>.
10. Build DrawOn tasks and frame-dirty slots.
```

The active-chain shadows are session-local. They do not enter `GlaImages` as new
image rows and they do not change document bindings during the session. Cancel
or error handling can therefore discard the whole session without publishing a
new document version.

## DrawOn And Derive

The executor has two image modification forms:

```text
DrawOn:
  input-driven
  ordered by source invocation
  may mutate or accumulate into one writable destination
  has no image read edges in the first design stage

Derive:
  dirty-driven
  unordered per destination tile
  full-overwrite for affected destination tiles
  must not read destination current
  may read destination backup
```

One image may have only one writer in the merged current session graph. A doc
image cannot be both a `DrawOn` destination and a session `Derive` destination
in the same draw session. Multiple dabs are multiple invocations of the same
DrawOn writer, not multiple writers.

Local/session derived commands may write session images or doc images declared
`ReadWrite`. They do not change the document graph. They are a session-scoped
way to produce the current value of a writable object.

## Coordinate Mapping And Dirty Upload

Every image has its own coordinate system. Read edge mappings are the single
truth source for render read footprints and dirty upload.

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
covering source tiles. The first executable source-footprint path supports
`Identity + None` precisely. Expanded and matrix source footprints remain TODOs
until layout-aware footprint enumeration and renderer sampling semantics are
wired.

Dirty upload uses the same edge in the reverse conservative direction. In this
document "upload" means moving dirty information from the written image toward
the root. It does not mutate tile resources or copy tile content; it computes
destination `TileSet` values and triggers recursive rendering.

For a frame, dirty is first collected per `DrawOn` command. On flush, each
DrawOn dirty set is uploaded through the graph and session command edges. The
session records document dirty for `ReadWrite` document ids and unions root
demand for repaint. The implementation intentionally keeps upload simple: each
DrawOn dirty set is uploaded independently, and repeated paths are only unioned
at the destination records. Expanded and matrix dirty uploads currently fall
back to full destination dirty.

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

Primitive document images must never contain `TileKey::INVALID`. Derived
document images may contain `TileKey::INVALID`. Session-local `Raw` rows are
allocated with valid empty tile keys.

Materializing an invalid document derived cache tile does not mark dirty and
does not trigger history. It is cache residency for the same document state.

## Demand Render And Cache Trust

`render` is the recursive materialization entry for read dependencies. It is not
a renderer call by itself:

```text
render(image_key, tile_index):
  if local key has a command:
    execute the local command for tile_index
    return the local tile key

  if doc primitive:
    return the document tile key

  if doc derived tile is valid:
    return the document tile key

  if doc derived tile is INVALID:
    execute the document graph command for tile_index
    return the now-valid document tile key
```

Document derived cache is trusted: if a doc derived tile has a valid tile key,
it can be used directly.

Session-local shadows are not trusted just because they have a valid tile key.
If a local key has a command, `render` executes that command on demand even when
the local slot is already valid. This keeps CoW resource sharing out of command
semantics and is correct because local shadows represent current session output.
The tradeoff is possible extra passes for expanded or matrix mappings until
local derived caching becomes more precise.

Unshadowed document derived repair may write directly into the existing document
cache key. This does not enter history and is not undone or redone.

## Frame Budget And Frame Flush

`FrameBudget` is a separate small module. The current implementation counts
accepted dabs:

```text
FrameBudget::new(max_dabs)
try_accept_dab()
accepted()
```

The app loop owns when to start a frame, how to combine time and work budgets,
and whether to stop accepting input for the current frame. `DrawFrame` owns the
frame dirty state and the dab pass buffer. A typical frame is:

```text
1. App loop accepts input samples under FrameBudget.
2. For each accepted dab, DrawFrame invokes DrawOn and records frame dirty.
3. DrawFrame::flush uploads each DrawOn dirty set toward root.
4. DrawFrame::flush renders root demand, recursively materializing dependencies.
5. DrawFrame::flush submits dab and derived passes to the render backend.
```

If `DrawFrame::flush` sees no frame dirty, it returns without rendering. In normal
use there is no app loop while nothing is being drawn; this path mostly covers
stationary stylus or timing-edge cases.

## Commit

A draw session commit consumes the session:

```text
DrawSession::commit(doc, history) -> Option<DrawCommit>
```

Commit requires that the document version still matches the version captured at
session start. Frame work must already have been submitted through
`DrawFrame::flush`; commit does not submit GPU work. Commit gathers `ImageEdit`s
from document shadows and returns `None` when there is no edit to publish.

Primitive document edits are applied in place to the current
`GlobalImage::Primitive(DenseImage)` for each `ImageId`. Before applying,
commit validates edit tile indices and records the old primitive tile owners as
an inverse `ImageEdit`.

Derived document edits are cache publication. Commit validates the edited tile
indices, writes the new tile keys into the currently bound derived cache image,
and releases replaced valid cache tiles. Derived cache edits are not stored in
draw history.

After successful application, commit bumps the document version, stores the
primitive inverse patch in `DrawHistory`, discards remaining session-local
tiles, and returns `Some(DrawCommit { record_id, version })`.

Document bindings are not changed by draw commit. The `ImageEdit.source` field
is retained for now, but the actual target row is resolved from the current
document binding under the checked document version.

## Draw History, Undo, And Redo

`DrawHistory` stores primitive image inverse patches:

```text
StoredImageEditPatch {
  version: DocumentVersionId,
  edits: ImageId -> ImageEdit,
}
```

Applying a stored patch checks the expected document version, applies its
primitive tile replacements in place, bumps the document version, and stores the
inverse as a new record. Undo and redo are therefore the same operation applied
to opposite records.

Undo and redo do not replay DrawOn input and do not re-execute session commands.
They also do not restore derived document cache tiles from history. Derived
caches may be repaired later by normal demand render from the document graph.

## Resource Cleanup

Tile write paths do not own history semantics. They only allocate or acquire
tiles.

Resource cleanup happens at session commit/discard time:

- committed primitive replacement tiles become document-owned through the
  existing image slots;
- old primitive tiles are retained by the inverse `ImageEdit` stored in
  `DrawHistory`;
- committed derived replacement tiles become cache-owned through the existing
  derived image slots;
- replaced valid derived cache tiles are discarded immediately;
- uncommitted session `Raw` tiles are discarded;
- uncommitted `Edit` tiles are discarded.

If a commit fails validation, the minimum fallback is to discard the whole
session. Without a version bump and history record, document truth is not
published.

## Example

A document tree may compile to this image graph:

```text
Primitive:
  bg
  base_paint
  detail
  lines

Derived:
  paint_group_cache =
    RenderPaintGroup(base_paint, detail)

  root_image =
    RenderRoot(bg, paint_group_cache, lines)
```

A watercolor draw session can declare:

```text
doc_images:
  base_paint: ReadWrite

session_images:
  stroke_coverage  Primitive D1 layout Like(base_paint)
  stroke_wetness   Primitive D1 layout Like(base_paint)
  soft_coverage    Derived D1 layout Like(base_paint)
  settled_pigment  Derived D4 layout Like(base_paint)

draw_on:
  DrawRadialKernel1D -> stroke_coverage
  DrawRadialKernel1D -> stroke_wetness

derive:
  BlurCoverage(stroke_coverage) -> soft_coverage
  SettlePigment(soft_coverage, stroke_wetness) -> settled_pigment
  MergeWatercolor(base_paint.backup, settled_pigment) -> base_paint
```

Dirty starts at the two DrawOn destinations, uploads through session derive
edges to `base_paint`, and then through document graph edges:

```text
base_paint -> paint_group_cache -> root_image
```

The draw IR does not include document graph render commands; they are already in
the document roles. The commit records only the primitive tile replacements for
`base_paint` in `DrawHistory`.
