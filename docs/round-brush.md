# Round 笔刷重构设计

## 目标语义

round 笔刷分为两层：

1. dab 绘制层
2. 最终 merge 层

配置项语义应当是：

- `size` / `radius`：控制笔刷几何尺度
- `spacing_ratio`：控制 dab 采样间距和笔刷尺度的比例关系
- `flow`：控制中间表示中的流量累积
- `hardness`：控制最终笔触从实心条带到柔和边缘的过渡
- `opacity`：控制最终提交到图像时的透明度上限

其中：

- 同一 stroke 内，`flow` 可以叠加
- 同一 stroke 内，`opacity` 不叠加；它只在最终 merge 时作为上限参与融合
- `hardness` 不应当通过“每个 dab 自己变软”直接实现，否则 dab 之间叠加会把边缘推硬

因此 round 笔刷需要的不是“软 dab 直接盖色”，而是：

- apply 阶段先构造一个无上限的累计流量场
- merge 阶段再把累计流量场映射为最终 alpha

## 理论模型

round brush 的 profile 可以拆成三个单变量关系：

- `dab_kernel(q)`：单个 dab 写入 intermediate 时使用的径向核
- `stroke_source(x)`：所有 dab 线性累加后的中间流量图
- `merge_coverage(s)`：把 intermediate source 映射成最终 coverage

`flow` 只缩放 `stroke_source`，`hardness` 只改变 `merge_coverage` 对 source 的解释。

### 1. Apply 阶段

设单个 dab 使用一个紧支撑径向核 `K_dab`，其定义域为归一化半径：

`rho = dist / radius`

其中 `K_dab(rho)` 支撑在 `[0, 1]`。

那么一个 dab 在位置 `c` 处产生的中间流量为：

`source_dab(x) = f_dab * K_dab(|x - c| / radius)`

其中：

- `f_dab` 是该 dab 的真实流量
- `radius` 是笔刷半径

整笔在 intermediate 上的结果是线性累加：

`source(x) = sum_i source_dab_i(x)`

这是一个无上限的累计流量场。

### 2. 直线局部模型

考虑笔刷沿直线匀速前进，dab 中心按固定归一化间距 `sigma` 排列：

`sigma = spacing_px / radius`

则某一点的累计流量是离散和：

`S(d) = flow * sum_{n in Z} K_dab(sqrt(d^2 + (n * sigma)^2))`

其中 `d` 是该点到笔划中心线的归一化垂直距离。

中心最大值不是单个 dab 的 `flow`，而是：

`U = flow * sum_{n in Z} K_dab(|n * sigma|)`

当 `sigma` 足够小、局部曲率不高时，这个离散和可以近似为连续直线积分。这里可以把它看成一个 Abel 型变换。

如果 `K_dab` 选成紧支撑幂函数核，那么：

- `K_dab` 的 Abel 变换 `S(d)` 仍然是同一类幂函数
- `S(d)` 可以用来估计某个几何半径处的累计 source

这正是本设计可行的核心原因。

### 3. Merge 阶段

最终希望得到的并不是累计流量本身，而是一个由 `hardness` 控制的截断式 coverage。

`hardness` 定义截断开始饱和的归一化半径：

- `d <= h` 的区域 source 高于阈值，merge 后 coverage 被截断为 1
- `h < d < 1` 的区域使用 source 的尾部高斜率区线性映射到 coverage
- `d >= 1` 的区域 source 为 0

这里的 `h` 是归一化硬边半径，也就是 `hardness` 参数的几何解释。

如果中间表示构造得当，那么在“局部近似直线、spacing 足够稳定、曲率变化不剧烈”的条件下，`S(h)` 就是这个硬边半径对应的累计 source 阈值：

`threshold = S(h)`

也可以写成：

`threshold = U * K_merge_sigma(h)`

其中：

`K_merge_sigma(h) = S(h) / S(0)`

merge 阶段不做 `S^{-1}` 重映射，而是保留截断行为：

`coverage = clamp(source / threshold, 0, 1)`

`alpha_brush(source) = opacity * coverage`

这一步是整个模型里最关键的设计结论：

- 目标图像并不是在 merge 时重新按曲线几何“现算”出来的
- 而是在 apply 阶段通过 dab 叠加，把几何目标近似编码进 intermediate
- merge 只需要把这个中间表示按 spacing-aware 阈值截断成最终 coverage / alpha
- `size / spacing / flow / hardness` 的几何与叠加语义都被压进了一个单变量 transfer function

## 当前实现约定

当前代码明确采用半径口径：

- `base_radius_px` 表示笔刷半径
- `spacing_px = radius_px * spacing_ratio`
- 因此 `spacing_ratio = 1` 表示中心间距等于半径，而不是“恰好不接触”

这个约定和当前 `round` sampler / merge transfer builder 的实现保持一致。

## 实现建议

### 核函数选择

当前实现采用紧支撑幂函数核，具有良好的 Abel 变换封闭性：

`K_dab(rho) = (1 - rho^2)^a,  rho in [0, 1]`

其 Abel 变换仍然为同一类幂函数：

`S(d) ∝ (1 - d^2)^{a+1/2}`

其中 `a` 是超参数供开发者调整笔刷软边手感，不暴露给用户。

对应的工程实现是：

- apply 阶段使用这个紧支撑、单峰、单调下降的幂函数核
- merge 阶段不做解析反解，而是使用 CPU 预计算得到的固定长度 1D transfer LUT

这样做的好处是：

- 保留紧支撑，便于 tile 局部执行
- 核形状可以后续替换，不会牵动 brush 接口
- merge shader 保持简单，只做一次 texture-like 查表或数组插值

### 推荐的 merge 形式

当前代码里，每笔 stroke 都会根据：

- `spacing_ratio`
- `flow`
- `hardness`

在 CPU 上生成一个单调 LUT：

`lut[i] = clamp(u_i / threshold, 0, 1)`

其中 `u_i` 取自累计流量范围 `[0, U]`，`U` 是直线常参数模型下的中心累计流量最大值，而不是单个 dab 的 `flow`。这点很重要：spacing 越小，中心像素会收到更多 dab 叠加；如果 LUT 仍按 `[0, flow]` 归一化，merge lookup 会过早 clamp，边缘会变硬。

阈值同样不能用单个 dab 的 `flow` 推导，而应使用同一个直线累计模型：

`threshold = S(h) = flow * sum_{n in Z} K_dab(sqrt(h^2 + (n * sigma)^2))`

这保持了当前想要的截断软边：实心区域由阈值决定，软边来自 source 尾部被线性截断的部分，而不是把整条 profile 重映射成目标覆盖曲线。

merge shader 读取 intermediate 的 `source` 后：

1. 归一化到 LUT 定义域
2. 查表得到 `coverage`
3. 输出 `alpha = opacity * coverage`

当前实现使用固定 128 项的 LUT。这样 merge 不需要再次知道曲线几何，也不需要在 shader 中做复杂反演；几何目标已经在 apply 阶段通过叠加模型近似编码进 `source`。

### 为什么推荐 LUT，而不是解析式

原因很直接：

- 真正使用的 `K_dab` 很可能是紧支撑近似核，不是纯高斯
- 离散 spacing 的和也不一定有漂亮闭式
- `hardness` 的目标剖面可能后续继续调整

把这些全部交给解析公式会让 shader 和 CPU 两侧都变脆弱。

而 LUT 方案允许：

- 先得到正确语义
- 后续再逐步替换核函数和反演策略
- 不需要修改 `smoother` / `sampler` / `renderer` 的层次结构
