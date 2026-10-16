# executor（`lectern-executor`）

后台任务：UI 线程之外的 IO、解析和 git 操作。

- 层：基础设施层。只依赖基础设施和 kernel 层；不依赖 GPUI。
- 设计文档：`docs/design/executor.md`，尚未编写。先按[模板](../../docs/design/TEMPLATE.md)编写，人审过再写代码。
- ADR：[0017](../../docs/adr/0017.md)

## 边界

- 任务可以取消，取消后不再向视图返回结果。
- 首帧之前不提交任务（[ADR 0017](../../docs/adr/0017.md)）。

## 测试

不依赖 GPUI，在任何平台上都能编译和测试：

```sh
cargo check -p lectern-executor     # 只改本 crate 时最快的反馈
just test -p lectern-executor
```

- 单元测试写在模块旁的 `#[cfg(test)] mod tests`；结构化输出（解析结果、归因、diff）用 insta 快照验收，快照改动要在 PR 里逐个看过。
- 真实数据放 `tests/fixtures/`，先脱敏；格式随上游版本变化的，每个版本留一份样本。
