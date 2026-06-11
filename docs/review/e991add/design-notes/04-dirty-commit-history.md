# Design Note 04: Dirty Propagation, Commit, And History

- **Status**: Implemented initial storage-side migration
- **Layer**: Session dirty tracking, global image publication, and draw history
- **Related code**: `gla_storage`, `gla_session`, `gla_image`, `tile_key`

## Old Behavior To Preserve

The old `DrawSession` mixed three responsibilities in one type:

- frame dirty tracking for repaint/materialization;
- commit-time publication of session edits into document images;
- primitive inverse patch storage for undo/redo.

The new storage model keeps the behavior but moves it onto `LocalStorage`,
`GlobalStorage`, and `DrawHistory`.

Dirty remains frame/session execution information. It is not the durable undo
record. A DrawOn write records dirty for its target tile. `flush_frame` uploads
that dirty through derive edges and materializes the resulting demand.

Commit remains version-gated. Only primitive global edits enter draw history.
Derived edits are cache publication: replacement tiles move into the cache image
and replaced cache tiles are released, but no derived tile owner is stored in
history.

Undo and redo still apply inverse `ImageEdit` patches to primitive images. They
do not replay DrawOn input and do not restore derived cache tiles from history.

## Dirty Propagation

`LocalRenderCtx::draw_on_write_pos(id, tile_index)` records frame dirty after it
successfully returns a writable destination. The caller still appends the actual
DrawOn renderer work. Dirty recording is therefore tied to storage mutation, not
to a particular brush implementation.

`LocalStorage::flush_frame(global)` consumes frame dirty and uploads it through
lowered derive commands:

- reads lowered to `SessionImageId::Current(id)` produce dirty edges;
- reads lowered to `SessionImageId::Global(id)` are backup/global-only reads and
  do not propagate current-session dirty;
- `Identity + None` with matching layouts passes the tile set through;
- `Identity + None` with different layouts clamps tile indices to the
  destination tile count;
- `Expand` and `Matrix` mappings conservatively become `TileSet::Full`.

`doc_dirty` keeps the old meaning: it accumulates dirty sets only for
`ReadWrite` global image ids. It does not record session-local images or
downstream derived caches.

Rendering demand is not root-based in the new storage layer. Before registered
views exist, the conservative behavior is:

1. activate all global derived images reachable upward from the write starts;
2. upload dirty through that active DAG;
3. render only terminal dirty derived nodes, letting recursive render
   materialize their dependencies.

This avoids treating `GlobalStorage.root` as the presentation truth while still
preserving the old "render the visible end of the dirty chain" behavior. Later
view registration can replace terminal-DAG rendering with viewed-image demand
and optional pruning.

## Commit And History

`GlobalStorage.version` is the Rust-side resource version. `LocalStorage::build`
and `LocalStorage::commit` reject mismatched expected versions. The current
registry patch path does not yet bump this version; registry-version integration
is separate follow-up work.

`LocalStorage::commit(self, global, history)` consumes the local storage. If
commit fails version or edit validation, it releases local tile owners and the
session cannot be retried. This matches the documented fallback: without a
version bump and history record, session truth is not published.

Commit applies non-empty `SessionImageContent::Edit` values by the current
global image role:

- `GlobalImage::Primitive(DenseImage)`: replacements move into dense slots, old
  dense tiles become inverse `ImageEdit` entries, and the inverse patch is stored
  in `DrawHistory`;
- `GlobalImage::Derived(CacheImage)`: replacements move into cache slots,
  replaced `Some(tile)` owners are released, and no history entry is recorded
  for that image.

Remaining session-local `Raw` tiles and uncommitted `Edit` tiles are released
after successful commit or explicit discard.

`DrawHistory` records move-only tile owners. Applying a stored patch therefore
cannot clone the record like the old `TileKey` implementation did. The new
behavior validates a record by reference, removes and consumes it on successful
apply, then stores the returned inverse as a new record. Undo and redo still use
the same apply-inverse mechanism; the record lifetime now matches exclusive tile
ownership.

## Deliberate Differences From The Old Component

- No `GlaImageKey`, document binding table, or `key_to_id` reverse map is
  involved in storage-side commit.
- No root image is treated as the long-term presentation target.
- No `ImageEdit.source` is needed; version checks and inverse generation provide
  the commit safety.
- History patches own tiles exclusively and are consumed when applied.
- Dirty demand is derived from lowered commands in `LocalStorage`, not from a
  separately persisted old `DirtyEdge` list.
