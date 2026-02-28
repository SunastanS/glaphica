# Testing Patterns

**Analysis Date:** 2026-02-28

## Test Framework

**Runner:**
- `cargo test` (Rust built-in test framework)
- Rust Edition 2024
- No external test framework dependencies (uses `#[test]` from `std`)

**Assertion Library:**
- Standard `assert!()`, `assert_eq!()`, `assert_ne!()`, `matches!()`
- Custom panic messages with context

**Run Commands:**
```bash
cargo test                          # Run all tests
cargo test -p tiles -- --nocapture  # Run tiles crate tests with output
cargo test -p renderer -- --nocapture  # Run renderer tests
cargo test -p renderer <test_name> -- --nocapture  # Run specific test
cargo test --test-threads=1         # Single-threaded (for GPU tests)
```

## Test File Organization

**Location:**
- Co-located test modules in `src/tests.rs` per crate
- Inline `#[cfg(test)]` modules in source files
- Test utilities in dedicated modules: `phase4_test_utils.rs`

**File structure:**
```
crates/
├── tiles/
│   └── src/
│       ├── lib.rs        # mod tests; at bottom
│       └── tests.rs      # 1200+ lines of unit tests
├── renderer/
│   └── src/
│       ├── lib.rs        # mod tests; mod wgsl_tests;
│       ├── tests.rs      # 1800+ lines of unit tests
│       └── wgsl_tests.rs # WGSL shader tests
└── glaphica/
    └── src/
        ├── lib.rs        # #[cfg(test)] modules inline
        ├── phase4_test_utils.rs
        └── phase4_threaded_tests.rs
```

**Module declaration from `crates/renderer/src/lib.rs`:**
```rust
mod tests;
#[cfg(test)]
mod wgsl_tests;
```

## Test Structure

**Test module pattern from `crates/tiles/src/tests.rs`:**
```rust
use super::*;

fn create_device_queue() -> (wgpu::Device, wgpu::Queue) {
    pollster::block_on(async {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..Default::default()
        });
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None,
                force_fallback_adapter: false,
            })
            .await
            .expect("request wgpu adapter");
        let limits = adapter.limits();
        adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("tiles tests"),
                required_features: wgpu::Features::empty(),
                required_limits: limits,
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
                memory_hints: wgpu::MemoryHints::Performance,
                trace: wgpu::Trace::Off,
            })
            .await
            .expect("request wgpu device")
    })
}

fn create_store(
    device: &wgpu::Device,
    config: TileAtlasConfig,
) -> (TileAtlasStore, TileAtlasGpuArray) {
    TileAtlasStore::with_config(device, config)
        .expect("TileAtlasStore::with_config")
}

#[test]
fn config_default_uses_medium15_tier() {
    let config = TileAtlasConfig::default();
    assert_eq!(config.tier, AtlasTier::Medium15);
}

#[test]
fn ingest_tile_should_define_gutter_pixels_from_edge_texels() {
    let (device, queue) = create_device_queue();
    let (store, gpu) = create_store(&device, TileAtlasConfig::tiny10());
    
    // Test implementation...
    assert_eq!(tile_count, 1);
    assert!(tile.iter().all(|&byte| byte == 0));
}
```

**Patterns observed:**
- Helper functions for test setup (no `#[test]` setup/teardown attributes)
- Descriptive test names explaining expected behavior
- Arrange-Act-Assert structure
- Multiple assertions per test when validating related invariants

## Test Naming Conventions

**Pattern:** `{functionality}_should_{expected_behavior}()`

**Examples:**
- `config_default_uses_medium15_tier()`
- `ingest_tile_should_define_gutter_pixels_from_edge_texels()`
- `release_is_cpu_only_and_dirty_triggers_clear_on_reuse()`
- `frame_sync_rejects_commit_after_epoch_change()`
- `build_leaf_tile_draw_instances_panics_on_unresolved_tile_key()`

**Panic test naming:**
- `{function}_panics_on_{condition}()` for `#[should_panic]` tests

