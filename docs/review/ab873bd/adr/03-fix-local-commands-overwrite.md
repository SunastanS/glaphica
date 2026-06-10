# Review ADR 03: Fix local_commands Overwrite When Session Image Shadows Doc Image

- **Status**: Proposed
- **Related**: Architecture finding — ID resolution bug

## Summary

`DrawSession::new()` inserts commands into `local_commands` in three phases:
1. Session derived declarations (lowered via `lower_session_command`)
2. Active chain doc graph commands (lowered via `lower_graph_command`)
3. IR derive commands (lowered via `lower_session_command`)

Phase 2 uses `HashMap::insert` which silently overwrites any command already
present for the same key. If a session derived image (phase 1) shadows a
Read-only doc derived image that is also in the active chain (phase 2), the
doc graph command overwrites the session derived command — the wrong command
wins.

## Root Cause

Session local images and (ReadWrite & doc derived) images share the same
`ImageId` namespace for Janet-layer convenience, but the Rust layer does not
properly distinguish them during command lowering. Phase 2 should not overwrite
commands already inserted by phase 1.

## Recommendation

1. Phase 2 (active chain doc commands) should use `HashMap::entry().or_insert()`
   to avoid overwriting session-local commands.
2. Alternatively, session derived declarations should explicitly prevent
   shadowing of doc derived images that are in the active chain.
3. Long-term: separate namespace for session-local vs document image keys to
   eliminate the ambiguity at the type level.
