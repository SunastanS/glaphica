
我按你更新后的“Phase 2 审查意见响应”逐条对照 **GitHub 上已 push 的内容**做了复核（重点看了 `223d8d0`、`88dc371` 以及相关 runtime/protocol 代码）。结论是：**你文档里有一部分状态与代码现状不一致**，需要把“完成/部分完成”的表格修正一下；同时你新增的 `AppCoreError` 方向整体可行，但建议你调整几处设计细节，让 Phase 2 的后续迁移更顺滑。

---

## 1) ✅ RuntimeCommand lifetime：确实已完成（并且是高质量落地）

你在 `223d8d0` 里把 **command 持有数据**写进了 protocol 设计注释，并把 `Resize/EnqueueBrush*` 彻底改成 owned data，确实解决了 lifetime 传播问题。([GitHub][1])
这块可以在“总体进展”里明确标记为 **Closed**。

---

## 2) ⚠️ 你文档里写“GpuRuntime 分层泄漏 ✅ 完成（添加 drain_view_ops 方法）”——我在代码里没看到

在 `223d8d0` 的 `crates/glaphica/src/runtime/mod.rs` 里：

* 仍然存在 `pub fn renderer_mut(&mut self) -> &mut Renderer`，并且注释是“Use with caution - prefer command interface”。([GitHub][2])
* **没有看到**你文档里提到的 `GpuRuntime::drain_view_ops()` 显式方法；现在 drain 是在 `execute(PresentFrame)` 内直接 `self.renderer.drain_view_ops()`。([GitHub][2])

所以这一项目前应当在文档里标成：

* **“仍有泄漏点：renderer_mut 仍是 pub”**（至少不应算完成）
* 如果你的确另外有一个 commit 添加了 `drain_view_ops()`，请把 commit hash 写进文档；否则就是“文档超前于代码”。

**我建议的最小修复（10 分钟级别）**

* 把 `renderer_mut` 降级为 `pub(crate)` 或删除（如果已无调用点）。
* 补一个显式 wrapper：`pub fn drain_view_ops(&mut self)`，给 AppCore 用，不要直接拿 renderer mut。

---

## 3) ⚠️ 你文档里写“brush 错误转换 ✅ 部分完成（添加 debug_assert 防护）”——我在 protocol 里仍看到 panic

在 `223d8d0` 的 `runtime/protocol.rs` 里，仍存在：

* `impl From<RuntimeError> for renderer::BrushRenderEnqueueError { ... other => panic!(...) }`
* 以及 `MergeSubmitError / MergePollError` 同样的 `panic!` downcast ([GitHub][1])

也就是说：**不仅 brush 没消掉 panic，merge submit/poll 也同样存在。**
这项在“总体进展”里应该是：**未完成**（或者“仍存在风险：panic downcast 三处未处理”），不适合标为“部分完成”。

**我建议的更稳的解法（比加 `Runtime(RuntimeError)` 更干净）**
不要再做 `From<RuntimeError> for X` 这种“会被误用成无条件 downcast”的 impl。改成显式 helper：

```rust
impl RuntimeError {
    pub fn into_brush_enqueue(self) -> Result<renderer::BrushRenderEnqueueError, RuntimeError> { ... }
    pub fn into_merge_submit(self) -> Result<renderer::MergeSubmitError, RuntimeError> { ... }
    pub fn into_merge_poll(self) -> Result<renderer::MergePollError, RuntimeError> { ... }
}
```

调用点自己决定：上抛 / log + debug_assert / 转 AppCoreError。这样 **不会把未来新增 RuntimeError variant 变成隐藏地雷**。

---

## 4) ✅ AppCoreError：你说 “Phase 1 完成（提交 88dc371）”——这点属实，但我建议你改两处字段设计

`88dc371` 里确实新增了 `AppCoreError`，并把错误分成 LogicBug / Recoverable / Unrecoverable 三类，整体方向 OK。([GitHub][3])

但我建议你对两个 variant 立刻调整（否则 Phase 2 方法迁移时会后悔）：

### A) `UnexpectedReceipt` 现在记录的是 `received_receipt: &'static str`

