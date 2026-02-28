# 文档迁移执行清单

> **创建日期**: 2026-02-28
> **状态**: 待执行

本文档列出统一文档架构需要执行的迁移操作。

## 迁移原则

1. **使用 `git mv`** - 保留 git 历史记录
2. **分批执行** - 按文档类别分组迁移
3. **验证链接** - 每批迁移后检查文档链接

## 第一批：架构文档迁移

```bash
# 架构概览和结构
git mv .planning/codebase/ARCHITECTURE.md docs/architecture/overview.md
git mv .planning/codebase/STRUCTURE.md docs/architecture/structure.md

# 决策记录（从 Wiki 迁移）
git mv docs/Wiki/brush_pipeline_design_decisions_2026-02-20.md docs/architecture/decisions/tile-size-128px.md
git mv docs/Wiki/brush_merge_lifecycle_decisions_2026-02-21.md docs/architecture/decisions/brush-merge-lifecycle.md
git mv docs/Wiki/merge_message_flow_decisions_2026-02-21.md docs/architecture/decisions/message-flow-design.md
```

## 第二批：开发指南迁移

```bash
# 指南和约定
git mv .planning/codebase/CONVENTIONS.md docs/guides/conventions.md
git mv .planning/codebase/TESTING.md docs/guides/testing.md

# 重构指南（从 Instructions 迁移）
git mv docs/Instructions/tiles_model_runtime_refactor_guide.md docs/guides/refactoring/tiles-model-runtime.md
git mv docs/Instructions/tiles_model_runtime_refactor_guide_review_1.md docs/guides/refactoring/tiles-model-runtime-review-1.md
git mv docs/Instructions/phase4_implementation_plan.md docs/guides/refactoring/phase4-implementation-plan.md
```

## 第三批：规划文档迁移

```bash
# 规划根目录文档
git mv .planning/PROJECT.md docs/planning/project.md
git mv .planning/ROADMAP.md docs/planning/roadmap.md
git mv .planning/REQUIREMENTS.md docs/planning/requirements.md
git mv .planning/STATE.md docs/planning/state.md

# 阶段计划（保留目录结构）
git mv .planning/phases/04-01-channel-infrastructure docs/planning/phases/
git mv .planning/phases/04-02-runtime-thread-loop docs/planning/phases/
git mv .planning/phases/04-03-appcore-migration docs/planning/phases/
git mv .planning/phases/4.2-runtime-thread-loop docs/planning/phases/

# TODO 迁移
git mv .planning/todos docs/planning/todos
```

## 第四批：指南文件重命名（统一命名规范）

```bash
# Instructions 目录文件重命名为 kebab-case
git mv docs/Instructions/coding_guidelines.md docs/guides/coding-guidelines.md
git mv docs/Instructions/debug_playbook.md docs/guides/debug-playbook.md
git mv docs/Instructions/wgpu.md docs/guides/wgpu-guide.md
git mv docs/Instructions/app_core_error_design.md docs/guides/app-core-error-design.md
```

## 第五批：Debug 文档重命名

```bash
git mv docs/debug/brush_merge_duplicate_tiles_2026-02-23.md docs/debug/brush-merge-duplicate-tiles.md
```

## 第六批：清理和整合

```bash
# 清理空目录
rmdir docs/Wiki 2>/dev/null || true
rmdir docs/Instructions 2>/dev/null || true
rmdir .planning/codebase 2>/dev/null || true
rmdir .planning/phases 2>/dev/null || true
rmdir .planning/todos 2>/dev/null || true
rmdir .planning 2>/dev/null || true
```

## 链接更新

迁移完成后，需要更新以下文档中的内部链接：

1. `docs/README.md` - 已更新为新结构
2. `docs/guides/coding-guidelines.md` - 检查相对链接
3. `docs/guides/debug-playbook.md` - 检查引用链接
4. `docs/guides/wgpu-guide.md` - 检查引用链接
5. `crates/renderer/AGENTS.md` - 更新指向 docs 的链接
6. `crates/render_protocol/AGENTS.md` - 更新指向 docs 的链接
7. `AGENTS.md` (根目录) - 更新指向 docs 的链接

## 验证步骤

### 1. 检查 git 状态

```bash
git status
```

确认所有移动都显示为 `renamed` 而不是 `deleted` + `new file`。

### 2. 检查 Markdown 链接

```bash
# 查找可能断裂的链接（需要手动检查）
grep -r "docs/Wiki" docs/ crates/
grep -r "docs/Instructions" docs/ crates/
grep -r "\.planning/" docs/ crates/
```

### 3. 编译检查

```bash
# 如果有 markdown 链接检查工具
cargo install markdown-link-check
# 或使用项目已有的检查脚本
```

## 回滚方案

如需回滚，执行反向移动：

```bash
# 示例：回滚架构文档
git mv docs/architecture/overview.md .planning/codebase/ARCHITECTURE.md
git mv docs/architecture/structure.md .planning/codebase/STRUCTURE.md
# ... 依此类推
```

## 迁移后检查清单

- [ ] 所有 `git mv` 命令执行完成
- [ ] `git status` 显示正确的重命名
- [ ] 文档内相对链接已更新
- [ ] crate 的 AGENTS.md 链接已更新
- [ ] 根目录 AGENTS.md 链接已更新
- [ ] 没有指向旧路径的引用
- [ ] 新文档导航可正常访问

---

**执行者**: \_\_\_\_\_\_\_\_\_\_
**执行日期**: \_\_\_\_\_\_\_\_\_\_
**验证者**: \_\_\_\_\_\_\_\_\_\_