## Mocking

**Pattern:** Custom resolver structs implementing traits

**Example from `crates/renderer/src/tests.rs`:**
```rust
#[derive(Default)]
struct DirtyPropagationResolver {
    propagate_calls: Cell<u32>,
    propagated_rects: HashMap<u64, Vec<DirtyRect>>,
}

impl RenderDataResolver for DirtyPropagationResolver {
    fn document_size(&self) -> (u32, u32) {
        (TILE_IMAGE, TILE_IMAGE)
    }

    fn visit_image_tiles(
        &self,
        _image_handle: ImageHandle,
        _visitor: &mut dyn FnMut(u32, u32, TileKey),
    ) {
        // No-op or controlled behavior
    }

    fn propagate_layer_dirty_rects(
        &self,
        layer_id: u64,
        incoming_rects: &[DirtyRect],
    ) -> Vec<DirtyRect> {
        self.propagate_calls.set(self.propagate_calls.get() + 1);
        self.propagated_rects
            .get(&layer_id)
            .cloned()
            .unwrap_or_else(|| incoming_rects.to_vec())
    }

    fn resolve_tile_address(&self, _tile_key: TileKey) -> Option<TileAddress> {
        None
    }

    fn layer_dirty_since(
        &self,
        _layer_id: u64,
        _since_version: u64,
    ) -> Option<tiles::DirtySinceResult> {
        None
    }

    fn layer_version(&self, _layer_id: u64) -> Option<u64> {
        None
    }
}
```

**What to Mock:**
- External dependencies (wgpu device/queue for unit tests)
- Resolver traits with controlled responses
- File I/O and network operations

**What NOT to Mock:**
- Core domain logic (test real behavior)
- GPU pipeline creation (test with real wgpu when possible)

## Fixtures and Factories

**Test data helpers:**
```rust
fn leaf(layer_id: u64, blend: BlendMode) -> RenderTreeNode {
    RenderTreeNode::Leaf {
        layer_id,
        blend,
        image_source: render_protocol::ImageSource::LayerImage {
            image_handle: image_handle(),
        },
    }
}

fn group(group_id: u64, blend: BlendMode, children: Vec<RenderTreeNode>) -> RenderTreeNode {
    RenderTreeNode::Group {
        group_id,
        blend,
        children: children.into_boxed_slice().into(),
    }
}

fn snapshot(revision: u64, root: RenderTreeNode) -> RenderTreeSnapshot {
    RenderTreeSnapshot {
        revision,
        root: std::sync::Arc::new(root),
    }
}

fn allocate_tile_keys(count: usize) -> Vec<TileKey> {
    pollster::block_on(async {
        // Setup wgpu device and atlas
        // Return allocated keys
    })
}
```

**Location:**
- Defined inline in test files (no separate fixtures directory)
- Helper functions at top of test modules

## Coverage

**Requirements:** No explicit coverage enforcement configured

**View Coverage:**
```bash
cargo install cargo-tarpaulin
cargo tarpaulin --out Xml        # Generate coverage report
cargo tarpaulin --html           # HTML report
```

## Test Types

**Unit Tests:**
- Primary test type throughout codebase
- Test individual functions and methods
- Use mock resolvers for dependencies
- Run with `cargo test -p <crate>`

**Integration Tests:**
- Located in `tests/` directory at workspace root
- Test cross-crate interactions
- Currently minimal (resources and records directories)

**GPU Tests:**
- Full wgpu device initialization in tests
- Use `pollster::block_on()` for async runtime
- Mark environment-sensitive tests with `#[ignore]`
- Run with `--test-threads=1` for GPU resource isolation

