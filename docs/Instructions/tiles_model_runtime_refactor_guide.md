# Tiles / Model / Runtime 重构指导方案

## 1. 目标与约束

本方案用于指导当前 `crates/tiles` 破坏性重构，目标分为两条主线：

1. 统一并简化数据结构，消除重复语义和常量冲突。
2. 分离关注点，引入轻量主线程 `runtime` 管理 GPU 资源，并通过 `crates/protocol` 风格通信连接主体业务与 runtime。

本方案基于当前代码现状，不是抽象蓝图。

## 2. 当前现状与核心问题

### 2.1 数据结构层面

1. `tiles` 与 `model` 同时定义 tile 几何语义，且常量不一致。
2. `crates/model/src/lib.rs` 与 `crates/tiles/src/model.rs` 的新结构尚未进入主调用链。
3. `VirtualImage` / `TileImage` / dirty bitset 逻辑散落在 `tiles`，语义边界不清晰。

### 2.2 运行时层面

1. `GpuState` 同时承担了 GPU 资源管理、业务编排、merge 桥接、document 交互，职责过重。
2. `engine + protocol` 已有通道基础，但应用主流程仍是直接函数调用，不是消息边界。
3. `tiles` 已有 CPU allocator + `TileOpQueue` + GPU drain 机制，具备 runtime 拆分基础，但尚未提升为独立边界。

### 2.3 协议层面

1. `render_protocol` 负责渲染域消息，`protocol` 负责线程通信容器，两者职责不同。
2. 本次 runtime 通信应复用 `crates/protocol` 的模式（`GpuCmdMsg` / `GpuFeedbackFrame`），不应把 runtime 线程生命周期逻辑塞进 `render_protocol`。
3. 对 `render_protocol` 字段的改动必须遵循该 crate 协作规则（先 receiver，再 initiator）。

## 3. 目标架构

## 3.1 逻辑分层

1. `crates/model`
   - 纯数据与布局语义。
   - 不依赖 wgpu。
2. `crates/tiles`
   - 保留 tiles 领域逻辑（分配、key 生命周期、tile image 领域行为）。
   - 不直接负责主线程 runtime 循环。
3. `crates/glaphica::runtime`（建议新增模块）
   - 主线程 GPU 资源管理与执行边界。
   - 通过 `protocol` 风格通道接收命令、输出反馈。
4. `crates/glaphica::app_core`（建议从现有 `GpuState` 拆出）
   - document、brush、merge 业务编排。
   - 不直接持有底层 GPU 资源对象。

## 3.2 运行时通信模型

1. 主体侧仅发送 `RuntimeCommand`。
2. runtime 侧仅返回 `RuntimeReceipt` / `RuntimeError`。
3. 实体通道使用 `engine::create_thread_channels` + `protocol::{GpuCmdMsg, GpuFeedbackFrame}`。
4. 短期可先单线程模拟（仍走消息接口），稳定后再切真实线程。

## 4. 迁移总原则

1. 先统一语义，再搬线程边界。
2. 先做无行为变化重排，再做行为迁移。
3. 任何阶段都保留可回滚点。
4. 全程 fail-fast，不引入 silent fallback。
5. `render_protocol` 字段改动走审批流程，不跨层偷改。

## 5. 分阶段执行计划

## Phase 0: 冻结基线与不变量

### 范围

1. 确认并冻结当前关键契约。

### 必做项

1. 明确唯一 tile 几何基线（`TILE_SIZE/TILE_GUTTER/TILE_STRIDE`）。
2. 明确 `TileKey` 语义：
   - 方案 A: opaque id（当前主链路做法）。
   - 方案 B: 编码 key（backend+generation+slot）。
3. 冻结 merge 生命周期不变量：
   - `submit -> completion notice -> ack -> finalize/abort`。

### 验收标准

1. 文档化后，全仓库仅允许一套“权威语义定义”。

## Phase 1: 模型统一（仅语义整合，不改线程模型）

### 目标

将 tile 布局与 image 布局语义集中到 `crates/model`，并让 `tiles` 仅消费该语义。

### 文件级迁移建议

