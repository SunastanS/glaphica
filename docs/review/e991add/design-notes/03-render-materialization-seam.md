# Design Note 03: Render Materialization Seam

- **Status**: Open
- **Layer**: Image rendering, tile materialization, and command execution
- **Related code**: `gla_session`, `gla_image_command`, `gla_image`, `tile_key`

## Current Direction

For outer modules, there should be one way to obtain a tile reference for an
image at a tile index:

```rust
render(image, tile_index)
```

Callers should not directly inspect image tile slots and then decide whether to
allocate, repair, or return a position. `render` owns materialization.

## Tile Slot Cases

Derived/cache slots are expected to use an optional tile representation:

```rust
Option<Tile>
```

The render seam handles the cases:

- `None` means invalid/cache miss. `render` looks up the image's derive
  command, allocates a valid tile, recursively renders dependencies, records the
  render command work, stores the resulting valid tile, and returns the
  materialized position/read view.
- `Some(tile)` on an active image means the tile identity exists, but the
  active command still needs to run. `render` uses that tile as destination,
  recursively renders dependencies, records command work, and returns the
  resulting position/read view.
- `Some(tile)` on an inactive image means the tile is trusted content.
  `render` returns the corresponding position/read view directly.

This preserves the confirmed behavior that active local/session outputs are
recomputed on demand even when their tile slot is already valid.

In this note, "active" means the image has a shadow edit in the current session.
Do not split this into separate "shadow chain" and "command overlay" concepts.
If an image has a shadow edit, the session may modify its content, so its cache
is not trusted and the render chain must be rerun through the session view.

The intended architecture distinguishes shadow kind:

- If a document primitive is shadowed as a session derive target, it has a local
  derive command. The `ImageEdit` derive has the same source image semantics as
  the document image; the difference is resolution: references to the shadowed
  image in the current session resolve to its `ImageEdit` view rather than
  directly to the document image.
- If a document primitive is shadowed as a primitive target, it is the target of
  a `DrawOn`. Primitive image render semantics are direct: render returns the
  target tile position/read view. It must already have a valid tile; `None`
  is not a valid primitive shadow state.

A derived document image must not be shadowed as a primitive target. Only a
primitive document image may be shadowed as a primitive `DrawOn` target. Under
primitive-to-primitive shadowing, invalid source tiles should not occur; if they
do, the document state is corrupt.

Any existing implementation detail that makes active behavior appear to depend
only on an explicit `local_commands` entry should be treated as implementation
evidence to audit, not as the design definition.

## Write Position Rule

`write_pos(None)` should be unreachable in the correct outer interface.
Writing happens as part of `render` materialization. If a destination slot is
invalid, `render` allocates a tile before command execution asks for the write
position.

This means the command execution seam should not let outer callers bypass
materialization by calling write acquisition directly on arbitrary image slots.

## Command Views

The command execution seam should receive resolved views rather than tile owner
tokens:

```rust
enum TileReadRef {
    Zero,
    Physical(TilePos),
}
```

`Copy(Zero)` means copying valid zero content into the destination. The current
implementation strategy can lower this to `renderer.clear(dst)`.

`RenderTo(Zero)` must not receive one blanket rule. Its behavior depends on the
operation/blend mode. Some composites may treat a zero source as no-op, while
others, such as mask-style operations, may produce a meaningful change. Each
operation must define its zero-source behavior explicitly. The first refactor
only needs to settle `Copy(Zero)`.

`write_dst` is a derive-command execution seam and should not become the public
DrawOn write interface. DrawOn is input-driven and performs atomic mutation of
tile content. Janet does not inspect the internal DrawOn operation, so the Rust
implementation should keep the DrawOn write path simple, explicit, and easy to
audit. It may share low-level resource helpers, but it should not be hidden
behind the same command seam as dirty-driven full-overwrite derive commands.

DrawOn may target session-local full-valid storage, and it may also target an
`ImageEdit` shadow for a primitive document image. This is the path for simple
brushes such as pixel replacement. When DrawOn asks the `ImageEdit` for a tile
and that index has not been edited yet, the edit allocates a new tile, records
the edit entry, copies the source document tile into the new tile, and then
hands that writable tile position to DrawOn. Later writes to the same index
reuse the existing edited tile.

This first-write logic is internal to `ImageEdit`, not a storage-only return
status that outer DrawOn code interprets. `ImageEdit` is a session-layer object
and may work with renderer/tile resources to perform the source copy. Outer
DrawOn code should ask for a writable tile position and then append the actual
DrawOn mutation; it should not independently decide whether a source copy is
needed.

The first-write copy and the actual DrawOn mutation must be ordered work in the
same renderer command sequence, or in an equivalent builder that preserves the
same ordering guarantee. The copy must complete before the DrawOn mutation for
that tile. This ordering is part of the normal "fill before mutate" semantics
and verifies that the call chain materializes the edit tile correctly.

This DrawOn first-write copy path is distinct from derive materialization.
Derived images still must not be shadowed as primitive DrawOn targets.

TODO: when the resource layer has a suitable way to communicate "destination is
now an empty binding" back to the owning image/session layer, `Copy(Zero)` could
release the destination physical slot and store an empty binding instead of
clearing allocated memory. This is a memory optimization, not required for the
first refactor.

## Identity Direction

This direction fits the larger image-storage refactor:

- document image references should move toward `ImageId`, not `GlaImageKey`;
- the command-layer session image id should model current-vs-document lookup:

```rust
enum SessionImageId {
    Current(ImageId),
    Doc(ImageId),
}
```

`Current(ImageId)` resolves through the session-local shadow table first. If the
image has a local shadow, that shadow is used; otherwise resolution falls back
to the current document image. `Doc(ImageId)` resolves only to the session-start
document image and never sees local shadows. This preserves the existing
`image` vs `image.backup` design from `docs/Session.md`.

- session-local state should be keyed like `HashMap<ImageId, SessionImage>`,
  where `SessionImage` can be a session-local full image or an `ImageEdit`
  shadow for a document image;
- command execution should receive resolved read/write views, not durable tile
  ownership keys.
