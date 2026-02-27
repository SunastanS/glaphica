# Phase 4 Detailed Implementation Plan and Decision Analysis

## Key evidence from your refactoring branch and platform constraints

Your repository already contains a fairly opinionated “engine/protocol + ring semantics” foundation that strongly influences Phase 4 design choices.

On the **protocol side**, `GpuFeedbackFrame` is explicitly designed around **waterlines + mailbox merging**, with **receipts/errors as non-overwritable deltas** that must not be dropped for correctness, and with guidance that callers should prefer waterline-based feedback when possible. fileciteturn45file0L1-L1 This is exactly the kind of contract you want if you plan to process feedback *periodically* and still remain correct by taking maxima for monotonic progress and merging only the non-contiguous deltas.

On the **engine side**, the channel and ring types encode *different semantics for different traffic classes*:

- **Input ring** is intentionally *lossy* (drop/evict is acceptable) and optimized for “newest data keeps flowing” even under contention. fileciteturn40file0L1-L1  
- **Input control queue** is *reliable* and grows a **bounded blocking push with timeout + backoff** instead of unbounded blocking, indicating an explicit backpressure policy rather than “just block forever”. fileciteturn45file0L1-L1  
- The engine code also uses structural markers (`Cell<()>`) to discourage accidental `Sync` sharing for SPSC-style producer/consumer usage, reinforcing “single owner per endpoint” as an architectural invariant rather than a casual convention. fileciteturn45file0L1-L1  

Separately, the **platform layer constraints** around windowing and surfaces strongly affect what you can safely move off the main thread:

- `winit` documents that while the window type is `Send + Sync`, on some platforms (macOS/Web/iOS) UI interactions are restricted to the main thread and cross-thread calls may be scheduled to main and block. citeturn2search6  
- `wgpu` surface creation has a platform-specific panic note (“macOS/Metal: will panic if not called on the main thread”), reinforcing that surface/window interactions are not a good candidate for a background “GPU thread” on all targets. citeturn2search10  

Taken together, these facts point to a thread model where:
- the **main/UI thread** remains the safest place to own window/surface/present, and
- the “engine thread” is better positioned as a **business/scheduling thread** that *feeds* GPU work to the main thread via channels.

That matches the spirit of your `engine` crate naming (“MainThreadChannels” vs “EngineThreadChannels”) and the performance-oriented semantics you have already encoded. fileciteturn45file0L1-L1  

## Target topology restated in repository terms

A Phase 4 design consistent with your current `engine/protocol` primitives is:

- **Engine thread**: owns the “business core” (what you currently call `AppCore` responsibilities: merge scheduling, brush session orchestration, document-driven decisions), consumes `InputRingSample` + `InputControlEvent`, and produces `GpuCmdMsg<RuntimeCommand>` messages.
- **Main thread**: owns the actual GPU executor (what you currently call `GpuRuntime` + renderer/surface/present), consumes `GpuCmdMsg<RuntimeCommand>`, executes them, and emits `GpuFeedbackFrame<RuntimeReceipt, RuntimeError>` back to the engine thread; it also owns exit policy for fatal GPU failures.

This is not just a preference, it’s the most robust way to deal with the “main-thread-only-ish” constraints seen in `winit`/`wgpu`, while still obtaining the intended CPU/GPU decoupling and backpressure semantics. citeturn2search6turn2search10  

Within this topology, Phase 4 is less about inventing a new wire protocol (you already have one) and more about:
- extracting a “thread-safe engine-side core” out of the current `AppCore` (which currently holds `GpuRuntime`), and
- implementing a **main-thread dispatcher loop** that drains GPU commands → runs `GpuRuntime::execute` → sends feedback frames.

Your repository already treats mailbox merging as a first-class primitive (`GpuFeedbackFrame::merge_mailbox(...)` requiring a reusable `GpuFeedbackMergeState`) and clarifies that waterlines should be “max-mergeable.” fileciteturn41file0L1-L1  

## Decision analysis for the key Phase 4 questions

Below are the requested Q1–Q4 analyses, but framed to align with what your `engine/protocol` crates are already optimized for.

### Q1: Where should the engine thread be “managed”?

**Option A: manage the engine thread inside `GpuRuntime`.**  
This is attractive if you think of the engine thread as a “GPU thread,” but it becomes awkward if you accept the likely reality: the main thread is still the safest place for surface/present/window integration on all targets. citeturn2search6turn2search10

If `GpuRuntime` owns:
- the join handle,
- the engine thread channel endpoints, and
- shutdown signals,

