# Testing Patterns

**Analysis Date:** 2026-03-05

## Test Framework

**Runner:**
- Built-in Rust test framework (`cargo test`)
- Config: No custom test configuration

**Assertion Library:**
- Standard `assert!`, `assert_eq!`, `assert_ne!` macros

**Run Commands:**
```bash
cargo test                # Run all tests
cargo test --package <crate>  # Run tests for specific crate
cargo test --test <test_name> # Run specific test
```

## Test File Organization

**Location:**
- Inline with source files in `#[cfg(test)]` modules
- No separate `tests/` directory

**Naming:**
- Test functions named with `#[test]` attribute
- Descriptive names: `mailbox_merge_is_idempotent_and_uses_max_waterlines`

**Structure:**
```
crates/thread_protocol/src/lib.rs
  ├── #[cfg(test)]
  └── mod tests {
        #[test]
        fn mailbox_merge_is_idempotent_and_uses_max_waterlines() { ... }
      }
```

## Test Structure

**Suite Organization:**
```rust
#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn mailbox_merge_is_idempotent_and_uses_max_waterlines() {
        let current = GpuFeedbackFrame { ... };
        let newer = GpuFeedbackFrame { ... };
        
        let mut merge_state = GpuFeedbackMergeState::default();
        let once = GpuFeedbackFrame::merge_mailbox(current, newer.clone(), &mut merge_state);
        let twice = GpuFeedbackFrame::merge_mailbox(once.clone(), newer, &mut merge_state);
        
        assert_eq!(once, twice);
    }
}
```

**Patterns:**
- Arrange-Act-Assert pattern
- Multiple assertions per test
- Helper structs defined within test module

## Mocking

**Framework:** None - uses real implementations

**Patterns:**
- Test-specific implementations for traits:
```rust
#[derive(Debug, Clone, PartialEq, Eq)]
struct TestReceipt {
    key: u64,
    payload_version: u64,
}

impl MergeItem for TestReceipt {
    type MergeKey = u64;
    
    fn merge_key(&self) -> Self::MergeKey {
        self.key
    }
    
    fn merge_duplicate(existing: &mut Self, incoming: Self) {
        if incoming.payload_version > existing.payload_version {
            *existing = incoming;
        }
    }
}
```

**What to Mock:**
- Trait implementations for test scenarios
- Custom error types for error handling tests

**What NOT to Mock:**
- Core types and primitives
- Business logic (test actual implementations)

## Fixtures and Factories

**Test Data:**
```rust
let current = GpuFeedbackFrame {
    present_frame_id: PresentFrameId(10),
    submit_waterline: SubmitWaterline::new(2),
    executed_batch_waterline: ExecutedBatchWaterline::new(3),
    complete_waterline: CompleteWaterline::new(4),
    receipts: vec![TestReceipt {
        key: 1,
        payload_version: 10,
    }],
    errors: vec![TestError { key: 2 }],
};
```

**Location:**
- Inline within test functions
- No separate fixture files

## Coverage

**Requirements:** None enforced

**View Coverage:**
```bash
cargo tarpaulin --out Html  # If tarpaulin installed
```

**Coverage Status:** Not actively tracked

## Test Types

**Unit Tests:**
- Scope: Individual functions, data transformations, merge logic
- Approach: Inline `#[cfg(test)]` modules
- Focus: Pure logic, no GPU/windowing dependencies

**Integration Tests:**
- None detected (would require GPU mocking or headless rendering)

**E2E Tests:**
- Not used (desktop application with GPU requirements)

## Common Patterns

**Async Testing:**
- No async tests detected (GPU init uses `.await` in main code)
- Tokio runtime used in main application, not tests

**Error Testing:**
```rust
#[test]
#[should_panic(expected = "current vector contains duplicated merge key before merge")]
fn mailbox_merge_panics_when_current_contains_duplicated_keys() {
    let current = GpuFeedbackFrame { ... };
    let newer = GpuFeedbackFrame { ... };
    
    let mut merge_state = GpuFeedbackMergeState::default();
    let _ = GpuFeedbackFrame::merge_mailbox(current, newer, &mut merge_state);
}
```

**Type Property Testing:**
```rust
#[test]
fn waterline_types_are_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<SubmitWaterline>();
    assert_send_sync::<ExecutedBatchWaterline>();
    assert_send_sync::<CompleteWaterline>();
}
```

## Test Coverage Analysis

**Well-Tested Areas:**
- Thread protocol: Mailbox merge logic, waterline types, input control events
- GPU commands: DrawOp, CopyOp, ClearOp construction and matching

**Untested Areas:**
- GPU runtime execution (requires GPU context)
- Stroke input processing (complex stateful logic)
- Document render tree transformations
- Brush engine runtime
- Main/engine thread coordination

**Test Gaps:**
- No integration tests for thread communication
- No tests for GPU command execution
- No tests for input processing pipeline
- No tests for render tree building

## Test Execution

**Example Test Files:**
- `crates/thread_protocol/src/lib.rs` - 7 tests, 175 lines of test code
- `crates/threads/src/lib.rs` - 1 test

**Running Tests:**
```bash
cargo test --verbose
```

**Test Output:** Standard Rust test output with `test result: ok. N passed; 0 failed`

---

*Testing analysis: 2026-03-05*