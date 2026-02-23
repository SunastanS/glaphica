# Debug Playbook (Renderer / wgpu / Render Tree)

这份文档把近期关于 brush merge、live preview、render tree、tile atlas 的排查经验沉淀成一套可重复的流程与工程规则，目的是让后续类似问题不需要“重走一遍对话”。

本项目已有更细的复盘记录：
- `docs/debug/brush_merge_duplicate_tiles_2026-02-23.md`
- `docs/Instructions/wgpu.md`

## 0. 第一原则：先增强可观测性

- 先把“肉眼信号强度”调大再排查：例如默认笔刷不要是 1px，否则对 tile 边界/GPU 污染/坐标偏差不敏感。
- 日志必须可开关，默认关闭；只在需要时打开（避免热路径 IO 把问题放大成卡顿）。
- 任何推断都要尽快变成“可证伪断言”，并放在数据流边界上。

## 1. 推荐排查顺序（从高概率到低概率）

当出现“tile 拼贴、重复、错位、断续、灰底卡死”等复杂现象时，优先按这个顺序收敛：

1. 输入坐标契约是否正确：driver 到底接收 `screen` 还是 `canvas`？
2. render tree revision/语义是否一致：语义变了是否 bump revision？缓存 key 是否稳定？
3. 赃区是否正确：是否把“每帧新笔迹影响的 tiles”标记成 dirty，并驱动局部 composite？
4. wgpu CPU->GPU 写入语义是否被误用：同一 submit 内多次覆写同一 buffer（uniform/storage/instance）？
5. tile atlas 写入边界：dab footprint 是否跨 slot/tile 边界？gutter 读写是否一致？

## 2. 常用开关（按需打开）

这些开关用于快速把现象定位到“输入/渲染树/缓存/merge/GPU 提交”其中一层：

- `GLAPHICA_BRUSH_TRACE=1`: 指针事件 `screen -> canvas` 打印、brush/merge 提交流水。
- `GLAPHICA_RENDER_TREE_TRACE=1`: render tree bind 原因、revision、semantic hash。
- `GLAPHICA_RENDER_TREE_INVARIANTS=1`: 强约束断言，render tree 语义变化必须伴随 revision bump。
- `GLAPHICA_PERF_LOG=1`: renderer/document/merge 的 perf 日志（例如 dirty poll、cache rebuild、merge submit/poll）。
- `GLAPHICA_FRAME_SCHEDULER_TRACE=1`: 帧调度器是否进入 brush 热路径、tick/activate/deactivate 频率。

工程规则：
- 热路径日志必须受环境变量门控；默认不打印。
- perf 日志出现时，优先读“dirty 层面”和“cache rebuild/copy 层面”，而不是先读 draw call 数。

## 3. Render Tree：revision 与语义的约束

典型错误表现：
- “点击/绘制后突然全量重绘、卡死、灰底、过一会又恢复”。
- `bind render tree: revision=...` 出现不符合预期的频率，或者同 revision 被反复 bind。
- 开启 invariants 后直接 panic：`render tree semantics changed without revision bump`。

验证方式：
- 打开 `GLAPHICA_RENDER_TREE_TRACE=1 GLAPHICA_RENDER_TREE_INVARIANTS=1`。
- 关注每次 bind 的 `reason/revision/semantic_hash`，确认：
- 语义变化时 revision 必须递增。
- 语义不变时，不应频繁触发“强制 composite dirty”。

修复要点（经验总结）：
- “live preview set/clear”通常只改变 active layer 的显示结构，但不应误触发全树重建。
- render tree 的结构尽量稳定：例如 layer 始终表现为一个 group，预览时只是给 group 增加一个 leaf，而不是把 leaf 变成 group 或反之。
- 如果某些 leaf（例如 brush buffer）不应影响“是否需要强制 document composite dirty”的语义比较，则在语义比较里明确忽略它，而不是靠“碰巧 hash 一样”。

## 4. 赃区模型：必须以 tile 为单位可查询

目标行为：
- 笔刷绘制时只重绘“本帧新增笔迹影响到的 tiles”，而不是重绘整个可见区域，更不是重绘整个文档。