then `GpuRuntime` becomes simultaneously:
- a GPU resource owner/executor **and**
- a cross-thread lifecycle orchestrator.

This worsens layering: your own refactor narrative has been consistently “keep GPU resources encapsulated and simplify the facade.” Even Phase 2.5-B shows `AppCore` calling into `runtime.execute` as a clean interface boundary. fileciteturn46file0L1-L1 Adding thread lifecycle into the runtime tends to reintroduce “God object” gravitational pull.

**Option B: create a dedicated `EngineThread` (or `EngineHost`) struct that owns the thread and its channels.**  
This option aligns more cleanly with the division your `engine` crate already implies: channels are a independent “bridge layer,” and both endpoints should be owned by structures whose *only job* is to provide safe lifecycle, backpressure rules, and clear drop semantics. fileciteturn45file0L1-L1

This also matches the SPSC intent you already embedded:
- rtrb endpoints are designed to be moved between threads but not shared as references across threads (i.e., `Send` but not `Sync`). citeturn3view2  
- your engine code reinforces this by discouraging `Sync` access to the input ring producer/consumer. fileciteturn45file0L1-L1  

If you put ownership in a dedicated `EngineThread` type, you can make illegal states unrepresentable:
- exactly one engine thread exists,
- exactly one sender/receiver endpoint exists per ring,
- shutdown is explicit and testable,
- and the `GpuRuntime` stays “pure main-thread GPU executor.”

**Recommendation (Q1): Option B — use a dedicated `EngineThread`/`EngineBridge` owner.**  
Concretely, define:
- `EngineBridgeMain`: lives on main thread, owns `MainThreadChannels<RuntimeCommand, RuntimeReceipt, RuntimeError>`, `GpuRuntime`, waterline counters used by the main thread, and the `GpuFeedbackMergeState` that’s needed to merge mailbox frames (or keep merge state engine-side if that’s where you merge). fileciteturn41file0L1-L1  
- `EngineThread`: owns the join handle + `EngineThreadChannels<...>` + the engine-side business core.

This keeps each layer honest and avoids re-coupling runtime internals with thread supervision.

### Q2: Command execution should be synchronous (blocking) or async (non-blocking)?

This decision is mostly about *where you want to pay complexity*.

**Option A: synchronous “send command → wait for feedback → return Result.”**  
Pros:
- simplest migration path from the existing `AppCore::render()` style (which currently performs `runtime.execute(...)` and matches on the immediate receipt/error). fileciteturn46file0L1-L1  
- aligns with the mental model of a function call.

Cons:
- If you block the engine thread waiting for feedback while the main thread is also waiting on the engine thread (directly or indirectly through control flow), you risk subtle deadlocks.
- It undermines your own channel semantics: you already distinguish lossy input signals from reliable control, and you carefully bounded blocking for control events using timeouts and backoff. That’s a signal that “blocking exists, but only as a controlled exception.” fileciteturn45file0L1-L1  
- It increases the chance that the main thread stalls, which is the worst-case scenario for UI responsiveness.

**Option B: async pipeline “submit commands → main thread executes when it can → feedback merged and processed periodically.”**  
Pros:
- matches the *mailbox merge + waterline* contract: you can safely merge frames and take max waterlines, and only treat receipts/errors as durable deltas. fileciteturn45file0L1-L1  
- naturally fits event-loop driven rendering: “each frame, drain N commands, emit feedback, and redraw.”
- aligns with your input ring design: for high-frequency streams you already accept loss and prioritize newest samples; this only makes sense if the engine doesn’t “block waiting on every small thing.” fileciteturn40file0L1-L1  

Cons:
- requires state machines rather than direct return values.
- requires you to identify which operations really need synchronous completion.

**Practical hybrid (often the best Phase 4 rollout):**
- Make the system async by default.
- Add a *small, explicit* sync mechanism only where unavoidable:
  - resizing that must be applied before next present,
  - initialization handshake,
  - or “query-like” commands where business logic cannot proceed without the result.

But importantly: do **not** implement sync by blocking the UI thread. If you require synchronous completion, block the engine thread with a timeout, and ensure the main thread continues to dispatch GPU commands and generate feedback. Your control-queue timeout pattern is a good precedent. fileciteturn45file0L1-L1  

**Recommendation (Q2): Option B — async by default, with explicit bounded sync only for a tiny set of operations.**  
This preserves your system’s responsiveness properties and leverages the “absorb/merge mailbox frames” contract you already invested in. fileciteturn41file0L1-L1  

### Q3: Waterline tracking granularity: per-command or per-batch?

Your protocol design is already a strong hint:

