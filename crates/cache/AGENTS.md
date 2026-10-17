# cache（`lectern-cache`）

内容寻址的 SQLite 缓存：解析、高亮、索引、diff 等派生数据以 git blob OID 为键（架构规则 5）。

- 层：基础设施层。只依赖基础设施和 kernel 层；不依赖 GPUI。
- 设计文档：[docs/design/cache.md](../../docs/design/cache.md)。
- issue：[#17](https://github.com/SoloJiang/lectern-app/issues/17)、[#31](https://github.com/SoloJiang/lectern-app/issues/31)
- ADR：[0006](../../docs/adr/0006.md)、[0017](../../docs/adr/0017.md)、[0019](../../docs/adr/0019.md)、[0026](../../docs/adr/0026.md)、[0027](../../docs/adr/0027.md)

## 边界

- 首帧之后才打开数据库。
- 除了按仓库分库，另有一个不属于任何仓库的应用级分库，存 `agents` 的会话轮次索引（键是来源 ID、会话 ID、适配器版本和索引格式版本，值带生成时的游标，有效性由 `agents` 按已解析前缀判断）和压缩会话的摘要（键是压缩文件的身份加适配器版本和摘要格式版本）（[ADR 0026](../../docs/adr/0026.md)）。
- SQLite 经 rusqlite（bundled，[ADR 0027](../../docs/adr/0027.md)）；本 crate 的连接最多 6 个、页缓存合计 ≤ 6 MB，与 `usage` 自己数据库的 ≤ 2 MB 合计 8 MB（ADR 0017）。缓存按字节 LRU；缓存可以随时删除重建，不存只有这里才有的数据。
- 缓存放在 `platform` 提供的应用缓存目录下，按仓库 git 公共目录的绝对规范路径区分（`git rev-parse --path-format=absolute --git-common-dir` 或解析后取规范路径），同一仓库的 worktree 共用，不在用户的 `.git` 里写文件。
- 模型分析结果的键是完整分析请求的哈希：规范化后的提示词、每个文件规范化后的完整 patch 的哈希（含新旧路径、OID、文件模式和状态）、被审阅 worktree 的指纹、schema 版本、模型 ID，以及调用的 agent、它的命令行版本、影响输出的模型服务配置、快照处理规则的版本（与基准快照的键相同，规则变了旧结果不再命中）和分析流程的版本（提示词模板、校验、覆盖检查、纠错、对账、启动参数、上限任何一项变化都升版本）（[ADR 0019](../../docs/adr/0019.md)）。规则生成的结果不缓存，每次重算。
- Analyze 的快照按字节 LRU 淘汰；只缓存不含工作区改动的基准快照（按树 OID 加处理规则的版本，规则随 agent 命令行版本的自动加载清单变化，写出后只读），叠加了工作区改动的克隆属于单次分析，不进缓存；正在使用的快照有引用计数，不参与淘汰（ADR 0019）。

## 测试

不依赖 GPUI，在任何平台上都能编译和测试：

```sh
cargo check -p lectern-cache     # 只改本 crate 时最快的反馈
just test -p lectern-cache
```

- 测试在 `src/tests/`（按设计文档「测试」的前缀分文件），用 `tempfile` 的临时目录和合成数据，不需要夹具。时钟和故障注入用本 crate 的 `test-support` feature（`testing::ManualClock`、`Partition::set_max_page_count`、`Partition::set_write_hook`），其他 crate 的测试也用它；检查文件内容时测试自己开一个 rusqlite 连接（`support::raw`），相当于另一个进程。
- 自建线程的测试代码只放在 `src/tests/` 下：验收检查 `rg 'load_extension|thread::spawn' crates/cache/src --glob '!**/tests*'` 要求库代码里没有。
- `crash_*` 用同一个测试二进制做子进程（`current_exe` 加 `--exact`），子进程的入口没有环境变量时直接返回。
- 目录清理按 `ManualClock` 的时刻比较文件的修改时间，所以测试的时钟从真实的当前时刻开始（`support::Fixture`）。
- 结构化输出（键的编码、文件布局、PRAGMA）用 insta 快照验收，快照改动要在 PR 里逐个看过。
- 性能：`lectern-bench` 的 `cache/*` 基准（`just bench cache/`；Linux 上是冒烟运行，`just bench --measure cache/` 采样并打印实测值），夹具用 `testing::Raw`、`testing::Zstd`、`testing::noise`、`testing::text`；`cache/memory` 在 bench 的 `memory` 目标里用子进程测常驻增量（SQLite 的内存不经计数分配器）。结果写进设计文档的「实际验证」。
