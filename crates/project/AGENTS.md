# project（`lectern-project`）

项目模型：仓库 → worktree → 子项目；清单解析（npm / yarn / pnpm、Maven、Gradle、Go、Cargo）。`project` 插件提供 `dyn Projects` 服务。

- 层：领域层，插件 crate（`plugin = true`，[ADR 0025](../../docs/adr/0025.md)）。可以依赖 kernel 层和不是插件的库 crate（`git`、`executor`、`platform`，第 4 步起 `fs_watch`）；不依赖其他插件 crate，只有 `app` 和 tool 层依赖本 crate；不得直接或间接依赖 GPUI（架构规则 2）。
- 设计文档：[docs/design/project.md](../../docs/design/project.md)
- issue：[#9](https://github.com/SoloJiang/lectern-app/issues/9)
- ADR：[0006](../../docs/adr/0006.md)、[0007](../../docs/adr/0007.md)、[0023](../../docs/adr/0023.md)、[0025](../../docs/adr/0025.md)

## 边界

- git 项目的文件列表读 git index，不遍历目录，每条带 index 的 blob OID（`git_access::blob_id` 把 `lectern_git::Oid` 转成 `BlobId`）；普通文件夹做有上限的遍历，没有 OID。
- git 只经 `lectern-git`（`src/git_access.rs` 的 `GitAccess`），不直接依赖 gix，不运行 git 命令行和任何构建工具。
- 所有 IO 在 executor 上；`snapshot`、`locate`、`open_projects` 只取锁复制或在内存里比较。
- 增量刷新只经 `lectern-fs-watch`（`src/model/watch.rs`）：每个打开的 git 项目一个 `RepoWatch`，当前 worktree 一个 `WorktreeWatch`，非当前 worktree 不监听工作区，文件夹项目不监听。`Sink::deliver` 只标记脏、提交扫描循环，不做 IO。
- 识别出的文件类型与清单交给插件市场做激活匹配（[ADR 0018](../../docs/adr/0018.md)），匹配不在本 crate 实现。

## 测试

不依赖 GPUI，在任何平台上都能编译和测试：

```sh
cargo check -p lectern-project     # 只改本 crate 时最快的反馈
just test -p lectern-project
```

- 单元测试写在模块旁的 `#[cfg(test)] mod tests`；结构化输出（解析结果、归因、diff、快照）用 insta 快照验收，快照改动要在 PR 里逐个看过。
- 模型测试（`src/tests/`）用 `testing::FakeGit`（git 元数据在内存里，worktree 文件是临时目录里的真实文件，记录每次调用；index 每条的 OID 是暂存时工作区内容的 blob OID，与真实仓库相同）和测试里的任务队列 `QueueHost`，不经 kernel；插件测试用真实的 `Kernel` 和 `Executor::manual()`；`real_repo_*` 用 `lectern_git::testing::TestRepo` 构造仓库，不调用 git 命令行。
- 刷新测试（`refresh_*`，`src/tests/refresh.rs`）用 fs_watch 的 `testing::manual` 和 `FakeBackend` 注入事件（`tests/support.rs` 的 `Watch::settle` 交替运行 fs_watch 与 `QueueHost`），`FakeGit` 每个 worktree 的 `FakeSource` 随它的 index 更新；要在一轮扫描中途制造变化用 `FakeGit::before_read`。
- 真实数据放 `tests/fixtures/`，先脱敏；格式随上游版本变化的，每个版本留一份样本。
