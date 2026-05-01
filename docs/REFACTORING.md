## 一、Atlas 核心层（`atlas` crate）

### 1. `BackendInner` 职责过重，建议拆分

`BackendInner` 目前同时承担了四个职责：slot 分配（`SlotPool`）、generation 追踪、缓存组管理与 LRU 淘汰、以及 `ClearBatch` 累积。这导致它有将近 400 行代码，且任何一个子策略的变动都要改动整个结构体。建议至少将缓存淘汰策略抽出为独立的 `CacheEvictionPolicy` 或类似组件：

```rust
struct CacheManager {
    groups: Vec<CacheGroup>,
    eviction_queue: VecDeque<CacheGroupId>,
    next_group_id: u32,
}
```

这样当你需要更换淘汰策略（比如从 FIFO 改为带权重的 LRU）时，不需要碰 slot 分配和 generation 逻辑。

### 2. `TileOwner::empty()` 每次分配一个全新的 `Arc<Mutex<Vec<TileKey>>>`

在 `GlaImage::new` 中会创建 `total_tiles` 个 `TileOwner::empty()`，每一个都构造了独立的 `Arc<Mutex<Vec>>`，而这些 handle 永远不会被真正使用（因为 `Drop` 时 key 是 `EMPTY` 会直接 return）。对于一张 (16 \times 16) 分片的图片，这就是 256 次无意义的堆分配。

建议将 `TileOwner` 改为：

```rust
pub struct TileOwner {
    recycle: Option<BackendRecycleHandle>,  // None 表示空
    key: TileKey,
}
```

或者用一个全局的空 handle 常量（通过 `LazyLock` 等），避免为每个空 slot 分配。

### 3. `Backend` 公共方法的模板代码重复

几乎所有 `Backend` 的 pub 方法都重复同样的三行：

```rust
let mut inner = self.lock_inner()?;
self.drain_owned_reclaims(&mut inner)?;
// ...实际操作
```

可以提取一个 `with_inner` 方法来减少噪音：

```rust
fn with_inner<R>(&self, f: impl FnOnce(&mut BackendInner) -> Result<R, AtlasError>) -> Result<R, AtlasError> {
    let mut inner = self.lock_inner()?;
    self.drain_owned_reclaims(&mut inner)?;
    f(&mut inner)
}
```

### 4. `CachedTileGroup` 泄漏内部实现

`CachedTileGroup::group_id()` 返回的 `u32` 是 `cache_groups` Vec 的内部索引。外部调用者拿到这个值没有任何合法用途，但却可以构造出非法的 group 引用。建议把 `group_id()` 从公共 API 中移除，或者至少改为 `pub(crate)`。同理，`keys` 字段目前是 `pub(crate)` 级别的 `Vec<TileKey>`，外部通过旧的 `alloc_cached_in_group` 能直接修改它——这种 "把 `&mut CachedTileGroup` 传入再在内部 push" 的模式打破了封装。可以考虑让 `alloc_cached_extending_group` 返回新的 key 和新的 group handle，由调用方自行持有：

```rust
// 现有设计：调用方必须持有可变引用并信任内部修改
backend.alloc_cached_extending_group(&cached)?;

// 建议：返回新 key，CachedTileGroup 在内部保持 immutable handle
let (key, cached) = backend.alloc_cached_extending_group(&cached)?;
```

---

## 二、Image 层（`glaphica_image` crate）

### 5. `GlaImage` 持有 `BackendId` 但不持有 `Backend`，导致 API 割裂

`ensure_active_tile_key` 要求调用方同时传入 `&Backend`，然后内部做 `backend_id` 一致性检查。这意味着每个调用点都要自行保证 image 和 backend 的配对关系。更好的做法是让 `GlaImage` 持有 `Backend`（它本身就是 `Arc<Mutex<...>>` 的 clone-cheap handle）：

```rust
pub struct GlaImage {
    layout: GlaImageLayout,
    tile_owners: Box<[TileOwner]>,
    backend: Backend,  // 替代 BackendId
}
```

