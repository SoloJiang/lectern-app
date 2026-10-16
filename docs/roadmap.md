# 路线图

工期按一人全职估算，P0 结束时按实际速度重排。每个阶段对应一个 GitHub Milestone，任务以 issue 跟踪。

## P0 搭建与验证（1–2 周）

出口关卡 G1：脚手架、终端网格渲染、UI 探针三项验证全部通过。

- [ ] Cargo workspace 骨架与四层 crate 划分，锁定工具链与 GPUI / gpui-component 版本
- [ ] 分层 AGENTS.md、ADR 目录、模块设计文档模板、agent 技能
- [ ] 统一命令入口（just）、nextest、sccache、dev profile 优化
- [ ] 架构规则检查脚本、cargo-deny 许可证检查、macOS CI
- [ ] libghostty-vt 接入与 GPUI 网格渲染原型
- [ ] UI 探针 P0：`ui_tree` 与带标注的 `screenshot`
- [ ] bench crate：按性能预算表跑基准，原型实测后校准目标值
- [ ] kernel crate：插件装载、服务注册表、事件总线、生命周期
- [ ] i18n 基础：Fluent 资源文件、en 与 zh-CN、跟随系统语言、CI 检查键一致

## P1 MVP（4–6 周）

出口关卡 G2：可以日常自用，开始用 Lectern 审阅 Lectern 自身的改动。界面结构见 [交互规范](design/interaction.md)。

- [ ] 项目模型：仓库 → worktree → 子项目，清单解析
- [ ] Welcome：最近项目、打开文件夹、发现有 agent 会话的文件夹
- [ ] 项目主页：项目侧栏（Sessions / Packages）、Needs review、会话外的改动
- [ ] 终端：portable-pty + libghostty-vt，会话 resume（Conversation 的 Terminal 页）
- [ ] Agent 会话发现：Claude Code / Codex JSONL 解析，按 cwd 匹配到项目
- [ ] 项目与分支级别的 token 统计：去重、按改动归属到项目和分支、聚合入 SQLite，显示在项目侧栏、并行 worktree 总览、会话简报和每轮标题；关联到 MR 在 P3 随 MR 视图完成（[ADR 0020](adr/0020.md)）
- [ ] Git 与 Diff：gix + imara-diff，统一 / 双栏视图
- [ ] 审阅视图：会话简报、按轮次的改动叙事、「为什么改」、已看进度、Drifted 标记
- [ ] 审阅意见：行内写意见、攒批、带文件与行号发回会话
- [ ] 并行 agent 总览：每个 worktree 的分支、会话、改动量

## P2 理解与反馈（约 4 周）

出口关卡 G3：Java 跳转可日常使用。

- [ ] 语法索引：tree-sitter tags → SQLite，增量更新
- [ ] Java 解析器：package / import / 内部类，Maven 与 Gradle
- [ ] 跨子项目导航：TS 包名、Maven 模块间依赖
- [ ] 上下文面板：调用方、相关测试、改动历史（影响面）
- [ ] Usage 页与额度：每日用量、按项目 / 分支 / 模型明细（标为「分支用量」，P3 起按 MR）、额度提醒，Claude Code statusline 默认接入
- [ ] 结构化 Diff：忽略纯格式变化、识别移动代码
- [ ] 文件浏览（侧栏 Files）与完整文件阅读、编辑（第三层）

## P3 Agent 原生化（4 周起）

- [ ] Conversation 面板：ACP 结构化对话（agent-client-protocol）
- [ ] 权限请求 UI
- [ ] MR 视图：GitHub 与 GitLab（含自部署），CI 状态；分支用量关联到 MR（[ADR 0020](adr/0020.md)）
- [ ] 依赖源码跳转（-sources.jar）与可选 jdtls / SCIP 精确语义
- [ ] 第三方插件：stdio JSON-RPC 接口与 Seatbelt 沙箱
