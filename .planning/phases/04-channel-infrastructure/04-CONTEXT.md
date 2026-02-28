# Phase 4.1: Channel Infrastructure - Context

**Gathered:** 2026-02-28
**Status:** Ready for planning
**Parent Phase:** Phase 4 - True Threading

<domain>
## Phase Boundary

Set up channel primitives and types for true multi-threaded execution. This phase establishes the communication infrastructure between main thread and runtime thread using the engine crate's channel primitives.

**In scope:**
- Define RuntimeReceipt and RuntimeError types in protocol crate
- Instantiate channels in GpuState::new() using engine::create_thread_channels()
- Add true_threading Cargo feature flag for compile-time gating
- Tests for channel creation and cross-thread communication

**Out of scope:**
- Runtime thread loop implementation (Phase 4.2)
- AppCore migration to channel-based commands (Phase 4.3)
- Tile lifetime safety invariants (Phase 4.4)

</domain>

<decisions>
## Implementation Decisions

### Channel Capacities
- **Capacities:** input_ring=1024 (lossy, high-frequency), input_control=256, gpu_command=1024, gpu_feedback=256
- **Configuration:** Fixed constants for now, tuned later via stress testing in Phase 4.4
- **Location:** Use engine::create_thread_channels() helper directly with hardcoded capacity arguments
- **Rationale:** Input ring needs more capacity as it's lossy and should minimize sample drops under load

### RuntimeReceipt Design
- **Coverage:** Only resource allocation commands need receipts; most commands use waterline tracking only
- **Payload:** Generic payload approach for receipts that do exist
- **Type location:** Define new types in protocol crate (not reusing render_protocol types)
- **Delivery:** Use GpuFeedbackFrame::receipts (SmallVec) - matches existing merge support pattern
- **Design principle:** Engine thread learns about work done via waterlines; receipts only for exceptional cases that need explicit acknowledgment

### RuntimeError Design
- **Coverage:** Command execution errors only (InvalidCommand, CommandFailed, ChannelClosed, Timeout)
- **Abstraction:** Abstract wgpu details - no wgpu error type leakage
- **Delivery:** GPU errors go through device.on_uncaptured_error callback first; RuntimeError is for command-level failures only
- **Severity:** All errors are fatal (fail fast) - no recovery attempts in runtime thread

### Feature Flag Strategy
- **Gating:** Cargo feature in glaphica/Cargo.toml: `true_threading = []`
- **Scope:** Single flag gates all threading features (no granular flags)
- **Code structure:** Conditional compilation in GpuState - `#[cfg(feature = "true_threading")]` gates threaded code paths
- **Flexibility:** Approach can be adjusted during Phase 4 development based on implementation learnings

### GpuState Structure
- **Type organization:** Separate structs with trait-based abstraction for common behavior
- **Relationship:** Unified refactor - GpuState redesigned with threading in mind from scratch
- **Ownership:** GpuRuntime owns threading logic; GpuState focuses on GPU resource management
- **Pattern:** Wrapper pattern - GpuRuntime wraps GpuState and manages channel/thread lifecycle

### Initialization Sequence
- **Order:** Channels → Renderer → Runtime thread
- **Error handling:** Result-based init - GpuState::new() returns Result, caller handles errors
- **Channel creation:** Use engine::create_thread_channels() helper directly (not inline or separate wrapper)

### Testing Approach
- **Coverage:** Both unit tests and integration tests with comprehensive coverage
- **GPU requirement:** Standard cargo test - no special GPU setup required for Phase 4.1 tests
- **Test location:** Unit tests in corresponding crate (renderer), integration tests in `tests/` directory
- **Focus:** Unit tests for types and channel creation; integration tests for cross-thread message passing
- **Test types:** Use real RuntimeReceipt/RuntimeError types (not test doubles)
- **Recording feature:** Hybrid approach - use input recording/replay for deterministic integration tests, manual scenarios for unit tests

### Claude's Discretion
- Exact capacity values can be tuned during planning based on code review
- Specific #[cfg] structure and module organization
- Test organization details within the standard structure

</decisions>

<code_context>
## Existing Code Insights

### Reusable Assets
- **engine::create_thread_channels()**: Already implements the channel creation logic for Command/Receipt/Error generic types - use directly
- **protocol::GpuFeedbackFrame<Receipt, Error>**: Already has merge_mailbox() support for combining feedback frames
- **protocol::merge_test_support::{TestReceipt, TestError}**: Test types available for unit testing
- **GpuState struct (crates/renderer/src/renderer_init.rs)**: Currently owns GPU resources and renderer - will need refactoring for threading
- **GpuRuntime (crates/glaphica/src/runtime/)**: Already exists as higher-level wrapper - natural place for threading ownership

### Established Patterns
- **Generic channel types**: Engine crate uses `<Command, Receipt, Error>` generics - follow this pattern
- **SmallVec for receipts/errors**: GpuFeedbackFrame uses SmallVec<[Receipt; 4]> for inline storage
- **Fail-fast invariants**: Codebase uses panic! with context for invariant violations
- **Environment-gated logs**: Existing env switches (GLAPHICA_BRUSH_TRACE, etc.) - add new ones if needed for threading debug

### Integration Points
- **GpuState::new() (renderer_init.rs:68)**: Where channels should be instantiated
- **Renderer::new()**: Called during GpuState init - ensure channel creation happens before renderer init
- **device.on_uncaptured_error()**: Already set up in renderer_init.rs:86 - GPU errors flow through here
- **crates/glaphica/src/engine_core.rs / engine_bridge.rs**: Existing engine integration - will need updates for threaded mode
- **crates/glaphica/src/app_core/**: Will consume channels in Phase 4.3

</code_context>

<specifics>
## Specific Ideas

- "Waterline-based tracking for most commands - receipts only for exceptional cases"
- "Fail fast on errors - let main thread handle recovery"
- "Hybrid testing: recording feature for deterministic regression tests"
- "Unified GpuState refactor with threading in mind from the start"

</specifics>

<deferred>
## Deferred Ideas

- Granular feature flags for debugging/stress testing - future enhancement
- Runtime-adjustable channel capacities - can add after Phase 4.4 if profiling shows need
- Detailed error severity levels - out of scope for Phase 4.1

</deferred>

---

*Phase: 04-channel-infrastructure (Phase 4.1)*
*Context gathered: 2026-02-28*
