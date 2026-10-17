# Codex 0.140.0 `legacy` fixtures

**全部是合成数据**，不是脱敏的真实日志：每一行都是按 [agents 设计「外部格式」](../../../../../../docs/design/agents.md#外部格式)记下的 Codex rollout 记录形状，以及公开的 [openai/codex](https://github.com/openai/codex) 源码（提交 `3b8f43a`：`codex-rs/history/src/lib.rs`、`codex-rs/protocol/src/protocol.rs`、`models.rs`、`items.rs`、`codex-rs/rollout/src/policy.rs`）里的序列化形状手工构造的。没有读取或参考本机的任何会话日志。内容全是占位文本、假的线程 ID（`aaaaaaaa-0000-4000-8000-00000000000N`）、假的调用与响应 ID（`call_l1_*`、`resp_l1_*`）、假的模型名（`fixture-model-x`、`fixture-model-y`），路径都在 `/Users/dev/fixture/` 下。设计要求的「脱敏的真实样本」要等人提供真实日志之后，用 `crates/agents/src/redact.rs` 的白名单脱敏工具生成，放进同一个版本目录（见设计文档「与设计的差异」）。

- 模拟的格式版本：`session_meta.cli_version` 为 `0.140.0`，`history_mode: "legacy"`（行上没有 `ordinal`）。
- 构造日期：2026-10-17。
- 目录布局与 `<CODEX_HOME>/sessions/YYYY/MM/DD/` 相同，文件名 `rollout-<时间>-<线程 ID>.jsonl`。
- `.jsonl.zst` 不提交：测试在临时目录里用 `ruzstd` 的编码器从这些文件生成。

## 文件与覆盖的记录

| 文件 | 内容 |
|---|---|
| `sessions/2026/10/02/rollout-…-aaaaaaaa-…-000000000001.jsonl` | 主会话，三轮。`session_meta`（`git.branch`）；`role: "user"` 的环境说明 `message`（不是 prompt）；`turn_context`；`task_started`；`user_message` / `agent_message`；`reasoning`、`agent_reasoning`（已知忽略）。工具：`exec_command`（相对 `workdir`，输出头 `Process exited with code 0`）；打开交互式 shell 的 `exec_command`（`Process running with session ID 7`）和随后写入它的 `write_stdin`；旧式 `shell`（`["bash", "-lc", 脚本]`，输出是带 `metadata.exit_code: 101` 的 JSON 文本）；第二轮 `turn_context` 改了 cwd 之后的 `custom_tool_call` `apply_patch`（Add、Update、Delete、Move，相对路径）与 `patch_apply_end.changes`；`function_call` 形式的 `apply_patch`，`patch_apply_end` 为 `failed`；`local_shell_call` 先 `in_progress` 后 `completed`，另一个 `incomplete`；`view_image`（输出是内容项数组）；`spawn_agent`；表外工具 `fixture__lookup`（旧版本输出 `{content, success: false}`）。用量与额度：只写 `token_count` 的前两轮（额度用旧版本的 `resets_in_seconds`），一个 `info` 为 null、只有额度的 `token_count`；第三轮模拟升级后续接，`token_usage_record` 与 `token_count` 两路都写（切换前后的合计等于最后的 `total_token_usage`）。另有不认识的 `event_msg` 类型、不认识的顶层类型、`world_state`（已知忽略）和 `turn_aborted`。 |
| `sessions/2026/10/03/rollout-…-aaaaaaaa-…-000000000002.jsonl` | 主会话的 `legacy` 分叉（`forked_from_id`，没有 `subagent_history_start_ordinal`）：开头复制了主会话第一轮（`task_started` 的 `turn_id` 是主会话的），之后是自己的一轮（`shell_command`）。 |
| `sessions/2026/10/04/rollout-…-aaaaaaaa-…-000000000003.jsonl` | 分叉的分叉（`forked_from_id` 指向上一个分叉）：复制的前缀里既有主会话的轮次（上一个分叉自己继承的），也有上一个分叉自己的轮次，之后是自己的一轮。 |

超长行（超过 16 MB）、没写完的末尾行、坏行和认不出格式的文件不放进 fixtures，由测试在临时目录里生成。
