c572a00 — Phase 2 Step 1-3：引入 AppCore + GpuRuntime 骨架

你做了什么（结构方向）

新增 app_core/ 与 runtime/，并把“业务逻辑 vs GPU 资源”作为模块边界（AppCore 不直接持有 GPU 资源，靠 runtime command 通信）。commit message 也明确“先搭架子，不迁移方法”。

AppCore 里开始集中：document、tile_merge_engine、brush_buffer_tile_keys 等业务态。

GpuRuntime 里集中：renderer、view_sender、atlas_store、brush_buffer_store、surface_size 等 GPU/渲染态，并提供 execute() 命令入口。

lib.rs 里仍保留旧的 GpuState 作为临时 facade，注释标明 Phase 2 会 split。

做得好的点

**“先平移接口形状，再迁移实现”**是正确的重构节奏：先让新架构可编译、可接线，然后再逐条迁路径。

GpuRuntime::execute() 这种“粗粒度命令 + receipt”的桥接方式，很适合你目前“单线程主线程同步执行”的阶段：迁移成本低、可逐步替换。

我建议尽早补的护栏（否则后续会越迁越痛）

分层泄漏风险：runtime 暴露 renderer 可变引用
你后续在 AppCore::render() 里用到了 runtime.renderer_mut().drain_view_ops()（后续 commit 里出现），这说明 runtime 还在对上层泄漏内部对象。建议尽早把这类操作变成 runtime 的显式命令/方法（例如 RuntimeCommand::DrainViewOps 或 GpuRuntime::drain_view_ops()），避免 AppCore “绕开命令接口”。（这个点在 a4684f0 更明显）

共享资源双持有的契约要写清楚
你现在的设计是：AppCore 也持有 atlas store / brush buffer registry 的 Arc（用于 merge 逻辑），同时 GpuRuntime 也持有对应 Arc（用于 GPU 更新）。这没问题，但要写清楚哪些操作必须在 runtime 执行、哪些可在 AppCore 执行、以及“读写时序”约束（尤其是 tile 分配/回收与 GPU drain 的先后）。否则之后引入多线程时会踩一致性坑。

a4684f0 — Step 4A：把 render/present 路径接到 runtime command

你做了什么

AppCore::render() 里通过 RuntimeCommand::PresentFrame { frame_id } 调 runtime，并匹配 RuntimeReceipt::FramePresented。

做得好的点

这是一次“路径级迁移”的正确示范：上层开始只关心“我要 present 一帧”，把具体 GPU 工作放进 runtime。

主要问题：错误处理策略现在会让重构期调试更难

你在 render 路径里对“意外 receipt / 意外 error”使用了 panic!。
重构期这样做短期省事，但会带来两个长期问题：

上层无法区分“可恢复错误 vs 逻辑 bug”（例如 TileDrain 失败现在直接 panic）；

当你把更多路径迁入 runtime 后，panic 位置会越来越“远离真实原因”。

建议改法（保持你现在的架构，不增加太多复杂度）

把 GpuRuntime::execute(PresentFrame) 的返回类型直接设计成 Result<(), PresentError> 或 Result<FramePresentedReceipt, PresentError>，避免上层再 match receipt。

如果你坚持“统一 execute 返回 receipt”，那至少：

AppCore::render() 不要 panic “unexpected receipt”，而是 Err(wgpu::SurfaceError::Lost) 之类不合适；更好的做法是引入一个 AppCoreError::UnexpectedReceipt 并向上传递（或 log + debug_assert）。

renderer::PresentError::TileDrain 这种强逻辑错误可以用 debug_assert! + 传递一个 RuntimeError::InvariantViolation，不要 panic 在业务层。

5109431 — 修测试缺失 import（修复型提交）

你做了什么

只是在 tests 里补齐 RenderDataResolver / FrameState / DirtyStateStore / FrameSync / LayerDirtyVersion 等 import，没有改测试逻辑。

建议

这种提交很干净；如果你后续会频繁做“重构导致 tests 编译不过”，建议把“修编译”与“迁移逻辑”继续保持分离（你现在就是这么做的）。

8c55eb4 — 更新重构文档（Phase 2 进度）

你做了什么

文档记录 Phase 2 的拆分、Step 1-3 完成、Step 4A 状态、设计决策与 next steps。

建议

很有价值。建议再加一张“迁移清单表”：每条路径（render/resize/brush/merge/gc 等）目前处于 Old(GpuState) / New(AppCore+Runtime) / Hybrid 的哪一档，并写明“最后要删掉的临时代码点”。这能显著降低你自己未来回看时的认知成本。

