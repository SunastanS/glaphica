# Design Note 01: Owning Tile Handles

- **Status**: Open
- **Layer**: Tile/resource identity and lifecycle
- **Related code**: `tile_key`, `atlas`, `gla_image`, `gla_session`

## Current Direction

The review has moved toward a stricter tile ownership model where the current
`TileKey` concept is renamed to `Tile` and becomes the move-only owner/token.
`Tile` should not implement `Copy` or `Clone`; moving a `Tile` transfers
ownership of that tile identity. The earlier wrapper idea (`Tile(TileKey)` or
`Tile(Option<TileKey>)`) is no longer needed under this direction.

Hot image slots should not use a large enum such as `ImageTile::{Invalid,
Empty, Owned}`. Image tile lookup is hot, and the representation should stay
compact. Prefer the standard-library niche representation at the `Tile`
level: store tile identity with a non-zero type such as
`NonZeroUsize`/`NonZeroU64`, so `Option<Tile>` remains one word while
encoding invalid/cache-miss as `None`.

Long-lived image tile arrays should not all share the same optional slot
representation. The target representation differs by image role:

```rust
// Primitive/raw: every slot is valid and owns a tile.
Box<[Tile]>

// Derived/cache: each slot may be invalid/cache-miss.
Box<[Option<Tile>]>
```

This is the point of distinguishing `PrimitiveImage` from `DerivedImage`: the
primitive representation cannot express per-tile invalid/cache-miss state, while
the derived representation can.

When `Tile` is backed by a non-zero integer, `Option<Tile>` uses the
standard-library niche representation for the empty sentinel. This keeps
optional derived/cache slots compact and permits invalid tile values without
reintroducing a public `Tile::INVALID` sentinel.

The non-zero encoding should keep the existing index bits unchanged and make
generation numbers start at 1. Do not encode non-zero-ness by adding 1 to the
whole packed value; that complicates index handling. With generation 0 reserved,
every valid packed tile key is non-zero, and `Option<Tile>::None` can represent
invalid/cache-miss state. Generation wrap should skip 0 rather than panic only
because 0 is reserved for the niche; with move-only ownership, retaining an old
tile after release is already an impossible public path.

`Tile` is a table-position identity used to look up `Tiles.bindings`.
Whether the binding at that position is empty is the next layer down.
`EmptyTile` is not a domain concept exposed above the tile resource layer. A
valid tile with no physical allocation and a valid tile with an allocated
zero-content physical slot should behave the same to image/session/renderer
callers. The resource layer may internally represent empty bindings with a
sentinel, but outer modules should only see valid tile identity and invalid
cache-miss state.

`Atlas` remains simple: atlas allocation is physical slot allocation. Empty
bindings are not an atlas concept; they are a tile-resource layer wrapper around
a valid tile identity that has not acquired a physical slot yet.

The public tile resource interface should not expose a direct physical
allocation call. Callers that want a writable physical slot first reserve a
valid `Tile`, then call acquire-for-write. Physical allocation is a consequence
of write acquisition, not a separate caller-visible construction path.

Acquire-for-read should not expose the internal empty binding sentinel as if it
were a physical atlas position. It should return a short-lived read view, for
example:

```rust
enum TileReadRef {
    Zero,
    Physical(TilePos),
}
```

This enum is acceptable at the acquire seam because it is not the hot image slot
representation. It also separates binding generation checks from the lower-level
renderer position semantics; the renderer does not need to know the generation
stored in a tile binding.

## Intended Leverage

The type system should make common lifecycle bugs harder to express:

- double free;
- freeing a tile still held by image state;
- history records silently retaining already-released tile resources;
- cleanup code treating copied keys as owned resources.

## Open Questions

1. What is the exact staged migration path from the resource layer into
   role-specific image storage?
2. What is the non-owning view type passed to renderer commands once
   `gla_image_command::RenderCtx` stops exchanging durable tile owners?

## More Aggressive Alternative

Callers that currently need a tile key for rendering should receive
already-resolved render views instead:

```rust
enum TileReadRef {
    Zero,
    Physical(TilePos),
}
```

and write paths would receive `TilePos` after materialization.

At `e991add`, this is structurally feasible but not a small change:

- `gla_image_command::RenderCtx` currently returns the current `TileKey` from `render` and
  `write_tile`, then immediately calls `acquire_for_read`/`acquire_for_write`.
  This seam can be deepened so command execution works in terms of
  `TileReadRef` and writable `TilePos` instead.
- `gla_image_command` does not need durable tile identity. Its operations need
  source content and destination positions. It can stop importing `Tile` once
  `RenderCtx` changes shape.
