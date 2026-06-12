# Design Note 04: Dirty Propagation, Commit, And History

- **Status**: Implemented initial storage-side migration
- **Layer**: Session dirty tracking, global image publication, and draw history
- **Related code**: `gla_storage`, `gla_session`, `gla_image`, `tile_key`

## Old Behavior To Preserve

The old `DrawSession` mixed three responsibilities in one type:

- frame dirty tracking for repaint/materialization;
- commit-time publication of session edits into document images;
- primitive inverse patch storage for undo/redo.

The new storage model keeps the behavior but splits it across `DrawFrame`,
`DrawSession`, `GlobalStorage`, and `DrawHistory`.

Dirty remains frame/session execution information. It is not the durable undo
record. A DrawOn write records dirty for its target tile in `DrawFrame`.
`DrawFrame::flush` uploads that dirty through derive edges, materializes the
resulting demand, submits the frame pass list to `RenderBackend`, and clears
frame state only after successful submit.

Commit remains version-gated. Only primitive global edits enter draw history.
Derived edits are cache publication: replacement tiles move into the cache image
and replaced cache tiles are released, but no derived tile owner is stored in
history.

Undo and redo still apply inverse `ImageEdit` patches to primitive images. They
do not replay DrawOn input and do not restore derived cache tiles from history.

## Dirty Propagation

`DrawFrame::draw_dab` calls into `DrawSession` to materialize writable DrawOn
destinations and records frame dirty after a writable destination is returned.
The same call appends the actual DrawOn dab passes to `DrawFrame::dab_passes`.
Dirty recording is therefore tied to storage mutation, not to a particular brush
implementation.

`DrawFrame::flush(session, global, backend)` clones frame dirty, starts with the
buffered dab passes, and uploads dirty through lowered derive commands:

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

`GlobalStorage.version` is the Rust-side resource version. `DrawSession::begin`
and `DrawSession::commit` reject mismatched expected versions. The current
registry patch path does not yet bump this version; registry-version integration
is separate follow-up work.

`DrawSession::commit(self, global, history)` consumes the session. Frame work
must already have been submitted by `DrawFrame::flush`; commit does not submit
GPU work. If commit sees no `ImageEdit`, it releases local tile owners and
returns `Ok(None)` without bumping the version or storing history. If commit
fails version or edit validation, it releases local tile owners and the session
cannot be retried. This matches the documented fallback: without a version bump
and history record, session truth is not published.

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

`DrawFrame::flush` is now the render-drain boundary. Dab passes may be buffered
between `draw_dab` and `flush`, but `flush` combines them with derived/cache
materialization passes and submits the ordered list to `RenderBackend`.
`DrawFrame` clears dirty and dab-pass state only after submit succeeds. After
that point, `commit` and `discard` can release session-local tile owners without
owning renderer lifecycle state.

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
- Dirty demand is derived from lowered commands in `DrawSession`, not from a
  separately persisted old `DirtyEdge` list.
