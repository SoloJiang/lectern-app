# kernel（`lectern-kernel`）

插件内核：插件装载、服务注册表、事件总线、生命周期（[ADR 0010](../../docs/adr/0010.md)）。插件按服务依赖自动激活与停用；插件注册的内容记入该插件自己的注册表，停用时全部撤销。

- 层：kernel 层。只依赖 kernel 层，不依赖 GPUI；任何层都可以依赖本 crate。
- 设计文档：`docs/design/kernel.md`，尚未编写。先按[模板](../../docs/design/TEMPLATE.md)编写，人审过再写代码。
- issue：[#8](https://github.com/SoloJiang/lectern-app/issues/8)、[#39](https://github.com/SoloJiang/lectern-app/issues/39)、[#40](https://github.com/SoloJiang/lectern-app/issues/40)、[#27](https://github.com/SoloJiang/lectern-app/issues/27)
- ADR：[0010](../../docs/adr/0010.md)、[0018](../../docs/adr/0018.md)

## 边界

- 不含任何业务功能。
- 进程插件宿主和 grammar 加载器只编排生命周期和协议；Seatbelt、代码签名校验、`dlopen` 由 `platform` 实现 kernel 定义的接口（[ADR 0018](../../docs/adr/0018.md)），kernel 不依赖 `platform`。
- 插件激活耗时和常驻内存要能被 `bench` 逐个测量。

## 测试

不依赖 GPUI，在任何平台上都能编译和测试：

```sh
cargo check -p lectern-kernel     # 只改本 crate 时最快的反馈
just test -p lectern-kernel
```

- 单元测试写在模块旁的 `#[cfg(test)] mod tests`；结构化输出（解析结果、归因、diff）用 insta 快照验收，快照改动要在 PR 里逐个看过。
- 真实数据放 `tests/fixtures/`，先脱敏；格式随上游版本变化的，每个版本留一份样本。
