# Max Dab Seams

## Behavior

In `RoundApplyDabBlendMode::Max`, large soft round dabs show visible seams at dab boundaries. The reported screenshot shows vertical low-alpha bands between neighboring dabs.

Expected behavior for a continuous brush stroke is no visible centerline seam between adjacent dabs.

## Reproduction

The behavior is reproduced by the unit test:

```text
cargo test -p brush max_blend_centerline_seams_when_spacing_exceeds_hardness_diameter -- --nocapture
```

The test models a straight stroke with:

- `spacing_ratio = 1.0`
- `hardness = 0.3`
- `stroke_flow = 1.0`
- `RoundApplyDabBlendMode::Max`

It verifies that the center of a dab merges to full coverage while the midpoint between adjacent dabs merges below full coverage.

The related monotonicity invariant is covered by:

```text
cargo test -p brush max_blend_dab_sequence_never_reduces_intermediate_source
```

That test verifies that applying a second Max dab never lowers the same sampled intermediate source value.

## Investigation

The apply shader for Max writes:

```text
intermediate = max(current, dab)
```

For a line of identical dabs this means the centerline source is:

```text
source(x) = max_i K(distance(x, dab_i) / radius)
```

Merge receives only this scalar `source`, and currently maps it as:

```text
coverage = clamp(source / threshold, 0, 1)
```

For Max mode, the threshold is the single-dab hardness boundary:

```text
threshold = K(hardness) * stroke_flow
```

At a dab center, source is `K(0) = 1`. At the midpoint between two dabs, source is `K(spacing_ratio / 2)`. Therefore a seam is mathematically expected whenever:

```text
K(spacing_ratio / 2) < K(hardness)
```

Since `K` is monotonically decreasing, this is equivalent to:

```text
spacing_ratio / 2 > hardness
```

With the current kernel exponent `a = 2.0`, example values are:

```text
spacing=1.00 hardness=0.30 threshold=0.8281 midpoint=0.5625 smooth coverage=0.7574
spacing=0.80 hardness=0.30 threshold=0.8281 midpoint=0.7056 smooth coverage=0.9408
spacing=0.50 hardness=0.30 threshold=0.8281 midpoint=0.8789 smooth coverage=1.0000
```

## Screenshot Pixel Analysis

For `./screenshot.png`, using:

```text
ink = 255 - (R + G) / 2
coverage = ink / 255
```

on the center row `y = 274`:

```text
seam center x=422 rgb=(60, 60, 255) coverage=0.7647
left/right shoulder coverage around x=402/442 ~= 0.8784
saturated dab body coverage = 1.0
```

The seam is therefore measurable and not just an optical illusion, but it is not near zero. The value is close to the model prediction for `spacing_ratio = 1.0`, `hardness = 0.3`, `a = 2.0`:

```text
K(0.5) = 0.5625
threshold = K(0.3) = 0.8281
raw = K(0.5) / K(0.3) = 0.6793
smoothstep(raw) = 0.7574
```

The screenshot seam coverage `0.7647` is within the expected range after 8-bit quantization and sampling.

If a later screenshot shows the same pixel lowering after an additional dab rather than simply being the fixed midpoint between two Max dabs, that should be investigated with intermediate readback.

## Root Cause

For the centerline seam case reproduced above, this is not a GPU readback, atlas, or merge shader lookup issue. It is a model mismatch:

- Max blending creates a union of independent radial dab fields.
- A continuous soft stroke expects a swept brush footprint or a spacing-aware accumulated source field.
- Merge only sees a scalar intermediate source, so it cannot distinguish "centerline between dabs" from "soft edge of one dab" when both have the same source value.

If the same pixel visibly becomes lighter after a later dab, that is a different bug. Max blending should preserve:

```text
intermediate_after_next_dab >= intermediate_before_next_dab
```

The CPU model satisfies that invariant. A violation in the app should be localized with GPU readback of the R16 intermediate before and after the later dab, or with final preview readback if intermediate remains monotonic.

## Fix Status

No production behavior was changed in this debug step. The current Max mode is correctly implementing `max(current, dab)`, but that operation does not guarantee a seamless stroke for `spacing_ratio / 2 > hardness`.

Possible design-level fixes:

- Lower spacing so `spacing_ratio <= 2 * hardness` for Max mode.
- Use `LinearAdd` or `Multiply` for continuous strokes where overlap should fill seams.
- Change ApplyDab from point dab stamping to a swept segment/capsule operation for Max mode.
- Add a separate merge interpretation that intentionally treats Max mode as a connected stroke, accepting that this no longer represents pure `max(current, dab)` semantics.

## Verification

Ran:

```text
cargo test -p brush
```

Result:

```text
51 passed
```
