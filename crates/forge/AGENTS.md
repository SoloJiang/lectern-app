# forge（`lectern-forge`）

GitHub 与 GitLab（含自部署）的 MR 与 CI 状态。

- 层：领域层。可以依赖领域、基础设施和 kernel 层；不得直接或间接依赖 GPUI（架构规则 2）。
- 设计文档：[docs/design/forge.md](../../docs/design/forge.md)（已接受，尚未实现）。
- issue：[#25](https://github.com/SoloJiang/lectern-app/issues/25)
- ADR：[0008](../../docs/adr/0008.md)、[0019](../../docs/adr/0019.md)、[0020](../../docs/adr/0020.md)

## 边界

- MVP 经 `gh api` / `glab api` 访问代码平台的 REST 接口，复用它们已有的登录态（[ADR 0008](../../docs/adr/0008.md)），本 crate 不保存、不读取凭据（不调用 `auth` 子命令）；命令行进程按需启动、用完即退，环境变量按设计文档「命令行的调用」的白名单传入。改成直接持有凭据要先修订 ADR 0008。
- 只向允许表里的主机发请求：内置的 github.com、gitlab.com，以及用户确认过的主机。不在表里的主机不启动任何子进程；响应里的 URL 一律不请求（设计文档「主机与允许表」）。
- 网络请求在后台任务里执行，离线时返回缓存或明确的离线状态。
- 改动来源的接口定义在 `services`，按能力划分：取 diff 是必需的，快速判断是否过期、加载完整文件、读写审阅评论都是可选能力，视图按能力决定显示哪些功能，不判断来源种类。本 crate 为 GitHub 和 GitLab 实现这个接口，本地 git 由 `git` 实现。代码平台的 diff 接口有文件数上限（GitHub 的 PR 文件接口最多返回 3000 个文件，GitLab 的 MR diff 超限时标 `overflow`）：所以 MR 的 diff 在本地算：head 和合并基点都在项目的对象库里时由 `git` 生成完整 diff；不在本地时用代码平台的文件列表，结果标为「部分内容未提供」并说明缺少多少个文件，不当作完整的 diff。是否由 Lectern 自己取回缺少的提交见设计文档「未决问题」2。
- 请求处理限流和条件请求（ETag），被限流时返回缓存和明确的状态。
- MR 的指纹包含 head、base（或合并基点）和进入分析的元数据（标题、描述）的哈希，任一变化时缓存的结果按过期处理。分别请求元数据、diff、文件和评论时，开始前记下 head、base 和元数据（标题、描述）的哈希，全部请求完成后三者再取一次，不一致就重取（最多 2 次），仍不一致时这次结果不缓存、标为可能过期，不把不同版本的数据拼在一起。
- 读评论按流读取，累计原始字节上限 32 MB、条数上限 1000，单条正文超过 2 MB 时跳过，达到上限就停止翻页（ADR 0019）。
- 读取 PR 评论里带 `<!-- pulls.review data -->` 标记的分析结果，作为分组参考：按创建顺序从最新往前读之外，还按更新时间从新到旧读（GitHub 仓库级评论接口 `sort=updated&direction=desc` 按 `issue_url` 过滤，读到早于 PR 创建时间的评论即停，单独上限 5000 条、64 MB，到上限时标为「共享分析可能未找到」，GitLab notes `order_by=updated_at&sort=desc`，到上限时同样标为「共享分析可能未找到」；不用作者可设置的提交时间做下界），并按以前找到过的评论 ID 直接重读，因为 pulls.review 原地更新同一条评论；只有受信任作者（对仓库有写权限的人：GitHub 查 collaborator permission 为 `admin` / `write`，GitLab 查成员 `access_level` 不低于 Developer（30）；以及设置里的机器人账号，按平台主机加账号的不变 ID 比较，不比用户名）的直接显示，查不到权限时按未验证处理；先判断机器人账号，权限查询结果按「平台主机 + 仓库 + 作者」缓存，否定的缓存 24 小时，肯定的只对检查之前最后更新的评论有效（`updated_at` 更晚时重查），每次读取最多新查 20 个作者，超出的这次按未验证处理并标为「部分作者未验证」，其他作者的列为未验证、点开才显示；评论只记录 head，head 不一致的标为过期，一致的也一律标为可能过期（评论没有 base 和元数据可比）；解析失败的忽略；压缩内容按流解压，解压后超过 2 MB 即停止并丢弃。不向 PR 写入 Lectern 自己的分析（[ADR 0019](../../docs/adr/0019.md)）。设计依据见 [pulls.review 调研](../../docs/research/pulls-review.md#对-lectern-的建议)。
- 为用量统计（ADR 0020）提供分支与 MR 的对应：每个 MR 的编号、标题、源仓库与源分支、目标仓库与目标分支、历史 base 提交（代码平台记录的 base OID，GitHub 的 `base.sha`、GitLab 各个 diff 版本的 `base_commit_sha`，含当前的 `diff_refs.base_sha`；已合并的 MR 另给合并前目标分支的提交，供判断提交是不是经这个 MR 交付，不用目标分支当前的位置）（仓库按代码平台主机加平台给仓库的不变 ID 标识（GitHub 的 repository id、GitLab 的 project id），改名或转移后不变，区分 fork；owner/name 只用于显示）、源分支的 head、状态变化的完整时间线（打开、关闭、重新打开、合并及其时间），包括已经合并和关闭的 MR，以便按 MR 的存续时间切分历史用量。

## 测试

不依赖 GPUI，在任何平台上都能编译和测试：

```sh
cargo check -p lectern-forge     # 只改本 crate 时最快的反馈
just test -p lectern-forge
```

- 单元测试写在模块旁的 `#[cfg(test)] mod tests`；结构化输出（解析结果、归因、diff）用 insta 快照验收，快照改动要在 PR 里逐个看过。
- 夹具放 `tests/fixtures/`，是按接口文档手写的合成数据，不含真实仓库、账号和会话日志的数据；格式随 API 版本变化的，每个版本留一份。
- 调用 `gh` / `glab` 的测试用 `testing::FakeCli` 把假命令放在 `PATH` 最前面，不依赖真实命令行、网络和登录状态。
