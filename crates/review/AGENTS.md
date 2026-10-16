# review（`lectern-review`）

审阅的领域逻辑：会话改动归因、片段对齐、已看状态、Drifted 判断、审阅意见攒批与回传。

- 层：领域层。可以依赖领域、基础设施和 kernel 层；不得直接或间接依赖 GPUI（架构规则 2）。
- 设计文档：`docs/design/review.md`，尚未编写。先按[模板](../../docs/design/TEMPLATE.md)编写，设计文档 PR 合并后再写代码。
- issue：[#15](https://github.com/SoloJiang/lectern-app/issues/15)、[#22](https://github.com/SoloJiang/lectern-app/issues/22)
- ADR：[0012](../../docs/adr/0012.md)、[0019](../../docs/adr/0019.md)

## 边界

- 归因结果的缓存键是文件内容的 blob OID 加上会话轮次历史的摘要（架构规则 5）：两个会话的最终内容相同时，改动叙事仍然不同，不能只按 OID 命中。
- 已看状态和审阅意见可以从 git 对象、会话 JSONL 和用户标注日志重建。
- 已看标记的键包含审阅对象（会话或比较范围）、路径、改动前后两个 blob OID、改动前后的文件模式和改动状态；重命名时新旧两个路径都记下，按任一路径都能查到。同一审阅对象和路径的这些值任一变化时，标为「审过之后又被改过」，作为第三种状态交给视图。设计依据见 [pulls.review 调研](../../docs/research/pulls-review.md#对-lectern-的建议)。
- 生成文件、lockfile、快照用同一份模式列表识别，同时决定默认折叠、规则分组和模型输入里的占位。
- 会话简报默认由规则生成；模型分析只在用户执行 Analyze 时进行（[ADR 0019](../../docs/adr/0019.md)）。模型输出经过结构校验、覆盖检查、一次纠错重试和对账后才交给视图；任何文件都不能因为模型漏掉而从界面上消失，没被分到的文件进入「未分类」。行备注只挂在 diff 里显示的行上，否则降为文件备注。

## 测试

不依赖 GPUI，在任何平台上都能编译和测试：

```sh
cargo check -p lectern-review     # 只改本 crate 时最快的反馈
just test -p lectern-review
```

- 单元测试写在模块旁的 `#[cfg(test)] mod tests`；结构化输出（解析结果、归因、diff）用 insta 快照验收，快照改动要在 PR 里逐个看过。
- 真实数据放 `tests/fixtures/`，先脱敏；格式随上游版本变化的，每个版本留一份样本。
