# Claude Code 2.1.302 fixtures

**全部是合成数据**，不是脱敏的真实日志：每一行都是按 [agents 设计「外部格式」](../../../../../../docs/design/agents.md#外部格式)记下的 Claude Code 2.1.30x 记录形状和公开的 Claude Code JSONL 格式手工构造的，内容全是占位文本、假的 UUID 和 ID（`msg_fixture_*`、`req_fixture_*`、`toolu_fixture_*`）、假的模型名（`fixture-model-a`、`fixture-model-b`），路径都在 `/Users/dev/fixture/` 下。设计要求的「脱敏的真实样本」要等人提供真实日志之后，用 `crates/agents/src/redact.rs` 的白名单脱敏工具生成，放进同一个版本目录（见设计文档「与设计的差异」）。

- 模拟的格式版本：Claude Code `2.1.302`（记录里的 `version`）。
- 构造日期：2026-10-17。
- 目录布局与 `<配置目录>/projects/` 相同：`projects/-Users-dev-fixture-repo/` 下是主会话、分叉会话和主会话的 `subagents/`。

## 文件与覆盖的记录

| 文件 | 内容 |
|---|---|
| `11111111-1111-4111-8111-111111111111.jsonl` | 主会话，三轮。`file-history-snapshot`（没有 `sessionId` 和 `cwd`）；`isMeta` 的 `<local-command-caveat>`；带 `origin.kind: "human"` 的 prompt；同一响应（`msg_fixture_0001`）的 `thinking`、`text`、`tool_use` 各一行、`usage` 逐步变完整；`Read` 及结果；成功的 `Edit`（`toolUseResult.structuredPatch`）；**失败的 `Edit`**（`is_error`，`toolUseResult` 是字符串）；`Write` 新建（`originalFile: null`）；同一响应的两行 `text`，第二行带 `usage.iterations[]` 的 `advisor_message` 和 `advisorModel`；`system`（`turn_duration`、`stop_hook_summary`）；**被中断的 `Bash`**（`toolUseResult.interrupted: true`）和随后的 `[Request interrupted by user for tool use]`；`cd` 之后 `cwd` 改变；`Task` 与 `Agent` 各派出一个子 agent；被拒绝的 `Bash`（`User rejected tool use`）；`Grep`（相对 `path`）、`Glob`（没有 `path`）、表外的 MCP 工具（输入里有非字符串的 `path`）；不认识的记录类型 `fixture-future-record`；`origin.kind: "task-notification"` 的注入消息；末尾的 `system` 与没有时间戳的 `last-prompt`。 |
| `11111111-…/subagents/agent-a0fa0001.jsonl` 与 `.meta.json` | 子 agent 一，在 `.claude/worktrees/agent-a0fa0001` 这个 worktree 里工作（`meta.json` 带 `worktreePath`）。记录带父会话的 `sessionId` 和自己的 `agentId`、`isSidechain: true`；第一条 `user` 是父 agent 给的任务；`Edit`（`replace_all`）及结果。`toolUseId` 指向主会话的 `toolu_fixture_0007a`。 |
| `11111111-…/subagents/workflows/wf-fixture/agent-a0fa0002.jsonl` 与 `.meta.json` | 子 agent 二，嵌套在 `workflows/<id>/` 下，与主会话同一个 cwd；`Glob`（绝对 `path`）。`toolUseId` 指向 `toolu_fixture_0007b`。 |
| `22222222-2222-4222-8222-222222222222.jsonl` | 分叉会话：开头原样复制了主会话第一轮的四行（prompt、`text`、`Read` 的 `tool_use` 与结果，`sessionId` 是主会话的），之后是自己的一轮（`Bash` 及结果）。 |

超长行（超过 16 MB）、没写完的末尾行、坏行和认不出格式的文件不放进 fixtures，由测试在临时目录里生成。
