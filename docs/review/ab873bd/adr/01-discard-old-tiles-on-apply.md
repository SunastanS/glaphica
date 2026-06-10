# Review ADR 01: Discard Old Tiles When Applying Edit Patches

- **Status**: Proposed

## Summary

`apply_image_edit_patch` replaces tile keys in document images but does not call
`Tiles::discard()` on the old tiles being replaced. Old tiles are retained only
by `DrawHistory` patches. When history is truncated or a patch is evicted, the
old tiles leak permanently.

## Recommendation

Add `Tiles::discard()` calls in `apply_image_edit_patch` for old tiles that are
being replaced by new tiles. Old tiles that are referenced by `DrawHistory`
inverse patches must be kept alive until the patch is evicted.

## Safety Conditions

- Must not discard tiles that are still referenced by any active `DrawHistory`
  patch (if history truncation is not yet implemented, all old tiles in history
  patches must be kept).
- Must coordinate with future history truncation/eviction mechanism.
- Derived cache tiles in `apply_derived_edits` are already correctly discarded
  — this pattern should be replicated for primitive tiles.
