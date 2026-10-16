# usage（`lectern-usage`）

token 用量去重、按项目和 MR 归属与聚合（入 SQLite）、价格表、额度快照；Claude Code 额度来自 statusline，Codex 额度来自日志（[ADR 0014](../../docs/adr/0014.md)、[ADR 0020](../../docs/adr/0020.md)）。

- 层：领域层。可以依赖领域、基础设施和 kernel 层；不得直接或间接依赖 GPUI（架构规则 2）。
- 设计文档：`docs/design/usage.md`，尚未编写。先按[模板](../../docs/design/TEMPLATE.md)编写，设计文档 PR 合并后再写代码。
- issue：[#31](https://github.com/SoloJiang/lectern-app/issues/31)、[#32](https://github.com/SoloJiang/lectern-app/issues/32)
- ADR：[0014](../../docs/adr/0014.md)、[0020](../../docs/adr/0020.md)

## 边界

- 统计分项目和 MR 两级（ADR 0020）：git 仓库以 git 公共目录的规范绝对路径作为项目标识，同一仓库的所有 worktree 是同一个项目；不是 git 仓库的文件夹以打开的根目录为准，也是项目，只是没有 MR 这一级，项目合计按时间窗口统计，包含它的全部 MR、未开 MR 的分支、不属于任何 MR 的用量和 Lectern 自己的分析；按每一轮的 cwd 和改动所在的 worktree 判断项目，两者都不在任何已打开项目里时才暂时归入「其他」，之后打开对应项目时自动归入。MR 的归属规则：有改动的轮次按改动所在 worktree 的分支归属，一轮改了多个分支时按「改动行数 + 改动文件数」分摊；没有改动的轮次跟随同一会话里最近的有改动轮次（先找之前、再找之后），会话里没有改动时才用日志记录的分支；子 agent 先按自己的改动归属再计入父会话；分支按「一次使用」区分（删除后重建、或没有开着的 MR 时重置到无关历史才算新的一次；`forge` 仍把分支对应到开着的 MR 时，本地删除重建也留在同一次），MR 区间在同一次使用之内，包含开 MR 之前的工作；`forge` 有对应 MR 时优先于默认分支的规则；一次分析覆盖多个分支时按改动权重分摊。读到日志时把每轮的 worktree、分支和 HEAD 写入 SQLite，重建以这份记录为准，无法确定分支的旧会话标为「无法归属」。会话和轮次是 MR 之下的明细，一轮被分摊到多个 MR 时，每个 MR 的明细显示分到它的那一份。归属规则逐条有 fixtures 覆盖，包括跨 worktree 改动和同名分支复用。
- 去重与解析规则以 ADR 0014 为准（Claude Code 按 `message.id` + `requestId` 去重，Codex 子 agent 只计 `task_started` 之后），每条规则都有 fixtures 覆盖；ccusage 的 `--json` 输出只在测试里做对照。
- 聚合结果写入 Lectern 自己的 SQLite（Claude Code 默认删除 30 天前的会话文件）；statusline 的安装在 `platform`，本 crate 只解析它存下的 JSON。

## 测试

不依赖 GPUI，在任何平台上都能编译和测试：

```sh
cargo check -p lectern-usage     # 只改本 crate 时最快的反馈
just test -p lectern-usage
```

- 单元测试写在模块旁的 `#[cfg(test)] mod tests`；结构化输出（解析结果、归因、diff）用 insta 快照验收，快照改动要在 PR 里逐个看过。
- 真实数据放 `tests/fixtures/`，先脱敏；格式随上游版本变化的，每个版本留一份样本。
