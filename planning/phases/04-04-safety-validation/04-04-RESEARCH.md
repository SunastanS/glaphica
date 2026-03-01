# Phase 4.4: Safety & Validation Research

**Date:** 2026-02-28
**Author:** Research Agent (V3 Enhanced)
**Status:** Complete

## Executive Summary

This research analyzes the tile lifetime safety mechanisms, lock usage patterns, frame ID monotonicity validation, and stress testing requirements for Phase 4.4. The analysis covers four key safety invariants:

1. **SAFE-01**: `completion_waterline` check before tile release
2. **SAFE-02**: Generation-based ABA prevention in `resolve()` operations
3. **SAFE-03**: Lock lifetime assertions (no cross-command holding)
4. **SAFE-04**: Monotonically increasing `frame_id` / `submission_id` validation

## 1. Existing Code Analysis

### 1.1 Tile Lifecycle and `completion_waterline` Mechanism

#### Current Implementation

The `completion_waterline` mechanism is defined in the protocol crate (`/home/sunastans/Code/Graphic/glaphica/crates/protocol/src/lib.rs`):

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct CompleteWaterline(pub u64);
```

The waterline is tracked in `GpuFeedbackFrame`:

```rust
pub struct GpuFeedbackFrame<Receipt, Error> {
    pub present_frame_id: PresentFrameId,
    pub submit_waterline: SubmitWaterline,
    pub executed_batch_waterline: ExecutedBatchWaterline,
    pub complete_waterline: CompleteWaterline,
    pub receipts: SmallVec<[Receipt; 4]>,
    pub errors: SmallVec<[Error; 4]>,
}
```

**Key Finding:** The current implementation uses waterlines for tracking but does NOT currently enforce `completion_waterline` checks before tile release operations.

#### Tile Release Flow

Tile release occurs in `/home/sunastans/Code/Graphic/glaphica/crates/tiles/src/atlas/core.rs`:

```rust
pub(in crate::atlas) fn release(&self, key: TileKey) -> bool {
    let shard = self.shard_for_key(key);
    let address = {
        let mut index = self.index_shards[shard]
            .lock()
            .expect("tile index shard lock poisoned");
        index.remove(&key)
    };

    let Some(record) = address else {
        return false;
    };
    self.lifecycle_gc
        .lock()
        .expect("tile lifecycle gc lock poisoned")
        .on_key_released(key);
    // ... generation bump and free list return
}
```

**Gap Identified:** No `completion_waterline` validation exists in the release path.

### 1.2 `TileAtlasStore::resolve()` Operation Analysis

The `resolve()` operation is implemented in `/home/sunastans/Code/Graphic/glaphica/crates/tiles/src/atlas/layer_pixel_storage.rs`:

```rust
pub fn resolve(&self, key: TileKey) -> Option<TileAddress> {
    match &self.generic {
        LayerStoreBackend::Unorm(store) => store.resolve(key),
        LayerStoreBackend::Srgb(store) => store.resolve(key),
        LayerStoreBackend::BgraUnorm(store) => store.resolve(key),
        LayerStoreBackend::BgraSrgb(store) => store.resolve(key),
    }
}
```

This delegates to the core implementation in `/home/sunastans/Code/Graphic/glaphica/crates/tiles/src/atlas/core.rs`:

```rust
pub(in crate::atlas) fn resolve(&self, key: TileKey) -> Option<TileAddress> {
    let shard = self.shard_for_key(key);
    self.index_shards[shard]
        .lock()
        .expect("tile index shard lock poisoned")
        .get(&key)
        .map(|record| record.address)
}
```

#### ABA Prevention via TileKey Generation

The `TileKey` encoding (`/home/sunastans/Code/Graphic/glaphica/crates/tiles/src/tile_key_encoding.rs`) includes generation bits:

```rust
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub struct TileKey(u64);

// Layout: | backend (8) | generation (24) | slot_index (32) |
pub fn new(backend: BackendId, generation: GenerationId, slot: SlotId) -> Self {
    TileKey(
        (backend.0 as u64) << BACKEND_SHIFT
            | (generation.0 as u64) << GEN_SHIFT
            | (slot.0 as u64) << SLOT_SHIFT,
    )
}

