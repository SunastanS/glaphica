# Brush System

This document describes the first-stage brush system design.

The main goal is to keep brush logic at the image-operation level. A brush does
not know about nodes, tile keys, atlas ids, or dirty propagation. A brush only
declares image requirements and emits image-level operations. The session
interprets those operations into tile-level resource access and tile commands.

## Stage 1 Scope

- A brush session has exactly one externally supplied `Target`.
- `Target` must be an `Image` node.
- The only document tree node kinds are `Group` and `Image`.
- `Group` has a layout. The root layout is inherited by all child groups and
  images.
- Input transformation is identity along the route.
- Dirty range transformation is identity along the route.
- `DirtyRange::Full` remains available for future node kinds, but stage 1 uses
  partial tile indices only.

## Session

A session holds `&mut Tiles` and `&mut Nodes` and owns resource management for
one brush stroke.

When a brush session begins:

- Find the route from root to the target node.
- Validate that the target node is an `Image`.
- Copy-on-write the target node and all ancestors on that route.
- Bind the copied target image node as built-in `Target`.
- Bind the old target image node as built-in `Backup`.
- Create brush-owned image nodes, such as `Pigment`, inside `Nodes`.
- Do not attach brush-owned image nodes to the document tree.
- Initialize the pipelines required by the brush.

`Backup` is the stroke-start image snapshot. It has two jobs:

- It is the read source for merge passes.
- It is the undo source through the old root key.

`Backup` is immutable for the brush session. When the session finishes, the old
root is kept by the undo record. `Backup` resources are not owned by the brush
temporary lifetime.

Brush-owned images are normal image nodes in `Nodes`, but they are not document
nodes. They are auxiliary variables used by the session to update `Target`.
They do not participate in document dirty propagation.

For the pixel round brush, `Pigment` is such a brush-owned image:

- It has explicit format `D1`.
- Its layout is inherited from `Target`.
- Its coordinate space and tile index space are identical to `Target`.
- It starts as a full logical image with empty physical bindings.
- It is retained after the brush session so future replay can be supported.

## Image Requirements

There are two built-in images:

- `Target`: the copied target image node, write access.
- `Backup`: the old target image node, read access.

A brush may declare additional brush-owned images. The brush declares their
formats, but not their atlas ids. Layout defaults to `Target` layout.

For the pixel round brush:

```text
Pigment:
  format: D1
  layout: Target.layout
```

Whole-brush image access is derived from dab command declarations and commit
operation declarations. It is not a separate hand-written table.

For the pixel round brush, the derived access is:

```text
dab:
  writes Pigment

commit:
  reads Backup, Pigment
  writes Target
```

Dab commands and commit operations describe access differently.

Dab commands are input-driven and have one write target:

```text
RoundDab:
  dst: Pigment
  write semantics: accumulate with zero initialization
```

Commit operations are dirty-driven and declare read/write image slots. These may
be inferred from the pipeline descriptor:

```text
Merge:
  Backup: Read
  Pigment: Read
  Target: Write
```

Only `Target` writes are backup-protected. `Target` uses built-in `Backup` as
its backup image. Brush-owned image writes are ordinary writes.

Stage 1 does not perform complete brush spec validity checks. If a brush never
writes `Target`, it simply produces no document-visible render dirty.

## Runtime Image Binding

Brush code does not use `NodeKey`. It references images through brush-local
slots.

```text
ImageSlot(0): Target
ImageSlot(1): Backup
ImageSlot(2..): brush-owned images
```

The session owns the runtime binding table:

```text
image_nodes: Vec<NodeKey> // index == ImageSlot
```

`ImagePassCommand` stores `ImageSlot`s. The session resolves each slot to a
node key, validates that the node is an image when needed, and then performs
tile-level access.

The session record stores retained temporary resources as `NodeKey`s, not as
`ImageSlot`s. Slots are execution-time binding indices. Records hold resources
directly.

Stage 1 can represent brush commands as a strongly typed enum, for example:

