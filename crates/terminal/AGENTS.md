# terminal（`lectern-terminal`）

portable-pty + libghostty-vt 的终端状态，运行在独占线程上；会话 resume。

- 层：领域层，插件 crate（`plugin = true`，[ADR 0025](../../docs/adr/0025.md)）：插件 "terminal"（`TerminalPlugin`）提供 `dyn Terminals`，依赖 `dyn Projects` 与 `dyn SessionSource`，`AfterFirstFrame` 激活。只依赖 kernel 层（`kernel`、`services`、`logging`）和非插件的库 crate（`executor`、`platform`）；`project`、`agents` 是插件，只经 `services` 的接口往来。不得直接或间接依赖 GPUI（架构规则 2）。
- 设计文档：[docs/design/terminal.md](../../docs/design/terminal.md)（已接受；第 2 步 libghostty-vt 接入与网格快照、第 3 步 PTY、启动、插件与服务已实现，`app` 装配尚未实现）。实现按它的「验收」分四步：`services::terminal`、libghostty-vt 接入与网格快照、PTY 与插件（前置：`platform::login_env`）、`app` 装配。构建需要 Zig 0.15.2（设计文档「依赖」）。
- issue：[#5](https://github.com/SoloJiang/lectern-app/issues/5)、[#12](https://github.com/SoloJiang/lectern-app/issues/12)
- ADR：[0002](../../docs/adr/0002.md)

## 边界

- 终端内核只接 libghostty-vt（[ADR 0002](../../docs/adr/0002.md)）；libghostty 用 vendored 源码，升级先确认。
- 向视图提供网格快照；视图不直接读写 PTY。
- 恢复命令是安全边界（设计「安全边界」）：程序和固定参数只来自 `src/launch.rs` 里的表，来源只给出会话 ID；`validate` 在调用线程上按顺序检查，`resolve` 在终端线程上规范化目录、再核对项目。agent 不经 shell 启动；不要在 `src/` 里加 `Command::new`、`"sh"`、`"-c"`（验收用 `rg` 查）。
- PTY 只经 `SlavePty::spawn_command` 启动、主端用原始 fd 非阻塞读写；不用 `try_clone_reader`、`take_writer`（读线程无法被打断），挂断只经 `ChildKiller`（`src/pty.rs` 里的唯一一处）。
- 所有 `fork` 经 `src/pty.rs` 的 `SPAWN` 锁，同一时刻只有一个线程 `fork`：多线程进程里并发的 `fork`，子进程会在 libSystem 的 atfork 处理里中止并被 `SIGKILL`（设计「已知限制」第 8 条）。不要绕过 `pty::spawn` 启动进程。
- 线程与 IO：`open`、`input`、`frame`、`list`、`info`、`watch`、`close` 都不阻塞、不做 IO（`src/service.rs`）；登录环境、PTY、子进程都在 `terminal-<id>` 线程上（`src/thread.rs`）。`terminal-<id>-wait` 只等 `Child::wait`。两个线程的主体都在 `catch` 里，panic 只让这一个终端 `Failed(Internal)`。
- 日志经 `lectern-logging` 的宏（[ADR 0046](../../docs/adr/0046.md)），字段值集中在 `src/diag.rs`；终端输出、标题、选区、粘贴与输入的文字、argv、环境变量、`$SHELL` 和 `PATH` 里的路径、`Theme` 的颜色不进日志（金丝雀测试 `logs_hold_no_terminal_content`）。

## 测试

不依赖 GPUI，在任何平台上都能编译和测试（构建和测试都带 `--all-features`，`just test` 已经带上）：

```sh
cargo check -p lectern-terminal     # 只改本 crate 时最快的反馈
just test -p lectern-terminal
```

- 构建要 Zig 0.15.2 在 `PATH` 上（根 AGENTS.md「命令」）。第一次构建克隆 Ghostty、编译静态库，之后按 `OUT_DIR` 复用。
- 不经 PTY 的引擎在 `test-support` 的 `testing::Engine`：`tests/frames.rs`（帧的快照、行复用、复制、主题）和 `tests/input.rs`（按键、鼠标、滚动、粘贴的编码）都用它，`bench` 的 `terminal/*` 也用它。
- 经 PTY 的测试用假程序 `lectern-terminal-fake`（`src/bin/fake.rs`，`required-features = ["test-support"]`，集成测试从 `CARGO_BIN_EXE_lectern-terminal-fake` 取路径）：测试把它链接成 `claude`、`codex`（放在 `with_login_env` 给的 `PATH` 里）或作 `with_shell` 的 shell；它把 argv、工作目录和环境变量名记进 `FAKE_TERMINAL_DIR/<pid>.txt`，按 `FAKE_TERMINAL_DIR/plan`（逐步的脚本，格式见文件头注释）回放输出。不用真实的 agent、不运行用户的 shell、不依赖网络和登录状态；夹具是合成的。
  - `tests/common/mod.rs`：`Kit`（临时目录、假 agent、项目目录、环境）。
  - `tests/launch.rs`：`launch_*`、`resume_refuses_*`、`no_echo_of_program_text`。
  - `tests/lifecycle.rs`：线程、替换、`close_*`、项目关闭、停用、退出码、`input_overflow_*`、`panic_fails_only_that_terminal`、静止、并发启动、事件、复制。
  - `tests/pty_input.rs`：`input_*` 经真实的 PTY（程序读到什么、窗口大小、进程退出之后）。
  - `tests/logs.rs`：`logs_hold_no_terminal_content`（`testing::capture()`，trace 级别）。
- `testing::Harness` 把插件放进 `TestKernel`，配假的 `Projects`（`FakeProjects`，按根目录前缀 `locate`，根目录要规范化：macOS 上临时目录在 `/private/var`）和两个假的 `SessionSource`；`TerminalPlugin::with_shell`、`with_login_env`、`with_thread_hook` 只在测试和 `test-support` 下存在。
- 单元测试写在模块旁的 `#[cfg(test)] mod tests`；结构化输出（解析结果、归因、diff）用 insta 快照验收，快照改动要在 PR 里逐个看过。
- 真实数据放 `tests/fixtures/`，先脱敏；格式随上游版本变化的，每个版本留一份样本。
- 基准：`just bench terminal/`；`terminal/open`、`terminal/input`、`terminal/echo_latency` 用 `Harness` 和 `/bin/cat`（`crates/bench/benches/time/terminal.rs`）。基准和测试都不打开窗口。
