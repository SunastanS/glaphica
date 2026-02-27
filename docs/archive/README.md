# 文档归档说明

> **归档日期**: 2026-02-27  
> **归档原因**: 历史文档，与当前代码状态不符或属于特定阶段记录

---

## 归档原则

### 什么文档应该归档？

1. **阶段性审查记录**: 如 Phase 2 审查响应，代码已合并后不再需要日常参考
2. **工具评估报告**: 如 Comby 测试报告，属于特定工具链评估历史
3. **过期的设计讨论**: 已被新决策替代的旧设计方案
4. **未使用的规范**: 如 Review.md，未集成到实际工作流

### 什么文档应该保留？

1. **活跃指南**: 当前开发仍需参考的规范（如 coding_guidelines.md）
2. **设计决策**: 仍影响当前架构的决策记录
3. **排查手册**: 仍有参考价值的 Debug 案例
4. **契约文档**: 如 merge_ack_integration.md，定义模块间契约

---

## 本次归档清单

### 2026-02 Phase 2 审查 (4 个文档)

**位置**: `archive/2026-02-phase2-review/`

| 文档 | 原位置 | 归档原因 |
|------|--------|----------|
| `phase2_review_response.md` | `docs/Instructions/` | 审查响应记录，代码已合并 |
| `phase2_completion_summary.md` | `docs/Instructions/` | 完成总结，历史记录 |

**访问方式**: 需要时可从归档目录查阅，但不应作为当前开发参考。

### 工具评估报告 (2 个文档)

**位置**: `archive/tool-evaluations/`

| 文档 | 原位置 | 归档原因 |
|------|--------|----------|
| `refactor_automation_experience.md` | `docs/Instructions/` | Comby 工具使用经验，特定工具链 |
| `comby_rust_support_test_report.md` | `docs/Instructions/` | Comby 测试报告，工具评估性质 |

**说明**: 如团队决定采用 Comby 作为标准工具，可移回 `Instructions/` 或新建 `tools/` 目录。

### 未使用规范 (1 个文档)

**位置**: `archive/`

| 文档 | 原位置 | 归档原因 |
|------|--------|----------|
| `Review.md` | `docs/Instructions/` | Code Review 角色定义，未在其他地方引用 |

---

## 如何恢复归档文档

```bash
# 如需要参考某文档
mv docs/archive/2026-02-phase2-review/phase2_review_response.md docs/Instructions/

# 或在原位置创建软链接（推荐，保持归档状态清晰）
ln -s ../archive/2026-02-phase2-review/phase2_review_response.md docs/Instructions/
```

---

## 归档文档索引

### 按主题分类

#### 架构重构
- `2026-02-phase2-review/phase2_review_response.md` - AppCore + GpuRuntime 审查响应
- `2026-02-phase2-review/phase2_completion_summary.md` - Phase 2 完成总结

#### 工具链评估
- `tool-evaluations/refactor_automation_experience.md` - Comby 重构经验
- `tool-evaluations/comby_rust_support_test_report.md` - Comby Rust 支持测试

#### 流程规范
- `Review.md` - Code Review 角色定义（未使用）

---

## 未来归档建议

以下文档未来可能归档（取决于项目状态）：

| 文档 | 潜在归档时间 | 条件 |
|------|-------------|------|
| `app_core_error_design.md` | Phase 4 完成后 | 错误处理迁移完全完成后 |
| `tiles_model_runtime_refactor_guide.md` | 重构完成后 | 所有 phases 完成并稳定后 |
| `debug/brush_merge_duplicate_tiles_2026-02-23.md` | 6 个月后 | 作为历史案例，已有更系统的 Debug Playbook |

---

## 归档维护

### 定期清理

建议每 3 个月检查一次归档目录：
1. 确认是否有文档可以永久删除
2. 确认是否有文档需要恢复为活跃状态
3. 更新归档索引

### 命名规范

归档目录命名格式：
```
archive/
├── YYYY-MM-topic/      # 按时间 + 主题组织
│   └── document.md
└── topic/              # 无明确时间的主题归档
    └── document.md
```

---

**维护者**: Development Team  
**最后更新**: 2026-02-27
