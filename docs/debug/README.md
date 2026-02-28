# Debug 记录

本目录包含具体的 Debug 案例和复盘记录。

## Debug 记录列表

### 2026-02-23: Brush Merge 重复瓦片问题

**问题**: Brush Merge 后出现重复瓦片

**根因**: [详细分析见文档](brush-merge-duplicate-tiles.md)

**复盘要点**:
- 输入坐标契约检查
- Render Tree Revision 一致性
- 赃区模型验证
- wgpu CPU->GPU 写入语义

---

## 如何添加 Debug 记录

当遇到并解决一个复杂 Bug 后，建议添加复盘记录：

1. 创建新文件，命名格式：`topic-date.md`
2. 包含以下内容：
   - 问题现象
   - 根因分析
   - 解决方案
   - 预防措施
3. 在本文档添加链接

## 相关文档

- [Debug Playbook](../guides/debug-playbook.md) - 系统性排查流程
- [wgpu-guide](../guides/wgpu-guide.md) - GPU 问题排查