这样 `ensure_active_tile_key` 不再需要外部参数，错误配对从类型层面就被消除了。

### 6. `GlaStoredImage` 反复重算 layout

`collect_non_empty_tile_indices`、`copy_tile_rgba8`、`tile_has_non_zero_pixel` 每次调用都 `self.layout()`（即 `GlaImageLayout::new(self.width, self.height)`）。虽然计算很轻量，但在批量扫描所有 tile 时这些 div_ceil 调用会重复成百上千次。建议在构造时缓存 layout，或者至少在方法内部只算一次再传入内部函数。

### 7. `GlaCachedImage` 的验证可更严格

`GlaCachedImage::new` 验证了 tile 总数和非空 tile 数量，但没有验证每个非空 `TileKey` 是否确实存在于 `cache_group.keys()` 中。目前只检查了 **数量** 相等——如果调用方传入了正确数量但完全不同的 key 集合，验证会通过。建议增加集合一致性检查。

---

## 三、跨模块设计

### 8. 错误类型缺乏统一策略

目前有六种独立错误类型：`AtlasError`、`AtlasManagerError`、`GlaImageCreateError`、`GlaImageTileAccessError`、`GlaImageEnsureActiveTileError`、`GlaCachedImageCreateError`、`GlaStoredImageError`。其中 `GlaImageEnsureActiveTileError` 已经包装了其他两种错误，但其余方法直接返回 `AtlasError`。建议在 image 层统一一个 `GlaImageError`，把 atlas 层的错误作为 source，避免调用方需要了解 atlas 内部错误变体。

### 9. `GlaImage` 和 `GlaCachedImage` 之间的转换路径不够清晰

目前 `GlaCachedImage::from_active_image` 从 `GlaImage` 转换而来，但没有反向路径（从 cached image 恢复到 active image）。考虑到 `Backend::activate_cached_group` 已存在，可以在 image 层提供对称的 API：

```rust
impl GlaCachedImage {
    pub fn activate(self, backend: &Backend) -> Result<GlaImage, ...> { ... }
}
```

### 10. 考虑为 tile 操作引入 trait 抽象

`GlaImage`、`GlaCachedImage`、`GlaStoredImage` 都有 `layout()`、`tile_count()` 等共同操作，但 atlas-backed image 与 pixel-backed stored image 的能力不同。如果上层代码需要多态处理，应拆成 grid、atlas tile map、pixel source 三层：

```rust
pub trait TileGrid {
    fn layout(&self) -> GlaImageLayout;
    fn tile_count(&self) -> usize;
}

pub trait AtlasTileMap: TileGrid {
    fn tile_key(&self, tile_index: usize) -> Option<TileKey>;
}

pub trait PixelTileSource: TileGrid {
    type Error;
    fn copy_tile_rgba8(&self, tile_index: usize, output: &mut Vec<u8>) -> Result<(), Self::Error>;
}
```

---

总结来看，最有价值的三个改动优先级是：**拆分 `BackendInner`**（降低维护成本）、**修复 `TileOwner::empty()` 的分配浪费**（性能影响直接可测）、**让 `GlaImage` 持有 `Backend` 而非 `BackendId`**（从 API 层面消除错误配对的可能）。

---

# 精炼执行计划：GPU 图像分片处理组件重构

## 代码库现状与原始计划之间的差异

