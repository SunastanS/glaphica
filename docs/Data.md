# Data Model Notes

The upper management layer owns the document tree. It may contain concepts such
as groups, layers, masks, filters, blend modes, transforms, and UI names.

Rust sessions do not execute against that tree directly. The document tree is
compiled into image-level state: document image roles, document image bindings,
and session-local execution rows.

## Ids And Keys

Janet-visible IR uses compact opaque `ImageId`s. `ImageId` is business-layer
identity. It is not an image storage key.

Rust stores resources behind generation-checked keys:

```text
GlaImageKey       // row in GlaImages: format, layout, tile slots
TileKey           // row in Tiles: atlas position or empty binding
GlaLocalImageKey  // row in one DrawSession local image table
```

Document bindings are `ImageId -> GlaImageKey`. A draw commit does not normally
replace these bindings. Instead it edits selected tile slots of the currently
bound document images.

Image command execution uses a session key wrapper:

```text
SessionImageKey =
  Doc(GlaImageKey)
  Local(GlaLocalImageKey)
```

`Doc` keys read and repair document image rows. `Local` keys address
session-owned raw images and document shadows. The image-command layer is
generic over this key type, so recursive render can use the same command code
for document rows and session-local rows.

Doc-level `ImageId`s are not reused during a document lifetime. Session-local
images use the same id type in IR and may shadow doc ids within a draw session;
after lowering, local rows live in an independent `GlaLocalImageKey` namespace.

## Document Model

`gla_doc` holds the current document state inline:

```text
Document {
  root: ImageId,
  roles: ImageId -> ImageRole,
  bindings: ImageId -> GlaImageKey,
  version: DocumentVersionId,
}
```

`ImageRole = Primitive | Derived(GraphCommand)`. The graph has exactly one root.
The binding table must contain a key for every image id in roles, and every
binding must correspond to a declared role.

In the current implementation `gla_doc` is intentionally small. It validates the
graph, exposes role and binding lookup, and owns the document version counter.
It does not own draw history, undo/redo, or draw-session commit logic.
`gla_session` applies draw commits to `GlaImages` and asks `Document` only to
bump the version after a successful commit.

## Image Rows And Tile Slots

An image is an array of tile keys. Empty tile bindings are valid zero content.
`TileKey::INVALID` means a cache tile has not been built yet.

Primitive document images must never contain `TileKey::INVALID`. Derived
document images may contain `TileKey::INVALID`; filling an invalid derived cache
tile is cache repair, not document content change.

Session-local raw images are allocated with full valid empty tile keys. This is
true for both session primitive declarations and session derived declarations.
The tradeoff is a few more tile keys in exchange for a simpler hot path: if a
session-local tile is written or read, it already has a valid key.

## Session Local Rows

Each draw session has a local image table:

```text
SessionImage =
  Raw {
    format,
    layout,
    tiles: Box<[TileKey]>,
  }

  Edit {
    format,
    layout,
    source: GlaImageKey,
    edits: Vec<(u32, TileKey)>,
  }
```

`Raw` is for session-created private images, such as brush coverage or pigment
images. `Raw` rows are discarded at session end and never enter document
bindings or history.

`Edit` is for document shadows. It records the source document image key and the
tile slots modified by the session. A read checks `edits` first and then falls
back to `source`. A write checks `edits` first; if the tile was not touched yet,
the session allocates a new tile key, copies the source tile when the source is
valid, inserts the edit, and writes into the new tile.

If the source tile is `TileKey::INVALID`, the copy pass is skipped. This is
correct under the current invariants:

- a document derived image is not shadowed as a DrawOn primitive target;
- derive commands fully overwrite their destination tiles.

If either invariant changes, first-write initialization for `Edit` rows must
become command-aware or materialize the source first.

## ImageEdit

Committed draw changes are represented as tile-level edits:

```text
ImageEdit {
  source: GlaImageKey,
  edits: Vec<(u32, TileKey)>,
}
```

`source` is retained for now as the image row the edit was derived from. The
commit path still applies edits by `ImageId` against the current document
binding, because document versioning is session-level and a draw commit does not
allocate a new document image key.

`edits` stores sorted unique tile-index replacements. Each pair means: replace
tile slot `u32` with `TileKey`.

For primitive document images, commit applies `ImageEdit` in place to the
currently bound `GlaImageKey`. The binding remains stable. Before mutation,
the session validates tile indices and records the old primitive tile keys in
an inverse `ImageEdit`. `DrawHistory` stores that inverse patch.

For derived document images shadowed by the active chain, commit also writes the
edited tile slots back to the currently bound cache image. These edits are cache
publication, not document truth history. Replaced valid cache tiles are released
when they are no longer referenced by the new slot.

Dirty does not need a separate history shape for draw undo. The edited tile
indices in `ImageEdit` are the dirty tile indices. `DrawSession::doc_dirty`
still exists as frame/session execution information for repaint demand, not as
the durable undo record.

## Draw History

`DrawHistory` lives in `gla_session`:

```text
DrawHistory {
  patches: DrawRecordId -> StoredImageEditPatch,
  next_id: DrawRecordId,
}

StoredImageEditPatch {
  version: DocumentVersionId,
  edits: ImageId -> ImageEdit,
}
```

Undo and redo both apply a stored `ImageEdit` patch to primitive document
images in place. Applying a patch returns a new inverse patch id, so redo is the
same operation as applying the inverse produced by undo. The operation checks
the document version before applying, bumps the version after applying, and does
not replay brush input or image commands.

Only primitive document edits enter draw history. Derived document caches can be
repaired or recomputed from their graph commands and are not history-owned in
this round.

## Command Layers

The command layers are deliberately separate:

```text
gla_ir:
  id-level cross-language declarations

gla_image_command:
  key-level image operation programs
```

`GraphCommand` and `SessionCommand` are IR-level declarations. Both describe
dirty-driven full-overwrite image commands. The only semantic difference is read
identity:

```text
GraphCommand:
  reads: [GraphRead]       // current document images only

SessionCommand:
  reads: [SessionRead]     // current image or backup document image
```

At draw-session start, `gla_session` lowers id-level graph/session declarations
through the completed local-first table into
`gla_image_command::DeriveCommand<SessionImageKey>`.

At execution time, `gla_image_command` owns only key-level ordering. A command
uses a `RenderCtx` to request `render(image_key, tile_index)` for read
dependencies, asks the same context for the destination tile key, then appends
tile passes directly to `gla_renderer`. The context owns tile-key acquisition,
so session code can keep renderer, tile resources, and recursive command lookup
in one reentrant state machine.

Session code enters image commands with an owned command value, normally by
cloning the row's `DeriveCommand` before execution. This prevents a recursive
`render` call from borrowing the command table while the current command is
still borrowed from that table.
