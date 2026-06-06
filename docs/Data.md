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

Janet-visible IR uses compact opaque `ImageId`s. Rust stores resources behind
generation-checked keys:

```text
RegistryGraphKey      // declarations, root, derived commands
ImageBindingTableKey  // ImageId -> ImageKey
ImageKey              // image tile array / content version
```

Doc-level `ImageId`s are not reused during a document lifetime. Session-local
images use the same id type and may shadow doc ids within a draw session.

## Image Declarations

The registry graph declares document images:

```text
PrimitiveImage:
  Editable or externally supplied document image.
  Valid ReadWrite draw target.
  Has no registry build command.
  Never contains TileKey::INVALID.

DerivedImage:
  Document cache or output image.
  Has exactly one build command.
  Not a direct draw target.
  May contain TileKey::INVALID.
```

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
  Local cache with one local build command, released at session end.
```

Session images do not enter the document binding table or undo records.

## Tile Resources

An image is an array of tile keys. Empty tile bindings are valid zero content.
`TileKey::INVALID` means a derived cache tile has not been built yet.

Tile write paths do not own history semantics. Session cleanup compares old and
new image tile arrays by index to release replaced cache resources. Primitive
history and root presentation caches are retained by session records.
