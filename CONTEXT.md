# Glaphica Context

This context records image-level drawing language for the Rust execution layer.
The upper management layer owns document trees and user tools; Rust executes
compiled image programs over images, tiles, sessions, frames, and dabs.

## Execution Tiers

**Global**:
The document-level image table and tile resource state outside an active stroke.
Global should stay close to storage: image roles, resource rows, document
version, and derived cache residency.
_Avoid_: engine, document service

**Session**:
A per-stroke execution program built from `DrawSessionIR`. Session owns IR
lowering, active document chain shadows, the session-local image table, and
stroke-level commit.
_Avoid_: frame, transaction

**Frame**:
A short-lived batch inside a Session. During preparation, Frame accepts
already-lowered DrawOn work and records intended frame edits and dirty starts
without publishing them into Session.
_Avoid_: session, flush service

**Frame Handoff**:
The final Frame lifecycle step that publishes one Frame's staged image changes
into its owning Session. Frame Handoff is not Session Commit; document
publication remains a Session-level action.
_Avoid_: session commit, global apply

**Frame Dirty**:
The set of logical image tile slots changed by one Frame. Frame Dirty is staged
execution state, separate from DrawOn Pass ordering and from durable history.
_Avoid_: renderer pass list, undo record

**Dab**:
A single high-frequency DrawOn invocation produced from routed input and tool
lowering. Dab execution should remain a tight Rust loop over affected tiles and
built-in primitive rules.
_Avoid_: stroke, brush preset, IR command

## DrawOn

**DrawOn**:
The built-in executor primitive layer for input-driven image editing. DrawOn
primitives are selected from a static built-in list; they are not runtime
plugins.
_Avoid_: brush, runtime extension

**DrawOn Primitive**:
A built-in edit primitive such as `RadialKernel1D` or `ReplaceCircle4D`, with
fixed input, target format, footprint, and GPU behavior rules.
_Avoid_: user tool, shader plugin

**DrawOn Input**:
Typed per-Dab payload already lowered from app input, route, and tool config.
DrawOn Input is hot-path data and should not carry IR parsing responsibilities.
_Avoid_: CanvasInput, brush config

**DrawOn Pass**:
Frame-buffered logical DrawOn work targeting image tile slots. DrawOn Pass is
not a Renderer Pass and does not target atlas positions; Frame Handoff resolves
it through Session state.
_Avoid_: public command, IR operation

**Image Tile Slot**:
A logical tile location inside an image, identified by image identity and the
tile's layout index. Image Tile Slot is distinct from an atlas position.
_Avoid_: TilePos, atlas slot

**Renderer Pass**:
The full renderer work item generated when a Frame is flushed. Frame preparation
stores only DrawOn Pass values; Renderer Pass values are created at the end of
the Frame after DrawOn batching and gutter work are known.
_Avoid_: DrawOn Pass

**DrawOn GPU Behavior**:
The renderer-side behavior paired with a DrawOn Primitive. This is where DrawOn
Input becomes GPU mutation, and each primitive may have a different execution
strategy.
_Avoid_: generic draw plugin

## Flagged Ambiguities

**DrawOn vs brush**:
Brushes are user-facing tools and presets. DrawOn is the compiled executor
primitive layer that brushes may lower into.

**DrawOn Pass vs command**:
DrawOn Pass is internal renderer work buffered for batching. Command refers to
image-level derive or graph execution, not the public shape of DrawOn.

## Example Dialogue

Developer: Should Frame inspect `DrawSessionIR` to decide which DrawOn primitive
to run?

Domain expert: No. Session lowers IR before Frame starts. Frame should receive
typed DrawOn Input and run the hot Dab loop against built-in DrawOn Primitive
rules.

Developer: Can users register new DrawOn primitives at runtime?

Domain expert: No. DrawOn primitives are built in. User tools and brushes lower
to that static list.

Developer: Are DrawOn Pass values part of the public DrawOn interface?

Domain expert: No. They are internal communication data buffered at Frame level
so DrawOn GPU Behavior can batch work efficiently.
