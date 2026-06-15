先说总体印象：分层是清晰且自洽的——`ImageId`（业务层）/ `GlaImageKey`(文档行) / `GlaLocalImageKey`(会话行) / `TileKey`(资源) 的四层身份体系，加上 "Edit shadow + 提交时就地替换 tile slot + 逆向 patch 做 undo" 这条主线，是站得住的。文档与实现的一致性也很好。下面按"正确性问题 → 架构缺口 → 建议路线"展开。

## 一、当前实现中的具体问题

**1. tile 索引换算与 gutter 不一致（实际 bug）**
`input_to_tile_index` 用 `input.x / 64.0`，但 `IMAGE_TILE_SIZE = 62`（64 − 2×gutter）。图像逻辑坐标应按 62px 切 tile，这里会从第二个 tile 起逐渐偏移。同时整个渲染路径目前对 gutter 完全没有维护：clear/copy/composite 都是整 64×64 操作，没有"写完 tile 后把边缘像素复制进 gutter"的 pass。等到做跨 tile 采样（Blur 的 `Expand`、显示时的双线性采样）时 gutter 必须先有正确内容，建议尽早把 "gutter 修复 pass" 定义成 tile 写入后的标准收尾步骤，否则后面所有滤波类 primitive 都会踩到。

**2. 会话命令图没有环检测（潜在死循环）**
`gla_doc` 校验了文档图无环，但会话侧只检查了"derive 不读自己的 current"。两个 session derived 互读（A 读 B、B 读 A）能通过 `collect_write_starts`，随后 `upload_dirty_from` 和 `render_impl` 都会无限递归。建议在 session 初始化第 9 步降级（lowering）之前，对"文档图 + 会话边"的合并图整体做一次拓扑校验，复用 `gla_doc` 里那段 Kahn 算法即可。

**3. 脏传播在 Identity 映射 + 不同 layout 时语义错误**
`upload_dirty_edge` 的 Identity+None 分支直接把 src 的 tile index 过滤后塞给 dst。当两边 `tile_count_x` 不同（例如 128×64 的源传给 64×64 的目标）时，tile index 的线性编号含义完全不同，Identity 指的是像素坐标恒等而不是 tile 编号恒等。`can_upload_dirty_without_projection` 检查了 layout 相等才走快路径，但慢路径同样按 index 过滤，是错的。正确做法是 index → (tx, ty) → 像素矩形 → dst tile 覆盖，这段几何换算和读 footprint 的 `for_each_source_tile` 是同一份数学，建议抽到 `gla_command_core` 里做成一个 `TileFootprint` 模块，render 和 dirty 双向共用（你文档里也说"读边映射是唯一真相源"，那就让代码真的只有一份实现）。

**4. `lower_graph_command` 把所有读都降成 `Copy`，合成语义缺失**
多读命令（如 `RenderRoot(bg, paint_group, lines)`）降级后是连续三个 Copy，每个都是 full-overwrite，结果是只剩最后一个输入。这不只是占位问题，根源是 IR 缺口：`GraphRead` / `SessionRead` 上没有 blend mode 和 opacity。建议在 IR 读边上加合成元数据（首个读 → Copy 或 Clear，后续读 → `RenderTo{mode, opacity}`），或者更彻底一点：把 `GraphCommand` 从 "reads 列表" 升级成小型 op 序列，与 `gla_image_command::Derive` 一一对应，lowering 只做 key 替换不做语义推断。

**5. `key_to_id` 假设 binding 单射**
两个 `ImageId` 绑定同一个 `GlaImageKey` 时（`Document::new` 不禁止），`key_to_id` 只保留先到者，`write_session_tile` 的 Doc 分支会按错误的 id 查角色。要么在 `Document::new` 校验 binding 单射并写进不变量文档，要么让需要 id 的路径直接携带 id 而不是从 key 反查。

**6. 单一 `atlas_id` 与格式不匹配（架构性缺口，建议优先处理）**
`Atlas` 是按 format 类型化的，但 `DrawSession` 全程只持有一个 `atlas_id`：session Raw 行分配、Edit 首写分配、doc derived 失效 tile 修复全部从同一个 atlas 拿 tile。水彩例子里 D1 coverage 和 D4 paint 共存时这立刻不成立。建议引入一个 `AtlasRegistry`：`GlaFormat -> Vec<atlas_id>`，按需创建、按格式分配，并处理"一种格式多个 atlas"的溢出。这是水彩 IR 真正跑起来前的硬前置。

**7. 历史无界增长 + tile 生命周期没有回收策略**
`DrawHistory.patches` 永不清理；undo 后再画新笔（标准的"截断 redo 分支"）会留下一串永远不可达的 record，它们持有的 `TileKey` 也永远不会 `discard`。tile 此时事实上被 live image 和多个历史 record 共享，所以简单 discard 也不安全。建议：把 history 改成线性栈（或显式树）+ 提交时截断；tile 引用计数或者按 record 维度的所有权标记（"该 tile 仅被此 record 持有则随 record 释放"）。这也是后面做 atlas 驱逐（eviction）的前置——驱逐 derived cache tile 时同样要知道没人引用它。

