# ui_probe（`lectern-ui-probe`）

UI 探针：导出语义树（`ui_tree`）和带元素标注的截图（`screenshot`），让 agent 能读取和操作运行中的 Lectern（[技术调研「UI 结构探针」](../../docs/design/tech-research.md#6-ui-结构探针)）。

- 层：tool 层，只用于调试与测试构建，可以依赖任何层。只有 `app` 可以依赖本 crate，本 crate 不进入发布构建。
- 设计文档：[docs/design/ui_probe.md](../../docs/design/ui_probe.md)
- issue：[#6](https://github.com/SoloJiang/lectern-app/issues/6)

## 边界

- 只在调试和测试构建中编译，发布构建不包含。
- 只通过 Action 操作界面，不模拟坐标点击。

## 测试

依赖 GPUI，只在 macOS 上编译：

```sh
just test -p lectern-ui-probe
```

- 同一套函数既供运行中的应用调用，也在 `#[gpui::test]` 里直接调用做断言；语义树存成 insta 快照。