1. 将 `ImageLayout/TilePos/TileImageNew` 的命名和接口定稿到 `crates/model/src/lib.rs`。
2. `crates/tiles/src/lib.rs` 中与布局常量强耦合部分改为引用 `model`。
3. 移除 `crates/tiles/src/model.rs` 中未接入的重复定义，或降级为临时草稿文档，不参与编译。

### 关键约束

1. 不改业务行为。
2. 不改 merge 提交流程。
3. 不改 renderer 外部 API。

### 验收标准

1. `tiles` 不再声明与 `model` 冲突的几何常量。
2. 全仓库所有布局计算都可追溯到 `model`。

### 回滚点

1. 若调用方改动过大，保留 `tiles` 兼容 re-export 一段时间，再二次收敛。

## Phase 2: 从 `GpuState` 拆分 `AppCore` 与 `GpuRuntime`（结构重排）

### 目标

在不改外部行为的前提下，把 `GpuState` 拆成两个结构体：

1. `AppCore`: 业务编排（document/merge/brush state）。
2. `GpuRuntime`: 资源执行（renderer/atlas/surface/present）。

### 拆分边界

1. 仅 GPU 资源对象留在 `GpuRuntime`。
2. `TileMergeEngine`、`BrushBufferTileRegistry`、`Document` 留在 `AppCore`。
3. `AppCore` 通过接口调用 runtime，不直接拿 `wgpu::Device/Queue`。

### 验收标准

1. `GpuState` 不再是 God object。
2. 运行行为与日志基线不变。

### 回滚点

1. 维持原有 `GpuState` facade，内部委托给 `AppCore + GpuRuntime`。

## Phase 3: 建立 runtime 命令/反馈协议（单线程先行）

### 目标

引入消息接口，但先不切线程，降低风险。

### 命令定义建议

`RuntimeCommand`（示例）

1. `DrainTileOps`
2. `EnqueueBrushCommand { .. }`
3. `SubmitPlannedMerge { receipt, ops, meta }`
4. `PollMergeNotices { frame_id }`
5. `AckMergeNotice { notice }`
6. `PresentFrame { frame_id }`
7. `Resize { width, height }`

`RuntimeReceipt`（示例）

1. `TileDrained { executed_tiles }`
2. `MergeSubmitted { submission_report }`
3. `MergeNotices { notices }`
4. `FramePresented`

`RuntimeError`（示例）

1. `TileDrainError`
2. `MergeSubmitError`
3. `MergePollError`
4. `PresentError`

### 实施方式

1. `AppCore` 只调用“发命令/收反馈”接口。
2. 当前进程内先用直接 dispatcher 执行命令（无跨线程）。
3. 保持接口和 `protocol` 容器兼容。

### 验收标准

1. `AppCore` 对 `Renderer` 的直接方法调用显著减少，转为命令式。

## Phase 4: 接入 `engine + protocol` 真通道

### 目标

将 Phase 3 的命令接口映射到真实通道，形成主体与 runtime 解耦。

### 执行步骤

1. 用 `engine::create_thread_channels<RuntimeCommand, RuntimeReceipt, RuntimeError>` 建链。
2. 主线程 event loop 中发送命令、收集反馈。
3. runtime 执行循环消费命令并写入反馈。
4. `GpuFeedbackFrame` 的 waterline 与 receipts/errors 按现有规则合并。

### 验收标准

1. 业务层不持有 runtime 内部对象引用。
2. 断言路径、错误路径、日志路径与现有一致。

### 回滚点

1. 保留“单线程 dispatcher 实现”，可在 feature flag 下切换。

## Phase 5: 清理与收口

### 目标

移除迁移过程中兼容层与重复结构。

### 必做项

1. 删除 `tiles` 中过渡 alias 和重复模型。
2. 文档更新：
   - `docs/Instructions/debug_playbook.md`
   - `crates/tiles/docs/api.md`
3. 统一对外入口，避免多路径初始化 atlas/store。

### 验收标准

1. 新架构路径唯一。
2. 旧路径仅保留短期兼容，不再被主流程依赖。

## 6. 关键设计决策清单（必须先拍板）

