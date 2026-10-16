# forge（`lectern-forge`）

GitHub 与 GitLab（含自部署）的 MR 与 CI 状态。

- 层：领域层。可以依赖领域、基础设施和 kernel 层；不得直接或间接依赖 GPUI（架构规则 2）。
- 设计文档：`docs/design/forge.md`，尚未编写。先按[模板](../../docs/design/TEMPLATE.md)编写，设计文档 PR 合并后再写代码。
- issue：[#25](https://github.com/SoloJiang/lectern-app/issues/25)
- ADR：[0008](../../docs/adr/0008.md)、[0019](../../docs/adr/0019.md)

## 边界

- MVP 调用 `gh` / `glab` 命令行工具，复用它们已有的登录态（[ADR 0008](../../docs/adr/0008.md)），本 crate 不保存凭据；命令行进程按需启动、用完即退。改成直接持有凭据要先修订 ADR 0008。
- 网络请求在后台任务里执行，离线时返回缓存或明确的离线状态。
- 改动来源的接口定义在 `services`，按能力划分：取 diff 是必需的，快速判断是否过期、加载完整文件、读写审阅评论都是可选能力，视图按能力决定显示哪些功能，不判断来源种类。本 crate 为 GitHub 和 GitLab 实现这个接口，本地 git 由 `git` 实现。
- 请求处理限流和条件请求（ETag），被限流时返回缓存和明确的状态。
- MR 的指纹包含 head、base（或合并基点）和进入分析的元数据（标题、描述）的哈希，任一变化时缓存的结果按过期处理。分别请求元数据、diff、文件和评论时，开始前记下 head、base 和元数据（标题、描述）的哈希，全部请求完成后三者再取一次，不一致就重取（最多 2 次），仍不一致时这次结果不缓存、标为可能过期，不把不同版本的数据拼在一起。
- 读评论按流读取，累计原始字节上限 32 MB、条数上限 1000，单条正文超过 2 MB 时跳过，达到上限就停止翻页（ADR 0019）。
- 读取 PR 评论里带 `<!-- pulls.review data -->` 标记的分析结果，作为分组参考：按创建顺序从最新往前读之外，还按更新时间从新到旧读（GitHub 仓库级评论接口 `sort=updated&direction=desc` 按 `issue_url` 过滤，读到早于 PR 创建时间的评论即停，单独上限 5000 条、64 MB，到上限时标为「共享分析可能未找到」，GitLab notes `order_by=updated_at&sort=desc`；不用作者可设置的提交时间做下界），并按以前找到过的评论 ID 直接重读，因为 pulls.review 原地更新同一条评论；只有受信任作者（对仓库有写权限的人：GitHub 查 collaborator permission 为 `admin` / `write`，GitLab 查成员 `access_level` 不低于 Developer（30）；以及设置里的机器人账号）的直接显示，查不到权限时按未验证处理，其他作者的列为未验证、点开才显示；评论只记录 head，head 不一致的标为过期，一致的也一律标为可能过期（评论没有 base 和元数据可比）；解析失败的忽略；压缩内容按流解压，解压后超过 2 MB 即停止并丢弃。不向 PR 写入 Lectern 自己的分析（[ADR 0019](../../docs/adr/0019.md)）。设计依据见 [pulls.review 调研](../../docs/research/pulls-review.md#对-lectern-的建议)。

## 测试

不依赖 GPUI，在任何平台上都能编译和测试：

```sh
cargo check -p lectern-forge     # 只改本 crate 时最快的反馈
just test -p lectern-forge
```

- 单元测试写在模块旁的 `#[cfg(test)] mod tests`；结构化输出（解析结果、归因、diff）用 insta 快照验收，快照改动要在 PR 里逐个看过。
- 真实数据放 `tests/fixtures/`，先脱敏；格式随上游版本变化的，每个版本留一份样本。
