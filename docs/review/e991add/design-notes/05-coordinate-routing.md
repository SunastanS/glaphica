# Coordinate Routing And Tile Footprints

This note records the current coordinate-routing direction for draw sessions.

## Mapping Direction

`Mapping` is interpreted as destination coordinates to source coordinates:

```text
read edge: dst image coord -> read/source image coord
```

Render footprint enumeration starts from a destination tile and uses the read
edge to find source tiles. Dirty upload walks the same read edge in the reverse
conservative direction.

Draw input is anchored by the app. Each shown image belongs to a window or view,
and the app passes that shown `ImageId` when submitting a dab. The session then
routes input through the active session graph from the shown image toward
reachable `DrawOn` writers.

## Input Routing

Input routing uses the active session graph, not raw global storage. It walks
only `SessionImageId::Current` read edges; `Global` reads represent backup or
global-only sources and are not writable input routes.

The route starts at the shown image. If the shown image itself is a `DrawOn`
target, it receives input. If the shown image derives from one or more current
images, input is passed down those read edges until `DrawOn` targets are found.
Targets execute in `draw_on_order`.

Tool input lowering combines raw input and tool config into the tool's standard
input after route mapping has put the input point in the target image coordinate
system. `DrawOnCommand` no longer carries a coordinate `input_mapping` field.

## Ambiguity TODO

If the same `DrawOn` target is reachable from one shown image through multiple
paths, the route is ambiguous. First implementation should reject that input
with an explicit session error. A later design can decide whether to allow
multi-path input by requiring named routes, explicit route priority, or an
unambiguous graph constraint.

## First Footprint Scope

The first stable footprint implementation supports `Identity + None` precisely
with layout-aware tile rectangles. `Matrix` and `Expand` remain conservative for
dirty upload and unsupported for render/source footprints until sampling and
expanded footprint semantics are defined.