| 事项 | 原始计划 | 实际情况 |
|---|---|---|
| Step 10（将 `CachedTileGroup.keys` 设为私有） | 仍需要做 | **已完成。** 两个字段（`group_id`、`keys`）均纯私有（无 `pub` 修饰符），atlas/src/lib.rs:251-252。同 crate 代码通过直接字段访问来读写它们，而非通过方法。 |
| Step 9 的 clone 开销 | 可接受 | 质疑成立，但并非阻塞项。`alloc_cached_extending_group` 零外部调用方，因此 clone 开销并非热路径问题。 |
| Step 6 的接口设计 | 模糊（"take closures or return decisions"） | 需要制定具体设计方能执行。见 Step 6 方案。 |
| Step 11 的 serde 兼容性 | 未提及 | `GlaStoredImage` 派生 `Serialize, Deserialize`。新增 `layout` 字段需妥善处理。 |
| Phase 5 风险 | 未充分评估 | `GlaImage` 持有 `Backend` 会改变所有权语义。需验证无图像将后端作为存活依赖的场景。 |
| RENDERER 中的重复实现 | 未提及 | `gla_doc_renderer::ensure_active_target_tile`（lib.rs:753）完整复刻了 `ensure_active_tile_key`。应在 Phase 5 之后消除。 |
| 错误类型统一（§8） | 缺失 | 可在 Phase 5 期间自然加入。 |

---

## 分步计划

### Phase 1：Atlas — 减少模板代码与微优化

#### Step 1 — 为 Backend 引入 `with_inner`（§3）

- 新增私有方法：
  ```rust
  fn with_inner<R>(
      &self,
      f: impl FnOnce(&mut BackendInner) -> Result<R, AtlasError>,
  ) -> Result<R, AtlasError> {
      let mut inner = self.lock_inner()?;
      self.drain_owned_reclaims(&mut inner)?;
      f(&mut inner)
  }
  ```
- 将所有 11 个执行 lock → drain → delegate 模式的 Backend 公共方法重构为使用 `with_inner`。
- **验证：** `cargo test -p atlas`

#### Step 2 — 修复 `TileOwner::empty()` 的分配浪费（§2）

- 将 `recycle: BackendRecycleHandle` 改为 `recycle: Option<BackendRecycleHandle>`，atlas/src/lib.rs:67
- `empty()` 返回 `Self { recycle: None, key: TileKey::EMPTY }`
- 仅在 `recycle.is_some()` 时入队 Drop
- 将创建 `TileOwner` 的 2 处构造调用更新为 `Some(self.recycle.clone())`（`Backend::alloc_active` 与 `activate_cached_tile`）

> **⚠️ 注意：** `TileOwner::backend_id()` 在 key 为 `EMPTY` 时会解码出 `BackendId(255)`。
> `cache_active_owners` 中有 `owner.backend_id() != self.backend_id()?` 检查。
> 需确认 empty owner 不会进入该路径，或将 `backend_id()` 改为返回 `Option<BackendId>`。

- **验证：** `cargo test -p atlas`

---

### Phase 2：Atlas — 拆分 `BackendInner` 缓存逻辑（§1）

#### Step 3 — 定义 `CacheManager` 结构体（仅字段 + `new()`）

- 新增：
  ```rust
  struct CacheManager {
      groups: Vec<CacheGroup>,
      eviction_queue: VecDeque<CacheGroupId>,
      next_group_id: u32,
  }
  ```
- **验证：** `cargo check -p atlas`

#### Step 4 — 将 group 访问器方法迁移至 `CacheManager`

- 迁移：`group()`、`group_mut()`、`acquire_vacant_group()`
- `BackendInner` 改为通过 `self.cache_manager.group(…)` 转发调用
- **验证：** `cargo test -p atlas`

#### Step 5 — 将缓存组生命周期方法迁移至 `CacheManager`

- 迁移：`cached_group_matches_handle()`、`detach_slot_from_group()`
- **注意：** `release_group` 需要同时访问 `CacheManager`（groups/slots）和 `BackendInner`（slot_owners、generations、slot_pool、clear batches）。`release_group` 保留在 `BackendInner`，内部调用 `CacheManager` 方法获取 slot 列表。
- **验证：** `cargo test -p atlas`

#### Step 6 — 将淘汰/容量逻辑迁移至 `CacheManager`（⭐ 关键设计决策）

`ensure_capacity` 和 `reclaim_oldest_cached_group` 需要同时操作缓存组状态和 slot 基础设施。

**采用 pop + finalize 模式（无回调）：**

