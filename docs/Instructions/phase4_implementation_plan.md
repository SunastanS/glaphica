# Phase 4 Implementation Plan: True Thread Channels

**Version**: 1.0  
**Date**: 2026-02-27  
**Status**: Ready for Implementation  
**Review Status**: ✅ Q1-Q4 decisions confirmed

---

## 0. Executive Summary

**Objective**: Migrate from single-threaded command dispatcher to true cross-thread channels using `engine`/`protocol` crate primitives.

**Architecture**:
- **Engine Thread**: Business logic (`EngineCore`), produces GPU commands
- **Main Thread**: GPU execution (`GpuRuntime`), consumes commands, produces feedback
- **Communication**: `create_thread_channels<RuntimeCommand, RuntimeReceipt, RuntimeError>`

**Key Decisions (Q1-Q4)**:
| Decision | Choice | Rationale |
|----------|--------|-----------|
| Q1: Thread management | **B: Dedicated `EngineBridge`** | Clean lifecycle, avoids God object |
| Q2: Sync/Async | **B: Async default + bounded sync** | UI responsiveness, leverages mailbox merge |
| Q3: Waterline granularity | **B: Per-batch** | Matches frame-driven rendering |
| Q4: Feedback timing | **A: Once per frame** | Deterministic, aligns with winit loop |

**Additional Decisions**:
- Document ownership: **EngineCore exclusive** (no UI reads)
- brush_buffer_tile_keys ownership: **EngineCore exclusive**
- Handshake timeout: Init 5s, Resize 1s
- Feedback queue capacity: 64 frames

---

## 1. Current State Analysis

### 1.1 Existing Infrastructure ✅

**Protocol Crate** (`crates/protocol/src/lib.rs`):
- `GpuCmdMsg<Command>` - Command wrapper
- `GpuFeedbackFrame<Receipt, Error>` - Feedback with waterlines + mailbox merge
- `InputRingSample`, `InputControlEvent` - Input transport
- Waterline types: `PresentFrameId`, `SubmitWaterline`, `ExecutedBatchWaterline`, `CompleteWaterline`

**Engine Crate** (`crates/engine/src/lib.rs`):
- `create_thread_channels<Command, Receipt, Error>()` - Channel factory
- `MainThreadChannels<>`, `EngineThreadChannels<>` - Typed endpoints
- Input ring with lossy semantics (evict oldest)
- Input control queue with bounded blocking + timeout

**Current Architecture** (Phase 2.5-B):
- `GpuState` (facade) → `AppCore` (business) → `GpuRuntime` (GPU executor)
- Single-threaded: `AppCore::execute_runtime()` calls `GpuRuntime::execute()` directly
- `RuntimeCommand` / `RuntimeReceipt` / `RuntimeError` protocol defined

### 1.2 Required Changes

**Structural**:
- Split `AppCore` → `EngineCore` (Engine Thread) + retain `GpuRuntime` (Main Thread)
- Create `EngineBridge` to manage channels + thread lifecycle
- Replace direct `execute()` call with channel send/receive

**Behavioral**:
- Commands sent asynchronously via channel
- Feedback merged via `GpuFeedbackFrame::merge_mailbox()`
- Waterlines tracked monotonically
- Handshake commands (Init/Resize) use bounded sync with timeout

---

## 2. Target Architecture

### 2.1 Thread Topology

```
┌─────────────────────────────────────────────────────────────┐
│                  Engine Thread (Business)                   │
│  ┌─────────────┐      ┌──────────┐      ┌──────────────┐  │
│  │  EngineCore │ ──→  │  Cmd     │      │  Feedback    │  │
│  │  (state)    │      │  Sender  │      │  Receiver    │  │
│  └─────────────┘      └────┬─────┘      └──────┬───────┘  │
│                            │                   │           │
│                     (crossbeam)          (crossbeam)       │
│                            │                   │           │
└────────────────────────────┼───────────────────┼───────────┘
                             │                   │
┌────────────────────────────┼───────────────────┼───────────┐
│                  Main Thread (GPU/UI)           │           │
│                            ▼                   │           │
│  ┌─────────────────────────────────┐           │           │
│  │  EngineBridge                   │           │           │
│  │  - main_channels                │           │           │
│  │  - gpu_runtime                  │           │           │
│  │  - waterlines                   │           │           │
│  │                                 │           │           │
│  │  Dispatcher Loop:               │           │           │
│  │  1. Drain commands              │           │           │
│  │  2. Execute (GpuRuntime)        │           │           │
│  │  3. Push feedback frame         │───────────┘           │
│  └─────────────────────────────────┘                       │
│          │                                                 │
│          ▼                                                 │
│  ┌─────────────────┐                                       │
│  │ wgpu::Surface   │ ←── Main-thread only (macOS/Metal)   │
│  └─────────────────┘                                       │
└─────────────────────────────────────────────────────────────┘
```

