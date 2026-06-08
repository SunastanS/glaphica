# Data Model Notes

The upper management layer owns the document tree. It may contain concepts such
as groups, layers, masks, filters, blend modes, transforms, and UI names.

Rust sessions do not execute against that tree directly. The document tree is
compiled into image-level state: a registry of image roles plus an image-key
binding table.

## Ids And Keys

Janet-visible IR uses compact opaque `ImageId`s. `ImageId` is business-layer
identity. It is not an image storage key.

Rust stores resources behind generation-checked keys:

```text
ImageKey              // image tile array / content version
```

`ImageId -> ImageKey` binding tables are doc/session-layer state. Runtime role
lookup is keyed by `ImageKey`: `ImageKey -> Primitive | Derived(command)`. This
matches `render(image_key, tile_index)` and avoids reverse lookup from image key
back to image id. The image storage layer works in `ImageKey` and `TileKey`; the
image-command layer works in key-level executable commands.

Doc-level `ImageId`s are not reused during a document lifetime. Session-local
images use the same id type and may shadow doc ids within a draw session.

## Document Model

`gla_doc` holds the current document state inline:

```text
Document {
  root: ImageId,
  roles: ImageId -> ImageRole,
  bindings: ImageId -> ImageKey,
  version: DocumentVersionId,
}
```

`ImageRole = Primitive | Derived(GraphCommand)`. The graph has exactly one root.
The binding table must contain a key for every image id in roles, and every
binding must correspond to a declared role.

`gla_doc` supports two state transitions:

- `commit_draw(patch) -> inverse_patch` — replaces bindings for write
  targets, bumps version. Purely id-key level; does not touch roles.
- `apply_registry_patch(patch, images, tiles) -> inverse_patch` —
  applies registry ops through a local-first overlay, sweeps unreachable
  images from the overlaid graph, validates the whole result, publishes the
  graph and binding changes, and generates an inverse patch for undo.

## Patch Resource Invariant

Patch history owns resource keys only when they are needed to restore document
truth or root presentation cache:

- Primitive image keys replaced or swept by registry changes are retained by
  `InsertImage` inverse ops.
- The derived cache key for the current root image is retained by `InsertImage`
  inverse ops as undo/redo presentation cache.
- Non-root derived cache keys are not history-owned. When they are replaced or
  swept by a successfully published registry patch, their image key and valid
  tile keys are released; undo recreates them from graph commands when needed.

Draw patches are id-key binding patches. Their image keys represent document
content versions; primitive old keys and tile keys follow the stored patch
lifetime. No separate orphan-tracking data is needed for derived cache keys.

`gla_session` is the app-loop entry point. It owns draw-session execution state,
including the local key overlay, active-chain tracking, and key-level derive
commands used while executing a session.

## Image Declarations

The registry graph declares document images:

```text
ImageRole =
  Primitive
  Derived(GraphCommand)
```

`Primitive` is editable or externally supplied document image content. It
is a valid `ReadWrite` draw target, has no graph command, and never
contains `TileKey::INVALID`.

`Derived(GraphCommand)` is document cache or output image content. It has
exactly one graph command, is not a direct draw target, and may contain
`TileKey::INVALID`.

Examples:

```text
Layer image       -> Primitive
Group cache       -> Derived(GraphCommand)
Filter cache      -> Derived(GraphCommand)
Root display      -> Primitive or Derived(GraphCommand)
Mask image        -> Primitive or Derived(GraphCommand), depending on ownership
```

The graph has exactly one root image. The binding table must contain a binding
for every image id that remains in the graph after registry patch garbage
collection.

## Session Images

Draw sessions may declare session-local images:

```text
Session Primitive:
  Full empty valid image, released at session end.

Session Derived:
  Local cache with one SessionCommand, released at session end.
```

Session images do not enter the document binding table or undo records.

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

`DrawOnCommand` is a different IR form. It is an input-driven atomic draw into
one `ReadWrite` destination and has no read list in the first design stage.

Document truth stays in id-level IR. At draw-session start, `gla_session`
lowers id-level graph/session declarations through the active local-first image
tables into key-level image-command programs:

```text
GraphCommand + current table -> gla_image_command::DeriveCommand
SessionCommand + current/backup table -> gla_image_command::DeriveCommand
DrawOnCommand + writable target lookup -> Draw task
```

`gla_image_command::DeriveCommand` is not document truth and is not stored in
records. It holds the destination `ImageKey`, destination layout, and ordered
operations. Its operations hold key-level `ImageRef`s with source `ImageKey`,
source layout, mapping, and footprint metadata. When a session creates new cache
shadows, downstream commands are re-lowered so their refs point at the new keys.

At execution time, `gla_image_command` owns only key-level ordering. A command
uses a `RenderCtx` to request `render(image_key, tile_index)` for read
dependencies, asks the same context for the destination tile key, then appends
tile passes directly to `gla_renderer`. The context also owns tile-key
acquisition, so session code can keep renderer, tile resources, and recursive
command lookup in one reentrant state machine.

Session code must enter image commands with an owned command value, normally by
cloning the row's `DeriveCommand` before execution. This prevents a recursive
`render` call from borrowing the command binding table while the current command
is still borrowed from that table.

## Tile Resources

An image is an array of tile keys. Empty tile bindings are valid zero content.
`TileKey::INVALID` means a derived cache tile has not been built yet.

Tile write paths do not own history semantics. Session cleanup compares old and
new image tile arrays by index to release replaced cache resources. Primitive
history and root presentation caches are retained by history patches; other
derived cache resources can be released when replaced or swept.
Unshadowed derived cache repair may fill `TileKey::INVALID` slots in an
existing document cache key without creating history; it is cache residency for
the same graph and binding state, not document content change.
When a shadowed derived cache replaces an older cache at commit, any
`TileKey::INVALID` slots in the new cache are first backfilled from the old
cache if the old slot is valid.