bd3f875 — Step 4B：resize 路径迁移到 runtime command（并把 view_transform 作为参数）

你做了什么

RuntimeCommand::Resize 增加 view_transform: &ViewTransform；AppCore::resize() 调用 .execute(RuntimeCommand::Resize { width, height, view_transform: &self.view_transform })。

commit message 说明：为 runtime 访问导出 push_view_state() 为 pub(crate)，并保留 GpuState::resize() fallback。

做得好的点

resize 是“高频但语义清晰”的路径，很适合作为第二条迁移示例。

我比较担心的点（接口形状）

命令里携带引用（&ViewTransform）会把 lifetime 传染到整个 command 系统
你现在的 RuntimeCommand 明显已经变成带 lifetime 的 enum（在 9e7c69e 里能看到 RuntimeCommand<'a>），这会让后续所有 command/receipt 的组合复杂度上升。

如果 runtime 在 execute() 内同步使用这个引用然后立刻返回，那技术上是可行的；但它会迫使你未来更多命令也走“借用输入”的风格，最终容易演化成“到处都是泛型 lifetime”。

resize 里 unwrap_or_else(panic!)
现在 AppCore::resize() 直接 panic。
这会让窗口 resize 这种“外部输入触发”的路径变得脆弱（尤其是在 wgpu surface 重建/丢失的边界情况下）。

建议改法（尽量小改）

把 view_transform 变成 runtime 内部状态（runtime 提供 set_view_transform(ViewTransform) 或 RuntimeCommand::SetViewTransform { .. }），Resize 不再携带引用。

AppCore::resize() 返回 Result<(), RuntimeError/AppCoreError>，至少把 panic 移到顶层应用层，而不是 core 层。

9e7c69e — Step 4C：brush 路径部分迁移（新增单条 enqueue 命令）

你做了什么

在 protocol 中新增 EnqueueBrushCommand { command: &'a BrushRenderCommand }，并新增 receipt BrushCommandEnqueued。

runtime 侧处理该命令：enqueue_brush_render_command(command.clone())。

AppCore 在 brush 路径里开始通过 runtime 绑定 tiles、enqueue brush command；merge 相关仍有 TODO 保留未迁移段。

增加 From<RuntimeError> for BrushRenderEnqueueError，但对非 BrushEnqueueError 的情况直接 panic。

做得好的点

你把 brush 的“GPU enqueue”先抽出来迁走，而把“merge orchestration（业务）”留在 AppCore，这个切割方向是对的：GPU enqueue 属于 runtime；merge 的决策/调度属于 core。

关键问题（建议优先修）

RuntimeCommand<'a> + &BrushRenderCommand 但 runtime 又 clone()
这意味着：你承受了 lifetime 复杂度，但并没有真正避免拷贝（因为最终还是 clone）。
更合理的两种选择：

要么命令就直接拥有数据：EnqueueBrushCommand { command: BrushRenderCommand }；

要么用 Arc<BrushRenderCommand> / Cow（如果你真的想减少 clone 成本）。

错误转换里 panic!("unexpected runtime error...")
这会把“新增命令后 runtime 未来扩展的错误类型”变成潜在崩溃点。
建议：让 BrushRenderEnqueueError 能表达 “RuntimeError::Other(...)” 或至少包一层 BrushRenderEnqueueError::Runtime(RuntimeError)，不要 panic。

brush 路径里存在重复/兜底分支看起来像“无差别 enqueue”
你现在的 match 结构里有 _ => { self.runtime.execute(EnqueueBrushCommand { ... }) } 这种“其他命令也 enqueue brush”的兜底分支（从 diff 片段看确实如此）。
这在重构期容易作为临时 glue，但建议尽快把匹配写成穷尽式：哪些 brush command 支持，哪些明确 unimplemented!/todo!/return Err，避免未来加入新分支时“悄悄走错路径”。

总体评价与下一步优先级（按“阻止技术债扩散”排序）

尽快把 RuntimeCommand 的借用/lifetime 从公共接口里拿掉（Resize/Brush 都已经把 lifetime 传染开了）。现在改成本最低。

把 core 层的 panic 收口：

render/resize/brush 这些“外部输入触发”的路径尽量 Result；

真的不可恢复的不变量用 debug_assert! + 结构化错误上抛。

减少分层泄漏：避免 AppCore 直接拿到 renderer 可变引用；让 runtime 提供明确动作接口。

把“共享 Arc 资源”的一致性约束写进文档（你已经开始维护 refactor guide 了，很适合把契约补齐）。