- Waterlines are intended to be monotonic and “max-mergeable” across mailbox merges. fileciteturn41file0L1-L1  
- Receipts/errors are explicitly described as non-overwritable deltas and should be used sparingly, preferring waterlines whenever possible. fileciteturn45file0L1-L1  

This implies that the optimal waterline should correspond to a unit of progress that:
- is frequent enough to model correctness-critical sequencing,
- but not so frequent that waterline traffic becomes overhead.

**Per-command waterlines** are usually:
- too fine-grained,
- increase state churn,
- and (most importantly) often don’t correspond to correctness boundaries if GPU execution is already batch/submission based.

**Per-batch waterlines** are the natural fit if you define “batch” as:
- “all GPU commands drained and executed during one main-thread dispatch slice” (often one render tick),
- or “one frame worth of GPU work.”

This matches your channel design and mailbox merge: you can drop intermediate feedback frames (merge them) and still know progress maxima.

**Recommendation (Q3): Option B — per-batch waterlines.**  
Concretely, define:

- `SubmitWaterline`: incremented by the engine thread when it publishes a *batch* of `RuntimeCommand`s that must be ordered together (e.g., “drain tiles + enqueue brush ops + present this frame”).  
- `ExecutedBatchWaterline`: incremented by the main thread when it finishes executing that published batch.  
- `CompleteWaterline`: incremented when the engine thread has processed feedback and advanced its own “safe-to-release” decisions (or when main thread has observed completions, depending on which side actually owns completion knowledge).

The exact semantic mapping is your choice, but per-batch is the right granularity to preserve the “merge by max” contract without excessive noise. fileciteturn45file0L1-L1  

### Q4: When should feedback be processed: once per frame or “realtime”?

You already have a mailbox+merge design that makes this decision easier:

- The fact that feedback frames are **mergeable** (absorptive for waterlines, dedup/merge for receipts/errors) is a direct invitation to process feedback in **periodic chunks** rather than per-message realtime. fileciteturn41file0L1-L1  
- Your input ring consumer is explicitly “drain batch with wait timeout,” i.e., a periodic polling pattern rather than realtime callbacks. fileciteturn40file0L1-L1  

**Per-frame feedback processing** (or more generally, per “engine tick”) is:
- easier to reason about,
- better aligned with winit’s redraw-driven rendering loop,
- less likely to introduce re-entrancy hazards.

**Realtime feedback processing** is only worth it if:
- you have ultra-low-latency correctness requirements (e.g., immediate cancellation, sub-frame latency input-to-output),
- and you are willing to pay complexity around concurrent state updates.

Given your present design goals (clarity, refactor safety, avoiding silent fallbacks) and the “merge mailbox” abstraction, realtime processing is not required for Phase 4 correctness or ergonomics.

**Recommendation (Q4): Option A — process feedback at most once per frame (but drain *all available* feedback frames each time).**  
That is: each engine tick, drain the feedback receiver until empty, merge them into one “current mailbox,” then apply state transitions once. This preserves low overhead and strong determinism.

## A Phase 4 implementation plan consistent with the repo’s primitives

This plan assumes you align with the direction implied by the `engine` crate: engine thread produces GPU commands; main thread executes them and produces GPU feedback. fileciteturn45file0L1-L1  

### Architecture deltas needed before wiring channels

The largest mismatch today is that `AppCore` holds a concrete `GpuRuntime` and calls `runtime.execute(...)` directly, expecting immediate results. fileciteturn46file0L1-L1 Phase 4 requires `AppCore` logic to run independently of the concrete GPU executor.

The smallest-change way to get there is:

- Split “business core” from “runtime executor” by introducing a trait boundary instead of moving types across threads immediately:
  - `trait RuntimePort { fn send(&mut self, cmd: RuntimeCommand); fn drain_feedback(&mut self) -> impl Iterator<Item = GpuFeedbackFrame<...>>; }`
- Implement two concrete ports:
  - `MainRuntimeExecutor`: on the main thread, wraps `GpuRuntime` and is fed by `gpu_command_receiver` (consume commands).
  - `EngineRuntimeProxy`: on the engine thread, wraps `gpu_command_sender` and `gpu_feedback_receiver` (send commands, drain feedback).

Once this boundary exists, migrating business logic to the engine thread is substantially safer.

### Phase 4 step sequence

A safe incremental sequence (minimizing “big bang” risk) is:

