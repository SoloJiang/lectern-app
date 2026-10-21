# Codex 0.140.0 `paginated` fixtures

**全部是合成数据**，不是脱敏的真实日志：每一行都是按 [agents 设计「外部格式」](../../../../../../docs/design/agents.md#外部格式)记下的 Codex rollout 记录形状，以及公开的 [openai/codex](https://github.com/openai/codex) 源码（提交 `3b8f43a`：`codex-rs/history/src/lib.rs`、`codex-rs/protocol/src/protocol.rs`、`models.rs`、`items.rs`、`codex-rs/rollout/src/policy.rs`、`ordinal.rs`）里的序列化形状手工构造的。没有读取或参考本机的任何会话日志。内容全是占位文本、假的线程 ID（`bbbbbbbb-0000-4000-8000-00000000000N`）、假的调用与响应 ID（`call_p1_*`、`resp_p1_*`）、假的模型名（`fixture-model-x`），路径都在 `/Users/dev/fixture/` 下。脱敏的真实样本见 `../0.140.0-legacy/SOURCE.md` 的说明。

- 模拟的格式版本：`session_meta.cli_version` 为 `0.140.0`，`history_mode: "paginated"`（每行带 `ordinal`，从 0 开始）。
- 构造日期：2026-10-17。
- 目录布局与 `<CODEX_HOME>/sessions/YYYY/MM/DD/` 相同。
- `.jsonl.zst` 不提交：测试在临时目录里用 `ruzstd` 的编码器从这些文件生成。

## 文件与覆盖的记录

| 文件 | 内容 |
|---|---|
| `sessions/2026/10/05/rollout-…-bbbbbbbb-…-000000000001.jsonl` | 主会话，两轮。prompt 与 agent 说明取 `item_completed` 里的 `UserMessage` / `AgentMessage`；`exec_command` 的输出是内容项数组，输出头 `Process exited with code 1`；`custom_tool_call` `apply_patch` 与 `item_completed` 的 `FileChange`（`id` 是调用的 `call_id`）；每个响应先写 `token_usage_record`，再写累加了同一响应的 `token_count`（新版本的额度：`resets_at`、`limit_id`、`plan_type`）；`Reasoning` 条目（已知忽略）与不认识的条目类型；`spawn_agent` 派出下面的子 agent；第二轮以 `turn_aborted` 结束。 |
| `sessions/2026/10/05/rollout-…-bbbbbbbb-…-000000000002.jsonl` | 子 agent rollout：`source.subagent.thread_spawn.parent_thread_id` 指向主会话，`subagent_history_start_ordinal: 6`。`ordinal` 1–5 是重放的父会话第一轮（含父线程的 `token_usage_record` 和 `token_count`），从 6 开始是自己的部分：`trigger_turn: true` 的 `inter_agent_communication_metadata`、自己的一轮、自己线程的 `token_usage_record` 与 `token_count`。 |

## `spawn_agent` 的输出

主会话 `call_p1_0003` 的输出 `{"agent_id": "bbbbbbbb-…-000000000002"}` 给出上面的子 rollout（`spawn_link_*` 测试用它）。这个形状在 2026-10-21 按公开源码核对过（openai/codex 提交 `07cc649c658be7018b6913299f12f843b24679f8`）：multi_agent v1 的 `codex-rs/core/src/tools/handlers/multi_agents/spawn.rs`（`SpawnAgentResult { agent_id, nickname }`）经 `codex-rs/core/src/tools/handlers/multi_agents_common.rs` 的 `tool_output_response_item` 写成 JSON 文本。上游还带 `nickname`，这里省略（提取时忽略）。参数里的 `task_name` 是 multi_agent v2 的写法，v1 没有这个参数；提取只看输出，参数没有改（改了会改变快照）。详见 [agents 设计「外部格式」](../../../../../../docs/design/agents.md#外部格式)。
