# Round Brush Close Dabs During Fast Straight Motion

## Problem

- Symptom: when drawing a fast straight stroke with the round brush, the stroke showed random darker spots.
- Visual interpretation: a few semi-transparent dabs were landing much closer than the configured spacing.
- Important negative signal: slow motion did not reproduce the issue reliably.

## Root Cause

The bug was in `SpanArcTable`, not in the render path and not in the smoothing window logic.

- `CommittedCanvasSpanBuffer::sample_by_arclength_from` samples by global arclength.
- That relies on `SpanArcTable::t_at_cumulative_s` to invert `s -> t` inside each span.
- `SpanArcTable::from_span` originally subdivided only by geometric flatness.
- For spans that were geometrically almost straight but had very uneven parameterization, the table could stay too sparse.
- In that case `t_at_cumulative_s` effectively performed a coarse linear interpolation across a large `t` interval.
- Result: a query for roughly `5px` of local arclength could map to a `t` whose geometric position had advanced much less, producing dabs that were too close together.

This showed up most easily on very short-duration spans with highly asymmetric endpoint velocities.

## Fix

After the usual geometric subdivision, the arclength table is additionally densified by a maximum `t` step.

That keeps `s -> t` inversion stable even when:

- the span is visually almost a line
- but `s(t)` is strongly non-linear

The implementation lives in `crates/brush/src/smoother.rs`:

- `SpanArcTable::from_span`
- `densify_arclength_samples`

## Useful Diagnostics

The fastest way to localize the bug was to assert a spacing invariant at multiple layers:

- outer layer: round brush emitted centers must stay farther apart than `spacing / 2`
- inner layer: arclength sampler outputs must also satisfy the same bound

Once the panic moved from `round.rs` into `sample_by_arclength_from`, the render path was effectively ruled out.

The decisive data points were:

- current and previous global `s`
- per-span `local_s`
- `t`
- span chord length
- span total arclength

That showed:

- total span length was often correct
- but local `s -> t` inversion inside a sparse arclength table was not

## Lessons

- Flatness-only subdivision is not sufficient for arclength lookup tables.
- A curve can be geometrically straight and still need denser sampling for parameter inversion.
- When tracking “random dark spots”, assert on emitted dab spacing first; it separates sampling bugs from rendering bugs quickly.
