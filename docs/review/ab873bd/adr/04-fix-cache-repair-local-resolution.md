# Review ADR 04: Cache Repair Must Use Doc-Only Resolution

- **Status**: Proposed
- **Related**: Backup read semantics violation

## Summary

`render_impl()` performs cache repair for doc derived images with INVALID tiles
by calling `self.lower_graph_command(command)`. This function resolves read
sources using `local_keys → doc_bindings` (local-first). But cache repair
should read only document images, not session-local shadows.

This causes two problems:
1. **Backup read transitivity violation**: A backup read of derived image D
   triggers cache repair of D's INVALID tile. D's graph command reads its
   dependencies via local-first resolution, potentially using session-modified
   versions. The backup guarantee ("session-start document state") only holds
   for the directly-read image, not its transitive dependencies.
2. **Non-deterministic cache content**: The same doc derived cache tile may
   receive different content depending on whether it was repaired during a
   session (using session-local state) or outside a session (using only doc
   state).

## Recommendation

`render_impl` should use a variant of graph command lowering that resolves
all reads exclusively from `doc_bindings` (producing only `SessionImageKey::Doc`),
never from `local_keys`. The existing `lower_graph_command` with local-first
resolution should only be used for the active chain shadow command insertion
(phase 2 of init), where session-local overrides are intentional.