**8. 几个小问题**
`gla_session::CanvasInput` 与 `gla_core::Input<CanvasCoordF>` 重复，建议统一；`render_impl` 对 doc derived 每次现场 `lower_graph_command`（克隆 + 分配），可在 session 初始化时为活动链之外的 derived 也预降级缓存；`SessionError::MissingImage { id: ImageId::new(0) }` 这种占位 id 会让错误日志失去意义，建议加一个 `UnknownDocKey(GlaImageKey)` 变体。

## 二、GPU 渲染路径的架构建议

当前 `RenderTo` 的实现是：dst tile → scratch_a 拷贝、建 uniform buffer、建 bind group、单 pass 画到 scratch_b、再拷回 dst。每个合成 pass 两次纹理拷贝、一次 buffer 创建、一次 bind group 创建，且所有 pass 串行竞争同一对 scratch，GPU 上完全无法并行。这在 64×64 的 tile 粒度下，固定开销会远超实际像素工作量。建议分三步改进：

1. **命令级融合**：`DeriveCommand` 本身是 full-overwrite 语义，天然适合"dst 拷入 scratch 一次 → 在 scratch 上连续执行整条 op 链 → 写回一次"。把 ping-pong 从"每个 op 一次"提升到"每条命令一次"，拷贝次数立刻从 (2N) 降到 2。
2. **资源池化**：uniform 改成一个大 buffer + dynamic offset（或 push constants），bind group 按 (atlas 视图, uniform slice) 缓存复用，scratch 改成小池子让不同 dst tile 的命令可并行。
3. **再往后**，考虑把同一帧内同一 dst atlas 的 pass 排序合批，或对"src 与 dst 不同 atlas"的常见情形直接以 dst tile 为 render target（加 viewport/scissor 到 tile 矩形），跳过 scratch。premultiplied 的 Normal/加法类混合可以直接用固定功能 blend state，只有 Multiply/Overlay 这类需要读 backdrop 的才走 scratch 路径——可以按 blend mode 分流两条管线。

另外 scratch 与 pipeline target 硬编码 `Rgba8Unorm`，F32 atlas 一进来就会在 `copy_texture_to_texture` 处崩。配合上面第 6 点的 AtlasRegistry，composite stage 也应该按 (src format, dst format) 缓存管线变体。

## 三、颜色架构建议

现在的默认解释是 "D4 U8 = LinearSrgb premultiplied"，但**线性值存 8bit 会在暗部产生肉眼可见的 banding**，这是绘画软件的经典坑。三个方向选一个尽早定下来：U8 存储改为 sRGB 编码（wgpu 用 `Rgba8UnormSrgb`，采样/写入自动转换，混合仍在线性域）；或者工作格式上 F16（建议给 `ChannelType` 加 F16，`Rgba16Float` 是绘画 app 的主流甜点位）；或者保持现状但明确接受画质代价。这会影响 atlas 格式集合、composite 管线和 `PixelInterpretation` 的默认值，越晚改动成本越高。

还有一个一致性风险：混合数学现在有 Rust（`gla_color`）和 WGSL 两份手写实现。建议加一个 GPU readback 的 parity 测试：随机颜色对分别走 CPU 函数和 GPU pass，断言误差在 (\le 1/255) 量级，防止两边悄悄漂移。

## 四、建议的下一步路线（按依赖排序）

1. **修正确性**：tile 尺寸 62px 换算、会话图环检测、脏传播的几何投影（与读 footprint 共用一份 `TileFootprint` 实现，顺带把 `Expand`/`Matrix` 的 TODO 一起落地，因为 BlurCoverage 等水彩 primitive 直接依赖它）。
2. **AtlasRegistry（按格式分配）+ gutter 维护 pass**：这是 D1/D4 混合 session 和滤波类 primitive 的硬前置。
3. **IR 合成语义**：给读边加 blend/opacity（或 op 序列化 GraphCommand），让 `RenderPaintGroup` / `MergePixelRound` 这类命令真正可表达；同时实现第一个真实的 `DrawRadialKernel1D` 管线（dab 实例化合批，按 footprint 圆覆盖多 tile，accumulate 混合写 coverage）。
4. **GPU 路径优化**：命令级融合 + uniform/bind group 池化。
5. **资源生命周期**：tile 引用计数（或 record 所有权），history 线性化与截断，然后基于它做 derived cache 的 atlas 驱逐与 `TileKey::INVALID` 回填。
6. **显示链路**：目前 root_image 渲染完就停在 atlas 里，还缺 root tiles → swapchain 的 present pass（带画布变换、跨 tile 双线性采样——又回到 gutter）。

整体来说骨架的不变量设计（单写者、backup 读隔离、derived full-overwrite、commit 原子性）是这套系统最有价值的部分，文档里也都写明了。上面 1–3 做完之后，BrushSystem.md 里的水彩示例应该就能端到端跑通，那会是一个很好的里程碑验证点。