这会导致你排查问题时信息不足。建议改成：

* `received: RuntimeReceipt`（或至少 `received_kind: RuntimeReceiptKind` + 可选 debug payload）

因为 receipt 往往携带关键字段（比如 submission ids / notices 数量），光一个静态字符串不够。

### B) `UnexpectedErrorVariant { error: String }`

这个有点像“把类型系统退化成字符串”。更好的是：

* `error: RuntimeError`（或 `Box<dyn Error + Send + Sync>`，但建议优先 RuntimeError）

否则你后续要么丢掉上下文，要么到处 `.to_string()`。

---

## 5) 你文档中的“共享 Arc 资源时序约束”建议改写（避免误导）

你草案里写的：

> “GPU drain 必须在 tile 释放之后”

这句话非常容易把实现引向“先 free 再 drain”的方向，从逻辑上更危险。更稳妥的契约表述应当是：

* **TileKey/slot 的生命周期必须覆盖所有可能引用它的 renderer/op 被消费完成之前**
* 如果要提前回收 slot，必须依赖 **generation/epoch** 防止 ABA

这点建议你尽早更新进 `tiles_model_runtime_refactor_guide.md`，否则未来并发化时会踩坑。

---

# 对你三个开放问题的答复

### 1) AppCoreError 分类是否合理？

合理。三层分类能帮助你在迁移期明确“该不该 panic”。但记得：**LogicBug 不等于“可以 panic”，更推荐 debug_assert + 返回错误**（你设计里也写了这点，保持一致就行）。([GitHub][3])

### 2) 迁移优先级是否合适？

`resize -> render -> 其他` 这个顺序可以。
但我会把 **“消灭 protocol 里的 panic downcast（brush/merge submit/merge poll）”** 提到更前面，因为它是“未来新增错误 variant 就可能炸”的结构性隐患。([GitHub][1])

### 3) 是否有更好的错误处理方式？

对你现在的结构（AppCore + Runtime 同线程同步执行），最实用的是：

* Runtime：`Result<RuntimeReceipt, RuntimeError>`
* AppCore：`Result<_, AppCoreError>`（`From<RuntimeError>` 即可，不要 downcast panic）
* 顶层 event loop：根据 `AppCoreError` 的类别决定恢复策略（Surface Lost/Outdated → resize/reconfigure；OutOfMemory/PresentFatal → fatal）

---

# 建议你把“总体进展表”修正为下面这样（基于我看到的代码）

* ✅ RuntimeCommand lifetime：完成
* 🟡 AppCore panic：Phase 1（类型定义）完成，方法迁移未做（符合你描述）
* ❌ GpuRuntime 分层泄漏：**未完成**（`renderer_mut` 仍 pub；未见 `drain_view_ops` wrapper）([GitHub][2])
* ❌ brush 错误转换：**未完成**（仍有 panic downcast，且 merge submit/poll 同样）([GitHub][1])
* 文档项（契约/清单表）：我这次没有直接打开对应 doc 文件内容核对（你没给具体链接/commit），如果你把文件路径对应的 commit hash/链接贴出来，我可以继续逐段 review。

---

如果你希望我给“最小补丁计划”，我建议拆成 2 个很小的 commit：

1. **protocol 去 panic downcast**（三处 `impl From<RuntimeError> for X` 全删，替换成显式 helper）
2. **runtime 收口 renderer_mut**（降级可见性 + 增加必要 wrapper，例如 `drain_view_ops()`）

这两步都不需要你先把 AppCore 方法签名改成 Result，但能立刻把“隐性炸点”和“边界腐蚀”止住。

[1]: https://raw.githubusercontent.com/SunastanS/glaphica/223d8d0/crates/glaphica/src/runtime/protocol.rs "raw.githubusercontent.com"
[2]: https://raw.githubusercontent.com/SunastanS/glaphica/223d8d0/crates/glaphica/src/runtime/mod.rs "raw.githubusercontent.com"
[3]: https://github.com/SunastanS/glaphica/commit/88dc371 "refactor: Add AppCoreError unified error type (Phase 2 error handling… · SunastanS/glaphica@88dc371 · GitHub"
