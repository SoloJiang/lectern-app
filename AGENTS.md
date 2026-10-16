# AGENTS.md

本仓库主要由 coding agent 开发。动手前先读本文件、[docs/design/interaction.md](docs/design/interaction.md) 和 [docs/design/tech-research.md](docs/design/tech-research.md)。
`CLAUDE.md` 是指向本文件的软链接。各 crate 目录下会有自己的 AGENTS.md，写该模块的职责和测试方式。

## 产品原则

- Lectern 以阅读和审阅为主：AI 写代码，人负责浏览、审 Diff、切换会话。编辑和终端为审阅服务。
- 入口是项目或文件夹，不以编辑器为中心。界面分三层（常驻 / 按需 / 收起），新增界面元素前先对照 [交互规范](docs/design/interaction.md) 和 [ADR 0012](docs/adr/0012.md)。
- 界面英文优先，支持简体中文（[ADR 0013](docs/adr/0013.md)）。所有界面字符串走 `i18n` crate 的 Fluent 资源文件，en 与 zh-CN 同时提供，不允许硬编码。
- 视觉与动效按 macOS 原生标准（[ADR 0015](docs/adr/0015.md)）：只用系统字体、系统语义色和系统控件；动画用 `ui` crate 的统一原语，时长与曲线见 [交互规范「动效」](docs/design/interaction.md#动效)，不在视图里自定义。
- 性能是第一约束。任何改动不得让 [性能预算](docs/design/tech-research.md#性能预算) 超标。

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
- 不得自行改变架构、依赖选型或性能预算。需要改变时，先对照 [docs/adr/](docs/adr/)，提出新 ADR 交给人决定。
- UI 线程上不做 IO、解析或 git 操作。

## 命令

脚手架完成后只用以下入口（P0 第一周提供）：

```sh
just check   # 编译检查 + clippy（警告即失败）
just test    # cargo nextest
just bench   # 性能预算基准
just ci      # 全量，等同 CI
```

只改领域逻辑时用 `cargo check -p <crate>`，不必编译 GPUI。

## 工作方式

- 一个任务一个 worktree、一个 issue、一个分支、一个 PR，粒度控制在一个 crate 内。
- 先写 `docs/design/<crate>.md`（接口与数据结构），人审过再写代码。
- 领域逻辑用单元测试 + insta 快照验收；视图用 `#[gpui::test]`；视觉效果用 UI 探针。
