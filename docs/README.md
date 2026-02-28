# Glaphica 文档导航

> **最后更新**: 2026-02-28
> **文档版本**: 2.0 (统一架构)

---

## 🚀 快速开始

### 新成员必读
1. **[README.md](../README.md)** - 项目简介
2. **[AGENTS.md](../AGENTS.md)** - AI 协作指南
3. **[guides/coding-guidelines.md](guides/coding-guidelines.md)** - 编码规范

### 遇到 Bug？
→ 直接查看 **[Debug Playbook](guides/debug-playbook.md)**

### 添加功能？
→ 先阅读 **[coding-guidelines.md](guides/coding-guidelines.md)** 和对应 crate 的 `AGENTS.md`

---

## 📚 文档分类

### 🏗️ 架构文档 (Architecture)

| 文档 | 用途 |
|------|------|
| [architecture/overview.md](architecture/overview.md) | 系统架构概览 |
| [architecture/structure.md](architecture/structure.md) | 代码结构分析 |
| [architecture/decisions/](architecture/decisions/) | 架构决策记录 (ADRs) |

#### 关键决策记录
- [tile-size-128px.md](architecture/decisions/tile-size-128px.md) - Tile 大小决策 (2026-02-20)
- [brush-merge-lifecycle.md](architecture/decisions/brush-merge-lifecycle.md) - Brush Merge 生命周期 (2026-02-21)
- [message-flow-design.md](architecture/decisions/message-flow-design.md) - 消息流设计 (2026-02-21)

#### Crate 架构文档
- [crates/renderer/DESIGN.md](../crates/renderer/DESIGN.md) - Renderer 设计
- [crates/brush_execution/DESIGN_DECISIONS.md](../crates/brush_execution/DESIGN_DECISIONS.md) - Brush 执行决策日志

### 📖 开发指南 (Guides)

| 文档 | 用途 | 适用场景 |
|------|------|----------|
| [guides/coding-guidelines.md](guides/coding-guidelines.md) | 编码规范 | 所有代码编写 |
| [guides/debug-playbook.md](guides/debug-playbook.md) | 渲染/GPU 问题排查 | 遇到渲染 Bug |
| [guides/wgpu-guide.md](guides/wgpu-guide.md) | GPU 语义陷阱 | GPU 提交/缓冲区问题 |
| [guides/testing.md](guides/testing.md) | 测试模式 | 编写测试 |
| [guides/conventions.md](guides/conventions.md) | 编码约定 | 命名、样式 |
| [guides/refactoring/tiles-model-runtime.md](guides/refactoring/tiles-model-runtime.md) | 重构指南 | Tiles/Model/Runtime 重构 |

### 📋 规划文档 (Planning)

| 文档 | 用途 |
|------|------|
| [planning/project.md](planning/project.md) | 项目说明 |
| [planning/roadmap.md](planning/roadmap.md) | 路线图 |
| [planning/requirements.md](planning/requirements.md) | 需求列表 |
| [planning/state.md](planning/state.md) | 当前状态 |
| [planning/phases/](planning/phases/) | 阶段计划 |

#### 当前阶段：Phase 4 - 双线程架构
- **Phase 4.1**: Channel 基础设施 ✅ 完成
- **Phase 4.2**: Runtime 线程循环 ✅ 完成
- **Phase 4.3**: AppCore 迁移 🔄 进行中
- **Phase 4.4**: 安全与验证 ⏳ 待开始

### 🐛 Debug 记录

| 文档 | 主题 |
|------|------|
| [debug/brush-merge-duplicate-tiles.md](debug/brush-merge-duplicate-tiles.md) | 重复瓦片问题复盘 |

### 📦 归档文档 (Archive)

历史文档已移至 [archive/](archive/) 目录：
- Phase 2 审查记录
- 工具评估报告
- 过期规范

---

## 🔗 按场景查找文档

### 场景 1: 我要写新代码
1. [coding-guidelines.md](guides/coding-guidelines.md) - 编码规范
2. 对应 crate 的 `AGENTS.md` - crate 特定规则
3. [conventions.md](guides/conventions.md) - 命名约定

### 场景 2: 我遇到了渲染 Bug
1. [debug-playbook.md](guides/debug-playbook.md) - 排查流程
2. [wgpu-guide.md](guides/wgpu-guide.md) - GPU 语义陷阱
3. [debug/brush-merge-duplicate-tiles.md](debug/brush-merge-duplicate-tiles.md) - 排查案例

### 场景 3: 我要修改协议类型
1. 对应 crate 的 `AGENTS.md` - 协作规则
2. [architecture/decisions/message-flow-design.md](architecture/decisions/message-flow-design.md) - 消息流设计

### 场景 4: 我要理解架构决策
1. [architecture/overview.md](architecture/overview.md) - 架构概览
2. [architecture/decisions/](architecture/decisions/) - 决策记录
3. Crate 内部的 `DESIGN*.md` 文件

### 场景 5: 我要了解当前进度
1. [planning/state.md](planning/state.md) - 当前状态
2. [planning/roadmap.md](planning/roadmap.md) - 路线图
3. [planning/phases/](planning/phases/) - 阶段计划

---

## 📁 完整目录结构

