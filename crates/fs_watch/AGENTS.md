# fs_watch（`lectern-fs-watch`）

与平台无关的文件监听接口。工作区监听的事件按 git index 过滤；另有一个不过滤的 git 元数据监听，监视 git 公共目录里的 reflog（`logs/HEAD`、`logs/refs/heads/`、`worktrees/*/logs/HEAD`），给每条新增记录打上亚秒的监听时刻和序号（FSEvents 是异步的，监听时刻只能证明记录写在它之前），供 `usage` 还原分支切换的先后，也监视 `logs/refs/stash`（[ADR 0020](../../docs/adr/0020.md)）。工作区监听的过滤另外接受一份临时的不过滤路径清单（如候选 stash 用 `-u` / `-a` 存下的未跟踪和被忽略文件），清单里的路径不按 index 过滤，事件照常交付，清单由调用方随候选 stash 的增减更新。

- 层：基础设施层。只依赖基础设施和 kernel 层；不依赖 GPUI。
- 设计文档：[docs/design/fs_watch.md](../../docs/design/fs_watch.md)。
- issue：[#14](https://github.com/SoloJiang/lectern-app/issues/14)
- ADR：[0006](../../docs/adr/0006.md)、[0019](../../docs/adr/0019.md)、[0020](../../docs/adr/0020.md)、[0024](../../docs/adr/0024.md)、[0026](../../docs/adr/0026.md)、[0046](../../docs/adr/0046.md)（运行日志）

## 边界

- FSEvents 与 kqueue 后端在 `platform`，由 `app` 装配时注入；本 crate 不出现 macOS 专属代码（架构规则 6）。过滤器与配置文件清单由 `git` 实现本 crate 定义的 `WorktreeSource` 提供，本 crate 不依赖 `git`。
- 按 worktree 维护单调递增的修订号：不被 `.gitignore` 忽略的路径上每到一批事件加一（`.gitignore`、`.gitattributes` 文件本身即使被忽略规则排除也照样计入，因为 git 仍然应用它们），worktree 的 git 目录里的 `HEAD`、`index`，以及公共目录里的 `refs/`、`packed-refs`，以及影响 diff 和快照内容的仓库配置：`config`（含 linked worktree 的 `config.worktree`）、`info/exclude`、`info/attributes` 和 `core.excludesFile`、`core.attributesFile` 指向的文件（没有设置时 git 读默认位置 `$XDG_CONFIG_HOME/git/ignore`、`$XDG_CONFIG_HOME/git/attributes`，`XDG_CONFIG_HOME` 未设置时是 `~/.config`，这两个文件同样监视），以及 `git` 实际读取的系统级属性文件；仓库用 reftable 存引用（`extensions.refStorage=reftable`）时监视的是 `reftable/` 目录，不是 `refs/` 和 `packed-refs`；仓库之外参与生效配置的文件（系统、全局、XDG 配置和 `include.path` / `includeIf` 引入的文件，清单由 `git` 从 gix 已加载配置的来源和 `include` 值得出，不运行 git 命令行，见设计文档「配置文件清单」）也监视（是符号链接的，同时监视链接本身和解析后的每一级目标，dotfiles 常用链接管理这些文件），其中任何一个变化时修订号加一，并重新计算这份清单和它们指向的文件；`HEAD` 变化时也重新计算，因为 `includeIf "onbranch:…"` 按当前检出的分支决定引入哪些文件（linked worktree 的 `HEAD`、`index`、`config.worktree` 在 `<公共目录>/worktrees/<名字>/` 下）上的事件同样加一（提交、`reset`、`update-index` 不写工作区文件也会改变基准），并提供「先交付已发生的事件再读修订号」的接口（FSEvents 后端用流的同步 flush），供 Analyze 判断结果显示前的指纹计算期间工作区有没有变（[ADR 0019](../../docs/adr/0019.md)）。
- 另提供不按 git index 过滤的递归目录监听，供 `agents` 监视 git 仓库之外的会话目录（`~/.claude/projects`、`~/.codex/sessions` 等），事件只带路径、合并后交付（[ADR 0026](../../docs/adr/0026.md)）。
- 日志经 `lectern-logging` 的宏（[ADR 0046](../../docs/adr/0046.md)），不写 stderr；字段值的写法集中在 `src/diag.rs`，只记监听的根（`Value::path`）、监听的种类和时长。批次里的路径、reflog 行和引用名、文件内容、错误文字和 panic 消息不进日志；分类、reflog 读取、合并和交付的热路径上不加记录（设计「日志与诊断」）。
- 测试用内存里的模拟后端驱动事件。

## 测试

不依赖 GPUI，在任何平台上都能编译和测试：

```sh
cargo check -p lectern-fs-watch     # 只改本 crate 时最快的反馈
just test -p lectern-fs-watch
```

- 单元测试写在模块旁的 `#[cfg(test)] mod tests`；结构化输出（解析结果、归因、diff）用 insta 快照验收，快照改动要在 PR 里逐个看过。
- 监听器的行为测试在 `src/tests/`，按设计文档「测试」一节分组（`filter_*`、`revision_*`、`reflog_*` 等），经公开接口和 `testing` 驱动：`testing::manual` 加 `Executor::manual()`，假时钟由 `Driver::advance` 推进；要让加载任务停在半路（过期的过滤器）时只处理队列、不运行 executor（`driver.shared().pump(true)`）。批次渲染成文本后用内联快照验收（`tests/support.rs` 的 `worktree_batches`、`repo_batches`、`dir_batches`）。
- reflog 和配置清单的测试用 `tempfile` 的真实目录（`FakeBackend::arm` 对不存在的路径返回 `NotFound`）；不读本机真实的 git 配置或会话目录。
- `tests/reflog_memory.rs` 是单独的测试二进制，用按线程计数的全局分配器测每个 reflog 文件的进度占用和 1000 个分支的仓库状态（设计文档「性能」）；改动 reflog 进度的结构时运行它（`just test -p lectern-fs-watch --no-capture reflog_progress` 打印实测值），并同步「性能」里的数字。
- 静止测试用 kernel 的 `TestKernel` 取真实的守卫；`RecordingSink` 在 `take` 之前一直持有守卫。
- `logs_*`（`src/tests/logs.rs`）用 `lectern-logging` 的 `testing::capture()` 在 trace 级别检查记录，金丝雀放在受跟踪文件的内容、reflog 行、分支名和 sink 的 panic 消息里，`assert_absent` 确认它们不进日志。`cargo test` 在同一进程里共用捕获器，按消息、根路径和 `timeout_us` 筛选记录。
- 真实数据放 `tests/fixtures/`，先脱敏；格式随上游版本变化的，每个版本留一份样本。
- 性能预算的基准在 `crates/bench/benches/time/fs_watch.rs`（`fs_watch/classify_burst`、`fs_watch/reflog_append`），同样经 `testing::manual`、`FakeBackend` 和 `FakeSource` 驱动；事件在计时之外构造，用 `FakeBackend::emit_batch` 注入。改动分类、reflog 读取或 `testing` 时运行 `just bench --measure fs_watch/`（Linux 上只报告，macOS CI 判定预算）。
