# 文档统一整理报告

> **报告日期**: 2026-02-28
> **状态**: 设计完成，待执行迁移

---

## 执行摘要

本文档报告完成了对项目文档的全面审计和统一架构设计。

### 审计范围

- ✅ `/docs/` 目录 - 19 个文档
- ✅ `/.planning/` 目录 - 23 个文档
- ✅ `/crates/*/` 目录 - 7 个文档
- ✅ 根目录文档 - 3 个文档

**总计**: 52 个文档文件

---

## 1. 文档审计报告

### 1.1 文档分类统计

| 类别 | 数量 | 位置 |
|------|------|------|
| 架构文档 | 5 | `.planning/codebase/`, `docs/`, `crates/` |
| 开发指南 | 8 | `docs/Instructions/`, `.planning/codebase/` |
| 设计决策 | 6 | `docs/Wiki/`, `crates/brush_execution/` |
| 规划文档 | 23 | `.planning/` |
| Debug 记录 | 1 | `docs/debug/` |
| API/契约 | 4 | `crates/*/docs/` |
| 历史归档 | 6 | `docs/archive/` |
| 其他 | 3 | 根目录 |

### 1.2 问题识别

#### 重复内容
1. **架构描述分散**: `ARCHITECTURE.md`、`STRUCTURE.md`、`README.md` 都包含架构信息
2. **Phase 4 计划多处存在**: `phase4_implementation_plan.md` 与 `.planning/phases/` 内容重叠

#### 链接不一致
- 文档间相对链接使用混合风格
- `docs/README.md` 未覆盖 `.planning/` 内容

#### 过期风险
- `phase4analysis.md` 内容需验证
- `tiles/docs/TODO.md` 需确认状态

---

## 2. 统一文档架构设计

### 2.1 新目录结构

```
docs/
├── README.md                        # 主导航入口（已更新）
├── architecture/                    # 架构文档
│   ├── overview.md                  # 系统架构概览
│   ├── structure.md                 # 代码结构分析
│   ├── decisions/                   # 架构决策记录 (ADRs)
│   │   ├── tile-size-128px.md       # Tile 大小决策
│   │   ├── brush-merge-lifecycle.md # Brush Merge 生命周期
│   │   └── message-flow-design.md   # 消息流设计
│   └── crates/                      # Crate 架构索引
├── guides/                          # 开发指南
│   ├── coding-guidelines.md         # 编码规范
│   ├── debug-playbook.md            # Debug 排查手册
│   ├── wgpu-guide.md                # GPU 指南
│   ├── testing.md                   # 测试指南
│   ├── conventions.md               # 编码约定
│   └── refactoring/                 # 重构指南
│       └── tiles-model-runtime.md   # 重构指南
├── planning/                        # 规划文档
│   ├── project.md                   # 项目说明
│   ├── roadmap.md                   # 路线图
│   ├── requirements.md              # 需求列表
│   ├── state.md                     # 当前状态
│   └── phases/                      # 阶段计划
│       ├── 04-01-channel-infrastructure/
│       ├── 04-02-runtime-thread-loop/
│       └── 04-03-appcore-migration/
├── debug/                           # Debug 记录
│   └── brush-merge-duplicate-tiles.md
└── archive/                         # 历史归档（保持现有）
    ├── README.md
    ├── 2026-02-phase2-review/
    └── tool-evaluations/
```

### 2.2 Crate 内部文档处理原则

| 文档类型 | 处理方式 |
|---------|---------|
| `AGENTS.md` | **保留在 crate 根目录** - AI 协作第一接触点 |
| `DESIGN.md` | **保留在 crate 内部** - 详细设计参考 |
| `DESIGN_DECISIONS.md` | **保留在 crate 内部** - 持续更新的决策日志 |
| `docs/api.md` | **保留在 crate 内部** - 符合 Rust 文档惯例 |
| `docs/TODO.md` | 迁移到 `/docs/planning/` 或转换为 GitHub Issues |

---

## 3. 已完成的更新

### 3.1 文档导航已更新
- ✅ `/home/sunastans/Code/Graphic/glaphica/docs/README.md` - 完整重写为新结构

### 3.2 内部链接已更新
- ✅ `/home/sunastans/Code/Graphic/glaphica/docs/Instructions/debug_playbook.md` - 更新引用链接
- ✅ `/home/sunastans/Code/Graphic/glaphica/docs/Instructions/wgpu.md` - 更新引用链接
- ✅ `/home/sunastans/Code/Graphic/glaphica/AGENTS.md` - 更新项目结构和资源链接
- ✅ `/home/sunastans/Code/Graphic/glaphica/crates/renderer/AGENTS.md` - 更新引用链接

### 3.3 迁移清单已创建
- ✅ `/home/sunastans/Code/Graphic/glaphica/docs/MIGRATION_TODO.md` - 详细迁移步骤

---

## 4. 待执行迁移操作

### 4.1 第一批：架构文档