**Step A: Introduce the bridge types and compile without changing behavior**
- Add `EngineBridgeMain` struct on main thread:
  - owns `MainThreadChannels<RuntimeCommand, RuntimeReceipt, RuntimeError>`,
  - owns `GpuRuntime`,
  - owns main-thread waterline counters (at least `PresentFrameId` and `ExecutedBatchWaterline`),
  - owns “fatal seen” flag (your existing Phase 3 pattern is compatible). fileciteturn46file0L1-L1  
- Add `EngineThread` struct:
  - owns `EngineThreadChannels<...>` and join handle.
- Add a `RuntimeCommand::Shutdown` (or equivalent) if you choose explicit shutdown over abandonment-based signaling.

At this step, the engine thread can be a stub that sends no GPU commands, just to validate lifecycle and shutdown.

**Step B: Implement the main-thread GPU command dispatcher**
- In the winit redraw loop (or equivalent), do:
  1. Drain `gpu_command_receiver` up to a budget (commands or time).
  2. Execute each `RuntimeCommand` by calling `GpuRuntime::execute(cmd)` (or a new method that takes command and returns receipt/error).
  3. Package outputs into a `GpuFeedbackFrame`:
     - bump/assign `present_frame_id` if this batch includes present,
     - update `executed_batch_waterline` once the batch is done,
     - include any non-contiguous receipts/errors in the frame while keeping them reliable. fileciteturn45file0L1-L1  
  4. Push the `GpuFeedbackFrame` into `gpu_feedback_sender`.

At this step, you validate the true main-thread execution wiring before you move business logic.

**Step C: Move a minimal slice of `AppCore` logic onto the engine thread**
Start with the safest / most deterministic paths:
- `resize` orchestration,
- “present frame” orchestration,
- and a no-op document pipeline that just requests a present.

This aligns with your Phase 2.5-B work where `render()` and `resize()` already have structured error types and clear fatal/recoverable splits. fileciteturn46file0L1-L1  

**Step D: Implement engine-side feedback processing using mailbox merge**
- On the engine thread, per tick:
  1. Drain `gpu_feedback_receiver` until empty.
  2. Merge frames using `GpuFeedbackFrame::merge_mailbox(current, newer, &mut merge_state)`. fileciteturn41file0L1-L1  
  3. Apply merged receipts/errors to engine state.
  4. Use waterlines to decide which resources/transactions are safe to finalize.

**Step E: Gradually migrate higher-risk paths**
- Brush enqueue and merge submission/polling become natural next steps, because they benefit from waterline-driven “safe to release” policies and from reliable receipt/error deltas (merge completions, allocation results). fileciteturn45file0L1-L1  

### Shutdown and abandonment strategy

You have two viable shutdown signaling mechanisms:

- **Explicit shutdown command** (`RuntimeCommand::Shutdown`):
  - clearer, deterministic, easier to test.
- **Abandonment-based shutdown**:
  - `rtrb` documents that dropping the consumer marks the producer as abandoned and can be checked from the other side. citeturn3view2  
  - useful as a “last resort” fallback if the other side is gone unexpectedly.

For Phase 4, explicit shutdown is usually the cleanest and matches your “fail-fast, no silent fallback” principle.

## Risk areas and validation checklist

The highest-risk failure modes in Phase 4 are mostly about **backpressure correctness** and **lifecycle safety**:

- **Feedback queue overflow**: receipts/errors are defined as correctness-critical deltas and must not be dropped. fileciteturn45file0L1-L1  
  - Validation: in debug builds, treat `PushError::Full` on feedback as a hard error; in release, consider bounded blocking with timeout (mirroring your control queue policy), but avoid deadlocking the UI thread. fileciteturn45file0L1-L1  
- **Waterline monotonicity violations**: mailbox merge assumes monotonic progress and uses maxima. fileciteturn41file0L1-L1  
  - Validation: debug-assert monotonicity at both endpoints; add unit tests that merge out-of-order frames and verify max semantics are correct.
- **Accidental multi-owner use of SPSC endpoints**: you already discourage `Sync` on ring endpoints. fileciteturn45file0L1-L1  
  - Validation: keep bridge constructs private; expose only safe methods; avoid cloning endpoints.
- **Main-thread constraints ignored**: moving surface/present off main thread can break on macOS/Metal. citeturn2search10  
  - Validation: keep window/surface and present in main dispatcher; keep engine thread strictly CPU/business.

A good “Definition of Done” for Phase 4, aligned with your existing protocol semantics, is:

- Main thread drains and executes engine-produced GPU commands and emits feedback frames.
- Engine thread merges feedback and updates waterlines and state.
- Waterlines are monotonic, and mailbox merge remains absorptive/max-correct.
- No correctness-critical receipts/errors are dropped. fileciteturn45file0L1-L1