1. `TileKey` 最终形态是 opaque 还是编码 key。 -> 编码 Key
2. tile 几何最终是否采用 `stride=128`（对应 image 126+gutter）或保留当前 130。 -> 换用 126 + 2
3. runtime 命令枚举放在：
   - 方案 A: `glaphica` 内部模块。
   - 方案 B: 新建 `crates/runtime_protocol`。 -> 方案 B
4. `merge_submission` 的归属：
   - 继续在 `tiles`（推荐，保持领域语义集中）。
   - 迁入 runtime（不推荐，会污染业务边界）。

## 7. 协议与协作规则

1. `render_protocol` 字段变更前，先确认调用方向。
2. 按规则先改 receiver/executor，再改 initiator/caller。
3. 对 `render_protocol` 的任何字段调整都要附带调用方全量迁移与回归测试。
4. `protocol` crate 仅承载通用通信容器，不承载渲染业务细节。

## 8. 风险矩阵与应对

1. 风险: 常量语义错配导致 tile 映射错乱。
   - 应对: Phase 0 固化单一基线，Phase 1 前全量 grep 清点常量来源。
2. 风险: `GpuState` 拆分后状态同步遗漏。
   - 应对: Phase 2 保留 facade，逐字段搬迁并加 invariant。
3. 风险: 通道化后出现重入与重复 ack。
   - 应对: 维持当前 merge 单通路不变量，不在 poll 中隐式推进状态。
4. 风险: 多阶段迁移造成临时重复结构长期存在。
   - 应对: 每 phase 结束定义删除清单，下一 phase 前先清理。

## 9. 里程碑与交付物

## M1: 模型统一完成

1. 交付物:
   - `model` 成为唯一布局语义源。
   - `tiles` 移除重复语义定义。

## M2: 结构拆分完成

1. 交付物:
   - `AppCore + GpuRuntime` 落地。
   - 外部行为不变。

## M3: 命令接口完成

1. 交付物:
   - runtime 命令/反馈枚举与 dispatcher。
   - 主流程通过命令接口驱动。

## M4: 真通道完成

1. 交付物:
   - `engine + protocol` 接入。
   - 主体与 runtime 解耦。

## M5: 清理完成

1. 交付物:
   - 删除兼容层与重复结构。
   - 文档与架构图更新。

## 10. 执行建议（每次 roll 的粒度）

每一轮只做“一个可验证目标”，推荐粒度：

1. Round A: 仅模型统一与常量收敛。
2. Round B: `GpuState` 纯重排拆分，不引入通道。
3. Round C: 命令接口替换直接调用。
4. Round D: 切 `engine/protocol` 真通道。
5. Round E: 清理旧路径与文档收口。

超过该粒度会显著提高回归风险和上下文失真风险。

## 11. 代码组织建议（目标落点）

1. `crates/glaphica/src/runtime/mod.rs`
2. `crates/glaphica/src/runtime/command.rs`
3. `crates/glaphica/src/runtime/loop.rs`
4. `crates/glaphica/src/app_core/mod.rs`
5. `crates/glaphica/src/app_core/merge_bridge.rs`

`tiles` 内优先保持以下边界：

1. `atlas/core.rs`: CPU allocator + key lifecycle。
2. `atlas/gpu.rs`: GPU drain 执行器。
3. `merge_submission.rs`: merge 业务状态机。
4. `merge_callback.rs`: completion notice 与 ack 类型。

## 12. 完成定义（DoD）

1. 数据模型唯一且无冲突定义。
2. runtime 与主体之间只有消息契约，无直接资源耦合。
3. merge 生命周期不变量保持成立。
4. 关键路径日志与断言仍可用于 debug。
5. 文档与代码结构一致，移除历史 TODO 漂移项。

---

## 13. Phase 1 实现记录（已完成 2026-02-27）

### 13.1 执行状态

**Phase 1 Round A: 模型统一（常量语义整合）** ✅ 已完成

- **执行时间**: 2026-02-27
- **执行者**: AI Agent + User collaboration
- **验收状态**: 
  - ✅ `model` 成为唯一布局语义源
  - ✅ `tiles` 导出弃用别名保持向后兼容
  - ✅ 全 workspace 编译通过
  - ✅ 迁移所有调用方到 `model::TILE_IMAGE`

### 13.2 实际执行步骤

#### Step 1: 统一常量定义 (`crates/model/src/lib.rs`)