```rust
struct ReclaimDecision {
    group_id: CacheGroupId,
    slots: Vec<u32>,
}

impl CacheManager {
    /// 弹出最老的非空组，返回其 slots 供调用方释放。
    /// 跳过已清空的组。
    fn pop_oldest_reclaimable(&mut self) -> Option<ReclaimDecision> {
        while let Some(group_id) = self.eviction_queue.pop_front() {
            let slots = &self.groups[group_id.0 as usize].slots;
            if !slots.is_empty() {
                let slots = slots.clone();
                return Some(ReclaimDecision { group_id, slots });
            }
        }
        None
    }

    /// 调用方释放完 slots 后，通知 CacheManager 清理组状态。
    fn finalize_reclaim(&mut self, group_id: CacheGroupId) {
        if let Some(group) = self.groups.get_mut(group_id.0 as usize) {
            group.slots.clear();
        }
    }
}
```

`BackendInner::ensure_capacity` 保持为一个循环：

```rust
fn ensure_capacity(&mut self, count: usize) -> Result<(), AtlasError> {
    while self.slot_pool.available(self.layout.total_slots()) < count {
        let Some(decision) = self.cache_manager.pop_oldest_reclaimable() else {
            return Err(AtlasError::OutOfSlots);
        };
        let mut cleared_slots = Vec::with_capacity(decision.slots.len());
        for slot in decision.slots {
            self.release_slot(slot, &mut cleared_slots)?;
        }
        self.cache_manager.finalize_reclaim(decision.group_id);
        self.push_clear_batch(cleared_slots);
    }
    Ok(())
}
```

这样 `CacheManager` 不需要回调，职责单向流动，也更容易独立测试。

- **验证：** `cargo test -p atlas`

#### Step 7 — 用单个 `CacheManager` 字段替换 `BackendInner` 中的三个字段

- 移除：`cache_groups`、`cached_group_queue`、`next_group_id`
- 替换为：`cache_manager: CacheManager`
- 更新 `BackendInner::new` 以初始化 `CacheManager::new()`
- **验证：** `cargo test -p atlas`

---

### Phase 3：Atlas — 清理 `CachedTileGroup` 公共 API（§4）

#### Step 8 — 将 `group_id()` 访问权限改为 `pub(crate)`

- `group_id()` 零外部调用方 —— 纯粹的可见性降级
- **验证：** `cargo check --workspace`

#### Step 9 — 将 cached group 扩展 API 改为不可变 handle 签名

**决策：接受不可变 handle 方案，并命名为 `alloc_cached_extending_group`。** 签名为 `&CachedTileGroup -> Result<(TileKey, CachedTileGroup)>`，通过 clone keys 构造新 handle。理由：零外部调用方，非热路径（可接受 clone 开销）；方法名必须表达调用方需要接住新的 group handle。

- `Backend::alloc_cached_extending_group(&self, cached: &CachedTileGroup) -> Result<(TileKey, CachedTileGroup)>`
- `BackendInner::alloc_cached_extending_group(&mut self, cached: &CachedTileGroup) -> Result<(TileKey, CachedTileGroup)>`
- 方法内部原先 `cached.keys.push(key)` 改为构造新的 `CachedTileGroup`
- 调用方通过重新绑定 `let (key, cached) = backend.alloc_cached_extending_group(&cached)?;` 来持有最新 handle
- **验证：** `cargo test -p atlas`

#### Step 10 — 已删除

该字段已经是私有状态，无需操作。

---

### Phase 4：`gla_image` — 模块级内部清理（§6、§7）

#### Step 11 — 在 `GlaStoredImage` 中缓存 layout（§6）

- 在 `struct GlaStoredImage` 中新增 `layout` 字段

