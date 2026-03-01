# 开发指南

本目录包含开发者日常参考的指南文档。

## 核心指南

| 文档 | 用途 | 必读场景 |
|------|------|----------|
| [coding-guidelines.md](coding-guidelines.md) | 编码规范 | 所有代码编写前 |
| [debug-playbook.md](debug-playbook.md) | Debug 排查手册 | 遇到渲染 Bug 时 |
| [wgpu-guide.md](wgpu-guide.md) | GPU 语义陷阱 | GPU 提交/缓冲区问题 |
| [testing.md](testing.md) | 测试模式 | 编写测试时 |
| [conventions.md](conventions.md) | 编码约定 | 命名、样式参考 |

## 重构指南

| 文档 | 用途 |
|------|------|
| [refactoring/tiles-model-runtime.md](refactoring/tiles-model-runtime.md) | Tiles/Model/Runtime 重构指南 |

## 按场景查找

### 场景 1: 我要写新代码
1. [coding-guidelines.md](coding-guidelines.md)
2. 对应 crate 的 `AGENTS.md`

### 场景 2: 我遇到了 Bug
1. [debug-playbook.md](debug-playbook.md)
2. [wgpu-guide.md](wgpu-guide.md)

### 场景 3: 我要修改协议
1. 对应 crate 的 `AGENTS.md`
2. [architecture/decisions/message-flow-design.md](../architecture/decisions/message-flow-design.md)

---

## 维护说明

- 新增指南时使用 `kebab-case` 命名
- 在文档末尾添加 **最后更新** 日期
- 重大变更在文档开头添加 **变更日志**
