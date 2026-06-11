# Design Note 02: Image Storage Map

- **Status**: Open
- **Layer**: Image storage and document image identity
- **Related code**: `gla_image`, `gla_doc`, `gla_session`, `gla_ir::RegistryPatch`

## Current Direction

`GlaImages::free` should not be deleted as a small cleanup. It should be
reworked as part of a larger image storage change.

The current implementation stores image rows in an arena-like `GlaImages` table
addressed by generation-checked `GlaImageKey`. Existing design documents
describe:

```text
Document bindings: ImageId -> GlaImageKey
GlaImageKey: row in GlaImages
```

But previous review synthesis also confirms that `gla_doc` is temporary
scaffolding. Long-term truth lives in the Janet layer, while Rust stores derived
images and graph commands and computes impact chains when Janet publishes
primitive modifications.

The emerging direction is that image storage should not be an arena addressed by
separate `GlaImageKey` rows. It should become a map-like structure keyed by
domain image identity, for example:

```rust
HashMap<ImageId, DocumentImage>
```

or an equivalent structure with the same ownership semantics.

In the target architecture there may be no `GlaImages` container module at all;
it is removed together with `GlaImageKey`. In that case `GlaImage` must own and
manage the lifecycle of its tile array directly. Construction must therefore
make the initial tile-slot state explicit:

- optional slots for derived/cache images that start invalid;
- non-optional valid slots for primitive/raw images that start as zero content.

The open constructor design is whether this should be represented as separate
constructors such as `new_none(...)` and `new_empty(...)`, or as staged
construction such as `new_none(...).valid_all(...)`.

Another open design option is to split image storage by role:

```rust
enum DocumentImage {
    Primitive(PrimitiveImage),
    Derived(DerivedImage),
}
```

or an equivalent enum that stores the derive command with the derived image
value. The current code already ties role and command at the metadata level with
`ImageRole::Derived(GraphCommand)`, but storage remains separate through
`Document.roles` and `Document.bindings`. In the target architecture, storing
the command inside a derived image may improve locality: the cache tiles and the
only command that can materialize them live together.

This split should not wrap a shared `GlaImage` storage type. The role split is
valuable because the two images have different slot representations:

```rust
struct PrimitiveImage {
    tiles: Box<[Tile]>, // every slot valid; Tile is move-only owner
}

struct DerivedImage {
    tiles: Box<[Option<Tile>]>, // None = invalid/cache miss
    command: GraphCommand,
}
```

Using one shared `GlaImage` with optional slots would erase the type-level
distinction and let primitive/raw images express states they should not have.

The reason this is now viable is the `ImageEdit` model. Applying an edit
generates the inverse edit needed for undo/redo, so image version management no
longer needs to be encoded by allocating replacement image rows. Draw commit can
mutate the current image's tile slots and return inverse tile ownership for
history.

## Intended Leverage

This reduces duplicated identity layers:

- `ImageId` remains the cross-language/domain identity.
- `PrimitiveImage`/`DerivedImage` own their tile arrays directly.
- Image row cleanup can move/drop owned tiles from the image value.
- `GlaImages::free` becomes image removal with tile ownership cleanup, not
  arena-slot release that ignores tile resources.

It also removes the need to reverse-map `GlaImageKey -> ImageId` in session code,
which is currently fragile when multiple ids could bind the same key.

## Known Impact

This is a large refactor, not a small fix:

- `Document.bindings: ImageId -> GlaImageKey` would need to disappear or become
  a temporary compatibility layer.
- `SessionImageKey::Doc(GlaImageKey)` would need a replacement, likely based on
  document `ImageId` or a resolved document image reference.
- `ImageEdit.source: GlaImageKey` may become unnecessary or change meaning.
- `gla_image_command` lowering currently uses key-level image identity and would
  need a new render context seam if `GlaImageKey` goes away.
- `RegistryPatch` remains relevant for Rust-owned derived image definitions, but
  its storage target may change from image rows to image-id map entries.

## Current Recommendation

Do not spend effort preserving the current `GlaImages::free` semantics. The
current function releases an arena row but does not release tile resources, so it
does not match the ownership model under review.

When this refactor is scheduled, define image removal around owned
`PrimitiveImage`/`DerivedImage` values and their tile arrays. Until then, leave
`free` as known legacy code rather than expanding it.

Do not use a shared inner `GlaImage` for both primitive and derived images. That
would preserve the exact ambiguity the role-specific types are meant to remove.

When image storage moves away from `GlaImageKey`, remove `ImageEdit.source`.
Source image row identity is a legacy arena concern. Undo/redo safety comes
from version checks and the inverse edit generated when a patch is applied.

Do not treat the active session `ImageEdit` as a pure storage patch. It is a
session-layer object, so first-write materialization for primitive DrawOn
shadows belongs inside `ImageEdit`. When DrawOn requests a tile that is not yet
present in the edit, `ImageEdit` may allocate the replacement tile, record the
edit entry, use renderer/tile resources to copy source content into the new
tile, and then return the writable tile. This keeps the copy-before-mutate
logic colocated with the edit state that decides whether a write is first-write
or a repeat write.

Use the same `ImageEdit` type for active edits, history patches, and inverse
patches. The data shape stays the same in all cases; only the methods used
differ. Active DrawOn paths may call a renderer-aware first-write method.
History and undo/redo paths treat the same value as replacement entries and do
not call renderer-aware first-write behavior.

`ImageEdit` replacements can be represented as valid tiles, not optional tiles:

```rust
ImageEdit {
    edits: Vec<(u32, Tile)>,
}
```

`ImageEdit` is role-agnostic. It does not know about undo history, cache
publication, or inverse retention. Applying the replacement entries to an image
moves each valid replacement tile into the target slot and returns another
`ImageEdit` containing the valid tiles that were replaced.

If the old slot contains a valid tile, ownership is exchanged and the returned
`ImageEdit` contains that index and old tile. If the old slot is
invalid/cache-miss (`None`), the replacement occupies the empty slot and the
returned `ImageEdit` does not contain that index.

This lets the same application primitive serve primitive edits and derived
cache publication. The session layer decides what to do with the returned edit:
store it as history inverse, release its tiles, or otherwise manage ownership.

`ImageEdit` entries should have strictly increasing unique indices, but duplicate
indices are unreachable in the intended generation path. Edits are created by
DrawOn/render paths that allocate a new tile only on first write to an index. If
that index already has a new tile in the current edit, the writer returns the
existing tile instead of appending another edit entry.
