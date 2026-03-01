# Tiles Crate 拆分计划

> **状态**: 📝 计划中 (未开始)  
> **创建日期**: 2026-02-27  
> **最后更新**: 2026-02-27  
> **优先级**: 低

## 目标

将 `crates/tiles` 拆分为两个独立的 crate:
- **tiles_core**: 纯逻辑层，无 GPU 依赖
- **tiles_gpu**: GPU 运行时，依赖 wgpu

## 当前结构分析

### Core (纯逻辑 - 无 GPU 依赖)
| 文件 | 内容 |
|------|------|
| `lib.rs` | 核心类型: `TileKey`, `TileAddress`, `TileSetId`, `TileSetHandle`, `TileImage`, `VirtualImage`, `TileDirtyBitset`, `BrushBufferTileRegistry` |
| `atlas/core.rs` | Atlas 核心逻辑: `TileAtlasCpu`, 分配/释放/retain 机制, TileOpQueue |
| `atlas/format_core.rs` | 格式规范: `TileFormatSpec`, `TilePayloadSpec` trait 及实现 |
| `merge_callback.rs` | Merge 回调类型 |

### GPU 相关 (需要 wgpu)
| 文件 | 内容 |
|------|------|
| `atlas/gpu.rs` | WGPU texture 创建, `GenericTileAtlasGpuArray::drain_and_execute` |
| `atlas/format_gpu.rs` | `TileGpuCreateValidator`, `TileGpuOpAdapter` trait 及实现 |
| `atlas/brush_buffer_storage.rs` | GPU 存储实现 |
| `atlas/layer_pixel_storage.rs` | GPU 存储实现 |
| `atlas/group_preview.rs` | GPU 存储实现 |
| `merge_submission.rs` | GPU merge 提交逻辑 |

### 依赖关系
```
tiles (current)
├── render_protocol
├── wgpu
├── pollster
└── bitvec
```

---

## 拆分步骤

### Phase 1: 创建 tiles_core

#### 1.1 创建新 crate
```bash
mkdir -p crates/tiles_core/src
```

#### 1.2 定义 tiles_core/src/lib.rs
移动以下内容:
- `lib.rs` 中的所有公共类型 (TileKey, TileAddress, TileSetId, TileSetHandle, TileImage, VirtualImage, TileDirtyBitset, BrushBufferTileRegistry)
- 相关的 error types
- 常量定义 (TILE_SIZE, TILE_GUTTER 等)
- `merge_callback.rs` 内容

#### 1.3 创建 tiles_core/src/atlas 模块
移动:
- `atlas/core.rs` → `tiles_core/src/atlas/core.rs`
- `atlas/format_core.rs` → `tiles_core/src/atlas/format.rs`
- `atlas.rs` 中的非 GPU 相关定义

#### 1.4 定义 trait 接口
在 tiles_core 中定义 GPU 模块需要实现的 trait:

```rust
// 例如:
pub trait TileAtlasBackend {
    fn allocate(&self) -> Result<TileKey, TileAllocError>;
    fn release(&self, key: TileKey) -> bool;
    // ...
}

pub trait TileAtlasGpu {
    fn view(&self) -> &wgpu::TextureView;
    fn drain_and_execute(&self, queue: &wgpu::Queue) -> Result<usize, ...>;
}
```

#### 1.5 tiles_core/Cargo.toml
```toml
[package]
name = "tiles_core"
version = "0.1.0"
edition = "2024"

[features]
default = []
test-helpers = []

[dependencies]
render_protocol = { path = "../render_protocol" }
bitvec = "1.0.1"
```

### Phase 2: 创建 tiles_gpu

#### 2.1 创建新 crate
```bash
mkdir -p crates/tiles_gpu/src
```

#### 2.2 tiles_gpu 依赖 tiles_core
```toml
[package]
name = "tiles_gpu"
version = "0.1.0"
edition = "2024"

[dependencies]
tiles_core = { path = "../tiles_core" }
wgpu = "28.0.0"
pollster = "0.4.0"
```

#### 2.3 迁移 GPU 模块
- `atlas/gpu.rs` → `tiles_gpu/src/gpu.rs`
- `atlas/format_gpu.rs` → `tiles_gpu/src/format_gpu.rs`
- `atlas/brush_buffer_storage.rs` → `tiles_gpu/src/brush_buffer_storage.rs`
- `atlas/layer_pixel_storage.rs` → `tiles_gpu/src/layer_pixel_storage.rs`
- `atlas/group_preview.rs` → `tiles_gpu/src/group_preview.rs`
- `merge_submission.rs` → `tiles_gpu/src/merge_submission.rs`

实现 tiles_core 定义的 trait 接口。

### Phase 3: 重构原 tiles crate

#### 3.1 tiles/Cargo.toml 改为 facade
```toml
[package]
name = "tiles"
version = "0.1.0"
edition = "2024"

[features]
default = ["gpu"]
gpu = ["tiles_gpu"]

[dependencies]
tiles_core = { path = "../tiles_core" }
tiles_gpu = { path = "../tiles_gpu", optional = true }
```

#### 3.2 tiles/src/lib.rs
简化为 re-export:
```rust
pub use tiles_core::*;

// 条件导出 GPU 类型
#[cfg(feature = "gpu")]
pub use tiles_gpu::*;
```

### Phase 4: 更新依赖方

更新以下 crate 的 Cargo.toml:
- `crates/renderer/Cargo.toml`
- `crates/document/Cargo.toml`
- `crates/brush_execution/Cargo.toml`
- `crates/glaphica/Cargo.toml`

根据实际使用情况选择依赖:
- 仅需逻辑: `tiles_core`
- 仅需 GPU: `tiles_gpu`
- 两者都需要: `tiles`

---

## 关键设计决策

### 接口设计
需要决定 tiles_core 提供什么样的接口让 tiles_gpu 实现:

1. **方案 A**: 分离的 trait
   - `TileAtlasStore` trait (CPU side)
   - `TileAtlasGpuArray` trait (GPU side)

2. **方案 B**: 单一 trait 包装
   - `TileAtlas` trait 同时包含 allocate 和 GPU 操作

### render_protocol 依赖
- 当前 merge_submission 直接使用 render_protocol 类型
- tiles_core 需要决定是否也依赖 render_protocol
- 或者在 tiles_gpu 中处理协议类型转换

### 测试策略
- tiles_core: 保留单元测试
- tiles_gpu: 集成测试
- tiles (facade): 可选的集成测试

---

## 风险与注意事项

1. **循环依赖**: 确保 tiles_core 不依赖 tiles_gpu
2. **API 稳定性**: 拆分后 interface trait 需要谨慎设计
3. **版本管理**: 三方 crate 的版本需要同步更新
4. **文档迁移**: 需要更新相关的 Design.md 文档

---

## 附录：当前决策

根据 `docs/guides/refactoring/tiles-model-runtime.md` 的决策：

- **TileKey 编码方案**: 已决策采用编码 key（backend + generation + slot），但尚未集成到主链路
- **拆分计划状态**: 暂未启动，当前优先级较低
- **替代方案**: 当前采用 `model` crate 统一布局语义，`tiles` 保持完整

如未来启动拆分，需参考重构指南的 Phase 划分。
