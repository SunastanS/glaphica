# Reliability Record: Tile Lifecycle in Undo/Redo

- **Code**: `apply_image_edit_patch()`, `DrawHistory`, `Tiles::discard()`
- **Classification**: Suspect — missing cleanup
- **Why**: `apply_image_edit_patch` replaces tile keys in document images but never
  discards the replaced tiles. Old tiles are retained by `DrawHistory` patches with
  no visible cleanup mechanism. `apply_derived_edits` correctly discards replaced
  derived cache tiles, but primitive tile cleanup is missing. Human confirms this
  is a resource leak that needs fixing.
- **Impact on analysis**: Do not use the current tile lifecycle as evidence of
  intended design. Tests that check tile retention in history should be aware
  that tiles retained by history patches may eventually need to be discarded.
