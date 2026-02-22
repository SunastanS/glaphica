# Brush/Merge 重复瓦片问题复盘（2026-02-23）

## 背景与最终结论

问题表现：
- 一笔绘制后，多处 tile 出现重复图像。
- 后续确认重复内容不仅包含新笔画，还包含下层原有图像，呈现“整块瓦片被贴到很多地方”。

最终确认的核心根因有两类：
1. 输入坐标链路问题：driver 直接把 `screen_x/screen_y` 当作 `canvas_x/canvas_y`，导致缩放/平移/旋转后笔画影响区域与预期不一致。
2. merge 编码问题：merge 在单次提交中循环覆写同一 uniform buffer 偏移，再统一 submit，导致多个 merge op 读到相同（通常是最后一次）uniform，视觉上像“所有受影响 tile 都变成同一个 tile”。

---

## 时间线与排查路径

### 1) 先修复可见性与基础稳定性

- 将默认笔刷从单像素写入改为固定半径写入（提高肉眼可观测性）。
- 增加 WGSL loadable 测试，防止 shader 基本错误在运行期才暴露。
- 修复 WGSL 保留字冲突（`meta`）。

关键经验：
- 在图像问题排查前先提高可视性，否则误差太小会误导判断。
- WGSL 语法/保留字错误必须在构建/测试阶段暴露，而不是在 merge 运行期用 GPU validation 才发现。

### 2) 第一轮假设：地址解析冲突（失败）

假设：
- 不同 key 被解析到同一 atlas 地址。

动作：
- 在 `DocumentRenderDataResolver` 增加 debug 断言：
  - 不同 `TileKey` 不得解析到同一 `TileAddress`。

结果：
- 可复现但不崩溃，排除该路径。

结论：
- 不是“key->address 冲突”，更可能是“多地址内容相同”或“渲染阶段状态复用”。

### 3) 第二轮假设：映射集合污染（部分排除）

增加断言：
- `stroke_buffer_key` 不能绑定到多个 tile 坐标。
- merge plan 中输出坐标不可重复。
- draw instances 内部坐标不可重复（leaf cache / effective_instances）。
- 同一 image 中同一 `TileKey` 不能出现在多个坐标。
- `dirty_tiles` 与 `new_key_mappings` 坐标集合必须完全一致。
- group cache image 不允许同 key 多坐标复用。

结果：
- 依然可复现但不崩溃。

结论：
- 说明数据结构层面的明显重复/冲突不是主因，问题更偏“坐标空间”与“GPU 提交状态”。

### 4) 第三轮定位：输入坐标空间（命中）

线索：
- 不缩放时受影响 tile 与光标经过 tile 基本一致，缩放/平移后偏差明显。

定位：
- `driver` 采样算法中 `canvas_x = input.screen_x`, `canvas_y = input.screen_y`。
- UI 输入进入 driver 前未做 view 逆变换。

修复：
- 在 `view` 增加 `screen_to_canvas_point`。
- 在 `glaphica` 输入入口统一先做 `screen -> canvas`，再喂给 driver。
- 在 `GLAPHICA_BRUSH_TRACE=1` 下打印 `phase + screen + canvas`。

效果：
- 受影响 tile 与光标轨迹显著对齐。

### 5) 第四轮定位：merge uniform 复用（命中并修复）

线索：
- 所有受影响 tile 看起来是同一个 tile 内容。

定位：
- merge 编码中每个 op 都写同一 uniform buffer 偏移 `0`，最终单次 submit。
- GPU 执行时多个 pass 可能读到同一个（最后一次）uniform 值。

修复策略（先保证正确性）：
- 每个 merge op 单独：
  - 写 uniform
  - 编码 command buffer
  - `queue.submit`

效果：
- “所有受影响 tile 变同一 tile”问题消失。

---

## 调试技巧沉淀

1. 先提高观测信号强度
- 把不可见/难见的现象先放大（如笔刷半径），再做逻辑排查。

2. 用“可证伪断言”快速收敛
- 每条假设都对应一条硬断言，不要只靠日志猜。
- 断言要放在数据流关键边界：输入映射、merge plan、resolver、draw submit。

3. 区分两类重复现象
- `key/address` 重复：映射错误。
- 内容重复但 key/address 不重复：状态复用、坐标空间错误、采样参数错误。

4. 注意 GPU 命令与 CPU 写入时序语义
- “循环写同一 uniform + 一次性提交”是高风险模式。
- 若无 ring buffer/dynamic offset 管理，优先保证每 op 状态隔离。

5. 输入系统必须定义清晰坐标契约
- driver 接收的是 canvas 坐标还是 screen 坐标必须明确且单一。
- 视图变换存在时，入口必须做逆变换。

---

## 本次新增排查开关与观测点

- `GLAPHICA_BRUSH_TRACE=1`
  - shader/pipeline 初始化校验日志
  - merge 提交过程关键信息
  - pointer `screen -> canvas` 映射日志

建议：
- 后续可新增专门的 `GLAPHICA_MERGE_TRACE=1`，将 merge op 的 base/stroke/output 三元组和实际 dirty tile 集合按 receipt 输出，降低混杂日志噪声。

---

## 后续防回归建议

1. 为 merge 编码增加回归测试
- 构造多个 op（不同 output tile），校验输出 tile 内容不会被最后一个 op 覆盖。

2. 为 view 坐标链路增加端到端测试
- 在非 1.0 zoom、非零 pan、非零 rotation 下输入同一屏幕轨迹，验证落点 canvas 坐标符合逆变换预期。

3. 将关键断言保留在 debug 构建
- 尤其是：
  - image key 唯一坐标
  - draw instances 唯一 tile 坐标
  - merge dirty/mapping 集合一致性