### 2.2 Component Responsibilities

| Component | Thread | Responsibilities |
|-----------|--------|------------------|
| **EngineCore** | Engine | Business logic, document state, merge orchestration, command generation |
| **EngineBridge** | Main | Channel management, GPU dispatch, feedback production, lifecycle |
| **GpuRuntime** | Main | GPU resource management, renderer execution, surface/present |

### 2.3 Data Flow

**Command Path** (Engine → Main):
```
EngineCore.process_input_sample()
    ↓
EngineCore.pending_commands (Vec<RuntimeCommand>)
    ↓
gpu_command_sender.push(GpuCmdMsg::Command(cmd))
    ↓
gpu_command_receiver.pop() (Main Thread)
    ↓
GpuRuntime.execute(cmd)
```

**Feedback Path** (Main → Engine):
```
GpuRuntime.execute(cmd) → RuntimeReceipt
    ↓
GpuFeedbackFrame { receipts, errors, waterlines }
    ↓
gpu_feedback_sender.push(frame)
    ↓
gpu_feedback_receiver.pop() (Engine Thread)
    ↓
GpuFeedbackFrame::merge_mailbox() (absorptive merge)
    ↓
EngineCore.process_feedback(frame)
```

---

## 3. Implementation Steps

### Step A: Bridge Type Definitions (2-3 hours)

**A.1**: Define `EngineCore` (from `AppCore` split)

```rust
// crates/glaphica/src/engine_core.rs
pub struct EngineCore {
    // Document owned exclusively by engine thread
    document: Document,
    
    // Merge engine
    tile_merge_engine: TileMergeEngine<MergeStores>,
    
    // Brush state
    brush_buffer_tile_keys: BrushBufferTileRegistry,
    
    // View state
    view_transform: ViewTransform,
    
    // GC state
    gc_evicted_batches_total: u64,
    gc_evicted_keys_total: u64,
    
    // Waterlines (received from main thread via feedback)
    waterlines: EngineWaterlines,
    
    // Channels
    engine_channels: EngineThreadChannels<RuntimeCommand, RuntimeReceipt, RuntimeError>,
    
    // Shutdown flag
    shutdown_requested: bool,
    
    // Pending commands (generated from business logic)
    pending_commands: Vec<RuntimeCommand>,
}

struct EngineWaterlines {
    submit: SubmitWaterline,
    executed: ExecutedBatchWaterline,
    complete: CompleteWaterline,
}
```

**Key Invariant**: `EngineCore` fields are **NOT `Arc`/`RwLock`** - exclusive to Engine Thread.

---

**A.2**: Define `EngineBridge`

```rust
// crates/glaphica/src/engine_bridge.rs
pub struct EngineBridge {
    // Main thread channels
    main_channels: MainThreadChannels<RuntimeCommand, RuntimeReceipt, RuntimeError>,
    
    // GPU runtime (main thread only)
    gpu_runtime: GpuRuntime,
    
    // Main thread waterlines
    waterlines: MainThreadWaterlines,
    
    // Engine thread handle
    engine_thread: Option<std::thread::JoinHandle<()>>,
}

struct MainThreadWaterlines {
    present_frame_id: PresentFrameId,
    executed_batch_waterline: ExecutedBatchWaterline,
    complete_waterline: CompleteWaterline,
}

impl Drop for EngineBridge {
    fn drop(&mut self) {
        // 1. Send shutdown command
        let _ = self.main_channels.gpu_command_sender.push(
            GpuCmdMsg::Command(RuntimeCommand::Shutdown { 
                reason: "EngineBridge dropped".to_string() 
            })
        );
        
        // 2. Join engine thread
        if let Some(handle) = self.engine_thread.take() {
            handle.join()
                .unwrap_or_else(|err| eprintln!("[error] engine thread panic: {:?}", err));
        }
    }
}
```

---

**A.3**: Define `SampleSource` trait (Phase 4.5 extension point)

