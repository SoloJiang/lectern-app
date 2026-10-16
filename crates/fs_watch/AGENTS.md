# fs_watch（`lectern-fs-watch`）

与平台无关的文件监听接口，事件按 git index 过滤。

- 层：基础设施层。只依赖基础设施和 kernel 层；不依赖 GPUI。
- 设计文档：`docs/design/fs_watch.md`，尚未编写。先按[模板](../../docs/design/TEMPLATE.md)编写，人审过再写代码。
- ADR：[0006](../../docs/adr/0006.md)

## 边界

- FSEvents 后端在 `platform`，由 `app` 装配时注入；本 crate 不出现 macOS 专属代码（架构规则 6）。
- 测试用内存里的模拟后端驱动事件。

## 测试

不依赖 GPUI，在任何平台上都能编译和测试：

```sh
cargo check -p lectern-fs-watch     # 只改本 crate 时最快的反馈
just test -p lectern-fs-watch
```

- 单元测试写在模块旁的 `#[cfg(test)] mod tests`；结构化输出（解析结果、归因、diff）用 insta 快照验收，快照改动要在 PR 里逐个看过。
- 真实数据放 `tests/fixtures/`，先脱敏；格式随上游版本变化的，每个版本留一份样本。
