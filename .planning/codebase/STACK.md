# Technology Stack

**Analysis Date:** 2026-02-28

## Languages

**Primary:**
- Rust 1.93.1 (Edition 2024) - All source code across 15 crates

**Secondary:**
- WGSL (WebGPU Shading Language) - GPU compute shaders for brush execution and tile compositing
- Shell scripts - CI/CD automation

## Runtime

**Environment:**
- Native desktop application (Windows/macOS/Linux via winit)
- GPU-accelerated via wgpu (WebGPU abstraction layer)

**Package Manager:**
- Cargo (Rust)
- Lockfile: `Cargo.lock` present (auto-generated, version 4 format)

## Frameworks

**Core:**
- wgpu 28.0.0 - GPU rendering abstraction (Vulkan/Metal/DX12 backend)
- winit 0.30.12 - Cross-platform window management and event handling
- slotmap 1.1.1 - Efficient entity/component ID mapping for render tree and tile management

**Testing:**
- cargo test (built-in Rust test framework)
- GPU tests run with `--test-threads=1` for stability

**Build/Dev:**
- cargo clippy - Linting (run in CI)
- cargo test - Unit and integration testing
- rust-cache@v2 - GitHub Actions caching

## Key Dependencies

**Critical:**
- bytemuck 1.25.0 (with derive feature) - Zero-copy type conversions for GPU buffer structs
- bitvec 1.0.1 - Bit-level data structures for tile metadata and layout tracking
- smallvec 1.13.2 - Stack-allocated vectors for protocol messages
- serde 1.0.228 (with derive) - Serialization for replay protocol and window state
- serde_json 1.0 - JSON serialization for input/output traces

**Infrastructure:**
- crossbeam-channel 0.5.15 - Lock-free channel communication between threads
- crossbeam-queue 0.3 - Concurrent queue primitives
- rtrb 0.3.2 - Real-time ring buffer for brush execution pipeline
- image 0.25.9 (png, jpeg features) - Image file I/O for texture loading
- pollster 0.4.0 - Async runtime helper for GPU initialization
- anyhow 1.0 - Error handling utilities
- clap 4.5 (with derive) - CLI argument parsing (code_analysis crate)
- walkdir 2.5 - Directory traversal (code_analysis crate)
- toml 0.8 - TOML parsing (code_analysis crate)

**External Git Dependency:**
- rust-code-analysis (mozilla/rust-code-analysis.git, master branch) - Code parsing in code_analysis crate

**WGSL Tooling:**
- naga 28.0.0 (with wgsl-in feature) - Shader parsing/validation (dev dependency for testing)

## Configuration

**Environment Variables (Debug Switches - default off):**
- `GLAPHICA_BRUSH_TRACE=1` - Brush/merge submission trace logging
- `GLAPHICA_RENDER_TREE_TRACE=1` - Render tree bind/revision tracking
- `GLAPHICA_RENDER_TREE_INVARIANTS=1` - Fail-fast on semantic changes
- `GLAPHICA_PERF_LOG=1` - Dirty polling and cache performance hints
- `GLAPHICA_FRAME_SCHEDULER_TRACE=1` - Frame scheduler ticks
- `GLAPHICA_QUIET=1` - Global quiet mode (suppresses business logs)

**Build Configuration:**
- Workspace root: `Cargo.toml` with 15 crate members
- Resolver: version 3 (latest Cargo feature resolver)
- Features: Conditional compilation in `tiles` crate (atlas-gpu, test-helpers)

## Platform Requirements

**Development:**
- Rust 1.85+ (project uses edition 2024, currently on 1.93.1)
- wgpu-compatible GPU with Vulkan (Linux/Windows), Metal (macOS), or DX12 (Windows) support
- Standard build toolchain with clippy component

**Production:**
- Native desktop deployment (no web/browser target)
- GPU drivers supporting WebGPU capabilities
- Tested on Ubuntu (CI), Windows, macOS

## Crate Structure

| Crate | Purpose | Key Dependencies |
|-------|---------|------------------|
| `crates/glaphica/` | Main binary | wgpu, winit, all internal crates |
| `crates/renderer/` | GPU rendering pipelines | wgpu, render_protocol, tiles, model |
| `crates/render_protocol/` | Cross-module message types | slotmap |
| `crates/tiles/` | Tile atlas management | wgpu, model, bitvec |
| `crates/document/` | Document model and layers | slotmap, tiles, model |
| `crates/brush_execution/` | Brush engine | driver, tiles, rtrb |
| `crates/driver/` | Input sampling | rtrb |
| `crates/frame_scheduler/` | Frame timing | (none) |
| `crates/view/` | View transforms | (none) |
| `crates/model/` | Shared data models | bitvec |
| `crates/protocol/` | Protocol primitives | smallvec |
| `crates/engine/` | Command engine | crossbeam, rtrb |
| `crates/replay_protocol/` | Replay serialization | serde, serde_json |
| `crates/window_replay/` | Input/output trace replay | serde, serde_json |
| `crates/code_analysis/` | Code analysis utilities | clap, rust-code-analysis, toml |

---

*Stack analysis: 2026-02-28*
