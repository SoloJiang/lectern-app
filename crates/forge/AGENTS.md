# forge（`lectern-forge`）

GitHub 与 GitLab（含自部署）的 MR 与 CI 状态。

- 层：领域层。可以依赖领域、基础设施和 kernel 层；不得直接或间接依赖 GPUI（架构规则 2）。
- 设计文档：`docs/design/forge.md`，尚未编写。先按[模板](../../docs/design/TEMPLATE.md)编写，设计文档 PR 合并后再写代码。
- issue：[#25](https://github.com/SoloJiang/lectern-app/issues/25)
- ADR：[0008](../../docs/adr/0008.md)、[0020](../../docs/adr/0020.md)

## 边界

- MVP 调用 `gh` / `glab` 命令行工具，复用它们已有的登录态（[ADR 0008](../../docs/adr/0008.md)），本 crate 不保存凭据；命令行进程按需启动、用完即退。改成直接持有凭据要先修订 ADR 0008。
- 网络请求在后台任务里执行，离线时返回缓存或明确的离线状态。
- 为用量统计（ADR 0020）提供分支与 MR 的对应：每个 MR 的编号、标题、源仓库与源分支、目标仓库与目标分支（仓库按代码平台主机加平台给仓库的不变 ID 标识（GitHub 的 repository id、GitLab 的 project id），改名或转移后不变，区分 fork；owner/name 只用于显示）、源分支的 head、状态变化的完整时间线（打开、关闭、重新打开、合并及其时间），包括已经合并和关闭的 MR，以便按 MR 的存续时间切分历史用量。

## 测试

不依赖 GPUI，在任何平台上都能编译和测试：

```sh
cargo check -p lectern-forge     # 只改本 crate 时最快的反馈
just test -p lectern-forge
```

- 单元测试写在模块旁的 `#[cfg(test)] mod tests`；结构化输出（解析结果、归因、diff）用 insta 快照验收，快照改动要在 PR 里逐个看过。
- 真实数据放 `tests/fixtures/`，先脱敏；格式随上游版本变化的，每个版本留一份样本。
