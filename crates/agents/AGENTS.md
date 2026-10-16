# agents（`lectern-agents`）

Claude Code / Codex 会话发现与解析，按 cwd 归位到项目和 worktree；P3 起包含 ACP 客户端。

- 层：领域层。可以依赖领域、基础设施和 kernel 层；不得直接或间接依赖 GPUI（架构规则 2）。
- 设计文档：`docs/design/agents.md`，尚未编写。先按[模板](../../docs/design/TEMPLATE.md)编写，设计文档 PR 合并后再写代码。
- issue：[#13](https://github.com/SoloJiang/lectern-app/issues/13)、[#23](https://github.com/SoloJiang/lectern-app/issues/23)
- ADR：[0012](../../docs/adr/0012.md)、[0014](../../docs/adr/0014.md)、[0020](../../docs/adr/0020.md)

## 边界

- 会话 JSONL 流式读取、增量解析，不整文件读入内存。
- 格式随 agent 版本变化：每个版本在 fixtures 里留一份脱敏样本，遇到不认识的记录跳过并计数，不报错退出。
- token 统计在 `usage`，本 crate 交出原始用量记录、Lectern 自己发起的每次分析的计量记录（由凭据转发端点计量：每个请求转发之前先交给 `usage` 落盘、状态为进行中，用量到达时更新，结束时标明完成、取消或失败，取消和失败的也交，带时间戳、agent 与模型、token、估算标记和发起时的项目与 MR 或分支上下文），以及按顺序排列的每个可能产生改动或切换分支的工具调用（写文件的工具和所有执行命令的工具）的开始与结果事件（调用开始和结果的时间戳、会话与轮次、目标路径），供 `usage` 按结果的时间戳从 worktree 的 HEAD reflog 还原当时的分支和 HEAD（不读解析时的当前状态）（[ADR 0020](../../docs/adr/0020.md)）。

## 测试

不依赖 GPUI，在任何平台上都能编译和测试：

```sh
cargo check -p lectern-agents     # 只改本 crate 时最快的反馈
just test -p lectern-agents
```

- 单元测试写在模块旁的 `#[cfg(test)] mod tests`；结构化输出（解析结果、归因、diff）用 insta 快照验收，快照改动要在 PR 里逐个看过。
- 真实数据放 `tests/fixtures/`，先脱敏；格式随上游版本变化的，每个版本留一份样本。
