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

`ImageId -> ImageKey` tables are doc/session-layer state. They do not belong to
the image storage layer. The image storage layer works in `ImageKey` and
`TileKey`; the image-command layer works in key-level executable commands.

Doc-level `ImageId`s are not reused during a document lifetime. Session-local
images use the same id type and may shadow doc ids within a draw session.

In code, `gla_doc` owns document graph snapshots, `ImageBindingTable`, active
document state, registry patch records, and undo/redo state transitions.
`gla_session` owns draw-session execution state, including `LocalImageTable` for
session-local `ImageId -> ImageKey` and `ImageId -> LocalImageDeclaration`.

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
  key-level executable image commands

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

Before execution, doc/session code lowers id-level IR through the active
evaluation context:

```text
GraphCommand + doc current table -> ImageCommand
SessionCommand + draw evaluation context -> ImageCommand
DrawOnCommand + draw evaluation context -> DrawCommand
```

`ImageCommand` and `DrawCommand` operate on `ImageKey`s. They do not know
`ImageId`, document bindings, session-local shadowing, or `.backup`.

## Tile Resources

An image is an array of tile keys. Empty tile bindings are valid zero content.
`TileKey::INVALID` means a derived cache tile has not been built yet.

Tile write paths do not own history semantics. Session cleanup compares old and
new image tile arrays by index to release replaced cache resources. Primitive
history and root presentation caches are retained by session records.