```rust
// crates/glaphica/src/sample_source.rs
use protocol::InputRingSample;

pub trait SampleSource {
    fn drain_batch(&mut self, out: &mut Vec<InputRingSample>, budget: usize);
}

// Phase 4: Simple channel implementation
pub struct ChannelSampleSource {
    receiver: crossbeam_channel::Receiver<InputRingSample>,
}

impl SampleSource for ChannelSampleSource {
    fn drain_batch(&mut self, out: &mut Vec<InputRingSample>, budget: usize) {
        out.clear();
        for _ in 0..budget {
            match self.receiver.try_recv() {
                Ok(sample) => out.push(sample),
                Err(crossbeam_channel::TryRecvError::Empty) => break,
                Err(crossbeam_channel::TryRecvError::Disconnected) => return,
            }
        }
    }
}
```

---

**A.4**: Extend `RuntimeCommand` and `RuntimeReceipt`

```rust
// crates/glaphica/src/runtime/protocol.rs
pub enum RuntimeCommand {
    // ... existing commands (PresentFrame, Resize, EnqueueBrushCommands, etc.)
    
    /// Shutdown engine thread (explicit handshake)
    Shutdown { reason: String },
    
    /// Initialize handshake (sync with ack)
    Init { ack_sender: Sender<Result<(), RuntimeError>> },
    
    /// Resize handshake (sync with ack)
    Resize { 
        width: u32, 
        height: u32, 
        ack_sender: Sender<Result<(), RuntimeError>> 
    },
}

pub enum RuntimeReceipt {
    // ... existing receipts
    
    /// Shutdown acknowledged
    ShutdownAck { reason: String },
    
    /// Init completed
    InitComplete,
    
    /// Resize completed
    ResizeComplete,
}
```

---

**A.5**: Extend `RuntimeError`

```rust
// crates/glaphica/src/runtime/protocol.rs
pub enum RuntimeError {
    // ... existing errors
    
    /// Shutdown requested
    ShutdownRequested { reason: String },
    
    /// Engine thread disconnected
    EngineThreadDisconnected,
    
    /// Feedback queue timeout (release mode graceful degradation)
    FeedbackQueueTimeout,
    
    /// Handshake timeout
    HandshakeTimeout { operation: &'static str },
}
```

---

### Step B: Main Thread Dispatcher (3-4 hours)

**B.1**: Implement command dispatch loop

```rust
// crates/glaphica/src/engine_bridge.rs
impl EngineBridge {
    pub fn dispatch_frame(&mut self) -> Result<(), RuntimeError> {
        let mut receipts = Vec::new();
        let mut errors = Vec::new();
        
        // 1. Drain commands (with budget)
        const COMMAND_BUDGET: usize = 256;
        for _ in 0..COMMAND_BUDGET {
            match self.main_channels.gpu_command_receiver.pop() {
                Ok(GpuCmdMsg::Command(cmd)) => {
                    self.execute_command(cmd, &mut receipts, &mut errors)?;
                }
                Err(crossbeam_queue::PopError::Empty) => break,
                Err(crossbeam_queue::PopError::Disconnected) => {
                    return Err(RuntimeError::EngineThreadDisconnected);
                }
            }
        }
        
        // 2. Update waterlines
        self.waterlines.executed_batch_waterline.0 += 1;
        
        // 3. Push feedback frame (MUST SUCCEED - protocol invariant)
        self.push_feedback_frame_all(receipts, errors)?;
        
        Ok(())
    }
    
    fn execute_command(
        &mut self, 
        cmd: RuntimeCommand, 
        receipts: &mut Vec<RuntimeReceipt>,
        errors: &mut Vec<RuntimeError>,
    ) -> Result<(), RuntimeError> {
        match cmd {
            RuntimeCommand::Shutdown { reason } => {
                // Send final feedback before shutdown
                receipts.push(RuntimeReceipt::ShutdownAck { reason: reason.clone() });
                self.push_feedback_frame_all(receipts, errors)?;
                eprintln!("[shutdown] {}", reason);
                Err(RuntimeError::ShutdownRequested { reason })
            }
            
            RuntimeCommand::Init { ack_sender } => {
                let result = self.gpu_runtime.initialize();
                let _ = ack_sender.send(result.clone());
                if result.is_ok() {
                    receipts.push(RuntimeReceipt::InitComplete);
                }
                Ok(())
            }
            
            RuntimeCommand::Resize { width, height, ack_sender } => {
                let result = self.gpu_runtime.resize(width, height);
                let _ = ack_sender.send(result.clone());
                if result.is_ok() {
                    receipts.push(RuntimeReceipt::ResizeComplete);
                }
                result.map_err(RuntimeError::from)
            }
            
            // ... other commands
            _ => {
                let receipt = self.gpu_runtime.execute(cmd)?;
                receipts.push(receipt);
                Ok(())
            }
        }
    }
    
    fn push_feedback_frame_all(
        &mut self,
        receipts: Vec<RuntimeReceipt>,
        errors: Vec<RuntimeError>,
    ) -> Result<(), RuntimeError> {
        let frame = GpuFeedbackFrame {
            present_frame_id: self.waterlines.present_frame_id,
            submit_waterline: self.waterlines.submit_waterline,
            executed_batch_waterline: self.waterlines.executed_batch_waterline,
            complete_waterline: self.waterlines.complete_waterline,
            receipts: receipts.into(),
            errors: errors.into(),
        };
        
        // Q2 strategy: Debug panic + Release timeout
        #[cfg(debug_assertions)]
        {
            self.main_channels.gpu_feedback_sender.push(frame)
                .expect("feedback queue full: protocol violation");
        }
        
        #[cfg(not(debug_assertions))]
        {
            // Release: timeout blocking (5ms)
            let timeout = std::time::Duration::from_millis(5);
            loop {
                match self.main_channels.gpu_feedback_sender.push(frame.clone()) {
                    Ok(()) => break,
                    Err(rtrb::PushError::Full(_)) => {
                        std::thread::sleep(timeout);
                        // After timeout, trigger shutdown
                        return Err(RuntimeError::FeedbackQueueTimeout);
                    }
                }
            }
        }
        
        Ok(())
    }
}
```

