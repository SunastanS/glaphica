# 在 TileOwner 与 Backend 之间插入凭证层

`TileKey` 目前同时充当分片的物理地址和槽位的逻辑身份。结果 `TileKey::empty()` 每后端只有一个哨兵值，不同槽位的空分片无法区分，导致渲染编排层的 compositing 逻辑无法将 source resolution / state mutation / command construction 拆分为可独立测试的步骤。

**决策**: 在 `TileOwner`（生命周期管理）和 `Backend`（物理分配器）之间插入 `TileManager` 凭证层。

```
TileOwner { credential: TileCredential, tile_key: Option<TileKey> }
         │ 凭证 ≠ 物理地址
         ▼
TileManager { records: Vec<TileKey>, free_records: Vec<u32> }
         │ 映射 credential → 物理 TileKey
         ▼
Backend (不变: 纯物理 slot 分配/释放/缓存/激活)
```

核心机制:
- `TileCredential`: 64-bit `(backend_id, generation, record_index)`, 与 `TileKey` 同形态, record_index 回收复用 + generation 防护
- `TileManager`: 1:1 持有 Backend, 管理凭证↦物理映射表, 回收通道指向 TileManager
- 所有 `TileOwner` 携带 recycle handle, Drop 时回收 record_index (不论 tile_key 是否为 None)

**考虑过的替代方案**: 将 source identity 下推到 `TileCompositeSource` 或 `CompositeTileCommand` 类型中。拒绝了 — 这是上层打补丁, 底层的 identity/address 混淆依然存在, compositing 逻辑仍无法拆分。

**影响**: `TileOwner`, `CachedTileGroup`, `GlaImage`, `GlaDocRenderer::compose_node_commands` 的签名和行为将逐步迁移。
