# Brush Execution Design Decisions

This document tracks architecture and implementation decisions for the `brush_execution` crate.

## Decision Log

### 2026-02-18 - Crate Initialization

- Status: accepted
- Context: We are starting the brush execution layer design from scratch.
- Decision: Create a dedicated `brush_execution` crate and maintain a living decision log in this file.
- Consequences:
  - Brush execution concerns are isolated from renderer and scheduler crates.
  - Future design discussions can be captured incrementally with explicit rationale.

### 2026-02-18 - Execution Layer Role and Boundaries

- Status: accepted
- Context: We need a dedicated, configurable brush execution layer between `driver` and `renderer`.
- Decision:
  - `brush_execution` runs as an independent brush execution thread.
  - It consumes sample stream data from `driver` and transforms it into dabs, then renderer-consumable paint commands.
  - It does not write directly into destination textures; it writes into an intermediate buffer.
  - The strategy used to merge/render this buffer into real textures is brush-engine-defined (not a single fixed pipeline).
- Consequences:
  - Clear ownership boundary between input generation (`driver`), execution (`brush_execution`), and final presentation (`renderer`).
  - Future brush engines can customize merge behavior without changing core renderer interfaces.

### 2026-02-18 - Reference Texture Access Model

- Status: accepted
- Context: Brush execution needs read-only context about existing layers to decide output.
- Decision:
  - Brush execution can request reference textures from external layer sources.
  - Supported selection modes:
    - a specified layer
    - current layer
    - current layer plus all layers below
    - all layers
  - Brush execution can read reference textures at runtime and use sampled data to generate render commands.
  - Brush execution must not read back its own intermediate buffer to avoid GPU lock contention and severe performance loss.
- Consequences:
  - Engine behavior can be context-aware with controlled read access.
  - Prohibiting intermediate buffer readback avoids a major synchronization bottleneck.

### 2026-02-18 - Brush Engine Language Direction

- Status: accepted
- Context: Brush execution has many configuration and behavior dimensions; hardcoding all policy in Rust would be rigid and costly.
- Decision:
  - Brush engine is defined as an independent scripting language.
  - Language scope is intentionally small: arithmetic operations and control flow only.
  - All control flow is compile-time expanded and must be finite.
  - Syntax direction: functional S-expression.
- Consequences:
  - Behavior can be configured and extended without recompiling the host.
  - Constraining the language simplifies implementation and safety reasoning.

### 2026-02-18 - Script Interface Functions and Execution Targets

- Status: accepted (high-level), details pending
- Context: Brush engine needs explicit host entry points that map to CPU/GPU execution stages.
- Decision:
  - Public script function forms:
    - `(fn sample (sample) dab-expr)` for sample-to-dab preprocessing; this is the only stateful function.
    - `(fn paint (ctx dab buf ref) color-expr)` for painting into current intermediate buffer using buffer/reference pixels.
    - `(fn merge (ctx buf origin ref) color-expr)` for composing intermediate buffer into true layer output.
    - `(fn ref-preprocessing (raw-ref) ref-expr)` for preprocessing and caching reference layers.
  - Execution split:
    - `sample` runs on CPU.
    - `paint`, `merge`, `ref-preprocessing` run on GPU.
- Consequences:
  - Pipeline has explicit stage contracts and future compilation targets.
  - CPU/GPU responsibilities are clear before implementation begins.

### 2026-02-18 - Cross-Layer Terminology Standard

- Status: accepted
- Context: Previous discussions used `dab` to refer to both driver output and brush stamps, which creates ambiguity.
- Decision:
  - Canonical semantic chain is `input -> sample -> dab -> command`.
  - `input`: raw device/pointer events entering `driver`.
  - `sample`: output of driver sampling stage (driver-facing stream).
  - `dab`: brush-execution stamp unit produced from samples.
  - `command`: renderer-facing command produced from dabs.
  - Driver-side struct naming must follow `sample` terminology.
- Consequences:
  - Layer boundaries are explicit and less error-prone.
  - Type names and APIs align with actual data semantics.

### 2026-02-18 - `sample` Stateful Scope

- Status: accepted
- Context: Script function `(fn sample (sample) dab-expr)` is the only stateful stage and requires a precise state lifetime boundary.
- Decision:
  - `sample` state is isolated per stroke.
  - State is created at stroke begin and destroyed at stroke end.
  - No state sharing across different strokes.
