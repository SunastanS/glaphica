# wgpu / WGSL 排查记录

另见：
- `docs/Instructions/debug_playbook.md`（更偏“渲染树/赃区/缓存/提交语义”的综合排查流程）

## 案例：WGSL 保留字导致管线无效

### 现象
- 运行期在 brush/merge 链路看到 `Validation Error`，并且最终表现为 `ComputePipeline ... is invalid`。
- 上层可能出现二次报错（例如 merge abort、callback channel closed），但这些通常不是根因。

### 根因
- `crates/renderer/src/brush_dab_write.wgsl` 中把 uniform 变量命名为 `meta`。
- 在当前 WGSL 语法下，`meta` 是保留字，shader 解析失败，导致后续 compute pipeline 无效。

### 直接修复
- 将变量名改为非保留字（例如 `dab_meta`），并同步替换所有引用。

### 推荐定位方式
1. 开启 `GLAPHICA_BRUSH_TRACE=1`。
2. 观察 `on_uncaptured_error` 输出，优先找第一条 shader/pipeline 级错误。
3. 在 pipeline 创建处使用 `push_error_scope(ErrorFilter::Validation)` + `pop()` 做 fail-fast。
4. 若运行期出现 merge 失败，先判断是否为上游 uncaptured GPU error 连带触发，避免误判为 merge 算法问题。

### 本次关键报错特征
- `In Device::create_shader_module ... parsing error: name meta is a reserved keyword`
- `create renderer.brush_dab_write.pipeline validation failed`

这两条同时出现时，应优先检查 WGSL 语法/标识符，而不是 merge 业务逻辑。

## 规则：WGSL 变更必须带可加载测试

- 强制要求：后续所有新增或修改的 WGSL 程序，必须至少补充 1 个“可正确加载”的自动化测试。
- 最低验收标准：
  1. `Device::create_shader_module` 成功（无解析错误）。
  2. 对应 pipeline（compute 或 render）可成功创建（无 validation error）。
- 建议做法：在测试中配合 `push_error_scope(ErrorFilter::Validation)` + `pop()`，并在出现错误时直接失败（fail fast）。

## 案例：同一提交内重复写同一 buffer 导致绘制断续

### 现象
- 笔迹出现明显断续、缺段或重复段。
- 复现通常与“是否跨 tile / 命令 fan-out”相关，但不一定固定在 tile 边界。
- 常见于一个逻辑批次里有多个 compute pass，且每个 pass 依赖同一个 storage/uniform buffer。

### 常见错误写法
- 在 CPU 循环里多次 `queue.write_buffer(A)` 覆盖同一个 buffer `A`。
- 同时把多个 pass 都编码到同一个 `CommandEncoder`，最后只 `queue.submit` 一次。
- 结果是前面 pass 可能读到被后续写入覆盖后的数据，出现“看起来随机”的断续。

### 正确写法（优先级从高到低）
1. 单批次单写入：
   - 先把本批次所有命令打包到 buffer（一次 `write_buffer`）。
   - 再编码一次 pass（或确定不会依赖后续覆盖的多 pass）。
2. 若必须多 pass 且每 pass 参数不同：
   - 使用动态 offset（uniform/storage）或每 pass 独立绑定区间，避免共享同一偏移。
3. 若历史包袱较重且需快速止血：
   - 每次写入后立即提交对应命令（保证顺序），后续再优化为批处理。

### 工程规则
- 禁止：在单次 submit 内，依赖“循环多次覆盖同一 buffer + 多 pass 读取”的隐式顺序语义。
- 必须：为 fan-out 场景设置容量上限断言，溢出直接 panic，防止 silent truncation。
- 建议：在 `GLAPHICA_BRUSH_TRACE=1` 下输出每批 `command_count/chunk_count`，便于快速判断是否进入高风险路径。

## 案例：同一 submit 内覆盖 tile instance buffer 导致 tile 错位/拼贴

### 典型现象
- 画面出现“拼贴/mosaic”或 tile 位置错乱：同一张 tile 被贴到多个位置，或不同 tile 被交换位置。
- 常伴随“新增了一层 group / live preview / 多一次 composite pass 后才出现”，即在 render tree 更深、composite pass 次数变多时更容易触发。
- 视觉上更像“整块瓦片被移动/复制”，而不是笔刷写入的局部像素错误。

### 根因（非常典型）
- 在同一个 `Queue` 提交（同一帧的同一次 composite submit）里：
  1. 使用同一个 GPU buffer（例如 `tile_instance_buffer`）作为 storage/uniform 输入；
  2. 在 CPU 侧反复调用 `queue.write_buffer(buffer, offset=0, ...)` 覆盖同一段内存；
  3. 同时把多个 render pass 都编码进同一个 `CommandEncoder`，最后只 `queue.submit` 一次。
- 由于 `queue.write_buffer` 的写入会在该 submit 执行前按顺序发生，最终 command buffer 执行时所有 pass 都只能读到“最后一次覆盖后的内容”，从而导致早先 pass 的 draw instance 被污染，表现为 tile 错位/拼贴。

一句话：**“同一 submit 内，多次覆盖同一段 buffer + 多个 pass 读取”不会产生你想要的 per-pass 语义。**

### 验证方式（推荐 fail-fast）
1. 写一个最小可视化回归测试，确保 tile 坐标映射正确：
   - 构造 `256x256` 图像拆成 `2x2` tiles（每块不同颜色）。
   - 走完整条 composite 路径并做 readback，逐像素断言输出和输入一致。
2. 再加一个“嵌套 group cache”版本：
   - leaf -> groupA (slot composite + tile copy) -> groupB (slot composite + tile copy) -> output (content composite)
   - tile_index 使用稀疏/不连续值，避免“偶然按顺序”掩盖问题。

本仓库对应的复现测试（默认 `#[ignore]`，需要手动跑）：
- `crates/renderer/src/tests.rs`: `composite_tile_mapping_renders_quadrant_image_exactly`
- `crates/renderer/src/tests.rs`: `composite_tile_mapping_survives_nested_group_cache_levels`

运行：
```bash
cargo test -p renderer composite_tile_mapping_renders_quadrant_image_exactly -- --ignored --nocapture --test-threads=1
cargo test -p renderer composite_tile_mapping_survives_nested_group_cache_levels -- --ignored --nocapture --test-threads=1
```

### 修复方式（优先级从高到低）
1. **实例数据用“arena/append-only”分配（推荐）**
   - 在一次 submit 期间，把每个 pass 需要的 instance 数据写到 buffer 的不同 offset（append-only）。
   - `draw()` 的 instance range 需要加上该 pass 的 `base_instance_index`。
   - 在 submit 之间重置 cursor（例如 composite submit 结束后再重置给 view submit）。
2. **每个 pass 单独 submit（止血方案）**
   - 每次 `write_buffer` 后立即 `queue.submit` 对应 encoder，保证写入和读取不会被后续覆盖。
   - 成本较高（submit 次数多），但语义最直观。
3. **使用动态 offset / 多个 buffer**
   - uniform 使用 dynamic offset；
   - storage 使用数组或多 buffer（每 pass 独立绑定区域）。

### 经验规则（避免回归）
- 禁止：在单次 submit 内，通过多次 `queue.write_buffer` 覆盖同一段内存来“给不同 pass 传参”。
- 必须：如果一个 encoder 内有多个 pass 且它们读取同类 per-pass 数据，数据要么在 GPU 侧可索引（数组/offset），要么在 CPU 侧按 offset 分配。