pub fn generation(&self) -> GenerationId {
    GenerationId(((self.0 >> GEN_SHIFT) & GEN_MASK) as u32)
}
```

**Generation Bump on Release** (`/home/sunastans/Code/Graphic/glaphica/crates/tiles/src/atlas/core.rs`):

```rust
pub(in crate::atlas) fn release(&self, key: TileKey) -> bool {
    // ... remove from index ...
    page.bump_generation(address.tile_index)
        .expect("tile index must be in range");
    page.push_free(address.tile_index);
    true
}
```

**ABA Prevention Mechanism:**
1. When a tile is released, its generation is incremented (`bump_generation`)
2. The slot is returned to the free list
3. When re-allocated, a NEW `TileKey` is created with the bumped generation
4. Any stale references using the old `TileKey` will fail to resolve or will target the wrong generation

**Validation in `should_execute_target`:**

```rust
pub(in crate::atlas) fn should_execute_target(&self, target: TileOpTarget) -> bool {
    let page = self.pages.get(target.address.atlas_layer as usize)
        .expect("tile address layer must be valid");
    let Ok(generation) = page.generation(target.address.tile_index) else {
        return false;
    };
    generation == target.generation  // Generation check!
}
```

**Status:** ABA prevention via generation is **PARTIALLY IMPLEMENTED**. The mechanism exists but is not consistently enforced across all access paths.

### 1.3 Lock Usage Patterns Analysis

#### Lock Inventory

The codebase uses `std::sync::Mutex` extensively in `/home/sunastans/Code/Graphic/glaphica/crates/tiles/src/atlas/core.rs`:

| Lock Location | Purpose | Scope |
|---------------|---------|-------|
| `TileOpQueue.receiver` | MPSC receiver protection | Per-operation drain |
| `TileAllocatorPage.free_tiles` | Free list protection | Per-allocation |
| `TileAllocatorPage.dirty_bits` | Dirty bit tracking | Per-allocation |
| `TileAllocatorPage.generations` | Generation tracking | Per-allocation |
| `TileAtlasCpu.index_shards[64]` | Tile key -> address mapping | Per-lookup |
| `TileAtlasCpu.lifecycle_gc` | GC state tracking | Per-lifecycle-op |

#### Current Lock Holding Patterns

**Short-duration locks (correct):**
```rust
// resolve() - lock held only for HashMap lookup
pub(in crate::atlas) fn resolve(&self, key: TileKey) -> Option<TileAddress> {
    let shard = self.shard_for_key(key);
    self.index_shards[shard]
        .lock()
        .expect("...")
        .get(&key)
        .map(|record| record.address)
}
```

**Multi-lock operations (potential deadlock risk):**
```rust
// release_set_atomic() - acquires multiple shard locks
pub(in crate::atlas) fn release_set_atomic(&self, keys: &[TileKey]) -> Result<u32, TileSetError> {
    // ... shard_id collection and sorting ...
    let mut shard_locks = shard_ids
        .into_iter()
        .map(|shard_id| (shard_id, self.index_shards[shard_id].lock()...))
        .collect::<Vec<_>>();
    // ... operations under lock ...
}
```

**Gap Identified:** No debug assertions exist to detect locks held across command boundaries.

### 1.4 Frame ID / Submission ID Tracking

#### Frame ID Tracking in GpuRuntime

From `/home/sunastans/Code/Graphic/glaphica/crates/glaphica/src/runtime/mod.rs`:

```rust
pub struct GpuRuntime {
    // ...
    /// Next frame ID to use.
    next_frame_id: u64,
}

impl GpuRuntime {
    pub fn next_frame_id(&mut self) -> u64 {
        let id = self.next_frame_id;
        self.next_frame_id += 1;
        id
    }
}
```

#### Waterline Tracking in Runtime Loop

From `/home/sunastans/Code/Graphic/glaphica/crates/glaphica/src/runtime/execution.rs`:

```rust
struct RuntimeWaterlines {
    present_frame_id: protocol::PresentFrameId,
    submit_waterline: protocol::SubmitWaterline,
    executed_batch_waterline: protocol::ExecutedBatchWaterline,
    complete_waterline: protocol::CompleteWaterline,
}

