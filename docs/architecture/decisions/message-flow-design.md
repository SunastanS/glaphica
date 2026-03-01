# Merge 消息传递与回执机制设计决策（2026-02-21）

本文档记录 merge 回执链路中与消息传递机制相关的关键决策，作为后续实现和评审基线。

## 背景

- merge 执行是异步行为（GPU 提交后跨帧完成），同步函数返回值无法直接承载最终结果。
- 现有分层职责：
  - `document` 持有业务事务语义（key 绑定更新、失败处理）。
  - `tiles` 持有 tile 化与 merge 事务聚合语义。
  - `renderer` 持有 GPU 提交、完成轮询、回执状态机。
- 已确认 bug：`poll` 自动 ack 与手动 ack 并存会导致重复推进和非法状态崩溃。

## 决策 1：回执推进采用单通路（Ack 是唯一状态推进入口）

### 结论

- `ack_merge_result` 是唯一允许将 receipt 从 `Pending` 推进到 `Succeeded/Failed` 的入口。
- `poll_submission_results` 只负责输出 completion notice，不隐式推进 receipt 业务状态。

### 理由

- 防止“双写通路”造成的重复 ack、非法状态与 panic。
- 使状态机行为可审计、可测试、可复现。
- 将“观察完成”和“确认入账”职责分离，降低耦合。

## 决策 2：消息流采用“下行命令 + 上行分层返回”

### 结论

- 下行：`A -> B -> C`（`brush_execution/document -> tiles -> renderer`）。
- 上行：`C -> B -> A`（`renderer completion -> tiles ack/聚合 -> document 业务处理`）。
- 不引入 `renderer -> document` 直连，避免抽象泄露。

### 理由

- `renderer` 缺少 tiles/document 业务上下文，不应承担上层事务解释。
- `tiles` 作为中间层最适合完成 completion 到业务语义的映射与聚合。
- 保持分层对称性，便于长期维护和定位问题。

## 决策 3：帧时序只在运行时单点接入，不向上层扩散

### 结论

- scheduler 信号由 renderer 侧（或其邻近 coordinator）消费。
- `tiles` 与 `document` 不直接依赖 scheduler 实体。

### 理由

- 避免时序依赖扩散为全链路耦合。
- 将“何时消费 completion”限定在单点时序入口，减少并发复杂度。
- 上层只处理事件结果，不处理帧驱动细节。

## 决策 4：回传机制允许存在，但需限制为单向阶段推进

### 结论

- 允许分层上行回传（`renderer -> tiles -> document`）。
- 禁止在上行处理同一栈帧内触发新的下行提交（禁止循环重入）。

### 理由

- 防止“循环调用 + 多向通道”导致拓扑失控。
- 保证一帧内语义清晰：先收敛 completion，再统一结算。

## 决策 5：通道模型不是目标，语义边界才是目标

### 结论

- 不强制某种 IPC/回调技术实现（回调、队列、事件都可）。
- 强制语义约束：
  1. completion 先产出
  2. tiles 统一 ack
  3. 再向 document 输出业务结果

### 理由

- 技术形态可替换，语义契约不可漂移。
- 避免“为通道而通道”的过度设计。

## 约束与不变量

- `poll_submission_results` 不得隐式调用 `ack_merge_result`。
- 重复 ack 仍然 fail-fast（返回错误，不做 silent ignore）。
- renderer 文档仅定义 renderer 与 tiles 之间的接口契约，不描述 tiles/document 内部传递细节。
- 同一 receipt 的业务终态（`Finalized/Aborted`）由上层按策略显式确认。
