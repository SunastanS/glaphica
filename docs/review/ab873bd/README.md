# Review: glaphica @ ab873bd

- **Target**: Full codebase
- **Commit**: ab873bd ("Update session image edit docs")
- **Reason**: Whole-codebase legacy review — all code initially untrusted
- **Scope**: All 11 crates, 4 design docs, inline tests
- **Out-of-scope**: WGSL shader semantics (GPU path will be replaced)
- **Status**: Complete — all layers reviewed bottom-up, 17 decisions confirmed

## Confirmed Architecture Decisions

| # | Layer | Decision | Status |
|---|-------|----------|--------|
| 1 | Pool | Placeholder; will be reimplemented | Confirmed |
| 2 | TileKey | Empty-bind ≠ INVALID; intentional & stable | Confirmed |
| 3 | GlaImage | copy_on_write/free/backfill are dead APIs | Confirmed |
| 4 | Document | Will be gradually eliminated; truth in Janet | Confirmed |
| 5 | Active chain | Rust computes impact chains; stays core responsibility | Confirmed |
| 6 | Dirty upload | Identity+None precise; rest conservative fallback | Confirmed |
| 7 | render_impl | Always recompute local shadows; caching is dessert | Confirmed |
| 8 | Commit | Primitive/derived asymmetry intentional | Confirmed |
| 9 | GPU renderer | Entire path will be replaced | Confirmed |
| 10 | IR layer | Stable; RegistryPatch stays | Confirmed |
| 11 | FrameBudget | Will add Rust-side budgeting | Confirmed |
| 12 | Undo/redo | Apply-inverse mechanism final; history → tree | Confirmed |
| 13 | Tool execution | Rust-compiled GPU shaders; draw_dab→record, flush→execute | Confirmed |
| 14 | Backup/Current | Key resolution only; rendering ops identical | Confirmed |
| 15 | Local keys | No generation checking needed | Confirmed |
| 16 | Janet integration | Documentation-only; no code | Confirmed |
| 17 | Test strategy | Inline characterization tests adequate for stage 1 | Confirmed |

## Suspect Findings → ADRs

- **ADR 01**: Discard old tiles on patch apply (primitive tile leak)
- **ADR 02**: Reject zero-size images at creation

## Reliability Records

- `drawon-tool-execution-gap` — Tool execution model in DrawOn
- `command-lowering-gap` — Command lowering semantic model
- `tile-lifecycle-history` — Primitive tile leak in undo/redo