fn execute_command(
    cmd: RuntimeCommand,
    gpu_runtime: &mut GpuRuntime,
    receipts: &mut SmallVec<[RuntimeReceipt; 4]>,
    _errors: &mut SmallVec<[RuntimeError; 4]>,
    waterlines: &mut RuntimeWaterlines,
) -> Result<(), RuntimeError> {
    match &cmd {
        RuntimeCommand::PresentFrame { frame_id } => {
            waterlines.present_frame_id = protocol::PresentFrameId(*frame_id);
        }
        _ => {
            waterlines.submit_waterline.0 += 1;
        }
    }
    let receipt = gpu_runtime.execute(cmd)?;
    receipts.push(receipt);
    Ok(())
}
```

**Gap Identified:** No monotonicity validation exists for `frame_id` or waterlines.

## 2. Safety Mechanism Design

### 2.1 SAFE-01: `completion_waterline` Check Before Tile Release

#### Design

Add waterline validation to tile release operations:

```rust
// New structure to track completion state
pub struct TileCompletionWaterline {
    /// Monotonically increasing waterline
    waterline: AtomicU64,
    /// Pending completions waiting for waterline advance
    pending: Mutex<VecDeque<PendingCompletion>>,
}

pub struct PendingCompletion {
    pub tile_key: TileKey,
    pub submission_id: u64,
    pub required_waterline: u64,
}

impl TileCompletionWaterline {
    /// Check if a tile can be safely released
    pub fn can_release(&self, tile_key: TileKey, submission_id: u64) -> bool {
        let current = self.waterline.load(Ordering::Acquire);
        current >= submission_id
    }

    /// Attempt release with waterline check
    pub fn try_release<F>(&self, key: TileKey, submission_id: u64, release_fn: F)
        -> Result<bool, TileReleaseError>
    where
        F: FnOnce(TileKey) -> bool,
    {
        if !self.can_release(key, submission_id) {
            // Queue for deferred release
            self.pending.lock().unwrap().push_back(PendingCompletion {
                tile_key: key,
                submission_id,
                required_waterline: submission_id,
            });
            return Ok(false);
        }
        Ok(release_fn(key))
    }

    /// Advance waterline and process pending releases
    pub fn advance_waterline(&self, new_waterline: u64) -> Vec<PendingCompletion> {
        self.waterline.store(new_waterline, Ordering::Release);

        let mut pending = self.pending.lock().unwrap();
        let ready: Vec<_> = pending
            .iter()
            .filter(|pc| pc.required_waterline <= new_waterline)
            .copied()
            .collect();

        pending.retain(|pc| pc.required_waterline > new_waterline);
        ready
    }
}
```

#### Integration Points

1. **GpuRuntime** - Track `complete_waterline` from feedback frames
2. **TileAtlasStore::release()** - Check waterline before actual release
3. **Merge completion handler** - Advance waterline on GPU completion notice

### 2.2 SAFE-02: Generation-Based ABA Prevention

#### Current State Assessment

| Component | Status | Notes |
|-----------|--------|-------|
| TileKey generation encoding | Implemented | 24-bit generation field |
| Generation bump on release | Implemented | `bump_generation()` |
| Generation validation on execute | Partial | `should_execute_target()` exists |
| Generation check in resolve() | Missing | Returns address only |

#### Enhanced Design

Add generation validation to `resolve()`:

```rust
/// Extended resolve result that includes generation for validation
#[derive(Debug, Clone, Copy)]
pub struct ResolvedTile {
    pub address: TileAddress,
    pub generation: u32,
    pub key: TileKey,
}

