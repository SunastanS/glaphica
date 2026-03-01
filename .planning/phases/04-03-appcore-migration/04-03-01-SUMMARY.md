# Phase 04-03-01: AppCore Channel Infrastructure

**Status:** IN PROGRESS - Architecture Migration

---

## 架构决策

**用户确认：业务逻辑应在引擎线程，主线程仅保留轻量数据结构**

```
主线程 (轻量)                    引擎线程 (业务核心)
─────────────                    ─────────────────
GpuState                         EngineCore
├── GpuRuntime              →    ├── Document
├── EngineBridge            ←    ├── TileMergeEngine
└── (无业务逻辑)                 ├── BrushBufferTileRegistry
                                 └── ViewTransform
```

---

## 已完成的工作

### Task 1-2: AppCore 添加通道字段 ✓

- 添加了 `gpu_command_sender` 和 `gpu_feedback_receiver` 字段
- 添加了 `new_with_channels()` 构造函数
- 添加了 `has_channels()` 辅助方法

**Commit:** 511796d feat(04-03-01): add channel fields and constructors to AppCore

### Task 3 (修订): EngineCore 扩展 ✓

- EngineCore 添加了完整的业务逻辑字段
- 添加了 `from_app_parts()` 转换方法
- 支持从 AppCore 迁移状态

**Commit:** c2277fe refactor(04-03): extend EngineCore and add AppCore::into_engine_parts()

### Task 4: AppCore 状态迁移方法 ✓

- 添加了 `AppCore::into_engine_parts()` 方法
- 支持将 AppCore 状态解构为 EngineCore 所需的组件
- 使用 destructuring pattern 避免 move 错误

**Commit:** 6ee02d2 refactor(engine): add EngineCore::from_app_parts() for state migration

### Task 5: GpuState::into_threaded() 更新 ✓

- 修改了 `into_threaded()` 签名，接收 EngineCore 参数
- 使用 `AppCore::into_engine_parts()` 进行状态迁移
- 添加了 `placeholder_for_threaded_mode()` 作为过渡方案

**Commit:** 60d4920 refactor(04-03): update GpuState::into_threaded() for engine thread migration

---

## 遗留问题

### GpuState 仍持有 AppCore

当前 `into_threaded()` 调用 `placeholder_for_threaded_mode()` 会 panic，因为：

1. GpuState 结构体仍有 `core: AppCore` 字段
2. 在线程模式下，这个字段不应该存在
3. 很多方法直接访问 `self.core`，需要重构

### 解决方案

需要重构 GpuState：

```rust
pub struct GpuState {
    #[cfg(not(feature = "true_threading"))]
    core: AppCore,
    
    exec_mode: GpuExecMode,
}

// 或者使用 Option:
pub struct GpuState {
    core: Option<AppCore>,  // None in threaded mode
    exec_mode: GpuExecMode,
}
```

---

## 下一步

1. 重构 GpuState 以在线程模式下不持有 AppCore
2. 更新所有访问 `self.core` 的方法以处理两种模式
3. 确保编译通过且功能正确