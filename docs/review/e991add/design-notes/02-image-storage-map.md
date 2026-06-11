# Design Note 02: Image Storage Map

- **Status**: Open
- **Layer**: Image storage and document image identity
- **Related code**: `gla_image`, `gla_storage`, `gla_doc`, `gla_session`,
  `gla_ir::RegistryPatch`

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

The emerging direction is that Rust global image storage should not be an arena
addressed by separate `GlaImageKey` rows. It should become a map-like structure
keyed by domain image identity, for example:

```rust
HashMap<ImageId, GlobalImage>
```

or an equivalent structure with the same ownership semantics.

In the target architecture there may be no `GlaImages` container module at all;
it is removed together with `GlaImageKey`. In that case `GlaImage` must own and
manage the lifecycle of its tile array directly. Construction must therefore
make the initial tile-slot state explicit:

- optional slots for derived/cache images that start invalid;
- non-optional valid slots for primitive/raw images that start as zero content.

The Rust-side owner should be named around storage, not document truth. Use
`GlobalStorage` for session-independent Rust resources and `LocalStorage` for
draw-session-local shadows and temporary images. Janet owns business/document
IR; Rust storage owns images, commands, tiles, renderer resources, and cache
materialization state.

The constructor direction is now explicit at the image value layer:

- `DenseImage::allocate(...)` reserves a full valid tile array initialized
  as zero/empty content through the tile resource layer. It asks `Tiles` for
  tiles by image format; callers should not pass an arbitrary `atlas_id`.
- `CacheImage::new_invalid(...)` creates a full cache-miss array with
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
type must return that owner. `DenseImage::replace_tile` and
`CacheImage::replace_tile` therefore return a tile-carrying error on
out-of-bounds indices instead of dropping the replacement tile.

Global image storage is split by role at the map level, while `gla_image`
stores only the underlying tile-slot shape:

```rust
enum GlobalImage {
    Primitive(DenseImage),
    Derived {
        image: CacheImage,
        command: GraphCommand,
    },
}
```

The current `gla_image` crate intentionally stops before that map: it owns only
`DenseImage` and `CacheImage` values and their tile slot invariants. The
`ImageId -> GlobalImage` map and command locality decision belong to the next
layer.

This split should not wrap a shared `GlaImage` storage type. The role split is
valuable because the two storage shapes have different slot representations:

```rust
struct DenseImage {
    tiles: Box<[Tile]>, // every slot valid; Tile is move-only owner
}

struct CacheImage {
    tiles: Box<[Option<Tile>]>, // None = invalid/cache miss
}
```

Using one shared `GlaImage` with optional slots would erase the type-level
distinction and let primitive/raw images express states they should not have.

The reason this is now viable is the `ImageEdit` model. Applying an edit
generates the inverse edit needed for undo/redo, so image version management no
longer needs to be encoded by allocating replacement image rows. Draw commit can
mutate the current image's tile slots and return inverse tile ownership for
history.

`GlobalStorage` should own the global image map directly:

```rust
struct GlobalStorage {
    version: DocumentVersionId,
    root: Option<ImageId>, // temporary SetRoot compatibility, later views
    images: HashMap<ImageId, GlobalImage>,
    tiles: Tiles,
    renderer: Renderer,
}

enum GlobalImage {
    Primitive(DenseImage),
    Derived {
        image: CacheImage,
        command: GraphCommand,
    },
}
```

Do not keep a separate authoritative registry table beside this image map.
Dependency indexes may be cached later, but they must be derived from
`GlobalStorage.images`.

`GlobalStorage.version` is the storage-side version gate for draw sessions and
history patch application. Janet still owns the business document/IR layer, but
Rust storage must reject tile/resource commits whose expected version no longer
matches the global image store. Registry patch versioning is separate follow-up
work; this migration introduces the version gate for draw commit and history
first.

`GlobalStorage.root` is only a temporary compatibility field for the existing
`RegistryPatchOp::SetRoot`. It is not the long-term presentation model. Registered
views should eventually replace it by selecting which `ImageId`s are rendered to
windows or surfaces. Because of that direction, global storage graph validation
checks that every graph read exists and that the graph is acyclic, but it does
not require every stored image to be reachable from `root`.

`GlobalStorage` applies `RegistryPatch` operations from Janet. Patch application
must be atomic from the storage caller's perspective: on failure, existing
storage remains unchanged and any newly staged tile owners are released. The
implementation should validate first, allocate replacement images into staging,
then commit replacements and release old images only after all patch operations
have succeeded.

Patch application uses sequential operation semantics. `NewImage` inserts a new
metadata row, `SetPrimitive` and `SetDerived` require an already-declared image,
and `SetRoot` requires the referenced image to exist at that point in the patch.
Existing primitive content is preserved when an image remains primitive. Derived
cache content is preserved only when the final graph command is unchanged;
changing role or command stages a replacement image and releases the old tile
owners after the patch commits.

