# terminal（`lectern-terminal`）

portable-pty + libghostty-vt 的终端状态，运行在独占线程上；会话 resume。

- 层：领域层。可以依赖领域、基础设施和 kernel 层；不得直接或间接依赖 GPUI（架构规则 2）。
- 设计文档：[docs/design/terminal.md](../../docs/design/terminal.md)（已接受；第 2 步 libghostty-vt 接入与网格快照已实现，PTY、插件与装配尚未实现）。实现按它的「验收」分四步：`services::terminal`、libghostty-vt 接入与网格快照、PTY 与插件（前置：`platform::login_env`）、`app` 装配。构建需要 Zig 0.15.2（设计文档「依赖」）。
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

- 构建要 Zig 0.15.2 在 `PATH` 上（根 AGENTS.md「命令」）。第一次构建克隆 Ghostty、编译静态库，之后按 `OUT_DIR` 复用。
- 不经 PTY 的引擎在 `test-support` 的 `testing::Engine`：`tests/frames.rs`（帧的快照、行复用、复制、主题）和 `tests/input.rs`（按键、鼠标、滚动、粘贴的编码）都用它，`bench` 的 `terminal/*` 也用它。
- 单元测试写在模块旁的 `#[cfg(test)] mod tests`；结构化输出（解析结果、归因、diff）用 insta 快照验收，快照改动要在 PR 里逐个看过。
- 真实数据放 `tests/fixtures/`，先脱敏；格式随上游版本变化的，每个版本留一份样本。
