# Glaphica

A GPU-accelerated drawing application with tile-based rendering architecture.

## Overview

Glaphica is a Rust-based GUI drawing application featuring:

- **Tile-based rendering**: Canvas divided into 128x128 tiles with GPU-backed atlas storage
- **Brush execution engine**: Configurable brush behavior with intermediate buffer architecture
- **Layer system**: Support for multiple image layers with dirty tracking and partial compositing
- **GPU-first design**: wgpu-based rendering with CPU→GPU command queue semantics

## Project Structure

```
glaphica/
├── crates/
│   ├── brush_execution/    # Brush engine and stroke processing
│   ├── document/           # Document model and layer management
│   ├── driver/             # Input sampling and event handling
│   ├── glaphica/           # Main application binary
│   ├── model/              # Shared data models and layout constants
│   ├── renderer/           # GPU rendering and composite pipelines
│   ├── render_protocol/    # Cross-module message types
│   ├── tiles/              # Tile atlas allocation and management
│   └── view/               # View transform and coordinate systems
└── docs/
    ├── Instructions/       # Core guidelines and playbooks
    ├── Wiki/               # Design decision records
    └── debug/              # Debug case studies
```

## Quick Start

### Prerequisites

- Rust 1.85+ (edition 2024)
- wgpu-compatible GPU (Vulkan/Metal/DX12)

### Build & Run

```bash
cargo run
```

### Development

```bash
# Check all crates
cargo check --workspace

# Run tests
cargo test --workspace

# Run specific crate tests
cargo test -p renderer --lib
```

## Documentation

- **[📚 Docs Navigation](docs/README.md)** - Complete documentation index
- **[🤖 AI Agent Guidelines](AGENTS.md)** - For AI-assisted development
- **[🐛 Debug Playbook](docs/Instructions/debug_playbook.md)** - Troubleshooting guide
- **[📐 Coding Guidelines](docs/Instructions/coding_guidelines.md)** - Code standards

## Architecture Highlights

### Tile Model

- **Tile Size**: 128x128 pixels (126 usable + 2px gutter)
- **Atlas Layout**: 32x32 tiles per atlas (4096x4096 texture)
- **Formats**: RGBA8 (filterable), R32Float (storage), R8Uint

### Brush Pipeline

```
Input → Sample → Dab → Command → GPU Execution → Merge → Document
```

See [`crates/brush_execution/DESIGN_DECISIONS.md`](crates/brush_execution/DESIGN_DECISIONS.md) for details.

### Render Tree

- Revision-based caching with semantic hashing
- Partial composite with dirty tile tracking
- Group cache hierarchy for nested transformations

See [`crates/renderer/DESIGN.md`](crates/renderer/DESIGN.md) for architecture details.

## Environment Variables

### Debug Switches (default off)

```bash
GLAPHICA_BRUSH_TRACE=1           # Brush/merge submission trace
GLAPHICA_RENDER_TREE_TRACE=1     # Render tree bind/revision tracking
GLAPHICA_RENDER_TREE_INVARIANTS=1  # Fail-fast on semantic changes
GLAPHICA_PERF_LOG=1              # Dirty polling and cache perf hints
GLAPHICA_FRAME_SCHEDULER_TRACE=1 # Frame scheduler ticks
GLAPHICA_QUIET=1                 # Global quiet mode
```

### Example

```bash
GLAPHICA_BRUSH_TRACE=1 GLAPHICA_PERF_LOG=1 cargo run
```

## Testing

### Unit Tests

```bash
cargo test -p <crate> <test_name> -- --nocapture
```

### GPU Tests (single-threaded for stability)

```bash
cargo test -p renderer -- --ignored --test-threads=1
```

## Key Design Decisions

### Tile Size: 128px

Chosen balance between:
- Brush stroke latency (smaller tiles = more tiles per dab)
- Large brush performance (256px brush = ~9 tiles at 128px vs ~25 at 64px)

See [`docs/Wiki/brush_pipeline_design_decisions_2026-02-20.md`](docs/Wiki/brush_pipeline_design_decisions_2026-02-20.md).

### Merge Lifecycle

```
StrokeEnded → MergeSubmitted → CompletionNotice → Ack → DocumentCommit → BufferRetained → BufferReleased
```

See [`docs/Wiki/brush_merge_lifecycle_decisions_2026-02-21.md`](docs/Wiki/brush_merge_lifecycle_decisions_2026-02-21.md).

### wgpu Submit Semantics

⚠️ Critical: Avoid overwriting same buffer range multiple times before single `queue.submit` if multiple passes read it.

See [`docs/Instructions/wgpu.md`](docs/Instructions/wgpu.md) for detailed hazards and safe patterns.

## Contributing

### For AI Agents

Read [`AGENTS.md`](AGENTS.md) before making changes. Key rules:

1. **Protocol changes**: Receiver side implements first, then report
2. **Initiator changes**: Report first, modify after approval
3. **Prefer additive changes**: New fields/types over breaking changes
4. **Fail fast**: Panics over silent fallbacks

### For Humans

1. Read the relevant crate `AGENTS.md`
2. Check existing design decisions in `Wiki/`
3. Add tests for new functionality
4. Update documentation

## License

[Add your license here]

## Acknowledgments

- Built with [`wgpu`](https://github.com/gfx-rs/wgpu)
- Inspired by tile-based rendering architectures

---

**Last Updated**: 2026-02-27
