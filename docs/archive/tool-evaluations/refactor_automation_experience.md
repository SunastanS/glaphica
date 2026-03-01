# 重构自动化经验总结

## Phase 2: GpuState 拆分重构经验

**执行时间**: 2026-02-27  
**重构工具**: Comby 1.8.2  
**重构规模**: 65+ 处字段访问替换，4 个核心模块创建

---

## 一、重构目标与范围

### 目标
将 `GpuState` 的业务逻辑委托给新创建的 `AppCore`，实现关注点分离：
- **AppCore**: 业务编排（document, merge, brush state）
- **GpuRuntime**: GPU 资源管理（renderer, atlas, surface）
- **GpuState**: 薄 facade，委托所有操作

### 范围
- 创建 `crates/glaphica/src/runtime/` (191 行)
- 创建 `crates/glaphica/src/app_core/` (528 行)
- 迁移 4 条主要路径到命令接口
- 委托 65+ 处字段访问

---

## 二、Comby 工具使用经验

### 2.1 工具选择对比

| 工具 | 优点 | 缺点 | 适用场景 |
|------|------|------|----------|
| **Comby** | 理解代码结构，安全 | 学习曲线，容器运行 | 大规模结构重构 |
| **sed** | 快速，简单 | 破坏性，易出错 | 简单文本替换 |
| **手动** | 精确控制 | 耗时，易疲劳 | 小范围修改 |

### 2.2 Comby 命令模式

**基本语法**:
```bash
podman run --rm --userns=keep-id -v $(pwd):/src:Z -w /src \
  comby/comby 'PATTERN' 'REPLACEMENT' FILE
```

**关键参数**:
- `-v $(pwd):/src:Z`: 挂载当前目录（SELinux 上下文）
- `--userns=keep-id`: 保持用户权限
- `-w /src`: 设置工作目录

### 2.3 成功替换模式

#### 模式 1: 方法调用链替换
```bash
# self.renderer.method() → self.core.runtime().renderer().method()
podman run ... comby/comby \
  'self.renderer' \
  'self.core.runtime().renderer()' \
  crates/glaphica/src/lib.rs
```

**效果**: 14 处替换，编译通过 ✅

#### 模式 2: 字段访问替换
```bash
# self.document → self.core.document()
podman run ... comby/comby \
  'self.document' \
  'self.core.document()' \
  crates/glaphica/src/lib.rs
```

**效果**: 2 处替换，编译通过 ✅

#### 模式 3: 可变借用修复
```bash
# self.brush_execution_feedback_queue → self.core.brush_execution_feedback_queue_mut()
podman run ... comby/comby \
  'self.brush_execution_feedback_queue' \
  'self.core.brush_execution_feedback_queue_mut()' \
  crates/glaphica/src/lib.rs
```

**效果**: 8 处替换，编译通过 ✅

### 2.4 遇到的陷阱与解决方案

#### 陷阱 1: 赋值语句的特殊处理

**问题代码**:
```rust
self.surface_size = PhysicalSize::new(width, height);
```

**错误替换**:
```rust
// ❌ 编译失败
self.core.runtime().surface_size() = PhysicalSize::new(width, height);
```

**原因**: `surface_size()` 返回 `&PhysicalSize`，不是 `&mut`

**解决方案**:
```rust
// ✅ 方案 A: 添加 setter 方法
self.core.runtime_mut().set_surface_size(PhysicalSize::new(width, height));

// ✅ 方案 B: 使用内部可变性
self.core.runtime().surface_size.set(...);

// ✅ 方案 C: 直接访问字段（如果可见性允许）
self.core.runtime_mut().surface_size = ...;
```

**教训**: 对于赋值操作，需要先分析目标字段的可变性，可能需要添加专门的 setter 方法。

#### 陷阱 2: 借用检查器冲突

**问题代码**:
```rust
let size = self.surface_size;  // 不可变借用
self.renderer.resize(...);     // 可变借用
```

**错误替换**:
```rust
// ❌ 编译失败
let size = self.core.runtime().surface_size();  // 不可变借用 runtime
self.core.runtime().renderer().resize(...);     // 可变借用 runtime - 冲突！
```

