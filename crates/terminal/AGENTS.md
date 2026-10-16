# terminal（`lectern-terminal`）

portable-pty + libghostty-vt 的终端状态，运行在独占线程上；会话 resume。

- 层：领域层。可以依赖领域、基础设施和 kernel 层；不得直接或间接依赖 GPUI（架构规则 2）。
- 设计文档：`docs/design/terminal.md`，尚未编写。先按[模板](../../docs/design/TEMPLATE.md)编写，人审过再写代码。
- issue：[#5](https://github.com/SoloJiang/lectern-app/issues/5)、[#12](https://github.com/SoloJiang/lectern-app/issues/12)
- ADR：[0002](../../docs/adr/0002.md)

## 边界

- 终端内核只接 libghostty-vt（[ADR 0002](../../docs/adr/0002.md)）；libghostty 用 vendored 源码，升级先确认。
- 向视图提供网格快照；视图不直接读写 PTY。

## 测试

不依赖 GPUI，在任何平台上都能编译和测试：

```sh
cargo check -p lectern-terminal     # 只改本 crate 时最快的反馈
just test -p lectern-terminal
```

- 单元测试写在模块旁的 `#[cfg(test)] mod tests`；结构化输出（解析结果、归因、diff）用 insta 快照验收，快照改动要在 PR 里逐个看过。
- 真实数据放 `tests/fixtures/`，先脱敏；格式随上游版本变化的，每个版本留一份样本。
