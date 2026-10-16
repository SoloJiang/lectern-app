# i18n（`lectern-i18n`）

Fluent 资源与语言切换：en 与 zh-CN，默认跟随系统语言（[ADR 0013](../../docs/adr/0013.md)）。

- 层：基础设施层。只依赖基础设施和 kernel 层；不依赖 GPUI。
- 设计文档：`docs/design/i18n.md`，尚未编写。先按[模板](../../docs/design/TEMPLATE.md)编写，设计文档 PR 合并后再写代码。
- issue：[#28](https://github.com/SoloJiang/lectern-app/issues/28)
- ADR：[0013](../../docs/adr/0013.md)

## 边界

- 所有界面字符串都在这里的资源文件里，视图不硬编码。
- en 与 zh-CN 的键必须一致，由 CI 检查（[#28](https://github.com/SoloJiang/lectern-app/issues/28)）。
- 额外的界面语言作为数据包从插件市场下载（[ADR 0018](../../docs/adr/0018.md)）。

## 测试

不依赖 GPUI，在任何平台上都能编译和测试：

```sh
cargo check -p lectern-i18n     # 只改本 crate 时最快的反馈
just test -p lectern-i18n
```

- 单元测试写在模块旁的 `#[cfg(test)] mod tests`；结构化输出（解析结果、归因、diff）用 insta 快照验收，快照改动要在 PR 里逐个看过。
- 真实数据放 `tests/fixtures/`，先脱敏；格式随上游版本变化的，每个版本留一份样本。