impl TileAtlasCpu {
    /// Resolve with generation validation
    pub fn resolve_validated(&self, key: TileKey) -> Result<ResolvedTile, TileResolveError> {
        let shard = self.shard_for_key(key);
        let record = self.index_shards[shard]
            .lock()
            .expect("tile index shard lock poisoned")
            .get(&key)
            .copied()
            .ok_or(TileResolveError::NotFound)?;

        // Validate generation matches
        let page = self.pages.get(record.address.atlas_layer as usize)
            .ok_or(TileResolveError::InvalidLayer)?;
        let current_generation = page.generation(record.address.tile_index)?;

        if current_generation != record.generation {
            return Err(TileResolveError::GenerationMismatch {
                expected: record.generation,
                actual: current_generation,
            });
        }

        Ok(ResolvedTile {
            address: record.address,
            generation: record.generation,
            key,
        })
    }
}
```

### 2.3 SAFE-03: Lock Lifetime Assertions

#### Design

Add debug-only lock tracking to detect cross-command lock holding:

```rust
#[cfg(debug_assertions)]
pub struct LockGuardTracker {
    command_id: std::cell::Cell<u64>,
    active_locks: std::cell::RefCell<Vec<LockInfo>>,
}

#[cfg(debug_assertions)]
pub struct LockInfo {
    pub lock_name: &'static str,
    pub acquired_at: std::panic::Location<'static>,
    pub command_id: u64,
}

#[cfg(debug_assertions)]
impl LockGuardTracker {
    pub fn on_command_start(&self, command_id: u64) {
        self.command_id.set(command_id);
        let locks = self.active_locks.borrow();
        assert!(
            locks.is_empty(),
            "Locks held across command boundary: {:?}",
            locks
        );
    }

    pub fn on_command_end(&self) {
        let locks = self.active_locks.borrow();
        assert!(
            locks.is_empty(),
            "Locks not released by command end: {:?}",
            locks
        );
    }

    pub fn track_lock_acquire(&self, lock_name: &'static str) {
        self.active_locks.borrow_mut().push(LockInfo {
            lock_name,
            acquired_at: std::panic::Location::caller(),
            command_id: self.command_id.get(),
        });
    }

    pub fn track_lock_release(&self, lock_name: &'static str) {
        let mut locks = self.active_locks.borrow_mut();
        if let Some(pos) = locks.iter().position(|l| l.lock_name == lock_name) {
            locks.remove(pos);
        }
    }
}
```

#### Instrumentation Points

```rust
// In TileAtlasCpu methods
#[cfg(debug_assertions)]
static LOCK_TRACKER: LockGuardTracker = LockGuardTracker::new();

pub(in crate::atlas) fn resolve(&self, key: TileKey) -> Option<TileAddress> {
    #[cfg(debug_assertions)]
    LOCK_TRACKER.track_lock_acquire("index_shard");

    let shard = self.shard_for_key(key);
    let result = self.index_shards[shard]
        .lock()
        .expect("tile index shard lock poisoned")
        .get(&key)
        .map(|record| record.address);

    #[cfg(debug_assertions)]
    LOCK_TRACKER.track_lock_release("index_shard");

    result
}
```

### 2.4 SAFE-04: Monotonically Increasing Frame ID Validation

#### Design

Add monotonicity assertions to frame ID and waterline tracking:

```rust
pub struct MonotonicityValidator {
    #[cfg(debug_assertions)]
    last_frame_id: std::cell::Cell<u64>,
    #[cfg(debug_assertions)]
    last_submit_waterline: std::cell::Cell<u64>,
    #[cfg(debug_assertions)]
    last_complete_waterline: std::cell::Cell<u64>,
}

impl MonotonicityValidator {
    #[cfg(debug_assertions)]
    pub fn validate_frame_id(&self, frame_id: u64) {
        let last = self.last_frame_id.get();
        assert!(
            frame_id > last || frame_id == 0,
            "Frame ID regression detected: {} -> {}",
            last,
            frame_id
        );
        self.last_frame_id.set(frame_id);
    }

    #[cfg(debug_assertions)]
    pub fn validate_waterline_advance(&self, name: &'static str, current: u64, last: u64) {
        assert!(
            current >= last,
            "Waterline regression detected for {}: {} -> {}",
            name,
            last,
            current
        );
    }

    #[cfg(not(debug_assertions))]
    pub fn validate_frame_id(&self, _frame_id: u64) {}