```bash
git mv .planning/codebase/ARCHITECTURE.md docs/architecture/overview.md
git mv .planning/codebase/STRUCTURE.md docs/architecture/structure.md
git mv docs/Wiki/brush_pipeline_design_decisions_2026-02-20.md docs/architecture/decisions/tile-size-128px.md
git mv docs/Wiki/brush_merge_lifecycle_decisions_2026-02-21.md docs/architecture/decisions/brush-merge-lifecycle.md
git mv docs/Wiki/merge_message_flow_decisions_2026-02-21.md docs/architecture/decisions/message-flow-design.md
```

### 4.2 第二批：开发指南

```bash
git mv .planning/codebase/CONVENTIONS.md docs/guides/conventions.md
git mv .planning/codebase/TESTING.md docs/guides/testing.md
git mv docs/Instructions/coding_guidelines.md docs/guides/coding-guidelines.md
git mv docs/Instructions/debug_playbook.md docs/guides/debug-playbook.md
git mv docs/Instructions/wgpu.md docs/guides/wgpu-guide.md
git mv docs/Instructions/app_core_error_design.md docs/guides/app-core-error-design.md
```

### 4.3 第三批：规划文档

```bash
git mv .planning/PROJECT.md docs/planning/project.md
git mv .planning/ROADMAP.md docs/planning/roadmap.md
git mv .planning/REQUIREMENTS.md docs/planning/requirements.md
git mv .planning/STATE.md docs/planning/state.md
git mv .planning/phases/04-01-channel-infrastructure docs/planning/phases/
git mv .planning/phases/04-02-runtime-thread-loop docs/planning/phases/
git mv .planning/phases/04-03-appcore-migration docs/planning/phases/
git mv .planning/phases/4.2-runtime-thread-loop docs/planning/phases/
git mv .planning/todos docs/planning/todos
```

### 4.4 第四批：重构指南

```bash
git mv docs/Instructions/tiles_model_runtime_refactor_guide.md docs/guides/refactoring/tiles-model-runtime.md
git mv docs/Instructions/tiles_model_runtime_refactor_guide_review_1.md docs/guides/refactoring/tiles-model-runtime-review-1.md
git mv docs/Instructions/phase4_implementation_plan.md docs/guides/refactoring/phase4-implementation-plan.md
```

### 4.5 第五批：Debug 文档

```bash
git mv docs/debug/brush_merge_duplicate_tiles_2026-02-23.md docs/debug/brush-merge-duplicate-tiles.md
```

### 4.6 第六批：清理

```bash
# 清理空目录（手动验证后执行）
rmdir docs/Wiki
rmdir docs/Instructions
rmdir .planning/codebase
rmdir .planning/phases
rmdir .planning/todos
rmdir .planning
```

---

## 5. 验证步骤

### 5.1 检查 git 状态
```bash
git status
```
确认所有移动显示为 `renamed` 而非 `deleted` + `new file`。

### 5.2 检查断裂链接
```bash
grep -r "docs/Wiki" docs/ crates/
grep -r "docs/Instructions" docs/ crates/
grep -r "\.planning/" docs/ crates/
```

### 5.3 验证导航
打开 `docs/README.md` 验证所有链接可正常访问。

---

## 6. 后续维护建议

### 6.1 文档添加流程

新增文档时：
1. 根据内容选择目录（`guides/` / `architecture/` / `planning/` / `debug/`）
2. 在 `docs/README.md` 添加链接
3. 如替代旧文档，将旧文档移至 `archive/`

### 6.2 文档命名规范

| 类型 | 命名格式 | 示例 |
|------|---------|------|
| 指南 | `kebab-case.md` | `coding-guidelines.md` |
| 决策记录 | `YYYY-MM-DD-topic.md` | `2026-02-20-tile-size-128px.md` |
| Debug 记录 | `topic-date.md` | `brush-merge-duplicate-tiles.md` |
| 规划文档 | `kebab-case.md` | `project.md` |

### 6.3 定期归档

建议每 3 个月检查一次：
- 确认是否有文档可以永久删除
- 确认是否有文档需要恢复为活跃状态
- 更新归档索引

---

## 7. 文档依赖关系图

```
AGENTS.md (根目录)
├── docs/guides/coding-guidelines.md
├── docs/guides/debug-playbook.md
├── docs/guides/wgpu-guide.md
└── crates/*/AGENTS.md

docs/guides/debug-playbook.md
├── docs/debug/brush-merge-duplicate-tiles.md
└── docs/guides/wgpu-guide.md

crates/renderer/AGENTS.md
├── docs/guides/debug-playbook.md
├── docs/guides/wgpu-guide.md
├── crates/renderer/DESIGN.md
└── crates/renderer/docs/merge_ack_integration.md
```

---

## 8. 最终检查清单

- [x] 文档审计报告完成
- [x] 统一架构设计完成
- [x] 主导航文档已更新 (`docs/README.md`)
- [x] 内部链接已更新 (4 个文件)
- [x] 迁移清单已创建 (`docs/MIGRATION_TODO.md`)
- [ ] 待执行 `git mv` 迁移操作（需 Bash 权限）
- [ ] 待验证所有链接
- [ ] 待清理空目录

---

**报告生成**: 2026-02-28
**下一步**: 执行 `docs/MIGRATION_TODO.md` 中的迁移命令
