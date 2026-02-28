# Phase 04-03-01: AppCore Channel Infrastructure

**Status:** ARCHITECTURAL REVISION IN PROGRESS

---

## 架构决策

**用户确认：业务逻辑应在引擎线程，主线程仅保留轻量数据结构**

```
主线程 (轻量)                    引擎线程 (业务核心)
─────────────                    ─────────────────
GpuState                         EngineCore
├── GpuRuntime              →    ├── AppCore (合并后)
├── EngineBridge            ←    │   ├── Document
└── (无业务逻辑)                 │   ├── TileMergeEngine
                                 │   └── BrushBufferTileRegistry
```

---

## 已完成的工作

### Task 1-2: AppCore 添加通道字段 ✓

- 添加了 `gpu_command_sender` 和 `gpu_feedback_receiver` 字段
- 添加了 `new_with_channels()` 构造函数
- 添加了 `has_channels()` 辅助方法

**Commit:** 511796d feat(04-03-01): add channel fields and constructors to AppCore

---

## 需要修订的计划

原计划假设 AppCore 留在主线程并使用通道通信。
现在需要修订为：**将 AppCore 合并到 EngineCore，在引擎线程运行**。

### 修订后的任务

**Task 3 (修订): 合并 AppCore 到 EngineCore**

1. EngineCore 应该包含或继承 AppCore 的所有业务逻辑
2. 主线程的 GpuState 只保留 GpuRuntime + EngineBridge
3. AppCore 的 render()/resize()/enqueue_brush_render_command() 等方法改为通过通道发送命令

---

## 下一步

需要重新规划 Phase 04-03 的 gap closure 计划以匹配这个架构方向。