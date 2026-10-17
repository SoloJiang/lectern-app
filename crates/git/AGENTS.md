# git（`lectern-git`）

用 gix 读仓库、用 imara-diff 计算 diff；结构化 diff 忽略纯格式变化并识别移动的代码。

- 层：领域层。可以依赖领域、基础设施和 kernel 层；不得直接或间接依赖 GPUI（架构规则 2）。普通库 crate，不是插件，`project`、`review` 等直接调用。
- 设计文档：[docs/design/git.md](../../docs/design/git.md)。
- issue：[#14](https://github.com/SoloJiang/lectern-app/issues/14)、[#21](https://github.com/SoloJiang/lectern-app/issues/21)
- ADR：[0006](../../docs/adr/0006.md)、[0024](../../docs/adr/0024.md)

## 边界

- gix 读取对象与 index 的调用都在后台任务里执行，不在 UI 线程上。
- diff 结果以两端 blob OID 为键缓存。
- blob OID 按仓库的哈希算法取完整长度（SHA-1 为 40 位、SHA-256 为 64 位），不假定固定长度。
- 工作区 diff 包含未跟踪文件，但不写用户的 index。单个文件的 diff 超过大小上限时，只保留文件头和增删行数，并标记为截断；Analyze 需要时，截断文件改动前的那一版按版本取出（ADR 0019）。整个 diff 也有上限（默认 5000 个文件、合计 50 MB patch），超过时其余文件只列出路径和改动类型，显示截断的数量。
- 读取仓库时不执行仓库配置的任何外部程序（textconv、外部 diff、clean / process 过滤器、`core.fsmonitor` 钩子），直接读对象和工作区文件；index 的 stat 信息只用来跳过能确定未改动的文件，不能确定时（如 racy-clean 的条目）比较内容。git 内置、不执行外部程序的转换（`text` / `eol` 换行、`ident`、`working-tree-encoding`）用 gix 按属性在比较前应用；设置了 clean / process 过滤器的文件（如 Git LFS）在不执行过滤器时无法与 index 正确比较，标为「使用了过滤器，暂不支持比较」，不按原始字节生成 diff；Analyze 的结果因此标为部分内容未提供（ADR 0019）。不得不调用 git 命令行时，除了 `--no-ext-diff --no-textconv` 还要关闭 fsmonitor 和过滤器，或者在不能执行仓库程序的沙箱里运行。
- 为 Analyze 叠加工作区文件时，先经过与生成 diff 相同的内置转换（`working-tree-encoding`、行尾、`ident`，不执行外部过滤器），按规范形式写进快照，与来自 git 对象的基准文件一致。
- 读工作区文件时从 worktree 根目录描述符出发，用 `O_NOFOLLOW_ANY | O_NONBLOCK` 打开（路径中间某一级被换成符号链接时打开失败，不读到 worktree 之外），对这个描述符 `fstat` 判断类型，之后从同一个描述符读取，不按路径再打开一次；只读普通文件和符号链接（`ELOOP` 时用 `readlinkat`），FIFO、socket、设备文件拒绝（`O_NONBLOCK` 让 FIFO 的打开不会阻塞），diff 里标为「不支持的文件类型」。`O_NOFOLLOW_ANY` 是 macOS 的标志，这套打开、判断和读取由 `platform` 的安全文件读取接口实现，本 crate 只调用接口（架构规则 6）。
- 实现 `fs_watch` 的 `WorktreeSource`（`Worktree::watch_source`）：过滤器是 index 路径加一次读完的忽略规则（含未被忽略目录里未跟踪的 `.gitignore`），只在内存里查询；配置文件清单从 gix 已加载配置的来源和 `include` 值得出，不运行 git 命令行（[fs_watch 设计](../../docs/design/fs_watch.md)「对其他 crate 的要求」）。
- 不往仓库的对象库里写任何东西：未跟踪文件直接读出来与空内容比较，不用 `git add --intent-to-add`。
- 工作区的指纹由基准树 OID 和每个改动文件的路径、改动状态、新旧文件模式、完整内容（符号链接为目标）的哈希组成，在按上限截断 diff 之前计算，被截断、只列出路径的文件也计入；超过单文件读取上限（默认 64 MB）的文件不读内容，指纹里用它的大小、修改时间和 inode 代替内容哈希，Analyze 的结果标为「部分内容未提供」，不在每次刷新时扫描几十 GB 的文件（性能预算），用来判断缓存的结果是否过期。Analyze 用的 diff 和指纹从它的快照计算，不从可能还在变化的工作区计算（ADR 0019）。设计依据见 [pulls.review 调研](../../docs/research/pulls-review.md#对-lectern-的建议)。

## 测试

不依赖 GPUI，在任何平台上都能编译和测试：

```sh
cargo check -p lectern-git     # 只改本 crate 时最快的反馈
just test -p lectern-git
```

- 单元测试写在模块旁的 `#[cfg(test)] mod tests`；结构化输出（解析结果、归因、diff）用 insta 快照验收，快照改动要在 PR 里逐个看过。
- 真实数据放 `tests/fixtures/`，先脱敏；格式随上游版本变化的，每个版本留一份样本。需要和 git 输出对照的夹具是目录格式（[说明](tests/fixtures/README.md)），由 `tests/fixtures/scripts/` 用真实的 git 生成一次；测试只用 `TestRepo::import_fixture` / `import_worktree_fixture` 导入，不运行 git。
- 测试仓库用 `testing::TestRepo`（`test-support` feature）构造，不调用 git 命令行；发现仓库用 `testing::discover`，只读仓库自己的配置，不受机器上的 git 配置影响。
- git 目录里的管理文件只在 `src/gitdir.rs` 里按路径读；工作区文件只经 `SafeRoot`。
- 工作区比较的测试（`compare/tests.rs`）每次比较前后对 git 目录做快照，必须相同（只读）；用 `hook::reads` 检查哪些文件被读了。和 git 的转换结果对照的工作区夹具见[夹具说明](tests/fixtures/README.md)「工作区夹具」。
- 计时：`cargo test --release -p lectern-git --all-features -- --ignored perf_smoke --test-threads=1`（`git_open`、`git_diff_50_cold` / `warm`、`git/changes_clean`、`git/changes_50`、`git/refresh_one`、`git/changes_capped` 和内存；内存用计数分配器量，要单线程）。bench 的 runner 合并后移进 `crates/bench`。
