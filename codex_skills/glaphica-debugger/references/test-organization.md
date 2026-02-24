# Test Organization (This Repo)

## Where Unit Tests Usually Live

Most crates use the pattern:
- `crates/<crate>/src/lib.rs` contains `#[cfg(test)] mod tests;`
- Unit tests live in `crates/<crate>/src/tests.rs`

Example commands:
- Run a single test by name:
  - `cargo test -p <crate> <test_name> -- --nocapture`
- Reduce flakiness for GPU-related tests:
  - `cargo test -p <crate> <test_name> -- --nocapture --test-threads=1`

## When to Use Root `tests/`

Use repo-root `tests/` when you need cross-crate integration behavior or a harness that doesn’t fit a single crate’s unit tests.

## Practical Workflow

1. Find the owning crate of the code under test.
2. Prefer adding a new focused `#[test]` in that crate’s `src/tests.rs`.
3. Name tests by behavior/invariant (not by function name).
4. Make the invariant crisp:
   - “does not panic”
   - “pixel-exact readback”
   - “mapping uniqueness”
5. If the test needs GPU access and may be environment-sensitive, mark it `#[ignore]` and document how to run it.

