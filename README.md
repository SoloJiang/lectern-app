# Lectern

> AI 写，人读。一个以代码审阅为核心的原生 IDE。
>
> A reading-first, review-first native IDE for the AI coding era.

AI 让写代码的成本大幅下降，人的时间转移到读懂和把关上。Lectern（讲台 / 阅读架）把「审阅 AI 改动」放在第一位：按 agent 会话归因的 Diff、内嵌 Ghostty 内核终端、轻量编辑、原生 monorepo 与多 worktree 支持，编辑和终端都为审阅服务。

**状态：P0 搭建中，尚无可用版本。**

## 它帮你回答五个问题

| 问题 | Lectern 的做法 | 阶段 |
|---|---|---|
| 改了什么？ | 按 agent 会话、按轮次切分改动，从 Claude Code / Codex 的会话记录还原每次修改 | P1 |
| 为什么改？ | 每个改动块旁显示触发它的 prompt 和 agent 的说明，可跳回会话原文 | P1 |
| 影响到哪？ | 对改动涉及的符号自动列出调用方与相关测试 | P2 |
| 改得对不对？ | 结构化 Diff、逐文件标记「已看」、终端里一键跑测试 | P2 |
| 怎么让 agent 改？ | 在代码行上写评论，带文件与行号一键发回对应会话 | P2 |

## 技术路线

单进程纯 Rust，首发只支持 macOS（Apple Silicon）。

| 层 | 选型 |
|---|---|
| UI 框架 / 组件 | [GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui) + [gpui-component](https://github.com/longbridge/gpui-component) |
| 终端 | [libghostty-vt](https://github.com/ghostty-org/ghostty) 内核 + GPUI 自绘网格 |
| 高亮 / 索引 | tree-sitter |
| Git / Diff | gix + imara-diff |
| Agent | 解析 Claude Code / Codex 本地 JSONL 会话；后期接 ACP |
| 代码平台 | GitHub 与 GitLab（含自部署） |

架构「一切皆插件」：内核只管插件装载、依赖和生命周期，内置插件编译期链接，第三方插件以独立进程接入。

## 性能预算（M 系列 Mac，目标值）

| 指标 | 预算 |
|---|---|
| 冷启动到首帧 | < 150 ms |
| 打开项目到文件树可用 | < 100 ms |
| 打开文件到首屏高亮 | < 16 ms |
| 打开一个会话的 Diff | < 50 ms |
| 切换 worktree / 分支 | < 100 ms |
| 空闲内存 | < 150 MB |
| 滚动与输入 | 稳定 120 fps |

## 路线图

| 阶段 | 工期 | 内容 | 出口关卡 |
|---|---|---|---|
| P0 搭建与验证 | 1–2 周 | agent 友好的工程脚手架、libghostty-vt 网格渲染、UI 探针、性能基准 | G1：三项验证都跑通 |
| P1 MVP | 4–6 周 | 文件树 + 轻量编辑、终端 + 会话 resume、按会话归因的 Diff、项目模型 | G2：自己能日常使用 |
| P2 理解与反馈 | 约 4 周 | 语法索引 + Java 解析、影响面、结构化 Diff、评论回传 | G3：Java 跳转可日常用 |
| P3 Agent 原生化 | 4 周起 | ACP 对话面板、MR 视图、依赖源码、权限请求 UI | — |

进度见 [Milestones](https://github.com/SoloJiang/lectern/milestones) 与 [docs/roadmap.md](docs/roadmap.md)。

## 文档

- [技术调研：Rust + GPUI 实现方案](docs/design/tech-research.md)
- [架构决策记录（ADR）](docs/adr/)
- [AGENTS.md](AGENTS.md)：给 coding agent 的仓库规则

## 许可证

MIT 或 Apache-2.0 双许可，任选其一。见 [LICENSE-MIT](LICENSE-MIT) 与 [LICENSE-APACHE](LICENSE-APACHE)。

本项目只参考 Zed 的思路，不复制其 GPL 代码。
