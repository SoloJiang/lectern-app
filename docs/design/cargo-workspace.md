# 工作区与 crate 划分

状态：已实现（[#33](https://github.com/SoloJiang/lectern-app/pull/33)、[#50](https://github.com/SoloJiang/lectern-app/pull/50)）。

对应 issue [#1](https://github.com/SoloJiang/lectern-app/issues/1)。架构规则见 [AGENTS.md](../../AGENTS.md)，分层理由见 [技术调研](tech-research.md)「技术路线与架构」。

## 目录

```
lectern-app/
├── Cargo.toml            # workspace，统一版本与依赖
├── Cargo.lock            # 提交到仓库
├── rust-toolchain.toml   # 锁定 Rust 版本
└── crates/
    └── <name>/           # 每个 crate 一个目录，扁平排列
```

所有 crate 的包名带 `lectern-` 前缀（`crates/review` → `lectern-review`，代码里写 `lectern_review`），避免和 `git`、`cache` 这类常见 crate 名混淆。全部 `publish = false`。

每个 crate 在 `Cargo.toml` 里声明自己所在的层，[#4](https://github.com/SoloJiang/lectern-app/issues/4) 的架构检查脚本据此校验依赖方向：

```toml
[package.metadata.lectern]
layer = "domain"
```

实现 `kernel::Plugin` 的 crate 另加 `plugin = true`；tool 层 crate 除外，它里面的 `Plugin` 实现只用于测量和测试，不提供也不要求服务（[ADR 0051](../adr/0051.md)）。插件 crate 只依赖 kernel 层和非插件的库 crate，插件 crate 之间不相互依赖，只有 `app` 和 tool 层依赖插件 crate（[ADR 0025](../adr/0025.md)）。

## crate 与层

依赖只能从上往下：`app` → `view` → `domain` → `infra`。`kernel` 和 `services` 构成插件机制，`logging` 是运行日志的调用接口（[ADR 0046](../adr/0046.md)），三者在 kernel 层，任何层都可以依赖。`tool` 只用于调试与测试构建，不进入发布构建。

| 层 | crate | 职责 |
|---|---|---|
| app | `app` | 二进制 `lectern`：启动、按 profile 装配插件、恢复窗口快照 |
| kernel | `kernel` | 插件装载、服务注册表、事件总线、生命周期（[ADR 0010](../adr/0010.md)） |
| kernel | `services` | 服务接口：`SessionSource`、`ForgeProvider`、`NavigationProvider`、`ProjectDetector` 等；插件只依赖接口 |
| kernel | `logging` | 运行日志：调用点的宏、类型化字段与关联作用域；落盘部分（`sink` feature）只由 `app` 和 tool 层启用（[logging 设计](logging.md)，第 1 步已实现） |
| view | `ui` | 系统色 token、动画原语、通用控件，封装 gpui-component（[ADR 0015](../adr/0015.md)） |
| view | `workspace` | 窗口骨架：侧栏、工具栏、检查器、浮动面板、底部浮条 |
| view | `welcome` | 打开项目窗口 |
| view | `project_home` | Overview 与项目侧栏 |
| view | `review_view` | 会话简报、按轮次的改动叙事、审阅意见 |
| view | `context_drawer` | 上下文面板：调用方、测试、历史 |
| view | `conversation` | 检查器的 Conversation 页 |
| view | `terminal_view` | 终端网格绘制（Conversation 的 Terminal 页） |
| view | `worktrees` | 并行总览 |
| view | `usage_view` | Usage 页 |
| view | `file_reader` | 完整文件阅读与编辑（第三层） |
| domain | `project` | 仓库 → worktree → 子项目模型，清单解析 |
| domain | `agents` | Claude Code / Codex 会话解析、ACP 客户端 |
| domain | `review` | 改动归因、片段对齐、已看状态、审阅意见 |
| domain | `git` | gix 读仓库、imara-diff、结构化 diff |
| domain | `code_intel` | tree-sitter 符号索引、Java / TS 解析、依赖源码 |
| domain | `syntax` | 语法高亮：语言表与高亮查询、着色区间、文本与行索引、grammar 的卡死检测（非插件的库，[ADR 0041](../adr/0041.md)） |
| domain | `terminal` | portable-pty + libghostty-vt，独占线程 |
| domain | `usage` | token 去重与聚合、额度快照（[ADR 0014](../adr/0014.md)、[ADR 0022](../adr/0022.md)） |
| domain | `forge` | GitHub 与 GitLab 的 MR 与 CI 状态 |
| infra | `cache` | 内容寻址 SQLite 缓存 |
| infra | `fs_watch` | 与平台无关的文件监听接口，事件按 git index 过滤；FSEvents 后端由 `platform` 实现这个接口，`app` 装配时注入 |
| infra | `executor` | 后台任务，UI 线程之外的 IO 与解析 |
| infra | `platform` | macOS 专属能力：FSEvents、`open -a`、钥匙串、Seatbelt、statusline 安装 |
| infra | `i18n` | Fluent 资源与语言切换（[ADR 0013](../adr/0013.md)） |
| tool | `bench` | 性能预算基准 |
| tool | `ui_probe` | UI 探针，只在调试和测试构建中编译 |
| tool | `doc_check` | 文档链接检查，`just check` 运行 |

本 issue 只建空 crate（带一行职责说明的 `lib.rs`），不写功能代码；依赖在各自的 issue 里按需加入。

## 工具链与 GPUI 锁定

- **Rust**：`rust-toolchain.toml` 锁定 `1.97.0`，带 rustfmt、clippy 和 `aarch64-apple-darwin` 目标。edition 2024。
- **GPUI**：Zed 把 GPUI 快照发布到 crates.io 的 `gpui-pre`，gpui-component 0.7.0 用 `=0.3.7` 精确锁定它（Zed `1a28cff`）。工作区跟随 gpui-component 的锁定，不再用 git 依赖锁 Zed commit（[ADR 0016](../adr/0016.md)；版本按 [ADR 0056](../adr/0056.md) 从 0.3.9 / 0.7.3 降下来，因为 gpui-mcp 的补丁只到 0.3.7）：
  - `gpui = { package = "gpui-pre", version = "=0.3.7" }`
  - `gpui_platform = { package = "gpui-pre-platform", version = "=0.3.7", features = ["font-kit"] }`：macOS 上必须开启 `font-kit`，否则 `gpui_macos` 退回空文字系统，界面不显示任何文字
  - `gpui-component = "=0.7.0"`；`ui` 的 `gpui-kit-assets = "=0.7.0"`
  - 四者只能一起升级，并且必须是 gpui-component 发布时使用的同一个快照；另外要等 gpui-mcp 有目标版本的补丁，或者自己移植（ADR 0056）。`Cargo.lock` 提交到仓库。
  - 本工作区没有 `[patch]`：打过补丁的 GPUI 只在单独的自动化工作区里（ADR 0056）。
- **libghostty-vt**：在 [#5](https://github.com/SoloJiang/lectern-app/issues/5) 中加入，届时锁定版本与 vendored Ghostty 源码。

依赖只在 `[workspace.dependencies]` 里写版本，crate 内用 `{ workspace = true }` 引用，版本只有一处。

## 构建范围

- 领域层与基础设施层不依赖 GPUI，在任何平台都能 `cargo check -p <crate>`。
- 视图层和 `app` 依赖 GPUI，只在 macOS 上编译；CI 在 Apple Silicon runner 上运行（[#4](https://github.com/SoloJiang/lectern-app/issues/4)）。

## 验收

- `cargo metadata` 能列出全部 crate，每个 crate 都有 `layer`。
- `cargo check` 能通过领域层与基础设施层的全部 crate。
- `Cargo.lock` 里 `gpui-pre` 为 `0.3.7`、`gpui-component` 为 `0.7.0`，来源都是 crates.io；根 `Cargo.toml` 里没有 `[patch]`。

## 实际验证

[ADR 0056](../adr/0056.md) 的降级（#310）：gpui-pre、gpui-pre-platform 降到 0.3.7，gpui-component、gpui-kit-assets 降到 0.7.0。本机 `just check` 通过，`just test` 3112 个测试通过（7 个打开真实窗口的测试按惯例跳过，由 CI 运行）；`cargo deny` 只多出 `bzip2-1.0.6` 许可证（`libbz2-rs-sys`，经 gpui-pre-http-client 0.3.7）。设计文档里按 0.3.9 / 0.7.3 源码写下的事实逐条对照了 0.3.7 / 0.7.0 的源码，都成立，版本号已改。两处小差异：0.3.7 的 `profiler` 不记帧信号的时刻（[ADR 0048](../adr/0048.md) 放弃的方案之一，结论不变）；0.3.7 的 `uniform_list` 用 f32 计算行的位置（[ADR 0034](../adr/0034.md) 的阈值相同，`file_reader` 按行号加像素偏移滚动，不受影响）。

[#50](https://github.com/SoloJiang/lectern-app/pull/50) 新增 tool crate `doc_check`，并让每个 crate 继承工作区 lint：

- 在 Linux 上运行 `just check` 通过，架构检查报告 30 个 crate；`just test -p lectern-doc-check` 和 `just deny` 通过。
- `doc_check` 属于 tool 层，不进入发布构建；它在 Linux CI 的 GPUI-free 范围内编译和测试。
- macOS 上的构建由 CI 的 macOS 任务运行。

## 与设计的差异

[#33](https://github.com/SoloJiang/lectern-app/pull/33) 没有记录差异。[#50](https://github.com/SoloJiang/lectern-app/pull/50) 在 crate 表的 tool 层里增加了 `doc_check`，并在每个 crate 的 `Cargo.toml` 里加上 `[lints] workspace = true`，其余结构与上文一致。
