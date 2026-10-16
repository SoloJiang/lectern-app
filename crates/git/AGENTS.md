# git（`lectern-git`）

用 gix 读仓库、用 imara-diff 计算 diff；结构化 diff 忽略纯格式变化并识别移动的代码。

- 层：领域层。可以依赖领域、基础设施和 kernel 层；不得直接或间接依赖 GPUI（架构规则 2）。
- 设计文档：`docs/design/git.md`，尚未编写。先按[模板](../../docs/design/TEMPLATE.md)编写，设计文档 PR 合并后再写代码。
- issue：[#14](https://github.com/SoloJiang/lectern-app/issues/14)、[#21](https://github.com/SoloJiang/lectern-app/issues/21)
- ADR：[0006](../../docs/adr/0006.md)

## 边界

- gix 读取对象与 index 的调用都在后台任务里执行，不在 UI 线程上。
- diff 结果以两端 blob OID 为键缓存。
- blob OID 按仓库的哈希算法取完整长度（SHA-1 为 40 位、SHA-256 为 64 位），不假定固定长度。
- 工作区 diff 包含未跟踪文件，但不写用户的 index。单个文件的 diff 超过大小上限时，只保留文件头和增删行数，并标记为截断；Analyze 需要时，截断文件改动前的那一版按版本取出（ADR 0019）。整个 diff 也有上限（默认 5000 个文件、合计 50 MB patch），超过时其余文件只列出路径和改动类型，显示截断的数量。
- 读取仓库时不执行仓库配置的任何外部程序（textconv、外部 diff、clean / process 过滤器、`core.fsmonitor` 钩子），直接读对象和工作区文件；index 的 stat 信息只用来跳过能确定未改动的文件，不能确定时（如 racy-clean 的条目）比较内容。git 内置、不执行外部程序的转换（`text` / `eol` 换行、`ident`、`working-tree-encoding`）用 gix 按属性在比较前应用；设置了 clean / process 过滤器的文件（如 Git LFS）在不执行过滤器时无法与 index 正确比较，标为「使用了过滤器，暂不支持比较」，不按原始字节生成 diff。不得不调用 git 命令行时，除了 `--no-ext-diff --no-textconv` 还要关闭 fsmonitor 和过滤器，或者在不能执行仓库程序的沙箱里运行。
- 读工作区文件时用 `O_NOFOLLOW | O_NONBLOCK` 打开，对这个描述符 `fstat` 判断类型，之后从同一个描述符读取，不按路径再打开一次；只读普通文件和符号链接（`ELOOP` 时用 `readlinkat`），FIFO、socket、设备文件拒绝（`O_NONBLOCK` 让 FIFO 的打开不会阻塞），diff 里标为「不支持的文件类型」。
- 不往仓库的对象库里写任何东西：未跟踪文件直接读出来与空内容比较，不用 `git add --intent-to-add`。
- 工作区的指纹由基准树 OID 和每个改动文件的路径、改动状态、新旧文件模式、完整内容（符号链接为目标）的哈希组成，在按上限截断 diff 之前计算，被截断、只列出路径的文件也计入，用来判断缓存的结果是否过期。Analyze 用的 diff 和指纹从它的快照计算，不从可能还在变化的工作区计算（ADR 0019）。设计依据见 [pulls.review 调研](../../docs/research/pulls-review.md#对-lectern-的建议)。

## 测试

不依赖 GPUI，在任何平台上都能编译和测试：

```sh
cargo check -p lectern-git     # 只改本 crate 时最快的反馈
just test -p lectern-git
```

- 单元测试写在模块旁的 `#[cfg(test)] mod tests`；结构化输出（解析结果、归因、diff）用 insta 快照验收，快照改动要在 PR 里逐个看过。
- 真实数据放 `tests/fixtures/`，先脱敏；格式随上游版本变化的，每个版本留一份样本。