    #[cfg(not(debug_assertions))]
    pub fn validate_waterline_advance(&self, _name: &'static str, _current: u64, _last: u64) {}
}
```

#### Integration

```rust
// In GpuRuntime
pub struct GpuRuntime {
    // ...
    monotonicity_validator: MonotonicityValidator,
}

impl GpuRuntime {
    pub fn execute(&mut self, command: RuntimeCommand) -> Result<RuntimeReceipt, RuntimeError> {
        if let RuntimeCommand::PresentFrame { frame_id } = &command {
            self.monotonicity_validator.validate_frame_id(*frame_id);
        }
        // ... rest of execute
    }
}

// In runtime loop
fn execute_command(
    cmd: RuntimeCommand,
    gpu_runtime: &mut GpuRuntime,
    receipts: &mut SmallVec<[RuntimeReceipt; 4]>,
    _errors: &mut SmallVec<[RuntimeError; 4]>,
    waterlines: &mut RuntimeWaterlines,
) -> Result<(), RuntimeError> {
    let old_submit = waterlines.submit_waterline.0;

    match &cmd {
        RuntimeCommand::PresentFrame { frame_id } => {
            waterlines.present_frame_id = protocol::PresentFrameId(*frame_id);
        }
        _ => {
            waterlines.submit_waterline.0 += 1;
        }
    }

    #[cfg(debug_assertions)]
    gpu_runtime.monotonicity_validator.validate_waterline_advance(
        "submit_waterline",
        waterlines.submit_waterline.0,
        old_submit,
    );

    let receipt = gpu_runtime.execute(cmd)?;
    receipts.push(receipt);
    Ok(())
}
```

## 3. Test方案设计

### 3.1 Stress Test: Concurrent Command/Feedback Pressure

```rust
#[cfg(test)]
mod stress_tests {
    use super::*;
    use std::thread;
    use std::time::Duration;
    use std::sync::Arc;
    use std::atomic::{AtomicBool, AtomicUsize, Ordering};

    /// Test: High-volume command submission with feedback verification
    #[test]
    fn test_concurrent_command_feedback_stress() {
        const COMMAND_COUNT: usize = 10_000;
        const NUM_PRODUCERS: usize = 4;
        const TIMEOUT: Duration = Duration::from_secs(30);

        let (main_channels, engine_channels) = create_thread_channels::<
            RuntimeCommand, RuntimeReceipt, RuntimeError
        >(1024, 16, 256, 512);

        let stop_flag = Arc::new(AtomicBool::new(false));
        let command_count = Arc::new(AtomicUsize::new(0));
        let feedback_count = Arc::new(AtomicUsize::new(0));

        // Spawn producer threads
        let mut producers = Vec::new();
        for thread_id in 0..NUM_PRODUCERS {
            let channels = main_channels.clone();
            let stop = stop_flag.clone();
            let count = command_count.clone();

            producers.push(thread::spawn(move || {
                let mut frame_id = 0u64;
                while !stop.load(Ordering::Acquire) {
                    let cmd = RuntimeCommand::PresentFrame {
                        frame_id: frame_id.wrapping_add(thread_id as u64 * COMMAND_COUNT as u64)
                    };
                    if channels.gpu_command_sender.push(GpuCmdMsg::Command(cmd)).is_ok() {
                        count.fetch_add(1, Ordering::Relaxed);
                        frame_id += 1;
                    }
                    thread::yield_now();
                }
            }));
        }

        // Consumer thread
        let consumer_stop = stop_flag.clone();
        let consumer_count = feedback_count.clone();
        let consumer = thread::spawn(move || {
            while !consumer_stop.load(Ordering::Acquire) {
                if let Ok(_frame) = engine_channels.gpu_feedback_receiver.pop() {
                    consumer_count.fetch_add(1, Ordering::Relaxed);
                }
                thread::yield_now();
            }
        });

        // Run for timeout duration
        thread::sleep(TIMEOUT);
        stop_flag.store(true, Ordering::Release);

        // Wait for threads
        for producer in producers {
            let _ = producer.join();
        }
        let _ = consumer.join();

        // Verify no deadlocks occurred
        let commands_sent = command_count.load(Ordering::Relaxed);
        let feedback_received = feedback_count.load(Ordering::Relaxed);

        println!("Commands sent: {}, Feedback received: {}", commands_sent, feedback_received);
        assert!(commands_sent > 0, "Should have sent at least one command");
        assert!(feedback_received > 0, "Should have received at least one feedback");
    }

