# project_home（`lectern-project-home`）

项目主页与项目侧栏：Overview（Needs Review、Outside Any Session、Recently Done）、Sessions、Packages，底部 Usage 额度条。

- 层：视图层。可以依赖视图、领域、基础设施和 kernel 层；不得依赖 `app` 和 tool 层。
- 设计文档：`docs/design/project_home.md`，尚未编写。先按[模板](../../docs/design/TEMPLATE.md)编写，设计文档 PR 合并后再写代码。
- issue：[#30](https://github.com/SoloJiang/lectern-app/issues/30)
- ADR：[0012](../../docs/adr/0012.md)

## 边界

- 打开项目到主页可用 < 100 ms：文件列表直接读 git index，不遍历目录（[性能预算](../../docs/design/tech-research.md#性能预算)）。
- 会话归位、改动统计由 `project`、`agents`、`review` 计算，本视图只显示快照。

## 测试

依赖 GPUI，只在 macOS 上编译；Linux 上的 `just` 命令不覆盖本 crate，结果以 macOS CI 为准：

```sh
just test -p lectern-project-home       # macOS
```

- 交互行为用 `#[gpui::test]` 配合 `TestAppContext` 模拟按键、Action 和状态变化。
- 视图只读领域层提供的快照。测试里直接构造快照，不使用真实的 git、会话扫描或文件 IO。
- 可交互元素带稳定 ID（`视图.区域.条目[业务键]`），UI 探针（[#6](https://github.com/SoloJiang/lectern-app/issues/6)）完成后语义树用 insta 快照验收。
- 所有字符串走 `lectern-i18n`，en 与 zh-CN 同时提供。