---

**B.2**: Integrate with winit event loop

```rust
// crates/glaphica/src/main.rs
impl App {
    fn render_frame(&mut self, event_loop: &ActiveEventLoop) {
        // Check for fatal error from previous frame
        if self.fatal_error_seen {
            eprintln!("[fatal] exiting due to previous render fatal error");
            event_loop.exit();
            return;
        }
        
        let Some(gpu) = self.gpu.as_mut() else {
            return;
        };
        
        // NEW: Dispatch GPU commands via EngineBridge
        if let Err(err) = gpu.bridge.dispatch_frame() {
            match err {
                RuntimeError::ShutdownRequested { .. } => {
                    event_loop.exit();
                }
                RuntimeError::FeedbackQueueTimeout => {
                    eprintln!("[error] feedback queue timeout - initiating shutdown");
                    self.fatal_error_seen = true;
                    event_loop.exit();
                }
                _ => {
                    eprintln!("[error] dispatch failed: {:?}", err);
                }
            }
        }
        
        // ... rest of render logic (present, etc.)
    }
}
```

---

### Step C: Engine Thread Loop (4-5 hours)

**C.1**: Implement `engine_loop()`

```rust
// crates/glaphica/src/engine_core.rs
use crate::sample_source::SampleSource;

pub fn engine_loop(
    mut core: EngineCore,
    mut sample_source: impl SampleSource,
) {
    let mut samples_buffer = Vec::with_capacity(1024);
    let mut feedback_merge_state = GpuFeedbackMergeState::<RuntimeReceipt, RuntimeError>::default();
    let mut pending_feedback: Option<GpuFeedbackFrame<RuntimeReceipt, RuntimeError>> = None;
    
    loop {
        // 1. Drain input samples (Phase 4: channel, Phase 4.5: ring)
        sample_source.drain_batch(&mut samples_buffer, 1024);
        
        // 2. Process business logic (brush, merge, etc.)
        for sample in &samples_buffer {
            core.process_input_sample(sample);
        }
        
        // 3. Send GPU commands (based on processed input)
        for cmd in core.pending_commands.drain(..) {
            match core.engine_channels.gpu_command_sender.push(GpuCmdMsg::Command(cmd)) {
                Ok(()) => {}
                Err(rtrb::PushError::Full(_)) => {
                    // Command queue full - skip this frame's commands
                    break;
                }
                Err(rtrb::PushError::Disconnected(_)) => {
                    // Main thread disconnected - shutdown
                    core.shutdown_requested = true;
                    break;
                }
            }
        }
        
        // 4. Drain and merge feedback
        while let Ok(frame) = core.engine_channels.gpu_feedback_receiver.pop() {
            pending_feedback = Some(match pending_feedback.take() {
                None => frame,
                Some(current) => {
                    GpuFeedbackFrame::merge_mailbox(current, frame, &mut feedback_merge_state)
                }
            });
        }
        
        // 5. Apply merged feedback
        if let Some(frame) = pending_feedback.take() {
            core.process_feedback(frame);
        }
        
        // 6. Check shutdown
        if core.shutdown_requested {
            // Send final shutdown command
            let _ = core.engine_channels.gpu_command_sender.push(
                GpuCmdMsg::Command(RuntimeCommand::Shutdown { 
                    reason: "EngineCore shutdown requested".to_string() 
                })
            );
            break;
        }
    }
}
```

