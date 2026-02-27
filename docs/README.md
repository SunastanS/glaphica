# Glaphica 文档导航

> **最后更新**: 2026-02-27  
> **文档总数**: 24 个活跃文档

---

## 🚀 快速开始

### 新成员必读
1. **[README.md](../README.md)** - 项目简介
2. **[AGENTS.md](../AGENTS.md)** - AI 协作指南
3. **[Instructions/coding_guidelines.md](Instructions/coding_guidelines.md)** - 编码规范

### 遇到 Bug？
→ 直接查看 **[Debug Playbook](Instructions/debug_playbook.md)**

### 添加功能？
→ 先阅读 **[coding_guidelines.md](Instructions/coding_guidelines.md)** 和对应 crate 的 `AGENTS.md`

---

## 📚 文档分类

### 🔧 核心指南（Instructions）

| 文档 | 用途 | 适用场景 |
|------|------|----------|
| [coding_guidelines.md](Instructions/coding_guidelines.md) | 编码规范 | 所有代码编写 |
| [debug_playbook.md](Instructions/debug_playbook.md) | 渲染/GPU 问题排查 | 遇到渲染 Bug |
| [wgpu.md](Instructions/wgpu.md) | GPU 语义陷阱 | GPU 提交/缓冲区问题 |
| [app_core_error_design.md](Instructions/app_core_error_design.md) | 错误处理设计 | AppCore 错误迁移 |
| [tiles_model_runtime_refactor_guide.md](Instructions/tiles_model_runtime_refactor_guide.md) | 重构指南 | Tiles/Model/Runtime 重构 |

### 🏗️ 架构设计（Architecture）

#### Crate 特定指南
| Crate | 文档 |
|-------|------|
| `renderer` | [AGENTS.md](../crates/renderer/AGENTS.md), [DESIGN.md](../crates/renderer/DESIGN.md) |
| `render_protocol` | [AGENTS.md](../crates/render_protocol/AGENTS.md) |
| `brush_execution` | [DESIGN_DECISIONS.md](../crates/brush_execution/DESIGN_DECISIONS.md) |
| `tiles` | [API.md](../crates/tiles/docs/api.md), [TODO.md](../crates/tiles/docs/TODO.md) |

#### 设计决策记录（Wiki）
| 文档 | 日期 | 主题 |
|------|------|------|
| [brush_merge_lifecycle_decisions_2026-02-21.md](Wiki/brush_merge_lifecycle_decisions_2026-02-21.md) | 2026-02-21 | Brush Merge 生命周期 |
| [merge_message_flow_decisions_2026-02-21.md](Wiki/merge_message_flow_decisions_2026-02-21.md) | 2026-02-21 | 消息流设计 |
| [brush_pipeline_design_decisions_2026-02-20.md](Wiki/brush_pipeline_design_decisions_2026-02-20.md) | 2026-02-20 | Tile 大小决策 |

### 🐛 Debug 记录
| 文档 | 主题 |
|------|------|
| [debug/brush_merge_duplicate_tiles_2026-02-23.md](debug/brush_merge_duplicate_tiles_2026-02-23.md) | 重复瓦片问题复盘 |

### 🔍 Merge 集成文档
| 文档 | 用途 |
|------|------|
| [crates/renderer/docs/merge_ack_integration.md](../crates/renderer/docs/merge_ack_integration.md) | Merge ACK 契约 |

---

## 🗂️ 目录结构

```
docs/
├── README.md                           # 本文档（导航入口）
├── Instructions/                       # 核心指南
│   ├── coding_guidelines.md
│   ├── debug_playbook.md
│   ├── wgpu.md
│   ├── app_core_error_design.md
│   ├── tiles_model_runtime_refactor_guide.md
│   └── tiles_model_runtime_refactor_guide_review_1.md
├── Wiki/                               # 设计决策记录
│   ├── brush_merge_lifecycle_decisions_2026-02-21.md
│   ├── merge_message_flow_decisions_2026-02-21.md
│   └── brush_pipeline_design_decisions_2026-02-20.md
├── debug/                              # Debug 记录
│   └── brush_merge_duplicate_tiles_2026-02-23.md
└── archive/                            # 历史归档
    ├── 2026-02-phase2-review/          # Phase 2 审查记录
    └── tool-evaluations/               # 工具评估报告
```

---

## 🔗 文档依赖关系