## Intended Leverage

This reduces duplicated identity layers:

- `ImageId` remains the cross-language/domain identity.
- `DenseImage`/`CacheImage` own their tile arrays directly.
- Image row cleanup can move/drop owned tiles from the image value.
- `GlaImages::free` becomes image removal with tile ownership cleanup, not
  arena-slot release that ignores tile resources.

It also removes the need to reverse-map `GlaImageKey -> ImageId` in session code,
which is currently fragile when multiple ids could bind the same key.

## Known Impact

This is a large refactor, not a small fix:

- `Document.bindings: ImageId -> GlaImageKey` would need to disappear or become
  a temporary compatibility layer. The first migration removes document
  bindings from `gla_doc`; document image storage is no longer represented as
  `ImageId -> GlaImageKey`.
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
storage-specific image value crate, not as an arena compatibility layer.

Image removal is defined around owned `DenseImage`/`CacheImage` values and
their tile arrays. The value layer can move all owned tiles into
`Tiles::release`/`release_optional`; it does not need row-generation cleanup.

Do not use a shared inner `GlaImage` for both dense and cache storage. That
would preserve the exact ambiguity the storage-specific types are meant to
remove.

Global image roles choose one of these storage shapes: primitive images use
`DenseImage`, while derived images use `CacheImage` plus a graph command at the
map level. Session-local `Raw` rows use `DenseImage` regardless of writer.
Existing session design keeps session-created images dense: both session
primitive declarations and session derived declarations allocate full-valid
zero-content tile arrays. The writer (`DrawOn` or `Derive`) determines how those
dense tiles are updated, but the raw local content representation does not need
a per-slot cache-miss branch.

Local session state should therefore separate the storage source from the
writer:

```rust
enum SessionImageContent {
    Raw(DenseImage),       // session-local full-valid storage
    Edit(ImageEdit),       // global-backed sparse replacement storage
}

enum SessionImageWriter {
    DrawOn(...),
    Derive(DeriveCommand<SessionImageId>),
}

struct SessionImage {
    format: GlaFormat,
    layout: GlaImageLayout,
    content: SessionImageContent,
    writer: SessionImageWriter,
}
```

The independent validity checks are:

- `Raw` means session-local storage, regardless of whether the writer is
  `DrawOn` or `Derive`.
- `Edit` means a global-backed shadow/replacement patch, regardless of whether
  the writer is `DrawOn` or `Derive`.
- `DrawOn` must target writable full-valid content after first-write
  materialization.
- `Derive` is dirty-driven and full-overwrite for affected tiles.
- Global derived images may only be shadowed by their global graph derive path,
  not by explicit local primitive writers.

`Derive` stores the lowered execution command. The local execution layer should
not distinguish whether the command came from Janet session IR (`SessionCommand`)
or from a global graph command (`GraphCommand`). Those differences are resolved
during session initialization/lowering.

The `gla_storage::LocalStorage` build starts from explicit writer targets:

- session `Primitive` declarations become `Raw + DrawOn` or `Raw + Derive`
  depending on their explicit writer;
- session `Derived` declarations carry their own `SessionCommand` in the current
  Rust IR and therefore become `Raw + Derive`;
- `draw_on` entries become `DrawOn` writers;
- explicit `derive` entries become `Derive` writers and may target a session
  image or a `ReadWrite` global primitive shadow;
- `ReadWrite` global primitive targets become `Edit`;
- duplicate writers are rejected before allocation.

After explicit writers are known, active-chain discovery uses the conservative
DAG rule: walk upward through global derived reverse-read edges and activate
every reached global derived image as `Edit + Derive`. Global graph-command reads
lower to `SessionImageId::Current(id)`, so a downstream cache shadow sees the
current session shadow when one exists and falls back to global content
otherwise. This may activate more derived images than strictly needed for
presentation, but it preserves correctness before view registration exists.

Once registered views exist, a later optimization may prune the upward walk by
whether a reached branch can affect any viewed image. That pruning is a cache
policy optimization, not a different session storage state model.

The textual brush examples that list `soft_coverage Derived ...` and then show
`BlurCoverage -> soft_coverage` under a separate `derive:` block should be read
as upper-layer brush pseudocode, not the concrete `gla_ir::DrawSessionIR` shape.
In the concrete Rust IR, a session derived declaration already owns its command;
an additional explicit `derive` targeting the same image is a duplicate writer.

`LocalStorage::build` should validate and lower before allocating image content.
The current implementation collects doc access, resolves session metadata in
declaration order, collects explicit writers, appends conservative active-chain
graph shadows, lowers derive reads to `SessionImageId::{Current, Global}`,
checks local writer cycles, and only then allocates `DenseImage` raw content. If
allocation fails, any staged raw tile owners are released and no local storage is
returned.

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