- `gla_image` and `gla_session` currently store the current `TileKey` in image slots,
  session raw rows, session edit rows, and history patches. Those are ownership
  storage sites and must stop copying keys before `Tile` can stop being
  copyable.

The biggest semantic question is zero-source rendering: `Copy(Zero)` clearly
means clear destination to zero, but each composite operation must define what
`RenderTo(Zero)` means before command execution can be fully expressed without
source tile keys.

Under this target, `Tile` should not be exposed as a public render handle.
The resource layer owns key lookup. The relevant operations are
private/resource-module equivalents of `get` and `get_mut`:

```rust
fn read_ref(&self, tile: &Tile) -> TileReadRef;
fn write_pos(&mut self, tile: &mut Tile) -> TilePos;
```

`read_ref` resolves an immutable tile slot into zero content or a physical
position. `write_pos` resolves a mutable tile slot into a physical destination,
materializing an empty binding if necessary. `Tile` remains an implementation
detail of those lookups.

## Current Recommendation

Start with a single-owner model unless a concrete undo/redo or cache-sharing
case proves that shared ownership is required. Keep hot tile slots compact:
primitive/raw images use non-optional valid tile keys, while derived/cache
images use `Option<Tile>`/`Option<NonZero*>` niche encoding. Avoid exposing
empty-binding details outside the resource layer.

Assume image tile ownership is exclusive: two different images should not own
the same tile. Under the current `ImageEdit` model, committing a session should
move tile ownership between session edit rows, document image slots, and history
patches; it should not copy tile keys as if they were shared resources.

`GlaImages::copy_on_write` should be removed. It predates `ImageEdit`; the
current edit model provides a cleaner session-local copy-on-write path without
creating multiple image rows that share the same tile keys.

`Tile` should become an internal move-only resource owner/token rather than a
public copyable render token. The first resource-layer migration should directly
replace the current `TileKey` concept with `Tile`; do not keep a public
copyable `TileKey` compatibility layer in the bottom crate. Because this breaks
upper crates immediately, the first implementation stage may temporarily shrink
the workspace to the bottom crates only:

```text
gla_core
gla_color
atlas
tile_key
```

The first-stage verification target is therefore the resource layer and its
direct dependencies, not the whole current application stack. Upper crates are
added back as their storage and render seams are migrated.

At the bottom layer, `Pool` uses 1-based generations and skips 0 on wrap. This
keeps `Tile(NonZeroU64)` simple while treating generation as an internal
consistency check, not as the primary ownership safety mechanism. Move-only
ownership is the safety boundary; generation only catches corrupted internal
state during migration and debugging.

Do not keep a public best-effort discard function. Normal release consumes
owning `Tile`:

```rust
fn release(&mut self, tile: Tile)
```

With move-only keys, double free should not be representable through the public
interface. Cleanup paths should also move owned tiles into `release`; they
should not release by copied `Tile`.

If release observes a stale generation, double free, atlas mismatch, or another
impossible ownership error, it should panic. Under this interface those cases
mean allocation or ownership construction already produced two owners for the
same `Tile`, or otherwise corrupted the resource model. They are not
recoverable business errors.

The fail-fast rule applies through the release stack. `Pool::free` and
`Atlas::free` should use runtime assertions for invalid frees, not debug-only
assertions or silent returns. Allocation exhaustion and invalid atlas selection
remain normal `Result` errors.

An optional cache slot with `None` is a valid invalid/cache-miss slot and owns no
resource. Releasing it is a no-op when the release helper receives an optional
slot and finds `None`. Panics are reserved for present tile keys whose
generation/binding invariants are broken.

Session-local `Raw` images should be allocated with full valid tile slots. They
should not contain optional/invalid slots. Reserving valid tile identities is
cheap, and this preserves the hot-path invariant that raw session buffers do not
need cache-miss handling.

## Rejected Direction

Do not introduce a public hot-path enum like:

```rust
enum ImageTile {
    Invalid,
    Empty(EmptyTile),
    Owned(Tile),
}
```

This makes image slots larger and leaks the resource layer's empty-binding
representation into callers that should only care whether a tile is valid.

Do not split the public interface into explicit empty-vs-physical allocation
constructors such as:

```rust
alloc_empty_from(...)
alloc_physical_from(...)
```

The empty/physical distinction is internal to `tile_key`. The caller-visible
path is reserve valid tile identity, then acquire for write when physical
storage is needed.

Do not keep public best-effort cleanup such as `discard_if_valid`. It re-exposes
resource implementation details and is not needed if ownership is represented
with move-only `Tile` values.

Do not introduce a `Tile(TileKey)` wrapper once the current `TileKey` concept is
renamed to move-only `Tile`. That wrapper only made sense while `TileKey`
remained a copyable non-owning handle.