**解决方案**:
```rust
// ✅ 分离借用
let size = self.core.runtime().surface_size();
self.core.runtime_mut().renderer_mut().resize(...);
```

**教训**: Comby 无法自动处理借用检查器问题，需要手动审查和修复。

### 2.5 性能数据

**重构统计**:
- **总耗时**: 20 分钟（使用 Comby）
- **替换次数**: 65+ 处
- **编译迭代**: 5 次
- **手动修复**: 3 处（赋值语句、借用冲突）

**对比估算**:
- **手动重构**: 2-3 小时
- **sed 重构**: 1 小时 + 大量修复时间
- **Comby 重构**: 20 分钟 ✅

**ROI**: 节省约 100-160 分钟

---

## 三、重构最佳实践

### 3.1 准备工作

1. **确保编译基线干净**
   ```bash
   cargo check --workspace
   # 确保无错误
   ```

2. **创建 Git 提交点**
   ```bash
   git add -A && git commit -m "Pre-refactor baseline"
   ```

3. **备份关键文件**
   ```bash
   cp lib.rs lib.rs.backup
   ```

### 3.2 分阶段执行

**阶段 1: 简单字段访问（低风险）**
```bash
# 先替换不 involvement 赋值的字段
comby 'self.document' 'self.core.document()'
comby 'self.tile_merge_engine' 'self.core.tile_merge_engine()'
cargo check  # 验证
```

**阶段 2: 方法调用链（中风险）**
```bash
comby 'self.renderer' 'self.core.runtime().renderer()'
comby 'self.view_sender' 'self.core.runtime().view_sender()'
cargo check  # 验证
```

**阶段 3: 可变借用（高风险）**
```bash
comby 'self.brush_execution_feedback_queue' 'self.core.brush_execution_feedback_queue_mut()'
cargo check  # 验证
```

**阶段 4: 特殊处理（手动）**
```bash
# 赋值语句、借用冲突等手动修复
```

### 3.3 验证策略

**每次替换后**:
```bash
cargo check -p glaphica
# 只检查修改的 crate，快速反馈
```

**阶段完成后**:
```bash
cargo check --workspace
# 完整验证
```

**最终验证**:
```bash
cargo test -p glaphica --lib
cargo clippy
```

### 3.4 回滚策略

**如果编译失败**:
```bash
# 快速回滚
git restore crates/glaphica/src/lib.rs

# 或者撤销最近的 comby 更改
podman run ... comby/comby \
  'self.core.runtime().renderer()' \
  'self.renderer' \
  crates/glaphica/src/lib.rs
```

---

## 四、架构决策记录

### 4.1 为什么选择 AppCore + GpuRuntime 架构

**问题**: `GpuState` 承担了太多职责（God Object）

**解决方案**: 分离关注点
```
GpuState (facade)
  ├─ AppCore (业务逻辑)
  │   ├─ Document
  │   ├─ TileMergeEngine
  │   └─ Brush state
  │
  └─ GpuRuntime (GPU 资源)
      ├─ Renderer
      ├─ Atlas stores
      └─ Surface
```

**优点**:
- 清晰的职责边界
- 便于未来引入线程隔离
- 易于测试和维护

### 4.2 为什么使用命令接口

**直接调用 vs 命令接口**:

| 方案 | 优点 | 缺点 |
|------|------|------|
| 直接调用 | 简单，性能略好 | 紧耦合，难测试 |
| 命令接口 | 解耦，易测试，易扩展 | 略微复杂 |

**选择命令接口**的理由:
1. 为未来线程隔离做准备
2. 清晰的调用边界
3. 便于添加日志和监控

### 4.3 粗粒度命令设计

**命令类型**:
```rust
enum RuntimeCommand {
    PresentFrame { frame_id: u64 },           // 一帧一次
    Resize { width, height, view_transform }, // 窗口调整
    EnqueueBrushCommand { command },          // 每个 brush 命令
    ProcessMergeCompletions { frame_id },     // 一帧一次
}
```