验证方式：
- 打开 `GLAPHICA_PERF_LOG=1`，观察：
- `layer_dirty_poll ... result`
- `frame_plan ... dirty_leaf_partial_tiles/dirty_group_partial_tiles`
- `group_cache_update ... mode=partial copied_tiles=...`
- 正确情况下，brush 热路径每帧应该产生小范围的 partial 更新。

经验规则：
- buffer 和 layer 在 document 视角下应共享同一套“version + dirty_since(version)”语义，才能复用现有 dirty 传播与 group cache 逻辑。
- 不要在渲染侧把“可见区域”当作“赃区”；可见只是裁剪，赃区决定是否需要更新缓存内容。

## 5. wgpu：最容易踩的 CPU->GPU 时序坑

这类问题的共同点：
- 现象看起来随机，且与绘制速度/输入节奏不严格相关。
- 改动 render tree 深度、增加 pass、合并 encoder 后突然出现。

必须记住的语义：
- `queue.write_buffer` 并不会“立刻让某个 pass 看见那次写入”，它只保证在**同一次 submit 的 GPU 执行前**按顺序完成写入。
- 在一个 `CommandEncoder` 里编码多个 pass，然后只 submit 一次时，如果你在 submit 前多次覆写同一段 buffer，多个 pass 很可能都会读到“最后一次写入”的内容。

典型症状、验证与修复见：
- `docs/Instructions/wgpu.md`

工程规则（强制）：
- 禁止：单次 submit 内，多次 `queue.write_buffer(buffer, offset=0, ...)` 覆盖同一段内存，同时 encoder 内存在多个读取该 buffer 的 pass。
- 若必须多 pass 且每 pass 参数不同：
- 用 dynamic offset 或“arena/append-only”分配不同 offset。
- 或者先用“每 pass 单独 submit”的止血方案保证正确性，再做批处理优化。

## 6. Tile Atlas：边界、slot 与 gutter

典型错误表现：
- tile 边界处出现 1px 污染，且频繁但不稳定。
- assertion 抓到 dab footprint 跨越 slot boundary。

验证方式（fail-fast）：
- 在 CPU 侧建立 dab footprint 断言：
- 断言 dab 的写入 bounds 不跨出目标 slot（包含 gutter 的 slot bounds）。
- 若未来要支持“跨 slot 写入”：
- 必须先在 CPU 侧把一个 dab 拆成多 tile 的子写入，每个 tile 独立命令，保证任何单次 GPU 写入不会越界污染相邻 tile。

实现策略建议（按可控性排序）：
1. CPU 拆分为 per-tile 命令（最小实现，便于 debug）。
2. GPU 侧实现“写入某区域”的原子语义：
 - 通过 bounds clamp + 只写入合法坐标，严格避免越界写。
3. 性能优化在正确性稳定后再做（合批、批量 uniform、减少 submit）。

## 7. 最小复现实验：把“看起来随机”变成可断言测试

当你怀疑 tile 坐标映射/缓存污染时，优先写最小回归测试，而不是继续观察 UI：

- `256x256` 图像，拆成 `2x2` tiles，每块不同纯色。
- 走完整 composite 流程，readback 后逐像素断言输出一致。
- 再加一个“嵌套 group cache”版本，使用稀疏 tile_index，确保不会被顺序偶然掩盖。

注意事项：
- wgpu/driver 在并行测试下更容易不稳定，建议用 `--test-threads=1` 跑 GPU 测试。

## 8. 一句经验总结（便于快速决策）

- “内容重复但 key/address 不重复”：优先查 GPU 提交语义或缓存复用，而不是地址解析。
- “受影响 tile 与光标不一致”：优先查 `screen -> canvas` 逆变换链路。
- “新增一层 group/preview 后出现拼贴”：优先查同一 submit 内是否覆写了 instance/uniform/storage buffer。
- “卡死/灰底/过一会恢复”：优先查是否触发了热路径的全量 composite/copy，或者把应该局部更新的路径变成了全量 rebuild。
