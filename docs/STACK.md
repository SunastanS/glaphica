# Technology Stack

**Analysis Date:** 2026-03-05

## Languages

**Primary:**
- Rust 2024 Edition - All crates use edition 2024

**Secondary:**
- None detected

## Runtime

**Environment:**
- Native executable (no runtime VM)

**Package Manager:**
- Cargo (Rust's official package manager)
- Lockfile: `Cargo.lock` present (65947 bytes)

## Frameworks

**Core:**
- wgpu 28.0.0 - GPU abstraction layer for cross-platform rendering
- winit 0.30 - Window creation and event loop management
- tokio 1.x (rt-multi-thread) - Async runtime for GPU initialization

**Testing:**
- Built-in Rust test framework (`#[cfg(test)]` modules)
- No additional testing frameworks detected

**Build/Dev:**
- Cargo (build system and package manager)

## Key Dependencies

**Critical:**
- `wgpu` 28.0.0 - GPU abstraction for rendering operations
- `winit` 0.30 - Window management and input handling
- `tokio` 1.x - Async runtime for GPU context initialization
- `crossbeam-channel` 0.5.15 - Multi-producer multi-consumer channels
- `crossbeam-queue` 0.3.12 - Lock-free queues
- `rtrb` 0.3.x - Single-producer single-consumer ring buffers
- `arc-swap` 1.7 - Atomic reference swapping for shared state
- `bitflags` 2.11.0 - Bitflag definitions
- `bytemuck` 1.x - Safe transmutation for GPU data

**Infrastructure:**
- `pollster` 0.4.0 (optional, feature-gated) - Blocking executor for GPU operations

## Configuration

**Environment:**
- No `.env` files detected
- Configuration via Rust code constants (e.g., `ATLAS_TILE_SIZE`, `GUTTER_SIZE` in `crates/glaphica_core/src/lib.rs`)

**Build:**
- Workspace configuration: `Cargo.toml` (workspace with 11 member crates)
- Cargo config: `.cargo/config.toml`

## Platform Requirements

**Development:**
- Rust toolchain with 2024 edition support
- GPU with wgpu-compatible drivers (Vulkan, Metal, DX12, or WebGPU)

**Production:**
- Native executable for target platform
- GPU hardware support required

---

*Stack analysis: 2026-03-05*