- Consequences:
  - Determinism and reasoning become simpler because stroke-local history cannot leak across strokes.
  - Concurrency model is safer since per-stroke execution has no implicit mutable global state.

### 2026-02-18 - `paint` Buffer Access Semantics

- Status: accepted (model), operator set pending finalization
- Context: We need expressiveness for accumulation-style brush behavior while preserving predictable GPU execution.
- Decision:
  - `paint` uses read-modify-write semantics on the intermediate `buf`.
  - Intended per-pixel logic stays simple, with operators in the style of:
    - `average`
    - `max`
    - `plus_max`
  - Mid-stage logic should remain lightweight rather than introducing deeply nested/expensive formulas.
- Consequences:
  - Brush behavior can implement common accumulation and dominance effects directly in `paint`.
  - Execution complexity remains controlled by constraining operations to simple math.

### 2026-02-18 - Intermediate Buffer Type Is Configurable

- Status: accepted
- Context: Different brush families require very different intermediate representations.
- Decision:
  - `buf` is a configurable brush-engine option, not a single fixed global format.
  - Expected examples include:
    - hard-edge style brushes: `bool`-like mask representation
    - soft brushes: `f32` scalar/intensity representation
    - blur/smudge/drag-like brushes: `sRGBA` color representation
  - The engine must support selecting an appropriate buffer kind per brush definition.
- Consequences:
  - The same execution pipeline can cover both simple mask brushes and color-carrying brushes.
  - API design must include explicit buffer type metadata in brush configuration and command planning.

### 2026-02-18 - M1 Buffer Type Scope

- Status: accepted
- Context: Full multi-type buffer support (`bool`/`f32`/`sRGBA`) is desirable long-term, but we need a narrow first milestone.
- Decision:
  - M1 supports only `f32` intermediate buffer.
- Consequences:
  - We can validate end-to-end execution architecture with lower implementation risk.
  - Type-system and shader-path generalization to other buffer kinds is deferred to later milestones.

### 2026-02-18 - Language Runtime Direction (Draft)

- Status: proposed
- Context: We need one scripting language with CPU and GPU execution stages (`sample` on CPU; `paint`/`merge`/`ref-preprocessing` on GPU).
- Decision (draft):
  - Keep a single front-end parser/validator for S-expression source.
  - Compile to an internal typed IR first, then lower to execution backends.
  - CPU stage (`sample`) executes via host runtime backend.
  - GPU stages lower from IR to WGSL.
- Consequences:
  - Avoids hard-coupling source language directly to one runtime library.
  - Makes future backend replacement feasible without rewriting parser and semantic checks.

### 2026-02-18 - Reference Access via Built-in Sampling Functions

- Status: accepted
- Context: Passing reference images as explicit function parameters makes script signatures noisy and less flexible.
- Decision:
  - Reference/texture reads are exposed as language built-ins instead of explicit parameters.
  - Built-ins:
    - `(sample-origin uv) -> color`
    - `(sample-brush uv) -> f32 | color`
  - Runtime behavior when target texture/reference is unavailable:
    - `sample-origin` returns current primary color.
    - `sample-brush` returns pure black.
- Consequences:
  - Script function signatures become simpler and more stable.
  - Missing-reference behavior is deterministic and centralized in runtime built-ins.
  - Compiler/runtime must type-check built-in return types against caller expectations.

### 2026-02-18 - Script Function Signatures Updated for Built-ins

- Status: accepted
- Context: After introducing built-in texture sampling, explicit `ref`/origin-like parameters are no longer required in GPU stages.
- Decision:
  - Signature direction becomes:
    - `(fn sample (sample) dab-expr)`
    - `(fn paint (ctx dab buf) color-expr)`
    - `(fn merge (ctx buf) color-expr)`
    - `(fn ref-preprocessing (raw-ref) ref-expr)`
  - Texture/reference access inside expressions uses built-ins (`sample-origin`, `sample-brush`) rather than argument passing.
- Consequences:
  - External host API surface for GPU-stage script entry points is reduced.
  - Future built-ins can extend capabilities without repeatedly changing all function signatures.

### 2026-02-18 - M1 Color Representation and Compute Space

