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

The constructor direction is now explicit at the image value layer:

- `PrimitiveImage::allocate(...)` reserves a full valid tile array initialized
  as zero/empty content through the tile resource layer.
- `DerivedImage::new_invalid(...)` creates a full cache-miss array with
  `None` slots.

Image constructors reject zero-tile layouts. `GlaImageLayout` can still compute
`tile_count() == 0`; the resource-owning image values are the layer that rejects
zero-size images.

Do not expose public constructors that accept raw tile arrays such as
`Box<[Tile]>` or `Box<[Option<Tile>]>`. With move-only tile owners, any fallible
constructor that accepts raw owned tiles needs an awkward error path that returns
the owners to the caller. The cleaner boundary is for image constructors to
validate layout before allocating or initializing slots. Whole-image migration,
deserialization, or test fixture paths can be added later as restricted helpers
with a clearly trusted precondition.

The same rule applies to slot mutation APIs that accept a replacement `Tile`.
If a method accepts a new owner and can fail before installing it, its error
type must return that owner. `PrimitiveImage::replace_tile` and
`DerivedImage::replace_tile` therefore return a tile-carrying error on
out-of-bounds indices instead of dropping the replacement tile.

Image storage is split by role:

```rust
enum DocumentImage {
    Primitive(PrimitiveImage),
    Derived(DerivedImage),
}
```

or an equivalent map-level enum that stores the derive command next to the
derived image value. The current `gla_image` crate intentionally stops before
that map: it owns only `PrimitiveImage` and `DerivedImage` values and their tile
slot invariants. The `ImageId -> DocumentImage` map and command locality decision
belong to the next layer.

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
  it must not carry arena row keys. The old `InsertImage { key: GlaImageKey,
  ... }` operation is removed with the arena; patch operations are image-id and
  metadata/role declarations.
- Session construction and commit currently take large resource stores by value
  (`GlaImages`, `Tiles`, `Renderer`) and then return `Result`. Under move-only
  resource ownership, these APIs need a sharper failure contract: validate before
  moving stores, borrow an external resource context, or return the resource
  bundle on failure. Otherwise an initialization or commit error can drop the
  caller's resource owners.

## Current Recommendation

Do not preserve the current `GlaImages`/`GlaImageKey` arena. Once the bottom
`Tile` resource layer is move-only, `gla_image` should be added back as a
role-specific image value crate, not as an arena compatibility layer.

Image removal is defined around owned `PrimitiveImage`/`DerivedImage` values and
their tile arrays. The value layer can move all owned tiles into
`Tiles::release`/`release_optional`; it does not need row-generation cleanup.

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
