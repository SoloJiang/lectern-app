# code_intel 设计

状态：实现中（设计 [#107](https://github.com/SoloJiang/lectern-app/pull/107)）。第 1 步（语言注册与抽取，[#113](https://github.com/SoloJiang/lectern-app/pull/113)）已实现；第 2–5 步尚未实现（见「验收」）。

对应 issue [#17](https://github.com/SoloJiang/lectern-app/issues/17)。本文覆盖导航的第 1 层「语法索引」：用 tree-sitter tags 按 blob 抽取定义和引用名，存进 `cache` 仓库分库的自定义表，经当前 worktree 的「blob → 路径」映射过滤，随 `fs_watch` 的批次增量更新；索引还不完整时，跳转到定义、查找引用和 ⌘K 的符号查找按固定顺序现场回答（[技术调研「代码导航与编辑」](tech-research.md#代码导航与编辑)、[代码导航调研](../research/code-navigation.md)）。依赖与延迟预算见 [ADR 0030](../adr/0030.md)，后台索引与静止的关系见 [ADR 0029](../adr/0029.md)。

## 范围

- 做：
  - 符号抽取：每个（blob OID、语言、grammar 版本、查询版本）抽取一次定义（名字、种类、位置、所在作用域、可见性、参数个数）和引用名，带单文件截止时间和大小上限。
  - 存储：`cache` 仓库分库（文件夹项目用文件夹分库）里的三张自定义表，在分库内单独计磁盘份额，按「最后一次出现在 worktree 里」的时间淘汰。
  - 每个打开项目的当前 worktree 的「blob → 路径」映射：index 里的 OID 加上未暂存的修改、未跟踪且未被忽略的文件；改掉或删掉的内容不再出现在查询结果里。
  - 建索引的五层优先级与调度（ADR 0029）：每层持有的守卫、所在线程组、批次、取消、暂停、电池策略。
  - 增量更新：消费 `fs_watch` 的 `WorktreeBatch`；重新打开项目时只抽取缓存里缺的键。
  - 查询：跳转到定义、查找引用与调用方、按名字列出定义。包括绑定依据规则、语言兼容组、候选分层与过滤、调用方解析，以及索引不完整时的回答顺序和限时搜索。
  - ⌘K 的符号名字表，供 [#102](https://github.com/SoloJiang/lectern-app/issues/102) 使用。
  - `code-intel` 插件，以及它在 `services` 里提供的 `dyn CodeIntel` 接口。
  - 两个扩展点：本 crate 内部的解析扩展点（[#18](https://github.com/SoloJiang/lectern-app/issues/18)、[#19](https://github.com/SoloJiang/lectern-app/issues/19) 接入），语义导航的注册（[#26](https://github.com/SoloJiang/lectern-app/issues/26) 接入）。
- 不做：
  - Java 的 package、import、全限定名与同名方法消歧（#18）；TS / JS 的相对路径 import、tsconfig `paths`、工作区包名与子项目之间的依赖（#19）。它们是本 crate 的后续 PR，经「解析扩展点」接入，本文只定义接口和调用时机。
  - 上下文抽屉的界面、结果的分组与呈现、「未跟着改的调用方」的判断（[#20](https://github.com/SoloJiang/lectern-app/issues/20)，`context_drawer`）；⌘K 的匹配、排序、分组与界面（#102）。本 crate 只交出数据。
  - jdtls 与 SCIP 导入（#26）：独立进程插件（[ADR 0018](../adr/0018.md)），经「语义导航的注册」接入。
  - 语法高亮、高亮与文本的 LRU、后退前进（`file_reader`、[#10](https://github.com/SoloJiang/lectern-app/issues/10)）。高亮方与本 crate 依赖同一批 grammar crate，链接进二进制的只有一份。
  - 文件列表与子项目（`project`）、工作区比较（`git`）、监听（`fs_watch`）、安全读取（`platform`）、SQLite 的连接与淘汰（`cache`）。本 crate 只调用它们的接口，需要它们新增的部分列在「对其他 crate 的要求」。
  - 语言包的下载、校验和加载（`marketplace` 与 kernel 的 grammar 加载器，ADR 0018）：本 crate 只定义接收已加载 grammar 的接口 `GrammarSource`。
  - 依赖源码（导航第 3 层，[ADR 0007](../adr/0007.md)）与文件内容的全文搜索。
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
- `kernel`：`PluginHandle::background()` 与 `BackgroundGuard`、批次边界的暂停检查，以及不阻塞、任何线程可调用的 `BackgroundGuard::pause_requested()`（看门狗用它在批次中途打断在途抽取）（[kernel 设计「静止」](kernel.md#静止)，ADR 0029 规定由单独的 PR 先于本 crate 实现）。
- `executor`：批量线程组与 `Priority::Batch`（[executor 设计](executor.md)，同上）；另加 `Executor::on_shutdown(f: Box<dyn FnOnce() + Send>) -> ShutdownHook`：`shutdown` 开始时在调用线程上依次调用已登记的回调（在 `contain` 里），`ShutdownHook` drop 时注销。专用线程上的闭包靠它得知 executor 正在关闭（本 crate 的看门狗，见「调度、批次与取消」）。
- `platform`：电源状态与变化通知（ADR 0029「影响」）。`app` 把它包成本 crate 的 `PowerSource` 传入；本 crate 不依赖 `platform` 的电源接口，测试用 `testing::FakePower`。
- `git`：`Worktree::canonical_oid(path, bytes) -> Result<Option<Oid>, GitError>`：对一份工作区字节按这个路径的属性和配置做 to-git 方向的内置转换（`text`、`eol`、`ident`、`working-tree-encoding`），返回规范内容的 OID；有外部过滤器时返回 None。工作区比较第 3 步已经在做同样的转换，只是没有单独公开。其余用到的接口（`changes`、`tracked`、`blob`、`head`、`index_checksum`）已经存在。
- `fs_watch`：不需要新接口。
- kernel 的 grammar 加载器（ADR 0018、[#39](https://github.com/SoloJiang/lectern-app/issues/39)、#42）：`tree_sitter::Language` 和由它得到的 `Parser`、`Query`、`TagsConfiguration`、`Tree` 都持有动态库里的函数指针。加载器给出的每个 `LoadedGrammar` 带一个租约（`GrammarLease`，加载的模块句柄的 `Arc`）；更新、回滚、撤销时加载器只停止交出这个版本，模块的卸载（`dlclose`）推迟到它的最后一个租约 drop 之后。撤销要求的「立即停用」由不再交出新的 `LoadedGrammar`、使用方取消在途工作来满足。
- 高亮方（`file_reader`）：依赖同一批 grammar crate、同一个 tree-sitter 版本。[#42](https://github.com/SoloJiang/lectern-app/issues/42) 指出 external scanner 不能被进度回调打断，它对 scanner 的处理（如对某个 grammar 停用）同样作用于本 crate。

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
    /// 电池供电时，这次调用让第 4、5 层恢复（见「建索引的分层」）。
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
}

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
    /// 定义节点的最后一行；引用为引用所在的行。
    pub end_line: u32,
}

/// 调研第 6 节的六层，靠前的先列出。
pub enum CandidateTier { CurrentFile, Imported, SamePackage, SameSubproject, DependentSubproject, Elsewhere }

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
    /// 当前文件超过按需抽取的上限（4 MiB）。
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
}

pub enum TierState { Pending, Done, Deferred /* 电池供电时暂缓 */ }
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
  - 先后：`head_only` 的提供者（SCIP，索引对应的提交与 HEAD 相同）负责内容与 HEAD 相同的文件；其余提供者（jdtls）负责有改动和未跟踪的文件，`head_clean` 为 false 时只请求它们。同一类里按提供者 ID 的字节序。
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
    /// 电源状态。`app` 用 `platform` 的电源接口实现；Linux 上用 `AlwaysOnAc`。
    pub power: Arc<dyn PowerSource>,
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
//   `PowerSource::subscribe`、`GrammarSource::subscribe` 的返回值交给 `PluginHandle::own`，停用时随守卫 drop。
// 项目关闭、切换当前 worktree 时：drop 这个项目的 `RepoWatch` / `WorktreeWatch`、取消它的任务、对未回复的语义请求调用 `cancel`；
//   停用时 kernel 撤销守卫和任务，同样的清理在 `Active` 的 `Drop` 里做完（看门狗见「调度、批次与取消」）。

pub trait PowerSource: Send + Sync {
    /// 当前是否用电池供电。不阻塞。
    fn on_battery(&self) -> bool;
    /// 电源状态变化时在任意线程上调用 `f`；`f` 只入队，不阻塞。返回值 drop 时退订。
    fn subscribe(&self, f: Box<dyn Fn(bool) + Send + Sync>) -> Box<dyn Send>;
}
pub struct AlwaysOnAc;

pub trait GrammarSource: Send + Sync {
    /// 已加载、已校验的语言包 grammar（kernel 的 grammar 加载器给出）。阻塞，只在 executor 上调用。
    fn grammar(&self, lang: &LangName) -> Option<LoadedGrammar>;
    /// 语言包认领的扩展名，如 "kt" → "kotlin"。不阻塞。
    fn language_for_extension(&self, ext: &str) -> Option<LangName>;
    /// 已装语言包集合的代数：安装、更新、回滚、撤销都加一。不阻塞。
    fn generation(&self) -> u64;
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

/// `test-support`：`FakeGit`、`FakeProjects`（内存里的 `dyn Projects`）、`FakePower`、`FakeGrammars`（可以安装、更新、回滚、撤销的语言包）、`ManualClock`、
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

pub(crate) struct Import { pub target: RelPath, pub names: ImportedNames /* All | Named(Vec<(本地名, 导出名)>) | Namespace(本地名) */ }
pub(crate) struct Bound { pub definition: DefinitionRef, pub binding: Binding }
```

`ResolveContext` 给出当前项目快照（`FileList`、子项目）、「blob → 路径」映射、按路径取已抽取的结果（命中缓存或现场抽取）。Go 的同包解析这类其他语言的规则以后也经这个 trait 加入。

### 依赖 git、fs_watch 与 cache 的接口

本 crate 只通过一个内部 trait 访问 git，生产实现转调 `lectern-git`，测试用 `testing::FakeGit`。都是阻塞调用，只在 executor 上执行；每次调用打开自己的句柄，返回时释放，不在两次调用之间持有 `git::Worktree` 和它的 `TrackedPaths`：

```rust
pub(crate) trait GitAccess: Send + Sync {
    /// 当前 worktree 里内容可能与 index 不同的路径：`Worktree::changes(Base::Head, limits)`，
    /// `limits` 取 `total_files = 0`、`total_patch_bytes = 0`、`retained_bytes = 0`，其余为默认值，
    /// 所以不算行 diff、不保留内容，只取每个改动的路径（含重命名的来源）。
    fn changed_paths(&self, root: &Path, cancel: &AtomicBool) -> Result<ChangedPaths, GitError>;
    /// 另一个 worktree 的 index 条目（第 5 层）。
    fn tracked(&self, root: &Path, cancel: &AtomicBool) -> Result<Arc<git::TrackedPaths>, GitError>;
    /// 对象库里的一个 blob（`TextOrigin::Object`）。
    fn blob(&self, root: &Path, oid: BlobId, max: u64) -> Result<Arc<[u8]>, GitError>;
    /// 当前 worktree 的 HEAD 提交（`Worktree::head`）；unborn 为 None。HEAD 输入的代数由它决定。
    fn head(&self, root: &Path) -> Result<Option<BlobId>, GitError>;
    /// index 文件末尾的校验和（`Worktree::index_checksum`），不解析条目。index 输入的代数由它决定。
    fn index_checksum(&self, root: &Path) -> Result<Option<BlobId>, GitError>;
    /// 按这个路径的属性和配置把工作区的原始字节转成规范内容，返回它的 blob OID（与 `git add` 得到的相同）。
    /// 设置了外部过滤器（clean / process）的路径返回 None。只在原始字节的 OID 与期望的不同时调用。
    fn canonical_oid(&self, root: &Path, path: &RelPath, raw: &[u8]) -> Result<Option<BlobId>, GitError>;
    /// fs_watch 的过滤器与配置来源，见下文。不做 IO。
    fn watch_source(&self, root: &Path) -> Arc<dyn lectern_fs_watch::WorktreeSource>;
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

- HEAD 差异：每次工作区比较的 `head` 存成项目的 `HeadDiff`（路径 → HEAD 里的 OID），连同这次比较时 HEAD、index、文件列表的代数。它与覆盖部分分开：覆盖部分只描述工作区与 index 的差别，只暂存的改动（工作区等于 index、不等于 HEAD）没有覆盖项，却在 `HeadDiff` 里。一个路径与 HEAD 相同（`head_clean`）当且仅当它在 HEAD 里的 OID 已知且等于当前工作区的 OID（覆盖项的 OID，没有覆盖项时是文件列表的 OID）：在 `HeadDiff` 里的取记下的 OID，不在里面的取文件列表的 OID。只在记下的三个代数都等于当前代数时成立，否则按「不相同」处理，直到核对任务重新比较完成（「输入代数与派生状态」）。`SemanticRequest::head_clean`、`head_only` 提供者的选择和它返回位置的过滤都只用这条规则。
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
    /// 超过后台上限（1 MiB）。文件被打开时按交互上限（4 MiB）重做。
    TooLarge,
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

一次抽取是 `cache` 的一个单元，命名空间 `code_intel.tags`（版本 1，不压缩），键是 `Key::new().bytes(oid).str(lang).u64(grammar).u64(query)`，单元的字节数由本 crate 按行估算（见下）。本 crate 登记的表：

```sql
-- 一次抽取一行。unit 是 cache 分配的单元 ID；单元被淘汰或删除时，外键级联在同一条语句里删除下面三张表里它的全部行
-- （cache 设计「自定义表」的登记规则：每张表有一列 REFERENCES entry (id) ON DELETE CASCADE，且这一列有索引）。
CREATE TABLE ci_ext (
  unit    INTEGER PRIMARY KEY REFERENCES entry (id) ON DELETE CASCADE,
  oid     BLOB    NOT NULL,           -- 20 或 32 字节
  lang    TEXT    NOT NULL,
  grammar INTEGER NOT NULL,
  query   INTEGER NOT NULL,
  status  INTEGER NOT NULL,           -- ExtractStatus：0 Done、1 TimedOut、2 TooLarge、3 Binary
  UNIQUE (oid, lang, grammar, query)
);

CREATE TABLE ci_def (
  unit     INTEGER NOT NULL REFERENCES entry (id) ON DELETE CASCADE,
  name     TEXT    NOT NULL,          -- 原样，区分大小写
  kind     INTEGER NOT NULL,
  line     INTEGER NOT NULL,
  col      INTEGER NOT NULL,
  end_line INTEGER NOT NULL,
  scope    TEXT,
  flags    INTEGER NOT NULL,          -- 位 0–2：Visibility
  arity    INTEGER
);
CREATE INDEX ci_def_name ON ci_def (name COLLATE NOCASE);
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
- `ci_def_name` 用 `NOCASE`（ASCII 大小写折叠）：跳转按 `name = ?1 COLLATE NOCASE` 取出后在 Rust 里按原样比较；大小写不敏感的语言包可以直接用这个索引。`ci_ref` 只按原样的名字查。
- `ci_def_unit`、`ci_ref_unit` 供删除单元和建名字表按单元扫描。
- 版本过滤：所有读取都经 `ci_ext` 限定在当前语言表的版本上。每种语言有一个当前三元组（语言、grammar 版本、查询版本），查询把兼容组内每种语言的当前三元组作为参数：

  ```sql
  -- 按名字查定义（跳转第 3 步、按名字查定义）
  SELECT d.name, d.kind, d.line, d.col, d.end_line, d.scope, d.flags, d.arity, e.oid, e.lang
  FROM ci_def d JOIN ci_ext e ON e.unit = d.unit
  WHERE d.name = ?1 COLLATE NOCASE AND e.status = 0
    AND ((e.lang = ?2 AND e.grammar = ?3 AND e.query = ?4) OR …);
  -- 按名字查引用的单元（查找引用第 1 步）
  SELECT e.oid, e.lang FROM ci_ref r JOIN ci_ext e ON e.unit = r.unit
  WHERE r.name = ?1 AND e.status = 0 AND ((e.lang = ?2 AND e.grammar = ?3 AND e.query = ?4) OR …);
  ```

  存在检查和名字表的构建用同样的三元组条件。grammar 或查询升级、`EXTRACTOR_VERSION` 变化之后，旧版本的单元不再出现在任何结果里，只等淘汰。
- 单元字节数：`64 + Σ定义 (2 × 名字长度 + 作用域长度 + 40) + Σ引用 (2 × 名字长度 + 24)`，近似行加两份索引的大小，计入 `meta.bytes` 和命名空间配额。
- 写入的键来自读到的字节：任何一次抽取（批量、按需、限时搜索、第 5 层）写入之前，都对实际读到的字节重新算 OID（原始字节的 OID；工作区一侧与排队时的 OID 不同时再用 `canonical_oid` 算一次），用这个 OID 作键。排队时记下的 OID 只用来挑选要读的文件，不作键。两者不同（排队之后文件又被改了）时，结果只写在实际的 OID 下，原来那个 OID 的 `indexed` 提示不置位；路径的新内容随 fs_watch 的事件进入覆盖部分，有效 OID 变成实际的 OID 后提示才会匹配。所以一份内容的抽取结果不会写到另一个 OID 的键下。
- 写入：一批（见「调度、批次与取消」）在一个写事务里建单元、插入全部行，一批约 100 KB，远低于 `MAX_BATCH`；写失败（`Dropped`）时这一批的结果留在内存里供本次查询使用，下次需要时重新抽取。
- 淘汰：按单元的访问时间（LRU，精度 1 小时）。本 crate 在热启动的存在检查和查询命中时标记访问，所以访问时间即「最后一次出现在某个 worktree 里或被用到的时间」；不在任何 worktree 里的旧 OID 的行留给淘汰，不主动删除。当前 worktree 的单元也可能被淘汰，内存里的 `indexed` 提示按「输入代数与派生状态」处理。
- 规模（调研第 4 节，Rust 语料的密度）：`cloudprint-miniapps` 形状约 1 MB；10 万文件约 200–250 MB，在 256 MiB 的配额以内。超过配额时最久没出现的 blob 先被淘汰，下次需要时重新抽取。调研的引用数只含调用一类的引用；兜底的引用模式（见「外部格式」）让每个单元多出普通标识符和成员名的行，第 1 步的夹具里每个文件的引用名是原来的 1.5–3.5 倍，`code_intel/disk_100k` 在第 2 步实测后校准这一条。
- 版本：表结构或单元的键变化时命名空间版本加一；只是抽取规则变化时改 `EXTRACTOR_VERSION`，旧单元不再命中、逐步淘汰。

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
    by_oid: Box<[(u64, u32)]>, // OID 前 8 字节 + 文件列表下标，按前缀排序；查找时用 FileList::oid 核对完整 OID
    generation: u64,
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
    /// 存在，但不建索引：超过 1 MiB、读不出、路径中间是符号链接、LFS 这类过滤器的指针文件。
    Unindexable,
}
```

规则：

- 基础部分来自 `ActiveTree::files`：每条有 OID、语言受支持、`FileKind` 是 `File` 或 `Executable` 的文件。子模块和符号链接不进入。
- 覆盖部分来自工作区：对一个要重新读的路径，经 `SafeRoot::read`（上限 1 MiB）读出内容，按仓库的哈希算法计算 `Oid::for_blob`。等于文件列表里的 OID 时删掉这个路径的覆盖项；不等或文件列表里没有这个路径时记为 `Content`；`NotFound` 记为 `Absent`；`TooLarge`、`PathThroughSymlink`、读取失败记为 `Unindexable`。
- 覆盖项的 OID 按原始字节计算。设置了 `core.autocrlf`、`eol` 一类转换的文件，原始字节与规范内容不同，这些文件被当作有改动、按原始字节的 OID 另存一份结果；macOS 上这类文件很少，结果仍然正确，只多一次抽取（已知限制）。
- 查找按（OID、语言）进行：在两个 `by_oid` 里按前缀二分查找，核对完整 OID，跳过基础部分里被覆盖项遮住的路径，再只留按路径判断的语言等于这个单元 `ci_ext.lang` 的路径。同一份字节放在 `.java` 和 `.js` 下 OID 相同，`java` 的单元只对应 `.java` 的路径。
- 基础部分、覆盖部分何时重算见「输入代数与派生状态」。
- 内存：10 万文件的 `by_oid` 1.2 MB，`indexed` 提示约 1.6 MB（每条 16 字节）；覆盖项通常几十条，未跟踪文件最多 20 000 条（`git` 的上限）时约 1.5 MB。
- 文件夹项目：`FileList` 没有 OID，基础部分为空，每个文件在第一次被读到时作为 `Content` 加入覆盖部分（上限是文件夹遍历的 20 000 个条目）。

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

映射代数不是独立的输入：它由文件列表、工作区内容、语言包三者的代数合成，任何一个变了它就变。

**派生状态**：每一项记下算出它时所依赖输入的代数。

| 派生状态 | 依赖的输入 | 代数不一致时 |
|---|---|---|
| 基础映射 | 文件列表、语言包 | 重建 |
| 工作区比较：`HeadDiff` 和「要重新读的路径」 | HEAD、index、文件列表、忽略规则与配置、语言包（决定哪些路径受支持）、工作区内容里的比较请求 | 再做一次 `changed_paths` |
| 覆盖部分 | 工作区比较、文件列表、语言包 | 与比较结果对齐，见下 |
| `indexed` 提示 | 每条按（路径、有效 OID、淘汰代数、语言包代数） | 不匹配即视为未入索引 |
| 第 3–5 层的游标、第 3 层的范围 | 映射代数、子项目边界、淘汰、语言包、焦点 | 重建游标 |
| 候选分层、依赖子项目的范围、限时搜索的范围 | 子项目边界、映射代数 | 每次查询按自己的快照计算，不缓存 |
| 名字表 | 映射代数、淘汰、语言包、写入 | 按「名字表」的规则重建 |
| 预取结果 | 映射代数、淘汰、语言包 | 丢弃，重新查询 |
| `head_clean` | `HeadDiff` 记下的 HEAD、index、文件列表代数 | 任何一个不一致时，结果按「与 HEAD 不同」处理，直到重新比较完成 |

**执行**：事件只让输入代数加一，并请求一次核对。每个项目一个核对任务（`Background`，`BusyGuard`，同时最多一个，运行期间又有代数变化的，结束后再运行一次）。它先读一遍当前代数，按上表的依赖顺序（基础映射 → 工作区比较 → 覆盖部分 → 游标 → 名字表）重算不一致的项。每项结果在装上之前再核对一次它依赖的代数，期间又变了的结果丢弃，留给下一轮，所以按旧输入算出的状态不会被装上。

几条由这个规则得出、实现时容易漏掉的情况：

- 只有子项目变了（`ChangedParts::packages`，文件列表不变）：子项目边界代数加一，第 3 层的范围和游标、候选的「同一子项目」和「依赖的子项目」层、限时搜索的范围都按新的子项目重算。
- 提交只改变 HEAD：index 和文件列表都不变时，HEAD 代数仍然加一，`HeadDiff` 重新比较，刚提交的文件变为与 HEAD 相同。
- 忽略规则或配置变了：下一次工作区比较之后，覆盖部分与比较结果对齐。重新读的路径是比较给出的路径加上现有的全部覆盖项；现有的未跟踪覆盖项（不在文件列表里）没有出现在比较结果里时，说明它现在被忽略或已经不在，从覆盖部分删除；`untracked_capped` 为 true 时比较结果不完整，这一步不删除。
- 有效 OID 变了：一个路径的有效 OID 是它的覆盖项的 OID，没有覆盖项时是文件列表的 OID。`indexed` 提示每条存（有效 OID 的前 8 字节、淘汰代数、语言包代数），读的时候与当前的有效 OID 和代数比较，所以 OID 一变提示就不再匹配，不需要逐个清除；新 OID 的存在检查命中或写入成功之后才重新置位。
- 淘汰：淘汰代数加一后全部提示不再匹配，第 3–5 层的游标重建、按层重新做存在检查。仍在的单元只重新置位、不重新抽取，被淘汰的重新抽取。查询现场用到一个文件的抽取结果时（第 2 步的 import 目标、引用的候选文件、限时搜索的命中文件）先做存在检查，没有就现场抽取，并按所在层排队写入。

### 名字表

- 内容：当前 worktree 映射里所有 OID 的定义名，去重（区分大小写），NFC 统一，按字节序排序；每个名字带它出现过的 `DefKind` 位集，⌘K 用它显示图标或过滤。
- 构建：后台任务里先取出 `ci_ext` 中键的语言、grammar 版本、查询版本与当前一致的全部 `(unit, oid)`，按映射以（OID、语言）过滤成单元集合，再按 `ci_def_unit` 顺序扫描 `ci_def`，合并去重。正在入索引的 OID 不在其中，下一代名字表再补上。
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
| 第 3–5 层：存在检查、抽取、写入；第 5 层读其他 worktree 的 index | `BackgroundGuard` | `Batch`，每个批量线程最多一个任务 |
| 截止时间的看门狗 | 不取守卫；没有在途抽取时阻塞等待，停用或 executor 关闭时退出 | 专用线程 `code-intel-watchdog`，每次激活最多一个 |

第 3–5 层满足 ADR 0029 的四条条件：跳转和 ⌘K 按「跳转到定义」的顺序现场回答，不等它们；结果按 OID 存进 `cache`，暂停、取消或退出不丢已写入的批次；每批之间检查暂停和取消；不是用户正在等的操作。

### 建索引的分层

| 层 | 内容 | 来源 |
|---|---|---|
| 1 | 当前文本、抽屉里正在看的候选文件 | `Focus::Current`、`Focus::Drawer` |
| 2 | 本次审阅改到的文件、agent 本会话碰过的文件，以及这些文件 import 的文件（解析扩展点给出；#17 的空实现不给出） | `Focus::Review`、`Focus::Touched` |
| 3 | 当前子项目：包含当前文本（没有时取审阅集合的第一个文件）的最深子项目；没有子项目时是整个 worktree | `ActiveTree::packages` |
| 4 | 当前 worktree 的其余文件 | 映射 |
| 5 | 其他 worktree 里 OID 不在缓存中的文件 | `GitAccess::tracked` |

- 第 3、4 层用游标遍历文件列表的区间（`range_under`），不把 10 万个条目放进队列；`indexed` 提示与当前有效 OID 和代数匹配的、语言不受支持的跳过。第 1、2 层和 `WorktreeBatch` 带来的路径是显式的小队列。
- 同一 OID 出现在多个路径上时只抽取一次：在途的键记在一个集合里，第二个路径等第一个的结果。
- 只有最近一次收到 `set_focus` 的项目跑第 3–5 层，其他打开的项目只做第 1、2 层和查询。
- 第 5 层逐个处理非当前、非 prunable 的 worktree：读它的 index，按批做存在检查，缺的经那个 worktree 根的 `SafeRoot` 读出；原始字节的 OID 与 index 的 OID 相同才抽取（有改动的文件等它成为当前 worktree 时再处理）。处理完一个 worktree 就丢掉它的 `TrackedPaths`。
- 电池供电（ADR 0029 的默认值，见「未决问题」3）：第 1–3 层照常；第 4、5 层暂缓，`coverage` 报告 `Deferred`。接上电源后继续；电池供电期间用户发起 `query` 或 `names` 时，这个项目的第 4、5 层也继续，直到项目关闭或电源状态再次变化。

### 调度、批次与取消

- 第 3–5 层：每个批量线程一个长期任务，循环：取一批 → 存在检查 → 抽取缺的 → 一个写事务写入 → 在批次边界询问 `BackgroundGuard`（暂停时登记 waker、返回 `Pending`）。一批在 64 个文件、读入 2 MiB，或开始 50 ms 之后的第一个文件边界处结束，先到者为准；所以一个大文件可以一直运行到它自己的截止时间。暂停和取消仍在约 50 ms 内生效，靠的是看门狗打断在途的那个文件（见下文），与 ADR 0029「每批约 64 个文件或 50 ms」「取消和暂停在约 50 ms 内生效」的要求一致：单个文件由截止时间约束，50 ms 的响应由看门狗保证。
- 每批开始时重新选层：第 3 层有剩余就先取第 3 层。第 1、2 层在另外的工作线程上运行，不排在批量任务后面（ADR 0029）。
- 存在检查：一条 `SELECT oid FROM ci_ext WHERE lang = ? AND grammar = ? AND query = ? AND oid IN (…)`（最多 500 个参数；登记的语句是静态文本，`IN` 里写满 500 个占位符，不够时其余绑定 NULL），命中的按当前有效 OID 和代数写入 `indexed` 提示并标记访问，缺的抽取。
- 单文件截止时间：后台 250 ms，交互 1 s。原来按线性吞吐估算 M 系列上 1 MiB 的文件约 230 ms；实测吞吐随文件变大而下降（tree-sitter-tags 的局部检查按「引用数 × 作用域数」增长，见 [ADR 0031](../adr/0031.md)），接近 1 MiB 的文件在 M 系列上估计要 360–400 ms，后台记为 `TimedOut`，打开时按交互截止时间重做（已知限制，`code_intel/extract_1mib` 只记录）。tree-sitter-tags 只接受一个取消标志，所以截止时间由看门狗线程实现：每个在途抽取登记（标志、截止时刻），看门狗每 10 ms 检查一次，到时置位；没有在途抽取时阻塞等待，不唤醒。生命周期见下一条。查询的按需解析由本 crate 自己调用 `Parser::parse_with_options`，进度回调直接比较时刻，不经看门狗。
- 看门狗线程另外持有一张到期表（名字表的延迟重建，见「名字表」）：`wait_timeout` 取在途抽取的下一个检查时刻与最早到期时刻中较早的一个，到期时提交对应的任务；两张表都空时才不带超时地等待。
- 看门狗的生命周期：每次激活最多一个看门狗线程，在这次激活第一次登记在途抽取或到期时刻时经 `PluginHandle::own_with` 和 `run_dedicated_gated` 启动。线程与登记方共用一份状态：互斥锁保护的在途表和 `stop` 标志，加一个 `Condvar`。线程循环：持锁检查 `stop`，为 true 就返回；在途表为空时 `wait`，否则 `wait_timeout(10 ms)` 后检查截止时刻。`stop()` 在锁内把标志置为 true、把在途表里的标志全部置位（正在运行的抽取随之返回 `Cancelled`），然后 `notify_all`，所以无论线程在等待还是在检查，都在一次唤醒之内返回。调用 `stop()` 的地方：
  - 停用：启动时同时把一个 `WatchdogStop` 守卫交给 `PluginHandle::own`，它的 `Drop` 调用 `stop()`。kernel 在停用、失败和 `shutdown` 的撤销第 4 步 drop 守卫，所以每次停用都会让这次激活的看门狗退出，重新激活时另起一个，线程数不随激活次数增长。`Active` 的 `Drop` 也调用一次（重复调用无害）。
  - executor 关闭：executor 不等专用线程，靠闭包自己结束（[executor 设计](executor.md)「接口」的 `shutdown`），所以本 crate 在启动看门狗时用 `Executor::on_shutdown` 登记一次 `stop()`（见「对其他 crate 的要求」），线程退出时注销。`app` 的退出顺序是 kernel 的 `shutdown` 完成后再关闭 executor，这条路径只在 kernel 没有先停用插件时（测试、异常退出）才起作用。
  - 闸门：激活在线程开始运行之前就失效时，`run_dedicated_gated` 不运行闭包，不会启动线程。
- 取消在 50 ms 内生效：项目关闭、切换 worktree 时置位取消标志，批量任务在下一个批次边界（或 tags 的取消检查）退出；正在解析的文件放弃结果，不写入。切换 worktree 或分支后第 3–5 层的游标整体重建，已写入的结果按 OID 留在缓存里。
- 暂停（kernel 的暂停开关）：在约 50 ms 内生效，正在解析的文件放弃这次结果（ADR 0029）。批量任务在文件之间、以及每个在途抽取在登记时（在调用方的线程上同步检查一次截止时间、取消标志和暂停探针，已经过期或已经请求暂停时不开始工作；小文件可能在看门狗第一次检查之前就抽取完）和看门狗的每次检查（10 ms）时读 `BackgroundGuard::pause_requested()`；`ParsedFile` 在解析之前同样先检查一次截止时间和取消标志；为 true 时看门狗置位这次抽取的取消标志，tags 在下一次取消检查时返回 `Cancelled`。这个文件的结果不写入，`indexed` 提示不变，放回这一层游标之前的「恢复后先做」小队列；这一批已经抽取完的文件照常在一个写事务里写入，然后在批次边界停下。恢复后先做小队列里的文件，再从游标处继续。
- 大小上限：后台 1 MiB（`SafeRoot::read` 的 `max`），超过记为 `TooLarge`；文件被打开（第 1 层）时上限 4 MiB，更大的不抽取，查询返回 `TooLarge`。

### 打开项目与热启动

1. 收到 `ProjectEvent::Opened` 或 `Changed` 后，按快照让对应的输入代数加一（见「增量更新」）。当前 worktree 的文件列表为 `Ready` 时打开工作区根的 `SafeRoot`、订阅 `fs_watch`（第一次时）；基础映射、工作区比较、覆盖部分由核对任务按「输入代数与派生状态」算出。文件夹项目没有订阅和工作区比较。
2. 第 1、2 层的路径随焦点到达；第 3–5 层的游标就位，批量任务开始。
3. 热启动只抽取缺的键：映射里每个 OID 先做存在检查，缓存里已有的不读文件、不解析。10 万文件全部命中时约 200 次存在检查，估算几百毫秒，在批量线程上进行（`code_intel/warm_100k`）。
4. 文件夹项目没有 index 的 OID，热启动要读每个文件求 OID，命中的不解析；上限是文件夹遍历的 20 000 个条目。
5. 映射、覆盖部分、`indexed` 提示、名字表都不持久化，只有 `cache` 里的表跨启动保留（调研第 5 节「持久化与重新打开」）。

工作区比较由本 crate 自己做一次：`review` 的比较结果不在 `services` 里，它的基准也可能是会话开始时的提交。这一次比较按 stat 跳过未改的文件、不算 diff、不保留内容，10 万文件估算一两百毫秒，期间的 `TrackedPaths` 随调用释放（见「性能」的临时内存）。

### 增量更新

事件只让输入的代数加一并请求一次核对，重算都由核对任务按「输入代数与派生状态」完成。`WorktreeBatch` 在 fs_watch 线程上交付，`deliver` 只把路径并进项目的重新读集合、让代数加一；`ProjectEvent` 在 executor 线程上到达，处理相同。

| 事件 | 加一的输入代数 |
|---|---|
| `ProjectEvent::Opened`、`Changed` 的 `files` 或 `active` | 文件列表（快照里的 `Arc` 与记下的不同时）；`active` 时同时清空这个项目的全部输入记录，按新的 worktree 重新开始 |
| `ProjectEvent::Changed` 的 `packages` | 子项目边界 |
| `ProjectEvent::Changed` 的 `worktrees`；`WorktreeBatch` 的 `meta.head`、`meta.refs` | HEAD（核对时读 `GitAccess::head`，与记下的不同才加一） |
| `WorktreeBatch` 的 `meta.index` | index（读校验和，不同才加一） |
| `paths` 里的文件路径 | 工作区内容：这些路径进入重新读集合 |
| `paths` 里的目录路径 | 工作区内容：映射里这个目录下的路径进入重新读集合，并请求一次工作区比较（找出移进来的未跟踪文件） |
| `rescan`、`health` 为 `Degraded` | 工作区内容（请求比较、全部覆盖项重新读）、忽略规则与配置 |
| `meta.rules`、`meta.config` | 忽略规则与配置 |
| `health` 为 `Failed` | 不改代数；丢掉这份订阅，等下一次 `ProjectEvent` 重新订阅 |
| 语言包的通知 | 语言包（见「语言包的变化」） |
| 读到 `cache` 的淘汰代数变化 | 淘汰 |
| 一批单元写入成功 | 写入 |

- 重新读的路径有了新 OID、而新 OID 不在缓存里时，按路径所在的层排队：在焦点里的按第 1、2 层，其余按第 3、4 层。旧 OID 的行不删除，留给淘汰（其他 worktree 可能还在用）。
- agent 连续写文件时 fs_watch 已按 `Coalesce::WORKTREE` 合并批次，本 crate 不再另加等待。

### 语言包的变化

语言包可以在项目打开期间安装、更新、回滚或被撤销（ADR 0018）。插件在 `started` 里调用 `GrammarSource::subscribe`，把返回值交给 `PluginHandle::own`。通知只入队；处理在一个 `Background` 任务里进行（`BusyGuard`，同时最多一个），运行期间到达的通知并进下一轮。每一轮重新读 `GrammarSource` 的当前状态，与上一轮记下的「语言 → grammar 版本」比较，按比较结果处理，所以漏掉或合并的通知不影响结果；`generation` 不大于已处理代数的通知直接丢弃。

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

1. **当前文本**：按 `TextRef` 读出内容。工作区一侧经 `SafeRoot` 读，只有能证明读到的字节就是 `TextRef::oid` 那份内容时才回答：原始字节的 OID 等于它，或 `GitAccess::canonical_oid` 把这些字节转成规范内容后的 OID 等于它；两者都不等（或有外部过滤器）时返回 `Stale`，不因为映射里还记着这个 OID 就接受。对象库一侧经 `GitAccess::blob` 读，按 OID 取出的内容不需要核对。然后解析，求出位置上的标识符和 `RefSyntax`。按「绑定规则」在引用处确定可见的局部定义是 `Bound(Local)`；不确定可见的局部定义（`Site::possible`）和本文件里其余同名定义是第 1 层的 `NameOnly` 候选，前者在前。光标在定义的名字上时，`symbol.defines` 是这个定义（`local` 按上面 `DefinitionRef::local` 的规则：locals 在非根作用域里给出时为 true），它本身作为唯一的 `Bound(Local)` 候选。
2. **import**：解析扩展点给出这个名字的 import 目标，按路径取目标文件的抽取结果（缓存命中或现场抽取），其中导出的同名定义是 `Bound(Import)`；扩展点的 `bind` 给出的结果按它的 `Binding`。#17 的空实现在这一步没有结果。
3. **索引**：`ci_def` 按名字取出全部定义，经 `ci_ext` 得到 OID 和语言，只取语言、grammar 版本、查询版本与当前语言表一致的单元（见「存储表」的版本过滤），按「候选的分层与过滤」过滤和分层，只留映射里有的 OID。
4. **限时搜索**：第 3 步之后仍没有 `Bound` 候选、且当前子项目里还有未入索引的文件时进行，见「限时搜索」。

第 1–3 步的结果作为第一批交出（目标：索引命中时 ≤ 50 ms），限时搜索的结果逐批追加。结果只追加：后到的候选排在已有候选之后，已有条目不重排、下标不变（调研第 5 节「索引不完整时」的呈现规则）。

去重：每次查询持有一个已交出集合，键是（来源、路径、OID、行、列），来源即 `Location::origin`。每一步的候选在加入结果之前先查这个集合，已经由前面的步骤交出的跳过，保留先交出的那一条（它的层更靠前）。例如当前文本的 blob 已经入索引时，第 3 步会再次查到第 1 步列出的本文件定义；第 2 步现场抽取过的文件，在第 3 步和限时搜索里也会再次出现。同一份内容出现在两个路径上时是两个候选。`semantic` 的结果与候选按位置集合比较，不进入这个集合。集合只存在于这次查询的任务里，查询关闭时释放。

`jump` 和 `preselect`：

- 有 `Bound` 候选、且恰好一个时，`jump` 是它；多个 `Bound` 候选（重载签名、同名的多个 `impl`）时不跳，`preselect` 为第一个。第 1 步有不确定的局部候选（`Site::possible`）时也不跳：这个名字可能就是那个局部声明（「绑定规则」的安全不变式）。
- 只有 `NameOnly` 候选时，即使只有一个也不跳，`preselect` 为第一个。方法调用、接收者类型不明的成员访问都属于这一类。
- Java 的候选满足「语义导航的注册」里的升级条件时，同时发出语义请求，`semantic` 为 `Pending`。

`query(Named)` 只做第 3 步（不分 `CurrentFile`、`Imported` 两层，`near` 给出的路径决定其余各层），全部是 `NameOnly`，不跳转。

### 候选的分层与过滤

候选按 `CandidateTier` 分层（调研第 6 节）：当前文件 → import 指向的文件 → 同包（扩展点的 `package_of`，默认同一目录）→ 同一子项目 → 当前子项目依赖的子项目（扩展点的 `dependencies`，#17 为空）→ 其余。同层内依次按：在 `Focus::Review` 里的优先、路径距离（去掉公共前缀后两边剩余的段数之和）由近到远、路径字节序、行号。

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

1. 候选文件：被查定义所在的文件（按 `of.location.origin` 读：`Object` 时读对象库里的那个 blob，这份内容里的引用的 `origin` 也是 `Object`），加上 `ci_ref` 里有这个名字的单元经映射得到的路径（语言兼容组过滤）。顺序：在 `Focus::Review` 里的、同一子项目、依赖的子项目、其余，同层按路径距离。被查定义是局部定义（`DefinitionRef::local`）时，候选文件只有它所在的文件，只扫描它的作用域，不查 `ci_ref`、不做限时搜索，结果没有 `same_name`（locals 能完整解析同一作用域里的引用）。
2. 逐个候选文件读出内容、解析，用 `ParsedFile` 的引用位置（tags 引用与 `@local.reference` 的并集）找出这个名字的每一处引用，逐处解析绑定：
   - locals 解析到被查定义（同一文件、同一位置）：调用方。
   - locals 解析到别的局部定义：丢弃，这处引用与被查定义无关。
   - 扩展点的 `bind` 给出被查定义：调用方；给出别的定义：丢弃。
   - 其余：同名引用（`same_name`），不计入调用方，也不用于判断 agent 是否漏改了调用方（调研第 5 节）。
3. 当前子项目里未入索引的文件走限时搜索，命中的文件按第 2 步处理。
4. 结果逐批追加：第一批在处理完前几个候选文件或 150 ms 时交出（目标 ≤ 200 ms），之后每 100 ms 或每 32 个文件一批；10 万文件的仓库全部结果目标 ≤ 2 s。
5. 去重：与「跳转到定义」相同，每次查询一个已交出集合，键是（来源、路径、OID、行、列），`callers` 和 `same_name` 共用，一处引用只进其中一组、只出现一次。另记已处理的（路径、OID）：被查定义所在的文件、第 1 步的候选文件、限时搜索命中的文件有重叠时，每个文件只解析一次。

#17 的空扩展点下，跨文件的引用只有 locals 能证明的那部分是调用方，其余都在同名引用里；#18、#19 的 `bind` 让 Java、TS 的调用方变准。Java 的 `References` 按升级条件同时请求语义导航。

### 限时搜索

- 范围：当前子项目（按查询快照里的子项目边界）里语言在兼容组内、`indexed` 提示与当前有效 OID 和代数不匹配（见「输入代数与派生状态」）、也不在在途抽取里的文件，按与当前文件的路径距离排序，同一目录的在前。在途抽取里的文件不重复抽取，但它们对这次查询不可见（索引里还没有，搜索也不扫描），所以计入 `Partial` 的 `unsearched`：有这样的文件时结果不会是 `Complete`。
- 每个文件经 `SafeRoot` 读出（上限 1 MiB），用 `memchr::memmem` 找这个名字的字节串，命中处前后不是标识符字符（ASCII 字母数字、`_`、`$` 和 UTF-8 的多字节序列）才算整词命中。没有命中的文件不解析。
- 命中的文件先做一次存在检查，缓存里有就直接用；没有就按后台截止时间抽取，结果用于这次查询，并交给一个 `Background` 任务写入缓存，写入成功后写入 `indexed` 提示。
- 从查询开始计 1 s 后停止：文件之间检查时刻，抽取的截止时间取「250 ms」与「剩余时间」中较小的一个。停止后交出最后一批，`state` 为 `Done(Partial { … })`；全部搜完为 `Done(Complete)`。
- 结果每 50 ms 或每个命中文件交出一批。
- 搜索只读、不建索引的文件不改变 `indexed` 提示，第 3 层的批量任务之后照常处理它们。

## 性能

影响的预算（[性能预算](tech-research.md#性能预算)，M 系列 Mac，系统文件缓存已热）：

| 预算 | 本 crate 的份额 | 基准 |
|---|---|---|
| 冷启动 20 ms | 首帧之前不做任何事：`AfterFirstFrame` 激活，`activate` 只建空表、不加载 grammar | `plugin/code-intel/activate`（kernel 的 < 1 ms） |
| 跳转：同文件或索引命中，候选 ≤ 50 ms | 当前文本（100 KB 以内）解析与 locals ≤ 10 ms；名字查找 ≤ 2 ms；过滤、分层与排序 ≤ 3 ms；合计 ≤ 20 ms，余下的给抽屉 | `code_intel/goto_indexed`（cloudprint 形状、10 万文件） |
| 跳转：唯一候选的目标首屏高亮 ≤ 100 ms | 本 crate 的部分同上；目标文件的读取与高亮属于 `file_reader` | `app/goto_single` |
| 跳转：未覆盖，第一批 ≤ 300 ms，到停止 ≤ 1.1 s | 第一批是第 1–3 步的结果；限时搜索 1 s 后停止，取消在 20 ms 内生效 | `code_intel/search_stop` ≤ 1.02 s |
| 引用：第一批 ≤ 200 ms，10 万文件全部 ≤ 2 s | `ci_ref` 查找 ≤ 5 ms；每个候选文件的现场抽取约 1–2 ms / 10 KB | `code_intel/references_100k` |
| 索引覆盖：审阅集合 ≤ 2 s，子项目 ≤ 10 s，10 万文件 ≤ 2 分钟（接电源） | 单线程抽取吞吐 ≥ 4 MB/s，按仓库形状的语料测量（每种首发语言约 1 MiB，切成约 8 KiB 的文件），核心语言用一次遍历抽取（[ADR 0031](../adr/0031.md)）；热启动 10 万文件的存在检查 ≤ 500 ms | `code_intel/extract_throughput`、`code_intel/extract_1mib`（只记录）、`code_intel/warm_100k` |
| 空闲内存：monorepo < 100 MB（后台暂停时和全部完成后） | 本 crate 常驻 cloudprint 形状 ≤ 4 MB、10 万文件 ≤ 10 MB：映射、覆盖部分、`indexed` 提示、已编译的 grammar 配置（按用到的语言加载）、项目表；不含名字表和 `file_reader` 的 8 MB 高亮与文本 LRU，与后者合计在 ADR 0030 的 ≤ 10 MB / ≤ 20 MB 目标之内 | `plugin/code-intel/resident_cloudprint`、`plugin/code-intel/resident_100k` |
| 名字表 | 10 万文件 ≤ 8 MB，构建 ≤ 1 s；cloudprint 形状 ≤ 0.1 MB、≤ 30 ms | `code_intel/names_100k`、`code_intel/names_100k_bytes` |
| 映射 | 10 万文件重建 ≤ 20 ms、≤ 3 MB | `code_intel/map_100k`、`code_intel/map_100k_bytes` |
| 建索引期间的临时内存（不设顶层预算，ADR 0029） | ≤ 40 MB：每个批量任务读入的内容 ≤ 2 MiB，加一棵 1 MiB 以内文件的语法树（估算 ≤ 10 MiB）；工作区比较期间的 `TrackedPaths`（10 万文件约 10 MB） | `code_intel/batch_peak` |
| 磁盘 | `code_intel.tags` 配额 256 MiB；cloudprint 形状约 1 MB，10 万文件约 200–250 MB | `code_intel/disk_100k`（只记录） |
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
| 当前文件超过 4 MiB | 不抽取 | `Failed(TooLarge)` |
| 某个文件解析超时 | 记 `TimedOut`，不自动重试；被打开时按交互截止时间重做一次 | 这个文件不提供候选，其余照常 |
| 二进制、超过 1 MiB、读不出、路径中间是符号链接 | 记状态或标为 `Unindexable` | 不提供候选 |
| `cache` 分库不可用、写入被丢弃 | 结果只在内存里用于本次查询，下次重新抽取 | 照常，只是变慢 |
| grammar 或查询编译失败 | 这种语言在本次运行中停用，向 stderr 写一行（`lectern-code-intel:` 前缀） | `Failed(UnsupportedLanguage)` |
| 工作区比较失败（`GitError`） | 保留上一份覆盖部分，下一次事件再试 | 未暂存的修改可能暂时按 index 的内容回答 |
| `fs_watch` 的监听失败 | 丢掉订阅，等下一次 `ProjectEvent` | 同上 |
| 语义导航的提供者失败或停用 | 作废它未回复的请求，按剩下的回复重新合并；都没有时 `semantic` 为 `Failed`，候选不变 | 抽屉保留已有结果 |
| 插件停用 | kernel 丢弃任务；视图手里的 `QueryResults` 仍有效，只是不再更新 | — |

## 测试

全部在 Linux 上运行（`just test -p lectern-code-intel`），不依赖 GPUI，不调用 git 命令行、不访问网络。用到的替身：`testing::FakeGit`（工作区比较、其他 worktree 的 index、blob，按内存里的状态回答并记录调用）、`testing::FakeProjects`（内存里的 `dyn Projects`，可以发 `ProjectEvent`）、fs_watch 的 `FakeBackend` 与手动监听器、`Executor::manual()`、`testing::FakePower`、`testing::ManualClock`（截止时间和限时搜索用，看门狗读同一个时钟）、`cache` 的临时目录。

- 语言（`lang_*`）：扩展名到语言、兼容组；语言包认领扩展名，与核心语言冲突时核心语言优先。
- 抽取（`extract_*`）：每种语言的夹具用 insta 快照记录定义（名字、种类、位置、作用域、可见性、参数个数）和引用名；TS 叠加 JS 的查询；局部变量的引用不在引用名里；二进制和超过上限的内容；`EXTRACTOR_VERSION` 或查询文本变化时查询版本变化。
- 截止时间（`deadline_*`）：病态输入在 1 ms 截止时间下返回 `TimedOut`；取消标志置位时返回 `Cancelled`；看门狗在截止时刻置位，没有在途抽取时不唤醒。看门狗的生命周期（`watchdog_*`）：插件停用后线程退出（`run_dedicated_gated` 返回的任务句柄 `is_finished`，`/proc/self/task/*/comm` 里没有 `code-intel-watchdog`）；连续激活、停用 20 次后，这个名字的线程最多一个；`Executor::shutdown` 在激活仍然存活时也让线程退出；停用时正在运行的抽取返回 `Cancelled`；激活在线程启动前失效时不建线程。
- 位置分析（`site_*`）：位置上的标识符与 `RefSyntax`；locals 解析到的定义；光标在定义名上时的 `defines`。
- 定义形式（`forms_*`）与作用域形式（`scopes_*`）：两张表对照 grammar 的覆盖、样例、locals 查询和本文的表（见「定义形式」「作用域形式」）；局部定义的可见性对照 locals 查询的作用域（`forms_local_definitions_are_file_visible`）。
- 存储（`store_*`）：单元的写入与读回；存在检查只返回键完全相同的单元并标记访问；按名字查找区分大小写；单元被淘汰后三张表里没有它的行；配额小于数据量时按访问时间淘汰本命名空间的单元；同一个 OID 有旧 grammar 版本或旧查询版本的单元时，按名字查定义、查引用、存在检查和名字表都只返回当前版本的单元（`store_version_filter_*`）；写入被丢弃时不 panic、内存里的结果仍可用。
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
- 电池（`battery_*`）：电池供电时第 4、5 层不运行、`coverage` 为 `Deferred`；接上电源后继续；电池供电时一次 `query` 让第 4、5 层继续。
- 增量（`incremental_*`）：用 `FakeBackend` 驱动 `WorktreeBatch`，检查「增量更新」表里每个事件让对应的输入代数加一：修改一个文件后按新 OID 抽取、旧 OID 不再返回；目录事件请求一次比较；`rescan`；`meta.refs` 让 HEAD 代数加一；连续事件并成一轮；从后端交出事件到覆盖部分更新完之前 kernel 不静止。
- 热启动（`warm_*`）：缓存里已有全部单元时重新打开项目，抽取函数一次也没被调用（计数钩子），只有存在检查；删掉一部分单元后只抽取这一部分。
- 跳转（`goto_*`）：四步的顺序与停下的条件；唯一 `Bound` 候选给出 `jump`；唯一 `NameOnly` 候选只有 `preselect`；兼容组、可见性、语法种类的过滤（含 Go 的 `Meters(value)` 跳到 `type Meters`）；同一份字节在 `.java` 和 `.js` 下时 `java` 的单元只给出 `.java` 的路径（`goto_oid_language_match`）；分层与同层排序；审阅集合优先；结果只追加、已有下标不变；当前文本的 blob 已入索引时，第 1 步列出的本文件定义不在第 3 步重复出现，现场抽取过的 import 目标不在第 3 步和限时搜索里重复出现（`goto_no_duplicates_*`）；用测试里的 `Resolver` 实现检查第 2 步和 `Bound(Import)`；`NoSymbol`、`UnsupportedLanguage`；从对象库一侧（`TextOrigin::Object`）发起的查询，本文件的候选 `origin` 为 `Object`，由它发起的 `References` 读同一个 blob，不读工作区（`goto_object_origin_*`）。
- 预取与焦点（`prefetch_*`、`focus_*`）：映射换代或淘汰代数变化之后，2 秒内同样的查询不沿用预取结果；非当前 worktree 的焦点不进入第 1、2 层，切换到那个 worktree 后生效。
- 限时搜索（`search_*`）：只解析整词命中的文件；`ManualClock` 推进到 1 s 时停止并给出 `Partial` 的计数；命中文件的结果写入缓存；搜完时为 `Complete`；范围里有一个文件正在后台抽取时，搜完其余文件结果仍是 `Partial`、`unsearched` 至少为 1（`search_in_flight_partial`）。
- 引用（`refs_*`）：局部变量和参数的 `References` 找到它在作用域里的全部用处（这些位置只在 `@local.reference` 里，不在 tags 引用和 `ci_ref` 里），不查 `ci_ref`、没有 `same_name`；locals 记为程序作用域局部定义的顶层函数，它的同文件引用也被找到；同时被 tags 和 locals 捕获的导出顶层函数（根作用域）`local` 为 false，另一个文件里对它的引用经 `ci_ref` 找到、未入索引的文件经限时搜索找到（`refs_toplevel_captured_by_locals`）；函数体里嵌套的 `const helper = () => …` 同时被两者捕获，`local` 为 true，引用只在外层函数的作用域里找，其他文件里同名的 `helper` 不出现（`refs_nested_helper_is_local`）；locals 证明的同文件引用是调用方；同一处引用、同一个文件在多个来源里出现时只列一次、只解析一次（`refs_no_duplicates_*`）；被局部变量遮住的同名引用被丢弃；没有依据的跨文件引用在 `same_name`；测试 `Resolver` 的 `bind` 让跨文件引用成为调用方；未入索引的部分经限时搜索找到。
- 名字表（`names_*`）：去重、NFC、只含当前 worktree 映射里的 OID（另一个 worktree 独有的名字不在其中）；第一次 `names` 返回 `Building`，`Names` 事件带着表，监听方保存后再调 `names` 返回 `Ready`、是同一个 `Arc`（`names_ready_after_build`）；构建期间重复调用不另起构建；代数落后时重建、期间返回旧表；所有持有者释放后再调用重新构建；表被持有时索引写入新的批次，`ManualClock` 推进过间隔后后台重建并发出带新 `Arc` 的 `Names`，间隔内的多次变化只触发一次重建，持有者释放后不再重建（`names_refresh_while_held`）。
- 升级（`escalate_*`）：Java 多候选和唯一 `NameOnly` 候选时调用提供者，其他语言不调用；提供者的 token 为 `Dead` 时不调用；结果相同时 `Same`、不同时 `Ready`；`head_only` 的提供者只在 `head_clean` 时被调用，返回的位置里内容与 HEAD 不同的文件被丢掉；只暂存、未提交的文件（工作区等于 index、不等于 HEAD）的 `head_clean` 为 false，提交之后为 true；index 变化之后、新的工作区比较完成之前为 false（`head_dirty_*`）；登记 drop 后不再调用；SCIP 与 jdtls 两个假提供者同时处理 Java 时，按两种回复顺序得到相同的合并结果，HEAD 相同的文件取 SCIP 的位置、有改动的文件取 jdtls 的位置，过时 ID 的回复被丢弃，查询关闭时未回复的请求收到 `cancel`；SCIP 先回复、已知候选都与 HEAD 相同时，jdtls 的请求不被取消，它之后返回的未跟踪文件里的位置进入合并结果（`escalate_arbitration_*`）；提供者接受请求后注销，只有它在请求中的查询 `semantic` 变为 `Failed`、不再停在 `Pending`，另有 SCIP 回复的查询按剩下的回复重新合并，注销之后到达的回复被丢弃（`escalate_unregister_*`）。
- 插件（`plugin_*`）：用真实的 `Kernel`（kernel 的 `test-support`）和 `Executor::manual()`；`activate` 之后、第一个项目打开之前没有任何 IO（`FakeGit`、缓存目录、`SafeRoot` 都没有被调用）；依赖 `dyn Projects`；`Results` 事件的顺序与 generation；一次查询从提交到最后一批写入之前 kernel 不静止；停用之后电源、语言包的订阅都已退订，fs_watch 的订阅都已 drop，项目关闭时未回复的语义请求收到 `cancel`（`plugin_teardown_*`）。

## 未决问题

需要产品决定（调研「[未决问题](../research/code-navigation.md#未决问题需要产品决定)」第 4–6 条里影响本 crate 的部分）：

1. **首发语言范围。** 选项：
   - (a) 符号抽取覆盖 ADR 0018 核心里的 TypeScript / JavaScript（含 JSX、TSX）、Rust、Java、Python、Go；按 import 解析先做 Java 和 TS（#18、#19），其余语言只按同文件、同目录、同子项目分层（默认）。
   - (b) 只做 Java 与 TS / JS，其余语言等语言包。少三个 grammar，安装包小一些。

   影响「依赖」表里的 grammar crate 和第 1 步的夹具；选 (b) 时删掉对应的行，其余设计不变。
2. **索引没建完时是否提示。** 选项：
   - (a) 不提示，符合「界面不显示索引状态」的规则（默认）。
   - (b) 只在限时搜索没有覆盖全部文件、结果可能不全时，在结果末尾显示一行「仍在搜索」。

   两种都由 `QueryState::Done(Completeness)` 支持，本 crate 不随选择改变；提示的界面属于 #20 和 #102。
3. **电池供电时的索引范围。** 选项：
   - (a) 按 ADR 0029 的默认值：接电源时索引整个仓库；用电池时只自动索引审阅集合和当前子项目，其余等接上电源或用户跳转、打开 ⌘K 时再继续（默认）。
   - (b) 不区分电源，始终索引整个仓库。
   - (c) 用电池时只索引审阅集合。

   确认结果不同时只改「建索引的分层」里的电池一条和 `battery_*` 测试（ADR 0029 同样只改这一条）。

技术问题都已在上文决定，没有其他未决项。

## 验收

前置条件（其他 crate 的 PR，都写 `Refs #17`，见「对其他 crate 的要求」）：

- A. `services`：`code_intel` 模块；`project` 模块的 `BlobId` 和 `FileList::oid`。`cargo check -p lectern-services` 通过，`rg -n 'pub trait CodeIntel' crates/services/src` 有一处，检查 `dyn CodeIntel` 的 `ID == "code-intel"`、`MULTIPLE == false` 的测试通过。
- B. `project`：文件列表带 OID，OID 变化时文件列表换代，`plugin/project/resident_*` 的份额已更新。
- C. `cache`：「自定义表」的设计与实现。
- D. `kernel` 的 `BackgroundGuard`、`executor` 的 `Priority::Batch`（ADR 0029）和 `Executor::on_shutdown`。
- E. `git`：`Worktree::canonical_oid`。

本 crate 分五步实现，每步一个 PR；前四步写 `Refs #17`，第 5 步写 `Closes #17`。每一步都要 `just check`、`just test -p lectern-code-intel`、`just deny` 通过，并把这一步加入的 bench 条目改为生效。

1. **语言注册与抽取**（不需要前置条件）：根 `Cargo.toml` 按 ADR 0030 加入 `tree-sitter`、`tree-sitter-tags` 和首发语言的 grammar crate，加入查询文件和夹具。
   - `lang_*`（含 `lang_grammar_versions_match_lock`）、`extract_*`、`deadline_*`、`site_*` 测试通过，每种首发语言至少一份 insta 快照。
   - `cargo tree -e features -i tree-sitter` 里没有 `wasm`。
   - `cargo tree -i tree-sitter-tags -e normal --depth 1` 只列出 `lectern-code-intel`。
   - `code_intel/extract_throughput` 在预算内。
2. **存储**（前置 C）：
   - `store_*` 测试通过（含读取淘汰代数）；`cargo tree -i rusqlite -e normal --depth 1` 仍只列出 `lectern-cache`。
   - `code_intel/warm_100k`（存在检查部分）在预算内。
3. **映射、调度与增量更新**（前置 B、D）：
   - `map_*`、`derived_*`、`overlay_*`、`schedule_*`、`grammar_*`、`battery_*`、`incremental_*`、`warm_*` 测试通过。
   - `rg -n 'std::fs::|File::open' crates/code_intel/src --glob '!**/tests*' --glob '!**/testing*'` 没有结果：工作区文件只经 `SafeRoot` 读取。
   - `rg -n 'Command::new|thread::spawn' crates/code_intel/src` 没有结果。
   - `code_intel/map_100k`、`code_intel/map_100k_bytes`、`code_intel/batch_peak` 在预算内。
4. **查询与插件**（前置 A、E）：`Cargo.toml` 加 `plugin = true`。
   - `goto_*`、`read_verify_*`、`search_*`、`refs_*`、`names_*`、`escalate_*`、`plugin_*`、`watchdog_*` 测试通过。
   - `python3 scripts/check_architecture.py` 通过：本 crate 不依赖 GPUI、不依赖其他插件 crate，除 `app` 和 tool 层外没有 crate 依赖它。
   - `code_intel/goto_indexed`、`code_intel/search_stop`、`code_intel/references_100k`、`code_intel/names_100k`、`code_intel/names_100k_bytes`、`plugin/code-intel/resident_cloudprint`、`plugin/code-intel/resident_100k` 在预算内。
5. **端到端基准**（前置 #20 的抽屉和 `file_reader` 在探针里的里程碑）：bench 探针增加打开抽屉、跳转、覆盖层完成的 `Milestone`，以及读取 `coverage` 的方式。
   - `app/goto_candidates`、`app/goto_single`、`app/goto_unindexed`、`app/goto_unindexed_done`、`app/index_review_set`、`app/index_subproject`、`app/index_100k`、`app/idle_monorepo_indexed` 在 `crates/bench/src/budgets.rs` 里改为生效，macOS CI 上在预算内。
   - `app/idle_monorepo` 在后台工作暂停时仍 < 100 MB。

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
