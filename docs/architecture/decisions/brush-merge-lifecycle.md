# Brush Merge 生命周期决策（2026-02-21）

本文档记录 brush merge 生命周期的当前基线决策，目标是统一实现与评审口径。

## 背景

- 现有系统采用版本化 tile key（COW）与异步 merge 回执链路。
- 目标是同时满足：
  - 主路径低延迟与一致性（merge 结果快速进入 document/layer）。
  - 后续“编辑上一笔”能力所需的可回退空间。

## 决策 1：`merge -> ack -> commit` 作为一笔结束后的正常主路径

### 结论

- 在 `EndStroke` 后立即发出 `MergeBuffer`，不再依赖“下一笔开始”或“空闲再触发”。
- merge completion 后，仍遵循既有单通路约束：
  1. `renderer` 产出 completion notice
  2. `tiles` 统一 ack 推进 receipt 状态
  3. `document` 执行 commit/abort 业务处理

### 理由

- 主路径应该稳定、可预期，不把 merge 时机耦合到用户是否继续下一笔。
- 该路径已具备明确状态机与 fail-fast 约束，不应被“编辑上一笔”需求拖慢。

## 决策 2：仅延后 `buffer_tile release`

### 结论

- `buffer_tile` 不随 merge 成功立即释放，进入延后回收队列（retained window）。
- merge 失败时不保留，立即释放对应 `buffer_tile`。

### 理由

- “编辑上一笔”本质依赖的是：在主路径完成后，仍可回退并访问上一笔相关 buffer 数据。
- 因此延后回收即可提供可回退窗口，无需延后 merge/ack/commit。

## 决策 3：可回退能力与主路径解耦

### 结论

- 当前阶段先不实现“编辑上一笔”具体操作。
- 先固定生命周期契约：主路径即时推进，回收策略独立演进。

### 理由

- 避免把交互功能（编辑上一笔）与底层事务收敛（merge/ack/commit）耦合成同一风险域。
- 后续可在 retained 策略上迭代（时间窗、内存水位、显式 pin/unpin），不破坏主路径。

## 决策 4：接入 tiles GC 淘汰事件，策略为“记录并继续”

### 结论

- `tiles` retain-batch GC 由 `glaphica::GpuState` 在主循环中主动消费：
  - 在 `AllocateBufferTiles` 后消费一次；
  - 在 `process_renderer_merge_completions` 收尾再消费一次。
- 当收到 evicted retain batch：
  - 记录 `retain_id(stroke_session_id)` 的能力状态为 `Evicted`；
  - 更新内部计数与日志；
  - 不中断主路径（不 panic，不回滚当前帧流程）。

### 理由

- GC 淘汰是容量压力下的预期行为，不应把绘制主路径变成失败路径。
- 上层必须消费该事件，否则“历史能力降级”不可观测，后续撤销设计无法闭环。

## 生命周期（当前基线）

1. `StrokeEnded`
2. `MergeSubmitted`（立即）
3. `MergeAckedSucceeded | MergeAckedFailed`
4. `DocumentCommitted | DocumentAborted`
5. `BufferRetained`（仅成功路径）
6. `BufferReleased`

## 历史笔画生命周期（面向撤销/重放设计预留）

当前撤销栈尚未落地，但系统已按 COW + tiles 生命周期控制预留能力边界。历史笔画随着新笔画压入，按资源释放进度可划分为以下状态：

### 1. 完全可重放

- 条件：
  - merge 已完成；
  - origin layer tile 未释放；
  - stroke buffer tile 未释放。
- 能力：
  - 可针对单笔修改参数并重放；
  - 可支持“跳过某一笔再重算后续”等高级编辑/撤销策略。

### 2. 可回退

- 条件：
  - merge 已完成；
  - stroke buffer tile 已释放；
  - origin layer tile 未释放。
- 能力：
  - 仍可通过版本化 key 回退到该笔之前状态；
  - 不能做字面意义上的该笔重放（缺少原始 stroke buffer）。

### 3. 不可回退（纯 GPU 视角）

- 条件：
  - stroke buffer tile 与 origin layer tile 均已释放。
- 能力：
  - 在纯 GPU 路径上无法继续回退该历史状态；
  - 后续可通过“tile 释放前 CPU 读回 + 存储”扩展更长历史撤销能力。

## 不变量

- `ack` 是 receipt 状态推进唯一入口。
- `DocumentCommitted/Aborted` 只能在 `MergeAcked*` 之后发生。
- `BufferReleased` 必须晚于 document 终态确认。
- 回收策略变化不得改变主路径提交与可见性时序。

## 已知实现映射（代码）

- Brush 结束即 merge：`crates/brush_execution/src/lib.rs`
- Completion -> ack -> business result：`crates/glaphica/src/lib.rs`
- Document begin/finalize/abort：`crates/document/src/lib.rs`
