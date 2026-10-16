# AGENTS.md

本仓库主要由 coding agent 开发。开始工作前先读本文件、[docs/design/interaction.md](docs/design/interaction.md) 和 [docs/design/tech-research.md](docs/design/tech-research.md)。
`CLAUDE.md` 是指向本文件的软链接。各 crate 目录下有各自的 AGENTS.md，记录该 crate 的职责和测试方式。

## 产品原则

- Lectern 以阅读和审阅为主：AI 写代码，人负责浏览、审 Diff、切换会话。编辑和终端为审阅服务。
- 入口是项目或文件夹，不以编辑器为中心。界面分三层（常驻 / 按需 / 收起），新增界面元素前先对照 [交互规范](docs/design/interaction.md) 和 [ADR 0012](docs/adr/0012.md)。
- 界面英文优先，支持简体中文（[ADR 0013](docs/adr/0013.md)）。所有界面字符串走 `i18n` crate 的 Fluent 资源文件，en 与 zh-CN 同时提供，不得硬编码。
- 视觉与动效按 macOS 原生标准（[ADR 0015](docs/adr/0015.md)）：只用系统字体、系统语义色和系统控件；动画用 `ui` crate 的统一原语，时长与曲线见 [交互规范「动效」](docs/design/interaction.md#动效)，不在视图里自定义。
- 性能是第一约束。任何改动不得让 [性能预算](docs/design/tech-research.md#性能预算) 超标：冷启动 20 ms、空闲内存 60 MB（[ADR 0017](docs/adr/0017.md)）。首帧之前不做任何非必要的工作，新功能默认在首帧之后初始化。安装包 ≤ 80 MB（[ADR 0018](docs/adr/0018.md)）：新功能先判断是否属于审阅闭环的核心，不属于核心的做成插件市场里的可选插件。

## 架构规则（违反即 CI 失败）

1. 四层依赖自上而下：入口 → 视图 → 领域 → 基础设施。
2. 领域层 crate 不依赖 `gpui`。
3. 单进程、无常驻运行时：不引入 Node、JVM、WASM 宿主；外部进程（如 jdtls）按需启动、空闲即关。
4. 数据单向流动：视图只读不可变快照、只发命令，不直接改状态，也不做 IO。
5. 派生数据（解析、高亮、索引、diff）以 git blob OID 为键缓存。
6. macOS 专属能力只出现在 `platform` crate。
7. 插件 crate 只依赖 `kernel` 和接口 crate，插件之间不直接依赖。
8. 不得复制 Zed 的 GPL 代码；`cargo-deny` 拒绝 GPL 依赖。

## 禁止事项

- 未经确认不得升级 GPUI、gpui-component（二者锁同一个 gpui-pre 快照，见 [ADR 0016](docs/adr/0016.md)）、libghostty、Rust 工具链。
- 改变架构、依赖选型或性能预算前，先对照 [docs/adr/](docs/adr/)，写一条新 ADR（背景、决策、影响、来源），由负责推进的 agent 决定并标为已接受，不必等人确认。产品方向（定位、目标用户、界面结构的大改）仍由人决定。
- UI 线程上不做 IO、解析或 git 操作。

## 命令

只用以下入口（需要 `just`、`cargo-nextest`、`cargo-deny`；要在多个 worktree 间共享编译结果，在 `~/.cargo/config.toml` 里设置 `build.rustc-wrapper = "sccache"`）：

```sh
just check   # 格式 + 架构规则 + clippy（警告即失败）
just test    # cargo nextest；参数透传，如 just test -p lectern-review
just doctest # 文档测试（nextest 不跑）
just bench   # 性能预算基准
just deny    # 许可证、来源、安全公告
just ci      # 本平台上 CI 跑的全部内容
just fmt     # 格式化
```

在 Linux 上这些命令只覆盖不依赖 GPUI 的 crate，在 macOS 上覆盖整个工作区。

只改不依赖 GPUI 的 crate（领域、基础设施、kernel 层）时，可以先用 `cargo check -p <crate>` 快速检查，不必编译 GPUI；提交前仍要跑上面的入口。

CI 的两个任务见 [docs/design/ci.md](docs/design/ci.md)。

## 合并规则

负责推进的 agent 可以直接合并 PR（squash），以下条件全部满足时才能合并：

1. Codex 已审阅当前最新提交，并在 PR 上留下 👍 reaction（审阅中是 👀，有意见时会留评论）。只改文档的 PR 不需要满足这一条；其中的设计文档 PR 还要等人审过才能合并。
2. CI 全部通过，没有合并冲突。
3. 所有审阅讨论串都已回复并解决。

审阅意见先逐条核实：成立的修复后推送，不成立或不在本 PR 范围的回复理由。Codex 审阅失败或没有更新时，在 PR 上评论 `@codex review` 重新触发。叠在其他分支上的 PR，先合底层，再把上层的 base 改为 `main`。

## 工作方式

- 一个任务一个 worktree、一个 issue、一个分支、一个 PR，粒度控制在一个 crate 内。
- 先写 `docs/design/<crate>.md`（接口与数据结构，按[模板](docs/design/TEMPLATE.md)），人审过再写代码。
- 新增 crate 时同时写 `crates/<name>/AGENTS.md`（职责、边界、测试方式），并建软链接 `CLAUDE.md -> AGENTS.md`。
- 领域逻辑用单元测试 + insta 快照验收；视图用 `#[gpui::test]`；视觉效果用 UI 探针。

## 技能

Claude Code 自动加载 `.claude/skills/` 下的技能；其他 agent 直接读对应的 `SKILL.md`。

| 技能 | 用途 |
|---|---|
| [design-doc](.claude/skills/design-doc/SKILL.md) | 按模板写模块设计文档，提文档 PR 给人审 |
| [adr](.claude/skills/adr/SKILL.md) | 写新 ADR 并更新索引 |
| [ship-pr](.claude/skills/ship-pr/SKILL.md) | 从提交到合并：检查、开 PR、Codex 审阅、处理意见、squash 合并 |
| [gpui](.claude/skills/gpui/SKILL.md) | 写 GPUI 与 gpui-component 代码：按锁定版本读上游指南，Lectern 规则优先 |