- Status: accepted
- Context: Choosing only `RGBA8 (0..255)` for all stages is simple but risks precision and color-space artifacts in iterative brush math.
- Decision:
  - External color-facing representation in M1 can use `RGBA8` semantics.
  - Runtime compute representation (including `buf` math) uses linear premultiplied `f32`.
  - Conversion policy:
    - sampled/input color: `RGBA8 (sRGB)` -> `linear premultiplied f32`
    - output/presentation color: `linear premultiplied f32` -> `RGBA8 (sRGB)`
- Consequences:
  - Reduces banding and repeated-blend artifacts for accumulation operations.
  - Establishes consistent alpha behavior for merge/composition semantics.
  - Keeps external APIs practical while preserving internal compute quality.

### 2026-02-18 - M1 Built-in Return Type Constraint

- Status: accepted
- Context: Built-ins were designed to allow `(sample-brush uv) -> f32 | color`, but M1 deliberately narrows execution scope.
- Decision:
  - In M1, `sample-brush` returns `f32` only.
- Consequences:
  - Simplifies type checking and IR instruction set for the first implementation.
  - Avoids mixed scalar/color semantics before multi-buffer and richer type support land.

### 2026-02-18 - M1 `sample-origin` Behavior

- Status: accepted
- Context: `sample-origin` can conceptually sample reference/origin textures, but M1 scope should remain narrow.
- Decision:
  - In M1, `sample-origin` returns the current primary color.
- Consequences:
  - Removes dependency on full origin/reference sampling pipeline in first milestone.
  - Keeps script behavior deterministic while core execution infrastructure is validated.

### 2026-02-18 - M1 Script Loading Strategy

- Status: accepted
- Context: We need runtime loading and debugging soon, but integrating editor-front communication now would expand scope too much.
- Decision:
  - M1 uses local-file-based script loading with hot reload.
  - Non-file sources (editor memory API, remote fetch) are deferred.
- Consequences:
  - Fastest path to validate runtime load/update loops and debugging workflow.
  - Keeps early system complexity focused on compiler/runtime correctness rather than frontend integration.

### 2026-02-18 - M1 Diagnostics and Debugging Strategy

- Status: accepted
- Context: Building a custom debugger and full diagnostic stack too early would significantly increase implementation cost.
- Decision:
  - Prefer translating script to intermediate/target representations and reusing their diagnostic capability in M1.
  - Do not build a custom step-debugger in M1.
  - Keep only a thin host-side mapping layer so backend errors can be mapped back to script source spans.
- Consequences:
  - Faster progress on runtime loading and basic observability.
  - Still requires stable source-span tracking through lowering stages.
  - Rich interactive debugging remains deferred to later milestones.

### 2026-02-19 - M1 Renderer Pipeline Ownership and Control ACK

- Status: accepted
- Context: Runtime script/pipeline loading must not trigger expensive GPU pipeline construction in the render hot path.
- Decision:
  - Renderer remains the sole owner of GPU resources and pipeline lifecycle.
  - Brush-side program loading is expressed as control commands sent to renderer.
  - Program readiness is gated by synchronous ACK (`Prepared`/`AlreadyPrepared`/`Activated`) before stroke data commands are accepted.
  - Render hot path must never build pipeline objects; missing prepared program at stroke begin is treated as runtime invariant violation.
- Consequences:
  - Pipeline creation cost is shifted to load/update time instead of dab-time.
  - Command queue semantics become split into control-plane (low frequency) and data-plane (high frequency).
  - Runtime failures become explicit and observable rather than hidden by lazy fallback creation.

### 2026-02-19 - M1 Brush Command Protocol Split

- Status: accepted
- Context: Existing `BrushCommandBatch` cannot express runtime-loaded programs or activation semantics.
- Decision:
  - Introduce new protocol types:
    - control: `UpsertBrushProgram`, `ActivateBrushProgram`
    - data: `BeginStroke`, `PushDabChunkF32`, `EndStroke`
  - Keep data-plane payload lightweight by carrying mostly handles (`brush_id`, `program_revision`, `stroke_session_id`) plus compact dab arrays.
  - In M1, `PushDabChunkF32` carries `x/y/pressure` only.
- Consequences:
  - Brush engine extensibility improves without exposing renderer internals.
  - Cross-thread copy volume stays bounded while keeping low-level expression potential.
  - Legacy `BrushCommandBatch` is no longer the main path for brush execution.

### 2026-02-19 - M1 Reference Set Handle

