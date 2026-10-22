# logging（`lectern-logging`）

运行日志：调用点的 `error!`、`warn!`、`info!`、`debug!`、`trace!` 宏、类型化字段和关联作用域；落盘部分（`sink` feature）负责首帧前的内存缓冲、写盘线程、文件滚动与清理、过滤与详细日志、panic 钩子和导出（[ADR 0046](../../docs/adr/0046.md)）。

- 层：kernel 层。只依赖 kernel 层和外部的 `log` crate，不依赖 `platform`、`executor` 和 GPUI；任何层都可以依赖本 crate。
- 设计文档：[docs/design/logging.md](../../docs/design/logging.md)。第 1 步（本 crate、`clippy.toml`、架构检查、`log/` 基准）已实现，其他 crate 的接入见设计文档「对其他 crate 的要求」。
- ADR：[0046](../../docs/adr/0046.md)、[0017](../../docs/adr/0017.md)（首帧前不做 IO）、[0018](../../docs/adr/0018.md)（安装包体积）

## 边界

- 调用线程不做 IO：记录在调用线程上编码，放进有界缓冲，文件操作只在 `lectern-log` 线程上进行。首帧之前不打开文件、不建线程，唯一的例外是写盘线程启动前发生的 panic。
- 消息只接受字符串字面量，字段值只经 `Value` 的构造函数产出；不提供接受运行时字符串的接口。文件内容、提示词与回复、会话内容、token 和凭据不进日志，详细日志也一样。
- `sink` feature 只由 `app` 和 tool 层启用；`Value::panic_message` 只在 `kernel`、`executor` 和本 crate 里调用。两条都由架构检查确认。
- 日志目录、系统版本和线程 QoS 由 `app` 从 `platform` 取得后传入，本 crate 不调用 macOS 接口。

## 测试

不依赖 GPUI，在任何平台上都能编译和测试：

```sh
cargo check -p lectern-logging     # 只改本 crate 时最快的反馈
just test -p lectern-logging
just bench --measure log/          # log/ 的基准
```

- 单元测试在 `src/tests/`，只在开启 `sink` feature 时编译（`just test` 带 `--all-features`），用 `ManualClock` 和临时目录，不靠 sleep；编码结果和过滤解析用 insta 快照验收。每个测试经 `Fixture` 安装日志器，`cargo test` 在同一进程里运行时也按顺序执行。
- `tests/no_alloc.rs` 用按线程计数的全局分配器检查写出的调用不分配内存。
- 隐私规则用 `tests/ui/` 的 trybuild 编译失败测试和自由文本处理的测试覆盖；金丝雀字符串不得出现在测试写出的任何文件里。
- 文件操作经测试钩子记录发起的线程，「UI 线程上没有 IO」和「首帧前没有 IO」都按钩子的记录判断。
