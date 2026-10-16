# review（`lectern-review`）

审阅的领域逻辑：会话改动归因、片段对齐、已看状态、Drifted 判断、审阅意见攒批与回传。

- 层：领域层。可以依赖领域、基础设施和 kernel 层；不依赖 GPUI，直接和间接都不行（架构规则 2）。
- 设计文档：`docs/design/review.md`，尚未编写。写代码前先按[模板](../../docs/design/TEMPLATE.md)写好，人审过再动手。
- issue：[#15](https://github.com/SoloJiang/lectern-app/issues/15)、[#22](https://github.com/SoloJiang/lectern-app/issues/22)
- ADR：[0012](../../docs/adr/0012.md)

## 边界

- 归因结果以 blob OID 为键缓存（架构规则 5）。
- 已看状态和审阅意见可以从 git 对象、会话 JSONL 和用户标注日志重建。

## 测试

不依赖 GPUI，在任何平台上都能编译和测试：

```sh
cargo check -p lectern-review     # 只改本 crate 时最快的反馈
just test -p lectern-review
```

- 单元测试写在模块旁的 `#[cfg(test)] mod tests`；结构化输出（解析结果、归因、diff）用 insta 快照验收，快照改动要在 PR 里逐个看过。
- 真实数据放 `tests/fixtures/`，先脱敏；格式随上游版本变化的，每个版本留一份样本。
