<!-- doc: architecture-reference -->
<!-- status: current -->
<!-- updated: 2026-05 -->

# Design

## Layer Boundaries

系统按 `core -> atlas -> image -> document -> brush -> doc_renderer -> renderer` 分层，最外层由 `app` 编排。`core` 是共享基础类型层；`gla_undo` 是 `gla_document` 的 backup/undo 支撑层。上层可以编排下层，但不应该重写下层已经定义的资源语义。

### `atlas`

`atlas` 是唯一的 tile 资源生命周期层。

它负责：

- 定义 `BackendId`、`TileKey`、`TileOwner`、`CachedTileGroup`
- 分配 active tile
- 将 active tile 转为 cached group
- 将 cached tile 或 cached group 激活回 active owner
- 回收 tile，并维护 generation/state 校验
- 提供 tile 状态与 backend 归属校验

它不负责：

- 逻辑图像尺寸、tile 在图像内的逻辑索引
- 文档树结构
- 笔刷行为
- GPU 渲染命令

约束：

- 一切 tile 管理能力都必须从 `atlas` 提供原子 API
- 外层不得手动模拟 active/cached 生命周期
- 外层不得复制 `TileKey -> atlas 坐标` 或 generation 校验逻辑

### `image`

`gla_image` 是逻辑图像层。它把文档里的二维图像切分为逻辑 tile 槽位，并把这些槽位映射到 atlas tile。

它负责：

- 定义 `GlaImageLayout`
- 定义逻辑 tile 索引与尺寸边界
- 持有 `Backend`，在逻辑 tile 槽位中通过 `TileOwner` 引用活跃 atlas tile
- 定义 `TileGrid`、`AtlasTileMap`、`PixelTileSource` trait，为 `GlaImage`、`GlaCachedImage` 等提供统一的 tile 访问接口
- 提供按逻辑 tile 访问、替换、清理、收集受影响 tile 的 API
- 表达 `GlaCachedImage` 的冻结 tile key 排布

它不负责：

- active/cached 生命周期规则
- 文档节点关系
- 笔刷中间层策略
- GPU 执行

约束：

- `gla_image` 只描述"哪一个逻辑 tile 槽位引用哪个 atlas tile"
- tile 的分配、缓存、激活必须继续委托给 `atlas`
- `TileGrid` / `AtlasTileMap` / `PixelTileSource` 为上层提供统一读接口，不引入新的生命周期语义

### `document`

`gla_document` 是文档真值层。它持有文档树、active layer、节点属性以及文档备份存储。

它负责：

- 定义 `GlaDoc`、节点树、layer/branch 结构
- 持有每个节点对应的 `GlaImage`
- 持有 active layer 真值
- 持有 `DocumentBackupStore`
- 定义文档序列化与存储格式
- 提供文档级 render plan 输入

它不负责：

- 笔刷 stroke 生命周期
- preview overlay
- render cache
- GPU command 执行

约束：

- 文档真值只能通过文档图像修改
- 备份 tile 的分配来源于 `DocumentBackupStore`，其底层 tile 生命周期仍由 `atlas` 负责

### `undo`

`gla_undo` 是文档备份与 undo/redo 层。它为 `gla_document` 提供 tile 级备份存储和回滚能力。

它负责：

- 定义 `GlaImageUndo`、`GlaImageUndoBackup`、`GlaImageUndoRestore`
- 定义 `GlaImageUndoTileAction` 和 `GlaImageUndoTileRecord` 以记录每次 tile 变更
- 在 stroke commit 时为受影响的文档 tile 分配 backup cached group
- 提供 undo/redo 时的 tile 置换逻辑

它不负责：

- 文档树结构管理
- 笔刷 stroke 行为
- render cache 或 preview overlay
- GPU 执行

约束：

- 备份 tile 的底层生命周期仍由 `atlas` 负责
- `gla_undo` 只描述"备份了哪些 tile"和"如何回滚"，不定义 atlas 资源规则
- `gla_document` 通过 re-export 将 `gla_undo` 的类型暴露给上层，但不改写其语义

### `brush`

`brush` 是 stroke 行为层。它描述一笔在生命周期内触达了哪些逻辑 tile、这些 tile 的 intermediate owner 是什么、以及要提交哪些 brush render command。

它负责：

- 定义笔刷 shader 注册信息
- 定义并持有每个 brush 对应的 `BrushBackend`
- 在注册 brush 时同时注册 shader spec 和 brush backend
- 通过 brush backend 在各个 stroke 之间复用同一个 intermediate atlas backend
- 持有旧 stroke 对应的历史 cached group 句柄，为未来可能支持定向编辑历史中的某一笔做准备；具体回收仍由 `atlas` 自动决定
- 持有 stroke 生命周期内的 `BrushStrokeState`
- 记录 touched tile 与对应 intermediate tile
- 为 dab、preview merge、commit merge 生成 `RenderCommand`
- 为不同笔刷编码自己的 shader payload

它不负责：

- tile active/cached 生命周期实现
- backup store 持久持有
- render cache 或 preview node 持有
- GPU shader 执行

约束：

