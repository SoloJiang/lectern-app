# Lectern

[English](README.md) · 简体中文

Lectern 是一个原生 macOS 应用，运行在 Apple Silicon 上，用来审阅 Claude Code 和 Codex 在你的仓库里写的代码。对每个 agent 会话，它显示你提出的要求、agent 做的改动，以及改动中还需要检查的部分。Lectern 以阅读和审阅代码为主；文件编辑和终端也有，但只在你打开时出现。

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/images/review-dark.png">
  <img src="docs/images/review-light.png" alt="在 Lectern 里审阅一个 Claude Code 会话">
</picture>

## 项目主页

Lectern 以文件夹或 git 仓库为单位打开项目。打开后，项目主页分三组列出：

- **Needs Review：** 有未审阅改动的 agent 会话。还在运行的会话也列在这里，并显示运行状态。
- **Outside Any Session：** 工作区里不属于任何 agent 会话的改动。
- **Recently Done：** 已经审阅完的会话。

monorepo 和多个 git worktree 不需要额外配置。

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/images/overview-dark.png">
  <img src="docs/images/overview-light.png" alt="项目主页：等待审阅的会话">
</picture>

## 会话审阅

Lectern 按会话发生的顺序显示内容。每一轮依次是你的 prompt、agent 的回复和这一轮产生的 diff。每一处改动都关联到改动它的那一轮，可以看到这一行为什么被改。会话结束后被人修改过的行单独标出，与 agent 的改动区分开。Lectern 无法识别某个会话的日志格式时，改为按文件显示普通的 diff。

审阅时可以：

- **查看改动的影响范围。** 上下文面板列出每个被改动符号的调用方和相关测试，并标出 agent 没有同步修改的调用方。结果来自本地索引。Java 项目可以手动开启可选的语言服务器，得到更精确的结果。
- **把审阅意见发给 agent。** 阅读时在具体的代码行上写意见，Lectern 把它们汇总后，连同文件路径和行号一次发给会话，agent 在下一轮处理。
- **跳过不需要审阅的部分。** 只涉及格式的改动默认折叠。标记为已审的文件会一直保留标记。

## 并行 worktree

多个 agent 同时在不同的 git worktree 里运行时，Lectern 把这些 worktree 分成 Active 和 Idle 两组列出。选中一个 worktree，右侧检查器把它与 main 对比：改动的文件、领先的提交数，以及其中是否有文件也在其他 worktree 里被改动。

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/images/worktrees-dark.png">
  <img src="docs/images/worktrees-light.png" alt="多个 worktree 并行运行 agent">
</picture>

## Token 用量与额度

Lectern 统计每个会话、每一轮的 token 数，并按 API 价格估算成本，同时显示 Codex 和 Claude Code 的使用额度已经用了多少。Claude Code 的状态栏会把数据发给 Lectern，所以它的额度窗口实时更新。

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/images/usage-dark.png">
  <img src="docs/images/usage-light.png" alt="用量：每天的 token 与额度窗口">
</picture>

## 性能与平台

Lectern 按以下预算开发（[ADR 0017](docs/adr/0017.md)、[ADR 0018](docs/adr/0018.md)），CI 中的基准测试会用这些预算检查每一次改动。

- **启动：** 启动后 20 ms 内显示第一帧，其余内容在这之后加载。
- **内存：** 空窗口时低于 60 MB，打开大型 monorepo 时低于 100 MB。语言服务器只在你开启时启动，空闲时自动关闭。Lectern 不常驻 Node、JVM 这类运行时。
- **安装包：** 小于 80 MB。其他编程语言、代码平台和 agent 的支持以插件形式提供，只在项目需要时下载。会运行代码的插件在沙箱中运行，只能使用它声明的权限。
- **macOS 集成：** 使用系统字体、系统颜色和系统控件，跟随系统的浅色或深色外观，动画采用 macOS 的标准方式。

## 支持的工具

- **Agent：** Claude Code 与 Codex。Lectern 读取它们的本地会话文件。
- **代码平台：** GitHub 与 GitLab，包括两者的自部署版本。
- **界面语言：** English 与简体中文。
- **硬件：** 搭载 Apple Silicon 的 Mac。

## 开发状态

Lectern 处于早期开发阶段，目前还不能使用。上面的截图来自[交互设计](docs/design/interaction.md)，会随系统的浅色或深色外观切换。

## 参与开发

Lectern 用 Rust 编写：界面基于 [GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui) 和 [gpui-component](https://github.com/longbridge/gpui-component)，代码解析用 tree-sitter，git 操作用 gix，终端用 [libghostty-vt](https://github.com/ghostty-org/ghostty)。

- [交互规范](docs/design/interaction.md)和[交互设计画布](https://claude.ai/artifact/LVSXNi9xPviRgzdycnskp9)
- [技术调研](docs/design/tech-research.md)和[架构决策记录](docs/adr/)
- [AGENTS.md](AGENTS.md)：coding agent 在本仓库里遵守的规则

## 许可证

Lectern 采用 MIT 与 Apache-2.0 双许可，可任选其一，见 [LICENSE-MIT](LICENSE-MIT) 与 [LICENSE-APACHE](LICENSE-APACHE)。部分设计参考了 Zed 的思路，但 Lectern 不包含 Zed 任何 GPL 许可的代码。