---

**C.2**: Implement `EngineCore::process_input_sample()`

```rust
impl EngineCore {
    fn process_input_sample(&mut self, sample: &InputRingSample) {
        // Brush session logic, etc.
        // Generate RuntimeCommands based on sample processing
        // ...
    }
}
```

---

### Step D: Feedback Processing & Waterlines (2-3 hours)

**D.1**: Implement `EngineCore::process_feedback()`

```rust
impl EngineCore {
    fn process_feedback(
        &mut self, 
        frame: GpuFeedbackFrame<RuntimeReceipt, RuntimeError>
    ) {
        // Store last waterlines for monotonicity check
        let last_submit = self.waterlines.submit;
        let last_executed = self.waterlines.executed;
        let last_complete = self.waterlines.complete;
        
        // 1. Update waterlines (max merge - monotonic guarantee)
        self.waterlines.submit = self.waterlines.submit.max(frame.submit_waterline);
        self.waterlines.executed = self.waterlines.executed.max(frame.executed_batch_waterline);
        self.waterlines.complete = self.waterlines.complete.max(frame.complete_waterline);
        
        // 2. Debug: assert monotonicity
        #[cfg(debug_assertions)]
        {
            assert!(frame.submit_waterline >= last_submit, "submit waterline regression");
            assert!(frame.executed_batch_waterline >= last_executed, "executed waterline regression");
            assert!(frame.complete_waterline >= last_complete, "complete waterline regression");
        }
        
        // 3. Process receipts
        for receipt in frame.receipts.iter() {
            self.apply_receipt(receipt);
        }
        
        // 4. Process errors
        for error in frame.errors.iter() {
            self.handle_error(error);
        }
        
        // 5. Safe-to-release decisions based on complete_waterline
        self.gc_evict_before_waterline(self.waterlines.complete);
    }
}
```

---

**D.2**: Implement `EngineCore::apply_receipt()`

```rust
impl EngineCore {
    fn apply_receipt(&mut self, receipt: &RuntimeReceipt) {
        match receipt {
            RuntimeReceipt::InitComplete => {
                eprintln!("[engine] init complete");
            }
            RuntimeReceipt::ResizeComplete => {
                eprintln!("[engine] resize complete");
            }
            RuntimeReceipt::ShutdownAck { reason } => {
                eprintln!("[engine] shutdown ack: {}", reason);
            }
            // ... handle other receipts
            _ => {}
        }
    }
}
```

---

**D.3**: Implement `EngineCore::handle_error()`

```rust
impl EngineCore {
    fn handle_error(&mut self, error: &RuntimeError) {
        match error {
            RuntimeError::FeedbackQueueTimeout => {
                eprintln!("[error] feedback queue timeout - initiating shutdown");
                self.shutdown_requested = true;
            }
            RuntimeError::EngineThreadDisconnected => {
                eprintln!("[error] engine thread disconnected");
                self.shutdown_requested = true;
            }
            // ... handle other errors
            _ => {
                eprintln!("[error] runtime error: {:?}", error);
            }
        }
    }
}
```

---

### Step E: Integration & Testing (3-4 hours)

**E.1**: Modify `GpuState::new()` to create `EngineBridge`

