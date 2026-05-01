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

`CachedTileGroup::group_id()` 返回的 `u32` 是 `cache_groups` Vec 的内部索引。外部调用者拿到这个值没有任何合法用途，但却可以构造出非法的 group 引用。建议把 `group_id()` 从公共 API 中移除，或者至少改为 `pub(crate)`。同理，`keys` 字段目前是 `pub(crate)` 级别的 `Vec<TileKey>`，外部通过 `alloc_cached_in_group` 能直接修改它——这种 "把 `&mut CachedTileGroup` 传入再在内部 push" 的模式打破了封装。可以考虑让 `alloc_cached_in_group` 返回新的 key，由调用方自行持有：

```rust
// 现有设计：调用方必须持有可变引用并信任内部修改
backend.alloc_cached_in_group(&mut cached)?;

// 建议：返回新 key，CachedTileGroup 在内部保持 immutable handle
let (key, cached) = backend.alloc_cached_in_group(cached)?;
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

`GlaImage`、`GlaCachedImage`、`GlaStoredImage` 都有 `layout()`、`tile_count()`、`tile_key()` 等共同操作，但目前没有统一 trait。如果上层代码需要多态地处理这三种 image（比如在渲染管线中统一查询 tile key），一个 `TileMap` trait 会很有用：

```rust
pub trait TileMap {
    fn layout(&self) -> GlaImageLayout;
    fn tile_key(&self, tile_index: usize) -> Option<TileKey>;
}
```

---

总结来看，最有价值的三个改动优先级是：**拆分 `BackendInner`**（降低维护成本）、**修复 `TileOwner::empty()` 的分配浪费**（性能影响直接可测）、**让 `GlaImage` 持有 `Backend` 而非 `BackendId`**（从 API 层面消除错误配对的可能）。
