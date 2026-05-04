# 术语表

<!-- 本文件是项目领域术语的规范定义。当讨论或代码中使用以下术语时，必须以本文的定义为准。 -->

## 空间与资源

### 图集

英文：atlas

GPU 上的纹理数组（texture array），所有分片的物理载体。图集包含多层，每层划分为固定大小的分片槽。系统中的分片生命周期管理围绕图集的容量约束展开。

### 后端

英文：backend

图集内的物理分配器，负责分片槽的分配/释放/缓存/激活。由 `BackendId` 标识，每个后端有自己的 `AtlasLayout`。`Backend` 仅操作物理 `TileKey`，不管理逻辑身份。

后端被 `TileManager` 持有，外部不直接调用其分配 API。不同后端之间分片不可互相引用。

### 笔刷后端

英文：brush backend

专用于笔刷绘制的后端。每个笔刷类型持有一个笔刷后端，跨笔画复用，用于分配笔刷分片。笔刷后端与文档后端分离，确保笔刷的中间数据不会占用文档图像的图集空间。

### 分片

英文：atlas tile / tile

图集中的一块 64×64 px 矩形区域（含 1px 双边 gutter），存储真实的像素数据。每个分片由 `TileKey`（= `backend_id` + `generation` + `slot_index`）唯一标识。

`TileKey` 是纯物理坐标，不携带逻辑身份。分片的空/非空状态由凭证层（`TileManager`）通过映射表查询，`TileKey::empty()` 作为未分配的哨兵值仅在后端内部使用。

### 槽位

英文：logical slot / slot

文档图像按固定尺寸（62×62 px）规则切分后的一个网格位置。一幅图像的所有槽位构成行列矩阵，每个槽位由其在该图像内的 `tile_index`（`usize`，行优先）唯一确定。

槽位的核心是位置关系——"图像的哪一格"，不持有像素数据。每个槽位引用一个分片作为其当前内容，这个引用由 `TileOwner` 维护，可以随时间变化（分片被淘汰后重新分配新分片，或 resize 后槽位网格本身改变）。

### 凭证

英文：credential / TileCredential

槽位与物理分片之间的逻辑引用。64-bit 编码 `(backend_id, generation, record_index)`，每个槽位持有唯一凭证。凭证不携带物理分配状态 — 两个不同槽位的凭证总是不同的，即使它们都未分配物理分片。

凭证由 `TileManager` 发放和回收，生命周期与 `TileOwner` 绑定（Drop 时回收 `record_index`）。

_Avoid_: tile key (凭证是逻辑身份，TileKey 是物理地址)

### 分片管理器

英文：tile manager / TileManager

后端之上的凭证管理层。1:1 持有 `Backend`，管理 `credential → TileKey` 的映射表（`Vec<TileKey>` 按 `record_index` 索引，empty 表示未分配）。提供统一的分配/缓存/激活 API，对外屏蔽物理分配细节。

_Avoid_: backend manager (TileManager 是凭证层的入口，BackendManager 是后端集合管理器)

### 分片缓存

英文：tile caching

`atlas` 层的核心机制。分片在两种生命周期状态之间转换：

- **Active** — 分片正在被读写，不可回收
- **Cached** — 分片保留像素内容但暂不活跃，LRU 可淘汰

状态的切换通过 `TileManager` 的原子 API 完成（`cache_active_owners`、`activate_cached_group` 等），底层委托给 `Backend` 执行物理分片的调度。

### 缓存组

英文：cached group

多个分片的原子操作单元（`CachedTileGroup`）。一组缓存分片可以被一起激活回 Active，或作为整体被 LRU 淘汰。缓存组是分片缓存的管理粒度——单个分片不能脱离其所属组单独被淘汰。

### 缓存态图像

英文：cached image

一幅图像的所有槽位当前都引用缓存分片时的整体表示（`GlaCachedImage`）。缓存态图像不持有 Active 分片，因此不能直接参与 GPU 渲染；必须先激活（`activate()`）恢复为活跃图像（`GlaImage`）。

`RenderImageState::Cached` 是渲染编排层对同一概念的投影——标记某个节点的渲染输出处于缓存态。

### 呈现分片

英文：present tile

渲染后端上的一个特定分片，作为帧输出目标跨帧复用（`ScreenPresentCache`）。它的复用机制与分片缓存无关——它始终保持 Active，"缓存"仅指纹理分配在帧之间不重复创建。

## 笔刷流水线

### 笔画

英文：stroke

用户从按下到抬起之间，笔刷产生的一次完整绘制。笔画的生命周期：输入点经 smoother 平滑 → 冻结为曲线段 → 采样为 dab 序列 → 渲染到笔刷分片 → 预览合并 → 最终提交到文档真值。

### dab

笔刷的一次原子绘制更新。dab 将其贡献写入笔刷分片（如果有），或直接写入文档图像（简单笔刷）。dab 的定义与是否存在 intermediate 层无关——它是笔刷行为的最小执行单元。

### 冻结段

英文：frozen span

smoother 输出的已稳定曲线段。其几何不会再被未来输入点修改，可安全从中采样 dab。"冻结"强调不可变性。

对应代码中的 `CommittedCanvasSpan` / `CommittedCanvasSpanBuffer` / `pop_committed_spans`。

### 笔刷分片

英文：brush tile

笔画期间在笔刷后端上分配的虚拟图像，用于累积 dab 之间的关系。一个笔刷可以有零个、一个或多个笔刷分片，取决于实现。Round 笔刷使用单个笔刷分片累积流量场，这只是特例。

### 流量场

英文：flow field

dab 在笔刷分片中累积的数据。其结构不限于标量场——可以是 GPU 支持的任何格式（向量场、多通道场等）。Round 笔刷的流量场是标量场，属特例。

### 转移函数

英文：transfer function

合并时将笔刷分片累积的流量场映射为最终 coverage / alpha 的函数。Round 笔刷以 LUT 形式实现转移函数。

### 合并

英文：merge / tile merge

GPU 着色器将笔刷分片（累积的流量场）与原始分片（origin）合成为目标分片（destination）的操作。合并是笔画提交的核心步骤——通过转移函数将流量场映射为 coverage / alpha，再与原始内容混合。

合并有**预览合并**（笔画进行中，目标为临时预览分片）和**提交合并**（笔画结束，目标为文档槽位引用的分片），两者使用相同的着色器，区别仅在于目标分片的身份。

对应代码中的 `MergeTileCommand` / `RenderCommand::MergeTile` / `push_preview_merge` / `BrushEncodeStage::encode_merge_tile`。

### 笔画提交

英文：stroke commit

将一笔画的中间结果合并到文档真值的不可逆操作。包括：为受影响槽位备份当前分片（用于 undo）、将笔刷分片与原始内容合并写回文档槽位、清除笔刷预览。

对应代码中的 `StrokeCommitBatch` / `commit_gpu` / `commit_active_stroke`。

## 文档与交互

### 文档真值

英文：document truth

文档槽位中存储的权威像素内容。它是 undo/redo 的基准、保存/导出时的数据来源。笔刷的预览和中间结果不属于文档真值——它们只在笔画提交后才写入。

### 预览

英文：preview

笔画进行中的实时视觉反馈。预览通过合并笔刷分片到临时预览分片实现，在屏幕上叠加显示。预览不是文档真值——笔画取消时预览被丢弃，笔画提交时预览内容被正式合并写入。

### 备份

英文：backup / undo backup

笔画提交前为受影响的文档分片创建的副本。备份存储在缓存组中，用于 undo 时恢复。备份分片的分配通过 `gla_undo` 完成，其底层分片生命周期仍由 `atlas` 管理。