```
AGENTS.md
├── Instructions/coding_guidelines.md
├── Instructions/debug_playbook.md
├── Instructions/wgpu.md
├── crates/renderer/DESIGN.md
└── crates/renderer/docs/merge_ack_integration.md

Instructions/debug_playbook.md
├── debug/brush_merge_duplicate_tiles_2026-02-23.md
└── Instructions/wgpu.md

crates/renderer/AGENTS.md
├── Instructions/debug_playbook.md
├── Instructions/wgpu.md
├── crates/renderer/DESIGN.md
└── crates/renderer/docs/merge_ack_integration.md
```

---

## 📦 归档文档

历史文档已移至 `archive/` 目录：

### Phase 2 审查（2026-02）
- `archive/2026-02-phase2-review/phase2_review_response.md`
- `archive/2026-02-phase2-review/phase2_completion_summary.md`

### 工具评估
- `archive/tool-evaluations/refactor_automation_experience.md` (Comby 经验)
- `archive/tool-evaluations/comby_rust_support_test_report.md` (Comby 测试)

### 其他
- `archive/Review.md` (Code Review 角色定义，未使用)

---

## 🎯 按场景查找文档

### 场景 1: 我要写新代码
1. [coding_guidelines.md](Instructions/coding_guidelines.md) - 编码规范
2. 对应 crate 的 `AGENTS.md` - crate 特定规则
3. [render_protocol/AGENTS.md](../crates/render_protocol/AGENTS.md) - 协议修改规则

### 场景 2: 我遇到了渲染 Bug
1. [debug_playbook.md](Instructions/debug_playbook.md) - 排查流程
2. [wgpu.md](Instructions/wgpu.md) - GPU 语义陷阱
3. [debug/brush_merge_duplicate_tiles_2026-02-23.md](debug/brush_merge_duplicate_tiles_2026-02-23.md) - 排查案例

### 场景 3: 我要修改协议类型
1. [render_protocol/AGENTS.md](../crates/render_protocol/AGENTS.md) - 协作规则
2. [merge_ack_integration.md](../crates/renderer/docs/merge_ack_integration.md) - ACK 契约

### 场景 4: 我要理解架构决策
1. [Wiki/](Wiki/) - 设计决策记录
2. [crates/brush_execution/DESIGN_DECISIONS.md](../crates/brush_execution/DESIGN_DECISIONS.md) - Brush 决策日志
3. [crates/renderer/DESIGN.md](../crates/renderer/DESIGN.md) - Renderer 设计

### 场景 5: 我要参与重构
1. [tiles_model_runtime_refactor_guide.md](Instructions/tiles_model_runtime_refactor_guide.md) - 重构指南
2. [app_core_error_design.md](Instructions/app_core_error_design.md) - 错误处理设计

---

## 📊 文档统计

| 类别 | 数量 | 位置 |
|------|------|------|
| 核心指南 | 5 | `Instructions/` |
| Crate 文档 | 6 | `crates/*/` |
| 设计决策 | 4 | `Wiki/` + `crates/` |
| Debug 记录 | 1 | `debug/` |
| 归档文档 | 5 | `archive/` |
| **总计** | **24** | - |

---

## 🔧 维护指南

### 添加新文档
1. 根据内容选择合适目录（`Instructions/` / `Wiki/` / `debug/`）
2. 在本文档添加链接
3. 如替代旧文档，将旧文档移至 `archive/`

### 更新文档
- 在文档末尾添加 **最后更新** 日期
- 重大变更在文档开头添加 **变更日志**

### 归档文档
```bash
# 创建归档目录（如需要）
mkdir -p docs/archive/YYYY-MM-topic

# 移动文档
mv docs/Instructions/old-doc.md docs/archive/YYYY-MM-topic/

# 更新本文档的归档列表
```

---

## ❓ 常见问题

**Q: 我应该把新文档放在哪里？**
- 指南类 → `Instructions/`
- 决策记录 → `Wiki/`
- Debug 记录 → `debug/`
- 临时/实验 → 先在 PR 中讨论

**Q: 如何找到特定 crate 的文档？**
- 查看 `crates/<crate>/` 目录下的 `AGENTS.md` 或 `DESIGN*.md`

**Q: 文档冲突了怎么办？**
- 以 `Instructions/` 下的文档为准
- 更新旧文档为"已弃用"并指向新文档

---

**维护者**: Development Team  
**文档规范**: 遵循 `Instructions/coding_guidelines.md`