- brush backend 是运行时复用对象，但它不重写 atlas 资源规则
- stroke 期间持有的 intermediate tile 以 `TileOwner` 形式存在
- stroke 结束后 brush backend 可以归档历史 cached group 句柄，但正常运行不会把这些历史内容重新取出参与新 stroke；active/cached 转换必须调用 `atlas` 原子 API，而不是在 `brush` 内部重写一套 tile 生命周期

### `doc_renderer`

`gla_doc_renderer` 是文档渲染编排层。它把文档树、branch/root cache、brush preview overlay 组织成 renderer 可执行的渲染输入。

它负责：

- 持有 render backend
- 持有 branch/root render cache
- 持有 brush preview image 这样的交互覆盖层
- 根据文档结构准备 active render plan
- 决定某个节点当前应读取文档真值、cached image 还是 preview overlay

它不负责：

- 文档结构真值
- 笔刷 intermediate 生命周期
- tile 生命周期实现
- GPU shader 细节

约束：

- preview overlay 是渲染态资源，不是文档真值
- render cache 与 preview tile 的分配仍然通过 `atlas`
- `gla_doc_renderer` 只组织资源来源，不定义 brush/atlas 的资源规则

### `renderer`

`renderer` 是 GPU 执行层。它只消费 `RenderCommand` 和 atlas backend 资源，把命令翻译成具体 GPU 操作。

它负责：

- 持有 GPU context、atlas texture、pipeline
- 把 `TileKey` 解析为 atlas texture 地址
- 执行 `CopyTile`、brush `ApplyDab`、brush `MergeTile`、composite、present
- 管理 brush shader pipeline 与 uniform 上传

它不负责：

- 文档树和业务状态
- tile 生命周期策略
- stroke 生命周期
- backup/preview/cache 的业务语义

约束：

- 所有 GPU 操作必须由 `renderer` 发起
- 其他层只能通过 `RenderCommand` 和 renderer 公共 API 触发 GPU 行为

### `app`

`app` 是应用编排层。它不定义任何资源语义，而是驱动整个渲染管线：接收用户输入、协调 brush stroke 生命周期、管理文档加载与保存、以及将各层的输出串联为完整的帧。

它负责：

- 持有 `GlaDoc`、`GlaDocRenderer`、`BrushRegistry` 等顶层对象
- 将输入事件路由到 brush，驱动 stroke 的开始/进行/结束
- 在 stroke 期间协调 brush preview 的创建与清除
- 管理文档的打开、保存、导出（直接遍历文档状态）
- 持有 undo stack 并驱动 undo/redo 操作

它不负责：

- tile 生命周期
- 文档结构定义
- 笔刷 dab/merge 语义
- render cache 策略
- GPU 命令编码

约束：

- `app` 可以遍历文档状态、持有整个文档进行 save/export，但不能绕过下层公共 API 直接操作 tile 或 GPU
- 输入 → brush → render 的驱动必须通过各层的原子 API，`app` 不实现渲染逻辑

## Cross-Layer Rules

- `atlas` 以下没有业务语义，只有 tile 资源语义。
- `gla_image` 不拥有 tile 生命周期，只拥有逻辑槽位到 tile 资源的引用关系。
- `gla_document` 只持有文档真值，备份存储委托给 `gla_undo`；不持有 brush intermediate 和 render cache。
- `gla_undo` 只持有 tile 级备份记录和 undo/redo 栈，不涉及文档树结构或笔刷语义。
- `brush` 持有跨 stroke 复用的 brush backend，以及 stroke 生命周期内的 intermediate 与 touched tile 记录；它不持有 preview node，也不重写 atlas 资源规则。
- `gla_doc_renderer` 只持有渲染态缓存和 overlay，不修改文档结构定义。
- `renderer` 不持有业务状态，只执行命令。
- `app` 是唯一可以持有全部顶层对象并直接遍历文档状态的层，但它不实现任何资源层或渲染层的逻辑。

## Brush Stroke Flow

一笔进行中：

1. 上层把输入点送入 `brush`
2. `brush` 为受影响逻辑 tile 分配 intermediate active owner
3. `brush` 生成 `ApplyDab` 命令，目标是 intermediate tile
4. `doc_renderer` 为 preview overlay 提供目标 tile
5. `brush` 生成 `MergeTile(origin truth, intermediate -> preview overlay)` 命令
6. `renderer` 执行这些命令

一笔结束时：

1. `brush` 基于 touched tiles 生成 commit 命令
2. 若文档真值 tile 非空，`gla_undo` 为受影响的 tile 分配 backup cached group
3. `renderer` 先执行 `CopyTile(image -> backup)`
4. `renderer` 再执行 `MergeTile(backup or empty + intermediate -> image truth)`
5. 如果要保留 intermediate 结果供后续历史定位或定向编辑，上层必须把 stroke 持有的 intermediate owners 交给 `atlas` 转为 cached group

## Design Intent

这套分界的目标是：

- tile 生命周期规则只写一次，并且只存在于 `atlas`
- 文档真值、stroke 中间态、preview overlay、render cache 互相独立
- `brush` 可以自由扩展 dab shader 和 merge shader，但不接管资源层语义
- `renderer` 保持为无业务状态的通用命令执行器