**设计原则**:
- 一帧一个主要命令（PresentFrame, ProcessMergeCompletions）
- 高频操作单独命令（EnqueueBrushCommand）
- 参数自包含，减少上下文依赖

---

## 五、常见问题 FAQ

### Q1: Comby 和 sed 哪个更好？

**A**: 取决于场景：
- **Comby**: 适合结构感知替换（理解语法）
- **sed**: 适合简单文本替换（不理解语法）

**推荐**: 优先尝试 Comby，失败时用 sed 补充。

### Q2: 如何处理借用检查器错误？

**A**: 分阶段处理：
1. 先用 Comby 完成所有替换
2. 运行 `cargo check` 收集所有借用错误
3. 批量修复同类错误（如 `runtime()` → `runtime_mut()`）

### Q3: 重构过程中如何保持可工作状态？

**A**: 小步提交策略：
```bash
# 每完成一类替换
comby 'pattern' 'replacement' file
cargo check
git add file && git commit -m "Refactor: pattern → replacement"
```

### Q4: 如何验证重构没有改变行为？

**A**: 三重验证：
1. **编译验证**: `cargo check --workspace`
2. **测试验证**: `cargo test --workspace`
3. **日志对比**: 对比重构前后的日志输出

---

## 六、工具配置推荐

### 6.1 Podman 配置

**别名**（添加到 `~/.bashrc`）:
```bash
alias comby='podman run --rm --userns=keep-id -v $(pwd):/src:Z -w /src comby/comby'
```

**使用**:
```bash
comby 'pattern' 'replacement' file.rs
```

### 6.2 Git 配置

**重构专用分支**:
```bash
git checkout -b refactor/phase2-gpu-state-split
```

**提交模板**:
```bash
git commit -m "refactor: <scope> - <description>

- Changed: <what changed>
- Reason: <why changed>
- Verified: <how verified>"
```

### 6.3 编辑器配置

**VS Code 插件**:
- Rust Analyzer（代码理解）
- Better TOML（配置编辑）
- GitLens（变更追踪）

---

## 七、后续改进建议

### 7.1 自动化测试生成

**问题**: 重构后手动编写回归测试耗时

**建议**: 开发工具自动生成回归测试
```bash
# 假想工具
cargo refactor generate-tests --before commit1 --after commit2
```

### 7.2 借用检查辅助

**问题**: Comby 无法自动处理借用检查器

**建议**: 开发 Rust 专用的借用感知重构工具
```rust
// 假想工具能识别：
let x = self.field;     // 不可变借用
self.mutate();          // 可变借用 - 冲突！

// 并建议修复：
let x = self.core.field();
self.core_mut().mutate();
```

### 7.3 增量重构框架

**问题**: 大规模重构难以回滚

**建议**: 开发增量重构框架
```rust
// 支持渐进迁移
struct GpuState {
    // 新旧字段共存
    old_renderer: Option<Renderer>,
    new_core: Option<AppCore>,
}

impl GpuState {
    fn resize(&mut self, size: PhysicalSize) {
        // 根据迁移状态选择实现
        if self.new_core.is_some() {
            self.new_core.resize(size);
        } else {
            self.old_resize(size);
        }
    }
}
```

---

## 八、总结

### 关键成功因素

1. **选择合适的工具**: Comby 适合结构感知替换
2. **分阶段执行**: 小步验证，降低风险
3. **频繁验证**: 每次替换后编译检查
4. **回滚策略**: 随时可以回退到安全状态

### 主要教训

1. **赋值操作需要特殊处理**: 不能简单替换字段访问
2. **借用检查器需要手动处理**: Comby 无法自动解决
3. **测试覆盖很重要**: 有测试的重构更安心

### 可复用的模式

1. **字段访问委托**: `self.field` → `self.core.field()`
2. **方法调用链**: `self.a.b()` → `self.core.a().b()`
3. **可变借用**: `self.queue` → `self.core.queue_mut()`

---

**文档版本**: 1.0  
**最后更新**: 2026-02-27  
**维护者**: Development Team
