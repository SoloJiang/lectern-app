# fs_watch（`lectern-fs-watch`）

与平台无关的文件监听接口。工作区监听的事件按 git index 过滤；另有一个不过滤的 git 元数据监听，监视 git 公共目录里的 reflog（`logs/HEAD`、`logs/refs/heads/`、`worktrees/*/logs/HEAD`），给每条新增记录打上亚秒的监听时刻和序号（FSEvents 是异步的，监听时刻只能证明记录写在它之前），供 `usage` 还原分支切换的先后，也监视 `logs/refs/stash`（[ADR 0020](../../docs/adr/0020.md)）。工作区监听的过滤另外接受一份临时的不过滤路径清单（如候选 stash 用 `-u` / `-a` 存下的未跟踪和被忽略文件），清单里的路径不按 index 过滤，事件照常交付，清单由调用方随候选 stash 的增减更新。

- 层：基础设施层。只依赖基础设施和 kernel 层；不依赖 GPUI。
- 设计文档：`docs/design/fs_watch.md`，尚未编写。先按[模板](../../docs/design/TEMPLATE.md)编写，设计文档 PR 合并后再写代码。
- ADR：[0006](../../docs/adr/0006.md)、[0020](../../docs/adr/0020.md)

## 边界

- FSEvents 后端在 `platform`，由 `app` 装配时注入；本 crate 不出现 macOS 专属代码（架构规则 6）。
- 测试用内存里的模拟后端驱动事件。

## 测试

不依赖 GPUI，在任何平台上都能编译和测试：

```sh
cargo check -p lectern-fs-watch     # 只改本 crate 时最快的反馈
just test -p lectern-fs-watch
```

- 单元测试写在模块旁的 `#[cfg(test)] mod tests`；结构化输出（解析结果、归因、diff）用 insta 快照验收，快照改动要在 PR 里逐个看过。
- 真实数据放 `tests/fixtures/`，先脱敏；格式随上游版本变化的，每个版本留一份样本。
