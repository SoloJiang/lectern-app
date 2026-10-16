# Lectern

[English](README.md) · 简体中文

> AI 写，人读。打开项目，先看 agent 改了什么。

AI 让写代码的成本大幅下降，人的时间转移到读懂和把关上。Lectern（讲台 / 阅读架）是一个原生 macOS 应用，用来审阅 coding agent 写的代码。它不是编辑器：打开项目或文件夹后，首先看到的是这个项目里等你审的 Claude Code / Codex 会话；审阅时按轮次读「你说了什么 → agent 做了什么 → 改了哪些代码」，有问题就把审阅意见批量发回会话。原生支持 monorepo 与多 worktree，界面英文优先、支持中文。

**状态：P0 搭建中，尚无可用版本。**

## 它帮你回答这些问题

| 问题 | Lectern 的做法 | 阶段 |
|---|---|---|
| 改了什么？ | 按 agent 会话、按轮次切分改动，从 Claude Code / Codex 的会话记录还原每次修改 | P1 |
| 为什么改？ | 每个改动块旁显示触发它的 prompt 和 agent 的说明，可跳回会话原文 | P1 |
| 影响到哪？ | 对改动涉及的符号自动列出调用方与相关测试 | P2 |
| 花了多少？ | 每个会话、每一轮的 token；Codex 与 Claude Code 的额度窗口 | P1 / P2 |
| 改得对不对？ | 结构化 Diff、逐文件标记「已看」、终端里一键跑测试 | P2 |
| 怎么让 agent 改？ | 在代码行上写审阅意见，攒成一批，带文件与行号一次发回会话 | P1 |

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

架构「一切皆插件」：内核只管插件装载、依赖和生命周期。审阅闭环的核心随安装包提供；其他语言包、Java 精确语义、更多代码平台与 agent 适配，在项目需要时从插件市场按需下载。下载的插件要么在 Seatbelt 沙箱里作为独立进程运行，要么是经我们审查源码、构建并签名后在主程序内加载的语法库。

## 性能预算（M 系列 Mac，目标值）

| 指标 | 预算 |
|---|---|
| 冷启动到首帧 | < 20 ms |
| 打开项目到项目主页可用 | < 100 ms |
| 打开文件到首屏高亮 | < 16 ms |
| 打开一个会话的 Diff | < 50 ms |
| 切换 worktree / 分支 | < 100 ms |
| 空闲内存 | 空窗口 < 60 MB，打开 monorepo < 100 MB |
| 安装包 | 安装后 ≤ 80 MB（目标 50 MB），下载 ≤ 40 MB |
| 滚动与输入 | 稳定 120 fps |

## 路线图

| 阶段 | 工期 | 内容 | 出口关卡 |
|---|---|---|---|
| P0 搭建与验证 | 1–2 周 | agent 友好的工程脚手架、i18n、libghostty-vt 网格渲染、UI 探针、性能基准 | G1：三项验证都跑通 |
| P1 MVP | 4–6 周 | 打开项目、项目主页、按轮次审阅、审阅意见回传、终端 + 会话 resume、项目模型 | G2：自己能日常使用 |
| P2 理解与反馈 | 约 4 周 | 语法索引 + Java 解析、上下文抽屉（影响面）、结构化 Diff、完整文件阅读与编辑 | G3：Java 跳转可日常用 |
| P3 Agent 原生化 | 4 周起 | ACP 对话面板、MR 视图、依赖源码、权限请求 UI | — |

进度见 [Milestones](https://github.com/SoloJiang/lectern-app/milestones) 与 [docs/roadmap.md](docs/roadmap.md)。

## 文档

- [交互规范](docs/design/interaction.md)（[交互设计画布](https://claude.ai/artifact/LVSXNi9xPviRgzdycnskp9)）
- [技术调研：Rust + GPUI 实现方案](docs/design/tech-research.md)
- [架构决策记录（ADR）](docs/adr/)
- [AGENTS.md](AGENTS.md)：给 coding agent 的仓库规则

## 许可证

MIT 或 Apache-2.0 双许可，任选其一。见 [LICENSE-MIT](LICENSE-MIT) 与 [LICENSE-APACHE](LICENSE-APACHE)。

本项目只参考 Zed 的思路，不复制其 GPL 代码。