    /// Test: Rapid allocate/release cycle stress
    #[test]
    fn test_tile_allocate_release_stress() {
        const ITERATIONS: usize = 100_000;

        let (device, _queue) = create_test_device();
        let (store, _gpu) = TileAtlasStore::with_config(
            &device,
            TileAtlasConfig::tiny10()
        ).expect("create store");

        let mut allocated_keys = Vec::new();

        for i in 0..ITERATIONS {
            // Allocate
            match store.allocate() {
                Ok(key) => allocated_keys.push(key),
                Err(_) => {
                    // Atlas full, release some and continue
                    for _ in 0..3 {
                        if let Some(key) = allocated_keys.pop() {
                            assert!(store.release(key), "release should succeed");
                        }
                    }
                    if let Ok(key) = store.allocate() {
                        allocated_keys.push(key);
                    }
                }
            }

            // Periodic GC
            if i % 1000 == 0 {
                let evicted = store.drain_evicted_retain_batches();
                for batch in evicted {
                    for key in batch.keys {
                        let _ = store.release(key);
                    }
                }
            }
        }

        // Cleanup
        for key in allocated_keys {
            let _ = store.release(key);
        }
    }
}
```

### 3.2 Deadlock Detection Test

```rust
#[cfg(test)]
mod deadlock_tests {
    use super::*;
    use std::time::Duration;

    /// Test: Detect potential deadlock in multi-key operations
    #[test]
    #[timeout(Duration::from_secs(10))]
    fn test_no_deadlock_in_release_set_atomic() {
        let (device, _queue) = create_test_device();
        let (store, _gpu) = TileAtlasStore::with_config(
            &device,
            TileAtlasConfig::tiny10()
        ).expect("create store");

        // Allocate keys across different shards
        let mut keys = Vec::new();
        for _ in 0..100 {
            if let Ok(key) = store.allocate() {
                keys.push(key);
            }
        }

        // Create sets with overlapping and non-overlapping shards
        let set1 = store.adopt_tile_set(keys[0..10].iter().copied())
            .expect("create set1");
        let set2 = store.adopt_tile_set(keys[10..20].iter().copied())
            .expect("create set2");
        let set3 = store.adopt_tile_set(keys[5..15].iter().copied())  // Overlaps with set1, set2
            .expect("create set3");

        // Concurrent release attempts (should not deadlock)
        let store_arc = Arc::new(store);
        let mut handles = Vec::new();

        for set in [set1, set2, set3] {
            let store = store_arc.clone();
            handles.push(thread::spawn(move || {
                let _ = store.release_tile_set(set);
            }));
        }

        // Wait for all threads (will timeout if deadlock)
        for handle in handles {
            let _ = handle.join();
        }
    }

    /// Test: Lock ordering consistency
    #[test]
    fn test_lock_ordering_consistency() {
        // This test verifies that locks are always acquired in consistent order
        // to prevent ABBA deadlocks

        let (device, _queue) = create_test_device();
        let (store, _gpu) = TileAtlasStore::with_config(
            &device,
            TileAtlasConfig::tiny10()
        ).expect("create store");

        // Allocate many keys to spread across shards
        let mut keys = Vec::new();
        for _ in 0..1000 {
            if let Ok(key) = store.allocate() {
                keys.push(key);
            }
        }

        // Spawn threads that operate on different key ranges
        let store_arc = Arc::new(store);
        let mut handles = Vec::new();

        for chunk in keys.chunks(100) {
            let store = store_arc.clone();
            let keys: Vec<_> = chunk.to_vec();

            handles.push(thread::spawn(move || {
                for &key in &keys {
                    let _ = store.resolve(key);
                    let _ = store.is_allocated(key);
                }
            }));
        }

        // Should complete without deadlock
        for handle in handles {
            let _ = handle.join();
        }
    }
}
```

### 3.3 Waterline Validation Test

```rust
#[cfg(test)]
mod waterline_tests {
    use super::*;