```rust
pub const TILE_STRIDE: u32 = 128;
pub const TILE_GUTTER: u32 = 1;
pub const TILE_IMAGE: u32 = TILE_STRIDE - 2 * TILE_GUTTER; // 126
pub const TILE_IMAGE_ORIGIN: u32 = TILE_GUTTER; // 1
```

**决策**: 采用 `126 image + 2 gutter = 128 stride` 方案（指南第 6 节决策 #2）

#### Step 2: 重命名 `tiles/src/model.rs`

**问题**: `tiles` crate 内部有 `mod model;` 与外部 `model` crate 冲突，导致 Rust 优先解析内部模块。

**解决方案**: 
```bash
mv crates/tiles/src/model.rs crates/tiles/src/tile_key_encoding.rs
```

并更新 `tiles/src/lib.rs`:
```rust
mod tile_key_encoding; // 替代 mod model;
```

**经验**: Rust 的模块解析规则 - 同名的内部模块会覆盖外部 crate。在设计 crate 结构时必须避免这种命名冲突。

#### Step 3: 修改 `tiles` 常量导出

```rust
// crates/tiles/src/lib.rs
pub use model::{TILE_STRIDE, TILE_GUTTER, TILE_IMAGE, TILE_IMAGE_ORIGIN};

#[deprecated(since = "0.1.0", note = "Use TILE_IMAGE from model crate instead.")]
pub const TILE_SIZE: u32 = TILE_IMAGE;
```

**策略**: 保留 `TILE_SIZE` 作为弃用别名，提供渐进迁移路径和回滚点。

#### Step 4: 添加依赖

修改 `Cargo.toml` 文件：
- `crates/document/Cargo.toml`: 添加 `model = { path = "../model" }`
- `crates/renderer/Cargo.toml`: 添加 `model = { path = "../model" }`
- `crates/glaphica/Cargo.toml`: 添加 `model = { path = "../model" }`

**经验**: 忘记添加依赖会导致 `unresolved import model` 错误，但这种错误在大型 workspace 中容易被忽略。

#### Step 5: 批量替换常量引用

使用 `sed` 批量替换：
```bash
sed -i 's/\bTILE_SIZE\b/TILE_IMAGE/g' \
  crates/renderer/src/geometry.rs \
  crates/renderer/src/renderer_cache_draw.rs \
  crates/renderer/src/renderer_draw_builders.rs
```

手动编辑其他文件：
- `crates/renderer/src/lib.rs`
- `crates/renderer/src/renderer_frame.rs`
- `crates/document/src/lib.rs`
- `crates/glaphica/src/lib.rs`

#### Step 6: 修复 `tile_key_encoding.rs` 溢出错误

**问题**: 原代码中移位常量计算错误：
```rust
// 错误代码（会溢出）
const SLOT_SHIFT: u64 = (1 << SLOT_BITS) - 1; // 4294967295
const GEN_SHIFT: u64 = SLOT_SHIFT + SLOT_BITS; // 4294967327
const BACKEND_SHIFT: u64 = GEN_SHIFT + GEN_BITS; // 4294967351
```

**解决方案**: 修正为正确的移位值：
```rust
const SLOT_SHIFT: u64 = 0;
const GEN_SHIFT: u64 = SLOT_BITS; // 32
const BACKEND_SHIFT: u64 = SLOT_BITS + GEN_BITS; // 56
```

**经验**: 
1. 移位常量应该是位数，不是掩码
2. 正确的编码布局：`| backend (8) | generation (24) | slot (32) |`
3. Rust 的 `#[deny(arithmetic_overflow)]` 在编译期捕获溢出，这是好事

### 13.3 遇到的困难与解决方案

#### 困难 1: 模块命名冲突

**现象**: `unresolved imports model::TILE_STRIDE, model::TILE_GUTTER...`

**根本原因**: `tiles/src/lib.rs` 中有 `mod model;`，Rust 优先解析内部模块而非外部 crate。

**解决方案**: 重命名内部模块为 `tile_key_encoding`。

**教训**: Crate 内部的模块命名应避免与依赖的 crate 同名。

#### 困难 2: 移位常量溢出