- Status: accepted
- Context: Stroke data should avoid repeatedly sending layer selection metadata for each dab chunk.
- Decision:
  - Introduce `reference_set_id` handle in `BeginStroke`.
  - Add control command `UpsertReferenceSet` to register layer selection once.
  - Renderer validates reference set existence at `BeginStroke` and fails fast on missing handle.
- Consequences:
  - Data-plane remains compact while preserving future reference sampling extensibility.
  - Missing reference metadata becomes an explicit runtime invariant violation.

### 2026-02-19 - Buffer Merge Timing Rule

- Status: accepted
- Context: Buffer-to-texture merge should not be tied to stroke end; brush effects may need to continue mutating previous-stroke buffer data before commit.
- Decision:
  - Dabs write only to intermediate brush buffer.
  - Buffer merge is represented as explicit data-plane command `MergeBuffer`.
  - Merge is emitted at next-stroke boundary: after previous stroke `EndStroke` and before next stroke `BeginStroke`.
  - Renderer enforces fail-fast ordering invariants:
    - `BeginStroke` is rejected when unmerged ended strokes exist.
    - `MergeBuffer` is rejected if stroke was not ended or layer target mismatches.
- Consequences:
  - Enables future post-stroke buffer effects (blur/distort/etc.) before merge.
  - Keeps renderer hot path deterministic via explicit command ordering.
  - Leaves an intentional edge case: last stroke in a session may remain unmerged until a later trigger command is introduced.

### 2026-02-19 - Tile Atlas Generic Payload Foundation

- Status: accepted
- Context: Brush execution requires tile-atlas-backed intermediate buffers with multiple pixel payloads (`f32`, `bool`-like masks, and existing RGBA image content), but existing tiles infrastructure was RGBA8-ingest-specific.
- Decision:
  - Introduce generic tile atlas primitives in `tiles`:
    - `GenericTileAtlasConfig`
    - `GenericTileAtlasStore`
    - `GenericTileAtlasGpuArray`
    - `TilePayloadKind` (`Rgba8`, `R32Float`, `R8Uint`)
  - Keep existing RGBA-facing APIs (`TileAtlasStore`, `TileAtlasGpuArray`, group atlas types) as compatibility wrappers so renderer/glaphica main render path remains unchanged.
  - Keep RGBA gutter behavior only on RGBA upload path; `R32Float`/`R8Uint` do not use RGBA ingest/gutter expansion.
  - For `R32Float` payload atlases, enforce storage-oriented creation contract at atlas creation time (including `STORAGE_BINDING` usage support checks) and fail fast when unsupported.
- Consequences:
  - Brush buffer tiling can start from a shared allocator/op-queue base without re-implementing atlas lifecycle logic.
  - Existing document image ingest/rendering remains stable during migration.
- Unsupported hardware/format combinations surface as explicit creation failures instead of silent fallback.

### 2026-02-22 - Brush Buffer Key Lifecycle Ownership in `tiles`

- Status: accepted
- Context: Previous brush buffer flow left lifecycle responsibility split across layers, with `brush_execution` and upper layers attempting to participate in key release timing. This caused ownership ambiguity and made retained-window behavior fragile.
- Decision:
  - `tiles` is the single owner of brush buffer tile key lifecycle after allocation.
  - After merge success, brush buffer keys are transitioned to retained state through `tiles` APIs.
  - Key release is on-demand only (for example, atlas retention eviction pressure), not time-based.
  - `brush_execution` does not manage pending/retained/release state for keys and does not emit key release commands.
  - Upper layers (including `glaphica`) only forward merge success/failure and eviction events to `tiles`; they do not maintain independent key lifecycle truth.
- Consequences:
  - Lifecycle authority is centralized, reducing double-release and state divergence risk.
  - Command protocol and execution layering become clearer: `brush_execution` handles stroke/dab/merge sequencing only.
  - Retained behavior is deterministic and pressure-driven, avoiding timer-driven policy complexity.
  - Future lifecycle policy changes can be implemented inside `tiles` without widening cross-crate lifecycle coupling.

## Open Questions

- What exact transport carries driver -> brush_execution input (lock-free queue, channel, ring buffer, shared mapped memory)?
- What exact transport carries brush_execution -> renderer commands?
- What are cache invalidation rules for `ref-preprocessing` results (per frame, per stroke, on layer revision change, manual)?
- What is the minimal first milestone (M1) we should implement in this crate?
- CPU `sample` backend for M1: direct Rust interpreter over IR, `rhai`, or `wasm`?
