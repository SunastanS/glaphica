# wgpu Submit Hazards (CPU→GPU Ordering)

This repo’s canonical notes live in `docs/Instructions/wgpu.md`. This reference is a short checklist.

## The Classic Footgun

If you:
1) encode multiple passes into one `CommandEncoder`, and
2) call `queue.write_buffer(buffer, offset=0, ...)` multiple times before *one* `queue.submit`,

then earlier passes may read the *last* write, because writes are only guaranteed to complete (in order) before GPU execution of the submit, not “per-pass”.

Typical symptom:
- “All outputs look like the last op” (e.g. multiple merges all apply the same uniform values).

## Safe Patterns

- Use dynamic offsets or an append-only arena so each pass reads a distinct range.
- Or temporarily “stop the bleeding” by submitting per op:
  - write uniforms
  - encode
  - `queue.submit`

## Debugging Pattern

When you see content repetition with unique keys/addresses:
1. Add an assertion/log that shows per-op parameters differ on CPU.
2. If parameters differ on CPU but results are identical, suspect submit/write hazards.
3. Make the behavior testable (minimal test with 2+ distinct ops).