**现象**: 
```
error: this arithmetic operation will overflow
  --> crates/tiles/src/tile_key_encoding.rs:48:13
   | attempt to shift left by `4294967351_u64`, which would overflow
```

**根本原因**: 常量定义混淆了"位数"和"掩码"概念。

**解决方案**: 重新计算正确的移位值（0, 32, 56）。

**教训**: 位操作常量需要仔细审查，尤其是涉及多位域编码时。

#### 困难 3: 依赖遗漏

**现象**: `unresolved import model` 在多个 crate 中。

**根本原因**: 忘记在 `Cargo.toml` 中添加 `model` 依赖。

**解决方案**: 系统性检查并添加依赖。

**教训**: 在 workspace 中添加新依赖时需要系统性地检查所有受影响的 crate。

#### 困难 4: 批量替换的准确性

**现象**: 部分 `TILE_SIZE` 未替换导致编译错误。

**解决方案**: 
1. 先用 `grep -rn "TILE_SIZE"` 定位所有使用位置
2. 对非测试代码使用 `sed` 批量替换
3. 对测试代码和复杂上下文手动替换

**教训**: 批量替换后必须编译验证，grep 搜索是必要的前置步骤。

### 13.4 最佳实践总结

#### 渐进迁移策略 ✅

1. **保留弃用别名**: 不立即删除旧常量，而是添加 `#[deprecated]` 标记
2. **分步验证**: 每修改一个 crate 就编译验证
3. **回滚点**: 保留 `tiles` 的 re-export 作为短期兼容层

#### 模块组织经验

1. **避免命名冲突**: crate 内部模块不要与依赖的 crate 同名
2. **清晰的边界**: `model` crate 只包含纯数据和布局语义，不依赖 wgpu
3. **单一权威源**: 所有几何常量都追溯到 `model` crate

#### 编译验证策略

1. **频繁编译**: 每改动一个文件就 `cargo check`
2. **全量验证**: 最后 `cargo check --workspace` 确保整体正确
3. **利用警告**: 弃用警告帮助识别剩余迁移工作

### 13.5 遗留问题与下一步

#### 遗留问题

1. **tiles 内部弃用警告**: `tiles` crate 内部仍使用 `TILE_SIZE`（43 个警告）
   - **优先级**: 低
   - **计划**: Phase 5 清理阶段统一替换为 `TILE_IMAGE`

2. **tile_key_encoding.rs 未使用代码**: 大量未使用的结构体和方法
   - **优先级**: 低
   - **计划**: 这是重构草稿代码，等待 TileKey 编码方案正式采用后再清理

#### 下一步建议

根据重构指南，推荐按以下顺序继续：

**选项 A: 继续清理（推荐）**
- Phase 1 Round B: 替换 `tiles` 内部的 `TILE_SIZE` 为 `TILE_IMAGE`
- 消除所有弃用警告，完成 Phase 1 收尾

**选项 B: 进入 Phase 2**
- 从 `GpuState` 拆分 `AppCore` 与 `GpuRuntime`
- 这是更大的重构，需要更多上下文和测试支持

**建议**: 先完成 Phase 1 Round B 清理，确保常量迁移完全稳定，再进入 Phase 2。

### 13.6 验证命令

```bash
# 检查编译状态
cargo check --workspace

# 检查 tiles crate（查看弃用警告）
cargo check -p tiles

# 统计 TILE_SIZE 使用情况（应仅剩 tiles 内部）
grep -rn "TILE_SIZE" crates/ --include="*.rs" | wc -l

# 验证 model 是唯一定义源
grep -rn "pub const TILE" crates/model/src/
```

---

### 13.7 Phase 1 Round B 清理记录（已完成 2026-02-27）

**执行时间**: 2026-02-27 (Phase 1 Round A 完成后立即执行)

**执行内容**:
1. 批量替换 `tiles/src/atlas/*.rs` 中的 `TILE_SIZE` → `TILE_IMAGE`
2. 批量替换 `tiles/src/tests.rs` 中的 `TILE_SIZE` → `TILE_IMAGE`
3. 批量替换 `renderer/src/tests.rs` 和 `renderer/src/renderer_frame.rs` 中的 `TILE_SIZE` → `TILE_IMAGE`
4. 移除 `tiles/src/lib.rs` 中的弃用别名定义

