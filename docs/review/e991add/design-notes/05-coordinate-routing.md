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

Draw input is anchored by the app. Each shown image belongs to a window or view.
The input mapping layer can call `DrawFrame::route_draw_targets` to route that
shown `ImageId` and point through the active session graph toward reachable
`DrawOn` writers.

## Input Routing

Input routing uses the active session graph, not raw global storage. It walks
only `SessionImageId::Current` read edges; `Global` reads represent backup or
global-only sources and are not writable input routes.

The route starts at the shown image. If the shown image itself is a `DrawOn`
target, it receives input. If the shown image derives from one or more current
images, input is passed down those read edges until `DrawOn` targets are found.
Targets are returned in `draw_on_order`.

The route query does not lower raw app input into tool input and does not execute
DrawOn. Tool input lowering combines routed target coordinates, raw app input,
and brush/tool config outside the session, producing typed per-DrawOn
`DrawOnInput` values. The frame executes those inputs through
`DrawFrame::draw_on`. `DrawOnCommand` no longer carries coordinate mapping or
tool parameter fields.

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
