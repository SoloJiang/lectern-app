# code_intel（`lectern-code-intel`）

tree-sitter 符号索引（tags → SQLite，增量更新）、Java / TS 解析、跨子项目导航、依赖源码跳转。

- 层：领域层。可以依赖领域、基础设施和 kernel 层；不得直接或间接依赖 GPUI（架构规则 2）。
- 设计文档：`docs/design/code_intel.md`，尚未编写。先按[模板](../../docs/design/TEMPLATE.md)编写，设计文档 PR 合并后再写代码。
- issue：[#17](https://github.com/SoloJiang/lectern-app/issues/17)、[#18](https://github.com/SoloJiang/lectern-app/issues/18)、[#19](https://github.com/SoloJiang/lectern-app/issues/19)、[#26](https://github.com/SoloJiang/lectern-app/issues/26)
- ADR：[0007](../../docs/adr/0007.md)、[0018](../../docs/adr/0018.md)

## 边界

- 核心只带常用语言的 grammar；其他语言包和 jdtls / SCIP 是插件市场里的可选插件（[ADR 0018](../../docs/adr/0018.md)）。
- 外部语言服务器按需启动、空闲即关（架构规则 3）。

## 测试

不依赖 GPUI，在任何平台上都能编译和测试：

```sh
cargo check -p lectern-code-intel     # 只改本 crate 时最快的反馈
just test -p lectern-code-intel
```

- 单元测试写在模块旁的 `#[cfg(test)] mod tests`；结构化输出（解析结果、归因、diff）用 insta 快照验收，快照改动要在 PR 里逐个看过。
- 真实数据放 `tests/fixtures/`，先脱敏；格式随上游版本变化的，每个版本留一份样本。