```rust
// crates/glaphica/src/lib.rs
impl GpuState {
    pub async fn new(window: Arc<Window>, startup_image_path: Option<PathBuf>) -> Self {
        // ... existing initialization (renderer, document, tile_merge_engine, etc.)
        
        // Create channels
        let (main_channels, engine_channels) = create_thread_channels(
            input_ring_capacity: 1024,
            input_control_capacity: 64,
            gpu_command_capacity: 256,
            gpu_feedback_capacity: 64,
        );
        
        // Create GpuRuntime (main thread)
        let gpu_runtime = GpuRuntime::new(
            renderer,
            view_sender,
            atlas_store,
            brush_buffer_store,
            size,
            0, // next_frame_id
        );
        
        // Create EngineCore (will move to engine thread)
        let engine_core = EngineCore {
            document,
            tile_merge_engine,
            brush_buffer_tile_keys,
            view_transform,
            gc_evicted_batches_total: 0,
            gc_evicted_keys_total: 0,
            waterlines: EngineWaterlines::default(),
            engine_channels,
            shutdown_requested: false,
            pending_commands: Vec::new(),
        };
        
        // Create sample source (Phase 4: channel)
        let sample_source = ChannelSampleSource {
            receiver: engine_channels.input_ring_consumer.receiver.clone(),
        };
        
        // Spawn engine thread
        let engine_thread = std::thread::spawn(move || {
            engine_loop(engine_core, sample_source);
        });
        
        // Create EngineBridge
        let mut bridge = EngineBridge {
            main_channels,
            gpu_runtime,
            waterlines: MainThreadWaterlines::default(),
            engine_thread: Some(engine_thread),
        };
        
        // Send init command (sync handshake with timeout)
        let (ack_tx, ack_rx) = bounded(1);
        bridge.main_channels.gpu_command_sender.push(
            GpuCmdMsg::Command(RuntimeCommand::Init { ack_sender: ack_tx })
        );
        
        match ack_rx.recv_timeout(Duration::from_secs(5)) {
            Some(Ok(())) => {
                eprintln!("[startup] init handshake successful");
            }
            Some(Err(err)) => {
                panic!("[startup] init failed: {:?}", err);
            }
            None => {
                panic!("[startup] init handshake timeout (5s)");
            }
        }
        
        let mut state = Self {
            bridge,
            #[cfg(debug_assertions)]
            last_bound_render_tree: None,
        };
        
        state.note_bound_render_tree("startup", &initial_snapshot_for_trace);
        eprintln!("[startup] initial render tree bound");

        state
    }
}
```

---

**E.2**: Unit Tests

```rust
// crates/glaphica/src/engine_bridge.rs
#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_engine_thread_shutdown() {
        // Create EngineBridge
        // Drop it - should join engine thread gracefully
        let bridge = EngineBridge::new(...);
        drop(bridge);  // Should not hang
    }
    
    #[test]
    fn test_feedback_queue_overflow() {
        // Send many commands without draining feedback
        // Verify backpressure or timeout behavior
    }
}
```

---

**E.3**: Integration Tests

```rust
// crates/glaphica/src/tests.rs
#[test]
fn test_command_feedback_roundtrip() {
    // Create EngineBridge
    // Send command, verify feedback received
}

#[test]
fn test_waterline_monotonicity() {
    // Simulate out-of-order feedback frames
    // Verify merge_mailbox produces monotonic waterlines
}
```

---

## 4. Risk Matrix

| Risk | Probability | Impact | Mitigation |
|------|-------------|--------|------------|
| **Feedback queue overflow** | Medium | High | Bounded timeout + shutdown trigger; Debug panic |
| **Waterline regression** | Low | High | Debug assert monotonicity; merge uses `max` |
| **EngineCore non-Send** | Medium | High | Compiler check; avoid `Rc`/`Cell` |
| **Main thread blocking** | Medium | High | Command budget; feedback timeout |
| **Deadlock** | Low | High | "Locks don't cross command boundaries"; no blocking while holding locks |
| **Handshake timeout** | Low | Medium | 5s/1s timeouts; clear error messages |

---

## 5. Definition of Done (Phase 4)

- [ ] `EngineBridge` created, manages channels + thread lifecycle
- [ ] `EngineCore` split from `AppCore` (exclusive ownership, no `Arc`/`RwLock`)
- [ ] Commands sent via channel, executed on main thread
- [ ] Feedback sent via channel, merged on engine thread
- [ ] Waterlines monotonic (debug assert)
- [ ] Handshake (Init/Resize) works with timeout
- [ ] Shutdown graceful (no hangs)
- [ ] Tests pass (command roundtrip, waterline monotonicity, shutdown)

---

## 6. Future Extensions (Phase 4.5+)

- **Phase 4.5**: Integrate input ring (replace `ChannelSampleSource` with `RingSampleSource`)
- **Phase 5**: True async command execution (futures instead of blocking)
- **Phase 6**: GPU completion tracking (fence-based, not batch-based)

---

## 7. References

- `crates/protocol/src/lib.rs` - Protocol definitions
- `crates/engine/src/lib.rs` - Channel factory
- `docs/Instructions/tiles_model_runtime_refactor_guide.md` - Overall refactor plan
- `docs/Instructions/app_core_error_design.md` - Error handling design

---

**Status**: Ready for implementation  
**Next Action**: Step A - Bridge type definitions