```text
RoundDab {
  dst: ImageSlot,
  center,
  radius,
  flow,
}
```

The longer-term design can replace this with `PipelineId + ParamBlock` through a
pipeline registry. The runtime id would resolve access descriptors, footprint
logic, parameter layout, and execution pipeline metadata. Names remain useful
for registration, debugging, and errors, but not for hot-path execution.

## Atlas And Resource Binding

Brushes never hold atlas ids. A brush declares image formats and pass access
relationships. Runtime resource planning binds images to actual atlases.

Document load creates the document atlas and its corresponding backup atlas.
The backup atlas has the same format and lifecycle as the document atlas.
Resource management keeps a mapping:

```text
doc_atlas_id -> backup_atlas_id
```

Brush load or app resource planning prepares atlas resources for brush-owned
image formats, such as `D1` for `Pigment`.

Even if a brush-owned image has the same pixel format as the document image, it
must not share the document atlas when a pass reads that image and writes the
document atlas. This avoids wgpu read-write conflicts in a single pass.

Stage 1 can use simple runtime binding rules:

- `Target` uses the active document atlas.
- `Backup` uses the document backup atlas.
- Brush-owned images use brush temporary atlases.

Conceptually, this follows from the pass access graph, not from brush-declared
atlas domains.

## Tile Access

An empty tile binding is a logical zero tile with no physical atlas allocation.

Read acquire:

- Validates the key.
- Returns the current position.
- May return an empty sentinel.
- Does not allocate.

Write acquire:

- Validates the key.
- If the key is already physically bound, returns its position.
- If the key is empty, allocates a physical tile from the key's current atlas
  id and updates the same key binding.
- Takes `&mut TileOpRecorder`.
- When it materializes an empty tile, immediately records `ClearTile` for the
  new physical position.
- Does not create a new key.
- Does not perform backup.

This keeps empty materialization simple: any newly allocated write destination
has zero contents before the pass writes to it. Some full-overwrite passes may
not strictly need the clear, but stage 1 prefers the simpler invariant.

Tile commands may use empty positions as read operands. Execution interprets
empty read operands as zero values. Write operands must be real positions and
therefore must go through write acquire first.

## Target Copy-On-Write

`copy_on_write` is the backup-protected write acquire used for `Target`.
The tile layer does not know which image is `Target`; the session is responsible
for calling this API only for `Target` writes.

It is idempotent within a session. If the key is already session-owned, it is
returned directly.

For a physically bound target tile:

```text
allocate physical tile in backup atlas
copy active document tile into backup tile
swap bindings
return the new target key now bound to the document atlas tile
```

After this operation:

- The old key, held by `Backup`, reads from the backup atlas.
- The returned key, held by `Target`, writes to the document atlas.

For an empty target tile:

```text
allocate a new logical key bound to empty(doc_atlas_id)
return the new target key
```

There is no backup tile allocation and no copy command for an empty target tile.

Commit must still call ordinary write acquire on the returned target key before
using it as the `Merge` output. This materializes the document tile when the
target key is still empty.

Any pass that writes `Target`, whether it is a direct dab pass or a commit pass,
uses the same sequence:

```text
current_key = Target.tiles[tile]
new_key = copy_on_write(current_key, backup_atlas_id, recorder)
Target.tiles[tile] = new_key
dst_pos = acquire_for_write(new_key, recorder)
append draw or merge command using dst_pos
```

The target image key is updated immediately after `copy_on_write` returns.
`acquire_for_write` then materializes that new key if it is still empty and
records `ClearTile` as needed.

## Pixel Round Brush

The pixel round brush has two main passes.

`RoundDab` writes dab coverage into `Pigment`.

Inputs:

- target-local coordinate
- radius
- flow

Access:

```text
dst: Pigment
write semantics: accumulate with zero initialization
```

For pixel round, overlapping dabs in the single `D1` pigment channel are
combined by linear addition. This is a brush-specific detail and should not leak
into the session design unless it affects resource access.

