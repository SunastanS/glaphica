---
phase: 04-03-appcore-migration
plan: 01-revised
type: execute
wave: 1
depends_on: []
files_modified:
  - crates/glaphica/src/engine_core.rs
  - crates/glaphica/src/lib.rs
autonomous: true
gap_closure: true
requirements: [ARCH-01, ARCH-02]

must_haves:
  truths:
    - "EngineCore contains all business logic from AppCore"
    - "GpuState in threaded mode does not hold AppCore"
    - "Business operations flow through RuntimeCommand channel"
  artifacts:
    - path: "crates/glaphica/src/engine_core.rs"
      provides: "Complete business logic on engine thread"
      contains: "Document, TileMergeEngine, BrushBufferTileRegistry"
    - path: "crates/glaphica/src/lib.rs"
      provides: "Lightweight GpuState with only GpuRuntime + EngineBridge"
  key_links:
    - from: "GpuState::into_threaded()"
      to: "engine_loop()"
      via: "EngineThreadChannels"
      pattern: "spawn_engine"
---

<objective>
重构架构：将业务逻辑迁移到引擎线程

Purpose: 主线程只保留轻量的 GPU 执行层，业务核心在引擎线程通过通道通信
Output: EngineCore 成为完整的业务逻辑持有者，GpuState 变为轻量封装
</objective>

<execution_context>
@/home/sunastans/.config/opencode/get-shit-done/workflows/execute-plan.md
@/home/sunastans/.config/opencode/get-shit-done/templates/summary.md
</execution_context>

<context>
@.planning/PROJECT.md
@.planning/ROADMAP.md
@.planning/STATE.md

## 架构决策

用户确认：业务逻辑应在引擎线程，主线程仅保留轻量数据结构。

### 当前状态
```
GpuState (主线程)
├── core: AppCore        ← 业务逻辑 (错误位置)
└── exec_mode: GpuExecMode
    ├── SingleThread { runtime }
    └── Threaded { bridge }
```

### 目标状态
```
GpuState (主线程)               EngineCore (引擎线程)
├── GpuRuntime             →    ├── Document
├── EngineBridge           ←    ├── TileMergeEngine
└── (无业务逻辑)                ├── BrushBufferTileRegistry
                               └── ViewTransform
```

## 关键文件

- `crates/glaphica/src/engine_core.rs` - 引擎线程业务核心
- `crates/glaphica/src/lib.rs` - GpuState 定义
- `crates/glaphica/src/app_core/mod.rs` - 当前业务逻辑 (将合并到 EngineCore)
</context>

<tasks>

<task type="auto">
  <name>Task 1: 扩展 EngineCore 以包含完整的业务逻辑</name>
  <files>crates/glaphica/src/engine_core.rs</files>
  <action>
将 AppCore 的字段和方法迁移到 EngineCore：

1. 添加 AppCore 中缺失的字段到 EngineCore：
```rust
pub struct EngineCore {
    // 从 AppCore 迁移
    pub document: Document,  // 已有
    pub tile_merge_engine: TileMergeEngine<MergeStores>,  // 已有
    pub brush_buffer_tile_keys: BrushBufferTileRegistry,  // 已有
    pub view_transform: ViewTransform,  // 已有
    
    // 需要添加
    pub atlas_store: Arc<TileAtlasStore>,
    pub brush_buffer_store: Arc<GenericR32FloatTileAtlasStore>,
    pub last_bound_render_tree: Option<(u64, u64)>,
    pub disable_merge_for_debug: bool,
    pub perf_log_enabled: bool,
    pub brush_trace_enabled: bool,
    pub next_frame_id: u64,
}
```

2. 添加从 AppCore 迁移的方法：
   - `process_renderer_merge_completions()`
   - `set_preview_buffer()` / `clear_preview_buffer()`
   - `drain_tile_gc_evictions()`

3. 保留现有的 `process_input_sample()` 和 `process_feedback()` 逻辑。
</action>
  <verify>
    <automated>cargo check -p glaphica --features true_threading</automated>
  </verify>
  <done>EngineCore 包含所有业务逻辑字段和方法，编译通过</done>
</task>

<task type="auto">
  <name>Task 2: 重构 GpuState 为轻量封装</name>
  <files>crates/glaphica/src/lib.rs</files>
  <action>
重构 GpuState 以移除对 AppCore 的依赖（在线程模式下）：

1. 修改 GpuState 结构：
```rust
pub struct GpuState {
    // 单线程模式：保留 AppCore
    #[cfg(not(feature = "true_threading"))]
    core: AppCore,
    
    // 通用字段
    exec_mode: GpuExecMode,
}

pub enum GpuExecMode {
    SingleThread { 
        #[cfg(not(feature = "true_threading"))]
        runtime: GpuRuntime,
        #[cfg(feature = "true_threading")]
        runtime: GpuRuntime,
        // 在 true_threading 模式下，AppCore 不在这里
    },
    Threaded { bridge: EngineBridge },
}
```

2. 修改 `into_threaded()` 方法：
   - 从单线程模式转换时，将 AppCore 的状态移动到 EngineCore
   - 创建 EngineCore 并传递给 engine_loop

3. 在线程模式下，GpuState 只持有 EngineBridge，不持有 AppCore。
</action>
  <verify>
    <automated>cargo check -p glaphica --features true_threading && cargo check -p glaphica</automated>
  </verify>
  <done>GpuState 在线程模式下不持有 AppCore，编译通过</done>
</task>

<task type="auto">
  <name>Task 3: 实现状态迁移逻辑</name>
  <files>crates/glaphica/src/lib.rs, crates/glaphica/src/engine_core.rs</files>
  <action>
实现从 AppCore 到 EngineCore 的状态迁移：

1. 在 EngineCore 中添加 `from_app_core()` 方法：
```rust
impl EngineCore {
    pub fn from_app_core(
        app_core: AppCore,
        channels: EngineThreadChannels<RuntimeCommand, RuntimeReceipt, RuntimeError>,
    ) -> Self {
        Self {
            document: app_core.document_into_inner(),
            tile_merge_engine: app_core.tile_merge_engine_into_inner(),
            // ... 其他字段
        }
    }
}
```

2. 在 GpuState::into_threaded() 中调用此方法：
```rust
pub fn into_threaded<F>(self, spawn_engine: F) -> Self
where
    F: FnOnce(EngineThreadChannels<...>, EngineCore) -> JoinHandle<()>,
{
    // 将 self.core (AppCore) 转换为 EngineCore
    let engine_core = EngineCore::from_app_core(self.core, engine_channels);
    
    // 传递给引擎线程
    let engine_thread = spawn_engine(engine_channels, engine_core);
    
    // ...
}
```
</action>
  <verify>
    <automated>cargo check -p glaphica --features true_threading</automated>
  </verify>
  <done>状态迁移逻辑实现，AppCore 可以转换为 EngineCore</done>
</task>

</tasks>

<verification>
1. `cargo check -p glaphica` 通过（单线程模式）
2. `cargo check -p glaphica --features true_threading` 通过（线程模式）
3. EngineCore 包含所有业务逻辑字段
4. GpuState 在线程模式下不持有 AppCore
</verification>

<success_criteria>
1. EngineCore 成为完整的业务逻辑持有者
2. GpuState 在线程模式下只持有 EngineBridge + GpuRuntime
3. 业务操作通过 RuntimeCommand 通道发送
4. 代码编译通过，两种模式都能工作
</success_criteria>

<output>
After completion, create `.planning/phases/04-03-appcore-migration/04-03-01-SUMMARY.md`
</output>