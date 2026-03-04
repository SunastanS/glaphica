# External Integrations

**Analysis Date:** 2026-03-05

## APIs & External Services

**Graphics/GPU:**
- wgpu - Cross-platform GPU abstraction
  - SDK/Client: `wgpu` crate 28.0.0
  - Auth: None (native GPU access)

**Windowing:**
- winit - Cross-platform window creation
  - SDK/Client: `winit` crate 0.30
  - Auth: None (native window system)

## Data Storage

**Databases:**
- None - In-memory document model

**File Storage:**
- Local filesystem only (no external storage detected)

**Caching:**
- GPU Atlas backends - Tile-based texture cache in GPU memory
- Branch cache backends - Render tree node caching

## Authentication & Identity

**Auth Provider:**
- None - Single-user desktop application

## Monitoring & Observability

**Error Tracking:**
- Console output via `eprintln!` macros
- Custom error types with `std::error::Error` trait implementation

**Logs:**
- Debug logging via `eprintln!` with structured prefixes:
  - `[INPUT]` - Input event logging
  - `[INPUT_TX]` - Input transmission logging
  - `[ENGINE_RX]` - Engine reception logging
  - `[BRUSH]` - Brush operation logging
  - `[MAIN_RX]` - Main thread reception logging
  - `[MAIN]` - Main thread operation logging
  - `[ENGINE]` - Engine thread operation logging

## CI/CD & Deployment

**Hosting:**
- None - Desktop application

**CI Pipeline:**
- None detected (no `.github/workflows`, `.gitlab-ci.yml`, or similar)

## Environment Configuration

**Required env vars:**
- None detected

**Secrets location:**
- None - No secrets management

## Webhooks & Callbacks

**Incoming:**
- None

**Outgoing:**
- None

## Third-Party Libraries

**Concurrency Primitives:**
- `crossbeam-channel` - Thread communication
- `crossbeam-queue` - Lock-free queues
- `rtrb` - Ring buffer for SPSC patterns
- `arc-swap` - Atomic reference swapping

**Data Handling:**
- `bytemuck` - Safe type casting for GPU buffers
- `bitflags` - Bitflag definitions

**Async Runtime:**
- `tokio` - Async runtime (multi-threaded)
- `pollster` - Blocking async executor (optional)

---

*Integration audit: 2026-03-05*