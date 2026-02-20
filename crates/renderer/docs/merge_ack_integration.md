# Merge Ack Integration Contract (Renderer <-> Tiles)

## Scope

This document defines how `tiles` should consume renderer merge completion data after
the ack-path refactor:

- `Renderer::poll_completion_notices` only reports completion notices.
- `Renderer::ack_merge_result` is the only state transition entry for receipt settlement.

This contract is intentionally limited to renderer-facing integration.

## Responsibilities

### Renderer layer

- Own GPU submission and completion polling.
- Emit `MergeCompletionNotice` records.
- Never auto-ack in polling.
- Keep receipt state transition in `ack_merge_result` only.

### Tiles (renderer caller)

- Consume renderer completion notices.
- Execute unified ack for each notice via `renderer.ack_merge_result(notice)`.
- Own any higher-level aggregation policy outside renderer.

## API Contract

## Renderer outbound data (implemented)

`MergeCompletionNotice`:

- `receipt_id: StrokeExecutionReceiptId`
- `audit_meta: MergeAuditMeta`
- `result: MergeExecutionResult`

`Renderer::poll_completion_notices(frame_id) -> Result<Vec<MergeCompletionNotice>, MergePollError>`

Semantics:

- Must be called by frame-driven runtime (scheduler tick path).
- Returns completed submission-derived notices only.
- Does not mutate receipt terminal outcome.

## Tiles inbound action

For each completion notice:

1. Call `renderer.ack_merge_result(notice)`.
2. Record ack result according to tiles-level policy.

Recommended tiles-side aggregation shape:

```rust
pub struct TileMergeBatchAck {
    pub frame_id: u64,
    pub total: usize,
    pub succeeded: usize,
    pub failed: Vec<TileMergeAckFailure>,
}

pub struct TileMergeAckFailure {
    pub receipt_id: render_protocol::StrokeExecutionReceiptId,
    pub message: String,
}
```

Notes:

- This type is only a renderer integration suggestion; keep final shape in `tiles` crate.
- `failed` can hold one or many entries based on product policy.

## Runtime Sequence

1. Operation path (downstream):
   - `tiles -> renderer.enqueue_planned_merge/submit_pending_merges`
2. Frame tick (scheduler-owned):
   - scheduler calls renderer poll path once per frame
   - renderer returns completion notices
3. Settlement path (upstream):
   - tiles receives notices
   - tiles performs unified ack through renderer
   - tiles maps ack outcomes into tiles-domain results

## Invariants

- Receipt settlement is single-path only: `ack_merge_result` with a notice produced by
  `poll_completion_notices`.
- Polling path must not call ack implicitly.
- Duplicate ack remains fail-fast (`MergeAckError::IllegalState`).
- Renderer does not define how tiles forwards outcomes further upstream.

## Retention policy

- `ack_receipt_terminal_state(Finalized|Aborted)` removes the receipt entry immediately.
- A submission entry is removed when all its receipt IDs have reached terminal cleanup.
- This prevents old submission/receipt data from accumulating across long sessions.

## Failure Handling Rules

- If polling returns `Err(MergePollError)`, tiles must surface it through its own error
  channel in the same frame tick.
- If ack fails for a receipt, record per-receipt failure and continue processing remaining
  notices for deterministic batch reporting.

## API naming and usage

- `poll_completion_notices`: source of truth for ackable completion data.
- `drain_receipt_progress_events`: status/event stream for observers only.
- Only `poll_completion_notices` output may drive `ack_merge_result`.

## Current limits

- Renderer maps device-level uncaptured errors and lost events to all in-flight merge
  submissions for fail-fast behavior.
- Per-operation GPU fault attribution is not guaranteed yet and requires deeper GPU scope
  instrumentation.
