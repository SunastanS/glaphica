# ADR 002: Frame Handoff Mutates Session State

Frame preparation stores logical image tile work rather than owning tile
resources. At the end of the frame, Frame Handoff may mutate Session image state
while resolving that logical work into renderer work; if renderer submit then
fails, the frame keeps the pending submit locked and dropping it aborts the
whole Session. This deliberately keeps CoW and first-write ownership logic in
Session instead of recreating a second `ImageEdit`-like ownership model inside
Frame, accepting that the worst failed-submit outcome is discarding the active
stroke.

If Frame Handoff or dirty/render pass generation fails before a renderer submit
is pending, the Session is aborted rather than trying to reconstruct pre-handoff
state. Only renderer submit failure keeps generated passes pending for retry.

Frame Handoff resolves first-write initialization in a tile batch before
DrawOn execution. All clears/copies for newly edited image tile slots may be
emitted before all DrawOn renderer work, while DrawOn invocations keep their
original order. This is correct because first-write initialization establishes
the pre-DrawOn tile value and has no dependence on the order between DrawOn
invocations.

For `ImageEdit` targets, first-write initialization is based on image tile slots
that are dirty in the Frame but absent from the Session edit set. For `Raw`
session targets, slots already exist as valid session content; resolving an
empty binding may still allocate and clear physical storage, but it is not CoW
first-write from document state.
