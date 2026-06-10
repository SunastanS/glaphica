# Review ADR 02: Reject Zero-Size Images at Creation

- **Status**: Proposed

## Summary

`GlaImageLayout` with `width_px=0` or `height_px=0` produces `tile_count()=0`.
`GlaImage::new()` accepts empty tile arrays (0 == 0), allowing zero-size images
to be created. All subsequent tile access operations at tile_index 0 fail with
`TileIndexOutOfBounds` because there are no tiles. The image should be rejected
at creation time.

## Recommendation

Add a validation check in `GlaImageLayout::new()` or `GlaImage::new()` that
rejects layouts with zero width or height. The simplest approach is to return
an error from `GlaImage::new()` when `layout.tile_count() == 0`.

## Safety Conditions

- Must be applied before any code relies on the existence of zero-size images.
- A dedicated error variant (`ZeroSizeImage`, `InvalidLayout`, etc.) should
  be added to `GlaImagesError`.