`Merge` commits current pigment changes into `Target`.

Inputs:

- frozen brush config: color, hardness, opacity, blend mode

Access:

```text
Backup: Read
Pigment: Read
Target: Write
```

For each dirty target tile, merge is a full-overwrite operation:

```text
Target[tile] = merge(Backup[tile], Pigment[tile], frozen_config)
```

There is no extra `CopyTile(Backup -> Target)` before merge. The merge pipeline
reads `Backup` and `Pigment` and writes `Target`.

Merge config comes from brush config and is frozen for the whole brush session.
Input data may affect dab parameters such as coordinate, radius, and flow, but
it does not change merge config during one stroke.

## Input And Frame Flow

Brush logic has two parts:

- `on input` / `on dab`: consumes target-local input and produces drawing
  operations.
- `on frame` / `on commit`: uses this frame's drawing result to produce display
  or merge operations.

The brush owns sampling behavior such as spacing, interpolation, smoothing,
jitter, and pressure mapping. For example, pixel round maps pressure to radius
inside brush logic. The session does not generate dabs from raw input.

The session keeps an input queue and a tile command table. Each input item has
already been transformed into target-local image space before it reaches the
brush.

Frame processing is ordered:

```text
1. consume input and expand dab commands
2. run frame/commit operations from their read image dirty sets
3. propagate Target dirty to root and generate render commands
```

For each consumed input, the brush may produce zero, one, or many
`ImagePassCommand`s:

```text
brush.consume(input) -> Vec<ImagePassCommand>
```

These commands are produced and immediately consumed by the session. They are
not retained as a long-lived queue. The session expands them into tile commands
right away because the frame budget is measured in expanded tile commands.

The commands produced by one input are appended atomically. If they push the
tile command table over the dab budget, the session stops consuming later input
for this frame. Already consumed input and already appended commands are not
rolled back.

An input may produce no commands. That only advances brush sampling state; it
does not create dirty tiles, trigger commit, or stop the frame loop.

`Merge` is not a drawing operation. It is the display/commit operation that
makes the current pigment changes visible in `Target`. The brush provides the
commit pipeline and frozen config, but the session decides the merge tile range
from the dirty sets of the images read by the merge operation.

After merge, render commands are generated from root dirty. Render commands are
part of the same frame execution sequence, but they are not brush commands and
do not participate in the brush budget.

Stroke finish does not immediately produce a `SessionRecord`. It moves the
session from active input handling into a finishing state:

```text
Active -> Finishing -> Finished
```

Finishing is still single-threaded CPU work. No new input is accepted, but the
session continues running the same frame loop until already queued input is
drained under the normal dab budget. Each finishing frame still performs:

```text
consume input -> commit dirty images -> render Target dirty
```

The session becomes finished only when:

- the input queue is empty,
- frame/commit operations have consumed the relevant image dirty,
- any remaining `frame_dirty[Target]` has been propagated to root,
- `session_root_dirty` contains the full root-level dirty range for the stroke.

Only then can the session produce its `SessionRecord`.

## Dirty Flow

The session maintains frame dirty and session dirty separately.

Frame dirty is one dirty set per `ImageSlot`:

```text
frame_dirty: Vec<TileSet> // index == ImageSlot
```

Frame dirty is runtime intermediate state. It is consumed by commit and render
work and then cleared.

Session dirty is cumulative and target-derived:

```text
session_root_dirty: TileSet
```

It records the root-level dirty range affected by the whole brush session and
becomes part of `SessionRecord`.

All dirty entries are created by write acquire. This rule is universal: writing
`Target`, `Pigment`, or any other brush-owned image updates that image slot's
frame dirty set. Writing `Target` also contributes to the session dirty after
the target dirty is propagated to root.

There are two range sources for image operations:

- Input/dab operations are input-driven. Their range comes from the operation
  footprint, such as a circle or box in target-local image space.
- Frame/commit operations are dirty-driven. Their range is the union of dirty
  sets for the operation's read image slots.

