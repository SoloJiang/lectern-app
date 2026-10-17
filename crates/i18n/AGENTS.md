# i18n（`lectern-i18n`）

Fluent 资源与语言切换：en 与 zh-CN，默认跟随系统语言（[ADR 0013](../../docs/adr/0013.md)）。

- 层：基础设施层。只依赖基础设施和 kernel 层；不依赖 GPUI。
- 设计文档：[docs/design/i18n.md](../../docs/design/i18n.md)
- issue：[#28](https://github.com/SoloJiang/lectern-app/issues/28)
- ADR：[0013](../../docs/adr/0013.md)

## 边界

- 所有界面字符串都在这里的资源文件里，视图不硬编码。
- en 与 zh-CN 的键必须一致，由 CI 检查（[#28](https://github.com/SoloJiang/lectern-app/issues/28)）。
- 额外的界面语言作为数据包从插件市场下载（[ADR 0018](../../docs/adr/0018.md)）。
- 运行时没有依赖：`build.rs` 在构建时解析、校验 `locales/` 并生成静态表格和 `keys` 模块，`fluent-syntax` 只是构建依赖，`fluent-bundle` 等只用于测试。不要给 `[dependencies]` 加 Fluent 相关的 crate。
- 不读写系统语言设置（归 `platform`），不做 GPUI 胶水（归 `ui`）。

## 目录

- `locales/<语言>/<文件>.ftl`：en 与 zh-CN 同名同键；键以文件名加 `-` 开头。新增界面文案时两种语言同时加。
- `build.rs`、`build/compile.rs`（解析与全部编译期校验）、`build/codegen.rs`（生成 Rust 代码）。
- `src/`：`Locale`、`Strings`、`Args`、`count_compact` 和手写的复数规则；`src/reference_tests.rs` 与 fluent-bundle 对照；`src/testing.rs`（`test-support`）的 `frame_strings` 供 `lectern-bench` 的 `i18n/frame_strings`。

## 测试

不依赖 GPUI，在任何平台上都能编译和测试：

```sh
cargo check -p lectern-i18n     # 只改本 crate 时最快的反馈
just test -p lectern-i18n
```

- 单元测试写在模块旁的 `#[cfg(test)] mod tests`；格式化结果用 insta 快照验收（`src/snapshots/`），快照改动要在 PR 里逐个看过。
- 编译期校验：`tests/validation.rs` 引入 `build/compile.rs`，对 `tests/fixtures/<规则>/{en,zh-CN}/` 下的每组错误资源断言报错（`tests/snapshots/`）。新增校验规则时加一组 fixture 和一个测试。
- 键的类型：`tests/compile_fail.rs` 用 trybuild 编译 `tests/ui/*.rs`，期望的编译错误在同名 `.stderr` 里。
- 更新快照：`INSTA_UPDATE=always just test -p lectern-i18n`；更新 `.stderr`：`TRYBUILD=overwrite just test -p lectern-i18n`。之后用 `git diff` 逐个检查。
- `just doctest` 运行公开 API 的示例和两个 `compile_fail` 示例。
