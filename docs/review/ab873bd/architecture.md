# Architecture Synthesis

## Reliable Anchors

The four design documents (Session.md, Data.md, BrushSystem.md, Color.md) are
the primary reliable anchors. All human-confirmed decisions below are considered
reliable for guiding future analysis and modification.

## Confirmed Architecture (Human-Confirmed)

### Foundation Layer
- **Pool**: Stage 1 placeholder. Will be reimplemented with complete generation-checked
  semantics including local shadow support. Caller-guaranteed safety (debug_assert only)
  is acceptable for now.
- **Empty binding vs INVALID**: Intentional and stable. Empty binding = valid zero
  content (has TileKey, no physical atlas backing). INVALID = cache miss (tile not
  computed yet). This distinction persists through all layers.
- **GlaLocalImageKey**: Simple u32 — no generation checking needed. Session lifecycle
  (consumed at commit) ensures safety.

### Image Layer
- **GlaImages::copy_on_write, free, backfill_invalid_from**: Dead APIs (test-only).
  Actual CoW lives in SessionImage::Edit tile-level write path. `free` never called
  in production — images owned by session/document for their lifetime.
- **insert_invalid**: Only used in tests currently. API for creating derived cache
  images with all-INVALID tiles.

### Document Layer
- **Document will be gradually eliminated**: Truth lives in Janet layer. Rust stores
  derived images and their commands, computes impact chains when Janet publishes
  primitive modifications. `gla_doc` is temporary scaffolding.
- **RegistryPatch stays**: Derived images and their graph commands remain a Rust-layer
  responsibility even after Document elimination.

### IR Layer
- **gla_ir stable**: Two-layer split (id-level IR → key-level commands) is the
  stable cross-language interface. Janet emits id-level IR, Rust lowers to key-level
  image commands.

### Session Layer
- **Active chain shadows**: Rust computes which derived images are affected when a
  primitive is modified. Full-chain shadowing exists for unified local-first key
  resolution. The impact chain computation remains core Rust responsibility.
- **Dirty upload**: Identity+None mappings are precise; Matrix and Expand fall back
  to conservative Full dirty. This is acceptable stage 1 behavior.
- **render_impl local recomputation**: Local shadows always recompute on demand (even
  with valid tiles). This prevents CoW resource sharing from entering command
  semantics. Local caching is a "dessert" optimization — not before app runs end-to-end.
- **Backup vs Current**: Distinction is key resolution only. Backup always resolves
  to `SessionImageKey::Doc` (original document binding), Current uses local-first-then-doc.
  Rendering operations do not distinguish between them.
- **Commit primitive/derived asymmetry**: Intentional. Primitive edits enter DrawHistory
  for undo/redo; derived cache edits are immediately published and old cache tiles
  discarded. Primitive old tiles retained by history — needs cleanup on history
  truncation (→ ADR 01).
- **Undo/redo**: apply-inverse patch mechanism is final design. History structure
  evolves from linear (HashMap<DrawRecordId, ...>) to tree-based for non-linear
  undo branching.
- **FrameBudget**: Will add Rust-side time and work budgeting beyond current
  dab-count limit.

### Tool Layer
- **Tool execution**: Rust-compiled GPU shaders. `draw_dab` records parameters,
  `flush_frame` executes shaders in batch. Current clear-only behavior is placeholder.

### Renderer Layer
- **GPU rendering path**: Entire current implementation will be replaced.
  Scratch texture pattern, per-tile uniform buffers, staging buffer for same-atlas
  copy — all temporary.

## Suspect Code

- **Tile lifecycle in undo/redo**: `apply_image_edit_patch` replaces tiles without
  discarding old ones. Old tiles retained only by DrawHistory — leak on history
  truncation/eviction. → ADR 01
- **Zero-size images**: `GlaImageLayout { width: 0, height: 0 }` produces valid
  image with 0 tiles, but all tile accesses fail out-of-bounds. → ADR 02

## Unverified / Undecided Code

- **Command lowering semantics**: Currently Copy-only. `GraphCommand`/`SessionCommand`
  lack operation identity. `RenderTo`/`Clear` exist in `gla_image_command` but not
  generated during lowering. Decision pending.
  → Reliability record: `command-lowering-gap`

## Open Questions

None remaining. All layers have been reviewed and decisions confirmed.
