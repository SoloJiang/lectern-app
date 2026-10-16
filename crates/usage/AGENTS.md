# usage（`lectern-usage`）

token 用量去重与聚合（入 SQLite）、价格表、额度快照；Claude Code 额度来自 statusline，Codex 额度来自日志（[ADR 0014](../../docs/adr/0014.md)）。

- 层：领域层。可以依赖领域、基础设施和 kernel 层；不得直接或间接依赖 GPUI（架构规则 2）。
- 设计文档：`docs/design/usage.md`，尚未编写。先按[模板](../../docs/design/TEMPLATE.md)编写，设计文档 PR 合并后再写代码。
- issue：[#31](https://github.com/SoloJiang/lectern-app/issues/31)、[#32](https://github.com/SoloJiang/lectern-app/issues/32)
- ADR：[0014](../../docs/adr/0014.md)、[0019](../../docs/adr/0019.md)

## 边界

- 去重与解析规则以 ADR 0014 为准（Claude Code 按 `message.id` + `requestId` 去重，Codex 子 agent 只计 `task_started` 之后），每条规则都有 fixtures 覆盖；ccusage 的 `--json` 输出只在测试里做对照。
- 除了会话日志，`agents` 会交来 Analyze 调用的原始用量记录（[ADR 0019](../../docs/adr/0019.md)）：凭据转发端点每个模型请求一条，带分析 ID，转发之前就落盘为按上限估算的待定记录，收到用量后替换，取消和失败的分析也有；没收到用量就中断（包括 Lectern 崩溃）的请求保留为「估算（上限）」。辅助会话不写入用户的会话目录，所以不会与日志重复。
- 聚合结果写入 Lectern 自己的 SQLite（Claude Code 默认删除 30 天前的会话文件）；statusline 的安装在 `platform`，本 crate 只解析它存下的 JSON。

## 测试

不依赖 GPUI，在任何平台上都能编译和测试：

```sh
cargo check -p lectern-usage     # 只改本 crate 时最快的反馈
just test -p lectern-usage
```

- 单元测试写在模块旁的 `#[cfg(test)] mod tests`；结构化输出（解析结果、归因、diff）用 insta 快照验收，快照改动要在 PR 里逐个看过。
- 真实数据放 `tests/fixtures/`，先脱敏；格式随上游版本变化的，每个版本留一份样本。