> **⚠️ Serde 处理方案：** 使用 `#[serde(try_from = "GlaStoredImageRaw", into = "GlaStoredImageRaw")]` 中间层模式。
> 定义一个不含 `layout` 字段的 `GlaStoredImageRaw`（仅 `width`/`height`/`pixels_rgba8`），
> 实现 `TryFrom<GlaStoredImageRaw> for GlaStoredImage`，在其中调用 `new_rgba8()` 同时校验像素长度并重建 layout。
> 这比 `#[serde(skip, default)]` 更安全，因为不会产生 layout 与 width/height 不一致的中间状态。
> 当前工作空间内无实际序列化调用方，此举为预防性措施。

- 在 `new_rgba8()` 中计算并存储 layout
- 更新 `collect_non_empty_tile_indices`、`copy_tile_rgba8`、`tile_has_non_zero_pixel`，使用 `self.layout`
- **验证：** `cargo test -p gla_image`

#### Step 12 — 在 `GlaCachedImage::new` 中新增更严格的 key 存在性校验（§7）

- 在现有计数校验之后，验证每个非 EMPTY 的 `tile_key` 是否存在于 `cache_group.keys()` 中
- 对 tile 数量 ≤256 使用线性扫描即可（无需 HashSet）
- 若失败，新增 `GlaCachedImageCreateError::KeyNotInCacheGroup` 变体

> **⚠️ 前置检查：** 先 grep 所有 `GlaCachedImage::new` 的调用点，确认没有渐进式构建的使用模式
> （即先构建 `GlaCachedImage` 再异步填充 `cache_group` 的场景）。

- **验证：** `cargo test -p gla_image`

---

### Phase 5：`gla_image` — `GlaImage` 持有 `Backend` 以替代 `BackendId`（§5）

> **⚠️ 前置风险评估：** 在开始前确认以下两点：
> 1. 是否有场景需要在销毁 `Backend` 后仍然持有 `GlaImage`（如序列化场景）？
> 2. `GlaImage` 从纯数据容器变为持有 `Arc<Mutex<...>>` 的活跃对象，上层所有权模型是否兼容？

#### Step 13 — 修改 `GlaImage` 结构体使其持有 `Backend`

- 将 `backend: BackendId` 改为 `backend: Backend`，image.rs:90
- 更新 `GlaImage::new` 入参为 `Backend`（`Arc<Mutex<…>>`，clone 极其轻量）
- 保留 `backend_id()` 便捷访问器（委托 `self.backend.backend_id()`），新增 `backend() -> &Backend`
- **验证：** `cargo check -p gla_image`

#### Step 14 — 简化 `ensure_active_tile_key`

- 移除 `backend: &Backend` 参数；改为使用 `self.backend.alloc_active()`
- 移除 `GlaImageEnsureActiveTileError` 中 `WrongBackend` 的使用（配对关系由类型系统保证）
- `GlaImageTileAccessError::WrongBackend` 保留用于 `replace_tile_owner`（该方法仍接受外部 `TileOwner`）
- **验证：** `cargo check -p gla_image`

#### Step 15 — 更新所有外部调用方

| 文件 | 行号 | 说明 |
|---|---|---|
| `gla_document/src/document.rs` | 168, 322, 332 | 传递完整 `Backend` 而非 `BackendId` |
| `gla_doc_renderer/src/lib.rs` | 263, 564, 733, 1044 | 同上 |
| `gla_image/src/image.rs` | 315, 328, 339, 356, 375, 397, 425, 461 | 测试更新 |
| `brush/src/lib.rs` | 962 | 测试更新 |

- **验证：** `cargo check --workspace`（逐文件迭代）

#### Step 16 — 更新 `ensure_active_tile_key` 调用方，移除 backend 参数

| 文件 | 行号 |
|---|---|
| `gla_document/src/document.rs` | 502 |
| `brush/src/lib.rs` | 648 |

- **验证：** `cargo check --workspace`

---

### Phase 5.5：消除 renderer 中的重复实现

#### Step 16.5 — 移除 `ensure_active_target_tile`，统一使用 `ensure_active_tile_key`

