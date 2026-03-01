# External Integrations

**Analysis Date:** 2026-02-28

## APIs & External Services

**None Detected:**
- This is a standalone desktop application with no external API integrations
- No cloud services, web APIs, or third-party service SDKs detected
- All functionality is local to the user's machine

## Data Storage

**Databases:**
- None - No database integration
- Application state persisted via file-based serialization (serde/serde_json)

**File Storage:**
- Local filesystem only
- Input/output traces serialized to JSON via `window_replay` crate
- No cloud storage integration

**Caching:**
- In-memory caching only (revision-based render tree cache)
- GPU texture atlases managed by `tiles` crate
- No external caching layer (Redis, Memcached, etc.)

## Authentication & Identity

**Auth Provider:**
- None - No authentication system
- Application runs locally without user accounts or online features

## Monitoring & Observability

**Error Tracking:**
- None - No external error tracking (Sentry, Bugsnag, etc.)
- Errors handled via Rust's `Result`/`anyhow` pattern
- GPU errors captured via wgpu's `on_uncaptured_error` callback

**Logs:**
- Environment-gated console logging via `println!` macros
- Debug switches controlled by `GLAPHICA_*` environment variables
- No structured logging framework (tracing, log crate not used)
- No log aggregation or remote logging

## CI/CD & Deployment

**Hosting:**
- Local desktop application (no hosting platform)
- Distributed as native binary (build process not documented)

**CI Pipeline:**
- GitHub Actions (`.github/workflows/ci.yml`)
- Triggers: push, pull_request
- Jobs:
  - `cargo clippy --workspace --all-targets --all-features`
  - `cargo test --workspace --all-targets --all-features`
- Caching: Swatinem/rust-cache@v2

**Version Control:**
- Git (GitHub)
- Standard workflow: feature branches → PR → merge

## Environment Configuration

**Required env vars:**
- None required for normal operation
- Optional debug switches (all default to off):
  - `GLAPHICA_BRUSH_TRACE`
  - `GLAPHICA_RENDER_TREE_TRACE`
  - `GLAPHICA_RENDER_TREE_INVARIANTS`
  - `GLAPHICA_PERF_LOG`
  - `GLAPHICA_FRAME_SCHEDULER_TRACE`
  - `GLAPHICA_QUIET`

**Secrets location:**
- No secrets required
- No `.env` files detected (and ignored in `.gitignore`)
- No credential files in repository

## Webhooks & Callbacks

**Incoming:**
- None

**Outgoing:**
- None

## File Format Integrations

**Supported Formats:**
- PNG images - via `image` crate (png feature)
- JPEG images - via `image` crate (jpeg feature)
- JSON - via `serde_json` for replay traces
- WGSL shader files - native wgpu format
- TOML - via `toml` crate (code_analysis)

**Serialization Protocol:**
- `replay_protocol` crate defines output schema for:
  - `DriverOutput` - Input event traces
  - `BrushExecutionOutput` - Brush execution results
  - `MergeLifecycleOutput` - Merge operation status
  - `RenderCommandOutput` - Render command traces
  - `OutputPhase` - Phase markers for replay synchronization

## GPU Backend Integration

**wgpu Backends (auto-detected):**
- Vulkan (Linux, Windows with compatible GPU)
- Metal (macOS)
- DX12 (Windows 10+)
- WebGPU (browser target not used)

**GPU Resource Management:**
- Direct wgpu API for:
  - Buffer allocation and mapping
  - Texture/atlas creation
  - Compute pipeline execution
  - Render pass encoding
- No GPU abstraction layer beyond wgpu

---

*Integration audit: 2026-02-28*
