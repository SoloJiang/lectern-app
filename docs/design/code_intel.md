# code_intel 设计

状态：实现中（设计 [#107](https://github.com/SoloJiang/lectern-app/pull/107)）。第 1 步（语言注册与抽取，[#113](https://github.com/SoloJiang/lectern-app/pull/113)）、第 2 步（存储，[#115](https://github.com/SoloJiang/lectern-app/pull/115)）、第 3 步（映射、调度与增量更新，[#117](https://github.com/SoloJiang/lectern-app/pull/117)）、第 4 步（查询与插件，[#121](https://github.com/SoloJiang/lectern-app/pull/121)）已实现，第 3 步的电源代码已按 [ADR 0032](../adr/0032.md) 删除；第 7 步（交互让路，第 4、5 层）已实现，见「实际验证」「与设计的差异」；第 5、6、8、9 步尚未实现（见「验收」）。

对应 issue [#17](https://github.com/SoloJiang/lectern-app/issues/17)。本文覆盖导航的第 1 层「语法索引」：用 tree-sitter tags 按 blob 抽取定义和引用名，存进 `cache` 仓库分库的自定义表，经当前 worktree 的「blob → 路径」映射过滤，随 `fs_watch` 的批次增量更新；索引还不完整时，跳转到定义、查找引用和 ⌘K 的符号查找按固定顺序现场回答（[技术调研「代码导航与编辑」](tech-research.md#代码导航与编辑)、[代码导航调研](../research/code-navigation.md)）。依赖与延迟预算见 [ADR 0030](../adr/0030.md)，后台索引与静止的关系见 [ADR 0029](../adr/0029.md)。

## 范围

- 做：
  - 符号抽取：每个（blob OID、语言、grammar 版本、查询版本）抽取一次定义（名字、种类、位置、所在作用域、可见性、参数个数）和引用名，带单文件截止时间和大小上限。
  - 存储：`cache` 仓库分库（文件夹项目用文件夹分库）里的三张自定义表，在分库内单独计磁盘份额，按「最后一次出现在 worktree 里」的时间淘汰。
  - 每个打开项目的当前 worktree 的「blob → 路径」映射：index 里的 OID 加上未暂存的修改、未跟踪且未被忽略的文件；改掉或删掉的内容不再出现在查询结果里。
  - 建索引的五层优先级与调度（ADR 0029）：每层持有的守卫、所在线程组、批次、取消、暂停；不区分电源（ADR 0032）；交互让路（[ADR 0033](../adr/0033.md)，kernel 的开关默认关闭，见「交互让路」）。
  - 依赖层（ADR 0033，见「依赖层」）：当前子项目直接依赖的声明闭包在批量层建索引，跳转时依赖目标按需解析；单元在 `code_intel.deps` 命名空间里。
  - 大文件（[ADR 0034](../adr/0034.md)，见「大文件」）：超过 1 MiB 的打开文件按窗口抽取、每个窗口完成即写入；跳转在超过 1 MiB 的当前文本里只解析位置所在的窗口；`Location` 带字节偏移。
  - 增量更新：消费 `fs_watch` 的 `WorktreeBatch`；重新打开项目时只抽取缓存里缺的键。
  - 查询：跳转到定义、查找引用与调用方、按名字列出定义。包括绑定依据规则、语言兼容组、候选分层与过滤、调用方解析，以及索引不完整时的回答顺序和限时搜索。
  - ⌘K 的符号名字表，供 [#102](https://github.com/SoloJiang/lectern-app/issues/102) 使用。
  - `code-intel` 插件，以及它在 `services` 里提供的 `dyn CodeIntel` 接口。
  - 两个扩展点：本 crate 内部的解析扩展点（[#18](https://github.com/SoloJiang/lectern-app/issues/18)、[#19](https://github.com/SoloJiang/lectern-app/issues/19) 接入），语义导航的注册（[#26](https://github.com/SoloJiang/lectern-app/issues/26) 接入）。
- 不做：
  - Java 的 package、import、全限定名与同名方法消歧（#18）；TS / JS 的相对路径 import、tsconfig `paths`、工作区包名与子项目之间的依赖（#19）。它们是本 crate 的后续 PR，经「解析扩展点」接入，本文只定义接口和调用时机。
  - 上下文抽屉的界面、结果的分组与呈现、「未跟着改的调用方」的判断（[#20](https://github.com/SoloJiang/lectern-app/issues/20)，`context_drawer`）；⌘K 的匹配、排序、分组与界面（#102）。本 crate 只交出数据。
  - jdtls 与 SCIP 导入（#26）：独立进程插件（[ADR 0018](../adr/0018.md)），经「语义导航的注册」接入。
  - 语法高亮、高亮与文本的 LRU、后退前进（`file_reader`、[#10](https://github.com/SoloJiang/lectern-app/issues/10)）。高亮方与本 crate 依赖同一批 grammar crate，链接进二进制的只有一份；高亮的计算按 ADR 0034 放在领域层的库里（由 `file_reader` 的设计决定）。ADR 0033 的两类只存内存的批量工作（空闲预取的高亮、大文件的整文件高亮）和行检查点（`syntax.lines`）都属于它，不在本 crate。
  - 文件列表与子项目（`project`）、工作区比较（`git`）、监听（`fs_watch`）、安全读取（`platform`）、SQLite 的连接与淘汰（`cache`）。本 crate 只调用它们的接口，需要它们新增的部分列在「对其他 crate 的要求」。
  - 语言包的下载、校验和加载（`marketplace` 与 kernel 的 grammar 加载器，ADR 0018）：本 crate 只定义接收已加载 grammar 的接口 `GrammarSource`。
  - Java 的 `-sources.jar`（#26，读它要 zip 解码器，归属按 ADR 0007 决定）与文件内容的全文搜索。npm 等生态的依赖声明（按需解析与依赖层）按 [ADR 0033](../adr/0033.md) 在本 crate 里（「依赖层」）；Cargo、Go、Python 的依赖根在它们的解析扩展点实现时接入，在那之前依赖层只为 TS / JS 运行。
- 本 issue 之外：
  - 文件夹项目的增量更新：`project` 在 P1 不监听文件夹项目（[project 设计「范围」](project.md#范围)），本 crate 同样只在打开时建索引；`project` 加入文件夹监听后，本 crate 改为消费同一来源的事件。
  - `review` 的工作区比较结果如果以后经 `services` 共享，本 crate 改用它，不再自己做一次比较（见「打开项目与热启动」）。

本 crate 是插件 crate（[ADR 0025](../adr/0025.md)）：实现 `Plugin`，第一个实现 `Plugin` 的 PR（「验收」第 4 步）在 `Cargo.toml` 的 `[package.metadata.lectern]` 加 `plugin = true`。它只依赖 kernel 层和非插件的库 crate，不依赖 `project` 等其他插件 crate；读项目模型只经 `services` 的 `Projects`。只有 `app` 和 tool 层依赖本 crate，`context_drawer`、⌘K 只依赖 `services::code_intel` 里的接口和快照类型。

## 依赖

| 依赖 | 用途 | 选用理由 |
|---|---|---|
| `lectern-kernel` | `Plugin`、事件、`BusyGuard`、`BackgroundGuard`（ADR 0029）、`ActivationToken` | 插件机制 |
| `lectern-services` | 本文定义的 `code_intel` 模块；调用 `Projects` 取项目快照 | 消费方只依赖接口（架构规则 7） |
| `lectern-executor` | `Interactive`、`Background`、`Priority::Batch` 任务；截止时间的看门狗线程（`run_dedicated_gated`） | 后台线程只由 `executor` 管理 |
| `lectern-cache` | 自定义表（见「对其他 crate 的要求」） | 派生数据以 blob OID 为键存进 SQLite（架构规则 5、[ADR 0027](../adr/0027.md)） |
| `lectern-git` | 当前 worktree 的工作区比较、其他 worktree 的 index、对象库里的 blob | gix 只在 `git` 里使用 |
| `lectern-fs-watch` | 当前 worktree 的 `WorktreeBatch` | 与 `project` 共用同一份 worktree 监听状态 |
| `lectern-platform` | `SafeRoot`：读工作区文件 | 工作区文件只经 `SafeRoot` 读取，不用 mmap（[platform 设计「读取上限」](platform.md#读取上限)） |
| `tree-sitter` `=0.27.1`（不开 `wasm`） | 解析、locals 查询、截止时间的进度回调 | ADR 0030 |
| `tree-sitter-tags` `=0.27.1` | tags 查询抽取定义和引用 | ADR 0030；只新增 `tree-sitter-language`、`streaming-iterator` 两个传递依赖 |
| `streaming-iterator` | `ParsedFile` 自己遍历查询匹配（`QueryMatches` 实现的是它的 trait） | tree-sitter-tags 的传递依赖，MIT OR Apache-2.0，不新增 crate |
| `tree-sitter-typescript`、`tree-sitter-javascript`、`tree-sitter-rust`、`tree-sitter-java`、`tree-sitter-python`、`tree-sitter-go` | 首发语言的 grammar（见「未决问题」1） | ADR 0018 的核心语言集合，MIT；版本取 ABI 落在 tree-sitter 0.27.1 支持范围内的最新版本，在 `[workspace.dependencies]` 用 `=` 锁定，与高亮方共用；体积计入 ADR 0018，由 [#38](https://github.com/SoloJiang/lectern-app/issues/38) 测量 |
| `memchr` | 限时搜索的整词查找（`memmem`） | 已在 Cargo.lock |
| `unicode-normalization` | 名字表统一成 NFC（ADR 0030：frizbee 不做规范化） | 已在 Cargo.lock |
| `insta`、`tempfile`（dev） | 抽取结果的快照；缓存目录与工作区夹具 | 工作区统一依赖 |

不直接依赖 `rusqlite`：SQL 经 `cache` 的自定义表接口执行，`cargo tree -i rusqlite -e normal --depth 1` 仍只列出 `lectern-cache`（[cache 设计「验收」](cache.md#验收)）。不依赖 `frizbee`：模糊匹配属于 ⌘K（ADR 0030）。版本由 Cargo.lock 锁定，新增依赖先跑 `just deny`。

`executor`、`cache`、`fs_watch` 由 `app` 创建一次，在构造插件时传入（ADR 0025「进程级的共享资源」）。

### 对其他 crate 的要求

以下改动各由对应 crate 的 PR 完成，写 `Refs #17`，在「验收」里列为前置条件。

- `services`：
  - 采纳本文「接口」一节定义的 `code_intel` 模块，`services` 的设计文档引用本节，不重复定义。
  - `project` 模块增加 `BlobId`（20 或 32 字节的对象 ID，`Copy`、可序列化为十六进制），`FileList` 增加每条的 OID：`FileList::oid(i) -> Option<BlobId>`，git 项目每条都有，文件夹项目都为 None；`FileListBuilder::push_with_oid`。序列化的 `entries` 每项多一个十六进制字段。这是 [project 设计「文件列表」](project.md#文件列表)里「需要时（`code_intel`）另加」的那一列。
  - ADR 0033、0034 带来的类型改动（「接口」里标了 ADR 的部分）：`TextOrigin::Dependency(DepOrigin)` 与 `DepOrigin`、`DepRoot`、`CacheRoot`、`Ecosystem`；`Location::byte`；`CandidateTier::Dependency`；`Coverage::deps`。与 `services` 的其余部分一样只是数据类型，随依赖层（「验收」第 9 步）之前的 `services` PR 加入；`Location::byte` 随第 6 步。
- `project`：从 index 建 `FileList` 时填入 OID（`TrackedEntry::oid`）；刷新时「路径与当前 `FileList` 相同就沿用」的判断改为路径和 OID 都相同，所以只暂存了内容变化时文件列表也会换代。OID 按列表的哈希算法定长存放，10 万文件多 2 MB（SHA-1）或 3.2 MB（SHA-256），`project` 的常驻份额（`plugin/project/resident_100k`）在同一个 PR 里相应修改。
- `cache`：按 [cache 设计「范围」](cache.md#范围)预留的做法增加「自定义表」一节和实现，满足本 crate 的以下需要（具体签名由 cache 的设计决定，已写在 [cache 设计「自定义表」](cache.md#自定义表)，与下列各条的差异见 cache 设计「与设计的差异」第 3 步）：
  1. 使用方以静态常量登记一组表（建表语句、索引、预编译语句的 SQL 文本）和它的版本；第一次使用时建表，版本变化时删掉旧表和它的全部单元后重建。
  2. 单元：一条自定义数据对应 `entry` 表里的一个条目（命名空间加键，带使用方声明的字节数），计入 `meta.bytes` 与 LRU。淘汰或删除一个单元时，在同一个事务里删除它在自定义表里的全部行（外键级联，或使用方登记的删除语句）。本 crate 的单元 ID 就是这个条目的 ID。
  3. 命名空间配额：`code_intel.tags` 在每个项目分库里最多占 256 MiB；超出时先按访问时间淘汰这个命名空间自己的单元，分库总预算（512 MiB）的淘汰规则不变，所以 hunk 缓存至少保留 256 MiB。
  4. 读：在读连接上执行登记过的语句，按整数、字节串、文本取列；公开接口里不出现 rusqlite 的类型。
  5. 访问时间：按单元 ID 标记访问（与读命中相同，精度 1 小时），不读值。
  6. 写：一个写事务里建单元、插入行、删除单元，全部生效或全部不生效；失败时返回 `Dropped`，语义与 `Batch::commit` 相同，单个事务受 `MAX_BATCH` 约束。
  7. 预编译语句缓存从每连接 16 条提高到 32 条（本 crate 约 12 条语句）。
  8. 淘汰代数：每个分库每个登记了自定义表的命名空间一个内存里的计数，这个命名空间有单元被淘汰或删除、或分库被删除重建时加一，任何线程可以不阻塞地读取（「输入代数与派生状态」用它）。
  9. 第二个命名空间 `code_intel.deps`（[ADR 0033](../adr/0033.md)）：本 crate 登记第二组自定义表，配额 32 MiB，`code_intel.tags` 的配额改为 224 MiB（第 3 条的 256 MiB 是两者之和），依赖的单元只淘汰依赖的单元（[cache 设计「命名空间」](cache.md#命名空间)已登记）。用的是已有的「每组表一个命名空间」的接口，`cache` 不需要新代码；包的路径表也是这个命名空间里的单元和行（见「依赖层」），不用普通条目。
  10. 行检查点的命名空间 `syntax.lines` 属于高亮的领域库（ADR 0034），本 crate 不用它：跳转在大文件里按自己在内存里的行检查点定位（见「大文件」）。
- `kernel`：`PluginHandle::background()` 与 `BackgroundGuard`、批次边界的暂停检查，以及不阻塞、任何线程可调用的 `BackgroundGuard::pause_requested()`（看门狗用它在批次中途打断在途抽取）（[kernel 设计「静止」](kernel.md#静止)，ADR 0029 规定由单独的 PR 先于本 crate 实现）。交互让路（ADR 0033）用 kernel 已有的 `PluginHandle::background_yielding()`、`BackgroundGuard::proceed_item`、`yield_requested`、`Grant` 和 `Kernel::set_yield`（[#123](https://github.com/SoloJiang/lectern-app/pull/123)，kernel 设计「后台工作」），不需要新接口（见「交互让路」）。
- `executor`：批量线程组与 `Priority::Batch`（[executor 设计](executor.md)，同上）；另加 `Executor::on_shutdown(f: Box<dyn FnOnce() + Send>) -> ShutdownHook`：`shutdown` 开始时在调用线程上依次调用已登记的回调（在 `contain` 里），`ShutdownHook` drop 时注销。专用线程上的闭包靠它得知 executor 正在关闭（本 crate 的看门狗，见「调度、批次与取消」）。
- `platform`：不需要电源接口（ADR 0032 取消了 ADR 0029「影响」里的这一项）。依赖根用已有的 `SafeRoot::open_dir`（ADR 0033），大文件用已有的 `SafeRoot::open_file`、`OpenFile::read_ranges`、`read_all`、`identity`（[#126](https://github.com/SoloJiang/lectern-app/pull/126)，[platform 设计「范围读取」](platform.md#范围读取)），不需要新接口。
- `git`：`Worktree::canonical_oid(path, bytes) -> Result<Option<Oid>, GitError>`：对一份工作区字节按这个路径的属性和配置做 to-git 方向的内置转换（`text`、`eol`、`ident`、`working-tree-encoding`），返回规范内容的 OID；有外部过滤器时返回 None。工作区比较第 3 步已经在做同样的转换，只是没有单独公开。其余用到的接口（`changes`、`tracked`、`blob`、`head`、`index_checksum`）已经存在。批量的 `Worktree::canonical_oids`（[#135](https://github.com/SoloJiang/lectern-app/pull/135)，整批至多解析一次 index）和 hunk、展示行的字节偏移（[#122](https://github.com/SoloJiang/lectern-app/pull/122)）也已存在，本 crate 按「规范内容 OID 的批量核对」和「大文件」使用。新增一个流式的 blob 哈希（「大文件」：按 1 MiB 的区间求大文件的 OID，不把整个文件读进内存）：

  ```rust
  /// 按 git blob 格式（`blob <len>\0` 加内容）求 OID，内容分段给出；与 `Oid::for_blob` 的结果相同（sha1dc / SHA-256）。
  /// 不做 IO、不阻塞，任何线程可调用。`len` 是内容的总长度；`update` 给出的字节合计不等于 `len` 时 `finish` 返回 None。
  pub struct BlobHasher { /* … */ }
  impl BlobHasher {
      pub fn new(kind: HashKind, len: u64) -> BlobHasher;
      pub fn update(&mut self, bytes: &[u8]);
      pub fn finish(self) -> Option<Oid>;
  }
  ```

  由 `git` 的一个 PR 加入（`Refs #17`，「验收」第 8 步的前置条件），测试按区间切分方式不同的输入与 `Oid::for_blob` 对照。
- `fs_watch`：不需要新接口。
- kernel 的 grammar 加载器（ADR 0018、[#39](https://github.com/SoloJiang/lectern-app/issues/39)、#42）：`tree_sitter::Language` 和由它得到的 `Parser`、`Query`、`TagsConfiguration`、`Tree` 都持有动态库里的函数指针。加载器给出的每个 `LoadedGrammar` 带一个租约（`GrammarLease`，加载的模块句柄的 `Arc`）；更新、回滚、撤销时加载器只停止交出这个版本，模块的卸载（`dlclose`）推迟到它的最后一个租约 drop 之后。撤销要求的「立即停用」由不再交出新的 `LoadedGrammar`、使用方取消在途工作来满足。
- 高亮方（`file_reader`）：依赖同一批 grammar crate、同一个 tree-sitter 版本。[#42](https://github.com/SoloJiang/lectern-app/issues/42) 指出 external scanner 不能被进度回调打断，它对 scanner 的处理（如对某个 grammar 停用）同样作用于本 crate。打开候选的一方按 `Location` 读内容：`TextOrigin::Dependency` 的位置经它的 `DepRoot` 读（`Worktree` 的根从工作区根的 `SafeRoot` 经 `open_dir` 打开，路径的各级都不是符号链接；`Cache` 的根按 ADR 0033 只取 Lectern 自己进程的环境），`byte` 有值时按它定位（ADR 0034），这些写进 `file_reader` 的设计。

## 接口

### 服务接口（`lectern-services` 的 `code_intel` 模块）

接口和数据类型放在 `services`，由本 crate 实现，与 `project`、`session` 模块的做法相同（[project 设计「服务接口」](project.md#服务接口lectern-services-的-project-模块)）。数据类型都派生 `Clone`、`PartialEq`、`Eq`、`Debug` 和 serde 的 `Serialize`、`Deserialize`（外部标记的枚举），能通过 JSON-RPC 传给以后的进程插件；`CodeIntel` 和 `NavigationProvider` 两个 trait 本身不序列化。`ProjectKey`、`WorktreeKey`、`RelPath`、`BlobId` 来自 `services::project`。

```rust
/// 单实现服务，`code-intel` 插件提供。
/// impl Service for dyn CodeIntel { const ID = "code-intel"; const MULTIPLE = false; }
pub trait CodeIntel: Send + Sync {
    /// 更新一类焦点，替换这一类上一次的值。立即返回，任何线程可调用（含 UI 线程）。
    /// 焦点决定第 1、2 层建索引的内容，也决定候选排序里的「本次审阅改到的文件」。
    /// `worktree` 是焦点里路径所在的 worktree（文件夹项目为 None）：只有它是当前 worktree 时焦点才参与建索引；
    /// 切换当前 worktree 后，各类焦点保留在原 worktree 名下，调用方对新的 worktree 重新设置。
    fn set_focus(&self, project: &ProjectKey, worktree: Option<&WorktreeKey>, focus: Focus);

    /// 开始一次查询。立即返回，任何线程可调用。结果分批写进这次查询的 `QueryResults`，
    /// 每写一批发出一次 `CodeIntelEvent::Results`；项目没有打开时结果直接是 `Failed(NotOpen)`。
    fn query(&self, query: Query) -> QueryId;

    /// 查询的当前结果。只取锁复制一个 `Arc`，不阻塞、不做 IO，UI 线程可调用。
    /// 查询已关闭或被挤出（同时保留的查询最多 16 个，超出时关闭最早的）时返回 None，调用方重新查询。
    fn results(&self, id: QueryId) -> Option<Arc<QueryResults>>;

    /// 不再需要这次查询：取消未完成的部分，丢弃结果。立即返回。
    fn close(&self, id: QueryId);

    /// 光标或指针在符号上停留约 150 ms 后预取（调研第 7 节）：只做跳转的前三步，结果不发事件。
    /// 2 秒内同样的 `query` 沿用它，前提是映射的代数、淘汰代数和语言包代数都没有变；变了就重新查询。立即返回。
    fn prefetch(&self, query: Query);

    /// ⌘K 的符号名字表。立即返回：仍有持有者的表返回 `Ready`，它的代数落后时同时开始重建；
    /// 没有时开始构建并返回 `Building`，已在构建时不另起一次。建好的表随 `CodeIntelEvent::Names` 交出；
    /// 表有持有者期间代数落后时，本 crate 自己在后台重建并再发一次 `Names`（见「名字表」）。
    fn names(&self, project: &ProjectKey) -> Names;

    /// 注册一个语义导航提供者（#26）。见「语义导航的注册」。立即返回。
    fn register_provider(&self, provider: Arc<dyn NavigationProvider>, token: ActivationToken) -> ProviderRegistration;

    /// 各层的覆盖情况。只读内存，不阻塞。只给测试和 bench 的探针用，界面不显示（ADR 0012）。
    fn coverage(&self, project: &ProjectKey) -> Option<Coverage>;
}

pub enum Focus {
    /// 当前打开的文本：完整文件阅读里的文件，或 Diff 里正在看的那一侧。第 1 层。
    Current(Option<TextRef>),
    /// 抽屉里正在看的候选文件。第 1 层。
    Drawer(Arc<[RelPath]>),
    /// 本次审阅改到的文件，按改动叙事的顺序。第 2 层；同层候选里排在前面。
    Review(Arc<[RelPath]>),
    /// agent 本会话写过、读过的文件（ADR 0020 的工具调用事件）。第 2 层。
    Touched(Arc<[RelPath]>),
}

/// 一份确定的文本：用户看到的那一份内容。
pub struct TextRef {
    pub worktree: Option<WorktreeKey>,   // 文件夹项目为 None
    pub path: RelPath,
    /// 这份文本的 blob OID（工作区一侧是规范内容的 OID，与 `git::Content::Blob` 相同；文件夹项目是原始字节的 SHA-1）。
    pub oid: BlobId,
    pub origin: TextOrigin,
}

pub enum TextOrigin {
    /// 工作区里的当前内容。本 crate 经 `SafeRoot` 读，内容与 `oid` 对不上时查询返回 `Stale`。
    Worktree,
    /// 对象库里的 blob，如 Diff 的旧一侧。本 crate 按 `oid` 从对象库读。
    Object,
    /// 依赖根里的文件（ADR 0033）：被忽略、不在 index 里，`oid` 是原始字节按 git blob 格式的哈希（没有规范化转换）。
    /// 本 crate 只经 `DepRoot` 读，路径不在本项目已发现的依赖根里、扩展名不在允许的列表里时查询返回 `Stale`。
    Dependency(DepOrigin),
}

/// 一个依赖文件所在的包（ADR 0033）。视图在候选的次要文字里显示 `package` 和 `version`（交互规范「状态与异常」）。
pub struct DepOrigin {
    pub ecosystem: Ecosystem,
    /// 包名，如 "react"、"@types/node"。
    pub package: Arc<str>,
    /// 包清单里的版本；清单没有写时为 None。
    pub version: Option<Arc<str>>,
    pub root: DepRoot,
}

/// `Npm`：TS / JS 的包（npm 注册表的包名），npm、pnpm、yarn 安装的都是。
pub enum Ecosystem { Npm }   // Cargo、Go、Python 随它们的解析扩展点加入

/// 依赖根（ADR 0033「依赖根的读取限制」）。`Location::path` / `TextRef::path` 相对这个根所在的目录：
/// `Worktree` 时相对 worktree 根（路径以 `dir` 开头），`Cache` 时相对那个缓存根。
pub enum DepRoot {
    /// 工作区里的依赖目录（`node_modules`、`.venv`、`vendor`）。`dir` 相对 worktree 根，各级都不是符号链接；
    /// 路径是解析过 pnpm 链接之后的真实路径（`node_modules/.pnpm/react@19.0.0/node_modules/react/index.d.ts`），也没有符号链接。
    Worktree { dir: RelPath },
    /// 工作区之外依赖缓存的源码子树，路径取自 Lectern 自己进程的环境。TS / JS 不用，留给 Cargo、Go 和 #26。
    Cache(CacheRoot),
}

pub enum CacheRoot { CargoRegistrySrc, CargoGitCheckouts, GoModCache, Maven, Gradle }

pub struct Query {
    pub project: ProjectKey,
    pub kind: QueryKind,
}

pub enum QueryKind {
    /// 光标所在符号的定义。
    Definition { at: TextRef, position: Position },
    /// 一个定义的引用与调用方。
    References { of: DefinitionRef },
    /// 按名字列出定义（⌘K 里选中一个符号名）。没有调用位置，所以候选没有绑定依据，也没有第 1、2 层。
    Named { name: Arc<str>, near: Option<RelPath> },
}

/// 行和列都从 0 起；列是这一行里的字节偏移。
pub struct Position { pub line: u32, pub column: u32 }

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct QueryId(pub u64);

/// 不可变。每追加一批整体替换为新的 `Arc`，视图持有旧结果不受影响。
pub struct QueryResults {
    pub id: QueryId,
    /// 每追加一批加一，与 `CodeIntelEvent::Results` 的 generation 一致。
    pub generation: u32,
    /// 位置上的符号。`Named` 查询只有名字。
    pub symbol: Option<Symbol>,
    /// `Definition`、`Named` 的候选。只追加：后到的一批排在已有条目之后，已有条目的顺序和下标不变。
    pub candidates: Arc<[Candidate]>,
    /// `References` 的结果：解析回被查定义的引用（调用方）。只追加。
    pub callers: Arc<[Reference]>,
    /// `References` 的结果：同名、但没有解析回这个定义的引用。不计入调用方。只追加。
    pub same_name: Arc<[Reference]>,
    /// 有绑定依据的唯一候选在 `candidates` 里的下标：视图直接跳转。
    pub jump: Option<u32>,
    /// 视图预选的候选下标（`jump` 为 None、有候选时为第一个候选）。
    pub preselect: Option<u32>,
    /// 语义导航（#26）的结果。与 `candidates` 不同时才为 `Ready`，相同时保持 `Same`。
    pub semantic: SemanticState,
    pub state: QueryState,
}

pub struct Symbol {
    pub name: Arc<str>,
    pub lang: LangName,
    pub syntax: RefSyntax,
    /// 光标就在一个定义的名字上时为这个定义：`References` 可以直接由它发起。
    pub defines: Option<DefinitionRef>,
}

/// 引用位置的语法种类，决定能接受哪些定义种类（见「候选的分层与过滤」）。
pub enum RefSyntax { Plain, Call, MemberCall, Member, Type, Construct }

pub struct Candidate {
    pub definition: DefinitionRef,
    pub tier: CandidateTier,
    pub evidence: Evidence,
    /// 定义所在的文件在 `Focus::Review` 里。
    pub in_review: bool,
    pub source: CandidateSource,
}

pub struct DefinitionRef {
    pub location: Location,
    pub name: Arc<str>,
    pub kind: DefKind,
    pub lang: LangName,
    /// 最近的外层定义的名字（方法所在的类、模块里的函数所在的模块）。
    pub scope: Option<Arc<str>>,
    /// locals 查询在非根作用域里给出的 `@local.definition`（局部变量、参数、嵌套的 `const helper = () => …`）为 true，
    /// 不论 tags 是否也捕获了它；只在所在文件的这个作用域里可见，`References` 只扫描这个作用域（见「查找引用与调用方」）。
    /// 根作用域（程序、模块顶层）的定义和只被 tags 捕获的定义为 false，按跨文件的方式查引用。
    /// tags 也捕获的非根作用域定义在 `ci_def` 里按 `Visibility::File` 记录，不作为其他文件的候选。
    /// 只有来自解析的值是确定的（跳转第 1 步对当前文本的解析）。`ci_def` 不存这个标记，来自索引的定义
    /// （跳转第 3 步、`Named`）一律为 false，意思是「不知道」：列出候选时不为它解析任何文件。
    /// `References` 不论 `DefinitionRef` 由谁给出，都按被查定义所在文件的解析确定它，用的是查引用本来就要做的
    /// 那一次解析（每个文件解析一次）；那次解析失败时沿用给出的值，false 时按跨文件的方式查，结果只多不少。
    pub local: bool,
}

pub struct Location {
    pub worktree: Option<WorktreeKey>,
    pub path: RelPath,
    /// 位置所在的内容来自工作区还是对象库（如从 Diff 旧一侧发起的查询，本文件的候选和引用来自对象库里的 blob）。
    /// 后续的 `References`、打开结果都按它读内容：`Object` 按 `oid` 从对象库读，`Worktree` 读工作区并核对 OID。
    pub origin: TextOrigin,
    /// 位置所在的那份内容。视图打开前比较：文件已经变了时重新查询，不跳到错误的行。
    pub oid: BlobId,
    pub line: u32,
    pub column: u32,
    /// 起点（定义的名字、引用）在 `oid` 那份内容里的字节偏移（ADR 0034）：视图打开大文件的中部时按它读窗口，
    /// 不从文件开头扫描换行。口径与 git 的 hunk 偏移相同：`oid` 的 blob 的字节。抽取时读到的字节不是这份内容
    /// （原始字节的 OID 与 `oid` 不同、经 `canonical_oid` 才对上，如 CRLF 的工作区文件）时为 None，视图按行号定位。
    pub byte: Option<u64>,
    /// 定义节点的最后一行；引用为引用所在的行。
    pub end_line: u32,
}

/// 调研第 6 节的六层，加上 ADR 0033 的依赖一层，靠前的先列出。`Dependency`：依赖层的单元（经依赖映射）和
/// 按需解析的依赖文件；import 解析给出的依赖目标仍是 `Imported` 层的 `Bound(Import)`。
pub enum CandidateTier { CurrentFile, Imported, SamePackage, SameSubproject, DependentSubproject, Elsewhere, Dependency }

pub enum Evidence {
    /// 有绑定依据：只有一个这样的候选时直接跳转。
    Bound(Binding),
    /// 只按名字找到。即使只有一个，也只预选、不跳转。
    NameOnly,
}

pub enum Binding {
    /// locals 查询在当前文件的作用域里解析到。
    Local,
    /// 经 import 解析到的文件里的定义（解析扩展点给出）。
    Import,
    /// 按全限定名唯一确定的类型（Java，#18）。
    QualifiedName,
}

pub enum CandidateSource { Index, Scanned, Resolver }

pub enum DefKind {
    Function, Method, Class, Interface, Struct, Enum, Trait, Type,
    Module, Macro, Constant, Field, Variable, Other,
}

pub struct Reference {
    pub location: Location,
    pub syntax: RefSyntax,
    /// 引用所在的定义的名字（Callers 列表显示「谁在调用」）。
    pub enclosing: Option<Arc<str>>,
    pub in_review: bool,
}

/// 语言的稳定短名："ts"、"tsx"、"js"、"rust"、"java"、"python"、"go"；语言包用它在索引里登记的名字。
pub struct LangName(pub Arc<str>);

pub enum SemanticState {
    NotRequested,
    Pending,
    /// 与 `candidates`（或 `callers`）相同，视图不提示。
    Same,
    Ready(Arc<[Location]>),
    Failed,
}

pub enum QueryState {
    Running,
    Done(Completeness),
    Failed(QueryError),
}

pub enum Completeness {
    /// 能参与回答的文件都已入索引或在这次查询里搜过；还在在途抽取里的文件算作没搜过。
    /// 依赖文件不在「能参与回答的文件」里，依赖层没有完成不会让结果成为 `Partial`（「依赖层」的完整性一条）。
    Complete,
    /// 限时搜索到时停止：当前子项目里还有 `unsearched` 个未入索引的文件没有搜，子项目之外还有 `outside` 个未入索引。
    Partial { unsearched: u32, outside: u32 },
}

pub enum QueryError {
    NotOpen,
    /// 这个路径的语言没有可用的 grammar。
    UnsupportedLanguage,
    /// 位置不在标识符上。
    NoSymbol,
    /// `TextRef` 的内容已经变了：调用方在下一次刷新后用新的 OID 重新查询。
    Stale,
    /// 当前文件超过按需抽取的上限（16 MiB，ADR 0034；原来是 4 MiB）。
    TooLarge,
    /// 读文件失败（附原因，日志用）。
    Unreadable(Arc<str>),
}

pub enum Names {
    Ready(Arc<NameTable>),
    Building,
    /// 项目没有打开，或还没有任何可用的索引。
    Unavailable,
}

/// 当前 worktree 的定义名，去重（区分大小写）、统一为 NFC，按字节序排序。连续缓冲加结束偏移，与 `FileList` 的布局相同。
pub struct NameTable { /* generation: u64, buf: Box<str>, ends: Box<[u32]>, kinds: Box<[u16]> */ }

impl NameTable {
    pub fn generation(&self) -> u64;
    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;
    pub fn get(&self, i: usize) -> Option<&str>;
    pub fn iter(&self) -> impl Iterator<Item = &str> + '_;
    /// 这个名字出现过的 `DefKind` 的位集（位序号是 `DefKind` 的声明顺序）。
    pub fn kinds(&self, i: usize) -> u16;
}

pub struct Coverage {
    pub generation: u64,
    pub focus: TierState,            // 第 1、2 层
    pub subproject: TierState,       // 第 3 层
    pub worktree: TierState,         // 第 4 层
    pub other_worktrees: TierState,  // 第 5 层
    /// 依赖层（ADR 0033）：当前子项目的直接依赖都已按身份对照、闭包里缺的都已抽取（或到了限额）时为 `Done`；
    /// 没有可识别的直接依赖时也是 `Done`。不参与完整性的核算（「依赖层」）。
    pub deps: TierState,
}

pub enum TierState { Pending, Done }
```

调用方：

- `context_drawer`（#20）：点符号或按 Space 时调 `query(Definition)`，选中定义后调 `query(References)`；收到 `Results` 事件后调 `results` 取快照；抽屉关闭时 `close`。打开的文件、抽屉里的候选经 `set_focus(Current / Drawer)` 告知。
- `review`（[#15](https://github.com/SoloJiang/lectern-app/issues/15)）或审阅视图：打开会话时 `set_focus(Review / Touched)`，由 #15 的设计决定由哪一方调用。
- `file_reader`：打开文件时 `set_focus(Current)`，指针停留时 `prefetch`。
- ⌘K（#102）：第一次搜索符号时调 `names`，选中名字后调 `query(Named)`。建好的表在 `Names` 事件里交出，⌘K 的监听保存这个 `Arc`，之后每收到一次同一项目的 `Names` 就换成事件里的新表，关闭 30 秒后释放由 ⌘K 负责；本 crate 只保留一个 `Weak`，表仍有持有者时 `names` 直接返回它。
- bench 与测试的探针：`coverage`。

### 事件

```rust
/// 在 executor 线程上、快照替换之后发出。监听只更新状态或安排重绘，不做 IO（kernel「事件分发」）。
pub enum CodeIntelEvent {
    Results { id: QueryId, generation: u32 },
    /// 带着建好的表：本 crate 自己只保留 `Weak`，表的所有权在这里交给监听方。
    Names { project: ProjectKey, generation: u64, table: Arc<NameTable> },
}
impl Event for CodeIntelEvent {}
```

### 语义导航的注册（#26 的升级钩子）

jdtls、SCIP 导入是独立进程插件（ADR 0018），在进程内由插件宿主的代理实现下面的 trait。kernel 没有可选依赖：本 crate 如果在清单里 `requires` 一个 `MULTIPLE` 服务，就要等到至少一个提供者激活才能激活。所以方向反过来：提供者的插件 `requires` `dyn CodeIntel`，在 `started` 里调用 `register_provider`，把返回的登记交给 `PluginHandle::own`。与 [kernel 设计](kernel.md#撤销)里 `dyn Contributions` 的界面注册相同，登记带着提供者这次激活的 `ActivationToken`：本 crate 每次调用提供者之前 `token.enter()`，取不到（提供者已停用）就当作没有登记，持有准入直到调用返回。

```rust
pub trait NavigationProvider: Send + Sync {
    /// 稳定 ID，日志用。
    fn id(&self) -> &str;
    fn handles(&self, lang: &LangName) -> bool;
    /// 只对内容与 HEAD 相同的文件回答（SCIP 索引按提交生成）。
    fn head_only(&self) -> bool;
    /// 立即返回，不阻塞；结果经 `reply` 在任意线程上交回。按需启动、空闲退出由提供者自己负责（架构规则 3）。
    fn definition(&self, request: SemanticRequest, reply: SemanticReply);
    fn references(&self, request: SemanticRequest, reply: SemanticReply);
    /// 立即返回，不阻塞；可在任意线程调用（包括 UI 线程：关闭查询时）。只登记取消，停止请求的工作由提供者自己在别处完成。`code_intel` 调用它时不持有自己的任何锁。
    fn cancel(&self, request: SemanticRequestId);
}

pub struct SemanticRequest {
    pub id: SemanticRequestId,
    pub worktree_root: Arc<Path>,
    pub at: TextRef,
    pub position: Position,
    /// `at` 的内容与 HEAD 里这个路径的内容相同。`head_only` 的提供者只在为 true 时被调用。
    pub head_clean: bool,
}

/// 本 crate 实现：`send` 只把结果放进队列，不阻塞；查询已关闭时丢弃。
pub struct SemanticReply { /* 查询 ID、代数、队列 */ }
impl SemanticReply { pub fn send(self, result: Result<Arc<[SemanticLocation]>, SemanticError>); }

/// 提供者只给出工作区里的路径和位置；本 crate 用这次查询开始时的映射快照补上 OID（`origin` 为 `Worktree`），
/// 映射里没有的路径（已删除、被忽略、不在当前 worktree）丢弃。
pub struct SemanticLocation { pub path: RelPath, pub line: u32, pub column: u32, pub end_line: u32 }

pub enum SemanticError { Unavailable, Timeout, Failed(Arc<str>) }

/// drop 时注销。`Send`，可以交给 `PluginHandle::own`；本 crate 已停用时 drop 不做任何事。
pub struct ProviderRegistration { /* Weak<登记表>, 登记序号 */ }
```

升级条件（调研第 6 节「何时升级」）由本 crate 判断，#26 只实现提供者：

- Java 的定义查询在第 3 步之后仍有多个候选（重载、接口的多个实现），或只有一个 `NameOnly` 候选：调用 `handles("java")` 的提供者的 `definition`。Java 的 `References` 同样调用 `references`。
- 只对工作区一侧发起的查询升级：`TextOrigin::Object`（Diff 的旧一侧）的文本是历史内容，提供者回答的是当前工作区，不发请求，`semantic` 保持 `NotRequested`。
- `head_only` 的提供者只在 `head_clean` 时调用；它返回的位置里，当前内容与 HEAD 不同的文件被丢掉，这些文件以 blob 索引的结果为准。
- 多个提供者处理同一种语言时（SCIP 导入与 jdtls 都处理 Java）按固定规则仲裁，结果不取决于激活顺序和回复的先后：
  - 先后：`head_only` 的提供者（SCIP，索引对应的提交与 HEAD 相同）负责内容与 HEAD 相同的文件；其余提供者（jdtls）负责有改动和未跟踪的文件，`head_clean` 为 false 时只请求它们。同一类里按提供者 ID 的字节序。`head_only` 一类只在它的请求还在等回复或已经带着位置回复时拥有与 HEAD 相同的文件；出错、超时或作废的请求不拥有任何文件，这些文件归另一类（`escalate_arbitration_by_outcome`：两类各四种结果、两种回复顺序）。
  - 合并：每个回复的位置按所在文件归给负责它的那一类，同一文件有多个回复时取先后靠前的提供者的位置。合并结果只取决于已到达的回复集合，每到一个回复重新算一次，再与候选比较。
  - 取消：每个请求有自己的 `SemanticRequestId`，只采用 ID 仍是这次查询当前请求的回复。查询关闭或被新的查询取代时，对还没回复的请求调用 `cancel`。非 `head_only` 的提供者（jdtls）在回复之前不因为别的提供者已经回复而被取消：它可能返回语法索引没找到的、在有改动或未跟踪文件里的位置。
  - 具体的提供者由 #26 实现，这里只规定 `code_intel` 一侧的仲裁。
- 注销：登记 drop（提供者停用、失败，或它的插件被 kernel 撤销），或者调用前 `token.enter()` 取不到准入时，本 crate 把这个提供者全部未回复的请求 ID 作废，按剩下的回复重新合并受影响的查询；没有别的提供者还在请求中、也没有可用回复时 `semantic` 改为 `Failed`。作废的 ID 不再被采用，迟到的回复直接丢弃。本 crate 自己停用或项目关闭时，对全部未回复的请求调用 `cancel`（仍在准入内）后丢弃。
- 合并结果与已列出的候选（位置集合）相同时 `semantic` 为 `Same`，不同时为 `Ready`，抽屉原地更新（技术调研「代码导航与编辑」的交互规则）。
- 其他语言不升级。#26 要增加的「查找实现」作为新的 `QueryKind` 变体随 #26 加入。

### 本 crate 的公开项

```rust
/// `app` 装配时构造，放进 `PluginSet`（id "code-intel"）。
pub struct CodeIntelPlugin { /* deps */ }

pub struct CodeIntelDeps {
    pub executor: lectern_executor::Executor,
    pub cache: lectern_cache::Cache,
    pub watcher: lectern_fs_watch::Watcher,
    /// 语言包里的 grammar（ADR 0018）。插件市场实现之前 `app` 传 `NoGrammarPacks`。
    pub grammars: Arc<dyn GrammarSource>,
}

impl CodeIntelPlugin {
    /// 不做 IO。
    pub fn new(deps: CodeIntelDeps) -> Self;
    /// 同一个插件，git 换成 `testing::FakeGit`（`test-support`）。
    #[cfg(any(test, feature = "test-support"))]
    pub fn with_fake_git(deps: CodeIntelDeps, git: Arc<testing::FakeGit>) -> Self;
}

// Manifest { provides: [service::<dyn CodeIntel>()], requires: [service::<dyn Projects>()], activation: [AfterFirstFrame] }
// activate：建空的项目表，provide 服务，订阅 `ProjectEvent`；不做 IO，不加载 grammar。
// started：对 `open_projects()` 里已经打开的项目各提交一次「打开」处理（见「打开项目与热启动」）；
//   `GrammarSource::subscribe` 的返回值交给 `PluginHandle::own`，停用时随守卫 drop。
// 项目关闭、切换当前 worktree 时：drop 这个项目的 `RepoWatch` / `WorktreeWatch`、取消它的任务、对未回复的语义请求调用 `cancel`；
//   停用时 kernel 撤销守卫和任务，同样的清理在 `Active` 的 `Drop` 里做完（看门狗见「调度、批次与取消」）。

pub trait GrammarSource: Send + Sync {
    /// 已加载、已校验的语言包 grammar（kernel 的 grammar 加载器给出）。阻塞，只在 executor 上调用。
    fn grammar(&self, lang: &LangName) -> Option<LoadedGrammar>;
    /// 语言包认领的扩展名，如 "kt" → "kotlin"。不阻塞。
    fn language_for_extension(&self, ext: &str) -> Option<LangName>;
    /// 已装语言包集合的代数：安装、更新、回滚、撤销都加一。不阻塞。
    fn generation(&self) -> u64;
    /// 已装语言包的语言，顺序不限。不阻塞。语言包的每一轮都读它，激活之前已装的包和漏掉的通知因此不会丢。
    fn installed(&self) -> Vec<LangName>;
    /// 语言包集合变化时在任意线程上调用 `f`（插件市场完成切换之后）；`f` 只入队，不阻塞。返回值 drop 时退订。
    fn subscribe(&self, f: Box<dyn Fn(GrammarChange) + Send + Sync>) -> Box<dyn Send>;
}

pub struct GrammarChange {
    pub generation: u64,
    pub lang: LangName,
    pub kind: GrammarChangeKind,
}

pub enum GrammarChangeKind {
    /// 新装的语言包，开始认领它的扩展名。
    Installed,
    /// 换成了新版本，`artifact_sha256` 变了。
    Updated,
    /// 新版本激活失败，回到上一个可用版本（ADR 0018）。
    RolledBack,
    /// 撤销或卸载：立即停用，不再加载（ADR 0018「撤销」）。
    Removed,
}
pub struct LoadedGrammar {
    /// 动态库的租约：所有克隆都 drop 之后，grammar 加载器才卸载这个库（见「语言包的变化」）。
    pub lease: GrammarLease,
    pub language: tree_sitter::Language,
    pub tags_query: Arc<str>,
    pub locals_query: Option<Arc<str>>,
    /// 语言包的 sha256（ADR 0018 的索引条目），前 8 字节作 grammar 版本。
    pub artifact_sha256: [u8; 32],
    pub compat_group: Option<Arc<str>>,
}
pub struct NoGrammarPacks;

/// 一个已加载 grammar 动态库的租约。克隆只增加引用计数。核心语言的 grammar 静态链接，租约为空。
#[derive(Clone)]
pub struct GrammarLease(/* Option<Arc<dyn Any + Send + Sync>>，加载器的模块句柄 */);

/// 纯函数与阻塞函数，供测试和 bench 直接调用；不经过插件。
pub mod extract {
    pub fn language_for_path(path: &str, packs: &dyn GrammarSource) -> Option<LangName>;
    /// 同一兼容组的语言可以互相引用（见「语言与抽取键」）。
    /// 当前语言表：核心语言加已装的语言包，每种语言带它的兼容组；语言包变化时整体替换（见「语言包的变化」）。
    pub struct LanguageTable { /* … */ }
    impl LanguageTable {
        /// 只有核心语言。不编译查询。
        pub fn core() -> LanguageTable;
        pub fn get(&self, name: &str) -> Option<&Lang>;
        pub fn iter(&self) -> impl Iterator<Item = &Lang> + '_;
        /// 先按核心语言的扩展名，再按语言包认领的扩展名（这个语言包要在表里）。
        pub fn for_path(&self, path: &str, packs: &dyn GrammarSource) -> Option<&Lang>;
        /// 加入或替换一个语言包；与核心语言同名的忽略。
        pub fn with_pack(&self, name: LangName, grammar: LoadedGrammar) -> LanguageTable;
        pub fn without(&self, name: &str) -> LanguageTable;
    }
    /// 表里的一种语言。克隆共享同一份，每个克隆都持有 grammar 的租约。
    pub struct Lang { /* … */ }
    impl Lang {
        pub fn name(&self) -> &LangName;
        pub fn grammar_version(&self) -> u64;
        pub fn query_version(&self) -> u64;
        pub fn compat_group(&self) -> Option<&str>;
        pub fn lease(&self) -> &GrammarLease;
        /// 编译查询（只编译一次，之后常驻）。阻塞，后台线程。编译失败时返回 false，这种语言在本次运行中停用。
        pub fn prepare(&self) -> bool;
    }
    /// 兼容组随语言包动态登记，所以按当前语言表判断。
    pub fn compatible(langs: &LanguageTable, a: &LangName, b: &LangName) -> bool;
    /// 截止时刻（看门狗的时钟）和内容大小上限，可选地带暂停探针。
    pub struct Deadline { /* watchdog, at, max_len, pause */ }
    impl Deadline {
        /// 现在起 250 ms，上限 1 MiB。
        pub fn background(watchdog: &Watchdog) -> Deadline;
        /// 现在起 1 s，上限 4 MiB。
        pub fn interactive(watchdog: &Watchdog) -> Deadline;
        /// 限时搜索的 min(250 ms, 剩余时间) 这类明确的时刻。
        pub fn at(watchdog: &Watchdog, at: Instant, max_len: usize) -> Deadline;
        /// 第 3–5 层传入 `BackgroundGuard::pause_requested`，看门狗每次检查时调用。
        pub fn with_pause(self, pause: PauseProbe) -> Deadline;
    }
    /// 抽取一份内容。阻塞（解析），任何非 UI 线程。`deadline` 到了返回 `TimedOut`；
    /// `cancel` 置位、暂停探针返回 true 或看门狗停止时返回 `Cancelled`。
    pub fn extract(lang: &Lang, source: &[u8], deadline: &Deadline, cancel: &Arc<AtomicBool>) -> Extraction;
}

/// 截止时间的看门狗（见「调度、批次与取消」）。每次激活一个，`Clone` 共享。
pub struct Watchdog { /* … */ }
pub type Starter = Box<dyn FnOnce(WatchdogRun) + Send>;
pub type PauseProbe = Arc<dyn Fn() -> bool + Send + Sync>;
impl Watchdog {
    /// `starter` 在第一次登记在途抽取时被调用一次，拿到线程的循环；插件经 `own_with` 和
    /// `run_dedicated_gated` 在专用线程 `WATCHDOG_THREAD`（"code-intel-watchdog"）上运行它。
    pub fn new(clock: Arc<dyn Clock>, starter: Starter) -> Watchdog;
    pub fn clock(&self) -> &Arc<dyn Clock>;
    /// 置位全部在途抽取的标志、唤醒线程；之后的登记立即取消，线程不再启动。重复调用无害。
    pub fn stop(&self);
    /// drop 时调用 `stop`，交给 `PluginHandle::own`。
    pub fn stop_guard(&self) -> WatchdogStop;
}
/// 线程的循环：有在途抽取时每 10 ms（`CHECK_INTERVAL`）检查一次，没有时不带超时地等待，`stop` 之后返回。
pub struct WatchdogRun { /* … */ }
impl WatchdogRun {
    pub fn run(self);
    /// 启动方用它把 `stop` 登记到 `Executor::on_shutdown`，线程退出时 drop 登记。
    pub fn watchdog(&self) -> Watchdog;
}
pub trait Clock: Send + Sync { fn now(&self) -> Instant; }
pub struct SystemClock;

/// `services::code_intel` 的 `LangName`、`DefKind`、`RefSyntax`、`Position` 在本 crate 的根模块重新导出。
/// 这些类型在 `services` 里没有方法，本 crate 用自由函数补上需要的部分。
pub use lectern_services::code_intel::{DefKind, LangName, Position, RefSyntax};
pub fn lang_name(name: &str) -> LangName;
/// 声明顺序，即 `NameTable::kinds` 的位序号和 `ci_def.kind` 的编码。
pub const DEF_KINDS: [DefKind; 14];
pub fn def_kind_from_tag(kind: &str) -> DefKind;
pub const fn def_kind_bit(kind: DefKind) -> u16;
pub fn def_kind_name(kind: DefKind) -> &'static str;

/// 按需解析一个文件和位置分析（见「抽取结果」的 `ParsedFile`、「跳转到定义」第 1 步）。
/// 供查询（第 4 步）、测试和 bench 使用。
pub mod site {
    pub struct ParsedFile { /* lang, source, tree, 定义, locals 的作用域与定义, 引用位置 */ }
    impl ParsedFile {
        /// 自己调用 `Parser::parse_with_options` 和 `QueryCursor::matches_with_options`，
        /// 进度回调直接比较 `clock` 的时刻和 `cancel`，不经看门狗。
        pub fn parse(lang: &Lang, source: Arc<[u8]>, clock: &dyn Clock, at: Instant, cancel: &AtomicBool) -> Result<ParsedFile, ParseError>;
        pub fn defs(&self) -> impl Iterator<Item = &Def> + '_;
        /// tags 引用与 `@local.reference` 的并集，按位置去重，不含定义自己的名字。
        pub fn references(&self) -> &[RefSite];
        /// 位置上（或光标紧跟其后）的标识符；不在标识符上时为 None（`QueryError::NoSymbol`）。
        pub fn site_at(&self, at: Position) -> Option<Site>;
    }
    pub enum ParseError { TimedOut, Cancelled, Unsupported }
    pub struct Site { pub name: Box<str>, pub syntax: RefSyntax, pub at: Position, pub binding: Option<LocalBinding>, pub defines: Option<SiteDef> }
    /// `local`：locals 在非根作用域里给出的定义（`DefinitionRef::local`）。
    pub struct SiteDef { pub def: Def, pub local: bool }
    /// 引用绑定到的局部定义（`Binding::Local`）；tags 也捕获它时带上它的 `Def`。
    pub struct LocalBinding { pub name: Box<str>, pub at: Position, pub def: Option<Def>, pub local: bool }
    pub struct RefSite { pub at: Position, pub start: usize, pub end: usize }
    /// 「候选的分层与过滤」的语法种类表，按 `DefKind::bit` 的位集给出。
    pub fn accepted_kinds(lang: &Lang, syntax: RefSyntax) -> u16;
    pub fn accepts(lang: &Lang, syntax: RefSyntax, kind: DefKind) -> bool;
}

/// `test-support`：`FakeGit`、`FakeProjects`（内存里的 `dyn Projects`）、`FakeGrammars`（可以安装、更新、回滚、撤销的语言包）、`ManualClock`、
/// 合成仓库的形状（cloudprint 形状、10 万文件）、语言夹具的生成。
#[cfg(any(test, feature = "test-support"))]
pub mod testing;
```

### 解析扩展点（#18、#19）

跳转的第 2 步、候选的第 2、3、5 层、调用方解析和第 2 层建索引的种子都要用到语言规则。规则放在本 crate 内部的一个 trait 后面，#17 只实现它的空实现，#18（Java）和 #19（TS / JS）在本 crate 里各加一个实现：

```rust
pub(crate) trait Resolver: Send + Sync {
    fn languages(&self) -> &[LangName];
    /// 当前文件的 import 指向的文件，以及每个 import 引入的名字。阻塞，后台线程；
    /// 用 `ctx` 读文件、查映射，不自己做 IO。结果按（blob OID、解析器版本）缓存由实现决定（ADR 0030）。
    fn imports(&self, file: &ParsedFile, ctx: &ResolveContext) -> Vec<Import>;
    /// 一个引用位置的绑定：Java 的全限定名、带接收者类型的方法调用。没有依据时返回 None。
    fn bind(&self, site: &Site, file: &ParsedFile, ctx: &ResolveContext) -> Option<Bound>;
    /// 「同包」的范围：Java 是同一个 package 的全部目录；默认是同一个目录。
    fn package_of(&self, path: &str, ctx: &ResolveContext) -> Option<PackageScope>;
    /// 一个子项目依赖的子项目（清单里的依赖坐标、TS 工作区包名），用于第 5 层候选（#19）。
    fn dependencies(&self, subproject: &str, ctx: &ResolveContext) -> Vec<RelPath>;
    /// 同名方法按所在类和参数个数消歧（#18）：返回保留的候选。默认全部保留。
    fn disambiguate(&self, site: &Site, candidates: &mut Vec<CandidateDraft>);
}

pub(crate) struct Import { pub target: ImportTarget, pub names: ImportedNames /* All | Named(Vec<(本地名, 导出名)>) | Namespace(本地名) */ }
pub(crate) enum ImportTarget {
    /// 工作区里的文件（相对 import、tsconfig `paths`、工作区包名）。
    Worktree(RelPath),
    /// 裸模块名解析到依赖根里的声明文件（ADR 0033）：实现只给出裸模块名和发起 import 的子项目，
    /// 由 `ResolveContext::dependency` 找出文件，实现不自己拼依赖根里的路径。
    Dependency(DepFile),
}
pub(crate) struct Bound { pub definition: DefinitionRef, pub binding: Binding }

impl ResolveContext<'_> {
    /// 裸模块名（"react"、"@types/node"、"lodash/fp"）在 `subproject` 看来解析到的依赖文件：按「依赖层」的发现规则
    /// 找到包目录，按入口规则（`exports` 的 `types` 条件……）找到声明文件。阻塞（读清单、`lstat`），只在查询的任务里调用；
    /// 结果按（子项目、模块名）在这次查询里缓存。找不到、越界或扩展名不允许时为 None。
    pub fn dependency(&self, specifier: &str, subproject: &RelPath) -> Option<DepFile>;
    /// 一个依赖文件的抽取结果：命中 `code_intel.deps` 就用，否则经它的根读出、按读到的字节求 OID 并当场抽取
    /// （交互上限与截止时间，计入查询），结果交给 `Background` 任务写入 `code_intel.deps`。
    pub fn dependency_extraction(&self, file: &DepFile) -> Option<Arc<Extraction>>;
}

/// 依赖根里的一个文件：所在的包（`DepOrigin`）、相对 `DepRoot` 所在目录的路径（见 `DepRoot`）。
pub(crate) struct DepFile { pub origin: Arc<DepOrigin>, pub path: RelPath }
```

`ResolveContext` 给出当前项目快照（`FileList`、子项目）、「blob → 路径」映射、按路径取已抽取的结果（命中缓存或现场抽取）、依赖文件的解析（上面两个方法，ADR 0033）。Go 的同包解析这类其他语言的规则以后也经这个 trait 加入。依赖根的读取限制（ADR 0033 的四条）由 `ResolveContext` 和依赖层的发现代码执行，`Resolver` 的实现看不到 `SafeRoot`，不能绕过：工作区内的根经 `SafeRoot::open_dir` 打开，工作区外只允许缓存的源码子树，包内的路径和链接按词法解析、离开所在的根就丢弃，只读声明和源码的扩展名。

### 依赖 git、fs_watch 与 cache 的接口

本 crate 只通过一个内部 trait 访问 git，生产实现转调 `lectern-git`，测试用 `testing::FakeGit`。都是阻塞调用，只在 executor 上执行；每次调用打开自己的句柄，返回时释放，不在两次调用之间持有 `git::Worktree` 和它的 `TrackedPaths`：

```rust
/// 一个 worktree：公共目录（哪个仓库）、它的 git 目录（哪个 worktree）和根目录。每次调用都带上它。
pub(crate) struct Tree { pub common_dir: Arc<Path>, pub git_dir: Arc<Path>, pub root: Arc<Path> }

pub(crate) trait GitAccess: Send + Sync {
    /// 当前 worktree 里内容可能与 index 不同的路径：`Worktree::changes(Base::Head, limits)`，
    /// `limits` 取 `total_files = 0`、`total_patch_bytes = 0`、`retained_bytes = 0`，其余为默认值，
    /// 所以不算行 diff、不保留内容，只取每个改动的路径（含重命名的来源）。
    fn changed_paths(&self, tree: &Tree, cancel: &AtomicBool) -> Result<ChangedPaths, GitError>;
    /// 另一个 worktree 的 index 条目（第 5 层）：普通文件和可执行文件、路径是 UTF-8 的，按 index 顺序。
    fn tracked(&self, tree: &Tree, cancel: &AtomicBool) -> Result<Vec<(RelPath, BlobId)>, GitError>;
    /// 对象库里的一个 blob（`TextOrigin::Object`）。
    fn blob(&self, tree: &Tree, oid: BlobId, max: u64) -> Result<Arc<[u8]>, GitError>;
    /// 当前 worktree 的 HEAD 提交（`Worktree::head`）；unborn 为 None。HEAD 输入的代数由它决定。
    fn head(&self, tree: &Tree) -> Result<Option<BlobId>, GitError>;
    /// index 文件末尾的校验和（`Worktree::index_checksum`），不解析条目。index 输入的代数由它决定。
    fn index_checksum(&self, tree: &Tree) -> Result<Option<BlobId>, GitError>;
    /// 按这个路径的属性和配置把工作区的原始字节转成规范内容，返回它的 blob OID（与 `git add` 得到的相同）。
    /// 设置了外部过滤器（clean / process）的路径返回 None。只在原始字节的 OID 与期望的不同时调用。
    fn canonical_oid(&self, tree: &Tree, path: &str, raw: &[u8]) -> Result<Option<BlobId>, GitError>;
    /// 一批路径的 `canonical_oid`，按输入顺序：`Worktree::canonical_oids`，整批一个句柄、至多解析一次 index（#135）。
    /// 一批至多 256 项、字节合计至多 2 MiB（调用方切分，单项超过时自成一批）；某一项出错时整批返回错误，调用方对这一批逐项调用 `canonical_oid`，
    /// 出错的那一项按读不出处理。见「规范内容 OID 的批量核对」。
    fn canonical_oids(&self, tree: &Tree, items: &[(&str, &[u8])]) -> Result<Vec<Option<BlobId>>, GitError>;
    /// fs_watch 的过滤器与配置来源，见下文。不做 IO。
    fn watch_source(&self, tree: &Tree) -> Arc<dyn lectern_fs_watch::WorktreeSource>;
}

pub(crate) struct ChangedPaths {
    /// 按字节序、去重。`ChangeSet::files` 里每一项的 `path`，加上 `Renamed` 的 `from`。
    pub paths: Vec<RelPath>,
    /// 与 HEAD 不同的路径和它在 HEAD 里的 OID（HEAD 里没有时为 None）：`ChangeSet` 每一项基准一侧的 `Content::Blob`。
    /// 包括只暂存、未提交的改动（工作区等于 index、不等于 HEAD）。
    pub head: Vec<(RelPath, Option<BlobId>)>,
    pub untracked_capped: bool,
}
```

- HEAD 差异：每次工作区比较的 `head` 存成项目的 `HeadDiff`（路径 → HEAD 里的 OID），连同这次比较时 HEAD、index、文件列表的代数。它与覆盖部分分开：覆盖部分只描述工作区与 index 的差别，只暂存的改动（工作区等于 index、不等于 HEAD）没有覆盖项，却在 `HeadDiff` 里。一个路径与 HEAD 相同（`head_clean`）当且仅当它在 HEAD 里的 OID 已知且等于当前工作区的 OID（覆盖项的 OID，没有覆盖项时是文件列表的 OID）：在 `HeadDiff` 里的取记下的 OID，不在里面的取文件列表的 OID。只在记下的三个代数都等于当前代数时成立，否则按「不相同」处理，直到核对任务重新比较完成（「输入代数与派生状态」）。事件已经到达、核对任务还没有消费的输入同样让它不成立：HEAD 或 index 待检查、请求了工作区比较时对所有路径；这个路径（或它上面的目录）在重新读集合里时对这个路径。`SemanticRequest::head_clean`、`head_only` 提供者的选择和它返回位置的过滤都只用这条规则。
- 工作区比较只用来找出「哪些路径要重新读」，不用它给出的工作区一侧的 OID：这些路径由本 crate 自己经 `SafeRoot` 读取并计算 OID（见「blob → 路径映射」），所以 `StagedOnly`、未暂存的删除、`Filtered`、`Unhashed` 都按同一条规则处理。
- `fs_watch`：本 crate 为每个打开的 git 项目的当前 worktree 自己订阅一份（`watch_repo` 加 `watch_worktree`，`Coalesce::WORKTREE`），与 `project` 的订阅共用同一份 worktree 状态（「同一 worktree 状态只用第一个句柄给的来源」，[fs_watch 设计「监听器与句柄」](fs_watch.md#监听器与句柄)）。`project` 在扫描当前 worktree 时先订阅，本 crate 在收到文件列表就绪的 `ProjectEvent::Changed` 之后才订阅，所以生效的来源是 `project` 的。本 crate 传入的 `watch_source` 是延迟实现：只有本 crate 的订阅先建立了状态时（`project` 的订阅恰好已经 drop）才在第一次调用 `filter` 时打开 `git::Worktree`，平时不读 index、不占内存。`Sink::busy` 返回 `PluginHandle::busy()`，`deliver` 只把批次并进项目的脏集合并在需要时提交刷新任务，守卫移进那个任务。
- `cache`：每个打开的 git 项目用 `cache.repo(common_dir)`，文件夹项目用 `cache.folder(root)`，在其上取本 crate 登记的自定义表（见「存储表」）。所有调用都在 executor 的任务里。

## 数据结构

### 语言与抽取键

语言由路径的扩展名决定，与内容无关：同一份内容在 `.js` 和 `.ts` 下 OID 相同，解析器和查询不同。

| 语言 | 扩展名 | grammar | 兼容组 |
|---|---|---|---|
| `ts` | `.ts`、`.mts`、`.cts` | `tree-sitter-typescript` 的 `typescript` | `js` |
| `tsx` | `.tsx` | `tree-sitter-typescript` 的 `tsx` | `js` |
| `js` | `.js`、`.mjs`、`.cjs`、`.jsx` | `tree-sitter-javascript`（含 JSX） | `js` |
| `rust` | `.rs` | `tree-sitter-rust` | — |
| `java` | `.java` | `tree-sitter-java` | `jvm`（语言包的 Kotlin 加入同组） |
| `python` | `.py`、`.pyi` | `tree-sitter-python` | — |
| `go` | `.go` | `tree-sitter-go` | — |

- 兼容组按调研第 6 节：JS / JSX / TS / TSX；Java / Kotlin；C / C++ / Objective-C 的头文件（语言包登记 `c-family` 组）。没有组的语言只与自己兼容。
- 语言包的扩展名由 `GrammarSource::language_for_extension` 认领，与核心语言冲突时核心语言优先。语言包在项目打开期间变化的处理见「语言包的变化」。
- 抽取键：（blob OID、语言短名、grammar 版本、查询版本）。
  - grammar 版本：核心语言取「crate 名 @ 版本」的 64 位哈希，版本号写成常量，测试 `lang_grammar_versions_match_lock` 检查它与 Cargo.lock 一致；语言包取 `artifact_sha256` 的前 8 字节。grammar 升级后旧结果不再命中（#42 的缓存键要求）。
  - 查询版本：tags 查询、locals 查询的文本加上 tree-sitter 与 tree-sitter-tags 的版本和本 crate 的 `EXTRACTOR_VERSION`（可见性、参数个数、作用域的计算规则变化时加一）合在一起的 64 位哈希。两个版本也是常量，由同一个测试检查；tree-sitter-tags 决定哪些匹配成为 tag，升级后旧结果同样不再命中。
  - 哈希用 64 位 FNV-1a：键会持久化，不能用随工具链变化的 `DefaultHasher`。
  - 首发语言的清单是 `lang.rs` 的 `CORE_LANGUAGES` 一张表（扩展名、grammar crate 与版本、兼容组、查询文件），加上两个 `Cargo.toml` 里的 grammar 依赖；「未决问题」1 选 (b) 时只删表里的行和对应依赖。
- 文件夹项目没有 git：OID 是原始字节按 git blob 格式的 SHA-1（`git::Oid::for_blob`）。

### 抽取结果

```rust
pub struct Extraction {
    pub status: ExtractStatus,
    pub defs: Vec<Def>,
    /// 引用名，去重、按字节序：调用、类型和构造之外，也包括普通标识符的用处和成员名（`const v = LIMIT`、`obj.field`），
    /// 所以 `ci_ref` 列出可能用到一个名字的每个文件（见「外部格式」）。
    /// locals 查询解析到局部定义的引用不在其中（tree-sitter-tags 的行为），
    /// 所以持久化的 `ci_ref` 也不含它们。这不丢结果：局部定义只能在它所在的文件里被引用，
    /// 而查引用时被查定义所在的文件总是现场解析，扫描的位置包括 `@local.reference`（见下文的 `ParsedFile`）。
    pub refs: Vec<Box<str>>,
}

pub enum ExtractStatus {
    Done,
    /// 超过截止时间。记下状态，不自动重试；文件被打开时按交互截止时间重做一次。
    TimedOut,
    /// 超过后台上限（1 MiB）。文件被打开时按交互上限（16 MiB，超过 1 MiB 的按窗口，ADR 0034）重做。
    TooLarge,
    /// 内容在窗口单元里（ADR 0034，见「大文件」）：整份抽取的这一行只是标记，窗口链覆盖到文件末尾时写入，
    /// 替换 `TooLarge`。没有定义和引用，它们在同一个 OID 的窗口单元里。
    Windowed,
    /// 前 8000 字节里有 NUL，与 git 的二进制判断相同。
    Binary,
    /// 取消或暂停，或看门狗已停止。不写入缓存。
    Cancelled,
    /// grammar 或查询编译失败，这种语言在本次运行中停用（见「状态与异常」）。不写入缓存。
    Unsupported,
}

pub struct Def {
    pub name: Box<str>,
    pub kind: DefKind,
    pub line: u32,
    pub column: u32,        // 名字起点的字节列
    pub byte: u64,          // 名字起点在抽取的字节里的偏移（ADR 0034）；窗口单元里是在整个文件里的偏移
    pub end_line: u32,      // 定义节点的最后一行
    pub scope: Option<Box<str>>,
    pub visibility: Visibility,
    pub arity: Option<u8>,  // 函数、方法的参数个数，超过 255 记为 255
}

pub enum Visibility {
    Public,
    /// 只在本文件可见：JS / TS 没有导出的顶层定义，Java 的 `private`。
    File,
    /// 只在同一目录可见：Go 小写开头的包级定义。
    Package,
    /// 只在本模块及其子模块可见：Rust 没有 `pub` 的项。
    Module,
    /// 语言没有可判断的规则（Python）。
    Unknown,
}
```

- `kind` 来自 tags 的 `@definition.<种类>`：`function`、`method`、`class`、`interface`、`module`、`macro`、`constant`、`type`、`field` 等按名字对应 `DefKind`，`struct`、`enum`、`trait` 用各语言查询里的同名种类，其余为 `Other`。
- 枚举的成员是定义：Rust 的变体、TS 的枚举成员、Java 的枚举常量是 `Constant`，`scope` 是所在的枚举，可见性取枚举的可见性（Rust 的变体没有自己的 `pub`，Java 的枚举常量没有修饰符）；Python 类体里直接赋值的名字（类属性，包括 `Enum` 的成员）是 `Field`，`scope` 是所在的类；方法（以及其中嵌套的函数）里给方法第一个参数的属性赋值（`self.name = name`，在 `__init__` 和其他方法里，也包括解包 `self.a, self.b = 1, 2`）也是这个类的 `Field`，一个类里同名的只记第一个，类属性优先；给其他对象的属性赋值仍是成员引用。查询用 `reference.lectern.receiver`（函数的第一个参数，带标注或默认值时取它的名字：`self: Self`、`self=None`）和 `reference.lectern.attribute`（属性赋值）两个标记交给规则判断。JS / TS 构造器里的 `this.x = …` 不是字段（已知限制，TS 的字段通常写成声明）。
- Python 的模块级常量按作用域判断，不按语法形状：不在函数或类作用域里的赋值目标都是 `Constant`，包括 `if`、`try`、`with`、`for` 体里的和连等 `A = B = None` 的每个名字；查询捕获每个赋值，`rules::finish` 去掉在局部作用域或类里的，连等里的名字没有 `scope`。JS / TS 和 Go 的顶层仍按形状判断，它们的规则本来就是「程序或源文件的直接子节点」（`let`、`const`、`class` 在块里是块作用域）；已知限制：顶层块里的 `var`（提升到程序作用域）不是定义。
- 每个名字节点只有一个定义。tree-sitter-tags 只在两次匹配都还在队列里时按模式顺序去重，后到的匹配会给同一个名字节点再加一个定义（方法又被函数模式匹配到）；抽取后按名字节点去重，保留更具体的种类（方法、类等先于函数，函数先于常量和变量），与查询里模式的先后一致。定义自己的名字不算引用。
- `scope`：同一文件里，范围包含这个定义的最内层定义的名字。抽取后按范围排序一遍求出。Rust 的 `impl` 块不是定义，但作为容器参与计算，块里的方法的 `scope` 是实现的类型；Go 的方法取接收者的类型。实现的类型的名字穿过引用、指针、数组、切片和元组（第一个元素），最多两层（`&[T]` 是 `T`）；泛型和路径类型取自己的名字（`Box<T>` 是 `Box`，`a::B` 是 `B`），基本类型是它自己（`u8`、`&u8`），穿过数组和元组时不算（`[u8]`、`(u8, u8)` 没有名字）；其他形式（`fn()`、`dyn Trait`）没有名字，trait 实现里的方法没有 `scope`，仍是 `Public`。
- `visibility` 和 `arity` 由每种语言的一个小函数得出；规则变化时 `EXTRACTOR_VERSION` 加一。tree-sitter-tags 只交出 tag、不交出语法树，所以函数读的是查询为它捕获的内容：
  - 标记：`@reference.lectern.<种类>` 的捕获给抽取规则用，标记本身不存。标记的模式排在引用模式之前，会占用名字节点；占用的是名字（导出的名字、实现的类型、Go 方法的接收者类型、Python 的属性赋值）而不是关键字（`export`、`for`、`pub`、`global`）时，这个名字仍记为引用名，`ParsedFile` 里仍是引用位置，除非它成了这个位置上的定义（Python 的字段；方法的第一个参数是参数自己的局部定义）。这是两条抽取路径共用的不变式（`Marker::consumes_name`），`extract_markers_keep_their_names` 对每种标记各取样例检查。Rust 的 `pub`（`visibility_modifier`）、`impl` 块与它实现的类型、trait 实现（`impl … for` 的 `for`，与类型分开，所以类型取不到名字时仍是 trait 实现），Java 的 `private`，JS / TS 程序顶层的 `export` 语句、`export { name }`、`export default name` 与 TS 的 `export = name` 导出的名字、`import` 语句（namespace 和 module 体里的 `export` 导出的是它们的成员，不算），TS 的 `declare global { … }` 和以字符串命名的环境模块 `declare module 'm' { … }`（常用于扩充其他包的类型；其中的声明是全局的或隐式导出的，同导出的一样可见，但不让文件成为 ES 模块）；每种导出形式在 `extract_export_forms_change_visibility` 的表里各有一行，检查它让名字从 `File` 变为 `Public`，Go 方法的接收者类型。
  - 参数列表：函数、方法的模式用 `@doc` 捕获参数列表节点（tags 把它的文本放在 `Tag::docs`），按括号、尖括号、字符串和注释的嵌套数顶层的逗号。Rust 的 `self` 接收者和 Python 的 `*`、`/` 分隔符不计数；没有括号的参数列表（`x => x`）是 1 个。查询不再用 `@doc` 捕获注释。
  - 局部定义：定义节点严格位于一个局部作用域之内的，在每种语言里都是 `File`，不再看下面按语言的规则。局部作用域是 locals 查询的作用域里除文件本身、类体（Python）和命名空间体（TS 的 namespace、module、`declare global`）之外的全部（「作用域形式」），由作用域的种类推出，不按语言写规则：遍历在同一遍里记下它们，tags 路径（核心语言）和 `ParsedFile` 取查询的结果。成员（字段、方法，含 TS 构造器里的参数属性）取它所属的类或 `impl` 块的位置，所以局部类的成员是 `File`，顶层类在构造器参数里声明的字段不是；最近的容器是函数或方法时（匿名类的成员），取自己的位置。语言包的作用域不分类，函数或方法定义里面的定义是 `File`。
  - 规则：Rust 有 `pub` 的项、trait 和 trait 实现里的成员、宏是 `Public`，其余 `Module`；Java 有 `private` 的是 `File`，其余 `Public`；Go 大写开头的是 `Public`，其余 `Package`；Python 和语言包是 `Unknown`。JS / TS：函数里的定义（如命名函数表达式的名字）是 `File`；其余取最外层定义的可见性：在 `export` 语句里或名字被导出时是 `Public`，程序顶层有 `import` / `export`（ES 模块）时是 `File`，没有时（脚本、CommonJS，以及只在 namespace 里有 `export` 的 TS 文件）是 `Public`。
- 按需抽取（第 1 层、限时搜索、引用的位置）另外需要语法树和引用位置，内部用 `ParsedFile`，用完即丢，不进缓存。它保留：语法树；tags 的引用位置；locals 查询的作用域、`@local.definition` 和 `@local.reference` 的位置。核心语言的定义取同一棵语法树上 `walk` 的结果，与索引里的定义一致；语言包的定义取查询的结果。解析、查询和遍历都按 `ParsedFile` 自己的截止时间和取消标志停止（解析和查询在进度回调里检查，遍历每 1024 个节点检查一次），返回之前再检查一次，所以截止时间之后不会交出结果。查询之后的绑定模型（`Collector::finish`：最内层作用域按起点二分查找再向外走，函数体按范围查表，每个作用域的定义有索引）和规则（`rules::finish`：作用域、导出范围按位置扫描或二分查找，Go 的接收者按范围查表）都与文件大小成线性，没有按「定义数 × 作用域数」增长的部分；两条抽取路径的看门狗登记覆盖到规则结束，之后再检查一次。`deadline_one_mib_returns_soon_after_the_deadline` 检查 1 MiB 的文件在 100 ms 的截止时间下很快返回（debug 构建、CI 机器上上限 600 ms；未检查的阶段以前要几秒），线性的证据是 release 构建的计时。引用位置取 tags 引用与 `@local.reference` 的并集，按位置去重：tags 省略了被 locals 解析掉的引用（包括 locals 查询把顶层函数也记为程序作用域里的局部定义时，对它的同文件引用），只扫 tags 的位置会漏掉局部变量、参数和这类同文件引用。

### 外部格式（tags 查询）

- 格式来源：tree-sitter 0.27.1 文档「Code Navigation Systems」（`@definition.*`、`@reference.*`、`@name`、`@doc`），locals 查询沿用 tree-sitter-highlight 的约定（`@local.scope`、`@local.definition`、`@local.reference`）。
- 查询文件放在 `crates/code_intel/queries/<目录>/tags.scm` 和 `locals.scm`，以对应 grammar 仓库在锁定版本里的同名文件为起点（MIT），文件头记下上游 commit 和 Lectern 的改动；上游没有 locals 查询的语言（Rust、Java、Python、Go）由 Lectern 编写，同样记在文件头。用 `include_str!` 编译进二进制，第一次用到一种语言时在后台线程编译 `TagsConfiguration`，之后常驻。
- TS 的 tags 查询按上游的做法叠加 JS 的查询。三种 grammar 的节点不完全相同（JSX 节点只在 `js` 和 `tsx` 里，JS 的 `field_definition` 和 `class_heritage` 在 TS 里是 `public_field_definition` 和 `extends_clause`），而查询里出现 grammar 没有的节点时编译失败，所以分成几个目录拼接：

  | 目录 | 内容 | 用到的语言 |
  |---|---|---|
  | `ecmascript` | 三种 grammar 都有的节点，以 tree-sitter-javascript 的查询为起点；`references.scm` 是兜底的引用模式，拼在每种语言的最后 | `js`、`ts`、`tsx` |
  | `js` | 只有 JS 有的节点（类字段、`extends`） | `js` |
  | `jsx` | JSX 组件引用 | `js`、`tsx` |
  | `ts` | 以 tree-sitter-typescript 的查询为起点 | `ts`、`tsx` |
  | `rust`、`java`、`python`、`go` | 各自的查询 | 同名语言 |

- 引用到标识符的模式带 `(#is-not? local)`：locals 解析到局部定义的引用不进入引用名（tree-sitter-tags 只对带这个谓词的模式过滤）。
- 每种语言的 tags 查询最后有一组兜底的引用模式：locals 没有解析到的每个标识符（`@reference.name`），以及每个成员名（`@reference.member`：Rust、Go 的 `field_identifier`，JS / TS 的 `property_identifier`，Java 的 `field_access.field`，Python 的 `attribute.attribute`；成员名不带 locals 谓词）。查引用第 1 步只经 `ci_ref` 选出已入索引的候选文件，只有调用、类型这类模式时，`let v = PUBLIC_CONSTANT;`、`obj.field` 所在的文件不会被选中。兜底模式要放在全部定义模式之后：tree-sitter-tags 对一个名字节点保留模式序号最小的 tag，排在前面的兜底模式会顶掉后面的定义。JS / TS 的兜底模式因此单独放在 `ecmascript/references.scm`，拼在每种语言的最后。
- locals 查询也把嵌套的函数和类的名字记为 `@local.definition`（Python 的 `def` 和 `class`，JS / TS 的函数和类声明，Rust 块里的 `fn`，Java 块里的局部类和记录）。声明本身也是一个作用域，名字在它的范围之内；`ParsedFile` 把作为函数或类作用域节点 `name` 字段的定义放进外层作用域，所以 `def outer(): def helper(): …; helper()` 里的调用绑定到 `helper`。块作用域的 `name` 字段是它绑定的名字（Java 增强 `for` 的变量），留在块里。Go 没有嵌套的具名函数。
- 局部解析是白名单：位置分析只对 locals 查询的 `@local.reference` 捕获（`identifier`、类型名 `type_identifier`、JS / TS 对象字面量的简写 `{ a }`）做局部绑定，其他节点种类（JS / TS 的 `property_identifier`、Rust 和 Go 的 `field_identifier` 等成员名、对象和结构体的键、JSX 的属性名）从不绑定到同名的局部。`(identifier) @local.reference` 也会匹配不是变量的位置：成员名和路径段（Java 的 `items.add`、`this::name`，Python 的 `obj.x`，Rust 的 `Queue::new`）、关键字（`f(x=1)`、`case Point(x=…)`）、import 的远端名、JSX 的内置元素名（`<label>`）。这些位置写在「定义形式」表的行上（`forms.rs` 的 `positions`），`ParsedFile` 按它把它们排除在局部引用之外；Go 复合字面量的键（`&Server{addr: addr}`，结构体里是字段名，map 里是表达式）同名的局部只是候选。语法种类为 `Member`、`MemberCall` 时也不做局部绑定。位置分析认的节点种类和模式里绑定名字的节点种类是同一张表（`locals.rs` 的 `name_kinds`）。`forms_positions_yield_sites` 检查定义形式和作用域形式的样例里每个声明和用处都是一个 `Site`，`site_binding_positions` 检查上面每种位置。
- 核心语言的抽取不经 tree-sitter-tags 的迭代器（[ADR 0031](../adr/0031.md)）：`walk` 模块为每种语言（JS / TS / TSX 共用一份）写了一次遍历，解析后按前序访问每个具名节点，在名字节点上按这种语言 tags 查询里模式的顺序判断它是定义、标记还是引用（第一个成立的模式生效，与 tree-sitter-tags 每个名字节点只留一个 tag、序号小的模式优先相同），带 `#is-not? local` 的引用按 tree-sitter-tags 的规则过滤（按名字比较之前出现的、所在作用域链上的 `@local.definition` 捕获的文本，遇到不继承的作用域停止），定义和标记交给与 tags 路径相同的 `rules::finish`。查询仍是规格：改 tags 或 locals 查询时同时改 `walk/<语言>.rs`，`extract_walker_matches_tags` 对全部夹具、每种语言逐个模式的样例、64 KiB 的语料和病态输入检查两条路径的抽取结果相同，没有允许的差异。Go 的 `const_spec` 的 `name` 字段也包含逗号，嵌套模式里的 `name: (identifier)` 只匹配到 `const A, B = 1, 2` 的 `A`，所以 tags 和 locals 查询写成 `(const_spec (identifier))`（`const_spec` 的标识符子节点只有名字）。节点种类和字段按名字解析成 ID，grammar 里缺少其中一个时这种语言退回 tags 路径并记一条日志。语言包仍用 tree-sitter-tags。
- 夹具：`crates/code_intel/tests/fixtures/extract/<语言>/` 下每种语言若干真实写法的样本（类、方法、闭包、重载、嵌套模块、导出与非导出、宏），抽取结果用 insta 快照；grammar 或查询升级时快照的变化在 PR 里逐个看过。另有一份病态输入（深度嵌套、超长行）用于截止时间的测试。

### 定义形式

每种首发语言里引入名字的语法形式，以及 Lectern 怎样处理它，都列在下表（`src/forms.rs` 的 `FORMS`）。tags 和 locals 查询、`walk` 的遍历按表实现；语法种类表（「候选的分层与过滤」）由表里的定义行推出：一个定义种类被它那一行列出的引用语法接受，`Plain` 和 Rust 以外的 `Member` 接受全部种类。

- 覆盖：grammar 的 `node-types.json`（grammar crate 的 `NODE_TYPES`）里，带 `name`、`declarator`、`pattern`、`alias`、`label`、`left`、`property`、`parameter`、`key` 中任一字段的具名节点都必须有一行；没有这些字段的绑定位置（参数列表、模式、`match` 的捕获）单独列出。`forms_cover_grammar` 检查这两点，并检查每一行的节点种类在 grammar 里存在，所以升级 grammar 时新增或改名的声明形式会让测试失败，直到它被归类。
- 样例：每个定义行的每个种类、每个局部绑定行都有样例（`src/tests/forms.rs` 的 `SAMPLES`）。`forms_samples_extract_and_bind` 检查定义样例在两条抽取路径上都给出这个定义且结果相同，绑定样例按「绑定规则」的期望绑定。
- 本表与代码里的表一致，由 `forms_table_matches_design` 检查；改表时按测试打印的内容替换两个标记之间的部分。
- 「处理」一列：定义的种类，括号里是除 `Plain` 外能指向它的引用语法；「局部」是绑定的命名空间（值、类型或两者，见「绑定规则」）；「组成部分」见所在的行；「引用」是用处，不是定义；「忽略」的原因在「说明」里。

<!-- forms:begin -->
| 语言 | 节点 | 处理 | 说明 |
|---|---|---|---|
| js ts tsx | `array_pattern` | 组成部分 | 解构：顶层声明里两层以内的名字是定义，见 variable_declarator |
| js ts tsx | `arrow_function` | 局部（值） | `x => …` 的参数 |
| js ts tsx | `assignment_expression` | 定义 Function（Call、MemberCall、Construct） | 右边是函数时（`f = () => …`、`obj.f = function () {}`）；其余是赋值 |
| js ts tsx | `assignment_pattern` | 组成部分 | 参数和解构的默认值：左边绑定，右边是用处 |
| js ts tsx | `augmented_assignment_expression` | 忽略 | 给已有的名字赋值 |
| js ts tsx | `binary_expression` | 忽略 | 表达式 |
| js ts tsx | `break_statement` | 忽略 | 标签：单独的命名空间，不导航 |
| js ts tsx | `catch_clause` | 局部（值） | `catch (e)` |
| js ts tsx | `class` | 定义 Class（Construct、Type） | 类表达式：名字只在类里可见，经索引找到 |
| js ts tsx | `class_declaration` | 定义 Class（Construct、Type）；局部（值和类型） |  |
| js ts tsx | `continue_statement` | 忽略 | 标签 |
| js ts tsx | `export_specifier` | 引用；不是变量：`alias` | `export { a as b }`：`a` 是用处，`b` 是导出名 |
| js | `field_definition` | 定义 Field（Call、MemberCall） | 类字段，值可以是函数 |
| js ts tsx | `for_in_statement` | 局部（值） | 带 `const` / `let` / `var` 时；否则是赋值 |
| js ts tsx | `formal_parameters` | 局部（值） | 每个参数，含解构和默认值 |
| js ts tsx | `function_declaration` | 定义 Function（Call、MemberCall、Construct）；局部（值） |  |
| js ts tsx | `function_expression` | 定义 Function（Call、MemberCall、Construct） | 名字只在函数里可见，经索引找到 |
| js ts tsx | `generator_function` | 定义 Function（Call、MemberCall） | 同函数表达式 |
| js ts tsx | `generator_function_declaration` | 定义 Function（Call、MemberCall）；局部（值） |  |
| js ts tsx | `import_clause` | 忽略；不是变量：其中的名字 | import：经索引和解析扩展点解析，不是局部定义 |
| js ts tsx | `import_specifier` | 忽略；不是变量：`name`、`alias` | import，同上 |
| js ts tsx | `namespace_import` | 忽略；不是变量：其中的名字 | `import * as ns`，同上 |
| js tsx | `jsx_closing_element` | 引用；不是变量：小写的 `name` | 组件名；小写的是内置元素，不是引用 |
| js tsx | `jsx_opening_element` | 引用；不是变量：小写的 `name` | 组件名；小写的是内置元素，不是引用 |
| js tsx | `jsx_self_closing_element` | 引用；不是变量：小写的 `name` | 组件名；小写的是内置元素，不是引用 |
| js ts tsx | `labeled_statement` | 忽略 | 标签 |
| js ts tsx | `member_expression` | 引用 | `a.b` |
| js ts tsx | `method_definition` | 定义 Method（Call、MemberCall） | `constructor` 不是定义（`new C` 指向类）；计算出的名字不记 |
| js ts tsx | `object_assignment_pattern` | 组成部分 | 解构的默认值 |
| js ts tsx | `object_pattern` | 组成部分 | 解构：顶层声明里两层以内的名字是定义，见 variable_declarator |
| js ts tsx | `pair` | 定义 Function（Call、MemberCall、Construct） | 值是函数时；其余是对象的键 |
| js ts tsx | `pair_pattern` | 组成部分 | 解构：键是属性名，值是模式 |
| js ts tsx | `regex` | 忽略 | 正则的内容 |
| js ts tsx | `rest_pattern` | 组成部分 | 解构的 `...rest` |
| js ts tsx | `shorthand_property_identifier_pattern` | 组成部分 | 解构的简写 |
| js ts tsx | `variable_declarator` | 定义 Function（Call、MemberCall、Construct）、Class（Construct、Type）、Constant（Call、MemberCall、Construct）、Variable（Call、MemberCall、Construct）；局部（值） | 值是函数或类时是函数或类；顶层（含 `export`、`declare`）的 `const` 是常量，`let` / `var` 是变量，解构时模式两层以内绑定的每个名字都是；TS 的 namespace / module 体里 `export` 的也是（只认单个名字）；都是局部绑定 |
| ts tsx | `abstract_class_declaration` | 定义 Class（Type）；局部（值和类型） |  |
| ts tsx | `abstract_method_signature` | 定义 Method（Call、MemberCall） |  |
| ts tsx | `ambient_declaration` | 组成部分 | `declare const` / `let` / `var` 见 variable_declarator，`declare function` 见 function_signature |
| ts tsx | `conditional_type` | 引用 | 类型 |
| ts tsx | `enum_assignment` | 定义 Constant | 有初始值的枚举成员 |
| ts tsx | `enum_body` | 定义 Constant | 没有初始值的枚举成员 |
| ts tsx | `enum_declaration` | 定义 Enum（Type）；局部（值和类型） |  |
| ts tsx | `function_signature` | 定义 Function（Call、MemberCall） | 重载签名和 `declare function` |
| ts tsx | `generic_type` | 引用 | `Foo<T>` |
| ts tsx | `index_signature` | 忽略 | `[key: string]: T` 的键名只是说明 |
| ts tsx | `infer_type` | 忽略 | `infer U`：只在条件类型的一个分支里可见，没有建模，只给索引候选（不直接跳转） |
| ts tsx | `interface_declaration` | 定义 Interface（Type）；局部（类型） |  |
| ts tsx | `internal_module` | 定义 Module（Type） | `namespace A.B` 记最后一段 |
| ts tsx | `mapped_type_clause` | 局部（类型） | `[K in keyof T]`，在映射类型里可见 |
| ts tsx | `method_signature` | 定义 Method（Call、MemberCall） |  |
| ts tsx | `module` | 定义 Module（Type） | `module A`；`declare module 'x'` 的名字是字符串，不记 |
| ts tsx | `nested_identifier` | 组成部分 | 模块名 `A.B`，见 internal_module |
| ts tsx | `nested_type_identifier` | 引用；不是变量：`name` | `ns.T` |
| ts tsx | `optional_parameter` | 定义 Field（Call、MemberCall）；局部（值） | 参数；带 `public` / `private` / `protected` / `readonly` / `override` 时也是字段（参数属性） |
| ts tsx | `property_signature` | 定义 Field（Call、MemberCall） |  |
| ts tsx | `public_field_definition` | 定义 Field（Call、MemberCall） | 类字段 |
| ts tsx | `required_parameter` | 定义 Field（Call、MemberCall）；局部（值） | 同 optional_parameter |
| ts tsx | `type_alias_declaration` | 定义 Type（Type）；局部（类型） |  |
| ts tsx | `type_parameter` | 局部（类型） | 在声明它的类、接口、类型别名或函数里可见 |
| ts tsx | `type_predicate` | 引用 | `x is T` |
| rust | `assignment_expression` | 忽略 | 给已有的绑定赋值 |
| rust | `associated_type` | 定义 Type（Type） | trait 里的 `type Item;` |
| rust | `binary_expression` | 忽略 | 表达式 |
| rust | `closure_parameters` | 局部（值） | 闭包的参数 |
| rust | `compound_assignment_expr` | 忽略 | 给已有的绑定赋值 |
| rust | `const_item` | 定义 Constant（Call）；局部（值） | 块里的是局部 |
| rust | `const_parameter` | 局部（值） | `const N: usize`，在声明它的项里可见 |
| rust | `enum_item` | 定义 Enum（Type）；局部（类型） | 块里的是局部 |
| rust | `enum_variant` | 定义 Constant（Call、Construct、Type） | 元组变体被调用（`Io(e)`），记录变体被构造和匹配（`Move { x, y }`） |
| rust | `extern_crate_declaration` | 忽略 | crate 的别名：经路径解析 |
| rust | `field_declaration` | 定义 Field（Member） |  |
| rust | `field_pattern` | 组成部分 | 结构体模式里的字段：简写和改名绑定 |
| rust | `for_expression` | 局部（值） |  |
| rust | `function_item` | 定义 Function（Call）、Method（Call、MemberCall、Member）；局部（值） | `impl` 和 trait 里的是方法；块里的是局部 |
| rust | `function_signature_item` | 定义 Function（Call）、Method（Call、MemberCall、Member） | trait 和 `extern` 块里的签名 |
| rust | `let_condition` | 局部（值） | `if let` / `while let` |
| rust | `let_declaration` | 局部（值） |  |
| rust | `lifetime_parameter` | 忽略 | 生命周期不进入索引，不导航 |
| rust | `macro_definition` | 定义 Macro（Call） |  |
| rust | `macro_invocation` | 引用；不是变量：`macro` | 宏调用 |
| rust | `macro_rule` | 忽略 | 宏的匹配模式 |
| rust | `match_arm` | 局部（值） |  |
| rust | `mod_item` | 定义 Module | 路径段是 `Plain` |
| rust | `parameter` | 局部（值） |  |
| rust | `qualified_type` | 引用 | `<T as Trait>` |
| rust | `range_pattern` | 引用 | 范围模式的端点 |
| rust | `scoped_identifier` | 引用；不是变量：`name`、`path` | 路径 |
| rust | `scoped_type_identifier` | 引用；不是变量：`name` | 路径 |
| rust | `static_item` | 定义 Constant（Call）；局部（值） | 块里的是局部 |
| rust | `struct_expression` | 引用 | 构造 |
| rust | `struct_item` | 定义 Struct（Call、Construct、Type）；局部（值和类型） | 元组结构体被调用；块里的是局部 |
| rust | `token_binding_pattern` | 忽略 | 宏的元变量 |
| rust | `trait_item` | 定义 Trait（Type）；局部（类型） | 块里的是局部 |
| rust | `type_binding` | 引用 | `Iterator<Item = T>` |
| rust | `type_item` | 定义 Type（Construct、Type）；局部（类型） | 块里的是局部 |
| rust | `type_parameter` | 局部（类型） | 在声明它的项里可见 |
| rust | `union_item` | 定义 Struct（Construct、Type）；局部（类型） | 块里的是局部 |
| rust | `use_as_clause` | 忽略 | `use` 的别名：经路径解析 |
| rust | `variadic_parameter` | 忽略 | C 的变长参数（`args: ...`，只在 nightly 上可用）：没有建模，只给索引候选 |
| rust | `where_predicate` | 引用 | 类型 |
| java | `annotation` | 引用 | 注解 |
| java | `annotation_type_declaration` | 定义 Interface（Type） |  |
| java | `annotation_type_element_declaration` | 定义 Method（Call、MemberCall） | `@interface` 的元素 |
| java | `assignment_expression` | 忽略 | 给已有的变量赋值 |
| java | `binary_expression` | 忽略 | 表达式 |
| java | `catch_formal_parameter` | 局部（值） |  |
| java | `class_declaration` | 定义 Class（Construct、Type）；局部（值和类型） | 块里的局部类是局部 |
| java | `compact_constructor_declaration` | 忽略 | 紧凑构造器：`new R(…)` 指向记录 |
| java | `constant_declaration` | 定义 Constant | 接口里的常量 |
| java | `constructor_declaration` | 忽略 | 构造器：`new C(…)` 指向类 |
| java | `element_value_pair` | 引用；不是变量：`key` | 注解里的元素名 |
| java | `enhanced_for_statement` | 局部（值） |  |
| java | `enum_constant` | 定义 Constant |  |
| java | `enum_declaration` | 定义 Enum（Type）；局部（值和类型） | 块里的是局部 |
| java | `field_access` | 引用；不是变量：`field` | `a.b` |
| java | `field_declaration` | 定义 Field |  |
| java | `formal_parameter` | 定义 Field（Call、MemberCall）；局部（值） | 方法、构造器和 lambda 的参数是局部；记录头里的组件是字段，调用语法指向它的访问器 |
| java | `inferred_parameters` | 局部（值） | `(a, b) -> …` |
| java | `instanceof_expression` | 局部（值） | 模式变量：可见范围取决于控制流，只给候选 |
| java | `interface_declaration` | 定义 Interface（Construct、Type）；局部（值和类型） | `new I() { … }`（匿名类）也指向接口；块里的是局部 |
| java | `lambda_expression` | 局部（值） | `x -> …` |
| java | `local_variable_declaration` | 局部（值） |  |
| java | `marker_annotation` | 引用 | 注解 |
| java | `method_declaration` | 定义 Method（Call、MemberCall） |  |
| java | `method_invocation` | 引用；不是变量：`name` | 调用 |
| java | `method_reference` | 引用；不是变量：第一个以外的名字 | `this::name`、`A::m` |
| java | `module_declaration` | 忽略 | `module-info.java` |
| java | `record_declaration` | 定义 Class（Construct、Type）；局部（值和类型） | 块里的是局部 |
| java | `record_pattern_component` | 局部（值） | 模式变量：只给候选 |
| java | `resource` | 局部（值） | `try (var r = …)` |
| java | `scoped_identifier` | 引用；不是变量：`name`、`scope` | 限定名 |
| java | `scoped_type_identifier` | 引用；不是变量：第一个以外的名字 | 限定的类型名 |
| java | `spread_parameter` | 定义 Field（Call、MemberCall）；局部（值） | 同 formal_parameter |
| java | `type_parameter` | 局部（类型） | 在声明它的类或方法里可见 |
| java | `type_pattern` | 局部（值） | 模式变量：只给候选 |
| java | `variable_declarator` | 组成部分 | 见所在的声明 |
| python | `aliased_import` | 忽略 | import：经索引解析 |
| python | `as_pattern` | 局部（值） | `with … as x`、`except … as x` |
| python | `assignment` | 定义 Constant（Call、MemberCall）、Field（Call、MemberCall）；局部（值） | 不在函数或类作用域里的是常量（含 `if`、`try`、`with`、`for` 里的和连等 `A = B = None` 的每个名字），类体里直接写的是字段，解包（`A, B = …`、`first, *rest = …`）时两层以内的每个名字都是；都是局部绑定 |
| python | `attribute` | 定义 Field（Call、MemberCall）；不是变量：`attribute` | `a.b`；方法（及其中嵌套的函数）里给方法第一个参数的属性赋值（`self.name = …`）是类的字段，其余是成员引用 |
| python | `augmented_assignment` | 局部（值） |  |
| python | `binary_operator` | 忽略 | 表达式 |
| python | `boolean_operator` | 忽略 | 表达式 |
| python | `case_pattern` | 局部（值） | `match` 的捕获；类名、关键字、带点的值和键不绑定 |
| python | `class_definition` | 定义 Class（Call、MemberCall）；局部（值） |  |
| python | `default_parameter` | 局部（值） | 默认值在外层作用域求值 |
| python | `dict_pattern` | 组成部分 | `match` 的映射模式，见 case_pattern |
| python | `except_clause` | 组成部分 | `as` 由 as_pattern 绑定 |
| python | `for_in_clause` | 局部（值） | 推导式的变量 |
| python | `for_statement` | 局部（值） |  |
| python | `function_definition` | 定义 Function（Call、MemberCall）、Method（Call、MemberCall）；局部（值） | 类体里的是方法 |
| python | `future_import_statement` | 忽略 | import |
| python | `global_statement` | 引用 | 作用域指令：名字是模块的，越过所有外层函数（「绑定规则」） |
| python | `import_from_statement` | 忽略 | import：经索引解析 |
| python | `import_statement` | 忽略 | import：经索引解析 |
| python | `keyword_argument` | 引用；不是变量：`name` | 调用的关键字参数名 |
| python | `keyword_pattern` | 组成部分；不是变量：第一个名字 | `case Point(x=…)`：关键字是属性名，值是模式 |
| python | `lambda_parameters` | 局部（值） |  |
| python | `list_pattern` | 组成部分 | 解包，见 assignment |
| python | `list_splat_pattern` | 组成部分 | 解包的 `*rest`，见 assignment |
| python | `named_expression` | 局部（值） | `:=` |
| python | `nonlocal_statement` | 引用 | 作用域指令：名字是最近的有它的外层函数的，不找到模块 |
| python | `pair` | 忽略 | 字典的键 |
| python | `parameters` | 局部（值） | 每个参数 |
| python | `pattern_list` | 组成部分 | 解包，见 assignment |
| python | `tuple_pattern` | 组成部分 | 解包，见 assignment |
| python | `type_alias_statement` | 定义 Type | `type X = …` |
| python | `type_parameter` | 局部（值） | `def f[T]()`，在声明它的函数或类里可见 |
| python | `typed_default_parameter` | 局部（值） | 类型和默认值在外层作用域求值 |
| python | `typed_parameter` | 局部（值） | 类型在外层作用域求值 |
| go | `assignment_statement` | 忽略 | 给已有的变量赋值 |
| go | `binary_expression` | 忽略 | 表达式 |
| go | `const_spec` | 定义 Constant；局部（值） | 包级的是常量（`const A, B` 的每个名字）；函数里的是局部 |
| go | `expression_list` | 组成部分 | `:=`、`range` 等的左边 |
| go | `field_declaration` | 定义 Field（MemberCall） | 函数类型的字段可以被调用 |
| go | `function_declaration` | 定义 Function（Call、MemberCall） |  |
| go | `import_spec` | 忽略 | import：经路径解析 |
| go | `keyed_element` | 引用；不是变量：`key`（同名的局部只是候选） | 复合字面量的键：结构体字面量里是字段名，map 里是表达式；同名的局部只是候选，引用名总是记下 |
| go | `labeled_statement` | 忽略 | 标签 |
| go | `map_type` | 引用 | 类型 |
| go | `method_declaration` | 定义 Method（MemberCall） |  |
| go | `method_elem` | 定义 Method（MemberCall） | 接口的方法 |
| go | `parameter_declaration` | 局部（值） |  |
| go | `qualified_type` | 引用；不是变量：`name` | `pkg.T` |
| go | `range_clause` | 局部（值） | 只有 `:=`；`for i = range xs` 是赋值 |
| go | `receive_statement` | 局部（值） | 只有 `:=`；`case v = <-ch` 是赋值 |
| go | `short_var_declaration` | 局部（值） | 只有同一作用域里新的名字（函数体还要除去参数和结果）；已声明的名字是对它的使用 |
| go | `type_alias` | 定义 Type（Type、Call、MemberCall） | 类型转换是调用语法 |
| go | `type_parameter_declaration` | 局部（值和类型） | 在声明它的函数或类型里可见；方法接收者里的类型参数（`(l *List[E])`）没有建模 |
| go | `type_spec` | 定义 Type（Type、Call、MemberCall） | 函数里的类型也进入索引，没有局部绑定 |
| go | `type_switch_statement` | 局部（值） | `switch v := x.(type)` |
| go | `var_spec` | 定义 Variable（Call、MemberCall）；局部（值） | 包级的是变量；函数里的是局部 |
| go | `var_spec_list` | 组成部分 | `var ( … )` |
| go | `variadic_parameter_declaration` | 局部（值） |  |
<!-- forms:end -->

### 作用域形式

「定义形式」的对应表：每种首发语言里可能开出词法作用域的节点种类，locals 查询是否在那里开作用域、开哪一种，它绑定什么，哪些部分在外层作用域求值（`src/tests/scopes.rs` 的 `SCOPES`）。局部绑定（「绑定规则」）和局部定义的可见性（「抽取结果」）都由这些作用域决定。

- 覆盖：grammar 的 `node-types.json` 里看起来可能开作用域的具名节点都必须有一行，即使只是「不开」：种类是 `block`，或以 `_clause`、`_block`、`_case`、`_body`、`_arm`、`_comprehension`、`_initializer`、`_group`、`_rule` 结尾，或带 `body`、`parameters`、`parameter`、`consequence`、`alternative`、`condition`、`initializer` 字段，或以 `_statement` 结尾且带 `left`、`value` 字段（循环头、`switch` 的初始化）。`scopes_cover_grammar` 检查这一点，并检查每一行的种类在 grammar 里存在；没被这个启发式选中、但需要说明的种类（Rust 的 `let_condition`、TS 的 `ambient_declaration`）也可以列出。
- 与查询一致：`scopes_match_locals_query` 在样例上运行每种语言的 locals 查询，查询开出的每个作用域都对应表里同种类、同类型的一行（命名空间体和 `with` 体的作用域捕获在 `statement_block` 等子节点上，对应父节点的那一行），表里每个开作用域的行都在它的样例上被查询开出。
- 样例：每个开作用域的行都有绑定样例（同一文件的 `SAMPLES`），`scopes_samples_bind` 按「绑定规则」的期望检查，多数样例检查作用域里的声明在作用域之外不绑定。
- 本表与代码里的表一致，由 `scopes_table_matches_design` 检查；改表时按测试打印的内容替换两个标记之间的部分。
- 「作用域」一列：函数（tree-sitter-tags 一侧不继承；`var` 和 Python 的 `:=` 放进这里）、块、类体（只对直接写在类体里的代码可见）、命名空间（照常绑定，但其中的定义是成员，不是局部）、动态（JS 的 `with` 体，见「绑定规则」的作用域指令）、不开。

<!-- scopes:begin -->
| 语言 | 节点 | 作用域 | 绑定 | 在外层求值 |
|---|---|---|---|---|
| js ts tsx | `arrow_function` | 函数 | 参数 |  |
| js ts tsx | `catch_clause` | 块 | `catch (e)` 的参数 |  |
| js ts tsx | `class` | 不开 | 类表达式的名字只在类里可见（不建模，经索引找到） |  |
| js ts tsx | `class_body` | 不开 | 成员经 `this` 访问，不是局部绑定 |  |
| js ts tsx | `class_declaration` | 不开 | 类名绑定在外层的块里 |  |
| js ts tsx | `class_static_block` | 不开 | 它的 `statement_block` 开块作用域 |  |
| js ts tsx | `do_statement` | 不开 | 循环体的块开作用域 |  |
| js ts tsx | `else_clause` | 不开 |  |  |
| js ts tsx | `export_clause` | 不开 | 导出名，不绑定 |  |
| js ts tsx | `export_statement` | 不开 |  |  |
| js ts tsx | `finally_clause` | 不开 |  |  |
| js ts tsx | `for_in_statement` | 块 | `for (const x of …)` / `for (let k in …)` 的变量 |  |
| js ts tsx | `for_statement` | 块 | `for (let i …)` 的变量 |  |
| js ts tsx | `function_declaration` | 函数 | 参数；函数名绑定在外层（提升） |  |
| js ts tsx | `function_expression` | 函数 | 参数；名字只在函数里可见（不建模，经索引找到） |  |
| js ts tsx | `generator_function` | 函数 | 同 `function_expression` |  |
| js ts tsx | `generator_function_declaration` | 函数 | 同 `function_declaration` |  |
| js ts tsx | `if_statement` | 不开 |  |  |
| js ts tsx | `import_clause` | 不开 | import 经索引解析 |  |
| js ts tsx | `labeled_statement` | 不开 | 标签不是绑定 |  |
| js ts tsx | `method_definition` | 函数 | 参数 | 计算出的方法名（`[key]() {}`）；TS 的参数装饰器 |
| js ts tsx | `statement_block` | 块 | `let` / `const` / `class` / 函数声明；TS 的 namespace / module 体是命名空间 |  |
| js ts tsx | `switch_body` | 块 | 各 `case` 里的 `let` / `const`：整个 switch 体 |  |
| js ts tsx | `switch_case` | 不开 | 属于 `switch_body` |  |
| js ts tsx | `switch_default` | 不开 | 属于 `switch_body` |  |
| js ts tsx | `switch_statement` | 不开 | `switch_body` 开块作用域 |  |
| js ts tsx | `ternary_expression` | 不开 |  |  |
| js ts tsx | `try_statement` | 不开 |  |  |
| js ts tsx | `while_statement` | 不开 |  |  |
| js ts tsx | `with_statement` | 动态 | 体是动态作用域：体里声明的名字照常绑定，体外的声明只是候选（名字可能是对象的属性） | 对象表达式 |
| ts tsx | `abstract_class_declaration` | 不开 | 类名绑定在外层的块里 |  |
| ts tsx | `abstract_method_signature` | 函数 | 参数只在签名里 |  |
| ts tsx | `ambient_declaration` | 命名空间 | `declare global { … }` 的块是命名空间，同 `internal_module` |  |
| ts tsx | `call_signature` | 函数 | 参数只在签名里 |  |
| ts tsx | `conditional_type` | 不开 | `infer U` 的类型名不建模 |  |
| ts tsx | `construct_signature` | 函数 | 参数只在签名里 |  |
| ts tsx | `constructor_type` | 函数 | 参数只在类型里 |  |
| ts tsx | `enum_body` | 不开 | 成员不是局部绑定 |  |
| ts tsx | `enum_declaration` | 不开 | 枚举名绑定在外层的块里 |  |
| ts tsx | `extends_clause` | 不开 |  |  |
| ts tsx | `extends_type_clause` | 不开 |  |  |
| ts tsx | `function_signature` | 函数 | 参数只在签名里 |  |
| ts tsx | `function_type` | 函数 | 参数只在类型里 |  |
| ts tsx | `implements_clause` | 不开 |  |  |
| ts tsx | `import_require_clause` | 不开 | import 经索引解析 |  |
| ts tsx | `interface_body` | 不开 | 成员不是局部绑定 |  |
| ts tsx | `interface_declaration` | 不开 | 接口名绑定在外层的块里（类型） |  |
| ts tsx | `internal_module` | 命名空间 | 体（`statement_block`）是命名空间：照常绑定，但其中的定义是成员（`NS.x`），不是局部 |  |
| ts tsx | `mapped_type_clause` | 不开 | 键名只在映射类型里（`owner` 规则） |  |
| ts tsx | `method_signature` | 函数 | 参数只在签名里 |  |
| ts tsx | `module` | 命名空间 | 同 `internal_module` |  |
| rust | `async_block` | 不开 | 它的 `block` 开块作用域 |  |
| rust | `base_field_initializer` | 不开 |  |  |
| rust | `block` | 块 | `let`（从声明结束起）、块里的项（整个块） |  |
| rust | `closure_expression` | 函数 | 参数 |  |
| rust | `const_block` | 不开 | 它的 `block` 开块作用域 |  |
| rust | `else_clause` | 不开 | `if let` 的模式在 `else` 里不可见（见 `let_condition`） |  |
| rust | `enum_item` | 不开 | 变体是定义，不是局部 |  |
| rust | `enum_variant` | 不开 |  |  |
| rust | `field_initializer` | 不开 |  |  |
| rust | `for_expression` | 块 | 模式，从循环体开始 | 被迭代的表达式 |
| rust | `foreign_mod_item` | 不开 | 项是定义，不是局部 |  |
| rust | `function_item` | 函数 | 参数 |  |
| rust | `function_signature_item` | 函数 | 参数只在签名里 |  |
| rust | `function_type` | 函数 | 参数只在类型里 |  |
| rust | `gen_block` | 不开 | 它的 `block` 开块作用域 |  |
| rust | `if_expression` | 不开 | `if let` 见 `let_condition` |  |
| rust | `impl_item` | 不开 | 项是定义，不是局部 |  |
| rust | `let_condition` | 不开 | 模式从条件结束起，到 `else` 之前（`while let`：到循环结束） |  |
| rust | `let_declaration` | 不开 | 模式从声明结束起；`let … else` 的 `else` 块看不到它 |  |
| rust | `loop_expression` | 不开 | 循环体的块开作用域 |  |
| rust | `macro_rule` | 不开 | 宏的元变量不建模 |  |
| rust | `match_arm` | 块 | 模式（守卫和分支体里） |  |
| rust | `match_block` | 不开 |  |  |
| rust | `match_expression` | 不开 |  |  |
| rust | `match_pattern` | 不开 | 属于 `match_arm` |  |
| rust | `mod_item` | 不开 | 项是定义，不是局部 |  |
| rust | `shorthand_field_initializer` | 不开 |  |  |
| rust | `struct_expression` | 不开 |  |  |
| rust | `struct_item` | 不开 |  |  |
| rust | `trait_item` | 不开 | 项是定义，不是局部 |  |
| rust | `try_block` | 不开 | 它的 `block` 开块作用域 |  |
| rust | `union_item` | 不开 |  |  |
| rust | `unsafe_block` | 不开 | 它的 `block` 开块作用域 |  |
| rust | `use_as_clause` | 不开 | import 经索引解析 |  |
| rust | `where_clause` | 不开 |  |  |
| rust | `while_expression` | 不开 | `while let` 见 `let_condition` |  |
| java | `annotation_type_body` | 不开 | 成员 |  |
| java | `annotation_type_declaration` | 不开 |  |  |
| java | `array_initializer` | 不开 |  |  |
| java | `block` | 块 | 局部变量（从声明符结束起）、局部类 |  |
| java | `catch_clause` | 块 | catch 参数 |  |
| java | `class_body` | 不开 | 成员 |  |
| java | `class_declaration` | 不开 |  |  |
| java | `compact_constructor_declaration` | 不开 | 体是块；记录的分量是字段，不建模为参数 |  |
| java | `constructor_body` | 不开 | 属于构造器的函数作用域 |  |
| java | `constructor_declaration` | 函数 | 参数 |  |
| java | `do_statement` | 不开 |  |  |
| java | `element_value_array_initializer` | 不开 |  |  |
| java | `enhanced_for_statement` | 块 | 循环变量，从循环体开始 | 被迭代的表达式 |
| java | `enum_body` | 不开 | 成员 |  |
| java | `enum_constant` | 不开 | 常量的类体是成员 |  |
| java | `enum_declaration` | 不开 |  |  |
| java | `finally_clause` | 不开 |  |  |
| java | `for_statement` | 块 | 初始化里的变量 |  |
| java | `if_statement` | 不开 | `instanceof` 的模式变量不确定（见「定义形式」） |  |
| java | `interface_body` | 不开 | 成员 |  |
| java | `interface_declaration` | 不开 |  |  |
| java | `lambda_expression` | 函数 | 参数 |  |
| java | `method_declaration` | 函数 | 参数 |  |
| java | `module_body` | 不开 |  |  |
| java | `module_declaration` | 不开 |  |  |
| java | `record_declaration` | 不开 | 分量是字段 |  |
| java | `record_pattern_body` | 不开 | 模式变量不确定 |  |
| java | `static_initializer` | 不开 | 它的块开块作用域 |  |
| java | `switch_block` | 块 | 各组里的局部变量：从声明到 switch 块结束，后面的组也看得到（JLS 6.3） |  |
| java | `switch_block_statement_group` | 不开 | 属于 `switch_block` |  |
| java | `switch_expression` | 不开 | `switch_block` 开块作用域 |  |
| java | `switch_rule` | 块 | `case … ->` 的模式变量和体 |  |
| java | `synchronized_statement` | 不开 |  |  |
| java | `ternary_expression` | 不开 |  |  |
| java | `try_statement` | 不开 |  |  |
| java | `try_with_resources_statement` | 块 | 资源变量，从声明结束起 |  |
| java | `while_statement` | 不开 |  |  |
| python | `block` | 不开 | Python 的块不开作用域 |  |
| python | `case_clause` | 不开 | `case` 的捕获绑定所在的函数 |  |
| python | `class_definition` | 类体 | 类体里的名字只对类体直接可见，方法里看不到 | 基类 |
| python | `dictionary_comprehension` | 函数 | `for` 的变量；`:=` 越过它绑定所在的函数 | 第一个 `for` 的可迭代对象 |
| python | `elif_clause` | 不开 |  |  |
| python | `else_clause` | 不开 |  |  |
| python | `except_clause` | 不开 | `as` 的名字是函数的局部变量（离开子句时被删除，不建模） |  |
| python | `finally_clause` | 不开 |  |  |
| python | `for_in_clause` | 不开 | 属于推导式 |  |
| python | `for_statement` | 不开 | 循环变量是函数的局部变量 |  |
| python | `function_definition` | 函数 | 参数 | 默认值、参数注解、返回注解 |
| python | `generator_expression` | 函数 | 同 `dictionary_comprehension` | 第一个 `for` 的可迭代对象 |
| python | `if_clause` | 不开 | 属于推导式 |  |
| python | `if_statement` | 不开 |  |  |
| python | `lambda` | 函数 | 参数 | 默认值 |
| python | `list_comprehension` | 函数 | 同 `dictionary_comprehension` | 第一个 `for` 的可迭代对象 |
| python | `match_statement` | 不开 |  |  |
| python | `set_comprehension` | 函数 | 同 `dictionary_comprehension` | 第一个 `for` 的可迭代对象 |
| python | `try_statement` | 不开 |  |  |
| python | `type_alias_statement` | 不开 | 类型参数只在语句里（`within` 规则） |  |
| python | `while_statement` | 不开 |  |  |
| python | `with_clause` | 不开 | `as` 的名字是函数的局部变量 |  |
| python | `with_statement` | 不开 |  |  |
| go | `assignment_statement` | 不开 | 给已有的变量赋值 |  |
| go | `block` | 块 | `:=`、`var`、`const`（从声明结束起） |  |
| go | `communication_case` | 块 | `case v := <-ch` 的变量和分支里的声明 |  |
| go | `composite_literal` | 不开 |  |  |
| go | `default_case` | 块 | 分支里的声明 |  |
| go | `expression_case` | 块 | 分支里的声明 |  |
| go | `expression_switch_statement` | 块 | 初始化语句的变量 |  |
| go | `for_clause` | 不开 | 属于 `for_statement` |  |
| go | `for_statement` | 块 | 初始化语句和 `range` 的变量（`range` 的从循环体开始） |  |
| go | `func_literal` | 函数 | 参数和结果 |  |
| go | `function_declaration` | 函数 | 参数和结果 |  |
| go | `function_type` | 函数 | 参数只在类型里 |  |
| go | `if_statement` | 块 | 初始化语句的变量（`else` 里也可见） |  |
| go | `method_declaration` | 函数 | 接收者、参数和结果 |  |
| go | `method_elem` | 函数 | 参数只在签名里 |  |
| go | `package_clause` | 不开 |  |  |
| go | `range_clause` | 不开 | 属于 `for_statement` |  |
| go | `receive_statement` | 不开 | `v := <-ch` 的变量属于外层 |  |
| go | `send_statement` | 不开 |  |  |
| go | `type_case` | 块 | 分支里的声明 |  |
| go | `type_switch_statement` | 块 | `switch v := x.(type)` 的别名（每个分支里） |  |
<!-- scopes:end -->

### 绑定规则

`ParsedFile` 的局部绑定（`site.rs` 和 `locals.rs`）按下面的规则解析。规则写在每种语言的 locals 查询里，以模式属性的形式出现，tree-sitter-tags 不读这些属性：

- 作用域的 `local.kind`：`function`（函数、方法、闭包、lambda，以及签名和函数类型：它们的参数只在签名里）、`comprehension`（Python 的推导式，是函数作用域，但 `:=` 跳过它）、`class`（Python 的类体）、`namespace`（TS 的 namespace、module 和 `declare global` 的体：按块绑定，其中的定义是成员）、`dynamic`（JS 的 `with` 体，见下面的作用域指令）。没有这个属性的作用域是块，文件本身是函数作用域。同一个节点被捕获两次时（一个语句块也是命名空间体或 `with` 体）取不是块的那一种。每种节点开哪种作用域见「作用域形式」。
- 定义的 `local.scope "function"`：定义放进最近的函数作用域（JS 的 `var`、Python 的 `:=`）；这个函数的 `body` 是作用域时（JS 的语句块）放进 `body`，参数的默认值看不到它。没有这个属性时放进包含它的最内层作用域。作为函数或类作用域节点 `name` 字段的定义（函数名、类名）放进外层作用域；块作用域的 `name` 字段是它绑定的名字（Java 增强 `for` 的变量），留在块里。
- 定义的 `local.visible`，决定从哪里开始可见：`scope` 是整个作用域，包括声明之前的位置（提升）；`end:<种类>` 从最近的这种节点结束处开始（`let x = x + 1;` 右边的 `x` 是之前的 `x`）；`body:<种类>` 从这种节点的 `body` 开始（`for x in x.iter()`）；`owner:<种类>` 只在最近的这种节点的父节点之内（类型参数在声明它的类、函数或项里），`within:<种类>` 只在最近的这种节点之内（Python 的 `type A[T] = …`），这两种定义放进这个节点外面的作用域，所以它包含的方法也看得到；没有这个属性时从名字本身开始。
- 定义的 `local.namespace`：`type`（类型参数、局部接口和类型别名）或 `both`（类、结构体、枚举，Go 的类型参数），没有时是值。类型位置的引用（`type_identifier`）只找类型和两者，其余位置只找值和两者；限定名的后几段（`a::B`、`ns.T`、`pkg.T`、Java 的 `Outer.Inner`）不找局部定义。
- 定义的 `local.until "<字段>:<种类>|<种类>"`，决定到哪里结束可见：到最近的这类节点的这个字段开始处，节点没有这个字段时到节点结束。只有 Rust 的 `if let` / `while let` 用它，绑定不进入 `else` 分支，也不越过 `while` 的结尾。
- 定义的 `local.redeclare "true"`：Go 的 `:=`，名字在同一作用域里已经声明过（作用域是函数体时也看函数的参数和命名结果）时不是定义，是对已有变量的使用，按引用解析。tree-sitter-tags 只按名字过滤，这样的名字本来就是这个作用域链上的局部定义，过滤的结果不变，所以 `walk` 不需要对应的判断。
- 定义的 `local.certain "false"`：可见范围取决于控制流的形式（Java 的 `instanceof` 模式和记录模式），只给候选，不给绑定。
- 引用的 `local.nonlocal`：Python 的 `global`（`"global"`）、`nonlocal`（`"true"`）语句里的名字，把这个名字移出所在函数的作用域，见下面的作用域指令。
- 捕获的节点可以是一个模式（`{a, b: [c]}`、`(a, *rest)`、`Some(v)`、`Point { x, y: z }`），`locals.rs` 把它展开成它绑定的名字。构造器和路径（`Some`、`Point`）、默认值、类型标注、键、属性和下标目标（`obj.count = 1`）不绑定名字；Rust 模式里大写开头的名字是单元变体或常量（`None`、`MAX`），不绑定。

**安全不变式**：只有规则表明一个声明在引用处可见时，局部绑定才成立，才挡住索引里的候选（`Bound(Local)`，直接跳转）。外层作用域里同名、但在引用处不确定可见的声明（在不提升的形式里声明在引用之后、可见范围取决于控制流、被 `global` / `nonlocal` 移出、引用在 `with` 体里而声明在体外）只作为候选（`Site::possible`，本文件的候选排在前面），这时仍查询索引，不会直接跳到别的文件，也不会直接跳到一个不在作用域里的局部声明。

解析：从引用的求值作用域（包含它的最内层作用域；它在这个作用域节点的「作用域入口」部分里时，是外面那个作用域）向外找同名、同命名空间的定义。离开一个函数或类体之后，再遇到的类体跳过（类体里的名字只对直接写在类体里的代码可见，对其中的方法和嵌套类不可见）。在一个作用域里，取引用之前最后一个可见的声明；没有时取引用之后的提升声明。找到确定的声明就是绑定；找到不确定的声明，结果只有候选。作用域指令改变向外找的路径，见下面的表。

| 语言 | 声明形式与可见范围 | 作用域 | 提升 | 模式 |
|---|---|---|---|---|
| JS / TS | `let`、`const`、`class` 声明：所在块，整个块（闭包可以在声明之前引用）；`var`：最近的函数的函数体，整个函数体；函数声明：所在块，整个块；参数、`catch` 参数：整个函数或子句；`for (let … of …)` / `for (var …)`：循环或函数体。TS：类型参数在声明它的类、接口、类型别名或函数里（类型）；映射类型的键在映射类型里（类型）；接口、类型别名（类型）和枚举、抽象类（两者）在所在块，整个块 | 块（语句块、`for`、`for…in`、`catch`、`switch` 体）；函数（函数、箭头函数、生成器、方法；TS 的签名和函数类型）；命名空间（TS 的 namespace / module 体）；动态（`with` 体） | 函数声明、`var`、TS 的类型声明 | 对象和数组的解构，`= 默认值`和键不绑定；解构赋值（没有 `let` / `const` / `var`）不是声明 |
| Python | 参数、赋值目标、增量赋值、`for` 目标、`with` / `except … as`、`match` 的捕获、`def`、`class`：所在函数的整个作用域；`:=`：最近的函数，跳过推导式；推导式的 `for` 目标：推导式；类型参数（`def f[T]`、`class C[T]`、`type A[T] = …`）：在声明它的函数、类或别名里，类的类型参数在方法里也可见 | 函数（函数、lambda）；推导式；类体（只对直接写在类体里的代码可见） | 函数作用域里的所有名字（赋值之前的引用也绑定到它） | 元组、列表和星号目标；属性和下标目标不绑定；`match` 里只有单段的名字、`*rest`、`**rest` 和 `as` 绑定，类名、关键字、带点的值和映射的键不绑定 |
| Rust | `let`：从声明结束处到块结束；`if let` / `while let`：从这个 `let` 结束处到 `if` 的 `else` 之前或 `while` 结束；`for` 模式：循环体；`match` 分支模式：整个分支（含守卫）；参数、闭包参数：整个函数；块里的 `fn`、`const`、`static`（值）、`struct`（两者）、`enum`、`union`、`trait`、`type`（类型）：整个块；类型参数（类型）和 const 参数（值）：在声明它的项里，含 `where` 子句和 `impl` 的方法 | 块、`for`、`match` 分支；函数（`fn`、闭包、没有函数体的签名、函数指针类型） | 块里的项 | 元组、元组结构体、结构体（字段简写和改名）、引用和 `@` 模式；构造器、路径、范围、宏和大写开头的名字不绑定 |
| Java | 局部变量：从声明符结束处到块结束；增强 `for` 变量：循环体；`try` 资源：从资源结束处；方法、构造器、lambda 的参数和 `catch` 参数：整个方法或子句（记录头里的组件不是局部，是记录的字段）；块里的局部类、记录、枚举、接口（两者）：从名字开始；类型参数（类型）：在声明它的类或方法里；`instanceof` 模式、记录模式：不确定 | 块、`for`、增强 `for`、`catch`、`try` 资源、`switch` 块（一个块里的声明在后面的分组里也可见）、`case … ->` 规则；函数（方法、构造器、lambda） | 无 | 记录模式的组件（不确定） |
| Go | `:=`：从语句结束处，只声明这个作用域里新的名字：同一块里已经声明过的名字，以及块是函数体时函数的参数和命名结果，是对那个变量的使用（`value, err := read()`，`local.redeclare`）；`if`、`for`、`switch` 的初始化语句各有自己的作用域，那里的 `:=` 总是新的；函数里的 `var`、`const`：从这一项结束处；包级的 `var`、`const`：整个文件（与顺序无关）；`range` 变量（只有 `:=`，`for i = range xs` 是赋值）：循环体；`select` 的接收语句（只有 `:=`）：从语句结束处；参数、类型 `switch` 的别名：整个函数或语句；类型参数（两者）：在声明它的函数或类型里 | 块、`for`、`if`、`switch`、各 `case` 和 `default`；函数（函数、方法、函数字面量、接口方法、函数类型） | 包级 `var`、`const` | 多变量声明 |

**作用域入口**：作用域节点里有些部分在外面的作用域求值，那里的引用、作用域和定义都从外面的作用域开始解析（`locals.rs` 的 `ENCLOSING`）。

| 语言 | 在外层作用域求值的部分 | 例子 |
|---|---|---|
| Python | 函数参数的默认值和类型标注、返回类型；lambda 参数的默认值；类的基类和关键字参数；推导式第一个 `for` 的可迭代对象 | `def f(value=value)`、`lambda item=item: item`、`class A(Mixin)`、`[x for x in x]` |
| JS / TS | 方法的计算名（`[key]() {}`）；TS 的参数装饰器。参数的默认值在参数作用域里求值：看得到前面的参数，看不到函数体里的 `var` 和 `let` | `class A { [key](key) {} }`、`f(a = x) { var x }` |
| Rust | 无：泛型约束和 `where` 子句里只有类型，按类型参数的规则解析 | |
| Java | 无：记录头不是作用域，组件是字段；注解和类型标注里只有类型 | |
| Go | 无：接收者的类型在包级解析；接收者里的类型参数（`(l *List[E])`）没有建模，只给索引候选 | |

**作用域指令**：把名字移到另一个作用域、或让名字在运行时才确定的语句，改变向外找的路径（`locals.rs` 的 `Directive` 和 `ScopeKind::Dynamic`）。

| 语言 | 指令 | 解析 | 例子 |
|---|---|---|---|
| Python | `global x` | 所在函数里 `x` 的声明（`x = …`）都是对模块级 `x` 的赋值，只作为候选；从所在函数直接跳到模块，越过所有外层函数，所以内层函数里的 `x` 也解析到模块。模块顶层的 `global` 不起作用 | `def outer(): x = 1; def inner(): global x; return x` 的 `x` 是模块的 |
| Python | `nonlocal x` | 所在函数里 `x` 的声明只作为候选；向外找最近的有 `x` 的外层函数，不找到模块 | 同上换成 `nonlocal`，`x` 是 `outer` 的 |
| JS | `with (obj) { … }` | 体是动态作用域：体里声明的名字照常绑定；离开体之后找到的声明（含体里的 `var`，它在函数作用域里）都只是候选，名字可能是 `obj` 的属性 | `with (o) { use(size) }` 的 `size` 只有候选 |

- 其他语言没有这类指令：Java、Go、Rust、TS（严格模式不允许 `with`，grammar 仍能解析，按 JS 处理）。Python 的 `del x` 不建模：之后的引用仍绑定到 `del` 之前的声明（已知限制，删除后再引用本来就是运行时错误）。
- import 不是局部定义，经索引解析（「跳转到定义」第 2、3 步）。
- 一种语言新增声明形式时，先在「定义形式」的表里归类，再在它的 locals 查询里加模式和属性，在 `tests/binding.rs` 的这种语言的表里加行；每种语言一个表驱动的测试（`site_binding_<语言>`），作用域入口、作用域指令和命名空间各一个（`site_binding_entry_points`、`site_binding_directives`、`site_binding_namespaces`），新增或改动一种作用域时同时改「作用域形式」的表和它的样例，每行是一段源码、引用的位置和期望（绑定到某个声明；只有这些候选，不允许直接跳到别的文件；没有局部定义，由索引回答）。
- 抽取时用同一个 locals 查询过滤引用名（`#is-not? local`，tree-sitter-tags 或按它的规则过滤的 `walk`）。它不读上面的属性，只按名字比较：函数作用域标成 `local.scope-inherits false`，过滤只看引用所在函数自己的局部定义；捕获整个模式的定义（解构）不等于任何名字，不过滤；Rust 的 `if let` / `while let` 绑定、各语言的类型参数和局部类型、Python 的 `match` 捕获、Go `range` 和 `select` 接收的 `:=` 用 `@definition.lectern.local` 捕获，tree-sitter-tags 在 locals 查询里只处理 `@local.scope` 和 `@local.definition`，所以不过滤它们（Go 的这两处要求 `:=`，模式在名字之后的 `:=` 处才完成，而 tree-sitter-tags 按匹配完成的顺序处理，名字上的兜底引用会先到），`if` 和 `while` 也不是作用域（tree-sitter-tags 对每个要过滤的引用检查已经出现的全部作用域，多数 `if` 不绑定名字）。这几处都让引用名多出名字，不会少；`catch`、`switch` 体、签名和函数类型这类作用域让里面的参数和声明不再过滤作用域之外的同名引用，同样只多不少。已知限制：过滤不知道作用域入口，推导式第一个可迭代对象和推导式变量同名时（`[x for x in x]`），这一处用到的外层名字不进入引用名；外层名字是 import 时，查这个名字的引用选不中这个文件。这种写法很少见。

### 存储表

两个命名空间用同一套表结构（[ADR 0033](../adr/0033.md)）：项目文件的 `code_intel.tags`（配额 224 MiB）和依赖的 `code_intel.deps`（配额 32 MiB，见「依赖层」），两者合计 256 MiB，依赖的单元只淘汰依赖的单元。两组表在同一个分库文件里，表名不同：`code_intel.tags` 的是下面的 `ci_*`，`code_intel.deps` 的是同样结构的 `cd_ext`、`cd_def`、`cd_ref`，另加包的路径表 `cd_pkg`、`cd_pkg_file` 和按单元存模块路径（闭包的出边）的 `cd_spec`（「依赖层」）；两组语句由同一个宏按表名前缀生成，`store.rs` 的查找、写入代码对两组共用，按 `TableSet` 区分。

一次抽取是 `cache` 的一个单元，命名空间 `code_intel.tags`（版本 3，不压缩，版本升级的原因见本节「版本」），键是 `Key::new().bytes(oid).str(lang).u64(grammar).u64(query).i64(part)`（`part` 见 `ci_ext`），单元的字节数由本 crate 按行估算（见下）。本 crate 登记的表：

```sql
-- 一次抽取一行。unit 是 cache 分配的单元 ID；单元被淘汰或删除时，外键级联在同一条语句里删除下面三张表里它的全部行
-- （cache 设计「自定义表」的登记规则：每张表有一列 REFERENCES entry (id) ON DELETE CASCADE，且这一列有索引）。
CREATE TABLE ci_ext (
  unit    INTEGER PRIMARY KEY REFERENCES entry (id) ON DELETE CASCADE,
  oid     BLOB    NOT NULL,           -- 20 或 32 字节
  lang    TEXT    NOT NULL,
  grammar INTEGER NOT NULL,
  query   INTEGER NOT NULL,
  status  INTEGER NOT NULL,           -- ExtractStatus：0 Done、1 TimedOut、2 TooLarge、3 Binary、4 Windowed
  part    INTEGER NOT NULL,           -- -1：整份抽取；≥ 0：窗口单元，窗口在文件里的起点字节（ADR 0034，「大文件」）
  part_end  INTEGER,                  -- 窗口单元：窗口的终点字节，即下一个窗口的起点；整份抽取为 NULL
  part_line INTEGER,                  -- 窗口单元：窗口起点所在的行号（从 0 起），续接链时用；整份抽取为 NULL
  canonical INTEGER NOT NULL,         -- 1：抽取的字节就是 oid 那份内容，`ci_def.byte` 可以交给视图；0：原始字节经 canonical_oid 才对上
  UNIQUE (oid, lang, grammar, query, part)
);

CREATE TABLE ci_def (
  unit     INTEGER NOT NULL REFERENCES entry (id) ON DELETE CASCADE,
  name     TEXT    NOT NULL,          -- 原样，区分大小写
  kind     INTEGER NOT NULL,
  line     INTEGER NOT NULL,          -- 窗口单元里也是在整个文件里的行号
  col      INTEGER NOT NULL,
  byte     INTEGER NOT NULL,          -- 名字起点的字节偏移（ADR 0034）；`ci_ext.canonical = 0` 时不交给视图
  end_line INTEGER NOT NULL,
  scope    TEXT,
  flags    INTEGER NOT NULL,          -- 位 0–2：Visibility
  arity    INTEGER
);
CREATE INDEX ci_def_name ON ci_def (name COLLATE NOCASE, unit, kind);
CREATE INDEX ci_def_unit ON ci_def (unit);

-- 每个单元每个名字一行，不存位置：位置在用到时对候选文件现场抽取。
CREATE TABLE ci_ref (
  name TEXT    NOT NULL,
  unit INTEGER NOT NULL REFERENCES entry (id) ON DELETE CASCADE,
  PRIMARY KEY (name, unit)
) WITHOUT ROWID;
CREATE INDEX ci_ref_unit ON ci_ref (unit);
```

- 状态不是 `Done` 的抽取也写一行 `ci_ext`、没有定义和引用：超时和超过上限的内容不会在每次打开项目时重试。
- `ci_def_name` 用 `NOCASE`（ASCII 大小写折叠）：跳转按 `name = ?1 COLLATE NOCASE` 取出后在 Rust 里按原样比较；大小写不敏感的语言包可以直接用这个索引。`ci_ref` 只按原样的名字查。`ci_def_name` 是 (name NOCASE, unit, kind)（[ADR 0037](../adr/0037.md)，命名空间版本 2）：一个名字的行按 (unit, kind, rowid) 的顺序直接从索引读出，按名字查找因此分页，每页一条语句，从上一页最后一行的 (unit, kind, rowid) 之后接着读；在给定文件里的查找按 (name, unit) 直接定位（每个 OID 在 `ci_ext` 的键和 `ci_def_name` 里各查一次）；剩下多少行只读索引就能数出；名字表按索引的顺序读 (name, unit, kind)，不读表。`ci_ref` 的主键 (name, unit) 给出同样的顺序，查引用的单元也按页读。`store_lookups_by_name_use_the_name_and_unit_index` 用 `EXPLAIN QUERY PLAN` 检查这几条语句都按名字和游标定位、不扫描整张表、不排序。
- 一页的 `LIMIT` 数的是索引给出的行，不是通过筛选的行（[ADR 0037](../adr/0037.md)）：页先在索引里取名字的下 256 行（子查询），再 `LEFT JOIN` 到 `ci_ext`（只连 `Done` 的单元）和定义的行，每一行原样出来一次，版本在 Rust 里与查询的版本比对。所以一条语句最多读 256 行，不论其中多少是旧版本、其他语言或名字的其他大小写；一次扫描覆盖查询的所有版本，不按版本分组；一个单元的同名定义可以跨页；一页可以一个定义也没有，原始行少于 256 时查找结束（`store_lookup_pages_bound_the_rows_read`、`store_lookup_pages_go_on_inside_a_unit` 用测试里的计数检查每条语句读的行数）。SQLite 按 (unit, kind) 定位、在其中按 rowid 过滤，所以一个单元里同一种类的同名定义超过一页时，后面的页会再走过它前面的行，以这个单元自己的行为限。
- `ci_def_unit`、`ci_ref_unit` 供删除单元和按单元读定义、引用。
- 版本过滤：所有读取都限定在当前语言表的版本上。每种语言有一个当前三元组（语言、grammar 版本、查询版本）。分页的查找和名字表的第一步在 Rust 里比对 `ci_ext` 的三元组；存在检查和在给定 OID 里的查找把三元组作为参数（兼容组每 4 种语言一条语句）：

  ```sql
  -- 按名字查定义的一页（跳转第 3 步、按名字查定义）：索引里的下 ?5 行，每行原样出来一次
  SELECT d.name, d.kind, d.line, d.col, d.end_line, d.scope, d.flags, d.arity,
         e.oid, e.lang, e.grammar, e.query, raw.unit, raw.kind, raw.id
  FROM (SELECT rowid AS id, unit, kind FROM ci_def INDEXED BY ci_def_name
        WHERE name = ?1 COLLATE NOCASE AND (unit, kind, rowid) > (?2, ?3, ?4)
        ORDER BY unit, kind, rowid LIMIT ?5) AS raw
  LEFT JOIN ci_ext e ON e.unit = raw.unit AND e.status = 0
  LEFT JOIN ci_def d ON d.rowid = raw.id AND e.unit IS NOT NULL;
  -- 在给定的 OID 里按名字查定义（第一批里离查询最近的文件，每条至多 500 个 OID）
  SELECT … FROM ci_ext e CROSS JOIN ci_def d INDEXED BY ci_def_name ON d.unit = e.unit
  WHERE d.name = ?1 COLLATE NOCASE AND e.status = 0 AND (…三元组…) AND e.oid IN (?15, …) LIMIT ?14;
  -- 按名字查引用的单元的一页（查找引用第 1 步）
  SELECT e.oid, e.lang, e.grammar, e.query, raw.unit
  FROM (SELECT unit FROM ci_ref WHERE name = ?1 AND unit > ?2 ORDER BY unit LIMIT ?3) AS raw
  LEFT JOIN ci_ext e ON e.unit = raw.unit AND e.status = 0;
  -- 名字表的第二步的一页：游标所在名字的其余行，再接后面的名字，两段按索引顺序归并，不排序
  SELECT name, unit, kind, rowid FROM ci_def INDEXED BY ci_def_name
    WHERE name = ?1 COLLATE NOCASE AND (unit, kind, rowid) > (?2, ?3, ?4)
  UNION ALL
  SELECT name, unit, kind, rowid FROM ci_def INDEXED BY ci_def_name WHERE name > ?1 COLLATE NOCASE
  ORDER BY 1 COLLATE NOCASE, 2, 3, 4 LIMIT ?5;
  ```

  grammar 或查询升级、`EXTRACTOR_VERSION` 变化之后，旧版本的单元不再出现在任何结果里，只等淘汰。
- 单元字节数：`64 + Σ定义 (2 × 名字长度 + 作用域长度 + 40) + Σ引用 (2 × 名字长度 + 24)`，近似行加两份索引的大小，计入 `meta.bytes` 和命名空间配额。
- 写入的键来自读到的字节：任何一次抽取（批量、按需、限时搜索、第 5 层）写入之前，都对实际读到的字节重新算 OID（原始字节的 OID；工作区一侧与排队时的 OID 不同时再用 `canonical_oid` 算一次），用这个 OID 作键。排队时记下的 OID 只用来挑选要读的文件，不作键。两者不同（排队之后文件又被改了）时，结果只写在实际的 OID 下，原来那个 OID 的 `indexed` 提示不置位；路径的新内容随 fs_watch 的事件进入覆盖部分，有效 OID 变成实际的 OID 后提示才会匹配。所以一份内容的抽取结果不会写到另一个 OID 的键下。
- 写入：一批（见「调度、批次与取消」）在一个写事务里建单元、插入全部行，一批约 100 KB，远低于 `MAX_BATCH`；写失败（`Dropped`）时这一批的结果留在内存里供本次查询使用，下次需要时重新抽取。
- 淘汰：按单元的访问时间（LRU，精度 1 小时）。本 crate 对热启动存在检查的命中和查询命中标记访问（存在检查本身只读，命中的单元排进队列，热启动之后再标记，见「调度、批次与取消」），所以访问时间即「最后一次出现在某个 worktree 里或被用到的时间」；不在任何 worktree 里的旧 OID 的行留给淘汰，不主动删除。当前 worktree 的单元也可能被淘汰，内存里的 `indexed` 提示按「输入代数与派生状态」处理。
- 规模（调研第 4 节，Rust 语料的密度）：`cloudprint-miniapps` 形状约 1 MB；10 万文件约 200–250 MB，在 256 MiB 的配额以内。超过配额时最久没出现的 blob 先被淘汰，下次需要时重新抽取。调研的引用数只含调用一类的引用；兜底的引用模式（见「外部格式」）让每个单元多出普通标识符和成员名的行，第 1 步的夹具里每个文件的引用名是原来的 1.5–3.5 倍。第 3 步用 `testing` 的 10 万文件合成仓库（`testing::synthetic`：语言比例约一成不是源码，每个源码文件约 8 KiB、每个文件里的名字各不相同）测 `code_intel/disk_100k`：9 万个源码文件的单元声明 279.7 MB（266.7 MiB，每个单元平均 3.1 KB），超过 256 MiB 的配额约 4%，写入过程中淘汰了 80 次；分库文件（含 `-wal`）298 MiB。所以 10 万文件的估算校准为约 270 MiB，与配额相当：这个规模的仓库里最久没出现的约 5% 的单元会被挤出，下次用到时重新抽取（「调度、批次与取消」的配额一条保证不会反复抽取）。合成语料每 8 KiB 有 34–53 个定义（函数为主，函数体是普通的调用和条件语句）、8–16 个不同的引用名（第 2 步由夹具拼成的语料是 99–152 个定义、17–50 个引用名）；真实仓库的密度由第 5 步在 macOS 上用真实仓库核对，配额是否调整留到那时决定。
- 版本：表结构或单元的键变化时命名空间版本加一；只是抽取规则变化时改 `EXTRACTOR_VERSION`，旧单元不再命中、逐步淘汰。版本 2：`ci_def_name` 从 (name NOCASE) 改为 (name NOCASE, unit, kind)，分页的查找、在给定文件里的定位和名字表的按索引读取都要它；旧版本的表在第一次使用时删除，单元随之后的写事务删除、重新抽取一次（[cache 设计「自定义表」](cache.md#自定义表)）。版本 3（ADR 0033、0034，「验收」第 6 步）：`ci_ext` 加 `part`、`part_end`、`part_line`、`canonical` 和状态 4，唯一键和单元的键加 `part`，`ci_def` 加 `byte`；配额从 256 MiB 改为 224 MiB。升级同样重新抽取一次。
- 整份抽取与窗口单元（ADR 0034，「大文件」）：存在检查和 `indexed` 提示只看 `part = -1` 的行（存在检查的语句加 `AND part = -1`，`UNIQUE` 的前缀仍可用）。按名字查定义、查引用和名字表都不看 `part`：窗口单元的状态是 `Done`，它们的定义和引用与整份抽取的一样找到，名字表的第一步取出全部 `Done` 的单元（含窗口单元，按 OID 经映射过滤），所以打开过的大文件的名字也在 ⌘K 里；`Windowed` 的标记行没有定义和引用，不是 `Done`，不进入名字表的单元集合。一个 OID 的整份抽取是 `Windowed` 时，它的内容就是这个 OID 的窗口单元；查询用到这个文件的定义时按 `(oid, lang, grammar, query)` 取出它的全部窗口（`part >= 0`，按 `part` 排序，`UNIQUE` 的索引直接给出这个顺序）。
- 配额（[ADR 0033](../adr/0033.md)）：`code_intel.tags` 224 MiB。10 万文件的合成仓库实测约 270 MiB（上一条「规模」），所以这个规模的仓库更早进入满额模式，被挤出的约 17% 的单元由第 1 层和查询现场补上（「调度、批次与取消」的配额一条、「完整性的核算」的满额模式）。`code_intel/disk_100k` 照常只记录。

### blob → 路径映射

每个打开项目的当前 worktree 一份，只在内存里，打开项目后重建，不持久化。查询只返回映射里有的 OID，所以已经改掉的定义、已删除的文件不会出现在结果里。

```rust
struct LiveMap {
    base: Arc<BaseMap>,        // 随 project 的文件列表整体替换
    overlay: Arc<Overlay>,     // 随工作区内容的变化替换，通常很小
    indexed: Arc<IndexedHints>, // 每个路径一条：（有效 OID 的前 8 字节、淘汰代数、语言包代数），提示，见「输入代数与派生状态」
}

struct BaseMap {
    files: Arc<FileList>,      // project 快照里的文件列表，带 OID
    table: LanguageTable,      // 建映射时的语言表
    langs: Box<[u8]>,          // 每个文件列表条目的语言（语言表里的位置加一，0 为不在映射里）
    prefixes: Box<[u64]>,      // by_oid：OID 前 8 字节，按前缀排序
    indices: Box<[u32]>,       // by_oid：对应的文件列表下标；查找时用 FileList::oid 核对完整 OID
}

struct Overlay {
    /// 按路径字节序。路径在文件列表里时遮住它的 index 条目。
    entries: Box<[(RelPath, WorktreeState)]>,
    by_oid: Box<[(u64, u32)]>,
}

enum WorktreeState {
    /// 工作区内容与 index 不同（或是未跟踪的文件），这是它的 OID。
    Content(BlobId),
    /// 工作区里没有这个文件（未暂存的删除、sparse 范围外的条目）。
    Absent,
    /// 存在，但不建索引：超过 1 MiB、读不出。内容不知道，查询计为未搜（「完整性的核算」）。
    Unindexable,
    /// 不是本 crate 索引的源文件：符号链接、路径中间是符号链接、其他种类的文件、LFS 这类过滤器的指针文件。不参与查询的核算。
    Excluded,
}
```

规则：

- 基础部分来自 `ActiveTree::files`：每条有 OID、语言受支持、`FileKind` 是 `File` 或 `Executable` 的文件。子模块和符号链接不进入。
- 覆盖部分来自工作区：对一个要重新读的路径，经 `SafeRoot::read`（上限 1 MiB）读出内容，按仓库的哈希算法计算 `Oid::for_blob`。等于文件列表里的 OID 时删掉这个路径的覆盖项；文件列表里有这个路径、OID 不等时调用一次 `canonical_oid`：返回 None（设置了 clean / process 过滤器，如 LFS 的指针文件）记为 `Excluded`，等于文件列表里的 OID（`eol`、`core.autocrlf` 这类内置转换）时删掉覆盖项，否则记为 `Content`；文件列表里没有这个路径时记为 `Content`；`NotFound` 记为 `Absent`；`TooLarge`、读取失败记为 `Unindexable`；符号链接、`PathThroughSymlink`、其他种类的文件记为 `Excluded`。路径的语言不受支持时不读；这样的路径不在文件列表里、却是一个目录时（目录事件），请求一次工作区比较。
- 覆盖项的 OID 按原始字节计算。未跟踪的文件不调用 `canonical_oid`：设置了 `eol` 一类转换的未跟踪文件按原始字节的 OID 存结果，与以后 `git add` 得到的 OID 不同，暂存之后再抽取一次（已知限制，只多一次抽取）。
- 查找按（OID、语言）进行：在两个 `by_oid` 里按前缀二分查找，核对完整 OID，跳过基础部分里被覆盖项遮住的路径，再只留按路径判断的语言等于这个单元 `ci_ext.lang` 的路径。同一份字节放在 `.java` 和 `.js` 下 OID 相同，`java` 的单元只对应 `.java` 的路径。
- 基础部分、覆盖部分何时重算见「输入代数与派生状态」。
- 内存：10 万文件的 `by_oid` 1.2 MB（每条 12 字节，前缀和下标分两个数组存放），每条的语言 0.1 MB，`indexed` 提示约 1.6 MB（每条 16 字节）；覆盖项通常几十条，未跟踪文件最多 20 000 条（`git` 的上限）时约 1.5 MB。
- 文件夹项目：`FileList` 没有 OID，基础部分为空，每个文件在第一次被读到时作为 `Content` 加入覆盖部分（上限是文件夹遍历的 20 000 个条目）。根目录在文件列表就绪后打开，文件列表里的每个文件随即读一遍（每次换文件列表时也是），没有 fs_watch 订阅和工作区比较；第 4 步实现，见「与设计的差异」。

### 输入代数与派生状态

内存里的派生状态（映射、覆盖部分、`HeadDiff`、`indexed` 提示、调度游标、名字表、预取结果）只按下面这一条规则更新，「增量更新」「语言包的变化」里的表只说明哪个事件让哪个输入的代数加一。

**输入**：每个打开项目一组，每个输入一个单调递增的代数。

| 输入 | 代数什么时候加一 |
|---|---|
| 文件列表 | `Projects::snapshot` 里当前 worktree 的 `files` 换成另一个 `Arc`（`project` 在路径或 OID 有变化时才替换） |
| 子项目边界 | 快照里的 `packages` 换成另一个 `Arc` |
| HEAD | 当前 worktree 的 HEAD 提交（`GitAccess::head`）与记下的不同。提交、`reset`、切换分支都会改变它，而 index 和文件列表可能不变 |
| index | index 校验和（`GitAccess::index_checksum`）与记下的不同 |
| 忽略规则与配置 | `.gitignore`、`.gitattributes`、配置文件可能变了 |
| 工作区内容 | 一次重新读改变了某个覆盖项，或要求做一次工作区比较（目录级变化、事件丢失） |
| 语言包 | `GrammarSource::generation` 变化 |
| 淘汰 | `cache` 的淘汰代数变化：`code_intel.tags` 有单元被淘汰、删除，或分库被重建（本 crate 不在那次写事务里） |
| 写入 | 本 crate 提交了一批单元 |

映射代数不是独立的输入：它由文件列表、工作区内容、语言包三者的代数合成，任何一个变了它就变。映射记下算出它时这三个代数（它的来历）：游标的依赖和 `indexed` 提示的戳记都取映射的来历，不取当前代数。语言包代数已经加一、映射还没重建的间隙里，旧映射带的是旧版本的 `Lang`，按当前代数戳记会让旧版本的命中看起来是新版本的。各项目共用的输入（语言包、淘汰）在比较处直接读共用的值，不在项目里存副本：副本会落后（语言包的一轮与项目打开同时进行时，新项目可能读到旧代数、又不在这一轮通知的项目里），落后的副本让派生状态永远「不一致」。语言表和它的代数在持有打开项目表的锁时一起更新，所以同时打开的项目要么在这一轮的通知里，要么一开始就读到新表。

**派生状态**：每一项记下算出它时所依赖输入的代数。

| 派生状态 | 依赖的输入 | 代数不一致时 |
|---|---|---|
| 基础映射 | 文件列表、语言包 | 重建 |
| 工作区比较：`HeadDiff` 和「要重新读的路径」 | HEAD、index、文件列表、忽略规则与配置、语言包（决定哪些路径受支持）、工作区内容里的比较请求 | 再做一次 `changed_paths` |
| 覆盖部分 | 工作区比较、文件列表、语言包 | 与比较结果对齐，见下 |
| `indexed` 提示 | 每条按（路径、有效 OID、淘汰代数、映射来历里的语言包代数） | 不匹配即视为未入索引 |
| 第 3–5 层的游标、第 3 层的范围 | 映射的来历、子项目边界、淘汰、第 3 层的范围本身（焦点只经它起作用：焦点变了、范围没变时不重建） | 重建游标 |
| 候选分层、依赖子项目的范围、限时搜索的范围 | 子项目边界、映射代数 | 每次查询按自己的快照计算，不缓存 |
| 名字表 | 映射代数、淘汰、语言包、写入 | 按「名字表」的规则重建 |
| 预取结果 | 映射代数、淘汰、语言包 | 丢弃，重新查询 |
| `head_clean` | `HeadDiff` 记下的 HEAD、index、文件列表代数；还没有消费的事件输入（HEAD、index 的检查、比较请求、重新读集合） | 任何一个不一致或还有待消费的输入时，结果按「与 HEAD 不同」处理，直到核对任务处理完 |

**执行**：事件只让输入代数加一，并请求一次核对。每个项目一个核对任务（`Background`，`BusyGuard`，同时最多一个，运行期间又有代数变化的，结束后再运行一次）。它先读一遍当前代数，按上表的依赖顺序（基础映射 → 工作区比较 → 覆盖部分 → 游标 → 名字表）重算不一致的项。每项结果在装上之前再核对一次它依赖的代数，期间又变了的结果丢弃，留给下一轮，所以按旧输入算出的状态不会被装上。

几条由这个规则得出、实现时容易漏掉的情况：

- 只有子项目变了（`ChangedParts::packages`，文件列表不变）：子项目边界代数加一，第 3 层的范围和游标、候选的「同一子项目」和「依赖的子项目」层、限时搜索的范围都按新的子项目重算。
- 提交只改变 HEAD：index 和文件列表都不变时，HEAD 代数仍然加一，`HeadDiff` 重新比较，刚提交的文件变为与 HEAD 相同。
- 忽略规则或配置变了：下一次工作区比较之后，覆盖部分与比较结果对齐。重新读的路径是比较给出的路径加上现有的全部覆盖项；现有的未跟踪覆盖项（不在文件列表里）没有出现在比较结果里时，说明它现在被忽略或已经不在，从覆盖部分删除；`untracked_capped` 为 true 时比较结果不完整，这一步不删除。
- 有效 OID 变了：一个路径的有效 OID 是它的覆盖项的 OID，没有覆盖项时是文件列表的 OID。`indexed` 提示每条存（有效 OID 的前 8 字节、淘汰代数、语言包代数），读的时候与当前的有效 OID 和代数比较，所以 OID 一变提示就不再匹配，不需要逐个清除；新 OID 的存在检查命中或写入成功之后才重新置位。命中或写入的版本与当前语言表、映射的语言表里这种语言的版本都相同时才置位。
- 淘汰：淘汰代数加一后全部提示不再匹配，第 3–5 层的游标重建、按层重新做存在检查。仍在的单元只重新置位、不重新抽取，被淘汰的重新抽取。查询现场用到一个文件的抽取结果时（第 2 步的 import 目标、引用的候选文件、限时搜索的命中文件）先做存在检查，没有就现场抽取，并按所在层排队写入。

#### 查询的视图

一次查询的结果与一个视图一致。查询在取快照时记下视图（`query::View`），之后每一步只读这个快照和视图允许的共享输入；读完一步之后在一个地方（`Inner::check_view`）与当前比较一次，按比较结果处理。

- 快照只来自一个发布的对象（`State::published`，`Published`）：核对任务在消费完全部输入（基础映射和覆盖部分对当前输入都已完整，`pending` 为假）的时候，在项目锁下整体发布当前映射、建它用的语言表（`BaseMap::table`）、停用的语言、会话的根、HEAD 树和子项目。查询只读它，不读核对中的状态，也不读当前的全局语言表：语言包刚升级、映射还没重建时，查询仍按旧表的版本查，与旧映射的提示戳记一致。核对进行中，上一个发布的对象照常给查询用；新会话（项目刚打开、刚切换 worktree）还没有发布时，问来的查询排进 `State::waiting`，发布时开始。文件夹项目打开时，基础映射建好、文件还没读进覆盖部分的那段时间没有可用的快照（`view_folder_project_opening_has_no_snapshot_before_its_overlay`）。核对任务消费完输入、仍建不出基础映射（没有当前 worktree、文件列表 `Unavailable`）时，记下原因（`State::unavailable`）：等着的查询以 `Failed(Unreadable(原因))` 结束，之后问来的查询在没有核对任务运行时立即同样结束，不会一直 `Running`；文件列表还在 `Pending` 时照常等待（`view_queries_end_when_no_snapshot_can_be_published`）。
- 发布对象另记映射是否缺工作区内容（`Published::incomplete`）：根目录打不开（什么都没读到），或最近一次工作区比较失败（覆盖部分停在比较之前）。这样的快照里，没有读过的文件都不算已入索引（`Unverified`），结果也不会是 `Complete`：比较本来会列出的未跟踪、已修改的文件映射不知道，按至少一个未搜计入 `unsearched`；比较成功之后的发布恢复（`view_map_without_the_worktree_is_never_complete`）。

- 视图的成员：会话（含当前 worktree 和它的根；项目关闭、当前 worktree 改变时结束）、映射代数、淘汰代数（提示戳记用的那个：`Project::evictions`，命名空间满了之后是冻结的那一代，与批量层同一个值，所以批量层自己的配额淘汰不让查询以 `Stale` 结束，见「完整性的核算」的满额模式）、命名空间是否已满（变化时按淘汰处理）、语言表代数、提示置位次数（`State::hinted`：每次在项目锁内让某条提示从不匹配变为匹配就加一，只有 `State::hint_paths` 一处置位提示）。文件内容按读核对：查询读的每个文件都要是快照的映射给这个路径的内容（`read_content`），否则这个文件计入 `unsearched`。
- 映射和语言表是快照自己的副本（`Arc`），查询读的是它们，不是共享状态，所以运行中的查询不因新映射、新语言表失效：结果与旧视图一致（位置带旧 OID，打开前核对）。查询手里由映射推出的数据（第 1–3 步的结果）必须与快照同一份映射：复用预取要求会话、映射、淘汰、满额、语言表都与查询任务自己取的快照相同（`View::same_basis`；`query` 在调用方线程上的比较只决定交不交给任务，`query` 返回到任务取快照之间发布的新映射会让它作废、重做第 1–3 步）；`check_view` 也先比较数据的视图与快照的映射和语言表，不同按淘汰处理（重做一次）。
- 比较点：跳转在第 1–3 步（含复用的预取）之后、限时搜索的范围读完提示之后、搜索结束算完计数之后；查引用在 `ci_ref` 那一遍之后、范围之后、搜索之后；`Named` 在计数之后。每次发布结果（`publish_in`）还在决定发布顺序的锁（`publishing`）下检查会话。
- 变化的处理：只多了提示（`Grew`），就再查一次索引，把新出现的候选追加进结果：读提示之前已置位的提示，它的写入在第二次查索引之前已经提交，所以不会再漏；有淘汰（`Evicted`），还没发布结果时用新快照重做一次（跳转的第 1–3 步，`Named` 整个），已经发布了或重做时又变了，查询以 `Failed(Stale)` 结束，已发布的结果留下，消费方再问一次；查引用从第一个文件就交出结果，所以有淘汰就以 `Stale` 结束；会话结束（`Ended`），查询已随会话关闭，不再发布任何结果。任何变化之后都不报 `Complete`。
- 会话在触发它的事件里同步结束：`Changed` 带来新的当前 worktree 时（`switch_session`）、`Closed` 时，在 `publishing` 锁下结束旧会话、关闭它的查询（语义请求的 `cancel` 在放开这把锁之后调用，查询持有的映射和预取的结果交给 `Background` 任务释放：关闭查询可能在 UI 线程上）；之后不会再发布旧会话的结果，也不会用旧映射回答新 worktree 的文本；新会话发布快照之前，查询等它（不再用空映射或只有基础部分的映射回答、报 `Complete`）。
- 查询读的、不来自发布对象的输入，逐项：会话、淘汰、提示置位次数（视图的成员，按上面比较）；提示本身（映射里共享的 `IndexedHints`，增长由置位次数覆盖）；在途抽取的键（取快照时读，计入 `unsearched`，只会让结果更保守）；会话的根（发布对象里的，发布时还没打开时用同一会话的根，路径相同）；当前文本的语言（映射里这个路径的语言；映射没有这个路径时按扩展名在快照的表里找，只决定怎么解析当前文本）；审阅集合（取快照时读的焦点，只影响排序和分层，不影响完整性）；解析扩展点的实现（每次激活注册一次）；名字表（查询不读，`Named` 直接查存储）；语义导航的提供者集合（只影响 `semantic` 一栏，不影响 `state` 的完整性；查询开始时选定要问的提供者）；`coverage`（查询不读）。

### 名字表

- 内容：当前 worktree 映射里所有 OID 的定义名（只读 `code_intel.tags`，不含依赖，ADR 0033），去重（区分大小写），NFC 统一，按字节序排序；每个名字带它出现过的 `DefKind` 位集，⌘K 用它显示图标或过滤。
- 构建：后台任务里先扫描一遍 `ci_ext`，取出语言、grammar 版本、查询版本与当前一致的全部 `Done` 的 `(unit, oid)`（不论 `part`：打开过的大文件的窗口单元也在其中，ADR 0034），按映射以（OID、语言）过滤成单元集合；再按 `ci_def_name` (name NOCASE, unit, kind) 的顺序分页读索引（每条语句 4096 项，不读表，语句之间检查是否停止），跳过集合以外的单元。大小写不同的同名在索引里相邻，所以每个名字只和它所在的那一串比较：同一串里每种写法记一次、合并种类位集，串结束时交给 `NameTableBuilder`；不为每个定义分配，也没有哈希表，每个不同的名字做一次 NFC（NFC 之后相同的写法由 builder 合并）。正在入索引的 OID 不在其中，下一代名字表再补上。
- 代：名字表依赖的输入见「输入代数与派生状态」，任何一个变化时代数加一。`names` 发现仍有持有者的表代数落后时开始重建，重建期间仍返回旧表（`Ready`）。
- 交付：每个项目同时最多一次构建，构建期间的 `names` 返回 `Building`（或仍有持有者的旧表），不另起一次。建好后在 `CodeIntelEvent::Names` 里交出 `Arc<NameTable>`，同时把 `Weak` 换成指向新表；所以第一次调用返回 `Building` 之后，监听方从事件里拿到表，之后的 `names` 只要表还有持有者就返回 `Ready`。所有持有者都释放之后，下一次 `names` 重新构建。
- 持有期间的刷新：表有持有者（`Weak` 能升级）时，只要代数落后（批次写入、映射换代、淘汰代数或语言包变化），本 crate 就在后台重建，建好后发出新的 `Names`，持有方换上新表，不需要再调 `names`。重建有间隔：上一次构建开始之后至少过 `max(1 s, 2 × 上一次构建的耗时)` 才开始下一次，期间的变化并进下一次；间隔未到时登记一个到期时刻，由看门狗线程（见「调度、批次与取消」）到期后提交重建任务。10 万文件的仓库在建索引期间因此最多每 2 秒左右重建一次，CPU 不会全花在名字表上。构建开始时取映射和淘汰代数的快照，表的代数就是这份快照的代数；建好时代数已经又落后的，照常交出，再按间隔排下一次。持有者全部释放后不再重建。
- 内存：10 万文件约 4–8 MB，只在 ⌘K 持有期间存在（ADR 0030）；重建期间新旧两份同时存在，最多约 16 MB，持有方换上新表后旧表释放。本 crate 只保留 `Weak`，事件分发完之后没有监听方保存的表随事件值释放。

## 数据流与线程

```
ProjectEvent ─▶ [Background + BusyGuard] 重建映射、订阅 fs_watch、工作区比较 ─▶ 覆盖部分
                         │
set_focus ─▶ 第 1 层 [Interactive + BusyGuard]、第 2 层 [Background + BusyGuard] ─┐
                                                                                ├─▶ 抽取 ─▶ cache 自定义表
映射里其余的 OID ─▶ 第 3–5 层 [Batch + BackgroundGuard，按批暂停] ───────────────┘
WorktreeBatch ─▶ 脏集合 ─▶ [Background + BusyGuard] 重新读 ─▶ 覆盖部分 ─▶ 按所在层排队
query ─▶ [Interactive + BusyGuard] 当前文件 → import → 索引 → 限时搜索 ─▶ QueryResults ─▶ CodeIntelEvent
```

### 线程与守卫

UI 线程只调用 `set_focus`、`query`、`results`、`close`、`prefetch`、`names`、`coverage`：入队、读原子值或取锁复制 `Arc`，不做 IO、解析和 git 操作（AGENTS.md「禁止事项」）。其余工作都经 `PluginHandle::own_with` 提交到 executor：

| 工作 | 守卫 | 优先级 |
|---|---|---|
| 查询（跳转、引用、按名字）、限时搜索、第 1 层的抽取 | `BusyGuard` | `Interactive` |
| 第 2 层的抽取 | `BusyGuard` | `Background`，同时最多 2 个任务 |
| 打开项目、文件列表换代、工作区比较、按 `WorktreeBatch` 重新读、名字表构建、查询结果的缓存写入 | `BusyGuard` | `Background` |
| 第 3–5 层：存在检查、抽取、写入；第 5 层读其他 worktree 的 index | `BackgroundGuard`：第 3 层用 `background()`，第 4、5 层用 `background_yielding()`（「交互让路」） | `Batch`，每个批量线程最多一个任务 |
| 依赖层：读包的清单、算闭包、求 OID、存在检查、抽取、写入 `code_intel.deps`（ADR 0033） | `BackgroundGuard`（`background_yielding()`） | `Batch`，与第 3–5 层同一批任务 |
| 打开项目、lockfile 或清单变化时读 lockfile、算直接依赖的身份（ADR 0033） | `BusyGuard` | `Background`，每个项目同时最多一个 |
| 跳转时依赖目标的按需读取与抽取（ADR 0033） | `BusyGuard`（查询的） | `Interactive`，计入查询的预算 |
| 打开的大文件的窗口链：可见窗口和前后各一个（ADR 0034） | `BusyGuard` | `Interactive` |
| 打开的大文件的窗口链：其余窗口 | `BackgroundGuard`（`background()`，不让路） | `Batch`，批量层最前面 |
| 当前文本的身份与行检查点（大文件，与 ADR 0037 的预热同一个任务） | `BusyGuard` | `Background` |
| 截止时间的看门狗 | 不取守卫；没有在途抽取时阻塞等待，停用或 executor 关闭时退出 | 专用线程 `code-intel-watchdog`，每次激活最多一个 |

第 3–5 层满足 ADR 0029 的四条条件：跳转和 ⌘K 按「跳转到定义」的顺序现场回答，不等它们；结果按 OID 存进 `cache`，暂停、取消或退出不丢已写入的批次；每批之间检查暂停和取消；不是用户正在等的操作。

### 建索引的分层

| 层 | 内容 | 来源 | 让路（ADR 0033） |
|---|---|---|---|
| 1 | 当前文本、抽屉里正在看的候选文件；超过 1 MiB 的打开文件按窗口抽取（ADR 0034，「大文件」） | `Focus::Current`、`Focus::Drawer` | 不在批量层；窗口链的批量部分不让路 |
| 2 | 本次审阅改到的文件、agent 本会话碰过的文件，以及这些文件 import 的文件（解析扩展点给出；#17 的空实现不给出） | `Focus::Review`、`Focus::Touched` | 不在批量层 |
| 3 | 当前子项目：包含当前文本（没有时取审阅集合的第一个文件，再没有时取项目打开时的路径，根目录除外）的最深子项目；没有子项目时是整个 worktree；有子项目、但没有这样的路径或没有子项目包含它时为空，全部属于第 4 层 | `ActiveTree::packages` | 不让路 |
| 4 | 当前 worktree 的其余文件 | 映射 | 让路 |
| 依赖 | 当前子项目清单里直接依赖的声明闭包（ADR 0033，「依赖层」） | 子项目的清单、lockfile | 让路 |
| 5 | 其他 worktree 里 OID 不在缓存中的文件 | `GitAccess::tracked` | 让路 |

- 批量层每批开始时按表的顺序选层：打开的大文件的窗口链（第 1 层留给批量层的部分）→ 第 3 层 → 第 4 层 → 依赖层 → 第 5 层。依赖层不改变第 1–5 层的编号和含义，`app/index_100k` 的里程碑（第 3、4 层完成）不等它。

- 第 3、4 层用游标遍历文件列表的区间（`range_under`），不把 10 万个条目放进队列；`indexed` 提示与当前有效 OID 和代数匹配的、语言不受支持的跳过。第 1、2 层和 `WorktreeBatch` 带来的路径是显式的小队列。
- 同一 OID 出现在多个路径上时只抽取一次：在途的键记在一个集合里，第二个路径等第一个的结果。
- 只有最近一次收到 `set_focus` 的项目跑第 3–5 层，其他打开的项目只做第 1、2 层和查询。
- 第 5 层逐个处理非当前、非 prunable 的 worktree：读它的 index，按批做存在检查，缺的经那个 worktree 根的 `SafeRoot` 读出；原始字节的 OID 与 index 的 OID 相同才抽取（有改动的文件等它成为当前 worktree 时再处理）。处理完一个 worktree 就丢掉它的 `TrackedPaths`。某个 worktree 的 index 变化只让这一个 worktree 重新处理；本次会话已经确认在缓存里的（OID、语言），以及当前映射里有匹配提示的，不再做存在检查、不再排进访问标记。这个集合存 8 字节的键（OID 的前 8 字节与语言混合），10 万个约 1.2 MB，计入常驻内存（「性能」）；键冲突只让一次检查推迟到下次会话。淘汰（配额未满时）、语言包变化和换会话时这个集合清空。
- 第 1 层只认 `Done`（`Binary` 也算，上限改不了它）：存在检查命中 `TimedOut` 或 `TooLarge` 时按交互截止时间和 16 MiB 上限重做（超过 1 MiB 的按窗口，见「大文件」；`Windowed` 也算满足），替换那个单元；每个（OID、版本）在命中或写入成功之后本次会话不再检查，所以重做后仍超时的不会反复重做；读不出来的（文件暂时不在）下次打开时再试。第 2–5 层认缓存里的任何状态。第 1、2 层要的键正在批量层抽取时，它们取消那次抽取、自己抽取，不排在批量任务后面（ADR 0029），批量任务暂停时也不等；第 1 层也接管第 2 层在途的键（换成交互上限），第 2 层不接管第 1、2 层的。
- 电源（ADR 0032，见「未决问题」3）：第 1–5 层不论接电源还是用电池都照常运行，本 crate 不读电源状态，没有电池专属的行为；`coverage` 每层只有 `Pending` 和 `Done`。

### 调度、批次与取消

- 第 3–5 层：每个批量线程一个长期任务，循环：取一批 → 存在检查 → 抽取缺的 → 一个写事务写入 → 在批次边界询问 `BackgroundGuard`（暂停时登记 waker、返回 `Pending`）。一批在 64 个文件、读入 2 MiB，或开始 50 ms 之后的第一个文件边界处结束，先到者为准；所以一个大文件可以一直运行到它自己的截止时间。暂停和取消仍在约 50 ms 内生效，靠的是看门狗打断在途的那个文件（见下文），与 ADR 0029「每批约 64 个文件或 50 ms」「取消和暂停在约 50 ms 内生效」的要求一致：单个文件由截止时间约束，50 ms 的响应由看门狗保证。
- 每批开始时重新选层：打开的大文件的窗口链有剩余就先做它，然后是第 3 层，再按「建索引的分层」的顺序（第 4 层、依赖层、第 5 层）。第 1、2 层在另外的工作线程上运行，不排在批量任务后面（ADR 0029）。让路开关打开时让路的层怎样前进见「交互让路」。
- 批量任务的数目：一次唤醒把任务补到批量线程数，一批做完、还留下工作（某层的小队列里有缺的文件）时再补一次。可能产生第 3–5 层工作的每一步之后（打开、关闭、跑第 3–5 层的项目改变、焦点、语言包、核对任务结束，以及批量任务之外排进的工作：第 1、2 层的存在检查命中、要标记访问的单元，第 1、2 层和查询现场抽取的写入）都唤醒一次，所以总是成立：要么没有项目有可做的第 3–5 层工作，要么批量任务数已满。排进访问标记只有一个入口（`Batch::queue_touches`），它返回的 `Wake` 必须交给 `Inner::wake`（`#[must_use]`，clippy 的警告即失败），批量任务之外的写入同样交出一个，所以新增的来源不会漏掉唤醒。测试的 `Harness` 在每次 `settle` 和 `set_focus` 之后检查这一条，漏掉的唤醒让测试失败；`Executor::manual()` 先跑 `Interactive`、`Background` 再跑 `Batch`，批量任务先跑完、第 1 层才命中的次序由 `warm_focus_hit_wakes_the_batch_for_its_touch` 用钩子构造。
- 存在检查：一条 `SELECT oid FROM ci_ext WHERE lang = ? AND grammar = ? AND query = ? AND oid IN (…)`（最多 500 个参数；登记的语句是静态文本，`IN` 里写满 500 个占位符，不够时其余绑定 NULL），命中的按当前有效 OID 和代数写入 `indexed` 提示，缺的抽取。存在检查只读、不标记访问：热启动的耗时就是「已入索引的仓库被认出已入索引」的时间，访问时间的写回不在这条路径上。一次存在检查本身是一批：游标在项目的锁内最多看 4096 个条目，取出至多 500 个提示不匹配的文件（按语言分组查询），缺的进入这一层的小队列，由之后的批次抽取，所以抽取的批次仍按 64 个文件、2 MiB、50 ms 结束。
- 配额：本次会话自己的写入引起淘汰时（写入前后淘汰代数不同），命名空间已经满了。此后游标的依赖和提示的戳记都用冻结的淘汰代数（命名空间满之前的那一代），淘汰不再让第 3–5 层的游标重新开始，焦点变化也不重建游标（见「输入代数与派生状态」），每个文件在一次会话里由批量层最多抽取一次；被挤出的单元等第 1 层打开它、查询现场用到时（第 4 步）或下次打开项目时再抽取。否则游标会重新抽取刚被挤出的单元，写入又挤出别的单元，不会结束。刚写入的单元的提示按写入之后的淘汰代数记下（冻结后按冻结的那一代）。冻结之后提示不再证明单元在：查询按「完整性的核算」的满额模式与存储核对，被挤出的文件搜索或计为未搜。
- 替换：写事务在项目的写锁里再查一次存在（所有写入都取这把锁，查过之后、写入之前不会有别的写入落下），已在缓存里的键只在替换时再写。替换有两种：`Done` 的结果替换 `TimedOut`、`TooLarge` 的单元，不论哪一层写入（第 1 层按交互上限重做的、查询现场抽取的、批量层的）；窗口链完成时写的 `Windowed` 标记替换同一个键（`part = -1`）上 `TimedOut`、`TooLarge` 的整份抽取（ADR 0034，「大文件」），它表示内容在窗口单元里，所以算作有内容。其余没有内容的结果从不替换任何单元（不替换 `Done` 和 `Windowed`，也不替换另一个没有内容的单元），层的高低不让没有内容的单元留下而丢掉 `Done` 的结果（`account_too_large_units_are_searched`）。所以写事务里的淘汰除了这两种替换只来自配额；cache 把替换计为一次淘汰（每个写事务一次）。写事务和它前后读淘汰代数在项目的写锁里进行：替换了单元（两种替换都算）、淘汰代数恰好加一的事务记为本 crate 自己的一次，提示、游标和第 5 层用的淘汰代数减去这些次数（替换事务进行期间读到的是它之前的值，cache 的加一与本 crate 的计数之间没有读者能看到多出的一代），所以打开一个大文件不会让全部提示失效、游标重建、第 5 层重来；批量层也不会把它当成配额已满。访问标记的队列仍按 cache 自己的淘汰代数（`Store::touch_pending` 与它比较）。已知限制：同一个替换事务里同时因配额淘汰的单元也被算成替换，本次会话不因它重新检查，等查询用到时或下次打开项目时再抽取。批量层从队列取文件时，跳过入队之后已由第 1、2 层写入（提示已匹配）的文件。
- 访问标记：存在检查命中的单元 ID 连同查询之前读到的淘汰代数排进项目的 `PendingTouches` 队列。批量线程在热启动的存在检查之后、在批次边界处每次标记 1024 个（`Store::touch_pending`），批次边界照常检查暂停和取消。淘汰代数变了，队列里的 ID 可能已经属于别的单元，整个队列直接丢弃、不标记。项目关闭或切换 worktree 时队列随项目的批量状态一起丢弃，没标记的单元保留原来的访问时间：丢掉的只是淘汰的先后，查询结果不受影响。
- 单文件截止时间：后台 250 ms，交互 1 s。原来按线性吞吐估算 M 系列上 1 MiB 的文件约 230 ms；实测吞吐随文件变大而下降（tree-sitter-tags 的局部检查按「引用数 × 作用域数」增长，见 [ADR 0031](../adr/0031.md)），接近 1 MiB 的文件在 M 系列上估计要 360–400 ms，后台记为 `TimedOut`，打开时按交互截止时间重做（已知限制，`code_intel/extract_1mib` 只记录）。tree-sitter-tags 只接受一个取消标志，所以截止时间由看门狗线程实现：每个在途抽取登记（标志、截止时刻），看门狗每 10 ms 检查一次，到时置位；没有在途抽取时阻塞等待，不唤醒。生命周期见下一条。查询的按需解析由本 crate 自己调用 `Parser::parse_with_options`，进度回调直接比较时刻，不经看门狗。
- 看门狗线程另外持有一张到期表（名字表的延迟重建，见「名字表」）：`wait_timeout` 取在途抽取的下一个检查时刻与最早到期时刻中较早的一个，到期时提交对应的任务；两张表都空时才不带超时地等待。
- 看门狗的生命周期：每次激活最多一个看门狗线程，在这次激活第一次登记在途抽取或到期时刻时经 `PluginHandle::own_with` 和 `run_dedicated_gated` 启动。线程与登记方共用一份状态：互斥锁保护的在途表和 `stop` 标志，加一个 `Condvar`。线程循环：持锁检查 `stop`，为 true 就返回；在途表为空时 `wait`，否则 `wait_timeout(10 ms)` 后检查截止时刻。`stop()` 在锁内把标志置为 true、把在途表里的标志全部置位（正在运行的抽取随之返回 `Cancelled`），然后 `notify_all`，所以无论线程在等待还是在检查，都在一次唤醒之内返回。调用 `stop()` 的地方：
  - 停用：启动时同时把一个 `WatchdogStop` 守卫交给 `PluginHandle::own`，它的 `Drop` 调用 `stop()`。kernel 在停用、失败和 `shutdown` 的撤销第 4 步 drop 守卫，所以每次停用都会让这次激活的看门狗退出，重新激活时另起一个，线程数不随激活次数增长。`Active` 的 `Drop` 也调用一次（重复调用无害）。
  - executor 关闭：executor 不等专用线程，靠闭包自己结束（[executor 设计](executor.md)「接口」的 `shutdown`），所以本 crate 在启动看门狗时用 `Executor::on_shutdown` 登记一次 `stop()`（见「对其他 crate 的要求」），线程退出时注销。`app` 的退出顺序是 kernel 的 `shutdown` 完成后再关闭 executor，这条路径只在 kernel 没有先停用插件时（测试、异常退出）才起作用。
  - 闸门：激活在线程开始运行之前就失效时，`run_dedicated_gated` 不运行闭包，不会启动线程。
- 取消在 50 ms 内生效：项目关闭、切换 worktree 时置位取消标志，批量任务在下一个批次边界（或 tags 的取消检查）退出；正在解析的文件放弃结果，不写入。每个取工作的循环（第 1、2 层的任务、批量层的取文件和第 5 层的收集）在取下一项之前检查会话是否已结束，结束后不再读文件，任务随即结束、交还它的 `BusyGuard`。切换 worktree 或分支后第 3–5 层的游标整体重建，已写入的结果按 OID 留在缓存里。
- 暂停（kernel 的暂停开关）：在约 50 ms 内生效，正在解析的文件放弃这次结果（ADR 0029）。批量任务在文件之间、以及每个在途抽取在登记时（在调用方的线程上同步检查一次截止时间、取消标志和暂停探针，已经过期或已经请求暂停时不开始工作；小文件可能在看门狗第一次检查之前就抽取完）和看门狗的每次检查（10 ms）时读 `BackgroundGuard::pause_requested()`；`ParsedFile` 在解析之前同样先检查一次截止时间和取消标志；为 true 时看门狗置位这次抽取的取消标志，tags 在下一次取消检查时返回 `Cancelled`。这个文件的结果不写入，`indexed` 提示不变，放回这一层游标之前的「恢复后先做」小队列；这一批已经抽取完的文件照常在一个写事务里写入，然后在批次边界停下。恢复后先做小队列里的文件，再从游标处继续。
- 大小上限：后台 1 MiB（`SafeRoot::read` 的 `max`），超过记为 `TooLarge`；文件被打开（第 1 层）时上限 16 MiB（[ADR 0034](../adr/0034.md)，原来 4 MiB）：不超过 1 MiB 的整份抽取，1–16 MiB 的按窗口抽取（「大文件」），更大的不抽取，查询返回 `TooLarge`。查询现场整份读和解析的「交互上限」（`Snap::file_limits`：被查定义所在的文件、有单元的文件、第 1 层的文件）仍是 4 MiB；整份抽取是 `Windowed` 的文件在查询里按它的窗口读和解析，不整份解析，所以不受这个上限（「大文件」的查询一条）。依赖文件按后台上限 1 MiB（ADR 0033）。

### 打开项目与热启动

1. 收到 `ProjectEvent::Opened` 或 `Changed` 后，按快照让对应的输入代数加一（见「增量更新」）。当前 worktree 的文件列表为 `Ready` 时打开工作区根的 `SafeRoot`、订阅 `fs_watch`（第一次时）；基础映射、工作区比较、覆盖部分由核对任务按「输入代数与派生状态」算出。文件夹项目没有订阅和工作区比较。第一次发布快照之后提交依赖的身份任务（读 lockfile、算直接依赖的身份，与 `code_intel.deps` 里的路径表对照，「依赖层」）。
2. 第 1、2 层的路径随焦点到达；第 3–5 层的游标就位，批量任务开始。
3. 热启动只抽取缺的键：映射里每个 OID 先做存在检查，缓存里已有的不读文件、不解析。10 万文件全部命中时约 200 次存在检查，估算几百毫秒，在批量线程上进行（`code_intel/warm_100k`）。命中的单元之后再标记访问，不在这段时间里（`code_intel/warm_100k_touch`，只记录）。
4. 文件夹项目没有 index 的 OID，热启动要读每个文件求 OID，命中的不解析；上限是文件夹遍历的 20 000 个条目。
5. 映射、覆盖部分、`indexed` 提示、名字表都不持久化，只有 `cache` 里的表跨启动保留（调研第 5 节「持久化与重新打开」）。

工作区比较由本 crate 自己做一次：`review` 的比较结果不在 `services` 里，它的基准也可能是会话开始时的提交。这一次比较按 stat 跳过未改的文件、不算 diff、不保留内容；比较自己解析一次 index（不用 `TrackedPaths`），10 万文件实测约 1.0 s（`git/changes_clean_100k`，Linux），期间的临时内存峰值约 66 MB（`git/compare_100k/peak_bytes`），`GitAccess` 每次调用新开 `Worktree`，返回时随句柄释放（见「性能」的临时内存、[ADR 0035](../adr/0035.md)）。

### 增量更新

事件只让输入的代数加一并请求一次核对，重算都由核对任务按「输入代数与派生状态」完成。`WorktreeBatch` 在 fs_watch 线程上交付，`deliver` 只把路径并进项目的重新读集合、让代数加一；`ProjectEvent` 在 executor 线程上到达，处理相同。

| 事件 | 加一的输入代数 |
|---|---|
| `ProjectEvent::Opened`、`Changed` 的 `files` 或 `active` | 文件列表（快照里的 `Arc` 与记下的不同时）；`active` 时同时清空这个项目的全部输入记录，按新的 worktree 重新开始 |
| `ProjectEvent::Changed` 的 `packages` | 子项目边界 |
| `ProjectEvent::Changed` 的 `worktrees`；`WorktreeBatch` 的 `meta.head`、`meta.refs` | HEAD（核对时读 `GitAccess::head`，与记下的不同才加一） |
| `WorktreeBatch` 的 `meta.index` | index（读校验和，不同才加一） |
| `paths` 里的文件路径 | 工作区内容：这些路径进入重新读集合；读的时候是目录的（新目录、文件被目录替换），请求一次工作区比较 |
| `paths` 里的目录路径 | 工作区内容：映射里这个目录下的路径进入重新读集合，并请求一次工作区比较（找出移进来的未跟踪文件） |
| `rescan`、`health` 为 `Degraded` | 工作区内容（请求比较、全部覆盖项重新读）、忽略规则与配置 |
| `meta.rules`、`meta.config` | 忽略规则与配置 |
| `paths` 里第 3 层子项目及其上各级的 `package.json`、`package-lock.json`、`npm-shrinkwrap.json`、`pnpm-lock.yaml`、`yarn.lock`（ADR 0033） | 不改上表的代数；提交依赖的身份任务（「依赖层」）。依赖目录本身不监听 |
| `health` 为 `Failed` | 不改代数；丢掉这份订阅，等下一次 `ProjectEvent` 重新订阅 |
| 语言包的通知 | 语言包（见「语言包的变化」） |
| 读到 `cache` 的淘汰代数变化 | 淘汰 |
| 一批单元写入成功 | 写入 |

- 重新读的路径有了新 OID、而新 OID 不在缓存里时，按路径所在的层排队：在焦点里的按第 1、2 层，其余按第 3、4 层。旧 OID 的行不删除，留给淘汰（其他 worktree 可能还在用）。
- agent 连续写文件时 fs_watch 已按 `Coalesce::WORKTREE` 合并批次，本 crate 不再另加等待。
- 工作区比较的请求只来自事件。比较本身列出的路径进入重新读集合，但不再请求比较：git 把未跟踪的嵌套仓库列为一个目录路径，它读出来是目录，如果再请求比较，核对任务会无限循环。核对任务的一轮只在这一遍有步骤装上或消费了什么、且还有没消费的输入（要打开的根目录、HEAD 和 index 的检查、不一致的派生状态、重新读集合）时再走一遍，最多 16 遍，超过时记日志、留给下一个事件。一遍里每一步的结果都因输入又变了而作废时停下：改变输入的一方（事件、语言包的一轮）会再请求一次核对，所以卡住的输入不会让每个事件付 16 次重建。

### 语言包的变化

语言包可以在项目打开期间安装、更新、回滚或被撤销（ADR 0018）。插件在 `started` 里调用 `GrammarSource::subscribe`，把返回值交给 `PluginHandle::own`。通知只入队；处理在一个 `Background` 任务里进行（`BusyGuard`，同时最多一个），运行期间到达的通知并进下一轮。每一轮重新读 `GrammarSource` 的当前状态（`installed` 列出的全部语言包，加上语言表里已有的），与上一轮记下的「语言 → grammar 版本」比较，按比较结果处理，所以漏掉或合并的通知不影响结果；`generation` 不大于已处理代数的通知直接丢弃。

每一轮先更新语言表，再让语言包代数加一；基础映射、覆盖部分、`indexed` 提示、游标和名字表随之按「输入代数与派生状态」重算。语言表的更新：

| 变化 | 语言表 |
|---|---|
| 安装 | 登记这种语言和它认领的扩展名，第一次用到时编译查询。基础映射重建后这些扩展名的文件进入映射，未跟踪的经下一次工作区比较进入覆盖部分 |
| 更新、回滚 | 换成新的 grammar 版本。键里的 grammar 版本随之变化，旧版本的单元不再命中（回滚时，之前用那个版本写过的单元重新命中） |
| 撤销、卸载 | 立即移除，不再加载这个 grammar。这种语言的文件随基础映射重建离开映射和 `coverage`，这种语言的查询返回 `UnsupportedLanguage` |

- 租约：语言表里的每个条目、由它编译出的 `TagsConfiguration` 和 locals `Query`、每个 `Parser`、每个在途抽取和 `ParsedFile`（持有 `Tree`）都持有一份 `GrammarLease` 的克隆。更新、回滚、撤销时语言表先换掉或删掉条目，旧条目的租约随最后一个使用者（在途抽取结束、查询释放 `ParsedFile`）drop，加载器这时才卸载动态库，所以不会有代码调用已卸载的库。同一个值里，租约在所有指向库的字段之后 drop：Rust 按声明顺序 drop 字段，语言表条目和 `ParsedFile` 把租约（或持有租约的 `Lang`）声明在最后；每线程复用的 `TagsContext`（持有指向库的解析器和查询游标）在语言包的每次抽取之后清掉。`lang_lease_outlives_grammar_backed_values` 检查 drop 的顺序：语法树、编译好的查询、模块。
- 在途的抽取：更新、回滚、撤销都置位这种语言在途抽取的取消标志。每个批次和每次按需抽取开始时记下所用的 grammar 版本，写入缓存之前核对仍是当前版本，不是就丢弃结果，所以旧 grammar 的结果不会以新键写入。已编译的旧配置在用它的在途抽取结束后释放。
- 旧版本的单元不主动删除，留给淘汰。
- 已交出的 `QueryResults` 不修改（只追加的规则）；正在进行的查询按自己开始时的快照完成。

### 跳转到定义

查询开始时取一份 `LiveMap` 快照（基础映射、覆盖部分、`HeadDiff` 的 `Arc`）和当前的语言表，全程使用：同一次查询的各批结果对应同一份映射，之后映射换代也不改写已交出的结果，`Location::oid` 让视图在打开前发现内容已变。`References` 相同。

`query(Definition)` 在一个 `Interactive` 任务里按下面的顺序回答。前一步得到有绑定依据的候选就停下；没有时继续，结果逐批追加：

1. **当前文本**：按 `TextRef` 读出内容。工作区一侧经 `SafeRoot` 读，只有能证明读到的字节就是 `TextRef::oid` 那份内容时才回答：原始字节的 OID 等于它，或 `GitAccess::canonical_oid` 把这些字节转成规范内容后的 OID 等于它；两者都不等（或有外部过滤器）时返回 `Stale`，不因为映射里还记着这个 OID 就接受。对象库一侧经 `GitAccess::blob` 读，按 OID 取出的内容不需要核对。然后解析，求出位置上的标识符和 `RefSyntax`。这份内容的解析已保留时（「当前文本的解析」）不读、不解析，位置直接在保留的解析上查。按「绑定规则」在引用处确定可见的局部定义是 `Bound(Local)`；不确定可见的局部定义（`Site::possible`）和本文件里其余同名定义是第 1 层的 `NameOnly` 候选，前者在前。光标在定义的名字上时，`symbol.defines` 是这个定义（`local` 按上面 `DefinitionRef::local` 的规则：locals 在非根作用域里给出时为 true），它本身作为唯一的 `Bound(Local)` 候选。1–16 MiB 的当前文本只读、只解析位置所在的窗口（「大文件」）；当前文本是依赖文件时经它的 `DepRoot` 读（「依赖层」）。
2. **import**：解析扩展点给出这个名字的 import 目标，按路径取目标文件的抽取结果（缓存命中或现场抽取），其中导出的同名定义是 `Bound(Import)`；扩展点的 `bind` 给出的结果按它的 `Binding`。#17 的空实现在这一步没有结果。目标是依赖里的文件（裸模块名）时经 `ResolveContext::dependency` 按需读取和抽取（「依赖层」）。
3. **索引**：`ci_def` 按名字取出定义，经 `ci_ext` 得到 OID 和语言，只取语言、grammar 版本、查询版本与当前语言表一致的单元（见「存储表」的版本过滤），按「候选的分层与过滤」过滤和分层，只留映射里有的 OID。按页取（[ADR 0037](../adr/0037.md)）：第一批只有第一页（索引里名字的 256 行，见「存储表」）；名字还有更多页（很多文件定义的名字）时，第一批另加离跳转最近的文件里的定义，按 OID 查找（见「存储表」）。最近的文件按分层的顺序取，分两段、各自用语句查，一条语句的行数上限不会让近的文件让位给远的：先是所在目录自己的文件（不含子目录，含覆盖部分里工作区的改动，审阅集合在前）；再是所在子项目的其他文件（没有子项目时是所在目录的子树，在根目录时没有），审阅集合在前、按路径距离由近到远。合计至多 2048 个（`goto_popular_name_nearest_in_a_deep_subproject`）。所以最近的候选总在第一批里，不论这个名字有多少定义。其余页在第一批之后、限时搜索之前逐页追加，每页一条语句，页与页之间检查查询是否已停止、时间盒是否已到；时间盒到时没读的定义行数计入 `Partial::unsearched`（文件数的上限）。依赖里的定义在 `code_intel.deps` 里按同样的分页查，排在项目的各层之后，不计入 `unsearched`（「依赖层」的查询一条）。
4. **限时搜索**：第 3 步之后仍没有 `Bound` 候选、且当前子项目里还有未入索引的文件时进行，见「限时搜索」。

第 1、2 步和第 3 步的第一批（见上）作为第一批交出（目标：索引命中时 ≤ 50 ms，本 crate 的份额 ≤ 20 ms，见「性能」），第 3 步的其余页和限时搜索的结果逐批追加。结果只追加：后到的候选排在已有候选之后，已有条目不重排、下标不变（调研第 5 节「索引不完整时」的呈现规则）；每批新到的候选在批内按分层排序。第一批不等核算：遍历和满额模式的存储核对都在它之后。

**当前文本的解析**（[ADR 0037](../adr/0037.md)）：第 1 步的整份解析（`ParsedFile`：任意位置的标识符、定义和绑定模型）按（blob OID、语言版本）保留在内存里，同一文本里的下一次跳转不读文件、不解析，答案与为光标处现场解析的相同（`goto_kept_parse_answers_as_the_cold_one`）。

- 上限：最多 2 份；源码 ≤ 128 KiB 的才保留；按每字节源码 48 字节估算合计 ≤ 12 MiB（实测 46.6 字节，`code_intel/parse_cache_bytes`），超过时去掉最久未用的，当前文本的那份最后去掉。当前文本（最近一次 `Focus::Current`）的那份在它是当前文本期间不过期，读多久之后跳转都不走冷路径；它不再是当前文本时（另一份成为当前文本、`Focus::Current(None)`、它的项目关闭）从那时起计 60 s，其他的从最后一次用到起计 60 s，没用到即释放（看门狗的到期项，`goto_current_text_parse_stays_while_current`）；语言表替换了它的语言时立即释放，不留住旧 grammar 的租约，换表之后才完成的解析不保留（`goto_kept_parses_are_bounded`、`grammar_lease_released_by_an_expired_prefetch`）。
- 预热：文本成为当前文本（`Focus::Current`）时，以及冷路径的跳转、预取之后，在 `Background` 任务里读出、整份解析并保留，排在首屏的 `Interactive` 工作之后；冷路径交给它已经读出并核对过的字节，不再读一次。跳转不等预热：没有保留的解析时按光标处解析（`parse_at`，冷路径），96 KiB 以内按 32 ms 判定，96–128 KiB 只记录（见「性能」）。预热进行中到来的跳转也不等它、自己再解析一遍：跳转在 `Interactive` 任务里，预热在 `Background` 任务里，让前者等后者要把跳转改成预热完成后的续接（单线程的手动 executor 下直接阻塞等待会死锁），复杂度不值得，窗口只有预热的那几十毫秒（已知限制，[ADR 0037](../adr/0037.md)）。
- 键是内容：同一个 OID 在对象库一侧（Diff 的旧一侧）和工作区一侧共用一份；文本变了 OID 就变了，旧的那份只等释放。

去重：每次查询持有一个已交出集合，键是（来源、路径、OID、行、列），来源即 `Location::origin`。每一步的候选在加入结果之前先查这个集合，已经由前面的步骤交出的跳过，保留先交出的那一条（它的层更靠前）。例如当前文本的 blob 已经入索引时，第 3 步会再次查到第 1 步列出的本文件定义；第 2 步现场抽取过的文件，在第 3 步和限时搜索里也会再次出现。同一份内容出现在两个路径上时是两个候选。`semantic` 的结果与候选按位置集合比较，不进入这个集合。集合只存在于这次查询的任务里，查询关闭时释放。

`jump` 和 `preselect`：

- 有 `Bound` 候选、且恰好一个时，`jump` 是它；多个 `Bound` 候选（重载签名、同名的多个 `impl`）时不跳，`preselect` 为第一个。第 1 步有不确定的局部候选（`Site::possible`）时也不跳：这个名字可能就是那个局部声明（「绑定规则」的安全不变式）。
- 只有 `NameOnly` 候选时，即使只有一个也不跳，`preselect` 为第一个。方法调用、接收者类型不明的成员访问都属于这一类。
- Java 的候选满足「语义导航的注册」里的升级条件时，同时发出语义请求，`semantic` 为 `Pending`。

`query(Named)` 只做第 3 步（不分 `CurrentFile`、`Imported` 两层，`near` 给出的路径决定其余各层），全部是 `NameOnly`，不跳转。它的第一批同样是第一页加上离 `near` 最近的文件里的定义，在核算的遍历之前交出（满额模式下遍历要核对存储，直到时间盒）；其余页逐页追加，遍历之后交出 `Done`。

### 候选的分层与过滤

候选按 `CandidateTier` 分层（调研第 6 节）：当前文件 → import 指向的文件 → 同包（扩展点的 `package_of`，默认同一目录）→ 同一子项目 → 当前子项目依赖的子项目（扩展点的 `dependencies`，#17 为空）→ 其余 → 依赖（ADR 0033，依赖映射里的文件；同层按包名、包内路径排序）。每一层都要所比较的一方已知：没有当前文本（`Named`）时没有候选是「当前文件」，`near` 只决定其余各层和距离；项目有子项目时，两个都不在任何子项目里的文件不算同一子项目（没有子项目时整个 worktree 是一个）（`goto_tiers_need_what_they_compare_with`）。同层内依次按：在 `Focus::Review` 里的优先、路径距离（去掉公共前缀后两边剩余的段数之和）由近到远、路径字节序、行号。

过滤：

- 语言：只留与当前文本的语言在同一兼容组里的候选。
- 可见性：`File` 只留同一文件；`Package` 只留同一目录；`Module`（Rust）只留同一文件，以及以它为父模块的文件（`a.rs` 对应 `a/` 之下；`mod.rs`、`lib.rs`、`main.rs` 对应所在目录之下）；`Public`、`Unknown` 不过滤。
- 语法种类：首发语言的表由「定义形式」推出（`forms::kinds_for`）：一个定义种类被它那一行列出的引用语法接受。`Plain` 接受全部种类；Rust 以外的 `Member` 也接受全部种类（左边可以是命名空间导入、模块、包或类，`utils.handle`、`models.User`、`pkg.Var`、`Outer.Inner` 的成员可以是任何定义）。推出的结果：

  | 语言 | `Call` | `MemberCall` | `Construct` | `Type` | `Member` |
  |---|---|---|---|---|---|
  | JS | `Function`、`Method`、`Constant`、`Field`、`Variable` | 同 `Call` | `Function`、`Class`、`Constant`、`Variable` | `Class` | 全部 |
  | TS / TSX | 同 JS | 同 `Call` | 同 JS | `Class`、`Interface`、`Enum`、`Type`、`Module` | 全部 |
  | Rust | `Function`、`Method`、`Struct`、`Macro`、`Constant` | `Method` | `Struct`、`Type`、`Constant` | `Struct`、`Enum`、`Trait`、`Type`、`Constant` | `Method`、`Field` |
  | Java | `Method`、`Field` | `Method`、`Field` | `Class`、`Interface` | `Class`、`Interface`、`Enum` | 全部 |
  | Python | `Function`、`Method`、`Class`、`Constant`、`Field` | 同 `Call` | 无 | 无 | 全部 |
  | Go | `Function`、`Type`、`Variable` | `Function`、`Method`、`Type`、`Field`、`Variable` | 无 | `Type` | 全部 |

  原因按行：JS / TS 里任何值都可以放函数或类（`const handler = () => …`、类字段 `onClick = () => …`）；Rust 的元组变体被调用（`Io(e)`），记录变体被构造和匹配（`Message::Move { x, y }`，构造是 `Construct`，匹配是 `Type`），`const` 和 `static` 可以放函数指针；Java 记录组件的访问器以调用语法出现，`new Runnable() { … }` 构造接口的匿名类；Python 的类、模块常量和类属性都可以被调用；Go 的类型转换是调用语法，函数类型的变量和字段可以被调用。语言包没有定义形式，按通用规则：`Call` / `MemberCall` 接受 `Function`、`Method`、`Macro`、`Class`、`Struct`，`Construct` 接受 `Class`、`Struct`，`Type` 接受 `Class`、`Interface`、`Struct`、`Enum`、`Trait`、`Type`、`Module`，其余全部。`site_kind_table` 逐行检查上表。表从定义形式推出，只看声明，所以另有一组用法样例（`tests/site.rs` 的 `USES`）：每种语言每个被收窄的引用语法，对它能指向的每种定义各有一个常见写法，`site_kind_masks_accept_uses` 检查这个位置的语法确是这一种、表接受这种定义。

  这张表是 `site::accepted_kinds`。引用位置的 `RefSyntax` 由每种语言的一张位置表判断（`site.rs`）：每行是（标识符的节点种类、父节点种类与它所在的字段、可选的外层节点种类与字段或父节点必须有的字段）→ `RefSyntax`，第一条匹配的行生效；没有匹配的类型标识符为 `Type`，其余为 `Plain`。例如 Rust 的 `call_expression.function` 里的 `identifier` 是 `Call`，`call_expression.function` 里的 `field_expression.field` 是 `MemberCall`，`struct_expression.name` 里的类型名和路径的最后一段（`Message::Move { .. }`）是 `Construct`；Java 的 `method_invocation.name` 在有 `object` 字段时是 `MemberCall`。语言包没有位置表，都是 `Plain`。

- 扩展点的 `disambiguate` 最后运行（Java 按所在类和参数个数，#18）。

### 查找引用与调用方

`query(References { of })` 在 `Interactive` 任务里进行：

1. 候选文件：被查定义所在的文件（按 `of.location.origin` 读：`Object` 时读对象库里的那个 blob，这份内容里的引用的 `origin` 也是 `Object`），加上 `ci_ref` 里有这个名字的单元经映射得到的路径（语言兼容组过滤）。顺序：在 `Focus::Review` 里的、同一子项目、依赖的子项目、其余，同层按路径距离。被查定义是局部定义（`DefinitionRef::local`，不论给出它的是跳转、`Named` 还是别的调用方，都按被查定义所在文件的解析确定：这个文件最先解析，同一次解析既给出局部性、也给出这个文件里的引用，不另外解析；解析失败时沿用给出的值）时，候选文件只有它所在的文件，只扫描它的作用域，不查 `ci_ref`、不做限时搜索，结果没有 `same_name`（locals 能完整解析同一作用域里的引用）。
   `ci_ref` 按页读（每页读索引的 ≤ 256 行，页与页之间检查查询是否已停止、时间盒是否已到），全部读完再按上面的顺序排列，所以很多文件用到的名字也不会让关闭的查询多等一条以上的语句。时间盒在读页时就到了，没读的单元按索引数出、计入 `unsearched`（`Store::refs_left`，上限）。
   这些文件在第 2 步里同样受时间盒约束（[ADR 0037](../adr/0037.md)）：每个文件的解析截止时间截到时间盒，逐个文件之前检查时间盒，到时剩下的文件不再读，记为 `NotReached`、计入 `unsearched`，结果是 `Partial`（`refs_indexed_files_stop_at_the_time_box`）。原来它们各按交互的 1 s 解析、不看时间盒，几千个文件用到的名字要 5–15 s。
2. 逐个候选文件读出内容、解析，读的上限和解析的截止时间按同一条规则（`Snap::file_limits`，见「限时搜索」）：被查定义所在的文件、有单元的文件（`ci_ref` 给出的、提示匹配的，含 `TooLarge`、`TimedOut` 的单元）和第 1 层的文件按交互上限（4 MiB、1 s），其余按后台上限（1 MiB、250 ms），截到查询的时间盒；所以 1.5 MiB 的 `TooLarge` 文件和第 1 层抽取过的 2 MiB 文件不会读进来之后在 250 ms 时超时（`account_files_are_parsed_under_their_units_limit`）。用 `ParsedFile` 的引用位置（tags 引用与 `@local.reference` 的并集）找出这个名字的每一处引用，逐处解析绑定：
   - locals 解析到被查定义（同一文件、同一位置）：调用方。
   - locals 解析到别的局部定义：丢弃，这处引用与被查定义无关。
   - 扩展点的 `bind` 给出被查定义：调用方；给出别的定义：丢弃。
   - 其余：同名引用（`same_name`），不计入调用方，也不用于判断 agent 是否漏改了调用方（调研第 5 节）。
3. 当前子项目里未入索引的文件走限时搜索，命中的文件按第 2 步处理。查引用的时间盒是 1.9 s（`REFERENCES_FOR`，ADR 0030 全部结果 ≤ 2 s，同样留 0.1 s 画出最后一批），限时搜索和满额模式的核对都在这里停止（[ADR 0037](../adr/0037.md)）。
4. 结果逐批追加：第一批在处理完前几个候选文件或 150 ms 时交出（目标 ≤ 200 ms），之后每 100 ms 或每 32 个文件一批；10 万文件的仓库全部结果目标 ≤ 2 s。计时不只在解析完一个文件之后检查：限时搜索每读一个文件（命中或没命中）检查一次，所以命中之后一长串没有命中的文件不会把它压到查询结束；`ci_ref` 一轮之后、读搜索范围（满额模式下要核对存储）之前，已找到的立即交出；限时搜索之后、核算的遍历之前再交出一次（`refs_indexed_references_go_out_before_the_search`、`refs_search_batches_go_out_on_time`）。
5. 去重：与「跳转到定义」相同，每次查询一个已交出集合，键是（来源、路径、OID、行、列），`callers` 和 `same_name` 共用，一处引用只进其中一组、只出现一次。另记已处理的（路径、OID）：被查定义所在的文件、第 1 步的候选文件、限时搜索命中的文件有重叠时，每个文件只解析一次。

#17 的空扩展点下，跨文件的引用只有 locals 能证明的那部分是调用方，其余都在同名引用里；#18、#19 的 `bind` 让 Java、TS 的调用方变准。Java 的 `References` 按升级条件同时请求语义导航。

### 限时搜索

- 范围：当前子项目（按查询快照里的子项目边界）里语言在兼容组内、`indexed` 提示与当前有效 OID 和代数不匹配（见「输入代数与派生状态」）、也不在在途抽取里的文件，按与当前文件的路径距离排序，同一目录的在前。命名空间满了之后，范围里提示匹配的文件先按批与存储核对（见「完整性的核算」），单元已不在的也在范围里；单元是 `TooLarge`、`TimedOut` 的提示匹配文件（单元里没有它的定义）也在范围里，按第 1 层的交互上限读和抽取，截到查询的时间盒。在途抽取里的文件不重复抽取，但它们对这次查询不可见（索引里还没有，搜索也不扫描），所以计入 `Partial` 的 `unsearched`：有这样的文件时结果不会是 `Complete`。范围只是搜索读哪些文件；子项目之外没入索引的文件不搜，但照样计入完整性（`outside`），见「完整性的核算」。
- 每个文件经 `SafeRoot` 读出，上限是它的单元抽取时的上限（`Snap::file_limits`）：有单元（提示匹配，或满额模式下被挤出的）或在第 1 层（当前文本、抽屉）的文件 4 MiB，其余 1 MiB；超过上限、二进制、读不出的文件计入 `unsearched`，不当作搜过。查引用读 `ci_ref` 给出的已入索引文件时同样按 4 MiB 读。抽取和解析的截止时间按同一条规则（同一个 `FileLimits`）：读的上限是 4 MiB 的文件用交互截止时间（1 s），其余用后台的 250 ms，不让一个文件按一层的上限读、按另一层的截止时间解析。用 `memchr::memmem` 找这个名字的字节串，命中处前后不是标识符字符（ASCII 字母数字、`_`、`$` 和 UTF-8 的多字节序列）才算整词命中。没有命中的文件不解析。
- 命中的文件先做一次存在检查，缓存里有就直接用；没有就按后台截止时间抽取，结果用于这次查询，并交给一个 `Background` 任务写入缓存，写入成功后写入 `indexed` 提示。
- 从查询开始计 1 s 后停止：文件之间检查时刻，抽取的截止时间取「250 ms」与「剩余时间」中较小的一个。停止后交出最后一批，`state` 由「完整性的核算」决定：没搜到的文件是 `Done(Partial { … })`，每个能参与回答的文件都已入索引或搜过才是 `Done(Complete)`。
- 截止时间被剩余时间截短、抽取因此 `TimedOut` 时，是查询的时间用完了，不是这个文件超时：结果不写入缓存，这个文件计入 `unsearched`，之后由批量层按自己的截止时间抽取。`Cancelled` 同样不写入、计入 `unsearched`。查询读文件的每条路径（跳转第 2 步、限时搜索、查引用的解析）都用同一个截止时间和同一条规则。
- 结果每 50 ms 或每个命中文件交出一批。
- 搜索只读、不建索引的文件不改变 `indexed` 提示，第 3 层的批量任务之后照常处理它们。

### 完整性的核算

`Completeness` 只在一处决定（`query::account`）：查询结束时把能参与回答的每个文件分类一次，`Complete` 当且仅当没有一个是「未搜」。能参与回答的文件是快照映射里语言在范围内的全部文件，不分层（当前文件、同一目录、同一子项目、依赖的子项目、其余）：跳转和查引用是语言在当前语言兼容组内的，`Named` 是全部启用的语言。

| 分类 | 条件 |
|---|---|
| 已入索引 | 没有被这次查询读过；提示匹配，单元不是 `TooLarge`、`TimedOut`（`Binary` 算：二进制内容没有定义和引用，上限改不了它），满额模式下还经存储核对单元仍在，快照不缺工作区内容 |
| 搜过 | 这次查询读到快照的内容并看过（当前文本、第 2 步的目标、`ci_ref` 的文件、搜索命中或没命中的文件） |
| 未搜（原因） | 搜索范围内没轮到（`NotReached`）、截短（`Cut`）、取消、自身超时或单元 `TimedOut`（`TimedOut`）、内容已变（`Stale`）、超过上限或单元 `TooLarge`（`TooLarge`）、读不出、覆盖部分读不出的工作区文件（`Unindexable`）、在途、范围之外（`Outside`）、被挤出（`Evicted`）、满额模式下没来得及核对（`Unchecked`）、快照缺工作区内容（`Unverified`） |

- 计数：未搜的文件在搜索范围之内的计入 `Partial::unsearched`，之外的计入 `outside`；查询读过、没能搜的文件不论在哪里都计入 `unsearched`。范围之外没入索引的文件因此让跳转、查引用、`Named` 都是 `Partial { outside }`，不再因为只搜了当前子项目就报 `Complete`（`account_outside_the_scope_is_partial`）。
- 没有内容的单元：批量层存成 `TooLarge`（超过 1 MiB、没打开过的生成文件）或 `TimedOut` 的文件也有匹配的提示，但单元里没有这个文件的定义和引用。本次会话记下这些键（`State::unfinished`，置位提示时按单元的状态更新；记录的变化也算一次提示置位），快照带上它的 `Arc`。核算把它们计为未搜；搜索范围包括它们，按交互上限（4 MiB、1 s，截到查询的时间盒）现场抽取，所以跳转和查引用能找到只在这样的文件里的定义和调用方，`Named` 不搜，报告它们（`account_too_large_units_are_searched`、`account_timed_out_units_are_not_indexed`、`account_binary_units_count_as_indexed`）。
- 覆盖部分里的工作区文件：修改过或未跟踪、超过 1 MiB 或读不出的文件是 `Unindexable`，内容不知道，计为未搜；过滤器的指针文件（LFS）、符号链接或路径经过符号链接的、其他种类的文件是 `Excluded`，不是本 crate 索引的源文件，不参与核算（`account_unindexable_worktree_files_are_unsearched`）。
- 有绑定依据就结束的跳转（第 1、2 步得到 `Bound`）是 `Complete`：依据本身证明了答案，其余文件改变不了它。局部定义的查引用只看它所在的文件：这个文件搜过才是 `Complete`，否则 `Partial { unsearched: 1 }`；被查定义所在的文件按第 1 层的交互上限（1 s、4 MiB）解析（`account_local_references_need_their_file_searched`）。按 OID 读的被查定义所在的文件（对象库里的、工作区里已经变了的）没能搜时也计入 `unsearched`，即使映射在这个路径上是另一份内容（`account_definition_read_as_an_object_counts_when_unsearched`）。
- 一次遍历（`Snap::walk`）读全部提示，并定下此时已读过的文件在遍历里的分类，都在查询最后一次比较视图之前；`Snap::finish` 不读提示：遍历定下分类的文件换成读到的结果，遍历之后才读的文件（比较发现只多了提示、再查一次索引时读的）只在没能搜时计入（保守）。已置位的提示都被那次查索引覆盖。遍历不建 `Effective`、不取路径，10 万个文件约 1.2 ms。
- 遍历的结果按项目缓存（`Project::walks`，最多 2 个）：键是发布对象、提示戳记、提示置位次数、搜索范围的目录和语言组，任何一个变了就不用；满额模式不缓存。同一个索引上重复的跳转因此不再遍历（`account_walks_are_cached_until_the_index_changes`）。
- 满额模式：命名空间满了之后（`Batch::full`）提示的戳记冻结在满之前那一代，被配额挤出的单元的提示仍然匹配，提示不再证明单元在。查询把计为「已入索引」之前的提示匹配文件按批与存储核对（与批量层同一条 `IN (…)` 语句，每条 500 个，按版本分组）：范围里的在搜索之前核对（10 万文件、子项目 1000 个文件约 2.3 ms），不在的进入搜索；范围之外的在遍历时核对。范围内外的每条语句之前都检查查询是否已停止、是否到了查询的时间盒（与搜索同一个：跳转和 `Named` 是查询开始后 1 s，查引用是 1.9 s，`REFERENCES_FOR`，在 ADR 0030 的 2 s 之内；查引用的 `ci_ref` 一轮在 10 万文件时就要 0.8–1 s，与跳转共用 1 s 时核对总被截断，`account_references_full_check_has_its_own_time_box`），停止时立即返回；到时没核对的计为 `Unchecked`：范围里的计入 `unsearched`，范围之外的计入 `outside`，都不算已入索引。所以根作用域（没有子项目）、`Named` 不带 `near` 时的整个 worktree 也不会让查询过了取消还在核对（[ADR 0037](../adr/0037.md)，`account_full_namespace_checks_the_hinted_files`、`account_full_check_stops_with_the_query`、`account_full_check_is_time_boxed`、`account_full_scope_check_stops_with_the_query`）。核对不推迟第一批：跳转在核对之前交出第 1–3 步，`Named` 在遍历之前交出候选，查引用在读搜索范围之前交出 `ci_ref` 一轮的结果（`account_named_candidates_go_out_before_the_walk`）。冻结之后再写入同一个单元不会让提示由不匹配变为匹配，所以满额模式下每次置位都让提示置位次数加一，查询据此再查一次索引。没满时不核对。

### 依赖层

[ADR 0033](../adr/0033.md)「依赖源码」。状态：已设计，尚未实现（「验收」第 9 步）。依赖目录不进文件列表、不整体索引、不监听；跳转进依赖时按需解析，空闲时只为当前子项目的直接依赖建声明闭包的索引。现在只有 npm（TS / JS，npm、pnpm、yarn 安装的 `node_modules` 都读，包括 pnpm 工作区）；Cargo、Go、Python 的依赖根和闭包规则在它们的解析扩展点实现时经同一个内部接口（`deps::Ecosystem`）接入，键、身份、配额、读取限制和分层不变。

**发现**（每个项目的每个会话一个 `deps::Roots`：按当前 worktree 建立，打开项目时为空，用到时建立；切换当前 worktree、项目关闭时随会话释放，所以一个 worktree 的根不会给另一个 worktree 读）：

- 子项目：第 3 层的子项目（「建索引的分层」），它的根目录下的 `package.json`；没有子项目时取 worktree 根的。第 3 层换了子项目时按新的子项目重新发现，原来的包留在依赖映射里（见下）。清单经 worktree 根的 `SafeRoot` 读，上限 1 MiB。
- 直接依赖：`dependencies`、`devDependencies`、`peerDependencies`、`optionalDependencies` 的键，去重，按清单里的顺序最多 512 个，多出的记日志、只走按需解析。名字按 npm 的规则校验（可选的 `@scope/` 加一段，每段非空、不是 `.` 或 `..`、没有 `/`、`\` 和 NUL，合计 ≤ 214 字节），不合规的跳过。
- 包目录：按 Node 的查找顺序，从子项目目录往上到 worktree 根，每一级的 `node_modules` 经 worktree 根的 `SafeRoot::open_dir` 打开（路径的任何一级是符号链接都失败，这一级跳过），在其中找 `<名字>`，第一个找到的就是这个包。这条链上的 `node_modules` 目录就是这个子项目的依赖根；一个 worktree 最多打开 8 个根，按（worktree、相对 worktree 根的目录）登记，同时记下打开时目录的设备号和 inode。
- 根的有效性：`npm ci` 重建、删掉重装 `node_modules` 之后，打开的描述符指向已删除的目录。每次身份任务运行时，按登记的路径经 worktree 根的 `SafeRoot` 对这个目录 `stat` 一次，设备号或 inode 变了（或已不存在）就重新经 `open_dir` 打开（或删去这个根）；任何依赖读取得到 `NotFound` 时同样重新验证一次、重试一次。所以根不会在一次会话里一直读不到。
- `<名字>` 是目录时就是包目录；是符号链接（pnpm）时读出目标，按词法相对链接所在的目录拼出路径（相对 worktree 根的路径），结果落在这个 worktree 已发现的某个依赖根之内才继续（[ADR 0040](../adr/0040.md)：pnpm 工作区里 `packages/app/node_modules/react` 指向根目录 `node_modules/.pnpm/…`，那是同一个子项目查找链上的另一个根），最多跟随 4 次，最后一级必须是从那个根出发、各级都不是符号链接的目录，之后经那个根的 `SafeRoot` 读；否则这个包读不到。所以 `node_modules/react` → `.pnpm/react@19.0.0/node_modules/react` 和工作区包指向根目录 `.pnpm` 的链接都可以读，指向已发现的根之外（`$HOME`、worktree 之外、没有作为根打开的目录）的链接不读；读取时从不跟随符号链接。pnpm 的 `virtual-store-dir` 配到 `node_modules` 之外时，链接的目标不在任何已发现的根里，读不到（已知限制，ADR 0040）。
- 入口（声明文件）：包的 `package.json`（≤ 1 MiB）里 `exports` 的 `"."`（`exports` 是字符串或条件对象时就是它）按对象顺序深度优先找 `types` 条件，`import`、`require`、`node`、`default` 之下也找；没有时依次是 `types`、`typings`、`index.d.ts`、`main` 换成 `.d.ts`；都没有时找 `@types/<名字>`（`@scope/x` 对应 `@types/scope__x`），按同样的规则作为一个单独的包。路径按词法相对包目录解析，离开包目录、扩展名不在允许的列表里的丢弃。
- 声明闭包：从入口出发，沿相对路径的 `import`、`export … from`、`import("…")` 类型和 `/// <reference path="…">` 走（裸模块名和 `/// <reference types>` 只记录，不跟随）。一个相对路径 `s` 依次试 `s`（扩展名允许时）、`s` 的 `.js` / `.mjs` / `.cjs` 换成 `.d.ts` / `.d.mts` / `.d.cts`、`s.d.ts`、`s.d.mts`、`s.d.cts`、`s.ts`、`s.tsx`、`s/index.d.ts`、`s/index.ts`，第一个存在的就是目标；离开包目录的丢弃。边由 TS / JS 的一次遍历在抽取时一并给出（`walk::ts` 收集这几种语句的模块路径），按内容存：依赖单元写入时连同它的模块路径一起写进 `cd_spec`（每个单元每个模块路径一行，原样的字符串，与路径无关），所以单元已经在缓存里（存在检查命中：版本升级而内容没变的文件、跳转时按需写过的入口）时，闭包从 `cd_spec` 读出边，不重新解析，也不会在入口处停下。按需解析和第 1 层写进 `code_intel.deps` 的单元同样带 `cd_spec`。没有内容的单元（`TooLarge`、`TimedOut`）没有边。
- 只读允许的扩展名（ADR 0033）：`.d.ts`、`.d.mts`、`.d.cts`、`.ts`、`.tsx` 和 `package.json`；其他文件不打开。lockfile 是工作区文件，经 worktree 根的 `SafeRoot` 读，不属于依赖根。
- 限额（ADR 0033）：每个包的闭包最多 4000 个文件、16 MiB；单个文件最多 1 MiB（超过的记 `TooLarge`，不跟随它的边）；每个项目在一次会话里，依赖层实际读入的源码合计最多 64 MiB，到了就停下（`coverage.deps` 为 `Done`），其余只走按需解析。身份没变、没有读文件的包不计入。

**身份与包的路径表**：

- 身份（决定要不要重读一个包）：npm 的 lockfile（子项目往上到 worktree 根，第一个 `npm-shrinkwrap.json` 或 `package-lock.json`，经 worktree 根的 `SafeRoot::open_file` 打开，上限 64 MiB）里 `packages["<包目录相对 lockfile 所在目录的路径>"]` 的 `integrity`。lockfile 用 serde_json 的流式访问器读（`Deserializer::from_reader`，读者按 1 MiB 的区间调用 `OpenFile::read_ranges`，读完核对身份）：只留下要找的那些键的 `integrity` 和 `version`，其余按 `IgnoredAny` 跳过，所以内存是一个 1 MiB 的区间加结果，与 lockfile 的大小无关（整个读进内存时不成立，所以必须按区间读）。
  - 已安装的状态：lockfile 记的是应当安装的内容，不是已经安装的：`git pull` 或切换分支之后、`npm install` 之前，lockfile 已经是新的，`node_modules` 还是旧的。所以身份另含包目录里已安装的 `package.json` 的五项 stat（设备号、inode、大小、修改时间、状态改变时间）：只有 `integrity` 和这五项都与路径表记下的相同（并且记录时不在 racy 窗口里）才算身份没变，任何一项不同都走下面的 stat 表。npm 解包时重写每个文件（新的 inode 和状态改变时间），所以换了版本的安装总会改变这五项。没有用 npm 自己写的 `node_modules/.package-lock.json`（隐藏的 lockfile，记的是已安装的树）：它在被忽略的目录里，要多一个允许读的文件，npm 在它比 `node_modules` 里的目录旧时也不用它；stat 已经足够发现安装状态的变化。
  - 后备：`pnpm-lock.yaml` 和 `yarn.lock` 不解析（要 YAML 和 yarn 格式的解析器，ADR 0033 不新增依赖），身份取 ADR 0033 的后备：包清单（`package.json`）的 OID 加上解析链接后的包目录路径（pnpm 的 `.pnpm/<名字>@<版本>_<peer 哈希>` 目录名含版本和 peer 的哈希，安装另一个版本时路径就变），同样另含已安装的 `package.json` 的五项 stat。没有 lockfile、lockfile 里没有这个键时同样用后备。后备只会让身份更容易「变了」、多读一些文件，不会给出错的身份。
- 读取都在身份任务里（`Background`），lockfile 每次任务读一次；lockfile 读不出、超过 64 MiB、不是 JSON 或没有 `packages` 时整个按后备。`node_modules` 不监听，所以只在 `node_modules` 里重装、lockfile 和清单都没改时，已安装的 `package.json` 的变化在下次打开项目或依赖读取 `NotFound` 时（根的有效性一条）才发现（已知限制，ADR 0033 同类）。
- 包的路径表：每个（生态、包名、版本、身份、包目录）一个 `code_intel.deps` 的单元，行在 `cd_pkg`（一行：生态、包名、版本、身份、已安装的 `package.json` 的五项 stat、包目录、记录时刻）和 `cd_pkg_file`（每个闭包里的文件一行：序号、相对 `DepRoot` 所在目录的路径、OID、设备号、inode、大小、修改时间和状态改变时间（纳秒）、状态）里；边不在路径表里，取自 `cd_spec`（上一条）。单元的字节数按行估算（每行约 80 字节加路径），一个 TS 项目约 250 行、20 KB。它与依赖的单元在同一个配额里按访问时间淘汰；淘汰了就当作身份没见过，走 stat 表或重读。用单元和行，不用 cache 的普通条目：路径表和它引用的单元在同一组表里、同一个写事务里写入，淘汰和配额的规则一致（[cache 设计「命名空间」](cache.md#命名空间)已按此修改）。
- 打开项目、lockfile 或清单变化、第 3 层换了子项目时（`Background`、`BusyGuard` 的身份任务，每个项目同时最多一个，运行中又有变化的结束后再运行一次）：对每个直接依赖找到包目录、`lstat` 它的 `package.json`、算身份。路径表里有同一个键的单元：直接用它的行，不读、不 stat 闭包里的文件；但路径表和它引用的单元分别淘汰（路径表每次打开项目都用到，单元只在查询时用到），所以用之前对表里的 OID 按 500 个一批对 `code_intel.deps` 做一次存在检查：命中的单元连同路径表的单元一起标记访问（两者从此一起变旧），缺的文件排进依赖层重新读和抽取（核对 OID、写入单元），不当作已入索引（20 个直接依赖、约 250 个文件、4 MiB 的 lockfile 合计约 20–30 ms，`code_intel/deps_identity`）。没有：取这个包名最近的一张路径表作为 stat 表，把这个包排进依赖层。
- stat 表（身份变了或取不到时，ADR 0033）：闭包里每个文件 `lstat` 一次（经它所在根的 `SafeRoot`），五项都与表里的相同、并且状态改变时间早于那张表的记录时刻 2 秒以上时，沿用表里的 OID，不读；否则读出、按读到的字节求 OID（git blob 格式，仓库的哈希算法；被忽略的文件不做规范化转换）。两种情况的出边都按 OID 取：单元在缓存里就读 `cd_spec`，不在就解析（抽取时写入单元和 `cd_spec`）。按身份或 stat 表的捷径得到的 OID 在跳转真正读这个文件时核对（下文的读取核对）。

**依赖映射**：每个项目一份，只在内存里，与工作区的 `LiveMap` 分开（ADR 0033）。

```rust
pub(crate) struct DepMap {
    pub generation: u64,
    pub packages: Box<[Arc<DepOrigin>]>,
    /// 每个文件：所在的包（`packages` 的下标）、路径、OID、语言。按包、按序号排列。
    pub files: Box<[DepEntry]>,
    /// 按 OID 前 8 字节排序的（前缀、`files` 的下标），查找时核对完整 OID，与 `BaseMap` 相同。
    pub by_oid: Box<[(u64, u32)]>,
}
```

- 由身份任务和依赖层在包的路径表就绪时整体替换（新的 `Arc`），查询在取快照时一并取它（`Published` 带 `Arc<DepMap>`），之后不受替换影响。第 3 层换了子项目时原来的包保留，按最近用到的子项目最多保留 8 个子项目的包；一个项目最多 20 000 个文件，多出的不进入映射（只走按需解析）。约每个文件 50 字节加路径，上限处约 1 MB，计入本 crate 的常驻份额（「性能」）。项目关闭、切换当前 worktree 时释放。
- 名字表不读它：⌘K 的符号搜索不包含依赖（ADR 0033 的调研「决定」第 1 条，产品负责人交给负责推进的 agent 决定），名字表只由 `code_intel.tags` 的单元和工作区映射建成。

**何时建索引**：

- 依赖层（批量层，排在第 4 层之后、第 5 层之前，`background_yielding()`）：工作是身份任务排进来的包。每批取一个包的闭包：按出边从入口顺序走，沿用 stat 表的文件不读；每个文件的 OID 按 500 个一批对 `code_intel.deps` 做存在检查，命中的从 `cd_spec` 取出边、不读不解析，缺的经它的根读出（沿用 stat 表的也要在这时读）、核对或求 OID、抽取，单元和 `cd_spec` 写入 `code_intel.deps`（一批一个写事务，与第 3–5 层的批次规则相同）；路径表排进来的缺失文件也按这一条补上。走完一个包，写它的路径表单元，替换依赖映射。抽取与第 3–5 层相同：后台上限 1 MiB、截止时间 250 ms、看门狗、暂停、取消、让路。
- 配额：依赖层自己的写入引起 `code_intel.deps` 的淘汰时（写入前后它的淘汰代数不同），依赖层在本次会话停下，`coverage.deps` 为 `Done`；项目的单元在另一个命名空间里，不受影响。之后按需解析的结果照常写入（只淘汰依赖的单元）。
- 按需（第一次跳进依赖时）：跳转第 2 步，解析扩展点把裸模块名交给 `ResolveContext::dependency`，找到的文件当场读、求 OID、对 `code_intel.deps` 做存在检查，命中就用，否则当场抽取（`Interactive`、查询的 `BusyGuard`、交互截止时间、后台上限 1 MiB），结果用于这次查询，并交给 `Background` 任务写入 `code_intel.deps`。依赖的依赖只在这里出现，不进入依赖层和依赖映射。TS 的裸模块名解析是 #19 的解析扩展点；在它之前依赖层照常运行，跳转只经第 3 步的依赖一层找到依赖里的定义。
- 依赖文件成为当前文本（`Focus::Current` 的 `TextRef::origin` 是 `Dependency`）时，第 1 层经它的根读、核对 OID、抽取，写进 `code_intel.deps`。第 2 层不包含依赖文件。

**查询里的依赖**：

- 跳转第 2 步：依赖目标里导出的同名定义是 `Imported` 层的 `Bound(Import)`，`Location::origin` 是 `Dependency`，可以直接跳。
- 跳转第 3 步：按名字在 `code_intel.deps` 里查定义，与项目的查找用同一套分页语句（`cd_def`），经依赖映射按（OID、语言）得到路径，候选的层是 `Dependency`，排在项目的各层之后；语言兼容组和可见性的过滤照常。第一批只加依赖的第一页（≤ 256 行），在项目的第一页和最近的文件之后；其余页排在项目的其余页之后，受同一个时间盒约束。
- `Named`（⌘K）不查依赖。`References` 的被查定义在依赖里时，它所在的文件经它的根读；调用方和同名引用只在项目文件里找，不搜依赖。
- 读取核对（ADR 0033）：查询读一个依赖文件时按读到的字节求 OID，与依赖映射或 `TextRef` 里的不同时：`TextRef` 的返回 `Stale`；映射里的当场按新字节抽取，结果用于这次查询，并更新路径表的这一行和依赖映射（`Background` 任务）。
- 完整性：依赖文件不在「完整性的核算」的能参与回答的文件里，依赖层没有完成、依赖的页没有读完都不让结果成为 `Partial`，也不计入 `unsearched` 或 `outside`。理由：依赖层有限额，大多数项目的依赖永远不会全部入索引；完整性回答的是「本仓库里还有没有没看过的文件」，依赖的候选是额外给出的。`coverage.deps` 只给探针和 bench 用。

**依赖根的读取限制**（ADR 0033 的四条，关键约束）在代码里只有一个入口：`deps::Roots` 打开根、解析链接，`DepFile` 只能由它构造，所有读取都经 `Roots::read(&DepFile)`：

1. 工作区里的根经 worktree 根的 `SafeRoot::open_dir` 打开，不按路径另开 `SafeRoot`。
2. 工作区之外的根（`CacheRoot`）的路径只取 Lectern 自己进程的环境变量和默认值，不读项目文件、不运行外部命令；TS / JS 不用它们。
3. 包内的路径按词法解析，离开包目录就丢弃；链接按词法解析，目标不在这个 worktree 已发现的某个依赖根之内就丢弃（[ADR 0040](../adr/0040.md) 对 ADR 0033「同一个根」的修订）；解析后的路径经目标所在根的 `SafeRoot` 读，各级不得是符号链接。
4. 只读允许的扩展名，单个文件 ≤ 1 MiB。

`deps_*` 测试逐条构造越界的情形（见「测试」）。

### 交互让路

[ADR 0033](../adr/0033.md)「交互时让路」，kernel 的接口见 [kernel 设计「后台工作」](kernel.md#后台工作)（`PluginHandle::background_yielding`、`BackgroundGuard::proceed_item`、`yield_requested`、`Grant`、`SLOT_MAX_BYTES`）。状态：已实现（「验收」第 7 步，第 4、5 层；依赖层随第 9 步接入同一套机制，窗口链随第 8 步，见「与设计的差异」第 7 步）。开关由 `app` 在离开交互静止时打开，本 crate 不判断空闲、不读输入状态，开关默认关闭。

- 守卫：每个批量任务任何时候只持有一个守卫，每个批次边界都检查它，所以 kernel 按守卫记的「已停下」覆盖这个任务，暂停时后台状态能到「全部已暂停」（`yield_pause_reaches_all_paused`）。要做的是窗口链或第 3 层时持有不让路的守卫（`background()`），是第 4 层、依赖层、第 5 层时持有让路的守卫（`background_yielding()`）；下一批的种类变了时先 drop 手里的守卫、再取另一种（drop 让路的守卫同时离开 kernel 的时段队列、交还保留给它的令牌）。换守卫只在批次边界发生，两次之间这个任务没有守卫、也不做工作。
- 批次边界：持有不让路的守卫时 `guard.proceed().await`（只看暂停），照常跑一批；持有让路的守卫时 `guard.proceed_item(下一项的字节数).await`：
  - `Grant::Batch`：照常跑一批（开关关着，或刚关上）。
  - `Grant::Slot { deadline }`：只做一项，到 `deadline` 为止：一个文件（读、存在检查、抽取，结果在这一项之后立即用一个写事务写入），或一次 ≤ 500 个 OID 的存在检查（大小记为 0）。`yield_requested()` 在 `deadline` 变为 true，在途的抽取在下一个分片边界停下（下一条）。
  - 时段做不了的项（超过 256 KiB，`SLOT_MAX_BYTES`；语言包的抽取一个时段做不完）放进这一层的「暂缓」列表（`Batch::aside`），按取出的顺序排着，这一层接着做后面的项；开关关闭后的第一批（`Grant::Batch`）先把暂缓的项按原来的顺序放回队列前面，再照常做。所以用户一直在交互时第 4、5 层照样前进（ADR 0033），时段做不了的项等交互结束。只剩暂缓的项（或停下的超过 256 KiB 的一项）时，边界向 `proceed_item` 要一个超过 256 KiB 的大小，kernel 等开关关闭后才返回 `Batch`，任务不退出。
- 等待时段期间有了窗口链或第 3 层的工作（打开文件、焦点变化，`kick_batch`）：批量任务等待的 future 同时登记在批量状态的唤醒列表里，被唤醒时先看有没有不让路的工作，有就 drop 让路的守卫（停下的那一项的状态留在任务里，之后取回让路的守卫时接着做；超过 256 KiB 的见下文 `LargeSlot` 一条）、取不让路的守卫去做它。所以第 3 层从不排在保活时段后面。换了运行第 3–5 层的项目（焦点到了另一个项目、项目关闭）或停下的那一项的会话结束时，等待的 future 同样被唤醒，任务丢掉停下的状态、交还 `LargeSlot`，再按新的项目取守卫，所以另一个项目的工作不排在原来项目的让路边界后面。
- 在途的抽取在分片边界停下、保留状态，不放弃（核心语言）：解析的进度回调在 `yield_requested()` 为 true 时返回 `Break`，`Parser` 不 `reset`、输入保留，下次用同一份输入继续（ADR 0034 调研：恢复后的树与一次解析相同）；一次遍历（ADR 0031）每 1024 个节点检查一次，停下时保存遍历的栈。抽取返回 `Outcome::Parked(Parked)`，批量任务带着它等下一个许可，得到 `Batch` 或 `Slot` 时先接着做它（超过 256 KiB 的等 `Batch`）。语言包用的 tree-sitter-tags 不能恢复，放弃这一项、放进暂缓列表，交互结束后从头重做。
- 截止时间按运行的时间计：停下时把这次抽取从看门狗的在途表里撤下、记下剩余的预算，继续时按剩余的预算重新登记，所以让路不会让一个文件因为等待而 `TimedOut`。暂停（`pause_requested()`）、取消和会话结束时丢掉保存的状态，按原来的规则放弃这一项。
- 超过 256 KiB 的一项停下之后：它拿着 `LargeSlot`（超过 256 KiB 的文件同时只在一个批量任务里抽取），要等开关关闭才能继续（不进入时段）。等待期间这一层还有时段能做的项时，时段做下一项、停下的大项留着；时段里那一项也停下时（每个任务只留一份保存的状态），大项丢掉保存的状态、交还 `LargeSlot`、放进暂缓列表。不让路的工作（窗口链的窗口、第 3 层超过 256 KiB 的文件）要 `LargeSlot` 时不等它：停下的持有者把保存的状态登记在 `LargeSlot` 里（`LargeTurn::park`），请求方直接接手，丢掉那份状态、作废持有者的那一轮（`revoked`），并唤醒等待的批量任务；持有者被唤醒后把这一项放回它那一层的队列，下次从头抽取。接手不需要持有者先运行，所以不让路的请求方与批量任务数和线程数无关；让路的批量任务要 `LargeSlot` 时仍在自己的线程上等持有者交还，这一点要求批量任务数不多于批量线程数（默认配置二者相等）。第 3 层和窗口链因此不因让路的大文件变慢（ADR 0033「第 3 层不让路」），一棵停下的大树和一个不让路的大工作也不同时在内存里。不大于 256 KiB 的停下的项不占 `LargeSlot`，在时段里继续。
- 内存：保存的状态（解析器、部分语法树、遍历的栈、输入）每个批量任务至多一份，超过 256 KiB 的至多一份（`LargeSlot`），所以建索引期间的临时内存仍按「性能」的 ≤ 40 MB 计。
- 不变的：第 1、2 层和查询（`Interactive`、`Background` 任务）不受开关影响；窗口链和第 3 层不让路；`pause_requested()` 的语义、后台状态和代数、bench 的采样都按 kernel 的规则，与开关无关。

```rust
// index/batch.rs（内部）。批量任务的循环里有 `guard: TaskGuard` 和 `parked: Option<Parked>`。
/// 任何时候只有一个守卫（kernel 按守卫记暂停）。
enum TaskGuard { Near(Arc<BackgroundGuard>), Far(Arc<BackgroundGuard>), None }
enum Outcome { Done(Done, usize), Paused, Skip, Parked(Parked), Aside }
// state.rs：`Batch::aside: [VecDeque<Item>; 3]`，第 4、5 层时段做不了的项，`Grant::Batch` 时放回队列前面。
// `LargeSlot`：停下的持有者登记保存的状态（`LargeTurn::park`），不让路的请求方接手、作废那一轮（`LargeTurn::revoked`）。
/// 让路的任务从一个边界带到下一个边界的一项：已读出、已登记在途；等它的时段（开关打开时超过 256 KiB），
/// 或停在分片边界、抽取的状态在 `resume` 里。`Send`，每个任务至多一份。
struct Parked { project: Arc<Project>, session: u64, t: usize, item: Item, flag: Arc<AtomicBool>, key: BlobId,
                bytes: Vec<u8>, resume: ResumeSlot, large: Option<LargeTurn> }

// extract.rs（公开）：抽取经 `Deadline::yielding` 让路，状态存在 deadline 带的槽里。
impl Deadline { pub fn yielding(self, yields: PauseProbe, slot: ResumeSlot) -> Deadline; }
pub type ResumeSlot = Arc<Mutex<Option<Resume>>>;
/// 解析器（未 reset）或语法树加遍历的状态，以及剩余的截止预算；输入由调用方再传一次。
pub struct Resume { state: ResumeState, pub budget: Duration }
enum ResumeState { Parsing(Parser), Walking { tree: Tree, walk: walk::WalkResume } }
// `ExtractStatus::Parked`：停下（核心语言，状态在槽里）或放弃（语言包，槽是空的），不写入。
```

### 大文件

[ADR 0034](../adr/0034.md)「打开文件的符号抽取」「文本存储与定位」。状态：已设计，尚未实现（「验收」第 8 步；`Location::byte` 在第 6 步）。

| 大小 | 后台（第 2–5 层、依赖） | 文件被打开（第 1 层） | 查询里的当前文本（跳转第 1 步） |
|---|---|---|---|
| ≤ 1 MiB | 整份抽取 | 整份抽取 | 整份解析（≤ 128 KiB 的保留，ADR 0037） |
| 1–16 MiB | `TooLarge` | 窗口链（下文），完成后整份抽取一行是 `Windowed` | 只解析位置所在的窗口，8 ms 截止时间 |
| > 16 MiB | `TooLarge` | 不抽取 | `Failed(TooLarge)` |

**窗口**：一个窗口从起点 `s` 开始，解析 `[s, min(s + 64 KiB, 文件长度))`；窗口到文件末尾时终点是文件末尾。否则按树切（[大文件调研「整个文件按窗口高亮」](../research/large-files-rendering.md#整个文件按窗口高亮)的规则）：从根节点往下，每一层取窗口末尾之前最后一个完整的子节点（不是 `ERROR`、不含 `MISSING`、终点严格早于窗口末尾）；被窗口截断的那个子节点（终点就是窗口末尾的那个）里还有这样的子节点时进入它、继续往下，终点取找到的最深的那一个的终点，即窗口末尾之前最后一个由树确认的语句或成员边界。终点等于窗口末尾的节点和叶子一律不算完整，即使它没有 `ERROR` 和 `MISSING`：解析器在窗口末尾当作文件末尾，`const foo = 1` 截在 `const fo` 时按自动分号补全成一个完整的声明，`fo` 也是一个完整的标识符，切在那里就把 `foo` 分成两半。终点严格早于窗口末尾的节点，它的最后一个 token 后面至少还有一个解析器看到过的字节，所以这个 token（标识符、数字、字符串）已经由词法结束，切在那里不会切开它。只有窗口到文件末尾时（不切）终点可以等于窗口末尾。所以包住整个文件的 IIFE、UMD 和压缩成一行的 bundle（`typescript.js`、webpack 的输出、`*.min.js`）也在函数体里的语句之间切开，窗口仍约 64 KiB、语法树仍约 2–5 MiB（调研实测最大 5.3 MiB）。找不到任何完整的节点时（例如窗口里只有一个超过 64 KiB 的字符串或注释）才把窗口扩到 256 KiB 再找一次；仍没有时切在窗口末尾之前最后一个叶子节点（token）的终点（同样要严格早于窗口末尾）；连叶子都没有（一个超过 256 KiB 的 token）时，从窗口末尾向后跳过标识符字符（ASCII 字母数字、`_`、`$`、UTF-8 的多字节序列）找到第一个非标识符字符处切开。任何情况下都不在一个标识符中间切开。窗口单独解析的作用域可能不完整（ADR 0034 的已知限制：窗口从函数体中间开始时，它里面的定义没有外层的 `scope`）。窗口的抽取结果是名字起点在 `[s, 终点)` 里的定义和引用，下一个窗口从终点开始。行号和字节偏移都是在整个文件里的：窗口单元记下起点所在的行（`part_line`），窗口里的行号加上它。一个窗口的语法树约 2–5 MiB，算完即释放。

**窗口链**（文件被打开、大小 1–16 MiB）：

- 开始：`SafeRoot::open_file` 打开，按 1 MiB 的区间顺序 `read_ranges`，边读边用 `git::BlobHasher`（流式的 blob 哈希，见「对其他 crate 的要求」）求 OID，读完核对每段返回的身份都与打开时相同；OID 等于要建索引的 OID（映射里的或 `TextRef` 的）才继续，内存里同时只有一个 1 MiB 的区间。原始字节的 OID 不等时这个文件需要规范化转换（如 CRLF），`canonical_oid` 要整份字节，这时才 `read_all` 一次（≤ 16 MiB，在 `LargeSlot` 里，用完即释放），对上了记 `canonical = 0`。之后只留下 `OpenFile`（`Arc`）和它的 `identity()`。状态改变时间在读之前 2 秒以内的文件推迟 2 秒再开始（与依赖的 stat 表相同的 racy 规则），避免同一个时间戳里又被改过。
- 每个窗口：`read_ranges` 读 `[s, s + 窗口长度)`，返回的 `meta.identity()` 与开始时的不同就放弃整条链（内容变了，fs_watch 的事件会带来新的 OID）；抽取，立即作为一个单元写入 `code_intel.tags`（键加 `part = s`，行见「存储表」），所以暂停、取消或退出只丢在途的那个窗口（ADR 0029 第二条）。一个窗口超时（`TimedOut`）也写一行，链从它的窗口末尾继续，但这条链不算完成。
- 续接：开始之前按 `(oid, lang, grammar, query)` 读出已有的窗口行（`part >= 0`，按 `part` 排序），从 0 起沿 `part_end` 走，第一个断开处就是续接的起点（行号取前一个窗口的 `part_line` 加上它里面的换行数，读那个窗口的字节数出来）；被淘汰的窗口因此会被补上。
- 完成：最后一个窗口的终点等于文件长度、链上没有 `TimedOut` 时，写整份抽取的一行，状态 `Windowed`，替换 `TooLarge` 或 `TimedOut`（「调度、批次与取消」的替换规则，那里列了这第二种替换，以及它算作本 crate 自己的一次淘汰），然后按 `Done` 置位 `indexed` 提示。之后这个文件算作已入索引，名字表、按名字查找、查引用经窗口单元用到它的定义和引用。
- 淘汰：窗口单元与其他单元一样按访问时间淘汰。存在检查命中 `Windowed` 时多一条语句核对链仍完整，不完整按 `TooLarge` 处理（核算里计为未搜，下次打开时续接）；标记访问时连同它的窗口单元一起标记。
- 线程：可见区域附近的三个窗口（可见窗口和前后各一个）先在 `Interactive` 上、持有 `BusyGuard`，按 ADR 0034 首屏窗口的启发式起点单独抽取，结果只在内存里（与保留的解析放在一起），供同文件跳转；它们的起点是启发式的，不是语法树确认的切点，所以不写入。持久的链全部在批量层最前面，持有不让路的 `BackgroundGuard`，不延长「静止」的等待。每个窗口（读入的窗口和它的语法树）都按超过 256 KiB 的工作计，照 `LargeSlot` 一次一个；需要规范化转换时的那次 `read_all` 也在 `LargeSlot` 里。所以链的临时内存是一个 1 MiB 的哈希区间，或一个窗口和它的树（64 KiB 的窗口约 2–5 MiB，扩到 256 KiB 的最多约 21 MiB，调研实测），或那一份 ≤ 16 MiB 的内容，三者不同时出现（`code_intel/window_peak`）。

**跳转里的大文件**（当前文本 1–16 MiB）：

- 当前文本的身份：文本成为当前文本时，ADR 0037 的预热任务（`Background`）对超过 1 MiB 的文本不整份解析，改为按 1 MiB 的区间流式读一次（同窗口链的开始：`BlobHasher`，内存里只有一个区间；需要规范化转换的才整份读入一次）、求 OID、核对 `TextRef::oid`、顺带数换行，记下 `LargeText { oid, identity, recorded_at, checkpoints }`：`checkpoints` 是每 64 KiB 一个（字节偏移、该处的行号），16 MiB 约 3 KiB。与保留的解析同一个上限和生命周期（最多 2 份、是当前文本期间保留、之后 60 s）。
- 第 1 步：`open_file`，身份等于记下的、记下时不在 racy 窗口里时不再求 OID；按行号找不超过它的检查点，`read_ranges` 读那个检查点之后的 64 KiB 加 4 KiB，数换行找到这一行的行首，目标字节是行首加列。窗口按 ADR 0034 首屏窗口的规则取：从目标之前约 8 KiB 处的安全位置（往前至多再找 4 KiB，只看目标前后各 64 KiB）到目标之后 4 KiB，用同一次 `read_ranges` 读出（至多 4 段、合计 ≤ 1 MiB）。`ParsedFile::parse_window`（基准字节和基准行号）解析它，截止时间 8 ms；locals 和绑定只在窗口里。
- 超时（8 ms 内没解析完）：按目标字节处的标识符字符（与限时搜索的整词规则相同）取出名字，`RefSyntax::Plain`，没有 `Bound(Local)`，接着做第 2、3 步。所以大文件里的跳转总能列出按名字找到的候选。
- 同文件里窗口之外的定义：来自同一个 OID 的窗口单元（第 3 步，层是 `CurrentFile`），以及内存里可见区域的三个窗口；链没写完时只有已写入的部分，其余靠第 3 步的其他层和限时搜索。
- 身份还没记下（预热没完成）或对不上时，第 1 步在 `Interactive` 上同样流式读一遍求 OID 并顺带算出检查点（16 MiB 在 Linux 上约 30–40 ms，冷路径，只记录），然后同上。
- 当前文本来自对象库（`TextOrigin::Object`，如大文件 Diff 的旧一侧）：`GitAccess::blob` 只能整份取出，不能按区间读。对象库的内容按 OID 不变，不需要核对身份；预热任务整份取出一次、算出检查点，记进 `LargeText`（不留内容）。跳转时位置落在保留的窗口解析里（上一次跳转解析过的窗口）就直接用它，不读；否则整份取出（≤ 16 MiB，`Interactive` 上的临时内存，取出窗口后立即释放）、解析窗口。整份取出在 16 MiB 时约几十毫秒（zlib 解压加复制），不在 20 ms 的份额里，作为已知限制只记录（`code_intel/goto_large/object`）；不把 16 MiB 的内容留在内存里，因为它超过 ADR 0037 保留解析的 12 MiB 上限，也不属于任何常驻份额。窗口的解析按（OID、语言版本、窗口起点）放进 ADR 0037 的保留解析里，计入它的 12 MiB（一个约 20 KiB 的窗口约 1 MiB）。

**查询读到的其他大文件**：整份抽取是 `Windowed` 的文件（打开过的大文件）按窗口用：查引用用 `ci_ref` 找出含这个名字的窗口单元，流式读一遍核对 OID（同窗口链的开始），只读、只解析这些窗口（起点和终点取自单元）；限时搜索不搜它们（已入索引）。其余超过 4 MiB 的文件照旧计为 `TooLarge`、未搜。

**字节偏移**（`Location::byte`）：抽取时每个定义记下名字起点的字节偏移（`Def::byte`、`ci_def.byte`），引用位置本来就有字节区间。交给视图时：单元的 `canonical = 1`（抽取的字节就是 `oid` 那份内容：原始字节的 OID 等于键，或对象库一侧、依赖文件）时为 `Some`，经 `canonical_oid` 才对上的为 `None`，视图按行号定位；现场解析的当前文本、引用同理。口径与 `git` 的 hunk 和展示行的偏移相同（[git 设计「展示行」](git.md#展示行)：这一侧 blob 的字节），所以从 hunk 打开、从候选打开都按 blob 的字节定位。

### 规范内容 OID 的批量核对

[git 设计「规范内容的 OID」](git.md#规范内容的-oid)：`canonical_oid` 每次调用在新句柄上解析一次 index（10 万文件约 22 ms），`canonical_oids` 整批也只解析一次（#135）。状态：已设计，尚未实现（「验收」第 6 步）。

- 改用整批的地方：
  - 核对任务的重新读：一遍里读出的路径先按原始字节的 OID 分出与文件列表不同的那些，再按每批 ≤ 256 项、字节合计 ≤ 2 MiB 调用 `canonical_oids`，一批的内容在这期间留在内存里（≤ 2 MiB，与批量层一批读入的上限相同，计入「性能」建索引期间的临时内存）。单个文件超过 2 MiB 时自成一批。checkout 之后 1000 个约 8 KiB 的 CRLF 文件进入重新读集合时，原来是 1000 次 index 解析（10 万文件约 22 s），现在 4 次（每批 256 个、约 2 MiB）。
  - 批量层的一批（≤ 64 个文件、≤ 2 MiB）：读到的 OID 与排队时不同的文件在写入之前一次 `canonical_oids`。
  - 查询结果的写入（限时搜索命中的文件、现场抽取的文件，由 `Background` 任务写入）：一次写入一批。
- 仍是单个调用：交互路径上只核对一份内容的地方（跳转第 1 步的当前文本、查引用时被查定义所在的文件、窗口链的开始），计入它所在的延迟预算。
- 出错：整批返回错误时对这一批逐项调用 `canonical_oid`，出错的那一项按读不出处理（覆盖部分里是 `Unindexable`），其余照常。
- 依赖文件不调用：被忽略的文件不做规范化转换（ADR 0033），OID 就是原始字节的哈希。

## 性能

影响的预算（[性能预算](tech-research.md#性能预算)，M 系列 Mac，系统文件缓存已热）：

| 预算 | 本 crate 的份额 | 基准 |
|---|---|---|
| 冷启动 20 ms | 首帧之前不做任何事：`AfterFirstFrame` 激活，`activate` 只建空表、不加载 grammar | `plugin/code-intel/activate`（kernel 的 < 1 ms） |
| 跳转：同文件或索引命中，候选 ≤ 50 ms（96 KiB 以内的当前文本任何时候成立；96–128 KiB 的靠保留的解析成立，预热完成之前的冷跳转是已知限制：[ADR 0037](../adr/0037.md) 对 ADR 0030 的修订） | 抽屉的份额是 18 ms（从收到第一批的事件到列表画出：一帧 16 ms 加 2 ms 送到 UI 线程），本 crate 从查询到第一批有候选的结果 ≤ 20 ms（[ADR 0037](../adr/0037.md)）。前提：当前文本（100 KB 以内）是当前文本、解析已保留；在首次跳转会遇到的状态下分别判定：核算的遍历已缓存或未缓存、约 3000 个定义的常见名字、命名空间满额。分项按实测（Linux 容器，「实际验证」）：在保留的解析上查位置 < 0.1 ms；按名字查找，第一页加上最近的文件 ≤ 5 ms（一页 ≤ 256 行，每 500 个文件一条语句，与名字有多少定义无关）；过滤、分层与排序 ≤ 1 ms。`Done` 在查询的 1 s 时间盒之内，最多再加一条语句（满额模式约 0.35 s）。冷路径（文本刚成为当前文本、预热还没完成，为光标处解析，随大小线性增长：128 KiB 的整次冷跳转 Linux 实测 29.6–35.9 ms，其中解析按 ADR 0034 的 TS 约 6 MiB/s 约 21 ms，其余是 locals 和查找）≤ 50 − 18 = 32 ms，在 96 KiB 处判定，128 KiB 处只记录 | `code_intel/goto_indexed`、`goto_uncached`、`goto_popular`、`goto_full`（10 万文件、每 1000 个一个子项目），各自的 `/done` 只记录，`goto_full/done` ≤ 1.02 s、每次 `Complete`；冷路径 `goto_cold_parse`（96 KiB）≤ 32 ms、`goto_cold_parse/at_128k` 只记录；同文件 `goto_same_file`、对象一侧 `goto_object`、新索引的第一次跳转 `goto_first` 只记录 |
| 跳转：唯一候选的目标首屏高亮 ≤ 100 ms | 本 crate 的部分同上；目标文件的读取与高亮属于 `file_reader` | `app/goto_single` |
| 跳转：未覆盖，第一批 ≤ 300 ms，到停止 ≤ 1.1 s | 第一批是第 1–3 步的第一批；限时搜索 1 s 后停止。取消在 20 ms 内生效，每条路径都是：查询里的每条语句都有上限（一页 ≤ 256 行，或 ≤ 500 个 OID），语句之间检查查询是否已停止 | `code_intel/search_stop` ≤ 1.02 s |
| 引用：第一批 ≤ 200 ms，10 万文件全部 ≤ 2 s | `ci_ref` 按页查找，每页读索引的 ≤ 256 行；每个候选文件的现场抽取约 1–2 ms / 10 KB；第一批不等搜索范围的读取和满额模式的核对；时间盒 1.9 s，满额模式的核对在 `ci_ref` 一轮之后有余地完成 | `code_intel/references_100k/all`、`code_intel/references_100k/first`；满额模式 `code_intel/references_full/all` ≤ 2 s、`/first` ≤ 200 ms，每次 `Complete` |
| 当前文本的解析（[ADR 0037](../adr/0037.md)） | 最多 2 份，源码 ≤ 128 KiB 的才保留，按每字节源码 48 字节估算合计 ≤ 12 MiB；是当前文本期间一直保留，之后 60 s 不用即释放，语言被替换时立即释放。只因阅读才填充，不在 [ADR 0035](../adr/0035.md) 的采样状态（首页加载完成、没有打开文件、Diff 或会话）里，所以不计入下面的空闲常驻，也不占 ADR 0035 的 12 MB 余量；`resident_*` 测量前丢掉它 | `code_intel/parse_cache_bytes` ≤ 12 MiB，在上限处测量：两份略小于 128 KiB 的 TypeScript（实测见「实际验证」） |
| 索引覆盖：审阅集合 ≤ 2 s，子项目 ≤ 10 s，10 万文件 ≤ 2 分钟（接电源） | 单线程抽取吞吐 ≥ 4 MB/s，按仓库形状的语料测量（每种首发语言约 1 MiB，切成约 8 KiB 的文件），核心语言用一次遍历抽取（[ADR 0031](../adr/0031.md)）；热启动 10 万文件的存在检查 ≤ 500 ms，不含之后的访问标记 | `code_intel/extract_throughput`、`code_intel/extract_1mib`（只记录）、`code_intel/warm_100k`、`code_intel/warm_100k_touch`（只记录） |
| 空闲内存：monorepo < 100 MB，10 万文件的仓库 < 120 MB（后台暂停时和全部完成后；后者的份额加总见 [ADR 0035](../adr/0035.md)） | 本 crate 常驻 cloudprint 形状 ≤ 4 MB、10 万文件 ≤ 10 MB：映射、覆盖部分、`indexed` 提示、依赖映射（≤ 1 MB，ADR 0033；`resident_*` 的夹具没有依赖，由 `deps_map_bytes` 单独测）、第 5 层本次会话已确认的 blob 集合（每个 8 字节的键，10 万个约 1.2 MB，项目关闭或换会话时释放）、已编译的 grammar 配置（按用到的语言加载）、项目表；不含名字表和 `file_reader` 的 8 MB 高亮与文本 LRU，与后者合计在 ADR 0030 的 ≤ 10 MB / ≤ 20 MB 目标之内 | `plugin/code-intel/resident_cloudprint`、`plugin/code-intel/resident_100k` |
| 名字表 | 10 万文件 ≤ 8 MB，构建 ≤ 1 s（一遍 `ci_ext`，再按 `ci_def_name` 的顺序分页读索引，每条语句 4096 项，见「名字表」）；cloudprint 形状 ≤ 0.1 MB、≤ 30 ms | `code_intel/names_100k`、`code_intel/names_100k_bytes` |
| 映射 | 10 万文件重建 ≤ 20 ms、≤ 3 MB | `code_intel/map_100k`、`code_intel/map_100k_bytes` |
| 建索引期间的临时内存（不设顶层预算，ADR 0029） | ≤ 40 MB：每个批量任务读入的内容 ≤ 2 MiB，加一棵 1 MiB 以内文件的语法树和遍历（实测接近 1 MiB 的 TS 文件约 31 MiB；超过 256 KiB 的文件同时只在一个批量任务里抽取，所以有几个批量线程都只有一棵大树）。`git` 的工作区比较（`GitAccess::changed_paths`）期间的临时内存不在这 40 MB 里：比较不用 `TrackedPaths`，自己解析 index，10 万文件时一次比较的峰值 65.8 MB（`git/compare_100k/peak_bytes` ≤ 72 MB，`git` 的份额，[ADR 0035](../adr/0035.md)），单独就超过 40 MB；`code_intel/batch_peak` 的夹具用 `FakeGit`，不含它。最坏情况下，打开项目时首页的计数比较和本 crate 的核对比较各在一个新 `Worktree` 上同时做冷比较，加上批量层，进程的临时峰值可达约 2 × 66 + 40 ≈ 170 MB（git 的比较之间没有串行化，这是已知的最坏情况，不设顶层预算，同 ADR 0029 的临时内存）。降低的办法是 ADR 0035 的「比较与 `tracked` 共用一次解析」；比较是否与超过 256 KiB 的文件的抽取错开，由本 crate 的下一步决定 | `code_intel/batch_peak` |
| 磁盘 | `code_intel.tags` 配额 224 MiB、`code_intel.deps` 配额 32 MiB，合计 256 MiB（[ADR 0033](../adr/0033.md)）；cloudprint 形状约 1 MB，10 万文件约 270 MiB（第 3 步的合成仓库实测，超过 224 MiB 的配额，见「存储表」的「规模」和配额一条）；一个 TS 项目的依赖约 5 MiB 源码，单元约 5–10 MiB | `code_intel/disk_100k`（只记录） |
| 依赖层（ADR 0033） | 依赖层每个项目每次会话读 ≤ 64 MiB、写 ≤ 32 MiB；21 个直接依赖、约 5 MiB 声明闭包的 TS 项目，从空缓存到依赖层完成单线程约 1.3 s（4 MB/s 的抽取吞吐），在批量层上、排在第 4 层之后，不推迟 `app/index_100k` 的里程碑；身份不变时打开项目只读 lockfile、每个直接依赖的链接和清单的 stat，再对路径表里的 OID 做存在检查（约 250 个文件一条语句）：≤ 50 ms（4 MiB 的 `package-lock.json`，按 1 MiB 的区间流式扫描）；依赖映射上限 20 000 个文件、≤ 1 MB，计入下面的常驻份额 | `code_intel/deps_ts_project`（只记录：合成的 21 个直接依赖，从空缓存到 `coverage.deps` 为 `Done`）、`code_intel/deps_identity` ≤ 50 ms（同上的项目、缓存里已有全部路径表和单元，从打开到身份任务结束）、`code_intel/deps_map_bytes` ≤ 1 MB（2 万个文件的依赖映射，`Memory`） |
| 交互让路（ADR 0033） | 开关打开后在途的抽取在一片之内停下（解析的进度回调约每 100 次操作一次，遍历每 1024 个节点一次）：≤ 10 ms；之后批量层合计 ≤ 一个核的 10%（kernel 的保活时段，每个时段只做一个 ≤ 256 KiB 的文件）；停下再继续的结果与一次做完的相同；不让路的层不变慢 | `code_intel/yield_stop` 最大值 ≤ 10 ms（1 MiB 的 TS 文件在解析中途打开开关，到这次抽取返回 `Parked`）；帧时间由 `app/scroll_frames_indexing` 判定（bench 设计，ADR 0033） |
| 大文件的跳转（ADR 0034） | 1–16 MiB 的当前文本，身份已记下时：打开、比较身份、读两段（≤ 68 KiB 与窗口）≤ 1 ms，窗口解析 ≤ 8 ms（截止时间），之后与普通跳转相同，第一批 ≤ 20 ms；身份没记下时流式读一遍求 OID（16 MiB 约 30–40 ms，Linux），只记录；对象库一侧的大文本每次整份取出 blob（不能按区间读），位置在保留的窗口解析里时不取，只记录（已知限制） | `code_intel/goto_large`（4 MiB 的 TypeScript，位置在中部，身份已记下）≤ 20 ms、`code_intel/goto_large/cold`、`code_intel/goto_large/object` 只记录 |
| 打开文件的窗口链（ADR 0034） | 窗口约 64 KiB（按树切，包住整个文件的 IIFE 也在函数体里切开），一个窗口的语法树约 2–5 MiB；16 MiB 的文件整条链约 4–5 s 的 CPU（Linux 估算），在批量层最前面。临时内存：求 OID 时一个 1 MiB 的区间（流式哈希），之后一个窗口和它的树；找不到切点、扩到 256 KiB 的窗口的树最多约 21 MiB（调研实测，压缩 JS），需要规范化转换的文件开始时整份读入一次（≤ 16 MiB）；三者都在 `LargeSlot` 里、不同时出现，计入建索引期间的 40 MB | `code_intel/window_chain_16mib`（只记录：16 MiB 的 TypeScript 从打开到 `Windowed` 写入）、`code_intel/window_peak` 最大值 ≤ 8 MiB（同上，不需要转换、窗口都是 64 KiB：一个窗口的树约 2 MiB 加 1 MiB 的区间，留约 2 倍的余量） |
| 规范内容 OID（git 的 `canonical_oids`） | 重新读、批量层的一批、查询结果的写入：每批（≤ 256 项、字节合计 ≤ 2 MiB，内容在这期间留在内存里，计入建索引期间的 40 MB）至多解析一次 index（10 万文件约 22 ms）；交互路径上的单个核对不变 | `code_intel/reread_crlf_1000`（只记录：`git` 的 `TestRepo` 上 1 万个跟踪文件、`core.autocrlf = true`，1000 个文件进入重新读集合，到覆盖部分装上） |
| 安装包 ≤ 80 MB | 六个 grammar crate 与 tree-sitter 运行时，由 #38 测量 | `app/bundle_size` |

- 首帧之前不调用本 crate；第一个任务在第一个项目打开之后提交，批量线程在第一次提交 `Batch` 任务时才创建（ADR 0029）。
- 以上分项是 `Tier::Share`，加入 bench 的预算表（[bench 设计「预算表」](bench.md#预算表)），状态为计划中，由「验收」各步改为生效。bench 设计已为 #17 预留的顶层条目（`app/goto_candidates`、`app/goto_single`、`app/goto_unindexed`、`app/goto_unindexed_done`、`app/index_review_set`、`app/index_subproject`、`app/index_100k`、`app/idle_monorepo_indexed`）在第 5 步改为生效。
- 夹具：cloudprint 形状（728 个跟踪文件、11 个 worktree）和 10 万文件的合成仓库，由 `testing` 的形状描述加 `git` 的 `TestRepo` 生成，源码内容按语言的样本拼接，不依赖网络；未索引的文件要多到 1 s 内搜不完，`app/goto_unindexed_done` 才能测到停止。
- 数字来自调研在 Linux 容器上的实测和估算，实现时在 M 系列上校准；改动 `Tier::Share` 的数字在同一个 PR 里修改本节。

## 状态与异常

本 crate 没有界面，界面不显示索引和语言服务器的状态（ADR 0012）。下表是交给调用方的结果：

| 情况 | 本 crate 的处理 | 调用方看到的 |
|---|---|---|
| 索引还没建完 | 按「跳转到定义」的顺序现场回答 | 结果陆续追加；限时搜索没搜完时 `Done(Partial)`，是否提示见「未决问题」2 |
| 语言不受支持 | 不抽取 | `Failed(UnsupportedLanguage)` |
| 位置不在标识符上 | — | `Failed(NoSymbol)` |
| 当前文本已经变了 | 不回答 | `Failed(Stale)`，调用方用新内容的 OID 重新查询 |
| 当前文件超过 16 MiB（ADR 0034） | 不抽取 | `Failed(TooLarge)` |
| 打开的文件 1–16 MiB，窗口链还没写完 | 已写入的窗口照常可查，其余在批量层续接；跳转第 1 步只解析位置所在的窗口 | 结果照常；这个文件在核算里计为未搜，直到链完成 |
| 大文件的窗口解析超过 8 ms | 按目标处的标识符取名字，跳过第 1 步的绑定 | 只有按名字找到的候选 |
| 依赖目标读不到、路径离开包目录、链接的目标不在已发现的依赖根里、`node_modules` 本身是符号链接、扩展名不在允许的列表里（ADR 0033、0040） | 不读、不建索引 | 这个候选不出现；不影响完整性 |
| `code_intel.deps` 满了（依赖层自己的写入引起淘汰） | 依赖层在本次会话停下 | 项目的单元不受影响；依赖里的定义只经按需解析找到 |
| lockfile 读不出、超过 64 MiB，或不是 npm 的 lockfile（`pnpm-lock.yaml`、`yarn.lock`） | 身份按包清单的 OID 和包目录（ADR 0033 的后备），另含已安装的 `package.json` 的五项 stat，照常走 stat 表 | 照常，最多多一次 stat 或读取；不会用错的身份 |
| lockfile 已经更新、`node_modules` 还没重新安装 | `integrity` 变了，走 stat 表：闭包里的文件没变，沿用原来的 OID；新的路径表记下的是已安装的 `package.json` 的 stat | 照常，结果是已安装的内容；安装之后那五项变了，再走 stat 表重读 |
| 交互让路开关打开 | 让路的层在分片边界停下，只在保活时段里前进 | 无可见变化；第 4 层、依赖层、第 5 层完成得更晚 |
| 某个文件解析超时 | 记 `TimedOut`，不自动重试；被打开时按交互截止时间重做一次 | 这个文件不提供候选，其余照常 |
| 二进制、超过 1 MiB、读不出 | 记状态（`Binary`、`TooLarge`），工作区里的修改或未跟踪文件标为 `Unindexable` | 不提供候选；二进制算已入索引，其余计为未搜，结果是 `Partial`（「完整性的核算」） |
| 符号链接、路径经过符号链接、过滤器的指针文件（LFS）、其他种类的文件 | 标为 `Excluded`，不读、不建索引 | 不提供候选；不是本 crate 索引的源文件，不参与核算 |
| `cache` 分库不可用、写入被丢弃 | 结果只在内存里用于本次查询，下次重新抽取 | 照常，只是变慢 |
| grammar 或查询编译失败 | 这种语言在本次运行中停用，向 stderr 写一行（`lectern-code-intel:` 前缀） | `Failed(UnsupportedLanguage)` |
| 工作区比较失败（`GitError`）、根目录打不开 | 保留上一份覆盖部分，下一次事件再试；发布的快照标为缺工作区内容 | 未暂存的修改可能暂时按 index 的内容回答；结果不是 `Complete`，直到比较成功 |
| 当前 worktree 没有，或文件列表 `Unavailable` | 没有可发布的快照 | 查询 `Failed(Unreadable)`，不一直 `Running` |
| `fs_watch` 的监听失败 | 丢掉订阅，等下一次 `ProjectEvent` | 同上 |
| 语义导航的提供者失败或停用 | 作废它未回复的请求，按剩下的回复重新合并；都没有时 `semantic` 为 `Failed`，候选不变 | 抽屉保留已有结果 |
| 插件停用 | kernel 丢弃任务；视图手里的 `QueryResults` 仍有效，只是不再更新 | — |

## 测试

全部在 Linux 上运行（`just test -p lectern-code-intel`），不依赖 GPUI，不调用 git 命令行、不访问网络。用到的替身：`testing::FakeGit`（工作区比较、其他 worktree 的 index、blob，按内存里的状态回答并记录调用）、`testing::FakeProjects`（内存里的 `dyn Projects`，可以发 `ProjectEvent`）、fs_watch 的 `FakeBackend` 与手动监听器、`Executor::manual()`、`testing::ManualClock`（截止时间和限时搜索用，看门狗读同一个时钟）、`cache` 的临时目录。

- 语言（`lang_*`）：扩展名到语言、兼容组；语言包认领扩展名，与核心语言冲突时核心语言优先。
- 抽取（`extract_*`）：每种语言的夹具用 insta 快照记录定义（名字、种类、位置、作用域、可见性、参数个数）和引用名；TS 叠加 JS 的查询；局部变量的引用不在引用名里；二进制和超过上限的内容；`EXTRACTOR_VERSION` 或查询文本变化时查询版本变化。
- 截止时间（`deadline_*`）：病态输入在 1 ms 截止时间下返回 `TimedOut`；取消标志置位时返回 `Cancelled`；看门狗在截止时刻置位，没有在途抽取时不唤醒。看门狗的生命周期（`watchdog_*`）：插件停用后线程退出（线程自己的闭包记录的 `plugin::WatchdogThread` 为 `Exited`；只在 Linux 上、且在它退出之后，再确认 `/proc/self/task/*/comm` 里没有 `code-intel-watchdog`）；连续激活、停用 20 次，每次激活的线程在下一次激活前都已退出；`Executor::shutdown` 在激活仍然存活时也让线程退出；停用时正在运行的抽取返回 `Cancelled`；激活在线程启动前失效时不建线程。
- 位置分析（`site_*`）：位置上的标识符与 `RefSyntax`；locals 解析到的定义；光标在定义名上时的 `defines`。
- 定义形式（`forms_*`）与作用域形式（`scopes_*`）：两张表对照 grammar 的覆盖、样例、locals 查询和本文的表（见「定义形式」「作用域形式」）；局部定义的可见性对照 locals 查询的作用域（`forms_local_definitions_are_file_visible`）。
- 存储（`store_*`）：单元的写入与读回；存在检查只返回键完全相同的单元、不标记访问，排队的命中在之后按块标记，淘汰代数变化后的队列被丢弃；按名字查找区分大小写；按名字查定义分页，每页不超过上限、每个定义只出现一次，剩下的行数按索引数出（`store_lookup_by_name_in_pages`）；每条语句读的索引行数不超过一页（测试里的计数 `store::PAGE_ROWS`），不论其中多少是旧版本的单元或其他语言，定义和引用都照样全部找到（`store_lookup_pages_bound_the_rows_read`）；一个单元里同名定义超过一页时跨页读完，一个不少（`store_lookup_pages_go_on_inside_a_unit`）；名字表的扫描按页、每种写法一次、种类位集与直接计算的相同，`stop` 在语句之间生效（`store_def_names_in_pages`）；在给定的 OID 里查找只返回这些单元的定义，每条语句不超过上限（`store_lookup_by_name_in_given_units`）；这几条语句的查询计划按名字和游标定位、不扫描、不排序（`store_lookups_by_name_use_the_name_and_unit_index`）；单元被淘汰后三张表里没有它的行；配额小于数据量时按访问时间淘汰本命名空间的单元；同一个 OID 有旧 grammar 版本或旧查询版本的单元时，按名字查定义、查引用、存在检查和名字表都只返回当前版本的单元（`store_version_filter_*`）；写入被丢弃时不 panic、内存里的结果仍可用。
- 输入代数（`derived_*`），每种输入一组，检查只改这一个输入时依赖它的派生状态都被重算、不依赖它的没有重算（计数钩子）：
  - `derived_files_*`：文件列表换代后基础映射重建，路径和 OID 都相同的覆盖项被删除。
  - `derived_packages_*`：只改子项目（文件列表不变），第 3 层的范围和游标、候选的子项目层、限时搜索的范围按新的子项目计算。
  - `derived_head_*`：提交只改变 HEAD（index 和文件列表都不变），`HeadDiff` 重新比较，刚提交的文件 `head_clean` 变为 true；比较完成之前为 false。
  - `derived_index_*`：只暂存（index 校验和变化），`HeadDiff` 与覆盖部分重新对齐。
  - `derived_overlay_*`：一个路径的有效 OID 变化后它的 `indexed` 提示不再匹配，限时搜索不跳过它；新 OID 的存在检查命中或写入成功之后才重新匹配。
  - `derived_rules_*`：新的 `.gitignore` 忽略了一个未跟踪文件，下一次比较之后它离开覆盖部分和查询结果；`untracked_capped` 时不删除。
  - `derived_grammar_*`：语言包代数变化后基础映射、提示和游标重算（各种变化的细节在 `grammar_*`）。
  - `derived_eviction_*`：配额小于数据量、淘汰掉一部分单元后，全部提示不再匹配；被淘汰的文件重新入索引，仍在的只重新匹配、抽取函数没有被调用；淘汰之后、重新入索引之前，跳转经限时搜索仍能找到这些文件里的定义；分库被删除重建后全部重新入索引。
  - `derived_writes_*`：写入一批后名字表的代数落后，持有时按「名字表」重建。
  - `derived_stale_result_*`：核对任务计算期间输入又变了，这次结果没有装上，下一轮按新输入重算。
- 读取核对（`read_verify_*`）：原始字节的 OID 等于 `TextRef::oid` 时回答；有 `eol` 转换的文件，`canonical_oid` 等于它时回答；内容已经变了、映射里仍记着旧 OID 时返回 `Stale`；有外部过滤器的路径在原始 OID 不等时返回 `Stale`。
- 映射（`map_*`）：从带 OID 的文件列表建映射；前 8 字节相同的两个 OID 都能正确查到；覆盖项遮住 index 条目；修改后的旧 OID、删除的文件不出现在查询结果里；未跟踪文件可以查到。
- 覆盖（`overlay_*`）：`FakeGit` 构造 git 设计「工作区比较」表里的情况，覆盖项与工作区实际内容一致：未暂存的修改、`StagedOnly`（工作区等于基准）、未暂存的删除、未跟踪文件、重命名、`Filtered` 与超过 1 MiB 的文件为 `Unindexable`。
- 调度（`schedule_*`）：层的先后；第 1、2 层持有 `BusyGuard`、在 `Interactive` / `Background` 上运行，第 3–5 层持有 `BackgroundGuard`、在 `Batch` 上运行；暂停开关在批次边界生效；一次抽取正在运行时打开暂停开关，`ManualClock` 推进 50 ms 内这次抽取返回 `Cancelled`、它的结果没有写入、`indexed` 提示不变，恢复后这个文件先被重新抽取（`schedule_pause_abandons_in_flight`）；一批在 64 个文件、2 MiB 或 50 ms 之后的第一个文件边界处结束，一个 200 ms 的大文件让这一批超过 50 ms、但暂停仍在 50 ms 内打断它；切换 worktree 后游标重建、已写入的不重复抽取；同一 OID 在两个路径上只抽取一次；一个文件以 OID A 排队、读之前被改成 B 时，结果只写在 B 下，A 的键下没有单元，A 的提示不置位（`schedule_key_from_bytes_read`）；第 5 层只抽取缓存里没有、且工作区内容与 index 相同的文件。
- 语言包（`grammar_*`，用 `testing::FakeGrammars`）：`grammar_installed_*`：安装后新扩展名的跟踪文件和未跟踪文件进入映射并被抽取；`grammar_updated_*`：更新后旧版本的单元不再命中、这种语言的文件按新版本重新抽取，旧版本在途抽取的结果没有写入；`grammar_rolled_back_*`：回滚后之前那个版本写过的单元重新命中、不重新抽取；`grammar_removed_*`：撤销后这种语言的文件从映射和 `coverage` 里消失，查询返回 `UnsupportedLanguage`，在途抽取被取消、结果没有写入；`grammar_merged_*`：连续多个通知并成一轮，结果与逐个处理相同，过时的代数被丢弃；`grammar_lease_*`：`FakeGrammars` 记录模块的卸载，撤销或更新时有一个在途抽取和一个仍被查询持有的 `ParsedFile`，两者都结束之前模块没有被卸载，结束之后恰好卸载一次。
- 增量（`incremental_*`）：用 `FakeBackend` 驱动 `WorktreeBatch`，检查「增量更新」表里每个事件让对应的输入代数加一：修改一个文件后按新 OID 抽取、旧 OID 不再返回；目录事件请求一次比较；`rescan`；`meta.refs` 让 HEAD 代数加一；连续事件并成一轮；从后端交出事件到覆盖部分更新完之前 kernel 不静止。
- 热启动（`warm_*`）：缓存里已有全部单元时重新打开项目，抽取函数一次也没被调用（计数钩子），只有存在检查；删掉一部分单元后只抽取这一部分。
- 跳转（`goto_*`）：四步的顺序与停下的条件；唯一 `Bound` 候选给出 `jump`；唯一 `NameOnly` 候选只有 `preselect`；兼容组、可见性、语法种类的过滤（含 Go 的 `Meters(value)` 跳到 `type Meters`）；同一份字节在 `.java` 和 `.js` 下时 `java` 的单元只给出 `.java` 的路径（`goto_oid_language_match`）；分层与同层排序；审阅集合优先；结果只追加、已有下标不变；当前文本的 blob 已入索引时，第 1 步列出的本文件定义不在第 3 步重复出现，现场抽取过的 import 目标不在第 3 步和限时搜索里重复出现（`goto_no_duplicates_*`）；用测试里的 `Resolver` 实现检查第 2 步和 `Bound(Import)`；`NoSymbol`、`UnsupportedLanguage`；从对象库一侧（`TextOrigin::Object`）发起的查询，本文件的候选 `origin` 为 `Object`，由它发起的 `References` 读同一个 blob，不读工作区（`goto_object_origin_*`），也不解析工作区里同一路径的文件（`goto_object_side_parses_only_its_text`）；很多文件定义的名字，第一批里最近的候选排在最前、其余页之后追加、每个定义只出现一次（`goto_popular_name_nearest_first`），其余页在时间盒到时停下、没读的计入未搜（`goto_popular_name_pages_are_time_boxed`）；当前文本的解析保留之后，各个位置的答案与现场解析的相同、不再解析，内容变了就不命中（`goto_kept_parse_answers_as_the_cold_one`），份数、大小和 60 s 的上限（`goto_kept_parses_are_bounded`）。
- 预取与焦点（`prefetch_*`、`focus_*`）：映射换代或淘汰代数变化之后，2 秒内同样的查询不沿用预取结果；非当前 worktree 的焦点不进入第 1、2 层，切换到那个 worktree 后生效。
- 限时搜索（`search_*`）：只解析整词命中的文件；`ManualClock` 推进到 1 s 时停止并给出 `Partial` 的计数；命中文件的结果写入缓存；搜完时为 `Complete`；范围里有一个文件正在后台抽取时，搜完其余文件结果仍是 `Partial`、`unsearched` 至少为 1（`search_in_flight_partial`）。
- 依赖（`deps_*`，ADR 0033、0040，临时目录里的 `node_modules` 夹具，npm 和 pnpm（含 pnpm 工作区）两种布局都由测试自己生成，只用合成的 `package.json`、`package-lock.json` 和 `.d.ts`）：
  - 发现：直接依赖的四种字段、512 个的上限、不合规的名字被跳过；从子项目往上找 `node_modules`；`exports` 的 `types` 条件（嵌套在 `import` / `require` 之下）、`types`、`typings`、`index.d.ts`、`main` 的顺序；`@types/<名字>` 和 `@types/scope__x`；闭包沿相对 import、`export * from`、`/// <reference path>` 走，裸模块名不跟随（`deps_entry_*`、`deps_closure_*`）。
  - 读取限制：`node_modules` 本身是指向根外的符号链接时打不开（`deps_root_symlink_is_not_a_root`）；`"types": "../../.ssh/id_ed25519"` 和 `../` 越过包目录的 import 被丢弃，`SafeRoot` 的读取记录里没有根外的路径（`deps_types_outside_the_package_is_dropped`）；pnpm 的链接在根内解析（`deps_pnpm_links_resolve_inside_the_root`）；工作区子项目的 `packages/app/node_modules/react` 指向根目录 `node_modules/.pnpm/…`，那是同一条查找链上已发现的根，经那个根的 `SafeRoot` 读到（`deps_link_into_a_discovered_root_is_read`）；指向任何已发现的根之外（`$HOME`、worktree 之外、worktree 里没有作为根打开的目录、另一个 worktree 的根）的链接不读，`SafeRoot` 的读取记录里没有那些路径（`deps_link_outside_the_discovered_roots_is_rejected`）；扩展名不在列表里的文件没有被打开（`deps_reads_only_allowed_extensions`）；工作区之外的根不从项目文件取路径（`deps_cache_roots_come_from_the_environment`，`Roots` 用测试给的环境）。
  - 身份与 stat 表：身份不变时重新打开项目不读、不 stat 闭包里的文件（`SafeRoot` 的调用计数，`deps_same_identity_reads_nothing`）；`integrity` 变了时只重读五项 stat 变了的文件；修改时间被设回原值、状态改变时间变了的文件被重读（`deps_ctime_defeats_a_restored_mtime`）；记录前 2 秒内改过的文件不走捷径；`pnpm-lock.yaml` 的项目按清单的 OID 和包目录取身份；`package-lock.json` 已改成新版本、`node_modules` 还没重新安装时，已安装的文件照常沿用；之后重新安装（新的 `package.json` 的 inode）时，`integrity` 虽与路径表记下的相同，内容变了的文件仍被重读（`deps_lockfile_ahead_of_install_is_not_the_identity`）；`package-lock.json` 按 1 MiB 的区间流式读，16 MiB 的 lockfile 的活跃字节峰值 ≤ 2 MiB（`deps_lockfile_read_is_streamed`）。
  - 出边与路径表：版本号变了、内容没变的包（单元都已在缓存里）走完整个闭包，路径表与第一次的相同（`deps_closure_from_stored_specifiers`）；跳转先按需写了入口的单元之后，依赖层仍走完闭包（`deps_closure_after_an_on_demand_entry`）；路径表还在、它引用的单元被淘汰后，打开项目时缺的文件被排进依赖层、补齐之后第 3 步又能找到这些定义（`deps_table_without_units_is_repaired`）。
  - 根的有效性：会话中删掉重建 `node_modules`（新的 inode）后，下一次身份任务或读取 `NotFound` 时重新打开，读到新的内容（`deps_reinstalled_root_is_reopened`）；切换当前 worktree 后，另一个 worktree 的根不再被读（`deps_roots_are_per_worktree`）。
  - 限额：每包 4000 个文件和 16 MiB、单个文件 1 MiB、每个项目 64 MiB，到了之后依赖层停下、`coverage.deps` 为 `Done`（`deps_limits_*`）。
  - 存储：依赖的单元和路径表在 `code_intel.deps`，配额小于数据量时只淘汰依赖的单元，`code_intel.tags` 的单元一个不少（`deps_quota_evicts_only_dependency_units`）；依赖层自己的写入引起淘汰后在本次会话停下。
  - 查询：第 3 步给出 `Dependency` 层的候选、排在项目的各层之后，`Location::origin` 带包名和版本（`deps_candidates_come_last`）；名字表和 `Named` 里没有依赖的名字（`deps_not_in_names`）；依赖层没完成时跳转仍可以是 `Complete`，`unsearched`、`outside` 不计依赖（`deps_do_not_count_in_completeness`）；读取核对：改过的依赖文件当场重新抽取，`TextRef` 指向改过的依赖文件时返回 `Stale`（`deps_read_verify_*`）；测试 `Resolver` 交出的裸模块名经 `ResolveContext::dependency` 得到 `Bound(Import)`（`deps_on_demand_import`）。
- 让路（`yield_*`，ADR 0033，kernel 的 `TestKernel` 和 `Kernel::set_clock` 的 `ManualClock`）：开关打开时第 4 层、依赖层、第 5 层在下一个分片边界停下，第 3 层和窗口链照常（`yield_tier3_does_not_yield`）；在途的 1 MiB 文件停下再继续，单元与不停时逐字节相同（核心语言每种一个，`yield_resumed_extraction_is_identical`）；语言包的抽取被放弃、之后重做；停下期间不计截止时间，停 1 s 之后不 `TimedOut`（`yield_parked_time_is_not_deadline_time`）；超过 256 KiB 的文件不进入时段、开关关闭后继续；等时段时来了第 3 层的工作，它不等时段（`yield_tier3_work_does_not_wait_for_a_slot`）；暂停优先：停下的状态被丢掉、文件放回小队列；第 3 层和第 4 层都有工作、开关打开着时打开暂停，kernel 的后台状态在批次边界之内变为「全部已暂停」（每个批量任务只持有一个守卫，`yield_pause_reaches_all_paused`）；超过 256 KiB 的一项停下拿着 `LargeSlot` 时，第 3 层一个超过 256 KiB 的文件和窗口链的窗口不等开关关闭：停下的状态被丢掉、`LargeSlot` 交给它们，活跃字节里没有两棵大树同时存在（`yield_large_parked_item_gives_way_to_tier3`）；开关反复切换 100 次，每个文件只写入一次。
- 大文件（`window_*`、`goto_large_*`，ADR 0034，合成的 2–16 MiB 文件，每种核心语言一个，另有一个顶层节点跨过 1 MiB 的文件、一个整个包在 IIFE 里的 UMD 文件和一个压缩成一行的 JS）：窗口链合并的定义和引用名与整份抽取的相同（顶层节点不跨窗口的文件逐项相同，`window_chain_equals_whole_extraction`；整个包在 IIFE 里的文件，函数体里是长短不一的 `const <名字> = …` 和调用，种子固定、让许多窗口的末尾落在标识符中间：合并后的定义名和引用名的多重集与整份抽取的相同，没有半截的名字，`window_chain_equals_whole_extraction_in_an_iife`）；窗口末尾正好截在 `const foo = 1` 的 `const fo` 处时，这个按自动分号补全的声明和 `fo` 都不算完整，切在前一个语句的终点，下一个窗口从 `const foo` 开始（`window_cut_skips_a_node_ending_at_the_window_end`，叶子的后备同样检查一次）；IIFE、UMD 和一行的 JS 也在函数体里按树切开，每个窗口 ≤ 64 KiB、没有窗口扩到 256 KiB（`window_cut_descends_into_the_truncated_node`）；只有一个超过 256 KiB 的字符串字面量的文件按叶子或标识符边界切开，任何窗口的边界都不在标识符中间（`window_cut_never_splits_an_identifier`）；行号和字节偏移是在整个文件里的；`Windowed` 替换 `TooLarge` 的那次事务算作本 crate 自己的淘汰、提示不因它全部失效（`window_marker_replace_is_own_eviction`）；打开过的大文件的名字在名字表里（`window_units_are_in_names`）；每个窗口完成即写入，暂停之后从最后一个窗口的终点续接、不重抽已写入的窗口，被淘汰的窗口被补上（`window_chain_resumes_after_pause`、`window_chain_fills_evicted_windows`）；链完成时写 `Windowed`、提示置位，之后这个文件算已入索引；读到一半文件被改（身份变了）时放弃、不以旧 OID 写入后面的窗口（`window_chain_aborts_on_identity_change`）；跳转第 1 步只读两段、只解析窗口（`SafeRoot` 和解析的计数），8 ms 超时时按标识符字符取名字（`goto_large_window_only`、`goto_large_timeout_falls_back_to_the_name`）；身份没记下时流式读一次、之后不再读，读的时候活跃字节里只有一个 1 MiB 的区间（`goto_large_cold_then_warm`）；对象库一侧的大文本，位置在保留的窗口解析里时不调用 `GitAccess::blob`，不在时调用一次（`goto_large_object_reuses_the_kept_window`）。
- 字节偏移（`location_byte_*`）：每个候选和引用的 `byte` 处的字节就是名字的第一个字节（工作区、对象库、依赖、窗口单元各一组）；CRLF 的工作区文件经 `canonical_oid` 对上时为 None；版本 2 的表在第一次使用时删除重建。
- 批量核对（`canonical_batch_*`，`FakeGit` 记录调用）：一遍重新读的 300 个 CRLF 文件只调用两次 `canonical_oids`（每批 ≤ 256 项）、没有调用 `canonical_oid`；一批里一项出错时逐项重试、其余照常；交互路径上仍是一次 `canonical_oid`。
- 引用（`refs_*`）：局部变量和参数的 `References` 找到它在作用域里的全部用处（这些位置只在 `@local.reference` 里，不在 tags 引用和 `ci_ref` 里），不查 `ci_ref`、没有 `same_name`；locals 记为程序作用域局部定义的顶层函数，它的同文件引用也被找到；同时被 tags 和 locals 捕获的导出顶层函数（根作用域）`local` 为 false，另一个文件里对它的引用经 `ci_ref` 找到、未入索引的文件经限时搜索找到（`refs_toplevel_captured_by_locals`）；函数体里嵌套的 `const helper = () => …` 同时被两者捕获，`local` 为 true，引用只在外层函数的作用域里找，其他文件里同名的 `helper` 不出现（`refs_nested_helper_is_local`）；locals 证明的同文件引用是调用方；同一处引用、同一个文件在多个来源里出现时只列一次、只解析一次（`refs_no_duplicates_*`）；被局部变量遮住的同名引用被丢弃；没有依据的跨文件引用在 `same_name`；测试 `Resolver` 的 `bind` 让跨文件引用成为调用方；未入索引的部分经限时搜索找到；`ci_ref` 一轮找到的在读搜索范围之前交出，限时搜索命中之后的结果按计时交出、不等下一个命中（`refs_indexed_references_go_out_before_the_search`、`refs_search_batches_go_out_on_time`）；被查定义的局部性取自它所在文件的那一次解析，`Named` 列出候选时不解析文件（`refs_nested_helper_from_named_is_local`、`refs_named_parses_no_file`）。
- 名字表（`names_*`）：去重、NFC、只含当前 worktree 映射里的 OID（另一个 worktree 独有的名字不在其中）；第一次 `names` 返回 `Building`，`Names` 事件带着表，监听方保存后再调 `names` 返回 `Ready`、是同一个 `Arc`（`names_ready_after_build`）；构建期间重复调用不另起构建；代数落后时重建、期间返回旧表；所有持有者释放后再调用重新构建；表被持有时索引写入新的批次，`ManualClock` 推进过间隔后后台重建并发出带新 `Arc` 的 `Names`，间隔内的多次变化只触发一次重建，持有者释放后不再重建（`names_refresh_while_held`）。
- 升级（`escalate_*`）：Java 多候选和唯一 `NameOnly` 候选时调用提供者，其他语言不调用；提供者的 token 为 `Dead` 时不调用；结果相同时 `Same`、不同时 `Ready`；`head_only` 的提供者只在 `head_clean` 时被调用，返回的位置里内容与 HEAD 不同的文件被丢掉；只暂存、未提交的文件（工作区等于 index、不等于 HEAD）的 `head_clean` 为 false，提交之后为 true；index 变化之后、新的工作区比较完成之前为 false（`head_dirty_*`）；登记 drop 后不再调用；SCIP 与 jdtls 两个假提供者同时处理 Java 时，按两种回复顺序得到相同的合并结果，HEAD 相同的文件取 SCIP 的位置、有改动的文件取 jdtls 的位置，过时 ID 的回复被丢弃，查询关闭时未回复的请求收到 `cancel`；SCIP 先回复、已知候选都与 HEAD 相同时，jdtls 的请求不被取消，它之后返回的未跟踪文件里的位置进入合并结果（`escalate_arbitration_*`）；提供者接受请求后注销，只有它在请求中的查询 `semantic` 变为 `Failed`、不再停在 `Pending`，另有 SCIP 回复的查询按剩下的回复重新合并，注销之后到达的回复被丢弃（`escalate_unregister_*`）。
- 插件（`plugin_*`）：用真实的 `Kernel`（kernel 的 `test-support`）和 `Executor::manual()`；`activate` 之后、第一个项目打开之前没有任何 IO（`FakeGit`、缓存目录、`SafeRoot` 都没有被调用）；依赖 `dyn Projects`；`Results` 事件的顺序与 generation；一次查询从提交到最后一批写入之前 kernel 不静止；停用之后语言包的订阅已退订，fs_watch 的订阅都已 drop，项目关闭时未回复的语义请求收到 `cancel`（`plugin_teardown_*`）。

## 未决问题

没有需要产品决定的问题。原来的三条（调研「[决定](../research/code-navigation.md#决定)」第 4–6 条里影响本 crate 的部分）已于 2026-10-18 决定：

1. **首发语言范围。** 选 (a)：符号抽取覆盖 ADR 0018 核心里的 TypeScript / JavaScript（含 JSX、TSX）、Rust、Java、Python、Go，第 1 步已按此实现；按 import 解析先做 Java 和 TS（#18、#19），其余语言只按同文件、同目录、同子项目分层。不采用只做 Java 与 TS / JS 的 (b)。产品负责人交给负责推进的 agent 决定，按默认值。
2. **索引没建完时是否提示。** 选 (a)：不提示，符合「界面不显示索引状态」的规则。不采用在结果末尾显示「仍在搜索」的 (b)。`QueryState::Done(Completeness)` 照常给出完整性，本 crate 不随这个决定改变。产品负责人交给负责推进的 agent 决定，按默认值。
3. **电池供电时的索引范围。** 选 (b)：不区分电源，始终索引整个仓库；省电模式暂不做。产品负责人决定，记录在 [ADR 0032](../adr/0032.md)；本文改动见「建索引的分层」的电源一条，第 3 步的电池代码已删除（「与设计的差异」）。

ADR 0033、0034 里影响用户看到的内容的几条，产品负责人已交给负责推进的 agent 决定（[大文件与渲染调研「决定」](../research/large-files-rendering.md#决定)）：⌘K 的符号搜索不包含依赖（名字表和 `Named` 不查 `code_intel.deps`）；依赖候选的次要文字显示包名和版本（`DepOrigin`，呈现归 #20）。依赖层、交互让路、大文件和批量核对的技术选择都在「依赖层」「交互让路」「大文件」「规范内容 OID 的批量核对」里决定，没有新增未决项。

技术问题都已在上文决定，没有其他未决项。「实际验证」里一度留给设计决定的一项（当前文本的解析与 locals 超出原来 ≤ 10 ms 的分项）已由 [ADR 0037](../adr/0037.md) 决定：份额按第一批计，当前文本的解析按 blob OID 保留，不再有单独的解析分项（「性能」）。

## 验收

前置条件（其他 crate 的 PR，都写 `Refs #17`，见「对其他 crate 的要求」）：

- A. `services`：`code_intel` 模块；`project` 模块的 `BlobId` 和 `FileList::oid`。`cargo check -p lectern-services` 通过，`rg -n 'pub trait CodeIntel' crates/services/src` 有一处，检查 `dyn CodeIntel` 的 `ID == "code-intel"`、`MULTIPLE == false` 的测试通过。
- B. `project`：文件列表带 OID，OID 变化时文件列表换代，`plugin/project/resident_*` 的份额已更新。
- C. `cache`：「自定义表」的设计与实现。
- D. `kernel` 的 `BackgroundGuard`、`executor` 的 `Priority::Batch`（ADR 0029）和 `Executor::on_shutdown`。
- E. `git`：`Worktree::canonical_oid`。

本 crate 分九步实现，每步一个 PR；第 6–9 步是 ADR 0033、0034 带来的工作，第 5 步之前或之后都可以做（第 9 步的按需解析部分等 #19）。最后完成的一步写 `Closes #17`，其余写 `Refs #17`。每一步都要 `just check`、`just test -p lectern-code-intel`、`just deny` 通过，并把这一步加入的 bench 条目在 `crates/bench/src/budgets.rs` 里加上、改为生效（[bench 设计「预算表」](bench.md#预算表)已列为计划中）。

1. **语言注册与抽取**（不需要前置条件）：根 `Cargo.toml` 按 ADR 0030 加入 `tree-sitter`、`tree-sitter-tags` 和首发语言的 grammar crate，加入查询文件和夹具。
   - `lang_*`（含 `lang_grammar_versions_match_lock`）、`extract_*`、`deadline_*`、`site_*` 测试通过，每种首发语言至少一份 insta 快照。
   - `cargo tree -e features -i tree-sitter` 里没有 `wasm`。
   - `cargo tree -i tree-sitter-tags -e normal --depth 1` 只列出 `lectern-code-intel`。
   - `code_intel/extract_throughput` 在预算内。
2. **存储**（前置 C）：
   - `store_*` 测试通过（含读取淘汰代数）；`cargo tree -i rusqlite -e normal --depth 1` 仍只列出 `lectern-cache`。
   - `code_intel/warm_100k`（存在检查部分）在预算内。
3. **映射、调度与增量更新**（前置 B、D）：
   - `map_*`、`derived_*`、`overlay_*`、`schedule_*`、`grammar_*`、`incremental_*`、`warm_*` 测试通过（第 3 步时还有 `battery_*`，已按 ADR 0032 删除）。
   - `rg -n 'std::fs::|File::open' crates/code_intel/src --glob '!**/tests*' --glob '!**/testing*'` 没有结果：工作区文件只经 `SafeRoot` 读取。
   - `rg -n 'Command::new|thread::spawn' crates/code_intel/src` 没有结果。
   - `code_intel/map_100k`、`code_intel/map_100k_bytes`、`code_intel/batch_peak` 在预算内。
   - `code_intel/disk_100k`（只记录）用 10 万文件的合成仓库测量，结果写进「存储表」的「规模」一条。
4. **查询与插件**（前置 A、E）：`Cargo.toml` 加 `plugin = true`。
   - `goto_*`、`read_verify_*`、`search_*`、`refs_*`、`cut_*`、`view_*`、`account_*`、`names_*`、`escalate_*`、`head_dirty_*`、`prefetch_*`、`focus_*`、`plugin_*`、`watchdog_*` 测试通过。
   - `python3 scripts/check_architecture.py` 通过：本 crate 不依赖 GPUI、不依赖其他插件 crate，除 `app` 和 tool 层外没有 crate 依赖它。
   - `code_intel/goto_indexed`、`code_intel/goto_uncached`、`code_intel/goto_popular`、`code_intel/goto_full`（含 `goto_full/done`）、`code_intel/goto_cold_parse`（96 KiB，≤ 32 ms）、`code_intel/search_stop`、`code_intel/references_100k`、`code_intel/references_full`（`/all` ≤ 2 s、`/first` ≤ 200 ms）、`code_intel/parse_cache_bytes`、`code_intel/names_100k`、`code_intel/names_100k_bytes`、`plugin/code-intel/resident_cloudprint`、`plugin/code-intel/resident_100k` 在预算内。
5. **端到端基准**（前置 #20 的抽屉和 `file_reader` 在探针里的里程碑）：bench 探针增加打开抽屉、跳转、覆盖层完成的 `Milestone`，以及读取 `coverage` 的方式。
   - `app/goto_candidates`、`app/goto_single`、`app/goto_unindexed`、`app/goto_unindexed_done`、`app/index_review_set`、`app/index_subproject`、`app/index_100k`、`app/idle_monorepo_indexed` 在 `crates/bench/src/budgets.rs` 里改为生效，macOS CI 上在预算内。
   - `app/idle_monorepo` 在后台工作暂停时仍 < 100 MB。
6. **存储版本 3、字节偏移与批量核对**（不需要新的前置条件：git 的 `canonical_oids` 和字节偏移已实现）：命名空间版本 3、配额 224 MiB，`Location::byte`（连同 `services` 的字段），`GitAccess::canonical_oids` 和三处整批调用。
   - `location_byte_*`、`canonical_batch_*` 测试通过，原有的 `store_*`、`overlay_*`、`account_*` 照常通过（满额模式的夹具按 224 MiB 改写）。
   - `canonical_batch_*` 用 `FakeGit` 的调用记录检查：重新读、批量层、查询结果的写入都不调用单个的 `canonical_oid`。
   - `code_intel/reread_crlf_1000`（只记录）有结果；`code_intel/disk_100k` 照常只记录，`goto_full`、`references_full` 仍在预算内。
7. **交互让路**（前置：kernel 的让路开关，[#123](https://github.com/SoloJiang/lectern-app/pull/123) 已实现）：两种守卫、`proceed_item`、可恢复的抽取。
   - `yield_*` 测试通过；`schedule_*`、`deadline_*` 照常通过。
   - `code_intel/yield_stop` ≤ 10 ms，`code_intel/batch_peak` 仍 ≤ 40 MB。
8. **大文件**（前置：`platform` 的范围读取，#126 已实现；`git` 的流式 blob 哈希 `BlobHasher`，见「对其他 crate 的要求」）：窗口抽取与窗口链、`Windowed`、跳转第 1 步的窗口解析、当前文本的身份与检查点。
   - `window_*`、`goto_large_*` 测试通过。
   - `rg -n 'std::fs::|File::open' crates/code_intel/src --glob '!**/tests*' --glob '!**/testing*'` 仍没有结果：大文件的读取也只经 `SafeRoot::open_file` 和 `OpenFile`。
   - `code_intel/goto_large` ≤ 20 ms，`code_intel/window_peak` ≤ 8 MiB，`code_intel/batch_peak` 仍 ≤ 40 MB；`code_intel/goto_large/cold`、`code_intel/goto_large/object`、`code_intel/window_chain_16mib` 有结果。
9. **依赖层**（前置：`services` 的依赖类型、`Coverage::deps`、`CandidateTier::Dependency`；`platform` 的 `SafeRoot::open_dir` 已实现；链接按 [ADR 0040](../adr/0040.md)；按需解析的部分另需 #19 的 TS 解析扩展点）：发现、身份与路径表、`cd_spec`、依赖映射、依赖层、`code_intel.deps`、查询里的依赖层。
   - `deps_*` 测试通过；`rg -n 'open_dir' crates/code_intel/src --glob '!**/tests*'` 只在 `deps` 模块里（读取限制只有一个入口）。
   - `code_intel/deps_identity` ≤ 50 ms、`code_intel/deps_map_bytes` ≤ 1 MB；`code_intel/deps_ts_project` 有结果；`app/index_100k` 的夹具含达到依赖层上限的 `node_modules`（bench 设计）之后，macOS CI 上仍 ≤ 120 s。

## 实际验证

第 1 步，在 Linux（x86_64，4 核 Xeon 2.1 GHz，内核 6.18）上运行。本 crate 不依赖 GPUI，也没有 macOS 专属代码；macOS 上的结果由 macOS CI 给出。

- `just check`、`just test`（rebase 到包含 #108、#109、#110 的 main 之后，整个工作区 1112 个测试通过，跳过 1 个）、`just doctest`、`just deny`（advisories、bans、licenses、sources 都通过）全部通过。
- `just test -p lectern-code-intel`：35 个测试通过。按前缀：`lang_*` 9（含 `lang_grammar_versions_match_lock`）、`extract_*` 10、`deadline_*` 8、`site_*` 8。insta 快照 8 个，每种首发语言至少一个（`ts`、`tsx`、`js` 两个、`rust`、`java`、`python`、`go`），逐个看过。`site_parsed_defs_match_extraction` 检查 `ParsedFile` 自己跑查询得到的定义与 tree-sitter-tags 的抽取结果逐条相同。
- `deadline_*` 用真实的看门狗线程（`testing::watchdog` 在 `Executor::manual()` 的专用线程上运行它）：1 MiB 的病态输入（2000 层嵌套的数组、一行 1 MiB）在 1 ms 截止时间下返回 `TimedOut`，用时 < 500 ms；解析途中置位取消标志、暂停探针返回 true、丢弃 `stop_guard()` 都让 4 MiB 的病态输入返回 `Cancelled`，`stop` 之后线程退出、之后的抽取立即取消、不再启动线程；`ManualClock` 不推进时看门狗检查但不置位，推进过截止时刻后置位；没有在途抽取时看门狗的循环计数在 100 ms 内不变。
- `cargo tree -e features -i tree-sitter` 里没有 `wasm`（只有 `default` 和 `std`，`default` 由 tree-sitter-tags 打开）；`cargo tree -i tree-sitter-tags -e normal --depth 1` 只列出 `lectern-code-intel`。Cargo.lock 新增 `tree-sitter` 0.27.1、`tree-sitter-tags` 0.27.1、`tree-sitter-language` 0.1.9、`streaming-iterator` 0.1.9 和六个 grammar crate（`tree-sitter-typescript` 0.23.2、`tree-sitter-javascript` 0.25.0、`tree-sitter-rust` 0.24.2、`tree-sitter-java` 0.23.5、`tree-sitter-python` 0.25.0、`tree-sitter-go` 0.25.0），都是 MIT（`streaming-iterator` 为 MIT OR Apache-2.0），`just deny` 通过；已有依赖的版本没有变化。六个 grammar 都是各自在 crates.io 上的最新版本，ABI 是 14（typescript、java）或 15，在 tree-sitter 0.27.1 支持的 13–15 之内。
- `code_intel/extract_throughput`（`just bench --measure code_intel/`，三次）：中位数 2.08–2.29 MB/s（MB 为 2^20 字节），在这台机器上低于 4 MB/s 的预算；Linux 上不判定，M 系列上没有测量，由 macOS CI 判定；预算保持设计的 4 MB/s，没有因 Linux 的结果调低。同一台机器（4 核 Xeon 2.1 GHz）上的拆分（release，每种语言约 1 MiB 夹具）：只解析 4.4–5.5 MiB/s；tags 抽取 TS 2.0、TSX 2.5–2.6、JS 2.0、Rust 3.0 MiB/s；上游的 Rust tags 查询（不带 locals）3.5 MiB/s；去掉 locals 查询快约 15%–30%，去掉 `@local.reference` 的模式没有变化。吞吐主要受解析速度限制，抽取的查询匹配约占一半。调研在同类 Xeon 上测得 4.3 MB/s，预算按「M 系列可能快一倍」估算，M 系列上的结果要看 macOS CI。
- 安装包体积（ADR 0018 的 80 MB）：在 Linux x86_64 上把 tree-sitter 运行时、tree-sitter-tags 和六个 grammar 链接进一个 release 二进制（thin LTO、strip），与只链接运行时的二进制相比多 6.9 MiB（7.27 MiB 对 0.34 MiB），gzip -9 之后多 1.2 MB（DMG 的 40 MB 目标）。各 grammar 的代码与数据：`tsx` 1.44 MB、`ts` 1.41 MB、`rust` 1.11 MB、`python` 0.46 MB、`java` 0.41 MB、`js` 0.41 MB、`go` 0.21 MB，运行时 0.21 MB，其余是 tags、`regex` 和绑定代码。解析表是数据，arm64 上的大小应当相近；macOS 上没有测量，正式数字由 #38 的 `app/bundle_size` 给出。「未决问题」1 选 (b) 时少 Rust、Python、Go 三个 grammar，约 1.8 MB。
- 没有验证的：M 系列上的吞吐；macOS 上的体积；`watchdog_*`（看门狗随插件停用退出、激活 20 次后最多一个线程，属于第 4 步；`Executor::shutdown` 让看门狗退出已由 `deadline_executor_shutdown_stops_the_watchdog` 经 `testing::watchdog` 检查）。

第 1 步，处理 [#113](https://github.com/SoloJiang/lectern-app/pull/113) 的审阅意见之后，同一台机器。

- `just test -p lectern-code-intel`：39 个测试通过。按前缀：`lang_*` 9、`extract_*` 11、`deadline_*` 8、`site_*` 11。新增 `site_member_values_accept_every_kind`、`site_nested_declarations_bind_locally`（Python 的 `def` 和 `class`、JS 的函数、Rust 块里的 `fn`、Java 的局部类）、`site_member_names_do_not_bind_locals`（Java 的 `items.add` 与 `this.add`、Python 的 `obj.x`、Rust 的 `Queue::new`）、`extract_plain_and_member_uses_are_names`（六种语言）。8 个快照的引用名都变了（兜底模式加入的普通标识符和成员名，以及 `new Store()` 现在解析到本文件的类声明、不再进入 JS 的引用名），逐个看过。
- `code_intel/extract_throughput`（`just bench --measure code_intel/`，每项三次，MB 为 2^20 字节）：

  | 版本 | 语料 | 中位数 |
  |---|---|---|
  | 审阅前（3de4dbc） | 每种语言一个 1 MiB 的文件 | 2.09–2.22 MB/s |
  | 加入兜底引用模式之后 | 同上 | 1.49–1.52 MB/s |
  | 同上 | 每种语言约 1 MiB，切成约 8 KiB 的文件（ADR 0031） | 2.53–2.58 MB/s |
  | 同上（`code_intel/extract_1mib`，只记录） | 每种语言一个 1 MiB 的文件 | 1.63–1.65 MB/s |

  macOS CI 上审阅前的版本是 3.7 和 3.49 MB/s，约为这台机器的 1.6–1.7 倍；按这个比例，仓库形状的语料在 M 系列上估计 4.0–4.4 MB/s，由 macOS CI 判定。
- 拆分（callgrind，TS，256 KiB，指令数）：解析约 38%，查询匹配约 40%，tree-sitter-tags 逐个 tag 的处理约 13%，本 crate 的 `finish` 约 5.6%，其余是树的释放和内存分配。同一语料按文件大小：16 KiB、64 KiB、256 KiB、1 MiB 的文件分别约 2.9、2.9、2.5、1.7 MiB/s。1 MiB 的 JS 文件里，tree-sitter-tags 的局部检查（内联进 `extract` 的代码和它的 `memcmp`）约占 45%；64 KiB 时约 2%。
- 试过、没有保留的改动（交替运行两个二进制，每项三次）：去掉 locals 查询的 `(identifier) @local.reference`，1 MiB 时 1.69 对 1.67–1.69 MiB/s，没有变化；引用名去重后再分配字符串，1 MiB 时 1.65–1.67 对 1.57–1.59，8 KiB 的文件 2.54–2.65 对 2.56–2.64，没有收益；兜底模式不带 `#is-not? local`，1 MiB 时 1.69 对 2.12，8 KiB 时没有变化，但局部变量名会进入引用名；函数级作用域标成 `local.scope-inherits false`，1 MiB 时 1.68–1.72 对 1.99–2.02，8 KiB 时 2.55–2.66 对 2.62–2.63，但改变了被过滤的引用。解析器、查询游标和 `TagsContext` 本来就按线程复用，locals 和 tags 本来就是一次匹配。

第 1 步，处理 [#113](https://github.com/SoloJiang/lectern-app/pull/113) 第二轮审阅意见（绑定规则、租约的 drop 顺序、枚举成员）之后，同一台机器。

- `just check`、`just test`（整个工作区 1145 个测试通过，跳过 1 个）、`just doctest`、`just deny` 全部通过。
- `just test -p lectern-code-intel`：47 个测试通过。按前缀：`lang_*` 10、`extract_*` 12、`deadline_*` 8、`site_*` 17。新增 `site_binding_javascript`、`site_binding_typescript`、`site_binding_python`、`site_binding_rust`、`site_binding_java`、`site_binding_go`（「绑定规则」的表驱动测试，共 58 行）、`extract_enum_members_are_definitions`（Rust、TS、Java、Python）、`lang_lease_outlives_grammar_backed_values`。7 个快照有变化，逐个看过：新增 Rust 的变体、TS 的枚举成员、Python dataclass 的类属性三类定义（枚举成员的名字随之不再作为引用名出现，`Inch`、`Closed`）；引用名多出原来被过滤掉的名字（解构出的名字、外层函数的局部定义、方法里的 `self`、Python 类体外的模块名、Go 的包级 `ErrRange`），都是保守方向的变化。
- `code_intel/extract_throughput`（交替运行这一版和 99fd327，每项三次，MB 为 2^20 字节）：

  | 版本 | 仓库形状（8 KiB 的文件） | `extract_1mib` |
  |---|---|---|
  | 99fd327 | 2.40–2.62 MB/s | 1.59–1.69 MB/s |
  | 本轮第一版（`if`、`while` 都是作用域） | 2.31–2.43 MB/s | 1.73–1.83 MB/s |
  | 本轮（`if let` / `while let` 用 `local.until` 限定范围，`if`、`while` 不是作用域） | 2.40–2.50 MB/s | 1.88–1.91 MB/s |

  第一版的下降集中在 Rust（按语言拆分，Rust 2.27–2.50 对 2.60–2.69 MiB/s，Go 约 4%，其余在噪声内）：tree-sitter-tags 对每个要过滤的引用检查已经出现的全部作用域，每个 `if`、`while` 多一个作用域。去掉这两种作用域后 Rust 回到 2.60–2.70 MiB/s；只把条件里有 `let` 的 `if` 记为作用域是 2.50–2.59 MiB/s（带字段和选择的模式本身的匹配也有开销），所以改用 `local.until`。`extract_1mib` 快 12%–18%：函数作用域标成 `local.scope-inherits false` 后，局部检查只扫描所在函数的作用域。

第 1 步，按协调方的要求重新评估 `code_intel/extract_throughput`（macOS CI 上同一份抽取代码测得 ≥ 4 MB/s 和 3.11–3.19 MB/s，[ADR 0031](../adr/0031.md)）之后，同一台机器。

- 各部分的开销（release，8 KiB 的文件，每种语言约 1 MiB，七种语言的调和平均，MiB/s，每项两到三次）：

  | 做法 | 吞吐 |
  |---|---|
  | 只解析 | 5.34 |
  | 解析后遍历语法树、收集名字 | 4.41 |
  | 解析后跑一遍 locals + tags 查询，不做任何处理 | 2.94 |
  | 同上，去掉全部兜底模式和 `(identifier) @local.reference` | 与上一行没有可测的差别 |
  | 自己遍历 `QueryCursor` 的匹配（作用域栈过滤局部引用、名字去重，不含可见性规则）| 2.83–2.88 |
  | tree-sitter-tags 抽取（a489f58） | 2.39 |
  | Go 的一次遍历原型 | Go 3.79–4.21（同一轮 tree-sitter-tags 2.43） |

  查询游标关掉全部模式时只遍历语法树，每 MiB 就要 62–94 ms，占查询时间（104–194 ms）的一半到三分之二，所以减少模式的上限很低。
- 改用一次遍历之后（`just bench --measure code_intel/`，与 99fd327 交替运行，每项三次）：`code_intel/extract_throughput` 3.86–3.97 MB/s（99fd327：2.48–2.54），`code_intel/extract_1mib` 3.19–3.25 MB/s（1.59–1.64）。按语言（MiB/s）：TS 3.2–3.5、TSX 3.8–4.0、JS 3.3–3.7、Rust 3.7–4.1、Java 4.2–4.7、Python 3.2–4.0、Go 3.5–3.7。参照名字集合换成不带密钥的快速哈希、遍历跳过匿名节点，两项都在噪声以内。
- `just check`、`just test`（整个工作区 1147 个测试通过，跳过 1 个）、`just doctest`、`just deny` 全部通过；`cargo tree -e features -i tree-sitter` 里仍没有 `wasm`，`cargo tree -i tree-sitter-tags -e normal --depth 1` 仍只列出 `lectern-code-intel`，没有新增依赖。
- `just test -p lectern-code-intel`：49 个测试通过。新增 `extract_walker_matches_tags`（7 份夹具、7 段逐个模式的样例、7 份 64 KiB 的语料、病态输入，两条路径的定义和引用名逐条比较，只允许上面那一处 Go 的差异）、`extract_definitions_on_one_node_are_siblings`。8 个快照没有变化：遍历在夹具上与 tree-sitter-tags 逐字节相同。`deadline_*` 现在经过遍历的路径，结果不变。

第 1 步，处理 [#113](https://github.com/SoloJiang/lectern-app/pull/113) 第三轮审阅意见（定义形式、作用域入口、语法种类表、`ParsedFile` 的截止时间）之后，同一台机器。

- `just check`、`just test`（整个工作区 1158 个测试通过，跳过 1 个）、`just doctest`、`just deny` 全部通过。
- `just test -p lectern-code-intel`：60 个测试通过。按前缀：`lang_*` 10、`extract_*` 14、`deadline_*` 11、`site_*` 20、`forms_*` 5。新增：
  - `forms_cover_grammar`：七种语言的 grammar 里带命名字段的节点种类都在「定义形式」的表里（182 行），表里的种类都在 grammar 里；`forms_have_no_duplicate_rows`；`forms_rows_have_samples`、`forms_samples_extract_and_bind`：151 个样例，覆盖每个定义行的每个种类和每个绑定行，定义样例在两条抽取路径上结果相同；`forms_table_matches_design`。
  - `site_binding_entry_points`（Python 的默认值、标注、返回类型、基类、推导式，JS / TS 的计算方法名、参数默认值与 `var`、TS 的参数装饰器）、`site_binding_namespaces`（TS、Rust、Java、Go 的类型参数，值与类型的区分，Java 记录组件不是局部）；绑定的表驱动测试共 89 行。
  - `site_rust_variants_are_reachable`（元组变体的调用、记录变体的构造和匹配都接受 `Constant`）；`site_kind_table` 改为逐行检查推出的语法种类表。
  - `deadline_walk_checks_stop_while_walking`、`deadline_parsed_file_checks_every_phase`、`deadline_parsed_file_walk_reads_the_deadline`：同一份 Java 源码作为核心语言和作为语言包解析，读时钟的次数之差正好是遍历的检查次数（修改之前是 0）。
  - 2 个快照有变化，逐个看过：TS 的参数属性 `constructor(public name: string)` 和 Java 记录 `Item(String name, Kind kind)` 的组件成为字段。
- `code_intel/extract_throughput`（每项三次以上，MB 为 2^20 字节）：本轮 8 次 3.72–4.12 MB/s；与 9a2835b 交替运行时本轮 3.72–3.96、9a2835b 3.70–3.89，在噪声以内（遍历只多了 Java 记录、TS 参数属性等几处判断）。`code_intel/extract_1mib` 3.23–3.52 MB/s（9a2835b：3.35–3.45）。

第 1 步，处理 [#113](https://github.com/SoloJiang/lectern-app/pull/113) 第四轮审阅意见（局部定义的可见性、`global` 与 `nonlocal`、解包和解构的定义、`catch` 的作用域）之后，同一台机器。

- `just check`、`just test`（整个工作区 1165 个测试通过，跳过 1 个）、`just doctest`、`just deny` 全部通过。
- `just test -p lectern-code-intel`：67 个测试通过。按前缀：`lang_*` 10、`extract_*` 14、`deadline_*` 11、`site_*` 21、`forms_*` 6、`scopes_*` 5。新增：
  - `scopes_cover_grammar`、`scopes_have_no_duplicate_rows`、`scopes_match_locals_query`、`scopes_samples_bind`、`scopes_table_matches_design`：「作用域形式」的表 168 行，其中 60 行开作用域，64 个绑定样例；查询在样例上开出的每个作用域都对应表里的一行，每个开作用域的行都被它的样例开出。
  - `forms_local_definitions_are_file_visible`：在全部定义形式的样例和六种语言的嵌套写法上，定义节点在 locals 查询的局部作用域里的都是 `File`，局部类的成员是 `File`，顶层类和命名空间的成员、TS 的参数属性不是；遍历记下的作用域与查询的作用域给出相同的可见性。
  - `site_binding_directives`：`global` 越过外层函数、`nonlocal` 停在最近的有这个名字的外层函数且不找到模块、模块顶层的 `global`、`with` 体；绑定的表驱动测试共 97 行。
  - 定义形式的样例新增 9 个（JS / TS 顶层的对象和数组解构、两层嵌套，Python 模块级和类体里的解包，Go 的 `const A, B`、`var a, b`），表新增 7 行（解构和解包的模式节点）。`WALKER_CASES` 加了这些写法，`extract_walker_matches_tags` 不再有允许的差异。
  - 1 个快照有变化，逐个看过：`billing.py` 里嵌套在 `register` 里的 `wrap` 由 `unknown` 变为 `file`。
- 测试辅助函数 `tests::site::parse` 改为共用一个编译好的语言表，`lectern-code-intel` 的测试从约 17 s 降到约 2 s。
- `code_intel/extract_throughput`（MB 为 2^20 字节）：本轮 4 次 3.73–4.09 MB/s，交替运行的 9a2835b 4.01–4.07 MB/s，在噪声以内；`code_intel/extract_1mib` 3.21–3.41 MB/s（9a2835b：3.31–3.43）。遍历只多记下局部作用域的范围，可见性的计算是一次按位置的扫描。

第 1 步，处理 [#113](https://github.com/SoloJiang/lectern-app/pull/113) 第五轮审阅意见（登记时的截止时间检查、Go 的 `:=`、Rust `impl` 的类型、TS namespace 里的声明与 ES 模块的判断）之后，同一台机器。

- `just check`、`just test`（整个工作区 1167 个测试通过，跳过 1 个）、`just doctest`、`just deny` 全部通过。
- `just test -p lectern-code-intel`：69 个测试通过。按前缀：`lang_*` 10、`extract_*` 15、`deadline_*` 12、`site_*` 21、`forms_*` 6、`scopes_*` 5。新增：
  - `deadline_checked_at_the_start`：看门狗的循环不运行时，已经过期的截止时间和已经请求的暂停仍让小文件在两条抽取路径上返回 `TimedOut` / `Cancelled`，`ParsedFile` 返回 `TimedOut`（修改之前三者都抽取完成）。
  - `extract_rust_impl_self_types`：`&T`、`&mut T`、`Box<T>`、`[T]`、`&[T]`、元组、裸指针、`u8`、路径类型、`fn()` 上的 trait 实现，方法都是 `Public`，`scope` 是实现的类型（`fn()` 没有）；固有 `impl` 的方法按自己的 `pub`；两条路径结果相同。
  - `site_binding_go` 加了 14 行：`:=` 重新声明参数、命名结果和同一块里的名字，新名字仍是声明；嵌套块和 `if`、`for`、`switch` 初始化里的 `:=` 是新变量；`for … = range` 和 `case … = <-ch` 是赋值。绑定的表驱动测试共 111 行。
  - `extract_visibility_rules` 加了 TS 的 namespace：只在 namespace 里有 `export` 的文件不是 ES 模块，顶层的函数是 `Public`；namespace 里导出的 `const` 是定义。定义形式的样例加了 namespace 和 `declare module` 里导出的 `const`、`let`，`WALKER_CASES` 加了这些写法和 Go 的 `=` 形式。
  - 1 个快照有变化，逐个看过：`units.go` 的引用名多了 `range` 的 `:=` 变量 `_`、`v`（这两种变量不再参与 tree-sitter-tags 一侧的过滤）。
- `code_intel/extract_throughput`：本轮 4 次 3.99–4.11 MB/s，交替运行的 9a2835b 4.15 MB/s；`code_intel/extract_1mib` 3.40–3.54 MB/s（9a2835b：3.48）。

第 1 步，处理 [#113](https://github.com/SoloJiang/lectern-app/pull/113) 的独立预审意见（局部解析改为白名单、线性的绑定模型和规则、按用法检查语法种类表、Python 的模块级与实例属性、导出形式）之后，同一台机器。

- `just check`、`just test`（整个工作区 1173 个测试通过，跳过 1 个；第一次运行时 `lectern-kernel` 的 `a_panicking_task_releases_its_payload_and_siblings_are_dropped` 等待超时，单独重跑和整体重跑都通过，本轮没有改 kernel）、`just doctest`、`just deny` 全部通过。
- `just test -p lectern-code-intel`：75 个测试通过。按前缀：`lang_*` 10、`extract_*` 17、`deadline_*` 13、`site_*` 23、`forms_*` 7、`scopes_*` 5。新增：
  - `site_binding_positions`：对象和解构的键、JSX 属性名和内置元素名、import 的远端名、Rust 结构体表达式和结构体模式的字段名、Java 的 `this::name`、Python 的 `case Point(x=…)` 不绑定；对象字面量的简写和各处的值绑定；Go 复合字面量的键只作候选。绑定的表驱动测试共 128 行。
  - `forms_positions_yield_sites`：定义形式和作用域形式的样例里每个声明、定义和用处都是 `Site`（含 Rust 结构体模式简写的绑定）。
  - `site_kind_masks_accept_uses`：56 个按用法写的样例，覆盖每种语言每个被收窄的引用语法。
  - `extract_export_forms_change_visibility`：12 种导出形式各让名字从 `File` 变为 `Public`，含 `export = X` 和 `declare global`。
  - `extract_python_module_level_and_attributes`：`if` / `try` / `with` / `for` 里的和连等的常量，函数和类里的不是；`self` 和嵌套函数里的接收者属性是字段，其他对象的属性是成员引用。
  - `deadline_one_mib_returns_soon_after_the_deadline`：1 MiB 的 JS、Python、TS 在 100 ms 的截止时间下，两条抽取路径和 `ParsedFile` 都在 600 ms 内返回（debug 构建在共享 CI 机器上的余量；以前未检查的阶段要几秒）。
  - 2 个快照有变化，逐个看过：`billing.py` 的 `Invoice.__init__` 里 `self.number`、`self.lines`、`self.currency` 成为字段（引用名少了 `number`）；`panel.tsx` 的引用名少了内置元素 `h2`、`li`、`section`、`ul`。
- `code_intel/extract_throughput`：本轮 3 次 3.99–4.16 MB/s，交替运行的 9a2835b 4.10 MB/s；`code_intel/extract_1mib` 3.57–3.92 MB/s（9a2835b：3.67），规则不再按「定义数 × 导出语句数」增长。

第 1 步，处理 [#113](https://github.com/SoloJiang/lectern-app/pull/113) 的第二次预审意见（标记占用的名字、环境模块、解包的属性赋值）之后，同一台机器。

- `just check`、`just test`（整个工作区 1174 个测试通过，跳过 1 个）、`just doctest`、`just deny` 全部通过。
- `just test -p lectern-code-intel`：76 个测试通过。新增 `extract_markers_keep_their_names`：每种标记各有样例，占用名字的标记的名字在两条路径上都是引用名、在 `ParsedFile` 里是引用位置或这个位置上的定义，关键字标记不产生引用名，方法的第一个参数是定义；`EXPORT_FORMS` 加了环境模块的两行（接口和它的成员）；Python 的解包属性赋值和 Rust 的 `[u8]`、`(u8, u8)`、`&u8` 加进已有的测试。
- 2 个快照有变化，逐个看过：`panel.tsx` 的引用名多了 `export default Panel` 的 `Panel`，`store.js` 多了 `export { api, ids as idSequence }` 和 `export default createStore` 的名字。
- `code_intel/extract_throughput`：本轮 5 次 3.87–4.05 MB/s，交替运行的 9a2835b 两次 3.80 和 4.13 MB/s，在噪声以内；`code_intel/extract_1mib` 3.50–3.75 MB/s（9a2835b：3.48）。

第 2 步（存储），叠在第 1 步的分支上，同一台机器。

- `just check`、`just test`（整个工作区 1149 个测试通过，跳过 1 个）、`just doctest`、`just deny`（advisories、bans、licenses、sources 都通过）全部通过。
- `just test -p lectern-code-intel`：51 个测试通过，其中 `store_*` 12 个：每种首发语言的夹具抽取后写入、读回与原结果逐项相同；`Def` 的每个字段（种类、可见性的五种取值、作用域、参数个数为空、0 和 255）读回相同；非 `Done` 的状态只有 `ci_ext` 一行、不出现在按名字的查找和名字表里，`Cancelled`、`Unsupported` 不写入；存在检查只返回（OID、语言、grammar 版本、查询版本）完全相同的单元，SHA-1 与 SHA-256 的 OID 都能查到，1300 个 OID 分三次查询；按名字查定义和引用区分大小写；五种语言的兼容组分两次查询、结果合并；同一个 OID 有旧 grammar 版本和旧查询版本的单元时，存在检查、按名字查定义、查引用和名字表的两步都只返回当前版本的单元（`store_version_filter_returns_current_units_only`）；同一个键再写一次时旧单元连同它的行被替换，淘汰代数加一；配额 64 KiB 的登记下超出配额时按访问时间淘汰，两小时后经存在检查标记过的单元留下、没有标记的被淘汰，被淘汰的单元在三张表里都没有行（测试自己的登记加了按单元计行数的语句），淘汰代数只在淘汰时变化，同一分库的两个句柄读到同一个代数（`store_quota_evicts_by_access_time`）；`set_max_page_count` 让写入返回 `Dropped`，不 panic，什么也没写入，代数不变，留出空间、过了 60 秒之后写入照常提交（`store_dropped_write_does_not_panic`）。
- `cargo tree -i rusqlite -e normal --depth 1` 只列出 `lectern-cache`。本 crate 新增 `lectern-cache` 依赖，dev 依赖加 `lectern-cache` 的 `test-support` 和 `tempfile`，Cargo.lock 只有本 crate 的依赖列表变化，没有新增 crate。
- 查询计划（Python 的 sqlite3 按同样的建表语句 `EXPLAIN QUERY PLAN`）：存在检查用 `UNIQUE (oid, lang, grammar, query)` 的自动索引按 `oid IN (…)` 逐个查找；按名字查定义先用 `ci_def_name`，再按 `ci_ext` 的主键取单元；按名字查引用用 `ci_ref` 的主键；按单元读定义、引用分别用 `ci_def_unit`、`ci_ref_unit`。
- `code_intel/warm_100k`（`just bench --measure code_intel/warm`，四次）：中位数 450、534、474、456 ms，预算 500 ms，Linux 上不判定，M 系列由 macOS CI 判定。拆分：去掉存在检查里的标记访问时约 82 ms（200 次查询）；其余约 370 ms 是 `TableSet::touch` 写回 10 万个单元的访问时间（每 4096 个一个写事务，逐行 `UPDATE`，约 3.7 µs 一个），与 `cache/tables_touch` 的预算（5000 个 < 20 ms）同一量级；访问时间在一小时以内、写回不改任何行时耗时相同（472 ms），开销在逐行执行语句本身。
- 单元的字节数（「存储表」的估算）对照实际文件：每种首发语言约 8 KiB 的夹具语料（`testing::corpus`）各写 1000 个单元，声明的字节数合计 52.8 MB，分库文件（含 `-wal`）59.1 MB，实际约为估算的 1.12 倍。这份语料由抽取夹具重复拼成，每 8 KiB 有 99–152 个定义、17–50 个引用名，定义比真实代码密得多，所以没有据此修改 10 万文件 200–250 MB 的估算；`code_intel/disk_100k` 仍按仓库形状的语料在后续步骤测量。

第 2 步，把访问标记移出热启动之后（[#115](https://github.com/SoloJiang/lectern-app/pull/115) 的 macOS CI 上 `code_intel/warm_100k` 中位数 537、549 ms，超出预算），同一台机器，不含 #116 的 cache 改动。

- `just check`、`just test`（整个工作区 1150 个测试通过，跳过 1 个）、`just doctest`、`just deny` 全部通过。`just test -p lectern-code-intel`：52 个测试通过，`store_*` 13 个。
  - `store_quota_evicts_by_access_time` 改为：两小时后的存在检查找到全部 40 个单元，只有前 20 个的命中排队并经 `touch_pending` 标记；超出配额后前 20 个留下，没标记的被淘汰。存在检查如果自己标记访问，40 个单元的访问时间相同，淘汰会按 ID 先删 0–13 号：在 `exists` 里临时加回标记时，这个测试失败（「unit 0 was evicted」）。
  - 新增 `store_pending_touches_drain_in_chunks`：2500 个命中分三次标记完（每次 1024 个）；淘汰代数变化之后，之前排队的命中被丢弃，新代数的命中替换旧队列。
- `just bench --measure code_intel/warm`，三次：
  - `code_intel/warm_100k`（只有存在检查）：中位数 82.6、81.9、81.2 ms，预算 500 ms。
  - `code_intel/warm_100k_touch`（只记录；10 万个命中每次 1024 个，访问时间都已过期，全部写回）：792、773、792 ms。这比第一版按减法估出的约 370 ms 高一倍，那个数字是两次运行的差，不是直接测量。
  - 按 500 个一块，或从队列头部取，结果相同（798、748 ms）。
- 合并 main（含第 1 步的最终版本 [#113](https://github.com/SoloJiang/lectern-app/pull/113)、`EXTRACTOR_VERSION` 6，以及 cache 的集合写回 [#116](https://github.com/SoloJiang/lectern-app/pull/116)）之后，同一台机器：
  - `just check`、`just test`（整个工作区 1191 个测试通过，跳过 1 个）、`just doctest`、`just deny` 全部通过；`just test -p lectern-code-intel` 89 个测试通过，其中 `store_*` 13 个。
  - `code_intel/warm_100k`：81.9、81.3、79.4 ms。
  - `code_intel/warm_100k_touch`：327、314、311 ms，#116 之前是 773–792 ms。
  - `EXTRACTOR_VERSION` 已经包含在查询版本里（「语言与抽取键」），`Version::of` 取的就是它，单元的键和版本过滤不用改。

第 3 步（映射、调度与增量更新），基于包含 [#113](https://github.com/SoloJiang/lectern-app/pull/113)、[#115](https://github.com/SoloJiang/lectern-app/pull/115) 的 main，同一台机器（Linux x86_64，4 核 Xeon 2.1 GHz）。

- `just check`、`just test`（整个工作区 1261 个测试通过，跳过 1 个）、`just doctest`、`just deny`（advisories、bans、licenses、sources 都通过）全部通过。Cargo.lock 只有本 crate 的依赖列表变化，没有新增 crate。
- 第 1 步的 `extract_walker_matches_tags` 在机器负载高时偶尔失败：两条路径共用一个 1 s 的交互截止时间，病态的 JS 让 tags 超时，超时的结果是空的。改为每条路径各自 60 s 的截止时间，并要求 tags 的结果是 `Done`；四个测试进程同时运行时不再失败。
- `rg -n 'std::fs::|File::open' crates/code_intel/src --glob '!**/tests*' --glob '!**/testing*'` 和 `rg -n 'Command::new|thread::spawn' crates/code_intel/src` 都没有结果。
- `just test -p lectern-code-intel`：159 个测试通过，第 3 步新增 70 个。按前缀：
  - `map_*` 7：从带 OID 的文件列表建映射（只含普通和可执行文件、受支持的语言）；前 8 字节相同的 OID 分得开；覆盖项遮住 index 条目，改掉的旧 OID、删除的文件、超过上限的文件不出现，未跟踪文件能查到；与 index 相同的覆盖项被丢掉；同一份字节在 `.java` 和 `.js` 下按语言区分；提示只匹配自己的 OID 前缀和戳记，换文件列表时按路径沿用；10 万文件的映射 ≤ 3 MB。
  - `overlay_*` 4：表驱动的 12 种工作区情况（干净、未暂存的修改、暂存后未再改、`StagedOnly`、未暂存的删除、未跟踪、被忽略的未跟踪、语言不受支持的未跟踪、未暂存的重命名、`Filtered`、超过 1 MiB、CRLF 检出），每种情况之后覆盖项与磁盘上的实际内容一致；工作区比较失败时保留上一份覆盖部分；生产的 `LecternGit` 在 `TestRepo` 仓库上与 `FakeGit` 给出相同的比较结果、HEAD、index 校验和、规范 OID、index 条目和 blob；未跟踪的嵌套仓库和被目录替换的跟踪文件，两者列出相同的路径和 HEAD OID（`overlay_lectern_git_and_the_fake_list_directories_alike`）。
  - `derived_*` 10：文件列表、子项目、HEAD、index、覆盖部分、忽略规则（含 `untracked_capped`）、语言包、淘汰、写入各一组，用计数钩子检查只有依赖这个输入的派生状态被重算；`HeadDiff` 比较完成之前 `head_clean` 为 false、提交之后为 true；核对任务计算期间输入变化时这次比较的结果被丢弃、按新输入再比较一次。
  - `schedule_*` 25：层的先后（第 1 层、第 2 层、当前子项目、其余文件按路径、其他 worktree）；第 1、2 层持有 `BusyGuard`、在 `Interactive` / `Background` 上运行，批量层持有 `BackgroundGuard`、在 `Batch` 上运行；暂停在批次边界生效（`[64, 5]` 两批之后停下，第 70 个文件在登记时被放弃，恢复后先做它）；暂停开关先于第一批打开时不抽取；一次 1 MiB 的抽取进行中打开暂停，这次抽取返回 `Cancelled`、没有写入、提示不变，恢复后它第一个被重新抽取（`schedule_pause_abandons_in_flight`）；批次在 64 个文件（`[64, 64, 22]`）、2 MiB（`[6, 4]`）、50 ms（`[3, 3, 3, 1]`）处结束，一个 200 ms 的文件一批（`[1, 2]`）；切换 worktree 后游标重建、只抽取缓存里没有的；同一 OID 在两个路径上只抽取一次；`schedule_key_from_bytes_read`；第 5 层只抽取缓存里没有、且工作区内容等于 index 的文件；大文件轮流抽取；配额 24 KiB 时 80 个文件各抽取一次、遍历结束；一次抽取进行中切换会话，旧会话的批次不改动新会话的计数；文件夹项目的各层都是 `Pending`、没有抽取、映射为空。预审之后补的：配额 24 KiB、80 个文件时 5 次切换文件，批量层不再抽取、游标不重建（原来每次多抽取 24–57 个）；150 个文件里第 10 个抽取时暂停，审阅集合的 10 个文件由第 2 层写入，恢复后批量层不再抽取和写入它们，共 151 次抽取（暂停放弃的那个两次），淘汰代数不变、`full` 未设置；批量层记为 `TimedOut` 的文件打开时按交互上限重做一次、存为 `Done`，再次打开不再检查；批量层正在抽取的文件被打开时第 1 层取消那次抽取、自己抽取；一个其他 worktree 的 index 变化只重新读那一个 worktree 的 index，没有新的存在检查和抽取；还没有 `set_focus` 时最后打开的项目跑第 3–5 层，之后打开的项目不再接管。第二轮预审之后补的：重做 `TimedOut` 的替换让 cache 的淘汰代数加一，提示的戳记、游标和第 5 层不变（另一个 worktree 的 index 没有再读）；审阅集合里的文件正在批量层抽取时，第 2 层取消它、自己抽取；打开时读不出来的文件不记为已满足，可读之后再打开时抽取；第 5 层已确认的集合 10 万个键 ≤ 1.5 MB、分得开 OID 和语言。#117 审阅之后补的：三个批量线程时一次唤醒就有三个任务（`peak` 为 3）；关闭跑第 3–5 层的文件夹项目后，排在它前面打开的 git 项目开始并走完第 3–5 层；第 2 层有 30 个文件的队列时在第一个文件的抽取中关闭项目，之后不再抽取任何文件，kernel 静止；`Harness` 在每次 `settle` 和 `set_focus` 之后检查批量任务的活性规则，全部 `schedule_*` 和其余建索引的测试都满足它。
  - `grammar_*` 10：安装、更新（在途的版本 1 抽取被取消，版本 1 的结果没有写入）、回滚（不重新抽取）、撤销（离开映射和 `coverage`，在途抽取被取消）、多个通知并成一轮（过时的代数被丢弃）、租约（撤销时有一个在途抽取和一个 `ParsedFile`，两者都结束之后模块恰好卸载一次）；语言包更新之后、映射重建之前运行了一个批次，或打开了那个文件，最后两个文件都按版本 2 写入、提示匹配（原来不再按版本 2 抽取）；项目打开时读到的语言包代数被随后的一轮越过（那一轮没有通知它），基础映射仍然装上、最多建两次，之后的事件不再重建（原来每个事件 16 次重建、映射一直为空）；激活之前已装的语言包在第一张映射里（`grammar_installed_before_activation_is_in_the_first_map`）。
  - `battery_*` 3、`incremental_*` 9、`warm_*` 2：未跟踪的嵌套仓库打开时最多比较两次、核对的遍数有界，它上面的事件最多再比较一次；跟踪文件被目录替换后文件离开映射、目录里的文件入索引，kernel 回到静止；事件已交付、核对任务还没运行时 `head_clean` 对改动的路径为 false、对其他路径仍为 true，目录事件和 HEAD 事件让所有路径为 false，核对之后恢复；其余见「测试」；`incremental_kernel_busy_until_the_overlay_is_updated` 检查从后端交出事件到覆盖部分更新完之前 kernel 不静止；热启动 40 个文件 7 种语言时只有 7 次存在检查、没有抽取。
- 基准（`just bench --measure code_intel/`，release，MB 为 2^20 字节）：

  | 基准 | 结果 | 预算 |
  |---|---|---|
  | `code_intel/map_100k` | 中位数 9.4–9.6 ms（两次）；预审修正之后 9.19 ms；第二轮之后 9.75 ms；#117 之后 10.8 ms | ≤ 20 ms |
  | `code_intel/map_100k_bytes` | 2.65 MB（两轮修正之后相同） | ≤ 3 MB |
  | `code_intel/batch_peak` | 最大 32.7 MB（两轮修正之后相同） | ≤ 40 MB |
  | `code_intel/disk_100k` | 298 MB（只记录；两轮修正之后相同，80 次淘汰） | — |
  | `code_intel/extract_throughput`（第 1 步，不变） | 3.81、4.05 MB/s；修正之后 4.27 MB/s；第二轮之后 3.76、3.41 MB/s，#117 之后 3.3 MB/s（见下） | ≥ 4 MB/s（macOS 判定） |
  | `code_intel/warm_100k`（第 2 步，不变） | 84.8、84.3 ms；修正之后 79.8 ms；第二轮之后 103 ms（报告为 noisy），#117 之后 85.8 ms | < 500 ms |

  第二轮和 #117 一轮的数字在容器重启之后测得。`extract_throughput` 低于 4 MB/s，但抽取的代码与预审修正那一轮（4.27 MB/s）相同：之后只改了 `index/`、`GrammarSource` 的一个方法和测试，基准不经过它们，所以这是机器的差别；这一项在 Apple 芯片上判定（[bench 设计](bench.md)），Linux 上的数字只作记录。
- 测试里 `Harness::quiescent` 原来在 `Kernel::barrier()` 之后立即读 `is_quiescent()`，负载高时偶尔读到忙（`incremental_kernel_busy_until_the_overlay_is_updated` 在 300 次压力运行里失败 1 次，这个测试本轮没有改动）。原因在 kernel：kernel 线程处理 barrier 时先回复、再把这个请求自己的计数减一，所以 `barrier()` 返回后有一瞬间仍是忙。`Harness::quiescent` 改为在 barrier 之后最多等 200 ms 让计数归零（真正持有的守卫仍然判为忙），300 次压力运行全部通过；kernel 的这个顺序另行修正，不在本 PR 里。

  `batch_peak` 的组成：去掉接近 1 MiB 的文件之后峰值是 454 KB，所以几乎全部来自一个 900 KiB 的 TS 文件的语法树和遍历（约 31 MiB），超过设计按「≤ 10 MiB」的估算；预算 40 MB 不变，大文件因此同时只在一个批量任务里抽取（「性能」已改）。tree-sitter 的分配经 `testing::route_tree_sitter_allocations` 计入；基准用一个批量任务，工作区比较是 `FakeGit` 的。
  `disk_100k`：9 万个源码单元声明 279.7 MB，分库文件 298 MiB，写入过程中淘汰 80 次，4 个线程 70 s；结论写在「存储表」的「规模」一条。
- 没有验证的：M 系列上的各项基准（由 macOS CI 判定）；`PowerSource` 的真实实现（`platform` 还没有电源接口，测试只用 `testing::FakePower`）；真实的 fs_watch 后端（测试用 `FakeBackend`）；几个批量线程同时运行（测试用 `Executor::manual()`，大文件轮流抽取由 `LargeSlot` 的单元测试检查）；文件夹项目的建索引（第 3 步不实现，`coverage` 报告 `Pending`）。

第 4 步（查询与插件），rebase 到包含 kernel 的 barrier 修正（#96，[#118](https://github.com/SoloJiang/lectern-app/pull/118)）的 main 之后，同一台机器（Linux x86_64，4 核 Xeon 2.1 GHz）。

- `just check`（含 `python3 scripts/check_architecture.py`：30 个 crate 通过，本 crate 的 `plugin = true` 生效，只有 `bench`（tool 层）依赖它）、`just test`（整个工作区 1318 个测试通过，跳过 1 个）、`just doctest`（7 个通过）、`just deny`（advisories、bans、licenses、sources 都通过）全部通过。Cargo.lock 只有本 crate 的依赖列表变化（`memchr`、`unicode-normalization` 原已在锁文件里），没有新增 crate。`Harness::quiescent` 仍是第 3 步的宽容等待（barrier 不等 kernel 线程自己排入的请求）。
- `rg -n 'std::fs::|File::open' crates/code_intel/src --glob '!**/tests*' --glob '!**/testing*'` 和 `rg -n 'Command::new|thread::spawn' crates/code_intel/src` 仍没有结果。
- `just test -p lectern-code-intel`：214 个测试通过，第 4 步新增 55 个。按前缀：
  - `goto_*` 10：第 1–3 步各自在有依据时停下（局部绑定、`Resolver::bind`、索引）；一个有依据的候选直接跳转，只有同名依据时预选第一个；分层与排序（当前文件、审阅集合、子项目、依赖、其余）；按兼容组、可见性和语法种类过滤；当前文件的定义不重复列出；同一份字节在 `.java` 和 `.js` 下是一个 OID、两个单元，`java` 单元只给出 `.java` 的路径；没有标识符和语言不受支持时 `Failed`；`Object` 来源（Diff 的旧侧）按 OID 读 blob、不读 worktree；结果只增不减（后一批只追加，第一批的下标不变）；测试自己的 `Resolver` 给出第 2 步的 import 目标，第 2 步读过的目标不被限时搜索重复抽取、不重复列出。
  - `read_verify_*` 1（表驱动）：原始字节的 OID 相符、CRLF 检出时规范 OID 相符，这两种回答；映射里的旧 OID 已不是磁盘内容（还没有事件）、外部过滤器（没有规范 OID）、文件已删除，这三种不回答。
  - `search_*` 5：整词匹配的表（标识符字符、Unicode、边界）；只解析整词命中的文件；1 s 停下，`Partial` 的计数与未搜的文件数一致；后台正在抽取的文件不搜索，其余文件都搜完结果仍是 `Partial`；跳转的限时搜索把命中文件的抽取结果写入缓存，那个路径的提示随之匹配。
  - `refs_*` 8：同文件的调用方；局部变量只在自己的作用域里；嵌套的同名函数是局部的；被遮蔽的使用不列出；顶层名字被 locals 捕获时仍是引用；`bind` 给出依据时是调用方；三个来源（当前文件、`ci_ref`、限时搜索）不重复；未入索引的文件经限时搜索找到。
  - `names_*` 4：去重、NFC、只含当前 worktree 映射里的 OID；`Building` 之后在 `Names` 事件里交出表，之后 `Ready`；持有中、代数落后时 `names` 仍返回旧表，新表等间隔到了由看门狗的到期项构建，几次变化只建一次，持有者全部释放后不再重建（`clear_events` 之后数 `name_builds`）；没有项目或没有索引时 `Unavailable`。
  - `escalate_*` 9：只有一个或几个同名候选时向 Java 提供者升级；`head_only` 提供者只对与 HEAD 相同的文本调用，它在与 HEAD 不同的文件里给出的位置被丢掉；回复的先后不影响合并结果；`Same` 与 `Ready`；关闭查询、淘汰时取消、作废的请求 ID 被忽略；SCIP 的回复不取消 jdtls 的请求；死掉或已注销的提供者不再调用，注销时未回复的请求作废并重新合并；查引用同样升级。`head_dirty_*` 1：暂存、改 index、提交三种情况下 `head_clean` 的变化。
  - `prefetch_*` 2、`focus_*` 1：2 s 内同一查询沿用预取（`Stats::prepares` 不增加）；过了 2 s、换了映射、发生淘汰之后不沿用（表驱动）；对不是当前 worktree 的焦点不进第 1、2 层，切换过去之后才进。
  - `plugin_*` 4（`TestKernel` 加模拟的 `dyn Projects`）：激活时不做 IO，没有项目时不建任何东西；缺少 `dyn Projects` 时激活失败；结果事件按代数顺序发出，最后一批交出之前 kernel 忙；关闭项目时取消它未回复的语义请求、退掉 fs_watch 订阅，停用时退掉电源和语言包的订阅。
  - `watchdog_*` 5（读 `/proc/self/task/*/comm`）：停用后看门狗线程退出；executor 关闭时退出；线程启动前已经结束的激活不起线程（抽取照常运行，没有截止时间的执行者）；激活 20 次之后最多一个线程；停用时进行中的抽取返回 `Cancelled`、不写入。
  - `derived_*` 新增 3：覆盖项改了 OID 的文件被搜索；被淘汰的定义经限时搜索找回；子项目变化后分层和搜索范围随之变化。`site_*` 新增 2：`site_reference_query_matches_full`、`site_parents_match_tree_sitter`（见「与设计的差异」）。`schedule_folder_project_indexes_at_open` 替换第 3 步的文件夹项目测试：打开即读出全部文件、各层走完、`coverage` 为 `Done`。
- 基准（`just bench --measure code_intel/ plugin/code-intel/`，release，MB 为 2^20 字节）：

  | 基准 | 结果 | 预算 |
  |---|---|---|
  | `code_intel/goto_indexed` | 中位数 30.6 ms（第一版）；`site_query` 之后 25.2 ms；`Parents` 之后 25.0、23.2 ms | ≤ 20 ms（macOS 判定） |
  | `code_intel/search_stop` | 最大 1 s | ≤ 1.02 s |
  | `code_intel/references_100k/all` | 中位数 1.26 s（第一版）；1.03 s | ≤ 2 s |
  | `code_intel/references_100k/first` | 中位数 12.4 ms；10.6 ms | ≤ 200 ms |
  | `code_intel/names_100k` | 中位数 1.07 s（逐个单元查 `ci_def_unit`）；一次扫描之后 814、796 ms | ≤ 1 s |
  | `code_intel/names_100k_bytes` | 4.66 MB（29 万个名字） | ≤ 8 MB |
  | `plugin/code-intel/resident_cloudprint` | 241 KB（第一版）；155 KB | ≤ 4 MB |
  | `plugin/code-intel/resident_100k` | 11.5 MB（第一版）；2.86 MB | ≤ 10 MB |

  同一次运行里第 1–3 步的基准：`map_100k` 9.22 ms、`map_100k_bytes` 2.65 MB、`batch_peak` 32.7 MB、`extract_throughput` 4.22 MB/s、`warm_100k` 83.5 ms，都在预算内；`disk_100k` 9 万个单元，分库文件 298 MiB，80 次淘汰。

  `goto_indexed` 在这台机器上超出 20 ms。拆分（release，64 KiB 的 TS 当前文本）：解析约 9 ms，locals 与引用模式的查询约 4.5 ms，处理匹配（作用域、局部引用的变量位置）约 4 ms，遍历约 2 ms，规则与收尾约 1 ms，合计约 20 ms，其余（读取与核对、按名字查定义、约 130 个候选的过滤与排序）约 3 ms。设计的分项是「解析与 locals ≤ 10 ms」，这台机器上单是解析就接近它；这一项与 `extract_throughput`、`warm_100k` 一样在 Apple 芯片上判定（[bench 设计](bench.md)），预算没有因 Linux 的结果调高。`resident_100k` 第一版的 11.5 MB 里约 9 MB 不属于本 crate：基准没有像 `project` 那样先订阅 worktree，fs_watch 为本 crate 的订阅建的跟踪路径过滤器算进了测量窗口（`Rig::watch_as_project` 改正）；另外 `touch_pending` 排空后保留了队列的容量（已释放）；核心语言不再编译 `Compiled` 也减少了常驻。
- 没有验证的：M 系列上的各项基准（`goto_indexed` 在这台机器上超出，见上）；`app` 装配本插件（`app` 依赖 GPUI，Linux 上不编译，第 5 步）；真实的 fs_watch 后端和语义导航提供者（测试用 `FakeBackend` 和测试里的假提供者，jdtls / SCIP 属于 #26；电源接口按 ADR 0032 不再需要）；几个批量线程同时运行；文件夹项目超过 2 万个文件时的行为（由 `project` 的遍历上限决定，没有测）；cloudprint 形状的名字表（≤ 0.1 MB、≤ 30 ms，没有对应的基准）。

第 4 步，处理预审意见之后，rebase 到包含 #119、#120（5eff366）的 main，同一台机器。

- `just check`、`just test`（整个工作区 1322 个测试通过，跳过 1 个）、`just doctest`、`just deny` 全部通过。`just test -p lectern-code-intel` 218 个测试通过，本轮新增 4 个：`search_cut_extraction_is_not_stored`（5 个未入索引的文件各抽取 200 ms，第 5 个在剩余的 200 ms 里被截短：结果是 `Partial { unsearched: 1 }`，它没有写入；批量层之后按自己的截止时间写入，下一次跳转找到全部 5 个、`Complete`）、`search_file_deadline_table`、`search_distance_table`、`goto_site_queries_compiled_after_open`（映射里有 TS 和 Rust 文件：静止之后两者的查询已编译，Go 的没有）。
- 不再依赖 OS 或时间窗口推断状态的测试：`watchdog_*` 看 `plugin::WatchdogThread`，`/proc` 只在 Linux 上、线程退出之后确认；`deadline_watchdog_idle_does_not_wake` 等看门狗的无超时等待计数加一，再确认 100 ms 里没有有超时的等待（原来只看 30 ms 内轮数不变，负载高时最后一轮落在窗口之后，macOS CI 上失败过）；`names_refresh_while_held` 的「不再重建」等到期表清空；`schedule_large_files_take_turns` 等等待方调用过两次停止探针。
- 压力运行（同时 4 个占满 CPU 的进程）：`-E 'test(watchdog) or test(deadline) or test(search)'`（32 个测试）`--stress-count 150` 全部通过；整个本 crate（218 个）`--stress-count 30` 全部通过。预审时这两组的失败：`watchdog_one_thread_after_20_activations` 150 次里 54 次，`deadline_one_mib_returns_soon_after_the_deadline` 的 TS `ParsedFile` 一项 150 次里 38 次（计时里包含第一次编译）。
- 基准（`just bench --measure code_intel/ plugin/code-intel/`）：

  | 基准 | 结果 | 预算 |
  |---|---|---|
  | `code_intel/goto_first`（新增） | 中位数 6.37、6.86 ms | 只记录（候选 50 ms） |
  | `code_intel/goto_indexed` | 中位数 23.7、23.9 ms | ≤ 20 ms（macOS 判定） |
  | `code_intel/search_stop` | 最大 1 s | ≤ 1.02 s |
  | `code_intel/references_100k/all`、`/first` | 1.01 s、951 ms；11 ms、10 ms | ≤ 2 s；≤ 200 ms |
  | `code_intel/names_100k`、`names_100k_bytes` | 881 ms；4.66 MB | ≤ 1 s；≤ 8 MB |
  | `plugin/code-intel/resident_cloudprint` | 374 KB（上一轮 155 KB） | ≤ 4 MB |
  | `plugin/code-intel/resident_100k` | 3.07 MB（上一轮 2.86 MB） | ≤ 10 MB |

  常驻增加的约 0.2 MB 是映射里七种语言的 `SiteQuery`（上一轮只编译了跳转用到的 TS）。同一次运行里 `extract_throughput` 为 3.78 MB/s，低于 4 MB/s：抽取的代码本轮没有改，这台机器上这一项在 3.3–4.27 MB/s 之间波动，在 Apple 芯片上判定；其余第 1–3 步的基准都在预算内。预审时 Linux 上第一次跳转的编译耗时（release）：ts 138 ms、tsx 109 ms、rust 82 ms、js 42 ms、python 33 ms；现在由后台任务承担，`goto_first` 不含它。
- 没有验证的：同上一轮；另外，第一次跳转发生在后台编译完成之前时，查询等这次编译结束（不会比自己编译更久），没有单独测量。

第 4 步，处理 #121 上 Codex 的两条 P1 之后，同一台机器。

- `just check`、`just test`（整个工作区 1328 个测试通过，跳过 1 个）、`just doctest`、`just deny` 全部通过。本 crate 224 个测试，新增 6 个：`cut_jump_finds_a_file_indexed_after_the_lookup`、`cut_prefetch_reused_after_a_batch_finds_its_file`、`cut_references_find_a_file_indexed_after_the_lookup`、`cut_named_finds_a_file_indexed_after_the_lookup`（批次在查索引与第一次读提示之间写入 c.ts：四种查询都列出它；去掉第二次查索引时这四个都失败）、`cut_unchanged_hint_is_no_installation`、`goto_site_queries_of_every_open_project`（两个项目的核对都排在编译任务之前：TS 和 Python 都已编译；换回原来按项目重看的实现时这个测试失败，Python 没有编译）。
- 压力运行（同时 4 个占满 CPU 的进程）：`-E 'test(cut_) or test(goto_site_queries) or test(search_) or test(prefetch_)'` （19 个测试）`--stress-count 150` 全部通过；整个本 crate（224 个）`--stress-count 30` 全部通过。
- 基准：本轮只改了查询在提示变化时的补查、编译任务的调度和批量任务的唤醒顺序。没有批次写入时，补查只多读一次项目锁里的计数，所以没有重跑基准。
- 没有验证的：`kick_batch` 的顺序只在多个 executor 线程同时运行时有区别，测试用的手动 executor 是单线程，没有专门的测试。

第 4 步，`goto_indexed` 在 macOS CI（3 核 M1 虚拟机）上中位数 24.2、26.5 ms，超出 20 ms 之后，同一台 Linux 机器。

- 分项（`crates/bench/tests/goto_phases.rs`：与 `goto_indexed` 相同的夹具，10 万文件、64 KiB 的 TS 当前文本、69 个候选，release，300 次跳转的中位数；每次跳转从提交查询到取回结果，`src/phases.rs` 计时，计时本身的开销算在内）：

  | 阶段 | 改前 | 改后 |
  |---|---|---|
  | 跳转合计 | 25.7 ms | 18.9 ms |
  | 读取并核对当前文本（1 次读、64 KiB 的哈希） | 0.11 ms | 0.10 ms |
  | tree-sitter 解析 | 9.3 ms | 8.8 ms |
  | 整棵树上的查询（locals；改前另有引用模式） | 9.6 ms（11 478 个匹配） | 4.9 ms（2 506 个匹配） |
  | 光标处再跑完整查询 | — | 0.02 ms |
  | locals 收尾 | 0.21 ms | 0.20 ms |
  | 遍历（定义） | 1.96 ms | 1.90 ms |
  | 规则与引用位置 | 1.47 ms | 0.22 ms |
  | `site_at`、排序器、第 2 步 | < 0.02 ms | < 0.02 ms |
  | 按名字查定义（1 个读事务、1 条语句，`prepare_cached`） | 1.22 ms | 1.16 ms |
  | 过滤与分层 | 0.25 ms | 0.25 ms |
  | 消歧与排序 | 0.04 ms | 0.04 ms |
  | 交出结果（两次）、限时搜索范围（0 个文件）、切面检查、访问标记 | 约 0.17 ms | 约 0.17 ms |
  | 每次跳转的分配（含 realloc） | 约 10 500 次 | 约 5 700 次 |

  随仓库或候选数增长的部分（查定义、过滤、排序、搜索范围）合计约 1.6 ms；候选不读文件、不算哈希；切面计数没有引起第二次查索引；名字表和映射没有复制。时间在当前文本的解析和整棵树上的查询：设计的分项「解析与 locals ≤ 10 ms」，改后仍是 8.8 + 4.9 = 13.7 ms（加上 locals 收尾和遍历 15.8 ms），超出它。解析本身接近 10 ms，不经缓存或重新组织 locals（按作用域只匹配光标所在的部分，或并入遍历）降不下去；这两种都改变绑定模型的取法或要设计决定，本轮没有做，预算也没有改。
- `just bench --measure code_intel/ plugin/code-intel/`：`goto_indexed` 中位数 18 ms（上一轮 23.7、23.9 ms），`goto_first` 5.07 ms，`references_100k/all` 1 s、`/first` 10.1 ms，`search_stop` 1 s，`names_100k` 803 ms，`resident_cloudprint` 650 KB（上一轮 374 KB，多了七种语言的 `jump_query`），`resident_100k` 3.34 MB；`extract_throughput` 3.98 MB/s，同前几轮在 4 MB/s 上下波动，本轮没有改抽取。
- `just check`、`just test`（整个工作区 1329 个测试通过，跳过 2 个：另一个是忽略的 `goto_phases`）、`just doctest`、`just deny` 全部通过；本 crate 225 个测试，新增 `site_parse_at_matches_parse`。同时 4 个占满 CPU 的进程下整个本 crate `--stress-count 30` 全部通过。

第 4 步，试过把跳转的 locals 查询限制在光标外围的顶层条目上（`QueryCursor::set_byte_range`，范围是根节点下包含光标的那个子节点），没有采用。

- 前提不成立：`Locals::resolve` 从光标所在的作用域一直走到模块作用域（作用域 0），模块级的 locals 定义（顶层的 `const`、`function`、`class`、`interface`、`import`，块里提升到顶层的 `var`，Python 模块级的赋值和 import，Rust 的 `use` 和顶层条目，Java 的 import）也绑定条目里的引用，给出 `Bound(Local)` 和 `jump`。遍历给出的是 tags 的定义，不进入绑定模型，补不上这一部分。
- 扩充后的 `site_parse_at_matches_parse`（见「与设计的差异」，约 6 000 个位置）在限制之后失败：`site_at` 有 161 个位置不同，例如 TS 夹具里 `implements Shape`、`extends Base` 原来绑定到同一文件的 `interface Shape` 和 `class Base`，限制后没有绑定；另有 2 649 个位置的定义的 `local` 标记不同。按语言：ts 1 203、tsx 281、js 403、python 697、rust 164、java 62。只有 Go 没有差异：它的包级定义模式以 `source_file` 为根，根节点与任何范围都相交，限制后仍被匹配。
- 限制的潜在收益（不成立的版本，3 000 个文件，64 KiB 的 TS）：整棵树的查询从 4.9 ms 降到 0.1 ms，跳转从 18.1 ms 降到 11.8 ms。要在结果不变的前提下拿到它，模块作用域的 locals 定义需要另外的来源（例如由遍历同时给出，ADR 0031 的遍历范围要扩大，每种语言要与 locals 查询做对照），这一项留给设计决定。
- 结论：所有语言都退回整棵树（即现状）。只对 Go 打开限制对 `goto_indexed`（TS）没有作用，也没有做。分项与上一轮相同：跳转 18.9 ms，其中解析 8.8 ms、整棵树的 locals 查询 4.9 ms，「解析与 locals ≤ 10 ms」这一项仍然超出。本轮只改了测试，生产代码与上一轮相同，基准没有重跑（上一轮 `goto_indexed` 18 ms）。

第 4 步，处理 #121 上 Codex 对 346f911 的三条 P1 之后（预取等的保留期限、排序比较里的重复计算、所有读文件的核对），同一台机器。

- `just check`、`just test`（整个工作区 1334 个测试通过，跳过 3 个：`goto_phases` 的两个分项）、`just doctest`、`just deny` 全部通过。本 crate 230 个测试，新增 5 个：`prefetch_expires_unused`、`grammar_lease_released_by_an_expired_prefetch`（关掉到期清理时这两个失败）、`escalate_answered_query_releases_its_map`、`read_verify_search_skips_a_file_changed_after_the_snapshot`、`read_verify_references_skip_an_indexed_file_changed_after_the_snapshot`（读出的字节不核对时这两个失败）。同时 4 个占满 CPU 的进程下：第一次整个本 crate `--stress-count 30` 里两个到期测试失败 4 次，原因在测试：等到期表清空再静止，而看门狗是先把到期的项移出表、再调用它的回调，回调提交的清理任务可能还没提交；改为数完成的回调（`Watchdog::fired`，`Harness::advance_and_fire`，名字表测试原来的 `fire_due` 有同样的问题，一并改掉）之后，新增与计时相关的 20 个测试 `--stress-count 150` 全部通过，整个本 crate `--stress-count 30` 全部通过。
- 基准（`just bench --measure code_intel/ plugin/code-intel/`）：`goto_indexed` 17.2 ms，`goto_first` 4.87 ms，`named_50k`（新增，只记录；54 000 个定义）493 ms，`references_100k/all` 971 ms、`/first` 9.93 ms，`search_stop` 1 s（每个读到的文件都核对 OID 之后），`names_100k` 919 ms，`resident_cloudprint` 650 KB，`resident_100k` 3.34 MB；`extract_throughput` 3.95 MB/s，同前几轮在 4 MB/s 上下波动，本轮没有改抽取。
- `named_50k` 的分项（`goto_phases.rs` 的 `named_50k_phases`，release，20 次的中位数）：按名字查定义（SQL）383 ms，组成候选 72 ms，排序 16 ms，访问标记 11 ms，计数 5 ms，交出 4 ms；合计 501 ms，约 27 万次分配。

第 4 步，查询改为按视图（「查询的视图」）之后，同一台机器。

- `just check`、`just test`（整个工作区 1340 个测试通过，跳过 3 个）、`just doctest`、`just deny` 全部通过。本 crate 236 个测试。
- 交错矩阵（`view_*`）：4 种查询（跳转、复用预取的跳转、查引用、`Named`）× 5 个步骤（取快照后、查索引后、读提示后、每次读文件前、发布前；复用预取另加「预取与查询之间」）× 7 种后台变化（批次提交、淘汰、映射重建、磁盘上改文件、切换 worktree、关闭项目、语言包变化），每格一个新仓库，共 147 格，其中 140 格的变化确实发生在那一步（`Named` 不读文件，它的 7 格「读文件前」走不到；测试按格数断言）。每格断言：结果是旧视图的、新视图的，或者不是 `Complete`；列出的每一项都属于其中一个视图；会话结束之后不再有这个查询的 `Results` 事件。另有 `view_query_right_after_a_switch_answers_from_the_new_worktree`（切换后立刻问新 worktree 的文本：按新映射回答或不是 `Complete`）和 `view_eviction_moves_the_generation`。分别去掉修复的各部分时：去掉淘汰这一成员，跳转和 `Named` 的矩阵失败（`Definition Step(Snapshot) Evict: complete but neither view's`）；去掉事件里的同步切换，四个矩阵都失败（`published after its session ended`），切换后立刻查询的测试也失败；去掉等映射，切换后立刻查询的测试失败（`Named` 按空映射报 `Complete`）；去掉发布时的会话检查，矩阵仍然通过：同步关闭在同一把锁下已经去掉了这些查询的条目，两者保证的是同一件事。
- 压力运行（同时 4 个占满 CPU 的进程）：`view_*`、`cut_*`、`read_verify_*`（18 个测试）`--stress-count 150` 跑满 147 次全部通过、第 148 次时到了后台任务的 60 分钟上限被中止，又补跑 `--stress-count 10` 全部通过，合计 157 次没有失败；整个本 crate（236 个）`--stress-count 30` 全部通过。
- 基准：`goto_indexed` 18.6 ms（视图的比较每次跳转合计约 0.35 µs，`goto_phases`），`goto_first` 4.73 ms，`named_50k` 492 ms，`references_100k/all` 972 ms、`/first` 10 ms，`search_stop` 1 s，`names_100k` 842 ms，`resident_cloudprint` 650 KB，`resident_100k` 3.34 MB；`extract_throughput` 3.88 MB/s，同前几轮在 4 MB/s 上下波动，本轮没有改抽取。

第 4 步，快照改为只来自发布的对象、语义仲裁区分失败之后，同一台机器。

- `just check`、`just test`（整个工作区 1342 个测试通过，跳过 3 个）、`just doctest`、`just deny` 全部通过。本 crate 238 个测试。
- 交错矩阵扩到 8 种变化（加「语言包升级」：k.jpack 所在的语言包换版本，换表之后、重建映射之前）、6 个位置（加「变化之后立刻查询」：核对任务还排在后面，切换之后没有可用快照，其余仍用上一个快照），复用预取另加「预取与查询之间」：4 种查询共 200 格，192 格的变化确实发生（`Named` 的 8 格「读文件前」走不到）。另有 `view_folder_project_opening_has_no_snapshot_before_its_overlay`：核对任务的测试钩子（`Inner::after_base`）在基础映射建好、覆盖部分还没读的时候取快照，必须没有；打开前就问的 `Named` 等到发布，结果不是没有 c.ts 的 `Complete`。分别去掉修复时：快照取当前的全局语言表，`Named Before Upgrade` 失败（`complete but neither view's`，少了 k.jpack）；`rebuild_base` 之后立刻发布，文件夹项目的测试失败（窗口里有快照）。
- 语义仲裁：`escalate_arbitration_by_outcome`，SCIP（`head_only`）与 jdtls 各四种结果（等待、成功、出错、超时）、两种回复顺序，共 32 种；恢复原来的规则（出错的回复也拥有与 HEAD 相同的文件）时 `scip Err, jdtls Ok` 失败。
- 压力运行（同时 4 个占满 CPU 的进程）：`view_*`、`cut_*`、`read_verify_*`、`escalate_*`（30 个测试）分两次各 `--stress-count 75` 全部通过（合计 150 次，分别 2589 s、2539 s）；整个本 crate（238 个）分两次各 `--stress-count 15` 全部通过（合计 30 次）。
- 基准：`goto_indexed` 18 ms，`goto_first` 4.15 ms，`named_50k` 444 ms，`references_100k/all` 867 ms、`/first` 9.1 ms，`search_stop` 1 s，`names_100k` 780 ms，`resident_cloudprint` 650 KB，`resident_100k` 3.34 MB，`extract_throughput` 4.34 MB/s；全部在预算内。

第 4 步，完整性改为一次核算、预取按任务的快照沿用之后，同一台机器。

- `just check`、`just test`（整个工作区 1349 个测试通过，跳过 4 个；`lectern-platform` 的 `openat2_rejects_escape` 在一次满负载的运行里失败过一次，单独运行和再跑一遍都通过，本轮没有改它）、`just doctest`、`just deny` 全部通过。本 crate 245 个测试。
- 交错矩阵改用语言包的语言（`jpack`），8 种变化 × 7 个位置（加「`query` 返回之后、任务取快照之前」），复用预取另加「预取与查询之间」：4 种查询共 232 格，224 格的变化确实发生（`Named` 的 8 格「读文件前」走不到）。变化作用于对这种查询有贡献的文件：查引用的提交、淘汰、换映射、改文件作用于 e、f。
- 每项修复分别去掉时，对应的测试失败：预取不按任务的快照判断，`Prefetch AfterAsk Map` 失败（`complete but neither view's`：c.jpack 的两个 OID）；快照取当前的全局语言表，`Definition`、`References`、`Named` 的 `Before Upgrade` 都失败（原来只有 `Named` 能测出）；查引用把读不出的已入索引文件直接跳过，`References Step(Snapshot) Map` 失败（原来改的是没有引用的 d.ts，测不出）；`rebuild_base` 之后立刻发布，文件夹项目的测试失败；不计范围之外，`account_outside_the_scope_is_partial` 和 `derived_packages_tiers_and_search_scope_follow` 失败；满额模式不核对，`account_full_namespace_checks_the_hinted_files` 失败；已入索引的文件按 1 MiB 读、超过就跳过，`account_files_are_read_under_their_units_limit` 失败；`cancel` 在 `publishing` 锁里调用，`escalate_cancel_runs_outside_publishing` 失败；等着的查询不结束，`view_queries_end_when_no_snapshot_can_be_published` 失败（一直 `Running`）；比较失败的快照不标记，`view_map_without_the_worktree_is_never_complete` 失败；恢复原来的仲裁规则，`scip Err, jdtls Ok` 失败。
- 满额模式的代价（`goto_phases` 的 `goto_indexed_full_phases`，10 万文件、命名空间满）：核算核对约 52 000 个提示、108 条语句，`4 accounting` 中位数约 280 ms（范围里的核对 `4 search scope` 约 2.3 ms）；第一批（第 1–3 步，约 18–22 ms）不变，`Done` 晚约 280 ms。没满时核算只是一次遍历，`4 accounting` 约 0.95 ms。
- 压力运行（同时 4 个占满 CPU 的进程）：`view_*`、`cut_*`、`read_verify_*`、`escalate_*`、`account_*`（37 个测试）分两次 `--stress-count 50` 和 `--stress-count 100` 全部通过（合计 150 次，555 s、1301 s）；整个本 crate（245 个）`--stress-count 30` 全部通过（1679 s）。
- 基准：`goto_indexed` 19.1 ms（单独测 17.4–19.3 ms；核算的遍历约 0.95 ms，原来搜完就报 `Complete`、不看子项目之外），`goto_first` 4.54 ms，`named_50k` 473 ms，`references_100k/all` 855 ms、`/first` 9.11 ms，`search_stop` 1 s，`names_100k` 870 ms，`warm_100k` 84 ms，`map_100k` 10.1 ms，`batch_peak` 33.3 MB；`extract_throughput` 一次 3.98 MB/s（与编译同时运行），单独再测 4.04 MB/s，本轮没有改抽取。

第 4 步，没有内容的单元与 `Unindexable` 计入核算、满额核对按时间盒、遍历缓存之后，同一台机器。

- `just check`、`just test`、`just doctest`、`just deny` 全部通过（整个工作区 1358 个测试通过，跳过 5 个）。本 crate 254 个测试。
- 每项修复分别去掉时，对应的测试失败：不看没有内容的单元，`account_too_large_units_are_searched`、`account_timed_out_units_are_not_indexed` 失败；`Unindexable` 覆盖项不计，`account_unindexable_worktree_files_are_unsearched` 失败；局部定义的查引用不看自己的文件，`account_local_references_need_their_file_searched` 失败；按 OID 读、映射里是另一份内容的文件没能搜时丢掉，`account_definition_read_as_an_object_counts_when_unsearched` 失败；满额核对不检查停止，`account_full_check_stops_with_the_query` 失败（关闭之后又做了语句）；不检查时间盒，`account_full_check_is_time_boxed` 失败；缓存的键不含提示置位次数，`account_walks_are_cached_until_the_index_changes`、文件夹项目的测试和 `view_matrix_definition`、`view_matrix_prefetch` 都失败。
- 跳转的分项（`goto_phases`，10 万文件）：重复跳转（遍历已缓存）`query to results` 16.2 ms、`4 accounting` 2.7 µs；索引变化之后的第一次（`goto_indexed_uncached_phases`，每次跳转前清掉缓存）19.5 ms、`4 accounting` 1.24 ms（这一次 `prepare` 也比前者慢约 2 ms，机器有波动）。
- 压力运行（同时 4 个占满 CPU 的进程）：`view_*`、`cut_*`、`read_verify_*`、`escalate_*`、`account_*`（46 个测试）`--stress-count 150` 全部通过（1825 s）；整个本 crate（254 个）`--stress-count 30` 全部通过（1575 s）。
- 基准：`goto_indexed` 17.3 ms（基准重复同一个跳转，量的是遍历已缓存的稳定状态；第一次跳转见上面的分项），`goto_first` 4.88 ms，`named_50k` 471 ms，`references_100k/all` 902 ms、`/first` 9.28 ms，`search_stop` 1 s，`names_100k` 916 ms，`warm_100k` 77.3 ms，`map_100k` 8.98 ms，`batch_peak` 33.3 MB，`extract_throughput` 4.31 MB/s。
- 满额模式（`CODE_INTEL_BENCH_FULL=1`，只记录）：`goto_indexed` 到 `Done` 287 ms（第一批不变；范围之外约 52 000 个提示在时间盒内核对完），`references_100k/all` 1.02 s，`search_stop` 1 s（没有入索引的文件，没有要核对的），`named_50k` 732 ms。核对超过查询的 1 s 时间盒时停下、没核对的计入 `outside`，所以 `Done` 不晚于时间盒加一条语句（500 个 OID，几毫秒）。

第 4 步，`Done` 替换没有内容的单元、来自索引的定义恢复局部性、分层要所比较的一方已知之后，同一台机器（rebase 到 #126 之后）。

- `just check`、`just test`（整个工作区 1409 个测试通过，跳过 5 个）、`just doctest`、`just deny` 全部通过。本 crate 256 个测试；整个本 crate `--stress-count 30` 在 4 个占满 CPU 的进程同时运行时全部通过（1533 s）。
- 每项修复分别去掉时，对应的测试失败：`Done` 不替换没有内容的单元，`account_too_large_units_are_searched` 失败（gen.ts 仍未搜，下一次跳转又抽取它）；`Named` 不恢复局部性、或查引用沿用给出的 `local`，`refs_nested_helper_from_named_is_local` 失败；没有当前文本时 `near` 路径算作当前文件、或两个都不在子项目里的文件算作同一子项目，`goto_tiers_need_what_they_compare_with` 失败。
- `goto_indexed`：分项（`goto_phases`）17.8 ms，`prepare` 17.5 ms，没有局部性的解析（基准的候选都是 `Public`），核算 2.8 µs；`just bench --measure code_intel/goto_indexed` 两次 22.7 ms（报告标为 noisy）、17.5 ms，这台机器的波动，跳转的路径与上一轮相同。

第 4 步，去掉来自索引的定义的局部性恢复（上一轮的 `recover_locality`，见「与设计的差异」）、查引用的局部性取自被查定义所在文件的那一次解析、写入时在写锁里查存在之后，同一台机器。

- `just check`、`just test`（整个工作区 1411 个测试通过，跳过 5 个）、`just doctest`、`just deny` 全部通过。本 crate 258 个测试；整个本 crate `--stress-count 30` 在 4 个占满 CPU 的进程同时运行时全部通过（1965 s）。此前一次中途停下的同样运行在第 16 轮有一次 `extract_markers_keep_their_names` 失败（walker 与 tags 的抽取结果不同）；两次抽取都按墙钟的交互截止时间，占满 CPU 时推测是其中一次超时，与本轮改动的代码无关，未复现。
- 解析次数由测试里的计数（`ParsedFile` 的每次解析都记下源码，`support::parsed`）直接检查：`Named` 列出 40 个 `File` 候选，不解析任何文件（`refs_named_parses_no_file`）；`Object` 一侧的跳转只解析自己的文本，不解析工作区里同一路径的文件（`goto_object_side_parses_only_its_text`）；从 `Named` 的候选查嵌套 `helper` 的引用，不论给出的 `local` 是什么，只解析 a.js 一次，结果是两个调用方、没有同名引用、`Complete`，`defines.local` 为 true；顶层定义给出 `local: true` 时改正为 false（`refs_nested_helper_from_named_is_local`）。
- `goto_indexed`：分项（`goto_phases`）19.7 ms，`prepare` 19.4 ms（其中 tree-sitter 解析 9.25 ms），核算 4 µs；`just bench --measure code_intel/goto_indexed` 三次 19.1 ms、24.1 ms（报告标为 noisy）、19.8 ms。跳转的路径与上一轮相同（去掉的恢复在基准里不运行：候选都是 `Public`；解析计数只在测试里编译），差别是第 1 步的解析，这台机器的波动。满额模式的分项 `query to results` 297 ms（只记录）。

第 4 步，份额按第一批计、当前文本的解析保留、按名字查找分页、核对按时间盒停止、查引用的截止时间与批次（[ADR 0037](../adr/0037.md)，见「与设计的差异」）之后，同一台机器（另一个 agent 同时在用，负载约 2）。

- `just check`、`just test`（整个工作区 1423 个测试通过，跳过 6 个，新增的是忽略的 `goto_indexed_cold_phases`）、`just doctest`、`just deny` 全部通过。本 crate 270 个测试；整个本 crate `--stress-count 30` 在 4 个占满 CPU 的进程同时运行时全部通过（1685 s）。上一轮中途那次 `extract_markers_keep_their_names` 的失败已找到原因：测试在取交互截止时间之后才编译 tags 查询，满负载时编译本身就用掉了 1 s；现在这几个测试先编译再取截止时间（`extract_*`、`forms_*` 里五处）。
- 每项修复分别去掉时，对应的测试失败：不查最近的文件，`goto_popular_name_nearest_first` 失败（第一批的第一个候选是 `aa/f000.ts`，不是同目录的 `zz/near.ts`）；搜索范围里的核对不检查时间盒，`account_full_scope_check_stops_with_the_query` 失败（`Complete`，应为 `Partial { unsearched: 7 }`）；查引用的截止时间只看是不是被查定义的文件，`account_files_are_parsed_under_their_units_limit` 失败（api.ts 没搜到）；`ci_ref` 一轮之后不交出、或搜索的文件之间不检查计时，`refs_indexed_references_go_out_before_the_search`、`refs_search_batches_go_out_on_time` 失败。
- 分项（`goto_phases`，10 万文件，100 KB 的当前文本，300 次的中位数）：解析已保留时 `prepare`（第 1–3 步）0.63 ms，其中按名字查（SQL）0.41 ms、过滤与分层 0.16 ms，查询任务 0.83 ms；没保留时（冷路径）`prepare` 27.2 ms，其中光标处的解析与 locals 24.4 ms，查询任务 27.4 ms（`query to results` 63 ms 含之后在后台整份解析并保留的约 35 ms，不在第一批之前）。
- 基准（`just bench --measure code_intel/ plugin/code-intel/`，从查询到第一批有候选的事件 / 到 `Done`）：`goto_indexed` 1.08 ms / 1.18 ms；遍历未缓存 `goto_uncached` 1.18 ms / 2.59 ms；约 3000 个定义的常见名字 `goto_popular` 5.31 ms / 31.7 ms（第一次单独测 4.99 ms / 29.4 ms）；满额 `goto_full` 1.28 ms / 最大 345 ms（≤ 1.02 s）；冷路径 `goto_cold_parse` 30.1 ms（第一次 33.1 ms，都在 50 ms 之内）；同文件 `goto_same_file` 16 µs；对象一侧 `goto_object` 1.14 ms；`goto_first` 11.7 ms。`parse_cache_bytes` 两份 100 KB 9.15 MB（每字节源码 46.6 字节，估算 9.85 MB）。`references_100k/all` 1.04 s、`/first` 11.1 ms，满额 `references_full/all` 1.15 s、`/first` 12.1 ms；`named_50k` 11.7 ms（量的是第一批，现在是第一页加最近的文件；原来的 471 ms 是查全部定义的一批）；`search_stop` 1 s；`names_100k` 995 ms；`names_100k_bytes` 4.66 MB；`warm_100k` 96.3 ms；`map_100k` 10.4 ms、`map_100k_bytes` 2.65 MB；`batch_peak` 33.3 MB；`resident_cloudprint` 650 KB、`resident_100k` 3.34 MB（测量前丢掉那次跳转保留的解析，见 ADR 0037）。`extract_throughput` 3.28 MB/s，单独再测 3.82 MB/s，低于 4 MB/s：本轮没有改抽取，测量时机器负载约 2，前几轮 3.9–4.3 MB/s。
- 存储：`ci_def_name` 带上 `unit` 之后，`disk_100k` 的分库文件 311 MiB（含 `-wal`，原来 298 MiB），声明的字节数不变（279.7 MB）。

同一轮 rebase 到 #129（[ADR 0035](../adr/0035.md)）之后，ADR 编号改为 0037；看门狗在系统时钟下等到最早的到期时刻（见「与设计的差异」）。

- `just check`、`just test`（整个工作区 1435 个测试通过，跳过 6 个）、`just doctest`、`just deny` 全部通过。本 crate 271 个测试，新增 `deadline_watchdog_sleeps_until_a_far_due_entry`；用到看门狗到期表的 `deadline_*`、`watchdog_*`、`names_*`、`prefetch_*` `--stress-count 30` 全部通过。
- 基准（从查询到第一批有候选的事件 / 到 `Done`）：`goto_indexed` 844 µs / 935 µs；`goto_uncached` 982 µs / 2.16 ms；`goto_popular` 4.39 ms / 26 ms；`goto_full` 1.07 ms / 最大 283 ms；`goto_cold_parse` 23.8 ms；`goto_same_file` 10.4 µs；`goto_object` 866 µs；`goto_first` 9.23 ms。`named_50k` 10 ms，`references_100k/all` 833 ms、`/first` 9.33 ms，`references_full/all` 1.02 s、`/first` 9.67 ms，`search_stop` 1 s，`names_100k` 815 ms、`names_100k_bytes` 4.66 MB，`warm_100k` 73.6 ms、`warm_100k_touch` 284 ms，`map_100k` 8.45 ms、`map_100k_bytes` 2.65 MB，`batch_peak` 33.3 MB，`parse_cache_bytes` 9.15 MB，`resident_cloudprint` 650 KB、`resident_100k` 3.34 MB，`disk_100k` 311 MB；`extract_1mib` 4.28 MB/s（只记录）。全部在预算内。
- `extract_throughput` 4.71 MB/s（≥ 4），上一轮的 3.28 / 3.82 MB/s 是负载约 2 时测的。本分支涉及抽取的改动只有：`Extraction::empty` 改为 `pub(crate)`；locals 的 `Collector::reference` 对普通引用提前返回（`is_plain_reference`）；语言表加了位置和跳转的查询；看门狗的到期项。都不在抽取的热路径上。
- 再 rebase 到 #133（含 [ADR 0036](../adr/0036.md) 的封顶采样和共享夹具）之后：`just check`、`just test`（1441 个通过，跳过 6 个）、`just doctest`、`just deny` 全部通过。基准（负载约 1.6–2）：`goto_indexed` 1.06 ms / 1.16 ms，`goto_uncached` 1.13 ms / 2.54 ms，`goto_popular` 4.84 ms / 28.1 ms，`goto_full` 1.14 ms / 最大 274 ms，`goto_cold_parse` 28.1 ms，`goto_first` 12.1 ms，`named_50k` 11.6 ms，`references_100k/all` 987 ms、`/first` 10.3 ms，`references_full/first` 10.5 ms，`search_stop` 1 s，`names_100k` 873 ms，`warm_100k` 86.2 ms，`map_100k` 9.82 ms，`parse_cache_bytes` 9.15 MB，`resident_100k` 3.34 MB；预算项全部在内，只有 `extract_throughput` 在整组运行里两次都是 3.83 MB/s，随后单独测 4.06 MB/s（负载约 1.1）。这组基准合计 1878 s：10 万文件的查询基准各自生成一次虚拟仓库，每个 115–135 s，`resident_100k` 332 s。
- 共享夹具（[ADR 0036](../adr/0036.md)，[bench 设计「夹具与 `test-support`」](bench.md#夹具与-test-support)）：10 万文件的虚拟仓库每个基准目标只写一次（`testing::VirtualTemplate`），基准和 `resident_100k` 的每个样本各复制一份缓存目录、在同一个公共目录路径上新建自己的 `FakeGit` 和 rig，所以写入、访问时间和淘汰都只落在自己的副本上；冷状态照旧（`goto_cold_parse` 每次清掉保留的解析，`goto_uncached` 每次清掉遍历缓存，`resident_100k` 每个样本一个新 rig，`names_*` 每个样本重建名字表）。`goto_popular` 的常见名字并入共享仓库（每 17 个文件一个 `fetchShared`），其他基准的单元也带着它，名字表多一个名字（290 001 个）。`just bench --measure code_intel/ plugin/code-intel/` 的墙钟时间（报告的 `benches`）合计 1898 s → 567 s：用虚拟仓库的 12 个基准 1693 s → 318 s，其中各目标第一个用到它的基准（`time` 的 `goto_cold_parse` 120 s、`memory` 的 `names_100k_bytes` 125 s）包含写仓库，其余每个 2–16 s（复制约 1–2 s）。没有改的 `disk_100k` 两次分别 67 s、114 s，第二次负载约 3.4。测量值与前一轮相同，只有 `names_100k` 这次是 1.01 s（负载约 3.4），单独再测 957 ms；前几轮 780–995 ms。

第 4 步，交互份额的第二轮自审之后（「与设计的差异」），rebase 到 #134 之后，同一台机器。

- `just check`、`just test`（整个工作区 1456 个测试通过，跳过 7 个）、`just doctest`、`just deny` 全部通过。本 crate 277 个测试，`--stress-count 30` 全部通过（867 s，负载约 3）。新增：`store_lookup_pages_bound_the_rows_read`、`store_lookup_pages_go_on_inside_a_unit`、`store_def_names_in_pages`、`goto_popular_name_nearest_in_a_deep_subproject`、`goto_current_text_parse_stays_while_current`、`account_references_full_check_has_its_own_time_box`。把最近文件的取法换回原来的（按路径走子树、截在 2048 个）时，`goto_popular_name_nearest_in_a_deep_subproject` 失败（第一批的前两个是 `web/a/d00/f0000.ts`、`web/a/d00/f0040.ts`，不是同目录的两个）。`goto_current_text_parse_stays_while_current` 和 `account_references_full_check_has_its_own_time_box` 断言的正是原来不成立的行为（读五分钟后跳转不重新解析；1.2 s 的 `ci_ref` 一轮之后仍是 `Complete`），没有对原来的代码单独运行。
- 名字表的分项（`goto_phases` 的 `names_100k_phases`，release）：10 万文件、270 万条定义、29 万个名字。按索引顺序分页之后、按单元二分查找时 998 ms，其中读索引 918 ms（660 条语句），`ci_ext` 一遍 25 ms、按映射过滤 15 ms、建表 35 ms。按索引顺序读时相邻两行几乎总是不同的单元，原来按「上一个单元」缓存的判断不再命中；3 万文件时同样的读取不做任何逐行处理是 92 ms、按二分查找 256 ms，改成单元 ID 的位图并只为要的单元读名字之后 189 ms。
- 基准（`just bench --measure code_intel/ plugin/code-intel/`，负载 1–3）：`goto_indexed` 1.32 ms / 1.43 ms；`goto_uncached` 1.94 ms / 3.89 ms；`goto_popular` 4.49 ms / 36.5 ms；`goto_full` 1.33 ms / 最大 339 ms，每次 `Complete`；`goto_cold_parse`（当前文本 126 KiB）35.3 ms，前一次 35.9 ms，超过 32 ms（Linux 只报告，100 KB 时是 28 ms）；`goto_same_file` 11.6 µs；`goto_object` 1.23 ms；`goto_first` 10.6 ms；`named_50k` 11.5 ms；`search_stop` 1 s；`references_100k/all` 1.01 s、`/first` 10.8 ms；`references_full/all` 1.2 s、`/first` 10.8 ms，每次 `Complete`（原来 1.02 s 是被截断的 `Partial`）；`names_100k` 704 ms（原来 873–988 ms）、`names_100k_bytes` 4.66 MB；`parse_cache_bytes` 在上限处 11.6 MiB（两份共 260 186 字节源码，每字节 46.7 字节，估算 12.49 MB，≤ 12 MiB）；`warm_100k` 86.1 ms；`map_100k` 9.7 ms；`batch_peak` 33.3 MB；`resident_100k` 3.35 MB；`extract_throughput` 3.98 MB/s（本轮没有改抽取，同前几轮在 4 MB/s 上下）。合计 515 s。
- rebase 到 #135 之后在最终的代码上重跑：`just check`、`just test`（1461 个通过，跳过 7 个）、`just doctest`、`just deny` 全部通过；本 crate `--stress-count 30` 全部通过（800 s）。基准（负载 1–3，比上面低）：`goto_indexed` 1.23 ms / 1.33 ms，`goto_uncached` 1.16 ms / 2.29 ms，`goto_popular` 4.36 ms / 35.8 ms，`goto_full` 1.43 ms / 最大 332 ms，`goto_cold_parse` 32.4 ms（判定 32 ms，Linux 只报告），`goto_object` 1.3 ms，`goto_first` 8.79 ms，`named_50k` 10.5 ms，`search_stop` 1 s，`references_100k/all` 814 ms、`/first` 9.36 ms，`references_full/all` 1.05 s、`/first` 9.31 ms，`names_100k` 544 ms，`names_100k_bytes` 4.66 MB，`parse_cache_bytes` 11.6 MiB，`resident_100k` 3.35 MB，`warm_100k` 79.2 ms，`map_100k` 7.78 ms，`extract_throughput` 4.47 MB/s；合计 482 s。
- 冷路径的判定点改到 96 KiB（[ADR 0037](../adr/0037.md)：128 KiB 的整次冷跳转 29.6–35.9 ms，与 32 ms 没有余量）之后：`goto_cold_parse`（当前文本略小于 96 KiB）24.7 ms ≤ 32 ms，`goto_cold_parse/at_128k` 29.6 ms（只记录；前几次 32.4–35.9 ms，负载更高）；其余跳转 `goto_indexed` 1.19 ms / 1.28 ms，`goto_uncached` 1.81 ms / 3.46 ms，`goto_popular` 4.65 ms / 38.3 ms，`goto_full` 1.43 ms / 最大 320 ms，`goto_object` 1.22 ms，`goto_first` 9.25 ms（`just bench --measure code_intel/goto_`，负载 1–2）。`just check`、本 crate 与 bench 的测试（347 个）通过。
- 第三轮（查引用第 2 步按时间盒截断，「与设计的差异」）之后：`just check`、`just test`（1462 个通过，跳过 7 个，新增 `refs_indexed_files_stop_at_the_time_box`）、`just doctest`、`just deny` 通过；`just bench --measure code_intel/references`：`references_100k/all` 779 ms、`/first` 9.04 ms，`references_full/all` 1.17 s、`/first` 10 ms，每次 `Complete`（300 个调用方都在时间盒之内）。
- 没有验证的：`goto_cold_parse` 在 M 系列上的值。Linux 上 128 KiB 是 30–36 ms，按 ADR 0034 整文件解析约 6 MiB/s 的估算还要加 locals，32 ms 的判定在 macOS 上是否通过要看 CI。

删除电源代码（ADR 0032，「与设计的差异」），基于 8ca16f1，同一台机器；之后 rebase 到包含 #136 的 main（bea815a）。

- `just check`、`just test`（1459 个通过，跳过 7 个；少了 3 个 `battery_*`）、`just doctest`、`just deny` 通过。本 crate `--stress-count 30` 全部通过（729 s）。`rg -n 'PowerSource|AlwaysOnAc|FakePower|Deferred|wide_allowed|power_changed' crates` 没有结果。
- 去掉 `query` / `names` 里的 `demand` 之后，`account_timed_out_units_are_not_indexed` 的活性检查失败（查询的限时搜索命中 `TimedOut` 的单元、排进访问标记，没有唤醒批量任务）；补上批量任务之外的写入之后的唤醒后通过。
- 基准（`just bench --measure code_intel/`，负载 1–3）：`warm_100k` 73.1 ms、`warm_100k_touch` 248 ms，`map_100k` 7.68 ms、`map_100k_bytes` 2.65 MB，`batch_peak` 33.3 MB，`disk_100k` 316 MB（9 万个单元、80 次淘汰），`extract_throughput` 4.96 MB/s；`goto_indexed` 1.18 ms / 1.27 ms，`goto_uncached` 1.28 ms / 2.46 ms，`goto_popular` 4.52 ms / 36.7 ms，`goto_full` 1.4 ms / 最大 389 ms，`goto_cold_parse` 23.5 ms，`named_50k` 10.7 ms，`references_100k/all` 816 ms，`references_full/all` 1.09 s，`names_100k` 489 ms，`names_100k_bytes` 4.66 MB，`parse_cache_bytes` 11.6 MB；全部在预算内，合计 477.6 s。

第 7 步（交互让路），基于包含 [#139](https://github.com/SoloJiang/lectern-app/pull/139) 的 main（0238432），同一台机器（Linux x86_64，4 核 Xeon 2.1 GHz）。

- `just check`、`just test`（整个工作区 1475 个测试通过，跳过 7 个）、`just doctest`、`just deny` 全部通过。本 crate 291 个测试，新增 10 个 `yield_*`（设计「测试」点名的 `yield_tier3_does_not_yield`、`yield_tier3_work_does_not_wait_for_a_slot`、`yield_resumed_extraction_is_identical`、`yield_parked_time_is_not_deadline_time`、`yield_pause_reaches_all_paused`、`yield_large_parked_item_gives_way_to_tier3`，以及语言包放弃后重做、超过 256 KiB 的文件等开关关闭、暂停丢掉停下的状态、开关切换 100 次每个文件只写一次）。本 crate `--stress-count 30` 全部通过（862 s，负载约 2–3）；之后 `yield_resumed_extraction_is_identical` 加了 tsx 和接近 1 MiB 的一组，`yield_*` 再 `--stress-count 30` 通过（238 s）。
- `yield_resumed_extraction_is_identical`：每种核心语言（ts、tsx、js、rust、java、python、go）两组。120 KiB 的文件在保活时段里做，每 40 次让路检查让时段到期一次，解析中、遍历中都停过；1 MiB 减 4 KiB 的文件（后台上限 `BACKGROUND_MAX_LEN` 之内）每 200 次检查打开一次开关，关闭后在批次里接着做。两组写入的单元都与一次做完的 `extract` 相同。
- 变异检查：改掉剩余预算的计算（继续时不按剩余的预算重新登记）时，`yield_parked_time_is_not_deadline_time`、`yield_resumed_extraction_is_identical`、`yield_switch_toggled_100_times_writes_each_file_once` 失败；去掉第 3 层要 `LargeSlot` 时放弃本任务停下的大项的一支时，`yield_large_parked_item_gives_way_to_tier3` 卡住（单个批量任务等自己手里的 `LargeSlot`）。都已还原。
- 基准（`just bench --measure code_intel/yield_stop code_intel/warm_100k code_intel/extract_throughput code_intel/batch_peak`，负载约 1.5–2.4）：`yield_stop` 30 个样本，中位数 26.9 µs、p95 40.5 µs、最大 58.9 µs（≤ 10 ms；运行器标为 noisy，最大值约为中位数的两倍）；`batch_peak` 33.3 MB（≤ 40 MB，与前几轮相同）；`warm_100k` 75.9 ms、`warm_100k_touch` 318 ms（前几轮 73–86 ms、248–284 ms）；`extract_throughput` 4.02 MB/s（≥ 4，前几轮 3.8–5.0 MB/s）。这几项都在开关关闭时运行，批次照旧，每个边界多一次 `proceed_item`。
- 没有验证的：保活时段下批量层合计 ≤ 一个核的 10%（「性能」）没有基准，由 kernel 的时段长度和间隔（50 ms / 500 ms）加上「一个时段只做一项」保证；macOS 上的数值看 macOS CI。

第 7 步自审之后（换项目时放弃停下的项、暂缓列表、`LargeSlot` 的接手，「与设计的差异」），同一台机器。

- `just check`、`just test -p lectern-code-intel`（293 个测试通过）通过；`yield_*` 与 `schedule_*` `--stress-count 30` 全部通过（380 s）。新增 `yield_parked_item_of_another_project_is_given_up`、`yield_large_slot_taken_over_from_a_parked_task`；`yield_large_file_waits_for_the_switch` 改为两个大文件排在最前（小文件在时段里做完、大文件按原顺序在开关关闭后做完），`yield_language_pack_is_given_up_and_redone` 检查同一层后面的项照常前进、语言包只试一次、开关关闭后重做的单元与一次做完的相同，`yield_large_parked_item_gives_way_to_tier3` 分别用一个和两个批量任务。
- 变异检查：去掉边界上的换项目和会话检查时，`yield_parked_item_of_another_project_is_given_up` 失败；不让路的请求方不接手、照旧等持有者时，`yield_large_parked_item_gives_way_to_tier3` 的两个任务一组卡住；时段里读出的大文件不放进暂缓列表时，`yield_large_file_waits_for_the_switch` 和 `yield_large_parked_item_gives_way_to_tier3` 失败；语言包的项照旧放回队列前面时，`yield_language_pack_is_given_up_and_redone` 失败。都已还原。
- 基准（`just bench --measure code_intel/yield_stop code_intel/batch_peak`，负载约 2–2.8）：`yield_stop` 中位数 33.1 µs、p95 70.2 µs、最大 110 µs（≤ 10 ms；这次的改动不在抽取里，与上一轮的差别是负载）；`batch_peak` 33.3 MB（≤ 40 MB）。

## 与设计的差异

前置条件 A（`services::code_intel`，[#108](https://github.com/SoloJiang/lectern-app/pull/108)）。下面只记上文「服务接口」「语义导航的注册」没有定下、实现时定下的形状；`BlobId` 与 `FileList::oid` 的形状记在 [project 设计「与设计的差异」](project.md#与设计的差异)。

- `NameTable` 的构造：上文没有给构造方法。实现加了 `NameTable::builder(generation)`，返回 `NameTableBuilder`（`push(name, kinds)`、`build()`）。`build` 按字节序排序，同名条目合并为一条、种类位集取并集；NFC 规范化由本 crate 在 `push` 之前完成，`services` 不依赖 `unicode-normalization`。`kinds(i)` 下标越界时返回 0。
- `NameTable` 的序列化：`{ "generation": n, "entries": [[名字, 种类位集], …] }`，与 `FileList` 一样不暴露缓冲与偏移，反序列化经 `NameTableBuilder`。
- `SemanticReply` 与 `ProviderRegistration`：字段属于本 crate（队列、登记表），`services` 里放不下。实现为包着回调的类型：本 crate 用 `new` 传入回调；`SemanticReply::send` 调用回调，没有调用就 drop 时回调随之 drop、不被调用；`ProviderRegistration` drop 时调用一次回调。两者都是 `Send`，不序列化。
- `SemanticRequestId` 上文没有定义，实现为 `SemanticRequestId(pub u64)`，与 `QueryId` 相同。
- 派生：除上文要求的标准派生外，小的键和值类型另派生 `Copy`、`Hash`（与 `services::project` 相同）；`CandidateTier` 另派生 `Ord`，顺序就是声明顺序，也就是候选列出的顺序。

第 1 步：

- `LangName`、`DefKind`、`RefSyntax` 和位置分析用的 `Position` 用 `services::code_intel`（前置条件 A）里的类型，本 crate 在根模块重新导出，没有自己的副本。`services` 里的这些类型没有方法，`LangName` 也不派生 `Ord`、不实现 `Display`；本 crate 需要的部分写成自由函数（`lang_name`、`DEF_KINDS`、`def_kind_from_tag`、`def_kind_bit`、`def_kind_name`，见「本 crate 的公开项」），日志和测试里的名字取 `.0`。形状都与 `services` 相同，这个 PR 不改 `services`。
- `extract` 的签名：`deadline` 改为 `&Deadline`，`cancel` 改为 `&Arc<AtomicBool>`。tree-sitter-tags 只接受一个取消标志，所以每个在途抽取在看门狗登记自己的标志，由看门狗把截止时刻、调用方的取消标志、暂停探针和 `stop` 汇总到这个标志上；看门狗线程要持有调用方的取消标志，只能是共享的 `Arc`。`Deadline` 同时带上大小上限（后台 1 MiB、交互 4 MiB），因为两者总是成对出现。「接口」已改。
- `ExtractStatus` 增加 `Unsupported`（grammar 或查询编译失败，不写入缓存）：设计在「状态与异常」里规定这种语言停用，但抽取函数没有对应的返回值。
- tree-sitter-tags 只在两次匹配都还在队列里时去重，实测 Rust 的方法会再作为函数出现一次（`impl` 里带泛型参数的方法），所以抽取后按名字节点再去重一次（「抽取结果」已补）。定义自己的名字不进入引用名。
- 可见性和参数个数不是从定义节点读出的：tree-sitter-tags 不交出语法树，再解析一次要多花一倍以上的时间（只解析的时间约占抽取的一半）。改为让查询捕获规则要用的内容：`@reference.lectern.*` 标记（`pub`、`private`、`export`、`import`、`impl` 块、Go 的接收者）和 `@doc` 捕获的参数列表，规则见「抽取结果」。查询因此不再用 `@doc` 捕获注释，`Def` 里本来也没有文档。
- JS / TS 的可见性按文件是否为 ES 模块区分：没有 `import` / `export` 的脚本和 CommonJS 文件（`module.exports = { … }`）顶层是 `Public`，否则这些文件里的函数会被可见性过滤掉，跨文件跳转找不到它们。`export { name }` 和 `export default name` 导出的名字也是 `Public`。类、对象字面量和命名空间里的成员取最外层定义的可见性；TS 的 `private` 成员和 JS 的 `#私有` 成员仍是 `Public`（设计只规定了顶层定义）。
- 查询文件按 grammar 的节点集合分目录（`ecmascript`、`js`、`jsx`、`ts`，见「外部格式」），而不是每种语言一个目录：查询里出现 grammar 没有的节点（TS grammar 没有 JSX 节点和 `field_definition`）时编译失败。
- 上游没有 locals 查询的 Rust、Java、Python、Go 由 Lectern 编写，标识符引用的模式加了 `(#is-not? local)`：tree-sitter-tags 只对带这个谓词的模式过滤局部引用，上游的查询都没有带。
- 查询版本另外包含 tree-sitter 和 tree-sitter-tags 的版本（「语言与抽取键」已改）。
- 语法种类表里 Go 的 `MemberCall` 也接受 `Type`：`units.Meters(value)` 和 `Meters(value)` 一样是类型转换。
- `site` 模块（`ParsedFile`、位置分析、语法种类表）是公开的，供第 4 步的查询、测试和以后的 `code_intel/goto_indexed` 使用；设计原来只说内部使用 `ParsedFile`。
- `Watchdog` 以 `Starter` 回调启动线程：本 crate 第 1 步没有插件，测试和 bench 经 `testing::watchdog` 在 `Executor::run_dedicated` 的专用线程上运行它，第 4 步的插件改用 `own_with` 和 `run_dedicated_gated`。`stop` 和 `stop_guard` 已实现。启动方经 `WatchdogRun::watchdog` 把 `stop` 登记到 `Executor::on_shutdown`（前置条件 D，[#109](https://github.com/SoloJiang/lectern-app/pull/109)），线程退出时 drop 登记：第 1 步由 `testing::watchdog` 这样做（`deadline_executor_shutdown_stops_the_watchdog`），第 4 步的插件在 `run_dedicated_gated` 的启动方里做同样的登记。看门狗的到期表（名字表的延迟重建）随第 4 步的名字表加入。
- `lectern-executor` 在第 1 步是可选依赖，只有 `test-support`（`testing::watchdog`）打开它；第 3 步起是普通依赖。`lectern-services` 第 1 步起是普通依赖（上面的共享类型）。`lectern-kernel` 等其他依赖随用到它们的步骤加入。
- `testing` 第 1 步只有 `ManualClock`、`FakeGrammars`（安装、更新、移除、计数加载次数）、`java_pack`、`watchdog`、语言夹具 `FIXTURES` 与 `corpus`、病态输入 `pathological_js`；其余替身随用到它们的步骤加入。
- 「性能」里本 crate 的其余分项预算还没有登记进 bench 的预算表，随实现它们的步骤登记。

第 1 步，处理 [#113](https://github.com/SoloJiang/lectern-app/pull/113) 的审阅意见：

- 语法种类表的 `Member` 原来只接受 `Field`、`Method`、`Constant`、`Variable`。`const cb = utils.handle`、`models.User` 这类成员是函数和类，被过滤掉了。改为只在 Rust 里收窄，其他语言接受全部种类（「候选的分层与过滤」已改）。
- 设计没有规定 locals 查询要捕获嵌套声明的名字，`def outer(): def helper(): …; helper()` 里的调用因此没有局部绑定。现在捕获，并规定声明名属于外层作用域（「外部格式」已补）。Go 没有嵌套的具名函数，不需要。
- 设计没有说明 `(identifier) @local.reference` 会匹配成员名和路径段，`int add = 0; items.add(x)` 里的 `add` 因此绑定到了局部变量。现在按位置表排除（「外部格式」已补）。tree-sitter-tags 一侧，成员调用的模式本来就不带 locals 谓词，兜底的成员模式也不带。
- 引用名原来只有调用、类型、构造和实现，`ci_ref` 选不出只有普通用处的文件。现在加了兜底模式（「抽取结果」「外部格式」已补）。同一个文件里对根作用域局部定义的用处（Python 模块级的 `TAX_RATE`）仍按设计不进入引用名，这个文件就是定义所在的文件，查引用时总会现场解析。（第二轮之后，函数里对它的用处会进入引用名，见下面第二轮的 `local.scope-inherits false`。）
- `code_intel/extract_throughput` 的测量方式改为仓库形状，另加只记录的 `code_intel/extract_1mib`，预算不变，见 [ADR 0031](../adr/0031.md)。设计原来没有规定语料的形状，第 1 步用的是每种语言一个 1 MiB 的文件。

第 1 步，处理 [#113](https://github.com/SoloJiang/lectern-app/pull/113) 第二轮审阅意见：

- 设计只说「locals 查询在作用域里解析到的定义是 `Bound(Local)`」，没有规定每种语言的声明形式、作用域、提升和模式，实现按名字找最内层作用域里的定义，出现了四类错误的直接跳转：引用在声明之前时取了之后的声明（`before.or(after)` 不区分是否提升）、Python 方法看到了类体里的名字、JS 的 `var` 按块处理、Rust 的嵌套模式没有展开。现在按新增的「绑定规则」解析，并加了安全不变式：不确定可见的局部声明只给候选，不直接跳转，也不允许跳到别的文件（「跳转到定义」的 `jump` 规则已补）。`Site` 因此多了 `possible` 字段和 `may_jump_elsewhere`。
- locals 查询改为按声明形式捕获：JS / TS 原来用上游的 `pattern/identifier`，它也匹配解构赋值（不是声明）；Rust、Java、Python、Go 的查询加了块、`for`、`match` 分支、`catch`、`switch` 分组等作用域，以及推导式、`:=`、`global` / `nonlocal`、`with` / `except … as`、Java 的局部类与模式、Go 的包级声明。import 仍不是局部定义。
- 函数作用域在 tree-sitter-tags 一侧标成 `local.scope-inherits false`。ADR 0031 记下这一项时是作为提速手段放弃的（它改变了被过滤的引用）；这一轮它是为了正确性：只用所在函数自己的局部定义过滤，Python 的类体不再挡住方法里的模块名，外层函数的同名局部不再让一个名字从引用名里消失。变化的方向都是引用名多出名字。
- 设计没有规定 grammar 租约相对其他字段的 drop 顺序。语言包的条目原来先 drop 租约、后 drop 编译好的查询，`ParsedFile` 先 drop 持有租约的语言、后 drop 语法树，库可能在查询和语法树释放之前被卸载。现在租约在最后（「语言包的变化」已补），并清掉每线程 `TagsContext` 里指向语言包的解析器和游标。
- 设计没有说枚举成员是定义：Rust 的变体、TS 的枚举成员和 Python 的类属性原来不在定义里，Java 的枚举常量有定义但可见性按自己的修饰符算（总是 `Public`）。现在都是定义，可见性取枚举的（「抽取结果」已补），Rust 的 `Call` 因此也接受 `Constant`（「候选的分层与过滤」已补）。

第 1 步，按 [ADR 0031](../adr/0031.md) 改用一次遍历抽取：

- 设计按 ADR 0030 用 tree-sitter-tags 抽取全部语言。核心语言改为 `walk` 模块的一次遍历（「外部格式」已补），语言包不变；`ParsedFile` 的定义随之取遍历的结果。原因是 tree-sitter-tags 路径在 macOS CI 上随机地低于 4 MB/s 的预算，而查询本身的开销就超出了预算（ADR 0031 的「背景」）。查询仍是规格，两条路径由 `extract_walker_matches_tags` 约束。
- Go 包级的 `const A, B = 1, 2`：查询引擎只匹配到 `A`，遍历把 `B` 也记为定义（「外部格式」已补）。
- 同一个节点上的几个定义（Go 的 `const A, B`、`name, kind string`，Java 的 `int a, b;`）原来互为 `scope`（后一个的 `scope` 是前一个的名字），现在都取外层的定义。两条路径共用这条规则。
- `EXTRACTOR_VERSION` 从 1 改为 2：上面两项改变了抽取结果。

第 1 步，处理 [#113](https://github.com/SoloJiang/lectern-app/pull/113) 第三轮审阅意见：

- `ParsedFile` 在核心语言上第二次遍历语法树时用了一个永远清零的标志，截止时间和取消在遍历时不起作用。现在遍历用 `ParsedFile` 自己的截止时间和取消标志，返回之前再检查一次（「抽取结果」已补）。检查过的其他路径：`extract` 的解析和遍历都读看门狗的标志，遍历之后再读一次；tree-sitter-tags 路径不变。
- 设计没有逐一列出每种语言引入名字的语法形式，几轮审阅分别找到漏掉的形式。现在有「定义形式」的表，由 grammar 的 `node-types.json` 检查覆盖（「定义形式」已补）。表带来的改动：
  - 新的定义：Java 记录头的组件（字段）和 `@interface` 的元素（方法）；TS 的参数属性（字段）、顶层的 `declare const` / `let` / `var`、`namespace A.B` 的最后一段；Rust trait 的关联类型；Python 的 `type` 别名（含泛型别名）。
  - Java 记录头的组件原来是文件作用域里的局部定义，文件里同名的引用都绑定到它；现在只有方法、构造器和 lambda 的参数是局部。
  - 新的局部绑定：各语言的类型参数，TS 的映射类型键和块里的接口、类型别名、枚举、抽象类，Rust 块里的结构体、枚举、联合、trait、类型别名和 const 参数，Python 的 `match` 捕获和类型参数。局部绑定分值和类型两个命名空间，类型位置的引用找类型（「绑定规则」已补）。
  - 不建模、只给索引候选的形式记为「忽略」并写明原因（TS 的 `infer`、Rust 的 C 变长参数、Go 方法接收者里的类型参数等）。
- 设计没有规定作用域节点里哪些部分在外层求值，Python 的参数默认值和标注绑定到了参数本身。现在按「作用域入口」的表解析：Python 的默认值、标注、返回类型、基类和推导式的第一个可迭代对象，JS / TS 的计算方法名和参数装饰器；JS 的 `var` 放进函数体，参数默认值看不到它（「绑定规则」已补）。
- 语法种类表原来是几条通用规则加两条追加，Rust 的记录变体（`Message::Move { x, y }`，`Construct` / `Type`）、JS 的函数字段、Java 记录的访问器等因此被过滤掉。现在由「定义形式」推出（「候选的分层与过滤」已补），Rust 的 `Member` 因此只接受 `Field`、`Method`（原来也接受 `Constant`、`Variable`，Rust 没有这样的成员）。Rust 路径形式的结构体构造（`a::B { .. }`）也归为 `Construct`。
- `ParsedFile` 给局部类型（类型参数、局部接口）的 `SiteDef` 种类是 `Type`，原来都是 `Variable`。

第 1 步，处理 [#113](https://github.com/SoloJiang/lectern-app/pull/113) 第四轮审阅意见：

- 局部定义的可见性原来按语言写规则，只有 JS / TS 把函数体里的定义记为 `File`，Python 嵌套在函数里的 `def` 是 `Unknown`，Rust 函数里的 `fn`、Java 的局部类和匿名类的方法、Go 函数里的类型也都按顶层的规则算，会成为其他文件的候选。现在在每种语言里由作用域推出：定义节点在局部作用域里的是 `File`，成员取所属的类（「抽取结果」已改）。`rules::finish` 因此多一个局部作用域的参数，遍历在同一遍里记下作用域，tags 路径（核心语言）用 locals 查询再算一次；`EXTRACTOR_VERSION` 从 2 改为 3。
- 设计没有逐一列出开作用域的语法形式。现在有「作用域形式」的表，由 grammar 检查覆盖、由 locals 查询检查一致（「作用域形式」已补）。表带来的改动：
  - JS / TS：`catch` 子句是作用域（参数原来漏到外面的块里）；`switch` 体是作用域（`case` 里的 `let` 原来在 `switch` 之外也可见）；`with` 体是动态作用域；TS 的签名和函数类型（`function_signature`、`method_signature`、`function_type` 等）是函数作用域，参数原来漏到所在的块或文件里，同一文件里同名的引用会绑定到签名的参数；namespace、module 和 `declare global` 的体是命名空间，其中的定义是成员，不是局部。
  - Rust 的 `function_signature_item` 和 `function_type`、Go 的接口方法和函数类型是函数作用域，理由同上；Go 的 `default` 分支是作用域。
  - Java 的作用域从 `switch` 分组改为 `switch` 块（一组里声明的局部变量在后面的组里也可见，JLS 6.3）和 `case … ->` 规则。
  - Java 增强 `for` 的变量是这个作用域节点的 `name` 字段，原来按函数名的规则放进了外层作用域，循环之后同名的引用会绑定到它；现在这条规则只用于函数和类。
- `global` 和 `nonlocal` 原来一样处理：都只是把名字移出所在函数，再从外层函数找。现在 `global` 直接跳到模块，`nonlocal` 不找到模块（「绑定规则」的作用域指令已补）。
- 模块级和类体里的解包（`A, B = load()`）、JS / TS 顶层的解构（`const { a, b: c } = obj`）原来不是定义，只是局部绑定。现在模式两层以内绑定的每个名字都是定义（「定义形式」已改），查询和遍历都按两层写，更深的嵌套是已知限制。
- Go 的 `const A, B = 1, 2`：原因是 `const_spec` 的 `name` 字段也包含逗号，不是查询引擎的问题；查询改为 `(const_spec (identifier))` 之后两条路径都把 `B` 记为定义，`extract_walker_matches_tags` 不再允许差异（「外部格式」已改）。函数里的 `const a, b` 也因此两个名字都是局部绑定。

第 1 步，处理 [#113](https://github.com/SoloJiang/lectern-app/pull/113) 第五轮审阅意见：

- 看门狗的槽位登记时是清零的，第一次检查在看门狗线程上，小文件可能在那之前就抽取完，已经过期的截止时间和已经请求的暂停不起作用。现在登记时在调用方的线程上同步检查一次，已经触发的不进入在途列表；`ParsedFile` 在解析之前也检查一次（「调度、批次与取消」已补）。
- Go 的 `:=` 原来把左边的名字都记为新的声明，`value, err := read()` 里已有的 `err` 成了新变量，之后的引用绑定到这一行。现在按 Go 规范只有作用域里新的名字是声明（`local.redeclare`，「绑定规则」已补）。`for i = range xs` 和 `case v = <-ch` 原来也是声明，现在只有 `:=` 形式是；这两种模式要在名字之后的 `:=` 处才完成，tree-sitter-tags 按完成顺序处理匹配，所以改用它不过滤的 `@definition.lectern.local`，引用名因此多出这些变量。
- Rust `impl Trait for &T`、`&mut T`、`[T]`、元组、基本类型等的方法原来没有 `impl` 标记，可见性是 `Module`。现在实现的类型穿过引用、指针、数组和元组取名字，trait 实现另由 `for` 标记，类型取不到名字时方法仍是 `Public`（「抽取结果」已补）；`EXTRACTOR_VERSION` 从 3 改为 4。
- TS namespace 和 module 体里导出的 `const` / `let` / `var` 原来不是定义。现在是（「定义形式」已改），只认单个名字，解构和 `declare namespace` 里没有 `export` 的隐式导出是已知限制。
- namespace 里的 `export` 原来让文件成为 ES 模块，顶层没有导出的定义因此成了 `File`。现在只看程序顶层的 `import` / `export`（「抽取结果」已改）。

第 1 步，处理 [#113](https://github.com/SoloJiang/lectern-app/pull/113) 的独立预审意见：

- 局部解析原来是黑名单：位置分析对任何名字节点按文本解析，`NON_VARIABLE_POSITIONS` 列出例外，对象的键（`{ name: name.trim() }`）、JSX 属性名、Rust 结构体表达式和结构体模式的字段名、JS 解构的键、Go 复合字面量的键、JSX 的内置元素名（`<label>`）都绑定到了同名的局部。现在只解析 locals 查询的 `@local.reference` 捕获，再按「定义形式」表行上的位置排除键、属性、路径段和标签名，Go 的键只作候选（「外部格式」已改）；locals 查询因此另外捕获类型名和 `{ a }` 简写。位置分析认的节点种类与模式展开的绑定种类合成一张表，Rust 结构体模式简写里的 `x`（`let Point { x, y } = p`）原来不在其中，点不到。
- `Collector::finish` 按「定义数 × 作用域数」找最内层作用域，`rules::finish` 按「定义数 × 导出语句数」判断导出，1000 KiB 的 JS 在 `ParsedFile` 里要 8.9 s，1 MiB 的 `export const` 抽取 905 ms；规则还在看门狗登记撤销之后运行。现在都是排序后的扫描、二分查找和查表，看门狗登记覆盖到规则结束（「抽取结果」已补）；release 下 `ParsedFile` 处理 1 MiB 的 JS 约 0.6 s，256 KiB 的约 0.2 s。
- 语法种类表只从声明推出，Java 的 `new Runnable() { … }` 指向接口时被过滤掉。现在 `interface_declaration` 也接受 `Construct`，另有按用法写的样例逐一检查（「候选的分层与过滤」已补）。
- Python 的模块级常量原来只认模块的直接子语句，`if` / `try` / `with` / `for` 里的赋值和连等里第一个以外的名字都不是定义；方法里的 `self.name = …` 也不是字段。现在模块级按作用域判断，`self` 的属性赋值是类的字段（「抽取结果」「定义形式」已改）。
- TS 的 `export = X` 没有被认作导出（grammar 里 `X` 不在 `value` 字段里）；`declare global` 里的声明是 `File`。现在都按导出处理，导出形式有一张逐行检查的表（「抽取结果」已补）。
- 规则的变化让 `EXTRACTOR_VERSION` 从 4 改为 5。

第 1 步，处理 [#113](https://github.com/SoloJiang/lectern-app/pull/113) 的第二次预审意见：

- 标记的模式排在引用模式之前，占用名字节点后这个名字就不再是引用：Python 的每次属性赋值（`self.n = …` 的第二次写、`o.n = 2`）不在 `ParsedFile` 的引用位置里，Go 只有方法的文件里接收者类型、`export { UserService } from …` 里的名字、Rust `impl` 的类型都不在引用名里。现在定为不变式：占用名字的标记在两条路径上都把名字记回引用名、在 `ParsedFile` 里记回引用位置，除非它成了这个位置上的定义（「抽取结果」已补）。
- TS 以字符串命名的环境模块（`declare module 'express-serve-static-core' { interface Request { … } }`）里的声明原来是 `File`，`req.user` 失去唯一的候选。现在与 `declare global` 一样按导出处理（「抽取结果」已补）。
- Python 的解包属性赋值（`self.a, self.b = 1, 2`）也是字段；属性赋值标记改在属性节点上。
- Rust `impl Trait for [u8]` 和 `for (u8, u8)` 的方法原来的 `scope` 是 `u8`，现在没有。
- 引用名的变化让 `EXTRACTOR_VERSION` 从 5 改为 6。

第 2 步：

- 设计没有给存储的接口。实现为公开的 `store` 模块，供第 3、4 步和 bench 使用：
  - `TABLES`（`code_intel.tags` 的登记，`NAMESPACE` 第 2 步是版本 1，第 4 步因 `ci_def_name` 改为 (name NOCASE, unit, kind) 升为 2，见「存储表」的「版本」；`QUOTA` 256 MiB）和 `Version`（语言、grammar 版本、查询版本，`Version::of(&Lang)` 取当前语言表里的三元组）。
  - `unit_key`、`unit_bytes`：「存储表」的键和字节数估算。
  - `Store`（`Store::new(&Partition)`，`Clone`、不持有连接）：
    - `evictions()`：淘汰代数，只读一个原子变量。
    - `exists(&Version, &[BlobId]) -> Option<Vec<Found>>`：存在检查，`Found` 带 OID、单元和状态。
    - `defs_by_name(name, &[Version]) -> Option<Vec<StoredDef>>`、`refs_by_name(name, &[Version]) -> Option<Vec<StoredUnit>>`：按名字查定义、查引用的单元，区分大小写。第 4 步（ADR 0037）加了分页：`NamePager::new(name, &[Version])`，`next_defs(&mut NamePager, limit)`、`next_refs(&mut NamePager, limit)` 各读一页（一条语句），`defs_left(&NamePager) -> u64` 数出没读的行；`defs_by_name_in(name, &[Version], &[BlobId], limit)` 在给定的 OID 里查。前两个改为逐页读完。
    - `units(&[Version])`、`def_names(&[Unit], f)`：名字表的两步。第 4 步改为 `def_names(&[Unit], stop, f(name, kinds))`：按索引分页读，每个不同的名字调用一次。
    - `load(Unit) -> Option<Extraction>`：读回一个单元，供解析扩展点「按路径取已抽取的结果」使用。
    - `touch(&[Unit])`、`write() -> StoreWrite`。
    - `touch_pending(&mut PendingTouches) -> usize`：标记排队的命中，每次最多 `TOUCH_CHUNK`（1024）个，返回还剩多少。`PendingTouches::push(generation, &[Found])` 把存在检查的命中连同查询之前读到的淘汰代数排进队列。
  - `StoreWrite`：`add(oid, &Version, &Extraction) -> bool`、`len`、`bytes`、`commit() -> Written`。

  读取返回 None 表示分库读不出，调用方按全部未命中处理。
- 存在检查查出 `unit, oid, status`，设计写的是 `SELECT oid`：标记访问要用单元 ID；状态让调用方区分 `TimedOut`、`TooLarge` 的单元（文件被打开时按交互截止时间重做）。按名字查定义、查引用也多查出单元 ID，调用方交出结果之后用它标记访问（[cache 设计「访问时间」](cache.md#访问时间)）。存在检查和按名字的查找都不标记，由调用方决定时机（存在检查见下面「把访问标记移出热启动」一条）。
- 版本过滤的 `OR …` 写成固定的 4 组三元组（`GROUP_TRIPLES`），用不到的组绑定 NULL，超过 4 种语言的兼容组分几次查询再合并：登记的语句是静态文本，参数个数固定（cache 设计「登记的规则」）。首发语言最大的组（`ts`、`tsx`、`js`）一次查询。存在检查和名字表的第一步只查一种语言，各用一组三元组。第 4 步：分页的查找和名字表的第一步不再在 SQL 里按三元组过滤，在 Rust 里比对（「存储表」），分组只留给存在检查和在给定 OID 里的查找。
- `ci_ref` 的插入用 `INSERT OR IGNORE`：抽取结果的引用名本来已经去重，重复的一行不至于让整批回滚。
- grammar 版本和查询版本是 `u64`，按位存成 SQLite 的 64 位有符号整数，读写用同一种转换。
- 语句共 11 条（三条插入、存在检查、两条按名字的查找、名字表两条、读回单元三条），在 cache 每连接 32 条的预编译缓存以内。
- 名字表的第二步按单元逐个查 `ci_def_unit`（`def_names` 的单元按 ID 排序后传入）：设计写的是「按 `ci_def_unit` 顺序扫描」，没有规定是一条语句还是逐个单元，逐个单元不需要在 SQL 里表达映射过滤后的单元集合。第 4 步改为一次扫描整张表，见下。
- `store_dropped_write_does_not_panic` 不检查恢复后写入的单元仍在：`SQLITE_FULL` 之后 cache 把分库预算降到文件大小以下，刚写入的单元会被立即淘汰（cache 设计「淘汰」），这是 cache 的行为。
- 早期数据：第 2 步用抽取夹具拼成的语料对照了单元字节数的估算与实际文件，实际文件约为估算的 1.12 倍（见「实际验证」）。这份语料的定义密度远高于真实代码，不用来校准 10 万文件的总量；校准由第 3 步的 `code_intel/disk_100k` 完成（「验收」第 3 步）。
- 把访问标记移出热启动。设计原来让存在检查给命中的单元标记访问（「调度、批次与取消」的存在检查一条、「存储表」的淘汰一条）。第 2 步第一版这样实现之后，`code_intel/warm_100k` 在 macOS CI 上中位数 537、549 ms，超过 500 ms 的预算。Linux 上的拆分：200 次查询约 82 ms，其余约 370 ms 是 `TableSet::touch` 写回 10 万个单元的访问时间。改动：
  - 存在检查只读，返回命中（含单元 ID），不写入。
  - 命中排进 `PendingTouches`，热启动之后由批量线程在批次边界每次标记 1024 个。
  - 淘汰代数变了时队列整个丢弃；项目关闭时队列随之丢弃。

  理由：`warm_100k` 守的是热启动，即已入索引的仓库被认出已入索引（`indexed` 提示置位）、导航能用上索引的时间；淘汰顺序的记账不需要在这条路径上。丢掉一次热启动的访问标记只影响淘汰的先后，不影响结果的正确性。预算不变；推迟的标记另由只记录的 `code_intel/warm_100k_touch` 测量，开销仍然可见。cache 的访问时间改为按集合写回（[#116](https://github.com/SoloJiang/lectern-app/pull/116)）与这一条无关，本分支不依赖它。

第 3 步：

- 设计把映射、核对任务和各层写成插件内部的状态，没有给第 3 步（还没有插件）的入口。实现为公开的 `index` 模块：`Indexer`（`Indexer::new(IndexDeps)`，测试和 bench 用 `with_fake_git`）持有每个打开项目的状态，第 4 步的插件在 `activate` / `started` 里构造它，把 `ProjectEvent`、`set_focus`、电源和语言包的通知转给它：`project_event`、`open_existing`、`set_focus`、`power_changed`、`grammar_changed`、`demand`（电池供电时的 `query` / `names`）、`coverage`、`map`、`head_diff`、`head_clean`、`subscribe`（电源和语言包的订阅，返回值交给 `PluginHandle::own`）、`close_all`。`IndexDeps` 是「本 crate 的公开项」里 `CodeIntelDeps` 的字段加上 `projects`（`Arc<dyn Projects>`）、这次激活的 `PluginHandle`、看门狗和批量任务数（`default_batch_tasks()`，与 executor 的批量线程数相同）：executor 不公开批量线程数，插件也只能在 `started` 之后拿到句柄。
- 映射的类型在公开的 `map` 模块（`BaseMap`、`Overlay`、`WorktreeState`、`IndexedHints`、`LiveMap`、`HeadDiff`），供第 4 步的查询和 bench 使用。`by_oid` 拆成前缀和下标两个数组（每条 12 字节，`(u64, u32)` 的元组对齐后是 16 字节），另存每条的语言（1 字节），10 万文件合计 2.8 MB，在 3 MB 以内（「blob → 路径映射」已改）。提示的戳记取淘汰代数的低 32 位和语言包代数的低 31 位，与 OID 前缀共 16 字节。
- `GitAccess` 的参数从 `root: &Path` 改为 `&Tree`（公共目录、git 目录、根目录）：生产实现要按公共目录打开仓库、按 git 目录取 worktree，只有根目录找不到它们。`tracked` 返回 `Vec<(RelPath, BlobId)>`（只有普通文件和可执行文件、UTF-8 路径），不返回 `Arc<git::TrackedPaths>`：第 5 层只用这些条目，`FakeGit` 也构造不出 `TrackedPaths`。`canonical_oid` 的路径改为 `&str`（「依赖 git、fs_watch 与 cache 的接口」已改）。
- 覆盖部分对文件列表里有、原始字节的 OID 不等的路径调用一次 `canonical_oid`：设计只按原始字节计算，`Filtered`（LFS 的指针文件）因此认不出来，`overlay_*` 要求它是 `Excluded`（原为 `Unindexable`，见下面「完整性的核算」第二条）；顺带，CRLF 检出的跟踪文件不再被当作有改动。未跟踪的文件仍按原始字节（「blob → 路径映射」已改）。
- 游标由批量任务在每批开始时按依赖的代数重建，不在核对任务里：游标只有批量任务用，依赖表不变。
- 第 3 层的范围：设计在没有当前文本和审阅集合时没有给出取法，实现依次取当前文本、审阅集合的第一个文件、项目打开时的路径（根目录除外）；有子项目、但没有这样的路径或没有子项目包含它时第 3 层为空。理由：取整个 worktree 时，电池供电下的第 3 层就是整个仓库，与 ADR 0029 的电池策略相反（「建索引的分层」已改）。只有最近一次收到 `set_focus` 的项目跑第 3–5 层；还没有任何 `set_focus` 时取最后打开的项目。
- 存在检查自成一批，游标每步最多看 4096 个条目、取出至多 500 个文件，缺的进入这一层的小队列（「调度、批次与取消」已补）：设计没有规定存在检查的批与抽取的批怎样划分；按 64 个文件做存在检查时 10 万文件要 1600 次查询，超过「约 200 次」。看 4096 个条目的上限让 UI 线程上的 `set_focus` 不会等一次长的扫描。
- 配额满时游标不因淘汰重新开始，提示也按冻结的淘汰代数比较和记下（「调度、批次与取消」已补）：设计没有考虑本 crate 自己的写入引起的淘汰，按原规则第 3–5 层会不停地重新抽取刚被挤出的单元。只冻结游标的依赖不够：游标因别的原因重建时（焦点、编辑），提示若按当前淘汰代数比较就几乎全部失效，每次切换文件都会重新抽取被挤出的单元（24 KiB 配额、80 个文件时，5 次切换各多抽取 24–57 个）。
- 映射记下它的来历（文件列表、工作区内容、语言包三个代数），游标的依赖和提示的戳记取自来历，不取当前代数；命中只在版本与当前语言表、映射的语言表都相同时置位提示（「输入代数与派生状态」已改）。原来按当前代数戳记时，语言包更新之后、映射重建之前运行的批次或第 1 层会把旧版本的命中戳成新代数，这些文件在本次会话里不再按新版本抽取。`coverage` 另外要求映射按当前的文件列表和语言包建成、游标的范围是当前的范围。
- 焦点不是游标的依赖，只经第 3 层的范围起作用（「输入代数与派生状态」已改）：每次 `set_focus` 都重建游标时，游标整个重新走一遍，这只有在全部提示都匹配时才无害。
- 第 1 层只认 `Done`，`TimedOut`、`TooLarge` 打开时重做一次，第 1 层接管批量层在途的键；批量层写入前再查一次存在、不替换，只有第 1 层替换（「建索引的分层」「调度、批次与取消」已补）。设计写了「打开时按交互截止时间重做」，原实现把缓存里的任何状态都当作已入索引；cache 把替换计为淘汰，所以要区分替换和配额压力，写事务放进项目的写锁（新增的锁不与本 crate 的其他锁同时持有）。
- 工作区比较的请求只来自事件，核对任务的一轮只在有没消费的输入时再走（「增量更新」已补）：原实现把比较列出的、读出来是目录的路径也当成「目录级变化」，未跟踪的嵌套仓库（git 把它列为一个目录路径）和被目录替换的跟踪文件会让核对任务无限地比较下去。
- 第 5 层：某个 worktree 的 index 变化只让它重新处理，本次会话已确认在缓存里的键不再检查和标记访问（「建索引的分层」已补）。原实现让任何一个 worktree 的 index 变化重新处理全部 worktree，并把全部命中重新排进访问标记；agent 在其他 worktree 里频繁 `git add` 时每次的代价是 N 个 index 的读取、每 10 万条约 200 次存在检查，以及把全部命中重新标记访问。
- 超过 256 KiB 的文件同时只在一个批量任务里抽取，等待的任务每 10 ms 检查一次暂停和取消（「性能」已改）：实测接近 1 MiB 的文件的语法树和遍历约 31 MiB，设计估算 ≤ 10 MiB；两三个批量线程同时抽取这样的文件会超过 40 MB 的份额。
- 读出时超过 1 MiB 的文件（文件列表里的条目，没有覆盖项）按排队时的 OID 记一行 `TooLarge`：没有读出字节，无法按实际字节算 OID；这一行只有状态，打开时按交互上限重做。
- 抽取返回 `Unsupported` 的语言在本次运行中不再进入任何一层（「状态与异常」的「这种语言在本次运行中停用」）。
- 语言包代数是本 crate 自己的计数（语言表每变化一次加一），不是 `GrammarSource::generation`：后者在没有改变语言表的通知（重复的安装）上也会变化。一轮开始读语言包的当前状态时清掉「再来一轮」的标记，所以之前到达的通知并进这一轮。这个代数只有一份，各项目比较时直接读它，不存副本（「输入代数与派生状态」已补）：第二轮预审发现，语言包的一轮先复制打开项目表、再让代数加一，其间打开的项目带着旧代数、又不在通知里；按项目的副本比较时，它的基础映射每遍都重建、每次都作废，映射一直为空、`coverage` 一直是 `Pending`，每个事件 16 次重建（10 万文件每次约 10 ms）。现在语言表和代数在持有打开项目表的锁时更新，核对任务在一遍没有任何进展时停下。
- 第 1 层的替换不算淘汰：替换引起的淘汰代数加一在写锁里记为本 crate 自己的，提示和游标用的淘汰代数减去它（「调度、批次与取消」已补）。不这样做，每打开一个记为 `TimedOut` 的大文件，全部提示失效、游标重建、第 5 层重新开始一次。
- 第 2 层也接管批量层在途的键（「建索引的分层」已改）：原来第 2 层遇到在途的键就跳过，批量任务暂停时那个文件要等到恢复，与 ADR 0029「第 1、2 层不排在批量任务后面」不符。在途表因此记下每个键的持有层。
- 第 1 层只在命中或写入成功之后把（OID、版本）记为本次会话已满足（「建索引的分层」已改）：原来在抽取之前就记下，读不出来的文件（暂时被删除、正在被替换）本次会话再打开也不会抽取。
- 第 5 层已确认的集合存 8 字节的键（「建索引的分层」「性能」已补）：原来每条约 56 字节，10 万个约 6 MB，常驻内存的份额没有计入它。
- 批量任务的唤醒补满任务数，并有一条可检查的活性规则（「调度、批次与取消」已补）：#117 的审阅发现一次唤醒只启动一个任务（两三个批量线程时只有一个在跑），关闭跑第 3–5 层的项目后接手的项目没有被唤醒，它的第 3–5 层一直不开始。
- 会话结束后（项目关闭）第 1、2 层的任务在取下一个文件之前停下（「调度、批次与取消」已补）：原来只比较会话号，关闭后同一会话号的任务继续读、哈希、抽取队列里剩下的文件，并一直持有 `BusyGuard`。
- `head_clean` 在事件已到、核对任务还没处理时也是 false（「依赖 git、fs_watch 与 cache 的接口」「输入代数与派生状态」已改）：原来它一直按上一次的 `HeadDiff` 回答，语义查询可能把内容已变的文件交给只认 HEAD 的提供者。
- `GrammarSource` 增加 `installed`（「本 crate 的公开项」已改）：激活时语言包的代数已不为零（启动前装好的包）时，第一轮没有通知可依据，原来的一轮只看语言表里已有的语言，这些包永远不加载。现在每一轮都读全部已装的包；`NoGrammarPacks` 返回空表，`testing::FakeGrammars` 返回它装着的包。
- `ProjectEvent::Changed` 的 `files` 同时让下一次核对读 index 校验和；`RepoBatch` 里其他 worktree 的 index 变化让第 5 层重新处理那个 worktree，`rescan` 让它重新处理全部 worktree。`RepoBatch` 的 `refs` 和当前 worktree 的 `head` / `index` 与 `WorktreeBatch` 的 `meta` 同样处理（「增量更新」只列了 `WorktreeBatch`）。fs_watch 的监听失败（`Health::Failed`）时，订阅在 executor 上 drop（不在 fs_watch 线程的回调里），下一次 `ProjectEvent` 重新打开根目录并订阅。
- 文件夹项目在第 3 步不建索引：它的 OID 要读每个文件才能算出，与第 4 步按需读取、核对 OID 的代码（`read_verify_*`）是同一段；第 3 步的映射、覆盖部分和各层只处理 git 项目。文件夹项目的 `coverage` 各层都是 `Pending`（「未入索引」），不会是 `Done`：`TierState` 没有「不适用」，`Pending` 已经表示「还没有入索引」，所以不扩展 `services` 的类型。调用方据此不信任索引，按「跳转到定义」的顺序现场回答、走限时搜索；映射为空，所以没有路径的 `indexed` 提示匹配，`head_clean` 为 false。第 5 步的探针等 `Done` 时，文件夹项目会一直等不到，要等文件夹项目建索引实现之后才能用它测量（`schedule_folder_project_reports_unindexed`）。
- 测试：`derived_*` 里依赖查询的部分（限时搜索不跳过有效 OID 已变的文件、淘汰之后经限时搜索找到定义、候选的子项目层、名字表的重建）随第 4 步的查询和名字表加入；第 3 步检查它们依赖的输入代数和派生状态（提示、游标、第 3 层的范围、写入代数）。分库被删除重建时 cache 让淘汰代数加一，与单元被删除走同一条路径，由 `derived_eviction_*` 覆盖。`battery_*` 里「一次 `query`」用 `Indexer::demand`（`query` 属于第 4 步）。
- `FakeGit` 与 lectern-git 一样把含 `.git` 的目录（嵌套仓库）列为一个未跟踪的目录路径，被目录替换的跟踪文件列为这个文件和目录里的文件；`overlay_lectern_git_and_the_fake_list_directories_alike` 在 `TestRepo` 和 `FakeGit` 上构造同样的情况、比较两者的结果。原来的 `FakeGit` 会走进嵌套仓库，掩盖了上面的核对循环。
- `testing` 新增 `FakeGit`、`FakeProjects`、`FakePower`、`host()`（只交出 `PluginHandle` 的插件）、`Rig`（bench 用的完整建索引环境）、`synthetic`（合成仓库：语言比例、路径、8 KiB 的源码）和 `route_tree_sitter_allocations`（让 tree-sitter 的 C 分配经过 Rust 的全局分配器，`code_intel/batch_peak` 才能计入语法树）。`Indexer::set_extractor` 让测试计数和控制抽取。依赖新增 `lectern-kernel`、`lectern-git`、`lectern-fs-watch`、`lectern-platform`，`lectern-executor` 改为普通依赖；都是已有的 crate，Cargo.lock 没有新增 crate。

第 4 步：

- 查询、名字表和语义导航的登记做在 `Indexer` 上（`query`、`results`、`close`、`prefetch`、`names`、`register_provider`，并 `impl CodeIntel for Indexer`），模块是 `query`（跳转、按名字、查引用、限时搜索、读取核对、分层）、`names`（`build_table` 公开，供 bench）、`semantic`（内部）和 `resolve`（解析扩展点，内部）。设计把它们写成插件内部的状态；理由与第 3 步的 `Indexer` 相同：测试和 bench 不经 kernel 就能驱动。
- 插件：`activate` 里提供的服务对象是 `CodeIntelService`，`Indexer` 在 `started` 里才建（`PluginHandle` 只在 `started` 拿到，第 3 步的 `IndexDeps` 要它）。kernel 在 `started` 之前不准入任何调用，服务在那之前按「没有打开的项目」回答（`query` 返回 `QueryId(0)`，结果是 `Failed(NotOpen)`）。`dyn Projects` 经 `ServiceRef` 包成 `ProjectsRef`，每次调用取提供者的准入。`CodeIntelPlugin` 另有 `test-support` 的 `with_clock`、`with_batch_tasks` 和 `services()`（最近一次激活的服务，只存 `Weak`，不延长停用的激活）。`app` 还没有装配本插件（`app` 依赖 GPUI，Linux 上不编译），留给第 5 步。
- 看门狗的到期表：`Watchdog::schedule(at, f)`（`Due`）。用 `ManualClock` 时，线程在有到期项时每次最多等 `CHECK_INTERVAL`（10 ms），所以测试的手动时钟推进后能被看到；用系统时钟（`Clock::moves_by_itself` 为 true）且没有在途抽取时，直接等到最早的到期时刻，不再每 10 ms 醒一次（原来保留的解析排着 60 s 的到期项，这 60 s 里看门狗每秒醒约 100 次；`deadline_watchdog_sleeps_until_a_far_due_entry`）。没有在途抽取也没有到期项时仍不带超时地等待。`stop` 丢弃到期表。
- 第 1 步的候选 `source` 是 `Scanned`（当前文本为这次查询现场解析，不是查索引得到的），设计没有规定。解析超时时返回 `Failed(Unreadable("parsing timed out"))`：`QueryError` 没有超时一项，交互截止时间是 1 s。
- 第 2 步读过的 import 目标，限时搜索不跳过、用第 2 步的抽取结果，按已交出的位置去重；只跳过当前文本（第 1 步已列出它的同名定义）。设计写「现场抽取过的 import 目标不在第 3 步和限时搜索里重复出现」，按文件跳过会丢掉这个文件里第 2 步没有列出的同名定义，所以按位置去重。
- 限时搜索的范围：有子项目、但没有子项目包含当前文本时搜整个 worktree（设计没有规定这种情况；第 3 层建索引在这时为空，见第 3 步的「第 3 层的范围」，搜索有 1 s 的上限，不受它影响）。`Partial::outside` 由「完整性的核算」计算，搜完也计算（见下面「完整性的核算」一条）。
- 解析扩展点：`Resolver::package_of` 返回 None 时用默认的「同一目录」；`Import::exported` 给出本地名对应的导出名。#17 只有空规则 `DefaultRules`，测试经 `Indexer::set_resolver` 装入自己的实现。
- 查引用：import 语句里的名字（`import { helper } from './a'`）也是引用位置，没有依据时列在 `same_name`，`bind` 给出依据时是调用方；这是 `ParsedFile` 的引用位置本来就包含的。限时搜索命中的文件只解析、不写入缓存：`ParsedFile` 不是抽取结果（它的引用位置包含局部引用），为写入再抽取一遍要多一倍时间；第 3 层的批量任务之后照常为它们建索引（设计「限时搜索」写的写入只对跳转做）。第一批在处理完 4 个文件或 150 ms 时交出，之后每 32 个文件或 100 ms。
- `Named`：`symbol` 为 None（`Symbol` 要求语言，按名字查询没有）；不按可见性和语法种类过滤（没有引用位置）；不做限时搜索，所以 `near` 所在子项目（没有 `near` 时整个 worktree）里还有未入索引的文件、或在途抽取时结果是 `Done(Partial)`，`outside` 是其余未入索引的文件，都入索引后才是 `Complete`。设计只说「只做第 3 步」，没有说完整性。
- 预取的沿用条件另含项目的会话号（切换 worktree 之后不沿用）；最多保留 4 个预取。用过一次即丢弃。
- 名字表的代数是本 crate 按项目记的计数：每次看到输入（会话、映射来历、淘汰代数、语言包代数、写入代数）与上次不同就加一。设计写「名字表依赖的输入……任何一个变化时代数加一」；直接用输入代数之和不行，切换 worktree 时项目的输入代数从零重新开始，表的代数会变小。名字表的状态放在 `Project` 上（不随会话重置），持有中的表在切换 worktree 后也会重建。重建的触发点是批次写入之后、核对任务结束时和 `names` 本身；淘汰代数的变化在这些点上读到。
- 语义导航：合并时「与 HEAD 相同的文件归 `head_only` 一类」只在这次查询还有等回复的、或带着位置回复的 `head_only` 请求时成立，否则全部文件归另一类：设计没有说 SCIP 注销、出错或超时之后它的文件归谁（原来出错的回复也算「已回复」，SCIP 出错、jdtls 成功时 jdtls 在与 HEAD 相同的文件里的位置被丢掉，「先后」已补）。`Same` / `Ready` 按（路径、行、列）的集合比较。注销后的重新合并、回复的处理都在 `Background` 任务里（回复只入队），事件在 executor 线程上发出。切换当前 worktree 时关闭这个项目的全部查询（未回复的请求收到 `cancel`）：它们回答的是旧 worktree 的内容。
- 覆盖：`coverage` 另外要求没有待读的路径、git 项目的工作区比较是当前的，否则第 3、4 层是 `Pending`：原来在核对任务装上基础映射、还没读完覆盖部分的间隙里，第 3、4 层可能报 `Done`。
- 文件夹项目在第 4 步建索引（第 3 步把它推迟到这一步，见上）：文件列表就绪时打开根目录的 `SafeRoot`，把文件列表里的每个文件放进重新读集合，核对任务读出、按原始字节的 SHA-1 记为覆盖项（`Content`），之后与 git 项目走同一套映射、各层和查询；换文件列表时全部重新读。没有 fs_watch 订阅、工作区比较和 HEAD（`head_clean` 总是 false），`ProjectEvent::Changed` 不再让文件夹项目重新打开根目录。热启动要读每个文件求 OID，命中的不解析（`schedule_folder_project_indexes_at_open`）。
- 名字表的第二步改为一条语句扫描整张 `ci_def`（`SELECT unit, name, kind FROM ci_def`，表的顺序），跳过单元集合以外的行；同一单元的行连续写入，对上一行单元的判断沿用到下一行。逐个单元查 `ci_def_unit` 时 10 万个单元要 10 万次语句执行和 300 万次按 rowid 回表，`code_intel/names_100k` 为 1.07 s，超出 1 s；一次扫描为 0.81 s。
- `ParsedFile`（跳转第 1 步、查引用和限时搜索现场解析的文件）在核心语言上不再跑 `Compiled` 的完整查询：定义和标记本来就取自遍历（ADR 0031），只需要 locals 的模式和给出引用的 tags 模式。`Lang::site_query` 用与 `Compiled` 相同的文本另编译一个查询，停用其余 tags 模式，模式和捕获的编号、属性、谓词与完整查询相同；它不经 `TagsConfiguration`，所以核心语言在生产中只编译这一个查询。`site_reference_query_matches_full` 在夹具、遍历的样例和每种扩展名的合成源码上检查两者的定义、引用位置和每个位置的 `site_at` 相同。另外，局部引用是否处在变量位置要看父节点，tree-sitter 的 `Node::parent` 每次从根往下逐层扫描子节点，顶层条目多的文件每次都扫描全部顶层条目；`site::Parents` 用二分查找定位根的子节点，往下与 tree-sitter 相同（`site_parents_match_tree_sitter` 检查每个节点的父节点与 `Node::parent` 相同）。64 KiB 的 TS 文件上 `ParsedFile` 从约 27 ms 降到约 20 ms（解析本身约 9 ms）。
- `ParsedFile` 要的查询（核心语言的 `SiteQuery`，语言包的 `Compiled`）不在查询线程上第一次编译：核心语言的抽取走遍历，从不编译它，第一次跳转原来要在 `Interactive` 线程上先编译（release，Linux：ts 138 ms、tsx 109 ms、rust 82 ms、js 42 ms、python 33 ms），超出候选 50 ms 的预算。每次核对任务结束时，这个项目的映射里有还没编译的语言，就请求一个 `Background` 任务编译（`warm_site_queries`）。任务同时最多一个，请求只保证它在运行；任务自己扫描所有打开项目的映射，编译到扫描为空，清除标志后再扫描一次，所以任务进行中别的项目发来的请求不会丢（原来请求方带上自己项目的语言，任务结束时只重看这个项目：几个项目同时打开时，后来者的语言要等到下一次与它无关的核对，`goto_site_queries_of_every_open_project`）。插件本身在首帧之后激活，所以这也在首帧之后。查询在编译进行中用到它时等这次编译（`OnceLock`），不会比自己编译更久。`Lang::prepare` 也编译它，测试和基准计时之前调用。设计没有写这些查询何时编译。
- 查询的视图（「输入代数与派生状态」的「查询的视图」，取代原来按输入逐个补的检查：提示的切面、预取的 `Inputs`）：一次查询读几个各自前进的输入（会话与当前 worktree、映射、淘汰、提示、文件内容、项目的打开与关闭），原来是逐个输入临时检查，Codex 先后指出三处同一个根因：读提示与查索引之间提交的批次（切面）、快照与查索引之间的淘汰（冻结的提示戳记仍然匹配，查不到的单元被当成已入索引，结果报 `Complete`）、切换 worktree 后旧会话、旧映射和运行中的查询要等核对任务才结束（新 worktree 的文本按旧映射回答，运行中的查询在切换之后还发布旧结果）。另外发现第四处：新会话（项目刚打开、刚切换）的映射建好之前，查询按空映射回答并报 `Complete`。现在：一个 `View`、一个比较（`Inner::check_view`，一次项目锁、一次原子读，O(1)），比较结果决定追加、重做一次、`Stale` 或停止；发布经 `publish_in` 在 `publishing` 锁下核对会话；切换 worktree 和关闭项目在事件里同步结束会话；没有映射的会话里，查询排队等它。测试：`view_*` 的交错矩阵（见「实际验证」），`cut_*` 改用同一组钩子（`Inner::step_hook`，代替 `after_lookup`）。名字表只按单元建表、不判断完整性，不受影响；它的重建由 `gens.writes` 触发。
- 发布的快照（「查询的视图」第一条）：原来查询的快照由几处拼成，当前状态的映射、全局语言表、根和子项目各取当前值。Codex 又指出两处同一个根因：文件夹项目打开时，等着的查询在基础映射装好、覆盖部分还没读的时候就开始，`Named` 和跨文件的搜索看到零个文件、报 `Complete`；语言包升级时全局语言表先换、映射后重建，这段时间的查询按新版本查存储，提示却按旧戳记匹配，跳过的文件没有结果，仍报 `Complete`。现在快照只来自核对任务消费完全部输入时整体发布的 `Published`（映射连同建它的语言表、停用的语言、根、HEAD 树、子项目），等着的查询在发布时开始，不在 `rebuild_base` 之后；当前文本的语言也取映射里的。交错矩阵加了「语言包升级（换表与重建映射之间）」这种变化和「变化之后立刻查询（核对任务还没运行：切换之后没有可用快照，其余仍用上一个）」这个位置，文件夹项目另有一个测试在基础映射与覆盖部分之间取快照（见「实际验证」）。
- 批量任务的唤醒（`kick_batch`）先置 `kicked` 再补满任务池：原来先补池再置位，池满时最后一个任务恰好在两步之间空闲退出，这次唤醒要等下一次。其余单飞的入口（核对的 `again`、语言包一轮的 `again`、名字表建完后的重查、语义回复的队列、看门狗按表运行）都在同一把锁里记下请求，不会丢。
- 跳转第 1 步解析当前文本用 `ParsedFile::parse_at`，不用 `parse`：跳转只看光标处一个位置、本文件的定义和绑定模型，用不到文件里其余引用的位置（`references`，只有查引用用）。核心语言上，整棵树只跑建绑定模型的 locals 模式（作用域、定义、`global` / `nonlocal`，`Lang::jump_query`）；光标处名字是不是 locals 的引用、在不在变量位置，由完整的查询在光标前后两个字节上再跑一次给出；引用位置不再收集。64 KiB 的 TS 文本上，查询从 9.3 ms 降到 4.9 ms，匹配从 11 478 个降到 2 506 个，引用位置的整理从 1.5 ms 降到 0.2 ms，分配从约 10 500 次降到约 5 700 次。`site_parse_at_matches_parse` 检查 `site_at` 与本文件的定义（连同 `local` 标记）都与 `parse` 相同：每种核心语言一段跨作用域绑定的样例（模块级的名字在函数、类、方法里使用，内层作用域遮蔽它，JSX / TSX 组件，Python 的 `global` / `nonlocal`，Go 的包级名字，Rust 的 crate 级条目，Java 的字段和 import）、夹具和遍历的样例上每个标识符的起止位置，8 KiB 的合成源码上每个来源约 150 个位置，合计约 6 000 个；`parse_at` 的结果在别的位置调用 `site_at` 或取 `references` 时 debug 断言失败。`jump_query` 与 `site_query` 一起由后台任务编译（`prepare_site`）。语言包仍走完整的 `parse`。
- 查询读文件只经 `Inner::read_content`（「跳转到定义」第 1 步核对当前文本的规则用到每一次读）：读出的字节只有是快照的映射给这个路径的内容（`Effective::oid`，原始 OID 或经内置转换）时才算数，否则 `Stale`，计入 `unsearched`，不抽取、不写入，也不以新字节的 OID 交出位置。用到它的是第 2 步的 import 目标、查引用的已入索引文件、限时搜索；预取走同样的步骤，名字表不读文件。没有 index 条目的路径（未跟踪的文件、文件夹项目）按映射里覆盖项记下的 OID，规则相同。原来这三处在字节不符时改用新字节的原始 OID（`key_of`），快照之后改动的文件会以新内容给出候选和引用、结果还报 `Complete`，消费方也发现不了（`read_verify_search_skips_a_file_changed_after_the_snapshot`、`read_verify_references_skip_an_indexed_file_changed_after_the_snapshot`）。查引用里这样的已入索引文件记在 `Found::unsearched`，结果为 `Partial`。超过 1 MiB 的文件与批量层一样不读（`TooLarge`）。
- 保留的查询状态都有界，用完即放（设计只写了个数上限）：预取最多 4 个，过了 `PREFETCH_FOR`（2 s）或输入变了就丢弃：每次预取和查询时清理，另在看门狗的到期表里排一项，到期时由一个 `Background` 任务清理（`expire_prefetches`），所以没有后续操作时也不会一直留着它的候选、抽取结果和语言包的租约（`prefetch_expires_unused`、`grammar_lease_released_by_an_expired_prefetch`）；原来只在第 5 个预取或同一查询取用时才丢。保留的查询最多 16 个（`RETAINED`），结果留到关闭或被新查询挤出，这是接口约定；其中语义导航部分（`SemQuery`）原来一直持有查询开始时的映射（含语言表的租约，10 万文件约 3 MB），现在只在还有请求未答复时持有，最后一个答复或提供者离开后放掉（`escalate_answered_query_releases_its_map`）。名字表只留 `Weak`；`Snap` 只活在查询任务里。
- 排序键每个候选只算一次（`sort_by_cached_key`）：候选的排序原来每次比较都重算路径距离并克隆路径（`Arc<str>`，克隆不分配，但每次比较两次原子操作和两次路径遍历），查引用的文件排序每次比较都扫描审阅集合、子项目和依赖。其余排序、二分和去重的比较都只借用。`Named` 查约 5 万个文件都定义的名字时（`code_intel/named_50k`，只记录），第一批要约 0.5 s，其中按名字查定义的 SQL 约 380 ms（每行约 7 µs）、组成候选约 70 ms、排序约 16 ms：先交出前 k 个也要读完所有行才能排序，降下来要改存储（覆盖索引）或给 `Named` 定预算，本轮没有做。
- 查询读文件的截止时间统一为 `query::search::FileDeadline`：后台截止时间（250 ms）与查询剩余时间中较小的一个（跳转第 2 步、限时搜索、查引用的解析；查引用原来在限时搜索里不看剩余时间）。被剩余时间截短后 `TimedOut`，以及 `Cancelled`，都不写入缓存、文件计入 `unsearched`（`FileDeadline::settles`，「限时搜索」已补这一条）：原来这样的 `TimedOut` 被写入，批量层不再重做，这个文件的定义要等打开它才出现，查询还报 `Complete`（`search_cut_extraction_is_not_stored`）。查询的其他写入只有访问标记，名字表和预取不写入。
- 看门狗线程的测试不用 `run_dedicated_gated` 的任务句柄：它交给 `own_with` 由这次激活持有，停用时丢弃。线程自己的闭包在开始和返回时记录到 `plugin::WatchdogThread`（`NotStarted` / `Running` / `Exited`），测试看它；`/proc/self/task` 只在 Linux 上、在线程已退出之后再确认（原来在 macOS 上没有 `/proc`，两个测试必然失败、两个空过；Linux 负载高时线程名在启动之后才设置，20 次激活的测试会失败）。「测试」已改。
- 基准：
  - `plugin/code-intel/resident_*` 在 `Memory` 目标里用计数分配器测本 crate 的活跃字节（tree-sitter 的分配经 `route_tree_sitter_allocations` 计入，不含 `project` 的文件列表和 SQLite 自己的内存）：`Process` 目标的进程级基准（bench 第 4、5 步）还没有实现，按 bench 设计的划分它们属于 `Process`，`budgets_belong_to_the_documented_targets` 为它们加了例外（[bench 设计「基准目标」](bench.md#基准目标)已改）。
  - `code_intel/references_100k` 拆成 `code_intel/references_100k/all`（≤ 2 s）和 `code_intel/references_100k/first`（≤ 200 ms）：「性能」这一行有两个预算。
  - bench 的 `budgets_ids_are_crate_slash_name` 允许 `plugin/<id>/…` 的插件 ID 一段里有 `-`：插件 ID 是 kebab-case，`plugin/code-intel/*` 是预算表里第一批带 `-` 的具体条目。
  - 10 万文件的基准用虚拟仓库：`testing::virtual_repo`（`FakeGit::stage_oids` 只有 index 条目，`assume_unchanged` 让工作区比较为空，单元是 `synthetic::extraction` 的合成抽取结果：每个文件 3 个自己的名字、27 个取自 2 万个共用名字，10 万文件约 32 万个不同的名字，对应调研对名字表 4–8 MB 的估算）；查询要读的文件才写到磁盘上。`goto_indexed` 的当前文本 64 KiB，目标名字有约 130 个定义。
- `testing`：`TestHost` 改为直接启动 `Kernel`、不经 `TestKernel`（`TestKernel` 记下目标插件发出的每个事件，会一直持有名字表）；`FakePower`、`FakeGrammars` 的订阅返回可退订的守卫（`subscribers()` 只数没退订的）。`Stats` 新增 `prepares`（跳转第 1–3 步的次数，预取的测试用）。
- 依赖：`memchr`（限时搜索的 `memmem`）和 `unicode-normalization`（名字表的 NFC）加入 `[workspace.dependencies]`，两者原已在 Cargo.lock 里，没有新增 crate。
- 完整性的核算（新增「完整性的核算」一节，「限时搜索」「查询的视图」已改）：原来 `Complete` 由几处各自的计数决定：搜索在范围搜完、没有在途时直接报 `Complete`，不看子项目之外；查引用另记读不出的已入索引文件；`Named` 自己数；读到超过 1 MiB 的文件直接跳过。审阅指出三处由此而来的漏报：子项目之外没入索引的文件不计（`pa/a.ts` 跳到只在 `pb/c.ts` 里定义的名字，返回空的 `Complete`，查引用漏掉其他包的调用方）；命名空间满了之后提示冻结，被挤出的文件提示仍匹配，搜索把它当作已入索引跳过（10 万文件的仓库约 5%）；第 1 层按 4 MiB 入索引的 1–4 MiB 文件，查引用按 1 MiB 读，读不出就丢掉。现在只有一处决定完整性：每个能参与回答的文件分类一次。决定：`Complete` 保持接口文档的含义（「能参与回答的文件都已入索引或在这次查询里搜过」），范围之外没入索引的文件计入 `outside`，三种查询一样；限时搜索仍只搜当前子项目。满额模式的核对除了范围里的文件（搜索之前，约 2.3 ms），也核对范围之外提示匹配的文件（遍历时）：只核对范围里的会让范围之外被挤出的文件仍算已入索引，正是这一类漏报；范围之外的核对按时间盒分条进行（见下一轮）。`View` 的淘汰代数改为提示戳记用的那个（满额后冻结），另加「是否已满」：满额后批量层自己的配额淘汰不再让并发的查询以 `Stale` 结束（`account_full_namespace_eviction_during_a_query_is_not_stale`）。
- 预取的沿用改在查询任务里、按它自己取的快照判断（「查询的视图」已改）：原来 `query` 在调用方线程上用当前视图判断，任务稍后才取快照，两者之间核对任务发布了新映射时，第 1–3 步来自旧映射、搜索来自新映射，同一个文件的两个 OID 都列出且 `Complete`。`check_view` 也比较数据的视图与快照的映射代数（`view_matrix_prefetch` 的「`query` 返回之后、任务取快照之前」）。
- 没有可发布快照的会话（「查询的视图」已补）：原来没有当前 worktree、文件列表 `Unavailable` 时等着的查询一直 `Running`；现在以 `Failed(Unreadable)` 结束。工作区比较失败或根目录打不开时发布的快照标为缺工作区内容，结果不是 `Complete`（原来按只有基础部分的映射回答 `Complete`）。
- `NavigationProvider::cancel` 写明「立即返回，不阻塞；可在任意线程调用」（「语义导航的注册」的接口已补）：它在 UI 线程上被调用（`close`、超过 16 个查询时挤出最旧的）。会话结束时原来在 `publishing` 锁里调用它，现在收集起来，放开锁之后再调用（`escalate_cancel_runs_outside_publishing`）；关闭的查询持有的映射、作废的预取交给 `Background` 任务释放，不在 UI 线程上放掉最后一份语言包租约或约 3 MB 的映射。
- 完整性的核算，第二轮（「完整性的核算」「blob → 路径映射」已改）：审阅又指出三处。批量层存成 `TooLarge`（没打开过的 1.5 MiB 生成文件）或 `TimedOut` 的文件有匹配的提示，原来算已入索引、搜索也跳过，只在它里面定义的类型跳转返回空的 `Complete`；修改过或未跟踪、超过 1 MiB 的文件是 `Unindexable` 覆盖项，遮住了基础条目，原来从核算里消失。决定修复、不作为已知限制：本次会话记下单元没有内容的键（`State::unfinished`），核算计为未搜，搜索按交互上限读它们；`Binary` 算已入索引（没有定义可找）；`WorktreeState` 分出 `Excluded`（过滤器的指针文件、符号链接），`Unindexable` 只剩超过 1 MiB 和读不出，计为未搜。局部定义的查引用原来不看自己的文件有没有搜成（大文件按后台的 250 ms 解析，超时就是零个引用、`Complete`），现在要它搜过，并按交互上限解析；按 OID 读、映射在这个路径上是另一份内容的被查定义所在的文件没能搜时也计入。`finish` 原来在最后一次比较视图之后又读提示（被读过的文件按当时的提示算出遍历里的分类）；现在遍历时定下这些分类，`finish` 不读提示。满额模式范围之外的核对原来一口气做完（10 万文件约 280 ms），不检查停止，`Done` 晚到约 1.28 s，超过「到停止 ≤ 1.1 s」，关闭查询也不停；现在每条语句之前检查停止和时间盒，没核对的计入 `outside`。另按审阅的建议缓存遍历的结果，`goto_indexed` 的重复跳转不再遍历（「实际验证」有第一次与重复两个数字）。
- 替换、局部性和分层（「调度、批次与取消」「候选的分层与过滤」「查找引用与调用方」和 `DefinitionRef::local` 已改）：审阅指出三处。查询现场按交互上限重做一个 `TooLarge`、`TimedOut` 的文件得到 `Done`，写入时按第 2 层判断，存着的单元「满足」这一层，`Done` 的结果被丢掉，`State::unfinished` 不变，每次跳转都在时间盒里重新解析它，`Named` 一直列不出它的定义；现在 `Done` 替换没有内容的单元，不论哪一层写入（`outranks`）。再审又指出写入前的存在检查在取写锁之前，其间另一次写入落下的 `Done` 会被第 1 层的 `TimedOut` 覆盖；现在存在检查在写锁里进行（所有写入都取这把锁），替换只有 `outranks` 这一种，没有内容的结果也不再替换另一个没有内容的单元（原来第 1 层会这样做，只多一次淘汰计数）。`Named` 的候选原来把 `local` 固定为 false，从 ⌘K 选中一个嵌套的 `const helper` 查引用时会查整个项目；`ci_def` 不存这个标记，持久化要在抽取时跑 locals 查询的定义判断、改存储格式和全部抽取快照，所以不持久化，由 `References` 确定：不论 `DefinitionRef` 由谁给出，都按被查定义所在文件的解析确定，用的是查引用本来就要做的那一次解析（`refs_nested_helper_from_named_is_local`）。第一版修复在跳转第 3 步和 `Named` 里为每个 `File` 候选的文件读一次、解析一次来恢复它，再审指出这在 ⌘K 交出结果之前加了没有上限的整文件解析（`File` 的候选有多少个文件就解析多少个），`Object` 一侧的跳转还会为工作区里的同一路径再解析一次；现在去掉了，来自索引的定义的 `local` 一律为 false（不知道），列出候选时不解析任何文件（`refs_named_parses_no_file`、`goto_object_side_parses_only_its_text`）。其余构造 `DefinitionRef` 的地方：第 1 步来自当前文本的解析；第 2 步只取不是 `File` 的导出定义；限时搜索的候选按可见性只来自其他文件，`File` 的进不来；都已正确。分层原来在没有当前文本时把 `near` 路径上的定义算作「当前文件」（`is_none_or`），两个都不在任何子项目里的文件算作同一子项目（两边都是 None）；现在每一层都要所比较的一方已知，`file_order` 同样（`goto_tiers_need_what_they_compare_with`）。
- 交错矩阵：文件改用语言包的语言（`jpack`，Java grammar，`jvm` 组），语言包升级和同组安装改变查询用的语言；每种变化作用于对这种查询有贡献的文件（跳转和 `Named` 是定义所在的 c、d，查引用是引用所在的 e、f），原来提交、换映射、改文件都作用于没有引用的 d.ts，查引用的那几格测不出遗漏。另加位置「`query` 返回之后、任务取快照之前」。
- 交互份额、解析的保留、按名字查找的上限、核对的停止和查引用的批次（[ADR 0037](../adr/0037.md)；「跳转到定义」「候选的分层与过滤」「查找引用与调用方」「限时搜索」「完整性的核算」「存储表」「性能」已改）：自审指出份额只在最容易的状态下成立。`goto_indexed` 重复同一次跳转（遍历已缓存）、当前文本只有 64 KiB，按名字查找没有上限（约 3000 个定义的名字多约 25 ms），10 万文件通常处在满额模式，根作用域和不带 `near` 的 `Named` 的核对没有上限也不能取消；性能表的分项与实际验证互相矛盾。现在份额是到第一批有候选的结果，在遍历未缓存、常见名字、满额模式下分别判定，当前文本是 100 KB；当前文本的解析按 blob OID 保留，同一文本里的跳转不再解析，冷路径只记录；按名字查找分页，第一批是第一页加最近的文件，其余页在之后的批次里；每条核对语句之前检查停止和时间盒，没核对的计为 `Unchecked`；`Named` 在遍历之前交出候选。`ci_def_name` 改为 (name, unit)，命名空间版本 2。查引用另有两处：读的上限按单元的上限（4 MiB），解析的截止时间却按「是不是被查定义的文件」，1.5 MiB 的 `TooLarge` 文件和第 1 层抽取过的 1–4 MiB 的文件读进来之后总在 250 ms 时超时、调用方找不到；现在两者按同一条规则（`Snap::file_limits`）。结果的批次只在解析完一个文件之后检查计时，`ci_ref` 一轮的结果可能等到约 1 s 之后；现在 `ci_ref` 一轮之后立即交出，限时搜索每读一个文件检查一次计时。另：`Store::defs_by_name`、`refs_by_name` 保留，改为逐页读完（存储的测试用）。
- 交互份额，第二轮（[ADR 0037](../adr/0037.md) 已改，并修订 [ADR 0030](../adr/0030.md)；「存储表」「名字表」「跳转到定义」「查找引用与调用方」「完整性的核算」「性能」已改）：自审又指出七处，各自从根上改：
  - 一页的 `LIMIT 256` 数的是通过 `ci_ext` 连接和版本、状态筛选的行：旧版本的单元（淘汰之前不删除）、另一语言组的页要走过名字的全部行，一条不能取消的语句可读几千到约 5 万行（`named_50k` 的第 2 组约 5 万次查找）；一个单元里同名定义超过 256 个时，其余的不读却仍报 `Complete`。现在页先在索引里取 256 行、再 `LEFT JOIN`，版本在 Rust 里比对，一次扫描覆盖所有版本，游标是 (unit, kind, rowid)。
  - 第一批的「最近的文件」按路径顺序走整个子树、覆盖部分排在最后，截在 2048 个，再按 OID 顺序每条 `LIMIT 256`：同目录的文件或刚改的文件可能不在第一批里，结果只追加，预选也就偏远。现在先查所在目录自己的文件（含覆盖部分，审阅集合在前），再按距离查子项目的其他文件，两段各自用语句。
  - 保留的解析从最后一次用到起 60 s 过期，读了一分钟的文本里跳转走冷路径（约 24–33 ms，只记录），与「冷路径在 50 ms 之内」的说法不符。现在当前文本的那份一直保留，不再是当前文本时才开始计时；冷路径按 50 − 18（抽屉）= 32 ms 判定（判定点见下面第三轮：96 KiB）。
  - 查引用与跳转共用 1 s 的时间盒，`ci_ref` 一轮就要 0.83–1.04 s，满额模式的核对总被截断，全部已入索引的 10 万文件报 `Partial { outside }`，基准也没检查。现在查引用的时间盒是 1.9 s；`references_full/all` ≤ 2 s，`references_full` 和 `goto_full` 每次都要 `Complete`。
  - 超过 128 KiB 的当前文本每次跳转都重新解析，超过 50 ms，ADR 0037 改了 ADR 0030 的 50 ms 却没有记下：现在记为对 ADR 0030 的修订，两边的状态行和索引都已改（修订的范围见下面第三轮：96 KiB 以内任何时候成立，96–128 KiB 靠保留的解析成立）。
  - 命名空间版本在本文里写成 1 和 2 两种：现在统一为 2，「存储表」的「版本」写明原因。
  - 名字表一条语句扫描整张 `ci_def`（约 1 s，不能停），每个定义查一次哈希表、为新名字分配：现在按 `ci_def_name` 的顺序分页读索引，与前一串比较去重，每个不同的名字一次 NFC。
  - 另：`parse_cache_bytes` 原来测两份 100 KB，说的是上限；现在在上限处测两份略小于 128 KiB。
- 交互份额，第三轮（[ADR 0037](../adr/0037.md)、[ADR 0030](../adr/0030.md) 的状态行、「查找引用与调用方」「性能」「验收」已改）：
  - 冷路径的判定点从 128 KiB 改到 96 KiB：128 KiB 的冷跳转在 Linux 上实测 29.6–35.9 ms（整次跳转：按 ADR 0034 的 TS 约 6 MiB/s，解析约 21 ms，其余是 locals 和查找），与 32 ms 的份额没有余量，macOS 上解析不比它快。现在 ADR 0030 的 50 ms 对 96 KiB 以内的当前文本任何时候成立，对 96–128 KiB 的靠保留的解析成立（成为当前文本时预热，是当前文本期间一直保留），预热完成之前的冷跳转是已知限制；`goto_cold_parse` 在略小于 96 KiB 的文本上判定 ≤ 32 ms（24.7 ms），`goto_cold_parse/at_128k` 只记录。预热进行中到来的冷跳转不等预热，理由见「跳转到定义」的「当前文本的解析」。
  - 查引用第 2 步读 `ci_ref` 给出的已入索引文件时不看时间盒：每个文件的解析截止时间是交互的 1 s，逐个文件只检查是否已停止，几千个文件用到的名字约 5–15 s，超出 1.9 s 的时间盒和 ADR 0030 的 2 s；`ViewCheck::Grew` 时的第二次调用一样。现在截止时间截到时间盒，逐个文件之前和读 `ci_ref` 的页之间都检查时间盒，到时剩下的文件记为 `NotReached`、没读的单元按索引数出，都计入 `unsearched`（`refs_indexed_files_stop_at_the_time_box`：每次读文件让时钟走 400 ms，12 个用到它的文件，找到的加上未搜的正好是 12、少于 12 个被搜；按原来的代码 12 个都被搜）。基准的 300 个调用方测不出这一点。
  - 本节第二轮里两处「128 KiB」的说法、「验收」第 4 步少了 `goto_cold_parse` 和 `references_full/all` 两条判定，一并改正。限时搜索的超出取决于看门狗 10 ms 的检查间隔，没有改：在抽取的进度回调里比较时刻需要把「自己停下」与「看门狗置位」区分开（否则记为解析失败），而且每次回调读一次时钟，`extract_throughput` 本来就贴着 4 MB/s；超出仍在 20 ms 的取消预算之内（`search_stop` ≤ 1.02 s）。
- 基准夹具跨运行保留（[#7](https://github.com/SoloJiang/lectern-app/issues/7)，[ADR 0039](../adr/0039.md)，[bench 设计「夹具与 `test-support`」](bench.md#夹具与-test-support)）：`testing::VirtualTemplate` 移到 `testing/template.rs`，另有 `cached(root, n, common, budgets)`：写好的模板按键（`testing::template_key`）保留在 `root/<键>/`，使用前复制一份，对照当前的写入逻辑核对单元数和抽样文件的单元，不符就重写（`template_*`）。一个文件写什么只由 `testing::virtual_unit` 决定，`virtual_repo_sharing` 和核对都用它。`open` 的副本经缓冲区逐字节写出，不用 `std::fs::copy`（APFS 上可能是 `clonefile`，克隆不共用页缓存，限时分页的查询在冷的副本上提前停下；#136 的第一次 macOS CI 里 `goto_popular` 剩下 686 个未搜、`named_50k` 只列出 22016 个，Linux 上同样的查询 36 ms 内完成）；`goto_*` 和 `named_50k` 的检查失败时报告查询用了多久、时间盒多长、副本里存着多少单元和淘汰代数（`code_intel_query.rs` 的 `diagnose`）。`build` 和 `virtual_repo_sharing` 的公共名字改为 `&[(名字, 每几个文件一个)]`，共享仓库另带 `id`（每 2 个文件一个），`code_intel/named_50k` 改用它（约 4.5 万个定义，原来是自己的 6 万文件仓库、约 5.4 万个）。虚拟仓库每 1024 个单元一个事务（原来 64 个），10 万文件写入约为原来的三分之一时间。

删除电源代码（[ADR 0032](../adr/0032.md)「影响」）：

- 删除 `PowerSource`、`AlwaysOnAc`、`testing::FakePower`、`CodeIntelDeps::power` 和 `IndexDeps::power`、`Indexer::power_changed`、`Indexer::demand` 与 `State::demand`、`wide_allowed`、`coverage` 里的 `Deferred`、`battery_*` 测试，以及 `services` 的 `TierState::Deferred`。第 3–5 层总是可以运行；`Indexer::subscribe` 只订阅语言包。上文第 3 步、第 4 步各条里提到这些名字的地方是当时的记录，不再是现状。
- `query` 和 `names` 不再调用 `demand`，所以它们不再唤醒批量任务。原来这次唤醒掩盖了一个漏掉的唤醒：批量任务之外的写入（第 1、2 层，查询的限时搜索现场抽取）命中了要标记访问的单元，或者写入引起淘汰、让遍历重新开始，都会产生第 3–5 层的工作，却没有唤醒（`account_timed_out_units_are_not_indexed` 在去掉 `demand` 之后由 `Harness` 的活性检查发现）。现在这样的写入之后唤醒一次（「调度、批次与取消」的活性一条已补）。自审又找到一处同类的漏洞：第 1、2 层的存在检查命中时不经写入、直接排进访问标记，也没有唤醒；温热的项目里打开一个文件，`set_focus` 唤醒的批量任务可能先跑完、停下，第 1 层才命中，标记一直等到下一次唤醒，配额已满时常常先被淘汰丢弃，打开过的文件因此更早被淘汰、重新抽取。现在排进访问标记只经 `Batch::queue_touches`，它返回必须交给 `Inner::wake` 的 `Wake`；写入之后的唤醒也是一个 `Wake`（`warm_focus_hit_wakes_the_batch_for_its_touch`，去掉第 1 层这一处唤醒时失败）。
- 测试里在索引背后删除单元的两处（`derived_eviction_rechecks_and_reextracts_only_what_went`、`view_*` 的 `Evict`）原来靠之后的 `demand` 唤醒，现在直接调用 `kick_batch`。已知限制：同一分库里其他命名空间的写入按分库预算淘汰本 crate 的单元时，第 3–5 层要等下一次唤醒（打开、焦点、文件事件、本 crate 的写入）才重新遍历；原来每次查询都会唤醒一次。查询的结果不受影响：淘汰代数变了，提示不再匹配，查询按「完整性的核算」搜索或计为未搜。

第 7 步（交互让路）：

- 只接入第 4、5 层。依赖层还没有实现，随第 9 步用同一套机制（让路的守卫、`proceed_item`、`Parked`）；窗口链随第 8 步，届时与第 3 层一样持有不让路的守卫、按不让路的请求方取 `LargeSlot`。
- 停下的状态经抽取的 deadline 传递：`Deadline::yielding(probe, slot)` 带让路的探针和 `ResumeSlot`，抽取停下时把 `Resume`（未 reset 的 `Parser`，或语法树加遍历的位置和已有的输出，以及剩余预算）放进槽里、返回 `ExtractStatus::Parked`，批量任务把它包成 `Outcome::Parked`。所以测试换上的抽取钩子（`Indexer::set_extractor`）照样经过停下和继续。查询不用让路的 deadline，`query::definition`、`search` 里的 `Parked` 按 `Cancelled` 处理。遍历停下时保存的是游标的后代序号和各层上下文对应的源码范围，继续时用 `goto_descendant` 回到原处、从祖先重建栈。
- 遍历之后按规则整理单元的一步（`rules::finish_with_refs`）不分片，在同一次登记下做完；它只读遍历的输出，比解析和遍历短得多。
- 语言包的放弃经看门狗：tree-sitter-tags 没有分片边界，看门狗每次检查（`CHECK_INTERVAL`，10 ms）也看让路的探针，为 true 时置位停止、记为新的 `Trip::Yielded`，抽取返回 `Parked`、槽是空的，批量任务把这一项放进暂缓列表（`Outcome::Aside`）。所以语言包至多晚一次检查停下；`yield_stop` 只测核心语言。
- 批次的顺序：原来先做三层的「恢复后先做」小队列，再按层遍历；现在第 3 层的小队列和遍历在第 4、5 层的小队列之前（`plan` 先看要不要不让路的守卫），否则第 4 层放回的项会让第 3 层的工作排在让路的守卫后面。热启动之后标记访问（`touch_pending`）算作让路的工作，持有让路的守卫做。
- 边界向 `proceed_item` 要的大小在每次 poll 时重新算（`boundary_bytes`）：停下的项不超过 256 KiB 时是它的大小；否则这一层还有时段能做的工作时是 0（时段里读出下一项，读出来超过 256 KiB 就放进暂缓列表、接着取下一项，读的字节计入这一批）；只剩暂缓的项或停下的大项时是 `SLOT_MAX_BYTES + 1`，等开关关闭。暂停时 kernel 唤醒等待许可的 future，它看到 `pause_requested()` 就丢掉停下的状态、放回这一项，再按暂停等在边界上。
- 暂缓列表（`Batch::aside`）是设计之外加的：设计里超过 256 KiB 的项和放弃的语言包项放回队列前面，开关打开时会挡住这一层后面的项，与 ADR 0033「持续滚动时第 4、5 层照样前进」不符。暂缓的项保留 `queued` 里的键（同一 OID 不重复排队），换代重建游标、第 5 层换代时与队列一起清空，`coverage` 把暂缓的项计为未完成。开关关闭后 `far_step` 先把它们按原顺序放回队列前面（`drain_aside`），读出时照常核对 OID。
- 换项目时放弃停下的项：等待的 future 每次 poll 都检查运行第 3–5 层的项目还是不是它（`Arc::ptr_eq`）、停下那一项的会话是否还在，不是就返回 `Interrupted`，循环开头丢掉停下的状态、交还 `LargeSlot`，再按新的项目取守卫。设计只写了第 3 层的工作打断等待。
- `LargeSlot` 的接手：停下的持有者把 `ResumeSlot` 登记在 `LargeSlot` 里，不让路的请求方取得时清空那份状态、换成新的一轮（`LargeTurn` 带轮次，作废的一轮 drop 时不释放新的一轮），再唤醒等待的批量任务；持有者在边界上看到 `revoked()` 就放回这一项，继续抽取前也先 `unpark()` 确认这一轮还有效。设计写的是请求方等持有者交还，那需要持有者的任务先运行：批量任务数多于批量线程（或测试里单线程的 executor 上两个任务）时，请求方在线程上等、持有者没有线程运行，会卡住。现在不让路的请求方接手不依赖持有者运行；让路的批量任务等 `LargeSlot` 时仍在线程上等持有者交还，所以批量任务数仍不得多于批量线程数（默认配置二者相等，现有测试不触发）。本任务自己停下的大项挡着本任务的第 3 层大文件时，仍直接放弃自己的那一项（提前释放它的字节）。
- `code_intel/yield_stop` 直接调用 `extract`，让路的探针读一个由测量线程置位的标志，不经过 kernel 的开关：从置位到抽取返回 `Parked`、状态在槽里。kernel 从开关到 `yield_requested()` 的时间由 kernel 的测试负责。
- 测试：`yield_resumed_extraction_is_identical` 每种语言用 120 KiB（在时段里停下再继续）和接近 1 MiB（开关打开时停下、关闭后继续）两组，见「实际验证」；`yield_large_parked_item_gives_way_to_tier3` 分别用一个和两个批量任务，在第 3 层抽取大文件时检查 `LargeSlot` 里没有登记的停下的状态，确认两棵大树不同时在内存里，不读活跃字节；`yield_large_slot_taken_over_from_a_parked_task` 在两个线程上检查接手不需要持有者运行；窗口链的部分随第 8 步。测试用的 `Inner::yield_hook`（只在 `cfg(test)` 下）在每次让路检查时运行，用来在抽取进行中推进时段的时钟或打开开关。
