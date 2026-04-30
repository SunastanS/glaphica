# Double Initial Dab from Race Between Reset and Blocked Drain

## Problem

- **Symptom**: every stroke began with a "double dot" — two dabs at (or very close to) the pen-down position. GUI inspection could not distinguish the two positions.
- **Visual interpretation**: the first `CursorMoved` after `MouseInput::Pressed` produced a second `spans=0` dab at the stroke origin, on top of the first `spans=0` dab from the press itself.
- **Stable reproduction**: consistent on every stroke with default settings (`base_radius_px=20`, `spacing_ratio=1.0` → `dab_spacing=20px`).
- **Not a recent regression in the smoother/sampler**: the arc-length cursor tracking correctly prevents re-sampling at `s=0`; the cursor advances by `spacing` after the first dab, so the sampler cannot produce a second sample at the origin under normal conditions.

## Root Cause

The bug is a **race condition** between the main thread calling `reset_active_stroke_processing()` and the brush thread blocked inside `process_canvas_input` waiting on the canvas ring buffer.

### Sequence

```
  brush thread: blocked in drain_batch_with_wait (ring empty, 1ms timeout)
       ↓
  main thread: reset_active_stroke_processing()
       ├─ stroke_generation += 1
       ├─ canvas_input_producer.clear()    ← no notify
       └─ brush_input_consumer.clear()
       ↓
  main thread: canvas_input_producer.push(first_input)   ← notify
       ↓
  brush thread: wakes up STILL INSIDE process_canvas_input
       ├─ drains first_input from ring
       ├─ push_canvas_inputs([first_input])   ← OLD processor (pre-reset)
       ├─ drain_brush_input → pop_stable_spans → initial point emitted
       └─ pushes stale BrushInput (spans=0) to ring
       ↓
  brush thread: returns to loop top
       ├─ loads current_generation → changed!
       └─ worker.reset_active_stroke()   ← cursor reset to 0
       ↓
  main thread: CursorMoved → push(second_input)
       ↓
  brush thread: process_canvas_input with FRESH processor
       └─ produces second spans=0 dab at (nearly) same position
```

The generation check at the loop top is **too late** — it happens after `process_canvas_input` has already consumed and processed the first input with the wrong processor.

### Why `clear()` does not wake the brush thread

`OverwriteRingProducer::clear()` takes the mutex, clears the queue, and releases the mutex. It does **not** call `notify_one()`. So if the consumer is blocked in `drain_batch_with_wait` → `Condvar::wait_timeout`, it stays blocked. Only the subsequent `push` (which calls `notify_one`) wakes it — but by then the damage is done: the consumer wakes up inside `process_canvas_input` and processes the just-pushed input with whatever processor it already had.

### Why the sampler cursor did not prevent this

The cursor tracking works correctly **within a single processor lifetime**. The bug creates **two processor lifetimes**: the old processor handles the first input (cursor advances to `spacing`), then `reset_active_stroke()` creates a conceptually new stroke with a new cursor at `0`. The second input is processed as a fresh stroke — hence a second `spans=0` dab.

### Why existing tests did not catch this

Existing tests call `process_canvas_input` synchronously with `Duration::ZERO` — no blocking, no interleaving. The race requires the brush thread to be blocked in the wait while the main thread changes state.

## Fix

The fix is in `process_canvas_input` (in `crates/app/src/input/brush_worker.rs`). The generation must be checked **after** `drain_batch_with_wait` returns, not before the call:

```rust
pub fn process_canvas_input(
    &mut self,
    canvas_input_consumer: &BrushThreadCanvasInputConsumer,
    brush_input_producer: &BrushThreadBrushInputProducer,
    max_batch_size: usize,
    wait_timeout: Duration,
    stroke_generation: &AtomicU64,       // reference, not pre-snapshotted value
    seen_generation: &mut u64,
) -> Result<usize, BrushWorkerError> {
    self.canvas_batch.clear();
    canvas_input_consumer.drain_batch_with_wait(&mut self.canvas_batch, ...);

    // Load generation AFTER the drain — captures changes during the wait.
    let current_generation = stroke_generation.load(Ordering::Relaxed);
    if current_generation != *seen_generation {
        self.reset_active_stroke();
        *seen_generation = current_generation;
    }

    // ... continue processing — batch is processed with (possibly reset) processor
```

Key properties of this fix:

- The drained batch is **not discarded** — it is processed with the fresh (post-reset) processor.
- The first input lands at the correct press position, not lost.
- No post-hoc `brush_input_producer.clear()` needed; `run_brush_thread` removes that workaround.

## Regression Test

`generation_change_during_wait_resets_before_processing_first_input` in `crates/app/src/input/brush_worker.rs`:

1. Spawns a worker thread blocked in `process_canvas_input` (ring empty, 10s timeout).
2. Main thread bumps `generation` from 0→1 while the worker is blocked.
3. Main thread pushes the first `CanvasInput` at a known `press_position`.
4. Worker wakes up, loads generation (sees 1≠0), resets, processes the batch with fresh processor.
5. Asserts:
   - `seen_gen` updated to 1 (reset happened inside `process_canvas_input`)
   - output produced (batch **not** discarded)
   - exactly one `BrushInput` with one block
   - dab position matches `press_position`

## Useful Diagnostics

The fastest way to localize the bug was adding `eprintln!` in `RoundBrushStrokeSampler::sample_brush_input` logging every dab's `spacing`, `span_count`, and position. The log immediately showed:

```
[DEBUG][dab] spacing=56.000 idx=0 pos=(110.935,79.795) dup=false spans=0
[DEBUG][dab] spacing=56.000 idx=0 pos=(111.401,79.795) dup=false spans=0   ← second spans=0!
[DEBUG][dab] spacing=56.000 idx=0 pos=(152.128,80.435) dup=false spans=3
...
```

Two `spans=0` dabs at nearly identical positions confirmed the processor was being reset and producing a second "first dab." The `spans=0` (single-knot, no span) is the signature of the initial-point emission path in `pop_stable_spans`.

## Lessons

- A generation/epoch pattern that relies on a loop-top check is vulnerable to wake-ups inside blocking calls. Always re-check the epoch **immediately after** the blocking call returns, before acting on the returned data.
- `clear()` on a ring buffer should consider `notify` if consumers are expected to wake and observe the cleared state. Here the missing notify was part of the race, but the root cause was the stale generation check, not the notification.
- The sampler cursor provides a strong invariant (no re-sampling at `s=0`), but `reset_active_stroke()` breaks it by creating a new cursor. This cross-layer invariant violation was the key to understanding the bug.
- Future direction: consider merging reset/begin/input/finish into a single command queue to eliminate dual-channel ordering problems entirely.