```
docs/
├── README.md                           # 本文档（导航入口）
├── architecture/                       # 架构文档
│   ├── overview.md                     # 系统架构概览
│   ├── structure.md                    # 代码结构分析
│   ├── decisions/                      # 架构决策记录 (ADRs)
│   │   ├── tile-size-128px.md          # Tile 大小决策
│   │   ├── brush-merge-lifecycle.md    # Brush Merge 生命周期
│   │   └── message-flow-design.md      # 消息流设计
│   └── crates/                         # Crate 架构索引（链接到 crates 内部）
├── guides/                             # 开发指南
│   ├── coding-guidelines.md            # 编码规范
│   ├── debug-playbook.md               # Debug 排查手册
│   ├── wgpu-guide.md                   # GPU 指南
│   ├── testing.md                      # 测试指南
│   ├── conventions.md                  # 编码约定
│   └── refactoring/
│       └── tiles-model-runtime.md      # 重构指南
├── planning/                           # 规划文档
│   ├── project.md                      # 项目说明
│   ├── roadmap.md                      # 路线图
│   ├── requirements.md                 # 需求列表
│   ├── state.md                        # 当前状态
│   └── phases/                         # 阶段计划
│       ├── 04-01-channel-infrastructure/
│       ├── 04-02-runtime-thread-loop/
│       └── 04-03-appcore-migration/
├── debug/                              # Debug 记录
│   └── brush-merge-duplicate-tiles.md  # 重复瓦片问题
└── archive/                            # 历史归档
    ├── README.md                       # 归档说明
    ├── 2026-02-phase2-review/          # Phase 2 审查
    └── tool-evaluations/               # 工具评估
```

### Crate 内部文档（保留原位）

```
crates/
├── renderer/
│   ├── AGENTS.md                       # AI 协作指南
│   ├── DESIGN.md                       # Renderer 设计
│   └── docs/
│       └── merge_ack_integration.md    # Merge ACK 契约
├── render_protocol/
│   └── AGENTS.md                       # 协议协作规则
├── brush_execution/
│   └── DESIGN_DECISIONS.md             # 设计决策日志
└── tiles/
    └── docs/
        ├── api.md                      # API 文档
        └── TODO.md                     # 待办事项
```

---

## 📊 文档统计

| 类别 | 数量 | 位置 |
|------|------|------|
| 架构文档 | 6 | `architecture/` + crates 内部 |
| 开发指南 | 7 | `guides/` |
| 规划文档 | 19 | `planning/` |
| 设计决策 | 4 | `architecture/decisions/` + crates 内部 |
| Debug 记录 | 1 | `debug/` |
| 归档文档 | 6 | `archive/` |
| **总计** | **43** | - |

---

## 🔧 维护指南

### 添加新文档

1. **指南类** → `guides/` 目录
2. **决策记录** → `architecture/decisions/` 目录，命名格式：`YYYY-MM-DD-decision-name.md`
3. **Debug 记录** → `debug/` 目录，命名格式：`issue-name-date.md`
4. **规划文档** → `planning/` 目录

### 更新文档

- 在文档末尾添加 **最后更新** 日期
- 重大变更在文档开头添加 **变更日志**

### 归档文档

```bash
# 创建归档目录
mkdir -p docs/archive/YYYY-MM-topic

# 移动文档（使用 git mv 保留历史）
git mv docs/guides/old-guide.md docs/archive/YYYY-MM-topic/

# 更新本文档的归档列表
```

### Crate 文档处理原则

| 文档类型 | 处理方式 |
|---------|---------|
| `AGENTS.md` | **保留在 crate 根目录** - AI 协作第一接触点 |
| `DESIGN.md` | **保留在 crate 内部** - 在 `architecture/crates/` 创建索引链接 |
| `DESIGN_DECISIONS.md` | **保留在 crate 内部** - 持续更新的决策日志 |
| `docs/api.md` | **保留在 crate 内部** - 符合 Rust 文档惯例 |

---

## ❓ 常见问题

**Q: 我应该把新文档放在哪里？**
- 指南类 → `guides/`
- 决策记录 → `architecture/decisions/`
- Debug 记录 → `debug/`
- 规划 → `planning/`
- 临时/实验 → 先在 PR 中讨论

**Q: 如何找到特定 crate 的文档？**
- 查看 `crates/<crate>/` 目录下的 `AGENTS.md` 或 `DESIGN*.md`
- 或在 [architecture/decisions/](architecture/decisions/) 查找相关决策

**Q: 文档冲突了怎么办？**
- 以 `guides/` 下的文档为准
- 更新旧文档为"已弃用"并指向新文档

**Q: 规划文档为什么在 docs/ 而不是 .planning/?**
- 统一文档访问入口，所有文档在 `docs/` 下
- `.planning/` 目录已迁移到 `docs/planning/`

---

## 🔗 链接检查

确保所有内部链接使用相对路径，格式：
- 同级目录：`[文档名](document.md)`
- 子目录：`[文档名](subdir/document.md)`
- 父目录：`[文档名](../document.md)`
- Crate 内部：`[文档名](../crates/crate-name/FILE.md)`

---

**维护者**: Development Team
**文档规范**: 遵循 [guides/coding-guidelines.md](guides/coding-guidelines.md)
