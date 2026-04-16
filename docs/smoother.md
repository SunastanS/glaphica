# 问题表述：

笔刷流水线的运行大致可以表述成：

屏幕输入 -> 平滑和重采样 -> 笔刷执行器 -> 渲染器

当前关注平滑和重采样模块到笔刷执行器模块的接口设计，其核心需求包括：
- 二者应该相对解耦，不同的平滑方案和笔刷之间应该能自由排列组合
- 平滑是在滑动窗口上进行的，已经被渲染到屏或者已经被消费的曲线区间不应该因新采样点的加入而改变
- 由于运行在笔刷热路径上，需要性能足够高
- 尽可能包含笔刷运行所需的数据，也就是说尽可能是个参数曲线 \(\alpha\to (x,y,t,\theta,\omega)\) 包含运动学参数和时间参数
- 笔刷执行器应该能按照具体笔刷的需要获得能满足各种需求的 dab 列表，例如
  - Round 笔刷需要按曲线路程均匀分布的 dab 列表，并且间距由笔刷指定
  - 平笔刷需要一列点和其对应的曲线切方向
  - 某些扭曲笔刷，例如膨胀/收缩，在笔刷固定一点时持续发挥作用
  - 但是绘制笔刷在光标位置不同时一般不持续绘制

因此这里的核心问题是需要一套即能保证模块解耦，又足够通用的曲线表示

# 接口形式

把接口定义成一串 **committed spans**，而不是一串 dab。

数学上写成：

[
\gamma_i:u\in[0,1]\mapsto (x_i(u),y_i(u),\tau_i(u)),\qquad \tau_i'(u)>0
]

工程上更简单的版本是直接存一段时间参数曲线：

[
p_i(t)=(x_i(t),y_i(t)),\qquad t\in[t_i,t_{i+1}]
]

第一版用 cubic Hermite 最自然：

[
p_i(u)=h_{00}(u)p_i+h_{10}(u)\Delta t_i v_i+h_{01}(u)p_{i+1}+h_{11}(u)\Delta t_i v_{i+1}
]

其中 (p_i) 是冻结后的平滑点，(v_i) 是冻结后的速度估计。
这里 **接口的一等公民不要直接是 ((x,y,t,\theta,\omega))**，而应该是：

[
(x,y,t,\dot x,\dot y,\ddot x,\ddot y)
]

然后把

[
\theta=\operatorname{atan2}(\dot y,\dot x),\qquad
\omega=\frac{\dot x\ddot y-\dot y\ddot x}{\dot x^2+\dot y^2}
]

作为惰性导出。这样比直接存 (\theta) 更稳，也更通用；很多 brush 其实只需要单位切向，不需要每次都做 `atan2`。

停笔时不要把数据“吞掉”，而是允许存在 **(\Delta s=0,\ \Delta t>0)** 的 stationary span。普通 drawing brush 的 (ds)-sampler 会自然忽略它；驻留型 brush 的 (dt)-sampler 会持续在同一点出 dab。这样两类 brush 才能在同一表示上自然共存。

# brush 侧怎么解耦

同一条 committed curve 上提供不同 sampler 即可：

* `sample_by_arclength(Δs)`：round brush
* `sample_by_arclength_with_frame(Δs)`：flat brush，顺手拿切向/法向
* `sample_by_time(Δt)`：膨胀/收缩、喷枪、驻留型效果
* `sample_stationary_only(Δt, v<ε)`：只在停留时持续作用

也就是说，**不要让 smoother 直接输出“统一 dab 列表”**。它应该输出“可查询曲线”；dab 生成策略属于 brush 侧。
