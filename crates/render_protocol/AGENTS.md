# AGENTS.md (crates/render_protocol)

## Scope

These instructions apply to everything under `crates/render_protocol/`.

## What This Crate Is

`render_protocol` defines the cross-module message/data types used for rendering, brush, merge,
and render-tree communication. Changes here ripple across multiple crates.

## Collaboration Rule for Field Changes (Must Follow)

This crate encodes a coordination rule at the top of `crates/render_protocol/src/lib.rs`:

- Receiver/executor side may implement first and then report.
- Initiator/caller side must report first and only modify after approval.

Apply this rule to all message-passing fields defined in this crate.

## Change Policy

- Prefer additive changes (new fields/types) over breaking changes unless the failure is
  fully understood and the migration plan is explicit.
- Do not “guess” a protocol change to fix a bug. First localize the bug with logs/tests.
- Keep types minimal and deterministic; avoid adding incidental allocations in hot paths.

## When Editing Protocol Types

Before changing any struct/enum field:
1. Identify sender(s) and receiver(s) crates and the direction of the message flow.
2. Confirm which side is the initiator/caller vs receiver/executor (per the rule above).
3. Add/adjust a focused regression test in the owning crate that motivated the change.
4. Update all call sites in the repo in the same change (do not leave partial migrations).

## Tests

`render_protocol` has unit tests in `crates/render_protocol/src/lib.rs` under `#[cfg(test)]`.
Run with:
- `cargo test -p render_protocol -- --nocapture`

