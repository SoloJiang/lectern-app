# agents（`lectern-agents`）

Claude Code / Codex 会话发现与解析，按 cwd 归位到项目和 worktree；P3 起包含 ACP 客户端。

- 层：领域层。可以依赖领域、基础设施和 kernel 层；不得直接或间接依赖 GPUI（架构规则 2）。
- 设计文档：`docs/design/agents.md`，尚未编写。先按[模板](../../docs/design/TEMPLATE.md)编写，设计文档 PR 合并后再写代码。
- issue：[#13](https://github.com/SoloJiang/lectern-app/issues/13)、[#23](https://github.com/SoloJiang/lectern-app/issues/23)
- ADR：[0012](../../docs/adr/0012.md)、[0014](../../docs/adr/0014.md)、[0019](../../docs/adr/0019.md)

## 边界

- 会话 JSONL 流式读取、增量解析，不整文件读入内存。
- 格式随 agent 版本变化：每个版本在 fixtures 里留一份脱敏样本，遇到不认识的记录跳过并计数，不报错退出。
- token 统计在 `usage`，本 crate 只交出原始用量记录。
- Analyze 调用的 Claude Code / Codex 命令行以只读模式运行（[ADR 0019](../../docs/adr/0019.md)；快照不是 git 仓库，Codex 需要 `--skip-git-repo-check`），按需启动、完成即退出。命令行在 `platform` 生成的 Seatbelt 沙箱里运行，只能读取本次分析的输入、被审阅版本的只读快照（从被审阅的提交树生成，只含受跟踪文件和本次改动的文件）和该 agent 的安装目录，不加载用户的其他配置和 MCP 服务器。真实凭据不进入沙箱、环境变量或参数：命令行的模型服务地址指向本 crate 在回环地址上提供的凭据转发端点，只拿到占位令牌，端点在沙箱外换上真实凭据再转发到该 agent 的模型服务主机，只允许该 agent 推理接口的方法、路径和这次配置的模型，同一次分析同时只转发一个请求，转发前检查并计入轮数与 token 预算，按剩余预算改写输出上限；命令行不支持设置服务地址时这个 agent 的 Analyze 不可用；模型服务配置按白名单传入；每次分析有轮数和 token 上限（ADR 0019）。每次调用有超时，超时、取消或关闭项目时终止整个进程组并回收，丢弃未返回的输出。它产生的辅助会话不出现在会话列表里：可用时用不保存会话的调用方式，否则记录会话 ID 并在会话发现时过滤。
- 查找命令行时依次用设置里的绝对路径、登录 shell 的 `PATH`（查询一次并缓存）和常见安装目录；找不到时返回明确的不可用状态，界面据此提示。
- Analyze 的工作目录是空的临时目录，快照作为额外可读目录传入，并关闭项目指令发现（Claude Code `--add-dir`，Codex `-c project_doc_max_bytes=0`）；配置目录（`CLAUDE_CONFIG_DIR`、`CODEX_HOME`）也指向临时目录，辅助会话写在这里并随之删除。
- Analyze 的用量由凭据转发端点按每个响应的用量字段记录，每个请求一条原始用量记录交给 `usage`，取消和失败的分析也计入；没收到最终用量就被中断的请求，输入按请求体、输出按已转发的流式片段估算，标为「估算」。
- 命令行以后台优先级（`PRIO_DARWIN_BG`）运行，每个进程用 `setrlimit` 限制 CPU 时间和内存，并统计整个进程组的 CPU 时间合计；进程组的进程数或常驻内存合计超过上限时终止整个进程组。
- 测试启动命令行的代码时，在 PATH 最前面放一个假命令，记录参数、标准输入和工作目录，按夹具回放 JSON 行输出。

## 测试

不依赖 GPUI，在任何平台上都能编译和测试：

```sh
cargo check -p lectern-agents     # 只改本 crate 时最快的反馈
just test -p lectern-agents
```

- 单元测试写在模块旁的 `#[cfg(test)] mod tests`；结构化输出（解析结果、归因、diff）用 insta 快照验收，快照改动要在 PR 里逐个看过。
- 真实数据放 `tests/fixtures/`，先脱敏；格式随上游版本变化的，每个版本留一份样本。