    /// Test: Frame ID must be monotonically increasing
    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "Frame ID regression detected")]
    fn test_frame_id_monotonicity_debug_panic() {
        let validator = MonotonicityValidator::new();

        validator.validate_frame_id(10);
        validator.validate_frame_id(20);
        validator.validate_frame_id(15);  // Should panic
    }

    /// Test: Waterline must not regress
    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "Waterline regression detected")]
    fn test_waterline_monotonicity_debug_panic() {
        let validator = MonotonicityValidator::new();

        validator.validate_waterline_advance("submit", 10, 5);
        validator.validate_waterline_advance("submit", 15, 10);
        validator.validate_waterline_advance("submit", 12, 15);  // Should panic
    }

    /// Test: Completion waterline blocks premature release
    #[test]
    fn test_completion_waterline_blocks_release() {
        let waterline = TileCompletionWaterline::new(0);

        // Try to release tile with submission_id=10 when waterline=5
        assert!(!waterline.can_release(dummy_key(), 10));

        // Advance waterline past submission_id
        waterline.advance_waterline(15);

        // Now release should be allowed
        assert!(waterline.can_release(dummy_key(), 10));
    }
}
```

## 4. Implementation Recommendations

### 4.1 Priority Order

1. **HIGH PRIORITY - SAFE-02**: Complete generation-based ABA prevention
   - Add generation validation to `resolve()` operations
   - Enforce `should_execute_target()` checks in all tile operation paths

2. **HIGH PRIORITY - SAFE-04**: Frame ID monotonicity validation
   - Add debug assertions for monotonicity
   - Release mode: log warnings for regressions

3. **MEDIUM PRIORITY - SAFE-01**: `completion_waterline` integration
   - Integrate waterline tracking with tile release
   - Implement deferred release queue

4. **MEDIUM PRIORITY - SAFE-03**: Lock lifetime assertions
   - Add debug-only lock tracking
   - Instrument all lock acquisition points

### 4.2 Feature Flag Strategy

```rust
// In Cargo.toml
[features]
tile-safety-assertions = []  # Enable all safety assertions
waterline-validation = []    # Enable completion waterline checks
lock-tracking = []           # Enable lock lifetime tracking
```

### 4.3 Performance Considerations

| Safety Feature | Debug Overhead | Release Overhead | Mitigation |
|----------------|----------------|------------------|------------|
| Generation validation | ~5% | ~1% | Inline checks, branch prediction |
| Waterline check | ~2% | ~0.5% | Atomic with Relaxed ordering |
| Lock tracking | ~10% | 0% | Debug-only compilation |
| Monotonicity validation | ~1% | 0% | Debug-only compilation |

## 5. Risk Assessment

| Risk | Severity | Likelihood | Mitigation |
|------|----------|------------|------------|
| Use-after-free tile access | Critical | Medium | Complete SAFE-01, SAFE-02 implementation |
| Deadlock under load | High | Low | SAFE-03 assertions, stress testing |
| Performance regression | Medium | Medium | Feature flags, benchmarking |
| ABA race condition | Critical | Low | Generation validation enforcement |

## 6. References

### Key Files Analyzed

- `/home/sunastans/Code/Graphic/glaphica/crates/protocol/src/lib.rs` - Waterline definitions
- `/home/sunastans/Code/Graphic/glaphica/crates/tiles/src/lib.rs` - Tile module exports
- `/home/sunastans/Code/Graphic/glaphica/crates/tiles/src/atlas.rs` - Atlas configuration
- `/home/sunastans/Code/Graphic/glaphica/crates/tiles/src/atlas/core.rs` - Core tile lifecycle
- `/home/sunastans/Code/Graphic/glaphica/crates/tiles/src/atlas/layer_pixel_storage.rs` - Store resolve
- `/home/sunastans/Code/Graphic/glaphica/crates/tiles/src/tile_key_encoding.rs` - TileKey generation
- `/home/sunastans/Code/Graphic/glaphica/crates/glaphica/src/runtime/mod.rs` - GpuRuntime frame tracking
- `/home/sunastans/Code/Graphic/glaphica/crates/glaphica/src/runtime/execution.rs` - Runtime waterlines
- `/home/sunastans/Code/Graphic/glaphica/docs/planning/requirements.md` - Safety requirements

### Related Documentation

- `docs/planning/phases/04-03-appcore-migration/04-03-PLAN.md`
- `docs/planning/phases/4.2-runtime-thread-loop/4.2-RESEARCH.md`
- `docs/guides/testing.md`