Brushes cannot emit dirty ranges directly. They emit image-level painting intent
with a maximum possible footprint. The session turns that footprint into tile
indices using the image layout, performs access acquisition for each tile, and
updates dirty sets when writes occur.

A commit operation declares read and write image slots. The session computes:

```text
commit_range = union(frame_dirty[slot] for slot in commit.reads)
```

Then it executes the commit operation for each tile in that range. Any write
acquire during commit updates the written image's dirty set.

After commit, the dirty sets of all read image slots are cleared completely. A
commit operation is an image-level operation that logically applies to the whole
image; dirty only optimizes which tiles are executed. Stage 1 does not support a
commit operation that reads and writes the same image slot.

Brushes may declare a list of commit operations. The session executes them in
that declared order. Each operation uses the dirty state produced by all earlier
operations in the same frame:

```text
for op in commit_ops:
  range = union(frame_dirty[slot] for slot in op.reads)
  if range is empty:
    continue
  execute op over range
  clear frame_dirty[slot] for every slot in op.reads
```

Writes from one commit operation update dirty sets immediately and can drive a
later commit operation. If no commit operation writes `Target`, rendering is not
triggered by that commit chain.

Rendering is driven by `Target` dirty:

```text
frame_dirty[Target] -> propagate to root -> root_frame_dirty
session_root_dirty += root_frame_dirty
render root_frame_dirty
```

After render commands are generated for `Target` dirty, the consumed `Target`
frame dirty is cleared.

Pixel round follows the same general rule:

```text
RoundDab writes Pigment:
  frame_dirty[Pigment] += footprint tiles

Merge reads Pigment + Backup and writes Target:
  range = frame_dirty[Pigment] union frame_dirty[Backup]
  frame_dirty[Backup] is normally empty
  writing Target creates frame_dirty[Target]
  frame_dirty[Pigment] and frame_dirty[Backup] are cleared

Render consumes frame_dirty[Target] and updates session_root_dirty
```

A simple brush can write `Target` directly:

```text
Dab writes Target:
  frame_dirty[Target] += footprint tiles

Commit is empty.
Render consumes frame_dirty[Target] and updates session_root_dirty.
```

The same mechanism works whether the brush owns zero, one, or multiple
intermediate images.

## Command Budget

Dab execution and commit commands are placed into the same command table because
they must execute together.

The frame budget only limits dab commands. Once a dab command group is accepted,
all commit commands caused by that accepted work are appended to the same table
and submitted in the same frame.

The budget is a soft limit:

```text
append one dab command group
check command table size
if over budget, stop accepting more dab groups
append required commit commands
submit frame
```

There is no rollback of the last accepted dab group. A dab group is atomic: it is
either delayed as a whole before being appended, or submitted as a whole after
being appended.

The budget unit is the expanded tile-level command count, not brush op count or
dab count.

## Session Record And Replay

Tile commands are execution details and are not stored in the session record.

Undo and redo only need root switching plus dirty rendering:

```text
undo: active_root = old_root_key
redo: active_root = new_root_key
```

`SessionRecord.dirty_tiles` is root-level dirty accumulated over the whole brush
session. It comes from `session_root_dirty`, not from any remaining per-frame
image dirty state. In stage 1, dirty propagation is identity, so root dirty and
target dirty have the same tile indices, but the record semantics are still
root-level.

Brush-owned retained images, such as `Pigment`, preserve intermediate state for
future replay. A future replay implementation can enumerate non-empty pigment
tiles and re-run the commit logic. A future resource downgrade step may release
retained intermediate resources and keep only undo/redo capability.

Stage 1 records the design intent for retained temp resources, but replay and
resource downgrade are not implemented yet.

## Errors

Stage 1 does not implement local rollback for partially prepared frame work. If
resource allocation, key validation, pipeline setup, or command expansion fails,
the error is treated as a session-level failure. The caller should not commit a
failed session as a valid brush record.

Resource cleanup after failed sessions is a later lifecycle concern.
