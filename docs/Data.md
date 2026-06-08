# Data Model Notes

The upper management layer owns the document tree. It may contain concepts such
as groups, layers, masks, filters, blend modes, transforms, and UI names.

Rust sessions do not execute against that tree directly. The document tree is
compiled into image-level state:

```text
ActiveDocumentState {
  graph: RegistryGraphKey,
  bindings: ImageBindingTableKey,
  version: DocumentVersionId,
}
```

## Ids And Keys

Janet-visible IR uses compact opaque `ImageId`s. `ImageId` is business-layer
identity. It is not an image storage key.

Rust stores resources behind generation-checked keys:

```text
RegistryGraphKey      // declarations, root, derived commands
ImageBindingTableKey  // ImageId -> ImageKey
ImageKey              // image tile array / content version
```

`ImageId -> ImageKey` binding tables are doc/session-layer state. Runtime role
lookup is keyed by `ImageKey`: `ImageKey -> Primitive | Derived(command)`. This
matches `render(image_key, tile_index)` and avoids reverse lookup from image key
back to image id. The image storage layer works in `ImageKey` and `TileKey`; the
image-command layer works in key-level executable commands.

Doc-level `ImageId`s are not reused during a document lifetime. Session-local
images use the same id type and may shadow doc ids within a draw session.

In code, `gla_doc` owns document graph snapshots, `ImageBindingTable`, active
document state, registry patch records, and undo/redo state transitions. Doc
image roles are persistent IR in the registry graph and can be indexed as
`ImageKey -> role` from the active graph and bindings.
`gla_session` is the app-loop entry point. It owns draw-session execution state,
including the local binding overlay and key-role index used while executing a
session.

## Image Declarations

The registry graph declares document images:

```text
ImageDeclaration =
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

`PrimitiveImage` is editable or externally supplied document image content. It
is a valid `ReadWrite` draw target, has no graph command, and never
contains `TileKey::INVALID`.

`DerivedImage` is document cache or output image content. It has exactly one
graph command, is not a direct draw target, and may contain `TileKey::INVALID`.

Examples:

```text
Layer image       -> PrimitiveImage
Group cache       -> DerivedImage
Filter cache      -> DerivedImage
Root display      -> PrimitiveImage or DerivedImage
Mask image        -> PrimitiveImage or DerivedImage, depending on ownership
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

tile command:
  tile-resource-level executable work
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
history and root presentation caches are retained by session records.
Unshadowed derived cache repair may fill `TileKey::INVALID` slots in an
existing document cache key without creating history; it is cache residency for
the same graph and binding state, not document content change.
When a shadowed derived cache replaces an older cache at commit, any
`TileKey::INVALID` slots in the new cache are first backfilled from the old
cache if the old slot is valid.
