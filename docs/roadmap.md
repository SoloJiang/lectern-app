# 路线图

工期按一人全职估算，P0 结束时按实际速度重排。每个阶段对应一个 GitHub Milestone，任务以 issue 跟踪。

## P0 搭建与验证（1–2 周）

出口关卡 G1：脚手架、终端网格渲染、UI 探针三项验证都跑通。

- [ ] Cargo workspace 骨架与四层 crate 划分，锁定工具链与 GPUI / gpui-component commit
- [ ] 分层 AGENTS.md、ADR 目录、模块设计文档模板、agent 技能
- [ ] 统一命令入口（just）、nextest、sccache、dev profile 优化
- [ ] 架构规则检查脚本、cargo-deny 许可证检查、macOS CI
- [ ] libghostty-vt 接入与 GPUI 网格渲染原型
- [ ] UI 探针 P0：`ui_tree` 与带标注的 `screenshot`
- [ ] bench crate：按性能预算表跑基准，原型实测后校准目标值
- [ ] kernel crate：插件装载、服务注册表、事件总线、生命周期

## P1 MVP（4–6 周）

出口关卡 G2：自己能日常使用，开始用 Lectern 审 Lectern。

- [ ] 项目模型：仓库 → worktree → 子项目，清单解析
- [ ] 文件树：读 git index，被忽略目录懒加载
- [ ] 代码视图 + 轻量编辑（gpui-component Editor）
- [ ] 自绘终端：portable-pty + libghostty-vt，会话 resume
- [ ] Agent 会话发现：Claude Code / Codex JSONL 解析，按 cwd 归位
- [ ] Git 与 Diff：gix + imara-diff，统一 / 双栏视图
- [ ] 审阅台：按会话、按轮次归因的 Diff，「为什么改」，「已漂移」标记
- [ ] 并行 agent 总览：每个 worktree 的分支、会话、改动量

## P2 理解与反馈（约 4 周）

出口关卡 G3：Java 跳转可日常用。

- [ ] 语法索引：tree-sitter tags → SQLite，增量更新
- [ ] Java 解析器：package / import / 内部类，Maven 与 Gradle
- [ ] 跨子项目导航：TS 包名、Maven 模块间依赖
- [ ] 影响面分析：改动符号的调用方与相关测试
- [ ] 结构化 Diff：忽略纯格式变化、识别移动代码
- [ ] 逐文件「已看」与评论回传 agent

## P3 Agent 原生化（4 周起）

- [ ] ACP 对话面板（agent-client-protocol）
- [ ] 权限请求 UI
- [ ] MR 视图：GitHub 与 GitLab（含自部署），CI 状态
- [ ] 依赖源码跳转（-sources.jar）与可选 jdtls / SCIP 精确语义
- [ ] 第三方插件：stdio JSON-RPC 接口与 Seatbelt 沙箱
