# Lectern 技术调研：Rust + GPUI 实现方案

2026-10-16 · @SoloJiang

> 项目立项时的技术调研快照。项目原名 LightIDE，现定名 Lectern。原文（含交互图）：[Claude 文档](https://claude.ai/artifact/9PrndEEHBCBymuZs17Hrvw)。后续变更以 `docs/adr/` 为准。
>
> 2026-10-16 更新：按 [ADR 0012](../adr/0012.md)（以项目为入口、不以编辑器为中心）与 [ADR 0013](../adr/0013.md)（英文优先、支持中文）修订了产品原则、界面结构、crate 划分和路线图。交互规范见 [interaction.md](interaction.md)。

## 结论速览

产品定位：AI 写、人读，代码查看与审阅是第一优先级。入口是项目或文件夹，打开后先看到这个项目里待审的 agent 会话；Lectern 不以编辑器为中心，编辑收在第三层。界面英文优先，支持简体中文。性能是第一约束，原生支持 monorepo 与多 worktree，首发只做 macOS。推荐路线：GPUI + gpui-component 做 UI，libghostty-vt 做终端内核，tree-sitter 做高亮，gix + imara-diff 做 Git/Diff，直接解析 Claude Code / Codex 的本地 JSONL 会话。整套是纯 Rust（终端内核经 C ABI 链入），技术上可行，3 个月内能出可用的 MVP。

- **UI 框架用 GPUI**：Zed 自研的 GPU 渲染框架，Apache-2.0，可独立使用，但仍是 pre-1.0，需要通过 crates.io 的 `gpui-pre` 快照精确锁定版本（[ADR 0016](../adr/0016.md)）。
- **组件库和编辑器都用 gpui-component**（Longbridge 出品）：60+ 组件，并自带支持 tree-sitter 与 LSP 的代码编辑器（20 万行内性能稳定），用于完整文件阅读和按需编辑（第三层功能），不必自己写编辑器内核。
- **终端接入 Ghostty 内核**：用 libghostty-vt（Rust 绑定 libghostty-rs）做终端状态机，在 GPUI 里自绘字符网格；不用 Zed 的 alacritty_terminal。
- **代码导航以本地索引为主**：tree-sitter 符号索引保证打开即可跳转，Java 额外做 package / import 解析和依赖 sources.jar 跳转，jdtls 只做后台增强。
- **Agent 集成分两层**：第一层只读本地会话文件 + 在内嵌终端里 `claude --resume` / `codex resume`；第二层再考虑接 ACP 协议，把 agent 对话做成原生面板。
- **许可证不再影响技术路线**：不复用 Zed 的 GPL 代码后，全部依赖都是宽松许可（GPUI、gpui-component 为 Apache-2.0，libghostty、tree-sitter 为 MIT），选哪种许可证都兼容。建议用 Rust 社区惯例 MIT + Apache-2.0 双许可，降低企业用户的采用门槛。

- **性能取舍**：不默认启动语言服务器、不做进程内的动态插件运行时（唯一例外是经审查、签名的 tree-sitter grammar 库，见 [ADR 0018](../adr/0018.md)）、不遍历被忽略的目录，文件列表直接读 git index，所有缓存以 git blob OID 为键。目标是冷启动 20 ms 内出首帧、空闲内存 60 MB 以内、打开文件不超过一帧（[ADR 0017](../adr/0017.md)）。
- **原生支持 Monorepo**：项目模型分三层（仓库 → worktree → 子项目），只读清单文件、不跑构建工具；agent 会话按 cwd 归位到对应的 worktree 和子项目。

## 关键决策

- 开源：是。不复用 Zed 的整体方案，按 Lectern 自己的取舍设计。
- 「连接 Ghostty」指接入内核（libghostty-vt），不嵌入 Ghostty 渲染器。
- 不以编辑器为中心：入口是项目或文件夹，项目主页先列待审的 agent 会话，审阅按轮次读改动；完整文件与编辑收在第三层。
- 首发只支持 macOS。
- 产品核心是代码查看与审阅 AI 改动，编辑和终端为它服务。
- Monorepo 与多 worktree 原生支持；极致性能是第一约束。
- Java 项目同时支持 Maven 与 Gradle。
- MR 支持 GitHub 与 GitLab（含自部署版）。
- 界面英文优先，同时支持简体中文；人和 agent 写的内容不翻译。
- 许可证不再影响技术路线；倾向 MIT + Apache-2.0 双许可。
- 架构完全可插拔：参考 DeepSeek Harness，内核只负责插件生命周期，功能全部是插件。

## 产品原则：以代码审阅为核心

AI 让写代码的成本大幅下降，人的时间转移到读懂和把关上。Lectern 的核心因此是帮人回答五个问题的「审阅台」，编辑和终端都为它服务。

> 图：AI coding 的审阅闭环 · 5 步（交互图见原设计文档）

每个问题对应一项功能，传统 IDE 大多不提供：

| 人要回答的问题 | 传统 IDE 的做法 | Lectern 的做法 | 阶段 |
|---|---|---|---|
| 改了什么？ | 按文件看 git diff | 按 agent 会话、按轮次切分改动；从 Claude Code 的 Edit / Write 工具调用和 Codex 的补丁记录还原每次修改 | P1 |
| 为什么改？ | 无 | 每个改动块旁显示触发它的 prompt 和 agent 的说明，可跳回会话原文 | P1 |
| 影响到哪？ | 手动查引用 | 对改动涉及的符号自动列出调用方与相关测试，复用代码导航索引 | P2 |
| 改得对不对？ | 从头读完整 diff | 结构化 Diff（忽略纯格式变化、识别移动的代码，参考 difftastic 基于 tree-sitter 的做法）；逐文件标记「已看」；终端里一键跑测试 | P2 |
| 怎么让 agent 改？ | 复制粘贴到聊天框 | 在代码行上写审阅意见，攒成一批，带文件与行号一次发回对应会话 | P1 |

由此确定的设计取舍：

- **以项目为入口**：打开文件夹进入项目主页，首先看到这个项目里待审的会话、运行中的 agent 和会话外的改动；侧栏显示项目结构（会话按 worktree 分组、子项目、Files 入口），不放常驻文件树。
- **审阅按叙事读**：选中会话后按轮次读「prompt → agent 说明 → 改动」，文件只作为右侧进度列表；顶部是会话简报（值得先看的几处）。
- **代码按需呼出**：在 Diff 里点符号打开上下文面板（调用方、相关测试、改动历史）；完整文件阅读和编辑放在第三层。
- **界面分三层**：常驻（项目主页、审阅、已看进度、审阅意见）、按需（上下文面板、对话面板、⌘K、并行总览、Files）、收起（完整文件、编辑、终端原始输出）。不做活动栏、文件标签页、状态栏和底部终端停靠。
- **归因会过期**：agent 改完后文件可能又被人或其他会话改过，需要用当前内容重新对齐修改片段，对不上的标记为「已漂移」，不做猜测性归因。

## 性能预算

Lectern 以阅读为主，可以去掉 Zed 为编辑能力付出的大部分开销，把资源集中在打开、切换和浏览的速度上。下表是验收标准，每个版本都跑基准，超标即回退。

| 指标（M 系列 Mac） | 预算 | 主要手段 |
|---|---|---|
| 冷启动到首帧 | < 20 ms | 首帧前只建窗口、画骨架，首帧前不读我们自己的文件（窗口快照在首帧后读）；插件、SQLite、监听、会话扫描都在首帧后；shader 预编译；字体走系统字体 API |
| 打开项目到项目主页可用 | < 100 ms | 文件列表直接读 git index，不遍历目录 |
| 打开文件到首屏高亮 | < 16 ms（一帧） | mmap 读取；只解析可见区域，整文件 tree-sitter 解析放后台 |
| 打开一个会话的 Diff | < 50 ms | 改动从会话 JSONL 增量提取并缓存；diff 结果按 blob OID 缓存 |
| 切换 worktree / 分支 | < 100 ms | 所有缓存以 git blob OID 为键，内容相同即命中 |
| 安装包 | `.app` ≤ 80 MB（目标 50 MB），`.dmg` ≤ 40 MB | 只带审阅闭环需要的核心；其余语言包、jdtls、平台与 agent 适配从插件市场按需下载（[ADR 0018](../adr/0018.md)） |
| 空闲内存 | 空窗口 < 60 MB；打开 monorepo < 100 MB | 不内置 LSP、扩展运行时和协同；SQLite 页缓存 8 MB；缓存按字节 LRU；会话流式读取 |
| 滚动与输入 | 稳定 120 fps | 虚拟滚动，按行缓存字形与高亮，渲染线程不做 IO |

这些数字是目标值，需要在 P0 用原型实测后校准。

### 排除的功能

- **不默认启动语言服务器**：jdtls、tsserver 只在用户请求精确跳转时按需拉起，空闲一段时间后自动关闭。
- **不做进程内的动态插件运行时**：Zed 用 WASM 扩展，VS Code 用 Node 扩展进程，Lectern 两者都不采用；内置插件编译期链接，可下载的代码插件以独立进程接入，见「插件架构」一节。唯一在进程内加载的下载内容是 tree-sitter grammar 库：只有官方审查过源码、由我们构建签名的版本，没有插件 API，只提供解析表和 scanner（[ADR 0018](../adr/0018.md)）。
- **不全量遍历工作区**：被 .gitignore 忽略的目录（node_modules、target、.next）默认不进文件列表、不建索引、不监听，展开时再加载。
- **不同步阻塞 UI**：所有 IO、解析、git 操作都在后台线程，UI 线程只读快照。

### 实测样例

以 `cloudprint-miniapps` 为例（2026-10-16 实测）：

| 项 | 数值 |
|---|---|
| git 跟踪的文件 | 728 个（Java 194、TS 163、TSX 141） |
| 磁盘上全部文件 | 约 11.4 万个 |
| node_modules | 659 MB |
| .git | 110 MB |
| git worktree | 11 个 |

其中 99% 以上的文件被 .gitignore 忽略。如果像传统编辑器那样遍历目录并监听全部文件，打开项目时的工作量会放大约 150 倍。直接读 git index 只需处理 728 条记录。

## Monorepo 与多 worktree 支持

AI 时代的仓库有三个特征：多语言混合的 monorepo、每个子项目一份 agent 指令文件、多个 agent 并行时用 git worktree 隔离。`cloudprint-miniapps` 三点都符合：yarn workspaces（TS）加 Maven 多模块（Java），11 个子目录各有 AGENTS.md（CLAUDE.md 是指向它的软链接），还挂着 11 个 worktree。Lectern 需要一层轻量的「项目模型」，Zed 没有这一层。

### 项目模型：仓库 → worktree → 子项目

| 层 | 识别方式 | 用途 |
|---|---|---|
| 仓库 | 共享同一个 .git 对象库的所有目录 | 一个入口看全部 worktree 和会话 |
| worktree | `git worktree list`；标记已失效（prunable）的条目 | 每个并行 agent 的工作区，可横向对比 |
| 子项目 | package.json workspaces、pnpm-workspace.yaml、pom.xml modules、settings.gradle、go.work、Cargo workspace；不在声明中但有自己清单文件的目录也算 | 侧栏 Packages 分组、搜索范围、影响面分析 |

子项目只从清单文件读出，不跑 `yarn`、`mvn` 等构建工具，毫秒级完成。例如 `apps/miniapp-console/frontend` 有自己的 package.json 却不在 workspaces 里，依然要识别为独立子项目。

### Agent 会话归属

- **会话归属**：会话的 cwd 可能是仓库根、某个子项目或某个 worktree；统一映射到「仓库 + worktree + 子项目」，这样在 worktree 里跑的会话也能在主仓库入口看到。
- **并行 agent 总览**：一个视图列出每个 worktree 的分支、最近会话、未提交改动量，点击后显示该 worktree 相对主干的 Diff。

### 跨子项目导航

- **TS 包名解析**：读 workspaces 里各包的 `name` 和 tsconfig `paths`，把 `@cloudprint-miniapps/core` 解析到 `frontend-packages/core` 源码，跳过 node_modules 里的符号链接和构建产物。
- **Maven 模块间依赖**：从各 pom.xml 的 dependency 坐标匹配本仓库模块，跳到源码；外部依赖才走 `-sources.jar`。
- **跨包影响分析**：改了 `frontend-packages/ui` 的导出时，列出引用它的各个 app。
- **前后端接口对照（后期）**：同一仓库里的 TS 请求路径与 Java Controller 路由按字符串匹配，改动一端时提示另一端。

### 性能要点

- **缓存按内容寻址**：解析结果、符号索引、diff 都以 git blob OID 为键，存入仓库级 SQLite。worktree 之间大部分文件内容相同，11 个 worktree 几乎只需索引一次。
- **只监听跟踪的文件**：用 macOS FSEvents 监听每个 worktree 根目录，事件先按 git index 过滤，node_modules 里的改动直接丢弃。
- **非活动 worktree 不常驻**：只保留元数据（分支、HEAD、改动量），打开时再加载。

## 需求拆解与 Zed 对照

Lectern 以阅读为主：AI 写代码，人负责浏览、审 Diff、切换会话。功能范围比 Zed 小一个量级，可以去掉 LSP、协同编辑、扩展系统等模块。

| Lectern 功能 | Zed 中对应的模块（只读参考） | Lectern 的做法 | 复杂度 |
|---|---|---|---|
| 窗口、面板、快捷键 | `gpui`、`workspace`、`ui` | 项目侧栏 + 阅读栏 + 右侧抽屉，不用 Dock 布局 | 中 |
| 文件浏览（按需） | `project_panel`、`worktree` | 侧栏 Files 入口，按需展开；文件列表读 git index | 低 |
| 完整文件阅读 + 编辑（第三层） | `editor`、`text`、`rope`、`language` | gpui-component 的 Editor，默认只读，去掉多光标、重构等功能 | 中 |
| Diff / MR 查看 | `buffer_diff`、`git_ui`、`multi_buffer` | gix 读仓库 + imara-diff 算差异，自绘双栏/统一视图 | 高 |
| 内嵌终端 | `terminal`（alacritty_terminal）、`terminal_view` | libghostty-vt 内核 + GPUI 自绘网格 | 高 |
| 识别 Claude Code / Codex 项目 | `agent_servers`、`acp_thread` | 扫描 `~/.claude/projects`、`~/.codex/sessions` 的 JSONL | 低 |
| 唤起 CLI / GUI | 无直接对应 | 内嵌终端执行 resume 命令；`open -a` 唤起桌面 App | 低 |
| Agent 原生面板（可选） | `agent_ui` + ACP | `agent-client-protocol` crate + claude-agent-acp / codex-acp 适配器 | 高 |

## 技术选型

每一层都有成熟的 Rust 方案。终端渲染已定为在 GPUI 里自绘网格。

| 层 | 推荐 | 备选 | 选择理由 |
|---|---|---|---|
| UI 框架 | [GPUI](https://github.com/zed-industries/zed/blob/main/crates/gpui/README.md)（crates.io `gpui-pre` 快照，精确锁版本） | Iced、egui、Tauri | 与 Zed 同源，GPU 渲染，macOS 用 Metal；学习资料主要是 Zed 源码 |
| 组件库 | [gpui-component](https://github.com/longbridge/gpui-component) | 自己写 | Dock 布局、虚拟化 List/Table、Markdown、主题系统都现成 |
| 终端内核 | [libghostty-vt](https://docs.rs/libghostty-vt) via [libghostty-rs](https://github.com/Uzaaft/libghostty-rs) | alacritty_terminal（Zed 同款） | 「连接 Ghostty」的产品定位；解析、滚动缓冲、reflow、按键编码都由它处理 |
| 终端渲染 | 自己在 GPUI 里画网格 | [gpui-libghostty](https://docs.rs/crate/gpui-libghostty/latest/source/README.md)（原生 Metal/OpenGL surface） | 自绘能叠加 GPUI 浮层、选区、搜索；原生 surface 是 alpha 且浮层要靠截图回读 |
| 文本存储 | `gpui-component Editor` | Zed 的 `rope` / `sum_tree` | 直接用 gpui-component Editor 自带的文本缓冲，无需单独实现 |
| 语法高亮 | `tree-sitter` + 各语言 grammar | `syntect` | 增量解析，能顺带做代码折叠、符号大纲 |
| Git 读取 | `gix`（gitoxide） | `git2`、调用 `git` CLI | 纯 Rust，无 libgit2 依赖；复杂写操作可回退 CLI |
| Diff 算法 | `imara-diff` | `similar` | 速度快，gix 与 Helix 都在用；行内词级差异可用 `similar` 补 |
| MR 数据 | `octocrab`（GitHub）/ GitLab REST | 调用 `gh` / `glab` CLI | 支持 GitHub 与 GitLab（含自部署版）；按 remote 的域名识别平台，自部署实例由用户配置域名与 API 地址；MVP 调 gh / glab CLI，复用已有登录态 |
| 文件监听 | `notify` + `ignore` | `watchman` | 跨平台；`ignore` 自动遵守 .gitignore |
| Agent 协议 | [`agent-client-protocol`](https://agentclientprotocol.com) crate | 直接解析 JSONL | Zed、JetBrains 等都在用 ACP，Claude Code 与 Codex 都有适配器 |
| 界面文案 | [Fluent](https://projectfluent.org)（`fluent-rs`） | `rust-i18n` | 支持复数、语序变化；en 与 zh-CN 的键一致由 CI 检查 |

### 终端：接入 libghostty-vt 内核

- **分工**：libghostty-vt 负责解析转义序列、维护屏幕与滚动缓冲、resize 时 reflow、按键与鼠标编码；Lectern 负责 PTY、渲染和交互。这与 Zed 用 alacritty_terminal 的分工一致，只是内核换成 Ghostty。
- **渲染自绘**：在 GPUI 里画字符网格，选区、搜索、链接识别、浮层都能与其他视图保持一致。
- **线程模型**：libghostty-vt 的类型是 `!Send + !Sync`，放在专属线程，通过 channel 向 UI 发送渲染快照。
- **不采用 gpui-libghostty**：它嵌入的是 Ghostty 整个渲染器，与「只接内核」的目标不符，只作为渲染效果的对照参考。

## 技术路线与架构

技术路线：单进程纯 Rust，数据单向流动，所有派生数据按内容缓存，视图只读不可变快照。代码按四层组织为 Cargo workspace 的多个 crate。

> 图：Lectern 分层架构 · 4 层 22 个模块（交互图见原设计文档）

依赖方向自上而下。以下五条规则用于防止性能在迭代中下降：

1. **单进程、无常驻运行时**：不带 Node、JVM、WASM 宿主；jdtls 等外部进程按需启动、空闲即关。
1. **数据单向流动**：外部 → 基础设施 → 领域层 → 不可变快照 → 视图；视图只发命令，不直接改状态，也不做 IO。
1. **内容寻址**：解析、高亮、符号索引、diff 都以 git blob OID 为键，内容不变就不重算，worktree 之间共享。
1. **领域层不依赖 GPUI**：可单独测试和跑基准，也可复用于命令行工具。
1. **平台代码集中**：FSEvents、`open -a`、钥匙串等 macOS 专属能力只出现在 `platform` crate。

核心数据流如下。性能预算中「打开一个会话的 Diff < 50 ms」依赖这条路径：

> 图：核心数据流 · 会话改动到归因 Diff（交互图见原设计文档）

对应的 crate 划分：

| 层 | crate | 职责 |
|---|---|---|
| 入口 | `lectern-app` | App 启动、按 profile 装配插件、启动快照恢复；内核是单独的 kernel crate，下表其余 crate 都以插件形式注册 |
| 视图 | `workspace`、`ui` | 窗口骨架、面板、系统色 token 与动画原语（[ADR 0015](../adr/0015.md)），封装 gpui-component |
| 视图 | `welcome`、`project_home`、`review_view`、`context_drawer`、`conversation`、`worktrees`、`file_reader` | 打开项目、项目主页、按轮次审阅、上下文面板、与 agent 对话（含终端页）、并行总览、完整文件阅读与编辑 |
| 领域 | `review` | 会话改动归因、片段对齐、「已看」状态、评论回传 |
| 领域 | `project` | 仓库 → worktree → 子项目模型；清单解析（npm/yarn/pnpm、Maven、Gradle、Go、Cargo） |
| 领域 | `code_intel` | tree-sitter 符号索引、Java/TS 解析、依赖源码、按需语言服务器 |
| 领域 | `git` | gix 读仓库，imara-diff 与结构化 diff |
| 领域 | `terminal` | portable-pty + libghostty-vt，独占线程 |
| 领域 | `agents` | Claude Code / Codex 会话解析、ACP 客户端 |
| 领域 | `usage` | token 去重与聚合、额度快照（[ADR 0014](../adr/0014.md)、[ADR 0022](../adr/0022.md)） |
| 领域 | `forge` | GitHub 与 GitLab（含自部署）MR 与 CI 状态 |
| 基础设施 | `cache`、`fs_watch`、`executor`、`platform`、`i18n` | 内容寻址 SQLite 缓存、文件监听接口与过滤（FSEvents 后端在 `platform`）、后台任务、macOS 系统能力、界面文案与语言切换 |
| 工具 | `bench` | 性能预算基准，CI 中超标即失败；另有 ui_probe：调试构建专用的 UI 探针，发布构建不包含 |

## 插件架构

Lectern 采用「一切皆插件」的设计：内核只负责插件的装载、依赖和生命周期，审阅台、终端、语言支持、会话来源等功能全部是插件。设计参考 DeepSeek Harness，差别在于内置插件在编译期链接，可下载的插件以独立进程接入；tree-sitter grammar 库是唯一的进程内例外，只接受官方审查、构建并签名的版本（[ADR 0018](../adr/0018.md)）。

### 参考：DeepSeek Harness

[DeepSeek Harness](https://thenewstack.io/deepseek-harness-open-source-plugins/) 建立在 Cordis 框架上（源自 Koishi 机器人生态），插件设计有五个要点：

- **内核没有特权**：模型适配、工具注册、会话日志、agent 循环都是插件，扩展功能时增加插件，不修改内核。
- **按服务依赖自动启停**：插件声明所需的服务，服务就绪时自动激活，服务消失时自动停用，不需要手写启动顺序。Cordis 称之为空间可组合。
- **卸载即撤销**：框架记录插件注册的事件监听、提示片段、工具定义，插件卸载时一并撤销。Cordis 称之为时间可组合。
- **通过服务键访问能力**：插件使用 `ctx.tools`、`ctx.llm` 这类稳定入口，不直接引用某个实现。
- **状态可从日志重建**：进入模型请求的内容都能从追加写的会话日志重建；运行配置（web、headless、sdk 等）决定启动时加载哪些插件。

### Lectern 的对应设计

| Harness 的做法 | Lectern 对应 | 说明 |
|---|---|---|
| Cordis 内核 | `kernel` crate：Context、服务注册表、事件总线、生命周期 | 内核不含任何业务功能 |
| 服务键 | 类型化的服务接口，如 `SessionSource`、`ForgeProvider` | 接口单独成 crate，插件只依赖接口 |
| 按服务依赖自动启停 | 插件声明 `requires`，依赖就绪才激活 | 例如 java 插件依赖 `code_intel` 服务 |
| 卸载即撤销 | 每个插件持有一张注册表，记录视图、Action、快捷键、监听器、后台任务 | 停用时全部撤销，可在运行中开关 |
| 运行配置 | profile：desktop、headless（命令行与基准）、test、probe | `ui_probe` 只在 probe profile 加载 |
| 状态可从日志重建 | 视图状态可从 git 对象、会话 JSONL 和用户标注日志重建 | 缓存可随时删除重建 |

> 图：插件模型 · 服务接口与实现插件（交互图见原设计文档）

新增一种语言、一个代码平台或一种 agent 会话来源，都是实现对应接口的一个新插件，审阅台等功能插件无需修改。插件接口示意（草案）：

```rust
pub struct JavaPlugin;

impl Plugin for JavaPlugin {
    const NAME: &'static str = "lang-java";
    type Config = JavaConfig;

    // 依赖的服务，就绪后才激活
    fn requires() -> &'static [ServiceId] { &[service::<dyn CodeIntel>()] }

    // 激活条件：仓库里出现这些文件时才加载
    fn activation() -> Activation { Activation::files(&["**/*.java", "pom.xml", "build.gradle*"]) }

    fn apply(ctx: &mut Context, cfg: Self::Config) -> Result<()> {
        // 注册的内容记入本插件的注册表，停用时自动撤销
        ctx.provide::<dyn NavigationProvider>(JavaNav::new(cfg));
        ctx.on::<FileOpened>(|ev, cx| { /* ... */ });
        Ok(())
    }
}
```

### 与 Harness 的差异

Harness 是 TypeScript 写的 agent 运行时，插件可以在进程内自由加载。Lectern 以性能为第一约束，做了四处调整：

- **内置插件编译期链接**：Rust 没有稳定 ABI，动态库插件既不安全也不稳定。内置插件是 workspace 里的 crate，由 profile 和配置决定是否激活，调用走静态分发或一次虚函数调用。
- **按需激活**：插件声明激活条件（出现 .java 文件、出现 pom.xml、用户打开终端），冷启动只激活审阅台所需的最小集合，未激活的插件不占启动时间。
- **第三方插件独立进程运行**（grammar 库除外，它没有第三方档，见 ADR 0018）：通过 stdio JSON-RPC 实现 `SessionSource`、`ForgeProvider`、`ProjectDetector`、`NavigationProvider` 等数据类接口，结果显示在内置视图里，接入方式与 MCP、ACP 一致。第一版不开放第三方绘制界面。外部进程用 macOS Seatbelt 限制文件与网络访问，Harness 对其 agent 子进程也是这样处理的。
- **每个插件有性能预算**：`bench` 记录每个插件的激活耗时和常驻内存，超标即 CI 失败。

### 与 agent 开发的关系

- **插件边界即任务边界**：一个任务通常只改一个插件 crate，最多加上对应的接口 crate。
- **插件之间禁止直接依赖**：CI 检查插件 crate 只依赖 `kernel` 和接口 crate。
- **用模拟服务单独测试**：每个插件在 test profile 里注入模拟服务即可运行。
- **新增插件有模板**：「新增一种语言」「新增一个代码平台」「新增一种会话来源」都做成 agent 技能，按模板生成插件骨架。

## 工程搭建：面向 coding agent 开发

Lectern 本身主要由 coding agent 写，因此仓库要先具备让 agent 自行验证结果的条件，再开始写功能。以下六项放在 P0 第一周完成。

### 1. Agent 上下文

- **分层的 AGENTS.md**：根目录写全局规则（四层依赖方向、性能预算、禁止事项），每个 crate 一份写本模块职责和测试方式；CLAUDE.md 用软链接指向它，与 `cloudprint-miniapps` 的做法一致。
- **决策记录（ADR）**：`docs/adr/` 下每个决策一个文件，本文「关键决策」一节就是第一批。agent 提出要改变路线时，先让它对照 ADR。
- **模块设计文档**：`docs/design/<crate>.md` 写接口和数据结构，先让 agent 写设计，设计文档 PR 合并后再写代码。
- **复用现成技能**：gpui-component 仓库自带 agent 技能（如 `new-component`、`gpui-style-guide`），可直接引入 `.claude/skills/`；再自建「新增一种语言」「新增一个视图」「跑性能基准」等技能。

### 2. 构建与测试反馈

Rust 编译耗时是 agent 开发的主要瓶颈，GPUI 依赖体量大，编译尤其慢。

- **领域层与 GPUI 隔离**：只改领域逻辑时 `cargo check -p lectern-review` 不编译 GPUI，反馈从分钟级降到秒级；这是「领域层不依赖 GPUI」规则的另一个收益。
- **统一入口命令**：用 `just`（或 `cargo xtask`）提供 `just check`、`just test`、`just bench`、`just ci`，AGENTS.md 里只写这几个命令，agent 不必猜参数。
- **编译提速**：细粒度 crate、dev profile 对依赖开优化、`sccache` 共享缓存。多个 agent 并行在不同 worktree 工作时，共享缓存能避免每个 worktree 重编一遍 GPUI。
- **cargo-nextest**：并行跑测试，失败输出简洁，适合 agent 读。

### 3. 自动化验收

agent 无法直接观察界面，验收结果需要能被程序自动判定。

| 验收对象 | 手段 |
|---|---|
| 领域逻辑 | 单元测试 + `insta` 快照：归因结果、子项目识别、diff 输出都对照已审过的预期文件 |
| 真实数据 | fixtures：脱敏的 monorepo 样例（参照 `cloudprint-miniapps` 的结构）、录制的 Claude Code / Codex 会话 JSONL、各版本格式样本 |
| 视图行为 | `#[gpui::test]` 配合 TestAppContext 模拟按键与状态变化 |
| 视觉效果 | UI 探针：语义树 JSON + 带元素标注的截图，详见下文第 6 点 |
| 性能 | `bench` crate 按性能预算表跑基准，超标即失败，并把耗时写进报告 |

### 4. 约束检查

- **锁定工具链与依赖**：`rust-toolchain.toml` 固定编译器版本；GPUI 与 gpui-component 锁同一个 `gpui-pre` 快照；libghostty 用 vendored 源码并固定 Zig 版本。AGENTS.md 规定：未经确认不得升级这些依赖。
- **架构规则自动检查**：CI 检查领域层 crate 的依赖里没有 gpui；`cargo-deny` 检查许可证（拒绝 GPL）；clippy 以警告即失败的模式运行。规则以脚本执行，违反时构建立即失败。
- **CI 在 macOS 上跑**：GitHub Actions 的 Apple Silicon runner 跑全量检查、测试与基准。

### 5. 开发流程与自举

- **一个任务一个 worktree**：多个 agent 并行，每个任务对应一个 issue、一个分支、一个 MR；任务粒度控制在一个 crate 内。
- **用 Lectern 审 Lectern**：P1 完成后，用它自己的审阅台审查 agent 提交的改动。这是最直接的产品验证，也能持续积累真实的会话 fixtures。
- **技术决策写 ADR**：架构、依赖选型、性能预算的变更由负责推进的 agent 决定，并写入 ADR 说明理由；产品方向由人决定。

### 6. UI 结构探针

只给 agent 截图不够：agent 需要从像素推断每块区域是什么、对应哪段代码。网页开发有 DOM 和 Playwright，GPUI 没有对应工具。Zed 自带的 GPUI inspector 能给元素生成路径式 ID 并跳到源码，但只在调试构建里供人点选，没有层级视图，也没有对外接口；社区的 gpui-devtools 同样是应用内工具。所以 Lectern 需要一层自己的「UI 探针」，相当于为 GPUI 做一个精简版 Playwright。

> 图：UI 探针 · agent 读取与操作运行中的 Lectern（交互图见原设计文档）

设计要点：

- **使用语义树**：GPUI 的原始元素树是大量嵌套的 div，agent 难以理解。每个视图实现一个 `Inspect` trait，导出语义节点：角色（文件列表、diff 块、评论框）、稳定 ID、文字、状态（选中、已看、展开）和屏幕范围。类似浏览器的可访问性树。
- **稳定、可读的元素 ID**：按 `视图.区域.条目[业务键]` 命名，例如 `review.files.item[apps/nutrition/App.tsx]`，业务键用路径或会话 ID，不用列表下标。CI 检查可交互元素都带 ID。
- **截图与树对齐**：截图上叠加元素边框和短编号，编号与语义树节点一一对应，多模态 agent 看图时能直接说出「第 12 号元素错位了」。
- **从元素到源码**：视图构造函数用 `#[track_caller]` 记录调用位置，`locate` 返回文件与行号；只记录业务视图层，避免像 Zed inspector 那样都指向公共组件。
- **只通过 Action 操作**：GPUI 的 Action 是带名字的结构体，探针可列出并触发它们；比模拟坐标点击更稳定，也顺带保证所有功能都能用快捷键完成。
- **零性能成本**：`ui_probe` 是独立 crate，用 Cargo feature 开关，只在调试和测试构建中编译，发布构建不包含它。
- **同一套接口服务三种场景**：agent 开发时连运行中的应用；`#[gpui::test]` 里直接调用同样的函数做断言；CI 里把语义树存成 insta 快照，布局变化会变成可读的文本 diff。
- **后续可接系统无障碍树**：Zed 在 2026 年已带「实验性 accessibility」开关。若 GPUI 稳定支持 macOS 无障碍树，语义节点可以直接映射过去，既服务读屏用户，也让通用的 computer-use 工具能读取 Lectern 的界面结构。

P0 只需做 `ui_tree` 与带标注的 `screenshot` 两个工具，验证 agent 能否靠它们独立完成一个视图；其余工具在 P1 补齐。

## 关键模块实现要点

### 1. Agent 项目发现

两种工具的会话都是本地 JSONL，格式不同但都带工作目录，按 cwd 聚合即可得到「项目列表」。

| 工具 | 路径 | 结构要点 | 恢复命令 |
|---|---|---|---|
| Claude Code | `~/.claude/projects/<编码后的路径>/<session-id>.jsonl` | 一个会话一个文件，追加写；每行带 `sessionId`、`cwd`、`gitBranch`、`version` | `claude --resume <id>` |
| Codex | `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`（`$CODEX_HOME` 可改） | 首行 `session_meta`（含 cwd），之后是 `response_item` / `event_msg` | `codex resume <id>` |

- 目录名里的路径编码有歧义（`-` 既是分隔符也可能是原名字符），**以文件内的 `cwd` 字段为准**，不要反解目录名。
- 字段会随版本变化，解析层用宽松的 `serde_json::Value` + 适配器，只取需要的字段，未知 `type` 直接跳过。
- 只读最后若干行拿标题、时间、最后一条消息即可，不要整文件解析；用 `notify` 监听目录，新增行时增量 tail。

### 2. 终端（libghostty-vt 内核）

1. `portable-pty` 创建 PTY 并启动 shell 或 `claude --resume <id>`。
1. 专属线程持有 libghostty-vt 的 `Terminal`，读取 PTY 字节传给它，按帧生成渲染快照发给 UI 线程。
1. GPUI 侧实现一个自定义 `Element`：按行合并同样式的字符批量绘制，光标、选区、超链接作为叠加层。
1. 键盘与鼠标经 libghostty-vt 的 `KeyEncoder` / `MouseEncoder` 编码后写回 PTY。
1. 「在外部 Ghostty 打开」作为补充入口：macOS 用 `open -na Ghostty --args --working-directory=<dir>`（参数需按已安装的 Ghostty 版本验证）。

### 3. 代码与 Diff 查看

- 视图只渲染可见行（虚拟滚动），tree-sitter 在后台线程解析，高亮结果按行缓存。
- Diff 数据流：`gix` 取 HEAD / 工作区 / 指定 commit 的 blob → `imara-diff` 出 hunk → 生成「展示行」模型（上下文折叠、增删配对）→ 统一视图与双栏视图共用同一模型。
- 「AI 改了什么」是核心场景：默认展示工作区相对 HEAD 的变更，并支持按会话开始时间对比，这能把会话和 Diff 关联起来。
- MR 视图：MVP 用 `gh pr diff` / `glab mr diff` 拿补丁，后续再接 API 支持评论。GitLab 自部署实例的 API 版本参差不齐，只依赖 v4 中长期稳定的接口（MR 列表、changes、notes），并在 MR 视图里把 CI 流水线状态一并展示。

### 4. Agent 原生面板（第二阶段）

用 `agent-client-protocol` crate 做客户端，子进程启动 `@zed-industries/claude-agent-acp` 或 `codex-acp`，通过 stdio 通信。这样能拿到结构化的消息、工具调用、权限请求，做出结构化的对话视图。Zed 的 `acp_thread` / `agent_ui` 是现成的参考实现（注意 GPL）。

## 代码导航与编辑

导航结果只通过上下文面板和 ⌘K 呈现，界面上不显示导航层、索引或语言服务器的状态。导航分四层：靠前的层响应快，靠后的层结果准；后面的层未就绪时，前面的层照常工作。这一设计针对 Zed 的不足：Zed 没有项目模型、不建索引，Java 跳转完全依赖 jdtls；而 jdtls 要求 JDK 21、需先导入 Maven / Gradle 工程，跳进库代码时还返回 `jdt://` 私有 URI，任一环节出问题跳转就失效。

| 层 | 实现 | 预期可用时机 | 解决的问题 |
|---|---|---|---|
| 1. 语法索引 | tree-sitter tags 查询抽取定义与引用，存入本地 SQLite；`notify` 增量更新 | 打开项目后数秒 | 所有语言的类、函数、字段按名跳转 |
| 2. Java 解析器 | 读 package / import / 内部类，全限定名映射到文件；同名方法按所在类与参数个数消歧 | 与第 1 层同时 | 项目内 Java 跳转基本准确，无需编译 |
| 3. 依赖源码 | 解析 pom.xml / build.gradle 依赖坐标，到 `~/.m2` 与 Gradle 缓存找 `-sources.jar` 直接解压 | 按需 | 跳进 Spring 等三方库源码 |
| 4. 精确语义（可选） | 后台启动 jdtls（自动找 JDK 21），或导入 scip-java 生成的 SCIP 索引 | 分钟级 | 重载、泛型、继承链等精确结果 |

Maven 与 Gradle 都要支持，但两者的难度不同。Maven 的 pom.xml 是 XML，静态解析即可拿到模块和依赖坐标。Gradle 的构建脚本是 Groovy 或 Kotlin 代码，无法完整静态解析，所以分三步处理：

- **静态读取**：settings.gradle(.kts) 的 `include` 得到子模块，`gradle/libs.versions.toml` 版本目录得到大部分依赖坐标，构建脚本里字面量形式的依赖用正则补充。
- **直接扫缓存**：`~/.gradle/caches/modules-2/files-2.1/<group>/<artifact>/<version>/` 下已下载的 `-sources.jar` 按类名建索引，不必知道依赖是怎么声明的。
- **可选的精确模式**：用户主动触发时在后台跑一次 Gradle 依赖解析，结果按构建文件的哈希缓存，平时不跑。

交互规则：跳转立即用 1–3 层结果响应；第 4 层结果与前面不同时才在抽屉里原地更新，结果相同不提示。第 2 层的做法可以推广到 Go、Kotlin、TypeScript 等有清晰模块规则的语言。

### 编辑范围（第三层）

- **默认只读**：完整文件阅读器默认只读，通过 Edit… 或 ⌘E 进入编辑。
- **基于 gpui-component 的 `Editor`**：已有 tree-sitter 高亮、折叠、Go to Definition 钩子，20 万行内性能稳定；导航结果通过它的定义跳转钩子接入。
- **保留**：撤销重做、查找替换、自动缩进、括号补全、保存时调用项目已有的 formatter。
- **不做**：多光标、宏、重构；代码补全默认关闭，写代码主要交给 agent。
- **与 agent 共存**：文件被 agent 改动时，若本地有未保存修改，弹出三方合并提示，避免互相覆盖。

## 分阶段路线图

先做出能审阅 AI 改动的版本，再补影响面、结构化 Diff 和评论回传，最后做 Agent 原生面板。

> 图：分阶段路线图 · 4 个阶段 3 个关卡（交互图见原设计文档）

| 阶段 | 工期（一人全职估算） | 内容 | 出口关卡 |
|---|---|---|---|
| P0 搭建与验证 | 1–2 周 | 脚手架 · AGENTS · CI · i18n；libghostty-vt 网格渲染；截图调试 · 性能基准 | G1：三项验证都跑通 |
| P1 MVP | 4–6 周 | 打开项目 + 项目主页；按轮次审阅 + 审阅意见回传；终端 + 会话 resume；项目与分支级别的 token 统计（MR 关联在 P3） | G2：自己能日常使用 |
| P2 理解与反馈 | 约 4 周 | 语法索引 + Java 解析；上下文面板（影响面）· 结构化 Diff；完整文件阅读与编辑；Usage 页与额度 | G3：Java 跳转可日常用 |
| P3 Agent 原生化 | 4 周起 | ACP 对话面板；MR 视图 · 依赖源码；权限请求 UI | — |

P0 第一周先搭好面向 agent 的工程脚手架（见「工程搭建」一节），再用 libghostty-vt 验证网格渲染，P1 交付自绘终端；P1 的核心是项目主页和按轮次归因的审阅，项目模型（monorepo、worktree）和性能基准也在 P1 落地，因为后面所有功能都建在它们之上；代码导航索引在 P2 建好，同时服务于跳转和「影响面」分析。工期为估算，P0 结束时应按实际速度重排。

## 风险

| 风险 | 影响 | 应对 |
|---|---|---|
| 误引入 GPL 代码 | 宽松许可的项目一旦混入 Zed 的 GPL 代码，就必须整体改为 GPL | 只参考 Zed 的思路、不复制代码；CI 里用 cargo-deny 检查依赖许可证，出现 GPL 即构建失败 |
| GPUI pre-1.0、无稳定版本 | 升级时 API 频繁破坏 | 用 `=` 精确锁定 `gpui-pre` 快照，与 gpui-component 使用的版本一致；随 gpui-component 发布集中升级 |
| libghostty 未打版本 | C API 变动导致绑定失效 | 用 libghostty-rs 的 vendored 构建锁定 Ghostty 源码；构建链需 Zig |
| 自研 Java 解析器的准确率 | 重载、泛型、静态导入等场景会跳错 | 只承诺「项目内按类跳转」准确；其余场景由 jdtls / SCIP 兜底并标注来源 |
| jdtls 环境要求 | 用户机器没有 JDK 21 时第 4 层不可用 | 自动探测 JDK，缺失时提示一键下载；第 1–3 层不依赖它 |
| 会话 JSONL 格式无官方契约 | Claude Code / Codex 升级后解析失败 | 宽松解析 + 版本适配器 + 解析失败降级为「仅显示文件」；长期可迁到 ACP |
| 仅支持 macOS | 早期只有 Mac 用户能用 | 可直接依赖 Metal、FSEvents 等系统能力；平台相关代码集中在一层，便于以后移植 |

## 参考资料

- [GPUI README（Zed 仓库）](https://github.com/zed-industries/zed/blob/main/crates/gpui/README.md)
- [gpui-component（Longbridge）](https://github.com/longbridge/gpui-component)
- [gpui-component input / Editor 模块文档](https://docs.rs/gpui-component/0.6.1/gpui_component/input/index.html)
- [Ghostty README：libghostty 路线与状态](https://raw.githubusercontent.com/ghostty-org/ghostty/main/README.md)
- [libghostty-rs：libghostty-vt 的 Rust 绑定](https://github.com/Uzaaft/libghostty-rs/wiki)
- [libghostty-vt crate 文档](https://docs.rs/libghostty-vt)
- [gpui-libghostty：GPUI 原生 Ghostty 终端组件](https://docs.rs/crate/gpui-libghostty/latest/source/README.md)
- [Zed 文档：从 IntelliJ 迁移（无索引、依赖 jdtls）](https://zed.dev/docs/migrate/intellij.md)
- [zed-extensions/java：jdtls 需 JDK 21 的问题](https://github.com/zed-extensions/java/issues/113)
- [Sourcegraph：SCIP 与 scip-java](https://sourcegraph.com/blog/announcing-scip)
- [Zed 文档：External Agents（ACP）](https://zed.dev/docs/ai/external-agents)
- [Zed 博客：Codex is live in Zed](https://zed.dev/blog/codex-is-live-in-zed)
- [Claude Code JSONL 会话格式说明](https://claude-dev.tools/docs/jsonl-format)
- [Claude Code 与 Codex 会话存储位置](https://labuladong.online/en/ai-coding/claude-code/session-storage/)
- [pulls.review 调研](../research/pulls-review.md)：按意图分组的 PR 审阅工具，已看状态、模型输出校验和本地 agent 调用的参考
