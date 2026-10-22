# executor（`lectern-executor`）

后台任务：UI 线程之外的 IO、解析和 git 操作。

- 层：基础设施层。只依赖基础设施和 kernel 层（`lectern-kernel`、`lectern-logging`）；不依赖 GPUI。
- 设计文档：[docs/design/executor.md](../../docs/design/executor.md)
- ADR：[0017](../../docs/adr/0017.md)、[0029](../../docs/adr/0029.md)（批量线程组）、[0046](../../docs/adr/0046.md)（运行日志）

## 边界

- 任务可以取消，取消后不再向视图返回结果。
- 首帧之前不提交任务（[ADR 0017](../../docs/adr/0017.md)）。
- 日志只经 `lectern-logging` 的宏写出，不写 stderr；每个任务带 `task` 序号和提交方的作用域（有闸门时是闸门的 `plugin`、`act`），每一步外设、步末恢复，不分配、不加锁；任务的 panic 记 `task panicked`，消息只经 `Value::panic_message`（[设计「日志与诊断」](../../docs/design/executor.md#日志与诊断)）。

## 测试

不依赖 GPUI，在任何平台上都能编译和测试：

```sh
cargo check -p lectern-executor     # 只改本 crate 时最快的反馈
just test -p lectern-executor
```

- 测试在 `src/tests/`，用真实的 `lectern-kernel`（`test-support`）驱动 `own_with` 和闸门；要确定顺序的用 `Executor::manual()` + `run_until_idle()`（本 crate 的 `test-support` feature，其他 crate 的测试也用它）。等待用通道或 `block_on`，不靠 sleep。
- 单元测试写在模块旁的 `#[cfg(test)] mod tests`；结构化输出（解析结果、归因、diff）用 insta 快照验收，快照改动要在 PR 里逐个看过。
- 真实数据放 `tests/fixtures/`，先脱敏；格式随上游版本变化的，每个版本留一份样本。
- 日志用 `lectern_logging::testing::capture()` 检查（`src/tests/logs.rs`），含金丝雀测试；同一进程里的测试共用一个捕获器，按激活序号和任务序号筛选记录。改了提交或 poll 的路径要跑 `just bench --measure executor/`，与改动前对照。