**Example GPU test:**
```rust
#[test]
fn ingest_tile_enqueues_upload_and_writes_after_drain() {
    let (device, queue) = create_device_queue();
    let (store, gpu) = create_store(&device, TileAtlasConfig::default());
    
    let mut bytes = vec![0u8; (TILE_IMAGE as usize) * (TILE_IMAGE as usize) * 4];
    bytes[0] = 9;
    let key = store
        .ingest_tile(TILE_IMAGE, TILE_IMAGE, &bytes)
        .expect("ingest tile")
        .expect("non-empty tile");
    
    let address = store.resolve(key).expect("resolve key");
    let tile_count = gpu.drain_and_execute(&queue).expect("drain upload");
    assert_eq!(tile_count, 1);
    
    let tile = read_tile_rgba8(&device, &queue, gpu.texture(), address);
    assert_eq!(&tile[..4], &[9, 8, 7, 6]);
}
```

## Ignored Tests

**Pattern:** `#[ignore = "reason"]` for tests requiring special conditions

**Examples from `crates/renderer/src/tests.rs`:**
```rust
#[test]
#[ignore = "repro for tile coordinate mapping regression; run explicitly while debugging"]
fn composite_tile_mapping_renders_quadrant_image_exactly() {
    // Reproduction harness for debugging specific regressions
}

#[test]
#[ignore = "repro for nested group-cache mapping regressions; run explicitly while debugging"]
fn nested_group_cache_slot_mapping_renders_correct_result() {
    // Reproduction harness for debugging
}
```

**Run ignored tests:**
```bash
cargo test -- --ignored              # Run only ignored tests
cargo test -- --include-ignored      # Run all tests including ignored
```

## Common Patterns

**Async testing with `pollster`:**
```rust
#[test]
fn some_async_test() {
    pollster::block_on(async {
        // Async test code
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..Default::default()
        });
        // ...
    });
}
```

**Panic testing:**
```rust
#[test]
#[should_panic(expected = "document size must be positive")]
fn document_clip_matrix_panics_on_zero_size() {
    let _ = document_clip_matrix_from_size(0, 1);
}

#[test]
#[should_panic(expected = "layer tile key unresolved while building full leaf draw instances")]
fn build_leaf_tile_draw_instances_panics_on_unresolved_tile_key() {
    // Test code that should panic
}
```

**Feature-gated tests:**
```rust
#[cfg(test)]
mod tests {
    #[test]
    #[cfg(feature = "test-helpers")]
    fn test_with_feature() {
        // Only runs with feature enabled
    }
}
```

**State tracking in mocks:**
```rust
use std::cell::Cell;

#[derive(Default)]
struct FakeResolver {
    visit_calls: Cell<u32>,
    resolve_calls: Cell<u32>,
    emit_tiles: bool,
}

impl RenderDataResolver for FakeResolver {
    fn visit_image_tiles(&self, _image_handle: ImageHandle, visitor: &mut dyn FnMut(u32, u32, TileKey)) {
        self.visit_calls.set(self.visit_calls.get() + 1);
        // Controlled behavior
    }
}

#[test]
fn test_tracks_calls() {
    let resolver = FakeResolver { emit_tiles: true, ..Default::default() };
    // Exercise code
    assert_eq!(resolver.visit_calls.get(), 1);
}
```

**Invariant testing:**
```rust
#[test]
fn atlas_size_should_match_tile_stride_and_capacity_contract() {
    let tile_stride = TILE_STRIDE;
    assert_eq!(
        ATLAS_SIZE,
        TILES_PER_ROW * tile_stride,
        "atlas should preserve tiles-per-row capacity when adding 1px gutter"
    );
}
```

## Test Best Practices (from AGENTS.md)

**From renderer AGENTS.md:**
- Unit tests validate frame sync behavior, geometry helpers, dirty propagation, planning decisions
- For GPU tests, consider `--test-threads=1` for resource isolation
- Mark environment-sensitive tests with `#[ignore]` and document intended run command
- Convert bug reproductions into focused regression tests
- Tests should be the "preferred fix vehicle" - flip from red to green with minimal fix

**Run commands:**
```bash
cargo test -p renderer <test_name> -- --nocapture
cargo test -p tiles -- --nocapture
```

---

*Testing analysis: 2026-02-28*
