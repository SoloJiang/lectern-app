# fs_watch（`lectern-fs-watch`）

与平台无关的文件监听接口，事件按 git index 过滤。

- 层：基础设施层。只依赖基础设施和 kernel 层；不依赖 GPUI。
- 设计文档：`docs/design/fs_watch.md`，尚未编写。先按[模板](../../docs/design/TEMPLATE.md)编写，设计文档 PR 合并后再写代码。
- ADR：[0006](../../docs/adr/0006.md)

## 边界

- FSEvents 后端在 `platform`，由 `app` 装配时注入；本 crate 不出现 macOS 专属代码（架构规则 6）。
- 按 worktree 维护单调递增的修订号：不被 `.gitignore` 忽略的路径上每到一批事件加一，worktree 的 git 目录里的 `HEAD`、`index`，以及公共目录里的 `refs/`、`packed-refs`，以及影响 diff 和快照内容的仓库配置：`config`（含 linked worktree 的 `config.worktree`）、`info/exclude`、`info/attributes` 和 `core.excludesFile`、`core.attributesFile` 指向的文件（linked worktree 的 `HEAD`、`index`、`config.worktree` 在 `<公共目录>/worktrees/<名字>/` 下）上的事件同样加一（提交、`reset`、`update-index` 不写工作区文件也会改变基准），并提供「先交付已发生的事件再读修订号」的接口（FSEvents 后端用 `FSEventStreamFlushSync`），供 Analyze 判断结果显示前的指纹计算期间工作区有没有变（[ADR 0019](../../docs/adr/0019.md)）。
- 测试用内存里的模拟后端驱动事件。

## 测试

不依赖 GPUI，在任何平台上都能编译和测试：

```sh
cargo check -p lectern-fs-watch     # 只改本 crate 时最快的反馈
just test -p lectern-fs-watch
```

- 单元测试写在模块旁的 `#[cfg(test)] mod tests`；结构化输出（解析结果、归因、diff）用 insta 快照验收，快照改动要在 PR 里逐个看过。
- 真实数据放 `tests/fixtures/`，先脱敏；格式随上游版本变化的，每个版本留一份样本。
