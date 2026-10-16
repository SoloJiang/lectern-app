# Lectern

[English](README.md) · 简体中文

**读懂 agent 写的代码。**

Lectern 是一个原生 macOS 应用（Apple Silicon），用来审阅 Claude Code 和 Codex 替你写的代码。它不是编辑器，而是你坐下来读代码的地方：你让它做什么，它做了什么，做得对不对。

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/images/review-dark.png">
  <img src="docs/images/review-light.png" alt="在 Lectern 里审阅一个 Claude Code 会话">
</picture>

## 从项目开始

打开一个文件夹或仓库，Lectern 先告诉你哪里需要你：等你审的 agent 会话、还在运行的会话，以及不属于任何会话的改动。monorepo 和 git worktree 开箱即用。

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/images/overview-dark.png">
  <img src="docs/images/overview-light.png" alt="项目主页：等待审阅的会话">
</picture>

## 像读故事一样读会话

会话按轮次展开：你的 prompt、agent 的说明、它产生的 diff，依次排列。改动对应到引发它的那一轮，很少需要猜某一行为什么变了。会话结束后被人改过的行会单独标出，你知道哪些不是 agent 写的；Lectern 不认识的会话格式会退回到按文件的普通 diff。

- **看到影响面。** 上下文面板列出改动符号的调用方和测试，并标出 agent 没有同步修改的调用方。它基于本地的快速索引；Java 的精确结果来自你手动开启的可选语言服务器。
- **把意见发回去。** 边读边在代码行上写意见，攒成一批，带着文件和行号一次发回会话，agent 下一轮就能接着改。
- **滤掉噪音。** 只改格式的部分自动折叠，看过的文件保持标记。

## 看着多个 agent 并行

在不同 worktree 里同时跑几个 agent，并排看它们：各自改了什么、领先多少提交、有没有两个在改同一批文件。

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/images/worktrees-dark.png">
  <img src="docs/images/worktrees-light.png" alt="多个 worktree 并行运行 agent">
</picture>

## 知道花了多少

每个会话、每一轮的 token，API 等价成本，以及 Codex 和 Claude Code 的额度用了多少。Claude Code 的状态栏直接把数据交给 Lectern，额度窗口实时更新。

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/images/usage-dark.png">
  <img src="docs/images/usage-light.png" alt="用量：每天的 token 与额度窗口">
</picture>

## 快，而且是地道的 Mac 应用

以下是 Lectern 开发时遵守的预算（[ADR 0017](docs/adr/0017.md)、[ADR 0018](docs/adr/0018.md)），CI 基准会用它们检查每一次改动。

- **秒开。** 20 ms 内画出第一帧，其余内容随后加载。
- **轻。** 打开窗口时内存低于 60 MB，打开大型 monorepo 也低于 100 MB；语言服务器只在你开启时启动、空闲即关，也没有常驻的运行时。
- **小。** 安装包不到 80 MB。其他语言、代码平台和 agent 适配在项目需要时才从插件市场下载，运行代码的插件都在沙箱里，只拥有它声明的权限。
- **原生。** 系统字体、系统颜色和系统控件，浅色与深色，动效遵循 macOS。

## 支持

- **Agent：** Claude Code 与 Codex，读取它们的本地会话文件
- **代码平台：** GitHub 与 GitLab（含自部署）
- **界面语言：** English 与简体中文
- **Mac：** Apple Silicon

## 状态

Lectern 处于早期开发阶段，还不能使用。上面的截图来自[交互设计](docs/design/interaction.md)，随系统的浅色 / 深色设置切换。

## 参与开发

Lectern 用 Rust 编写，基于 [GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui) 与 [gpui-component](https://github.com/longbridge/gpui-component)，以及 tree-sitter、gix 和 [libghostty-vt](https://github.com/ghostty-org/ghostty)。

- [交互规范](docs/design/interaction.md)（[交互设计画布](https://claude.ai/artifact/LVSXNi9xPviRgzdycnskp9)）
- [技术调研](docs/design/tech-research.md) · [架构决策记录](docs/adr/)
- [AGENTS.md](AGENTS.md)：coding agent 在本仓库的工作规则

## 许可证

MIT 或 Apache-2.0，任选其一。见 [LICENSE-MIT](LICENSE-MIT) 与 [LICENSE-APACHE](LICENSE-APACHE)。Lectern 参考了 Zed 的思路，但不复制其 GPL 代码。