- `gla_doc_renderer::ensure_active_target_tile`（lib.rs:753）完整复刻了 `ensure_active_tile_key` 的逻辑
- 在 Step 14 之后，`GlaImage` 已自带 backend 引用。将调用方替换为 `image.ensure_active_tile_key(tile_index)?` 并删除该函数
- **验证：** `cargo test -p gla_doc_renderer`

---

### Phase 5.6：统一错误类型（§8）

#### Step 16.6 — 引入统一的 `GlaImageError`

- 引入 `GlaImageError` 封装 `AtlasError`、`GlaImageTileAccessError`、`GlaImageCreateError`
- 让 `ensure_active_tile_key` 返回 `Result<_, GlaImageError>`
- Step 14 已挪开 `WrongBackend`，现在正是统一的最佳时机
- **验证：** `cargo check --workspace`

---

### Phase 6：跨模块改进（§9、§10）

#### Step 17 — 拆分 image tile traits（§10）

- 在 `gla_image/src/lib.rs` 中定义：
  ```rust
  pub trait TileGrid {
      fn layout(&self) -> GlaImageLayout;
      fn tile_count(&self) -> usize;
  }

  pub trait AtlasTileMap: TileGrid {
      fn tile_key(&self, tile_index: usize) -> Option<TileKey>;
  }

  pub trait PixelTileSource: TileGrid {
      type Error;
      fn copy_tile_rgba8(&self, tile_index: usize, output: &mut Vec<u8>) -> Result<(), Self::Error>;
  }
  ```
- `GlaImage`、`GlaCachedImage` 实现 `TileGrid + AtlasTileMap`
- `GlaStoredImage` 实现 `TileGrid + PixelTileSource`
- **验证：** `cargo test -p gla_image`

#### Step 18 — 新增 `GlaCachedImage::activate()`（§9）

- `pub fn activate(self, backend: &Backend) -> Result<GlaImage, GlaImageError>`
- 调用 `backend.activate_cached_group(&self.cache_group)`，按 `self.tile_keys` 将 owner 重新映射到新的 `GlaImage` 中
- 处理 tile 数量不匹配：`cache_group.keys().len()` 可能小于 `layout.total_tiles()`（允许存在 EMPTY 图块）——未被 `activate_cached_group` 返回的 slot 位置填入 `TileOwner::empty()`
- **验证：** `cargo test -p gla_image`

---

## 风险排序

| Phase | 风险等级 | 说明 |
|---|---|---|
| Phase 1–2 | 🟢 低 | 纯内部 atlas 重构，不改变公共 API |
| Phase 3 | 🟢 低 | 几乎全为可见性降级 |
| Phase 4 | 🟡 中 | Step 11 需处理 serde，Step 12 改变校验契约 |
| Phase 5 | 🟠 中高 | 跨 crate 签名变更，影响范围最广，但受益也最大 |
| Phase 5.5–5.6 | 🟡 中 | 依赖 Phase 5 完成，但改动本身较机械化 |
| Phase 6 | 🟢 低 | 纯新增 API，不影响现有代码 |

## 变更汇总

| 改动 | 说明 |
|---|---|
| **Step 10 已移除** | `CachedTileGroup` 字段已经是私有状态 |
| **Step 6 采用 pop + finalize 模式** | 无回调，职责单向流动，便于独立测试 |
| **Step 9 采用不可变 handle 方案** | 签名 `&CachedTileGroup -> Result<(TileKey, CachedTileGroup)>`，通过 clone 构造新 handle |
| **Step 11 采用 serde `from` 中间层** | 比 `#[serde(skip, default)]` 更安全，无不一致中间状态 |
| **Step 13 `GlaImageCreateError` 新增 `Backend(AtlasError)`** | 替代错误的 `map_err(|_| TooManyTiles)` |
| **`restore_cached_image` 替换为 `GlaCachedImage::activate()`** | renderer 统一使用 activate API，消除重复恢复逻辑 |
| **新增 Step 16.5** | 消除 `ensure_active_target_tile` 的重复实现 |
| **新增 Step 16.6** | 统一错误类型（§8） |