**命令记录**:
```bash
# 替换 tiles atlas 文件
sed -i 's/\bTILE_SIZE\b/TILE_IMAGE/g' \
  crates/tiles/src/atlas/layer_pixel_storage.rs \
  crates/tiles/src/atlas/format_core.rs \
  crates/tiles/src/atlas/format_gpu.rs

# 替换 tiles 测试
sed -i 's/\bTILE_SIZE\b/TILE_IMAGE/g' crates/tiles/src/tests.rs

# 替换 renderer 测试
sed -i 's/\bTILE_SIZE\b/TILE_IMAGE/g' \
  crates/renderer/src/tests.rs \
  crates/renderer/src/renderer_frame.rs

# 移除弃用别名（手动编辑 tiles/src/lib.rs）
# 删除以下两行：
# #[deprecated(since = "0.1.0", note = "Use TILE_IMAGE from model crate instead.")]
# pub const TILE_SIZE: u32 = TILE_IMAGE;
```

**验收结果**:
- ✅ 全 workspace 编译通过
- ✅ 0 个弃用警告（`cargo check --workspace 2>&1 | grep -c "deprecated"` = 0）
- ✅ 0 个 `TILE_SIZE` 引用（`grep -rn "TILE_SIZE" crates/ --include="*.rs"` 仅剩 `BRUSH_BUFFER_TILE_SIZE`）

**遗留警告**: 21 个 dead_code 警告（`tile_key_encoding.rs` 中的重构草稿代码）
- 这些是 TileKey 编码方案的草稿实现
- 等待正式采用 TileKey 编码方案后再清理或启用

### 13.8 Phase 2: GpuState 拆分（进行中 2026-02-27）

**执行时间**: 2026-02-27 开始

**目标**: 将 `GpuState` 拆分为 `AppCore`（业务编排）和 `GpuRuntime`（资源执行）

**当前状态**: Step 4A 完成（render 路径基础设施）

#### 已完成步骤

**Step 1-3: 创建骨架** ✅
- 创建 `crates/glaphica/src/runtime/` 模块
  - `protocol.rs`: `RuntimeCommand`/`RuntimeReceipt`/`RuntimeError` 枚举
  - `mod.rs`: `GpuRuntime` 结构体和 `execute()` 方法
- 创建 `crates/glaphica/src/app_core/` 模块
  - `mod.rs`: `AppCore` 结构体和 `MergeStores` 类型
- 实现 `MergeTileStore` trait for `MergeStores`

**Step 4A: render 路径迁移** ✅
- 扩展 `RuntimeError` 支持 `wgpu::SurfaceError`
- AppCore 添加 `next_frame_id` 字段管理
- 实现 `AppCore::render()` 使用 `RuntimeCommand::PresentFrame`
- 保留 `GpuState::render()` 当前实现，添加 TODO 标记

**设计要点**:
1. **粗粒度命令**: `PresentFrame { frame_id }`
2. **frame_id 管理**: AppCore（业务逻辑）
3. **错误处理**: 完全保留原有 panic 逻辑
4. **drain_view_ops**: 显式在 AppCore::render() 中调用

**编译状态**:
```bash
cargo check --workspace
# Finished ✓
# 7 warnings (GpuRuntime 字段暂未使用 - 预期)
```

**测试状态**:
```bash
cargo test -p renderer --lib
# 47 passed ✓
```

#### 下一步计划

**Step 4B**: 迁移 `enqueue_brush_render_command()` 路径
- 添加 `EnqueueBrushCommands` 命令实现
- 实现 `AppCore::enqueue_brush_render_command()`
- 委托 `GpuState::enqueue_brush_render_command()`

**Step 4C**: 迁移 `resize()` 路径
- 实现 `RuntimeCommand::Resize`
- 实现 `AppCore::resize()`
- 委托 `GpuState::resize()`

**Step 5**: 完全委托
- 修改 `GpuState` 构造函数创建 `AppCore`
- 所有方法委托给 `AppCore`
- 移除直接字段访问

---

本方案用于指导多轮实施，不要求单轮完成全部内容。每轮结束后应更新本文件对应阶段状态和剩余风险。
