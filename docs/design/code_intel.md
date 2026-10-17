# code_intel 设计

状态：已接受，尚未实现（设计 [#107](https://github.com/SoloJiang/lectern-app/pull/107)）。

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
- `project`：从 index 建 `FileList` 时填入 OID（`TrackedEntry::oid`）。OID 按列表的哈希算法定长存放，10 万文件多 2 MB（SHA-1）或 3.2 MB（SHA-256），`project` 的常驻份额（`plugin/project/resident_100k`）在同一个 PR 里相应修改。
- `cache`：按 [cache 设计「范围」](cache.md#范围)预留的做法增加「自定义表」一节和实现，满足本 crate 的以下需要（具体签名由 cache 的设计决定）：
  1. 使用方以静态常量登记一组表（建表语句、索引、预编译语句的 SQL 文本）和它的版本；第一次使用时建表，版本变化时删掉旧表和它的全部单元后重建。
  2. 单元：一条自定义数据对应 `entry` 表里的一个条目（命名空间加键，带使用方声明的字节数），计入 `meta.bytes` 与 LRU。淘汰或删除一个单元时，在同一个事务里删除它在自定义表里的全部行（外键级联，或使用方登记的删除语句）。本 crate 的单元 ID 就是这个条目的 ID。
  3. 命名空间配额：`code_intel.tags` 在每个项目分库里最多占 256 MiB；超出时先按访问时间淘汰这个命名空间自己的单元，分库总预算（512 MiB）的淘汰规则不变，所以 hunk 缓存至少保留 256 MiB。
  4. 读：在读连接上执行登记过的语句，按整数、字节串、文本取列；公开接口里不出现 rusqlite 的类型。
  5. 访问时间：按单元 ID 标记访问（与读命中相同，精度 1 小时），不读值。
  6. 写：一个写事务里建单元、插入行、删除单元，全部生效或全部不生效；失败时返回 `Dropped`，语义与 `Batch::commit` 相同，单个事务受 `MAX_BATCH` 约束。
  7. 预编译语句缓存从每连接 16 条提高到 32 条（本 crate 约 12 条语句）。
  8. 淘汰代数：每个分库每个登记了自定义表的命名空间一个内存里的计数，这个命名空间有单元被淘汰或删除、或分库被删除重建时加一，任何线程可以不阻塞地读取（「派生状态的核对」用它）。
- `kernel`：`PluginHandle::background()` 与 `BackgroundGuard`、批次边界的暂停检查，以及不阻塞、任何线程可调用的 `BackgroundGuard::pause_requested()`（看门狗用它在批次中途打断在途抽取）（[kernel 设计「静止」](kernel.md#静止)，ADR 0029 规定由单独的 PR 先于本 crate 实现）。
- `executor`：批量线程组与 `Priority::Batch`（[executor 设计](executor.md)，同上）；另加 `Executor::on_shutdown(f: Box<dyn FnOnce() + Send>) -> ShutdownHook`：`shutdown` 开始时在调用线程上依次调用已登记的回调（在 `contain` 里），`ShutdownHook` drop 时注销。专用线程上的闭包靠它得知 executor 正在关闭（本 crate 的看门狗，见「调度、批次与取消」）。
- `platform`：电源状态与变化通知（ADR 0029「影响」）。`app` 把它包成本 crate 的 `PowerSource` 传入；本 crate 不依赖 `platform` 的电源接口，测试用 `testing::FakePower`。
- `fs_watch`、`git`：不需要新接口。用到的接口见「依赖 git、fs_watch 与 cache 的接口」。
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
    /// 只由 locals 查询的 `@local.definition` 给出（局部变量、参数、局部函数），只在所在文件的这个作用域里可见。
    /// locals 和 tags 在同一个名字位置都给出定义时（locals 查询把顶层函数记为程序作用域里的局部定义），为 false：
    /// tags 的定义按跨文件的方式查引用。为 true 的定义不在 `ci_def` 里；`References` 只扫描它所在的文件（见「查找引用与调用方」）。
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
    /// 能参与回答的文件都已入索引或在这次查询里搜过。
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
  - 取消：每个请求有自己的 `SemanticRequestId`，只采用 ID 仍是这次查询当前请求的回复。查询关闭、被新的查询取代、或某个提供者的结果按上面的规则不会被采用（比如被查文件和全部候选文件都与 HEAD 相同、SCIP 已经回复）时，对还没回复的请求调用 `cancel`。
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
    pub fn compatible(a: &LangName, b: &LangName) -> bool;
    /// 抽取一份内容。阻塞（解析），任何非 UI 线程。`deadline` 到了或 `cancel` 置位时返回 `TimedOut` / `Cancelled`。
    pub fn extract(lang: &Lang, source: &[u8], deadline: Deadline, cancel: &AtomicBool) -> Extraction;
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

- HEAD 差异：每次工作区比较的 `head` 存成项目的 `HeadDiff`（路径 → HEAD 里的 OID），连同这次比较时基础映射的代数。它与覆盖部分分开：覆盖部分只描述工作区与 index 的差别，只暂存的改动（工作区等于 index、不等于 HEAD）没有覆盖项，却在 `HeadDiff` 里。一个路径与 HEAD 相同（`head_clean`）当且仅当它在 HEAD 里的 OID 已知且等于当前工作区的 OID（覆盖项的 OID，没有覆盖项时是文件列表的 OID）：在 `HeadDiff` 里的取记下的 OID；不在里面的，只有基础映射的代数仍等于比较时的代数，HEAD 里的 OID 才等于文件列表的 OID。不知道时按「不相同」处理。`SemanticRequest::head_clean`、`head_only` 提供者的选择和它返回位置的过滤都只用这条规则。
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
  - 查询版本：tags 查询、locals 查询的文本加上本 crate 的 `EXTRACTOR_VERSION`（可见性、参数个数、作用域的计算规则变化时加一）合在一起的 64 位哈希。
- 文件夹项目没有 git：OID 是原始字节按 git blob 格式的 SHA-1（`git::Oid::for_blob`）。

### 抽取结果

```rust
pub struct Extraction {
    pub status: ExtractStatus,
    pub defs: Vec<Def>,
    /// 引用名，去重、按字节序。locals 查询解析到局部定义的引用不在其中（tree-sitter-tags 的行为），
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
    /// 取消或暂停。不写入缓存。
    Cancelled,
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
- `scope`：同一文件里，范围包含这个定义的最内层定义的名字。抽取后按范围排序一遍求出。
- `visibility` 和 `arity` 由每种语言的一个小函数读定义节点的修饰符和参数节点得出，不经过 tags 查询；规则变化时 `EXTRACTOR_VERSION` 加一。
- 按需抽取（第 1 层、限时搜索、引用的位置）另外需要语法树和引用位置，内部用 `ParsedFile`，用完即丢，不进缓存。它保留：语法树；tags 的引用位置；locals 查询的作用域、`@local.definition` 和 `@local.reference` 的位置。引用位置取 tags 引用与 `@local.reference` 的并集，按位置去重：tags 省略了被 locals 解析掉的引用（包括 locals 查询把顶层函数也记为程序作用域里的局部定义时，对它的同文件引用），只扫 tags 的位置会漏掉局部变量、参数和这类同文件引用。

### 外部格式（tags 查询）

- 格式来源：tree-sitter 0.27.1 文档「Code Navigation Systems」（`@definition.*`、`@reference.*`、`@name`、`@doc`），locals 查询沿用 tree-sitter-highlight 的约定（`@local.scope`、`@local.definition`、`@local.reference`）。
- 查询文件放在 `crates/code_intel/queries/<语言>/tags.scm` 和 `locals.scm`，以对应 grammar 仓库在锁定版本里的同名文件为起点（MIT），文件头记下上游 commit 和 Lectern 的改动；TS 的 tags 查询按上游的做法叠加 JS 的查询。用 `include_str!` 编译进二进制，第一次用到一种语言时在后台线程编译 `TagsConfiguration`，之后常驻。
- 夹具：`crates/code_intel/tests/fixtures/extract/<语言>/` 下每种语言若干真实写法的样本（类、方法、闭包、重载、嵌套模块、导出与非导出、宏），抽取结果用 insta 快照；grammar 或查询升级时快照的变化在 PR 里逐个看过。另有一份病态输入（深度嵌套、超长行）用于截止时间的测试。

### 存储表

一次抽取是 `cache` 的一个单元，命名空间 `code_intel.tags`（版本 1，不压缩），键是 `Key::new().bytes(oid).str(lang).u64(grammar).u64(query)`，单元的字节数由本 crate 按行估算（见下）。本 crate 登记的表：

```sql
-- 一次抽取一行。unit 是 cache 分配的单元 ID；单元被淘汰时 cache 在同一事务里删除下面三张表里它的全部行。
CREATE TABLE ci_ext (
  unit    INTEGER PRIMARY KEY,
  oid     BLOB    NOT NULL,           -- 20 或 32 字节
  lang    TEXT    NOT NULL,
  grammar INTEGER NOT NULL,
  query   INTEGER NOT NULL,
  status  INTEGER NOT NULL,           -- ExtractStatus：0 Done、1 TimedOut、2 TooLarge、3 Binary
  UNIQUE (oid, lang, grammar, query)
);

CREATE TABLE ci_def (
  unit     INTEGER NOT NULL,
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
  unit INTEGER NOT NULL,
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
- 写入：一批（见「调度、批次与取消」）在一个写事务里建单元、插入全部行，一批约 100 KB，远低于 `MAX_BATCH`；写失败（`Dropped`）时这一批的结果留在内存里供本次查询使用，下次需要时重新抽取。
- 淘汰：按单元的访问时间（LRU，精度 1 小时）。本 crate 在热启动的存在检查和查询命中时标记访问，所以访问时间即「最后一次出现在某个 worktree 里或被用到的时间」；不在任何 worktree 里的旧 OID 的行留给淘汰，不主动删除。当前 worktree 的单元也可能被淘汰，内存里的 `indexed` 位按「派生状态的核对」处理。
- 规模（调研第 4 节，Rust 语料的密度）：`cloudprint-miniapps` 形状约 1 MB；10 万文件约 200–250 MB，在 256 MiB 的配额以内。超过配额时最久没出现的 blob 先被淘汰，下次需要时重新抽取。
- 版本：表结构或单元的键变化时命名空间版本加一；只是抽取规则变化时改 `EXTRACTOR_VERSION`，旧单元不再命中、逐步淘汰。

### blob → 路径映射

每个打开项目的当前 worktree 一份，只在内存里，打开项目后重建，不持久化。查询只返回映射里有的 OID，所以已经改掉的定义、已删除的文件不会出现在结果里。

```rust
struct LiveMap {
    base: Arc<BaseMap>,        // 随 project 的文件列表整体替换
    overlay: Arc<Overlay>,     // 随工作区内容的变化替换，通常很小
    indexed: Arc<IndexedBits>, // 文件列表下标 → 这个 OID 的抽取在缓存里（提示，见「派生状态的核对」），带置位时的淘汰代数
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
- 查找一个 OID：在两个 `by_oid` 里按前缀二分查找，核对完整 OID；基础部分里被覆盖项遮住的路径跳过。
- 文件列表换代（index 变化、切换分支）时重建基础部分，并清理覆盖项：路径现在在文件列表里、OID 也相同的覆盖项删除。
- 内存：10 万文件的 `by_oid` 1.2 MB，`indexed` 12.5 KB；覆盖项通常几十条，未跟踪文件最多 20 000 条（`git` 的上限）时约 1.5 MB。
- 文件夹项目：`FileList` 没有 OID，基础部分为空，每个文件在第一次被读到时作为 `Content` 加入覆盖部分（上限是文件夹遍历的 20 000 个条目）。

### 派生状态的核对

内存里从缓存派生、没有随缓存一起更新的状态都只是提示：`indexed` 位、名字表、存在检查的结论。`cache` 的淘汰（超出配额或分库预算）和重建（损坏、版本不符）随时可能删掉单元，本 crate 不在那次写事务里。规则：

- `cache` 为每个分库的 `code_intel.tags` 维护一个淘汰代数：这个命名空间有单元被淘汰、删除，或分库被重建时加一（只在内存里，进程内单调递增；见「对其他 crate 的要求」）。
- `IndexedBits` 记下置位时读到的淘汰代数。读位的一方（调度游标、限时搜索、`coverage`）发现当前代数更大时，把全部位当作未知：这一代的位清零，第 3–5 层的游标重建，按层重新做存在检查。存在检查很便宜（10 万文件约 200 次查询），仍在的单元只重新置位、不重新抽取，被淘汰的按层重新抽取。
- 用到时核对：限时搜索不跳过已置位的文件，改为跳过「已置位且代数仍是当前代数」的文件；查询现场用到一个文件的抽取结果时（第 2 步的 import 目标、引用的候选文件、限时搜索的命中文件）先做存在检查，没有就清掉这个位、现场抽取，并把它按所在层重新排队写入。
- 名字表的代数包含淘汰代数，所以淘汰之后下一次 `names` 重建。

### 名字表

- 内容：当前 worktree 映射里所有 OID 的定义名，去重（区分大小写），NFC 统一，按字节序排序；每个名字带它出现过的 `DefKind` 位集，⌘K 用它显示图标或过滤。
- 构建：后台任务里先取出 `ci_ext` 中键的语言、grammar 版本、查询版本与当前一致的全部 `(unit, oid)`，按映射过滤成单元集合，再按 `ci_def_unit` 顺序扫描 `ci_def`，合并去重。正在入索引的 OID 不在其中，下一代名字表再补上。
- 代：映射、索引或 `cache` 的淘汰代数（见「派生状态的核对」）有变化时代数加一。`names` 发现仍有持有者的表代数落后时开始重建，重建期间仍返回旧表（`Ready`）。
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

- 第 3、4 层用游标遍历文件列表的区间（`range_under`），不把 10 万个条目放进队列；`indexed` 已置位的、语言不受支持的跳过。第 1、2 层和 `WorktreeBatch` 带来的路径是显式的小队列。
- 同一 OID 出现在多个路径上时只抽取一次：在途的键记在一个集合里，第二个路径等第一个的结果。
- 只有最近一次收到 `set_focus` 的项目跑第 3–5 层，其他打开的项目只做第 1、2 层和查询。
- 第 5 层逐个处理非当前、非 prunable 的 worktree：读它的 index，按批做存在检查，缺的经那个 worktree 根的 `SafeRoot` 读出；原始字节的 OID 与 index 的 OID 相同才抽取（有改动的文件等它成为当前 worktree 时再处理）。处理完一个 worktree 就丢掉它的 `TrackedPaths`。
- 电池供电（ADR 0029 的默认值，见「未决问题」3）：第 1–3 层照常；第 4、5 层暂缓，`coverage` 报告 `Deferred`。接上电源后继续；电池供电期间用户发起 `query` 或 `names` 时，这个项目的第 4、5 层也继续，直到项目关闭或电源状态再次变化。

### 调度、批次与取消

- 第 3–5 层：每个批量线程一个长期任务，循环：取一批 → 存在检查 → 抽取缺的 → 一个写事务写入 → 在批次边界询问 `BackgroundGuard`（暂停时登记 waker、返回 `Pending`）。一批最多 64 个文件、读入的内容合计最多 2 MiB、耗时最多 50 ms，先到者为准。
- 每批开始时重新选层：第 3 层有剩余就先取第 3 层。第 1、2 层在另外的工作线程上运行，不排在批量任务后面（ADR 0029）。
- 存在检查：一条 `SELECT oid FROM ci_ext WHERE lang = ? AND grammar = ? AND query = ? AND oid IN (…)`（最多 500 个参数），命中的置 `indexed` 位并标记访问，缺的抽取。
- 单文件截止时间：后台 250 ms（M 系列上 1 MiB 的文件约 230 ms，按调研的 4.3 MB/s 和「M 系列可能快一倍」估算，实测后校准），交互 1 s。tree-sitter-tags 只接受一个取消标志，所以截止时间由看门狗线程实现：每个在途抽取登记（标志、截止时刻），看门狗每 10 ms 检查一次，到时置位；没有在途抽取时阻塞等待，不唤醒。生命周期见下一条。查询的按需解析由本 crate 自己调用 `Parser::parse_with_options`，进度回调直接比较时刻，不经看门狗。
- 看门狗线程另外持有一张到期表（名字表的延迟重建，见「名字表」）：`wait_timeout` 取在途抽取的下一个检查时刻与最早到期时刻中较早的一个，到期时提交对应的任务；两张表都空时才不带超时地等待。
- 看门狗的生命周期：每次激活最多一个看门狗线程，在这次激活第一次登记在途抽取或到期时刻时经 `PluginHandle::own_with` 和 `run_dedicated_gated` 启动。线程与登记方共用一份状态：互斥锁保护的在途表和 `stop` 标志，加一个 `Condvar`。线程循环：持锁检查 `stop`，为 true 就返回；在途表为空时 `wait`，否则 `wait_timeout(10 ms)` 后检查截止时刻。`stop()` 在锁内把标志置为 true、把在途表里的标志全部置位（正在运行的抽取随之返回 `Cancelled`），然后 `notify_all`，所以无论线程在等待还是在检查，都在一次唤醒之内返回。调用 `stop()` 的地方：
  - 停用：启动时同时把一个 `WatchdogStop` 守卫交给 `PluginHandle::own`，它的 `Drop` 调用 `stop()`。kernel 在停用、失败和 `shutdown` 的撤销第 4 步 drop 守卫，所以每次停用都会让这次激活的看门狗退出，重新激活时另起一个，线程数不随激活次数增长。`Active` 的 `Drop` 也调用一次（重复调用无害）。
  - executor 关闭：executor 不等专用线程，靠闭包自己结束（[executor 设计](executor.md)「接口」的 `shutdown`），所以本 crate 在启动看门狗时用 `Executor::on_shutdown` 登记一次 `stop()`（见「对其他 crate 的要求」），线程退出时注销。`app` 的退出顺序是 kernel 的 `shutdown` 完成后再关闭 executor，这条路径只在 kernel 没有先停用插件时（测试、异常退出）才起作用。
  - 闸门：激活在线程开始运行之前就失效时，`run_dedicated_gated` 不运行闭包，不会启动线程。
- 取消在 50 ms 内生效：项目关闭、切换 worktree 时置位取消标志，批量任务在下一个批次边界（或 tags 的取消检查）退出；正在解析的文件放弃结果，不写入。切换 worktree 或分支后第 3–5 层的游标整体重建，已写入的结果按 OID 留在缓存里。
- 暂停（kernel 的暂停开关）：在约 50 ms 内生效，正在解析的文件放弃这次结果（ADR 0029）。批量任务在文件之间、以及每个在途抽取在看门狗的每次检查（10 ms）时读 `BackgroundGuard::pause_requested()`；为 true 时看门狗置位这次抽取的取消标志，tags 在下一次取消检查时返回 `Cancelled`。这个文件的结果不写入，`indexed` 位不变，放回这一层游标之前的「恢复后先做」小队列；这一批已经抽取完的文件照常在一个写事务里写入，然后在批次边界停下。恢复后先做小队列里的文件，再从游标处继续。
- 大小上限：后台 1 MiB（`SafeRoot::read` 的 `max`），超过记为 `TooLarge`；文件被打开（第 1 层）时上限 4 MiB，更大的不抽取，查询返回 `TooLarge`。

### 打开项目与热启动

1. 收到 `ProjectEvent::Opened` 或 `Changed`（`active` 或 `files`）后，`Background` 任务（`BusyGuard`）读 `Projects::snapshot`。当前 worktree 的文件列表为 `Ready` 时：打开工作区根的 `SafeRoot`，建基础映射，订阅 `fs_watch`（第一次时），调用 `GitAccess::changed_paths`，把返回的路径交给「重新读」。文件夹项目跳过订阅和工作区比较。
2. 第 1、2 层的路径随焦点到达；第 3–5 层的游标就位，批量任务开始。
3. 热启动只抽取缺的键：映射里每个 OID 先做存在检查，缓存里已有的不读文件、不解析。10 万文件全部命中时约 200 次存在检查，估算几百毫秒，在批量线程上进行（`code_intel/warm_100k`）。
4. 文件夹项目没有 index 的 OID，热启动要读每个文件求 OID，命中的不解析；上限是文件夹遍历的 20 000 个条目。
5. 映射、覆盖部分、`indexed` 位、名字表都不持久化，只有 `cache` 里的表跨启动保留（调研第 5 节「持久化与重新打开」）。

工作区比较由本 crate 自己做一次：`review` 的比较结果不在 `services` 里，它的基准也可能是会话开始时的提交。这一次比较按 stat 跳过未改的文件、不算 diff、不保留内容，10 万文件估算一两百毫秒，期间的 `TrackedPaths` 随调用释放（见「性能」的临时内存）。

### 增量更新

`WorktreeBatch` 在 fs_watch 线程上交付，`deliver` 只把内容并进项目的脏集合并提交一个刷新任务（同一项目同时最多一个，运行期间到达的批次并进下一轮）：

| 批次内容 | 处理 |
|---|---|
| `paths` 里的文件路径 | 重新读这个路径 |
| `paths` 里的目录路径（目录被删除、改名，或后端只报告了目录） | 重新读映射里这个目录下的全部路径，并做一次工作区比较（找出移进来的未跟踪文件） |
| `rescan`、`health` 为 `Degraded` | 工作区比较，并重新读全部覆盖项 |
| `meta.index`、`meta.head` | 等 `project` 交来新的文件列表（`ProjectEvent::Changed` 的 `files`）后重建基础映射、清理覆盖项，再做一次工作区比较：暂存、提交、切换分支都会改变 `HeadDiff` |
| `meta.rules`、`meta.config` | 工作区比较（忽略规则或转换规则可能变了） |
| `health` 为 `Failed` | 丢掉这份订阅，等下一次 `ProjectEvent` 重新订阅 |

- 重新读的结果按读取时的基础映射代数计算（等于还是不等于文件列表的 OID）。写回覆盖部分时代数已经变了（期间文件列表换代）的，丢弃这次结果，把这些路径重新放进脏集合再读一次，所以覆盖项不会按旧的文件列表判断。
- 重新读的结果改变了覆盖项时，新 OID 不在缓存里就按路径所在的层排队：在焦点里的按第 1、2 层，其余按第 3、4 层。旧 OID 的行不删除，留给淘汰（其他 worktree 可能还在用）。
- agent 连续写文件时 fs_watch 已按 `Coalesce::WORKTREE` 合并批次，本 crate 不再另加等待。
- 工作区比较在同一项目里同时最多一次，运行期间又触发的在结束后再做一次。

### 语言包的变化

语言包可以在项目打开期间安装、更新、回滚或被撤销（ADR 0018）。插件在 `started` 里调用 `GrammarSource::subscribe`，把返回值交给 `PluginHandle::own`。通知只入队；处理在一个 `Background` 任务里进行（`BusyGuard`，同时最多一个），运行期间到达的通知并进下一轮。每一轮重新读 `GrammarSource` 的当前状态，与上一轮记下的「语言 → grammar 版本」比较，按比较结果处理，所以漏掉或合并的通知不影响结果；`generation` 不大于已处理代数的通知直接丢弃。

| 变化 | 处理 |
|---|---|
| 安装 | 在语言表里登记，第一次用到时编译查询；重建基础映射（新认领扩展名的文件进入），做一次工作区比较（这些扩展名的未跟踪文件和有改动的文件进入覆盖部分）；第 3–5 层的游标重建，这些文件按所在层排队 |
| 更新、回滚 | 换成新的 grammar 版本：键里的 grammar 版本随之变化，旧版本的单元不再命中（回滚时，之前用那个版本写过的单元重新命中）；清掉这种语言的文件的 `indexed` 位，游标重建，按层重新做存在检查和抽取 |
| 撤销、卸载 | 立即从语言表里移除，不再加载这个 grammar；从基础映射和覆盖部分去掉这种语言的文件，它们不再计入 `coverage`，查询不再返回它们的候选，这种语言的查询返回 `UnsupportedLanguage` |

- 租约：语言表里的每个条目、由它编译出的 `TagsConfiguration` 和 locals `Query`、每个 `Parser`、每个在途抽取和 `ParsedFile`（持有 `Tree`）都持有一份 `GrammarLease` 的克隆。更新、回滚、撤销时语言表先换掉或删掉条目，旧条目的租约随最后一个使用者（在途抽取结束、查询释放 `ParsedFile`）drop，加载器这时才卸载动态库，所以不会有代码调用已卸载的库。
- 在途的抽取：更新、回滚、撤销都置位这种语言在途抽取的取消标志。每个批次和每次按需抽取开始时记下所用的 grammar 版本，写入缓存之前核对仍是当前版本，不是就丢弃结果，所以旧 grammar 的结果不会以新键写入。已编译的旧配置在用它的在途抽取结束后释放。
- 旧版本的单元不主动删除，留给淘汰。
- 名字表的代数加一。已交出的 `QueryResults` 不修改（只追加的规则）；正在进行的查询从下一步起使用新的语言表。

### 跳转到定义

查询开始时取一份 `LiveMap` 快照（基础映射、覆盖部分、`HeadDiff` 的 `Arc`）和当前的语言表，全程使用：同一次查询的各批结果对应同一份映射，之后映射换代也不改写已交出的结果，`Location::oid` 让视图在打开前发现内容已变。`References` 相同。

`query(Definition)` 在一个 `Interactive` 任务里按下面的顺序回答。前一步得到有绑定依据的候选就停下；没有时继续，结果逐批追加：

1. **当前文本**：按 `TextRef` 读出内容（工作区一侧经 `SafeRoot`，读到的原始字节的 OID 等于 `TextRef::oid`、或 `TextRef::oid` 等于映射里这个路径当前的 OID 时照常回答，都不等时返回 `Stale`；对象库一侧经 `GitAccess::blob`），解析，求出位置上的标识符和 `RefSyntax`。locals 查询在作用域里解析到的定义是 `Bound(Local)`；本文件里其余同名定义是第 1 层的 `NameOnly` 候选。光标在定义的名字上时，`symbol.defines` 是这个定义（只有 locals 的 `@local.definition` 在这个位置、tags 没有定义时 `local` 为 true），它本身作为唯一的 `Bound(Local)` 候选。
2. **import**：解析扩展点给出这个名字的 import 目标，按路径取目标文件的抽取结果（缓存命中或现场抽取），其中导出的同名定义是 `Bound(Import)`；扩展点的 `bind` 给出的结果按它的 `Binding`。#17 的空实现在这一步没有结果。
3. **索引**：`ci_def` 按名字取出全部定义，经 `ci_ext` 得到 OID 和语言，只取语言、grammar 版本、查询版本与当前语言表一致的单元（见「存储表」的版本过滤），按「候选的分层与过滤」过滤和分层，只留映射里有的 OID。
4. **限时搜索**：第 3 步之后仍没有 `Bound` 候选、且当前子项目里还有未入索引的文件时进行，见「限时搜索」。

第 1–3 步的结果作为第一批交出（目标：索引命中时 ≤ 50 ms），限时搜索的结果逐批追加。结果只追加：后到的候选排在已有候选之后，已有条目不重排、下标不变（调研第 5 节「索引不完整时」的呈现规则）。

去重：每次查询持有一个已交出集合，键是（来源、路径、OID、行、列），来源即 `Location::origin`。每一步的候选在加入结果之前先查这个集合，已经由前面的步骤交出的跳过，保留先交出的那一条（它的层更靠前）。例如当前文本的 blob 已经入索引时，第 3 步会再次查到第 1 步列出的本文件定义；第 2 步现场抽取过的文件，在第 3 步和限时搜索里也会再次出现。同一份内容出现在两个路径上时是两个候选。`semantic` 的结果与候选按位置集合比较，不进入这个集合。集合只存在于这次查询的任务里，查询关闭时释放。

`jump` 和 `preselect`：

- 有 `Bound` 候选、且恰好一个时，`jump` 是它；多个 `Bound` 候选（重载签名、同名的多个 `impl`）时不跳，`preselect` 为第一个。
- 只有 `NameOnly` 候选时，即使只有一个也不跳，`preselect` 为第一个。方法调用、接收者类型不明的成员访问都属于这一类。
- Java 的候选满足「语义导航的注册」里的升级条件时，同时发出语义请求，`semantic` 为 `Pending`。

`query(Named)` 只做第 3 步（不分 `CurrentFile`、`Imported` 两层，`near` 给出的路径决定其余各层），全部是 `NameOnly`，不跳转。

### 候选的分层与过滤

候选按 `CandidateTier` 分层（调研第 6 节）：当前文件 → import 指向的文件 → 同包（扩展点的 `package_of`，默认同一目录）→ 同一子项目 → 当前子项目依赖的子项目（扩展点的 `dependencies`，#17 为空）→ 其余。同层内依次按：在 `Focus::Review` 里的优先、路径距离（去掉公共前缀后两边剩余的段数之和）由近到远、路径字节序、行号。

过滤：

- 语言：只留与当前文本的语言在同一兼容组里的候选。
- 可见性：`File` 只留同一文件；`Package` 只留同一目录；`Module`（Rust）只留同一文件，以及以它为父模块的文件（`a.rs` 对应 `a/` 之下；`mod.rs`、`lib.rs`、`main.rs` 对应所在目录之下）；`Public`、`Unknown` 不过滤。
- 语法种类：

  | `RefSyntax` | 接受的 `DefKind` |
  |---|---|
  | `Call`、`MemberCall` | `Function`、`Method`、`Macro`，以及 `Class`、`Struct`（Python、JS 的构造调用，Rust 的元组结构体） |
  | `Construct`（`new X`） | `Class`、`Struct` |
  | `Type` | `Class`、`Interface`、`Struct`、`Enum`、`Trait`、`Type`、`Module` |
  | `Member` | `Field`、`Method`、`Constant`、`Variable` |
  | `Plain` | 全部 |

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

- 范围：当前子项目里语言在兼容组内、`indexed` 位没有在当前淘汰代数下置位（见「派生状态的核对」）、也不在在途抽取里的文件，按与当前文件的路径距离排序，同一目录的在前。
- 每个文件经 `SafeRoot` 读出（上限 1 MiB），用 `memchr::memmem` 找这个名字的字节串，命中处前后不是标识符字符（ASCII 字母数字、`_`、`$` 和 UTF-8 的多字节序列）才算整词命中。没有命中的文件不解析。
- 命中的文件先做一次存在检查，缓存里有就直接用；没有就按后台截止时间抽取，结果用于这次查询，并交给一个 `Background` 任务写入缓存、置 `indexed` 位。
- 从查询开始计 1 s 后停止：文件之间检查时刻，抽取的截止时间取「250 ms」与「剩余时间」中较小的一个。停止后交出最后一批，`state` 为 `Done(Partial { … })`；全部搜完为 `Done(Complete)`。
- 结果每 50 ms 或每个命中文件交出一批。
- 搜索只读、不建索引的文件不改变 `indexed` 位，第 3 层的批量任务之后照常处理它们。

## 性能

影响的预算（[性能预算](tech-research.md#性能预算)，M 系列 Mac，系统文件缓存已热）：

| 预算 | 本 crate 的份额 | 基准 |
|---|---|---|
| 冷启动 20 ms | 首帧之前不做任何事：`AfterFirstFrame` 激活，`activate` 只建空表、不加载 grammar | `plugin/code-intel/activate`（kernel 的 < 1 ms） |
| 跳转：同文件或索引命中，候选 ≤ 50 ms | 当前文本（100 KB 以内）解析与 locals ≤ 10 ms；名字查找 ≤ 2 ms；过滤、分层与排序 ≤ 3 ms；合计 ≤ 20 ms，余下的给抽屉 | `code_intel/goto_indexed`（cloudprint 形状、10 万文件） |
| 跳转：唯一候选的目标首屏高亮 ≤ 100 ms | 本 crate 的部分同上；目标文件的读取与高亮属于 `file_reader` | `app/goto_single` |
| 跳转：未覆盖，第一批 ≤ 300 ms，到停止 ≤ 1.1 s | 第一批是第 1–3 步的结果；限时搜索 1 s 后停止，取消在 20 ms 内生效 | `code_intel/search_stop` ≤ 1.02 s |
| 引用：第一批 ≤ 200 ms，10 万文件全部 ≤ 2 s | `ci_ref` 查找 ≤ 5 ms；每个候选文件的现场抽取约 1–2 ms / 10 KB | `code_intel/references_100k` |
| 索引覆盖：审阅集合 ≤ 2 s，子项目 ≤ 10 s，10 万文件 ≤ 2 分钟（接电源） | 单线程抽取吞吐 ≥ 4 MB/s；热启动 10 万文件的存在检查 ≤ 500 ms | `code_intel/extract_throughput`、`code_intel/warm_100k` |
| 空闲内存：monorepo < 100 MB（后台暂停时和全部完成后） | 本 crate 常驻 cloudprint 形状 ≤ 4 MB、10 万文件 ≤ 10 MB：映射、覆盖部分、`indexed` 位、已编译的 grammar 配置（按用到的语言加载）、项目表；不含名字表和 `file_reader` 的 8 MB 高亮与文本 LRU，与后者合计在 ADR 0030 的 ≤ 10 MB / ≤ 20 MB 目标之内 | `plugin/code-intel/resident_cloudprint`、`plugin/code-intel/resident_100k` |
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
- 存储（`store_*`）：单元的写入与读回；存在检查只返回键完全相同的单元并标记访问；按名字查找区分大小写；单元被淘汰后三张表里没有它的行；配额小于数据量时按访问时间淘汰本命名空间的单元；同一个 OID 有旧 grammar 版本或旧查询版本的单元时，按名字查定义、查引用、存在检查和名字表都只返回当前版本的单元（`store_version_filter_*`）；写入被丢弃时不 panic、内存里的结果仍可用。
- 核对（`revalidate_*`）：把 `code_intel.tags` 的配额设得小于当前 worktree 的数据量，淘汰掉一部分单元后：淘汰代数增加，被淘汰的文件重新入索引；淘汰之后、重新入索引之前，跳转仍能经限时搜索找到这些文件里的定义（它们没有被当作已入索引跳过）；查询现场发现单元不在时清掉位并重新排队；仍在的单元只重新置位、抽取函数没有被调用（计数钩子）；分库被删除重建后全部重新入索引。
- 映射（`map_*`）：从带 OID 的文件列表建映射；前 8 字节相同的两个 OID 都能正确查到；覆盖项遮住 index 条目；修改后的旧 OID、删除的文件不出现在查询结果里；未跟踪文件可以查到；文件列表换代后清理覆盖项。
- 覆盖（`overlay_*`）：`FakeGit` 构造 git 设计「工作区比较」表里的情况，覆盖项与工作区实际内容一致：未暂存的修改、`StagedOnly`（工作区等于基准）、未暂存的删除、未跟踪文件、重命名、`Filtered` 与超过 1 MiB 的文件为 `Unindexable`。
- 调度（`schedule_*`）：层的先后；第 1、2 层持有 `BusyGuard`、在 `Interactive` / `Background` 上运行，第 3–5 层持有 `BackgroundGuard`、在 `Batch` 上运行；暂停开关在批次边界生效；一次抽取正在运行时打开暂停开关，`ManualClock` 推进 50 ms 内这次抽取返回 `Cancelled`、它的结果没有写入、`indexed` 位不变，恢复后这个文件先被重新抽取（`schedule_pause_abandons_in_flight`）；一批不超过 64 个文件、2 MiB、50 ms；切换 worktree 后游标重建、已写入的不重复抽取；同一 OID 在两个路径上只抽取一次；第 5 层只抽取缓存里没有、且工作区内容与 index 相同的文件。
- 语言包（`grammar_*`，用 `testing::FakeGrammars`）：`grammar_installed_*`：安装后新扩展名的跟踪文件和未跟踪文件进入映射并被抽取；`grammar_updated_*`：更新后旧版本的单元不再命中、这种语言的文件按新版本重新抽取，旧版本在途抽取的结果没有写入；`grammar_rolled_back_*`：回滚后之前那个版本写过的单元重新命中、不重新抽取；`grammar_removed_*`：撤销后这种语言的文件从映射和 `coverage` 里消失，查询返回 `UnsupportedLanguage`，在途抽取被取消、结果没有写入；`grammar_merged_*`：连续多个通知并成一轮，结果与逐个处理相同，过时的代数被丢弃；`grammar_lease_*`：`FakeGrammars` 记录模块的卸载，撤销或更新时有一个在途抽取和一个仍被查询持有的 `ParsedFile`，两者都结束之前模块没有被卸载，结束之后恰好卸载一次。
- 电池（`battery_*`）：电池供电时第 4、5 层不运行、`coverage` 为 `Deferred`；接上电源后继续；电池供电时一次 `query` 让第 4、5 层继续。
- 增量（`incremental_*`）：重新读进行中文件列表换代时，这次结果被丢弃、路径按新的文件列表再读一次；用 `FakeBackend` 驱动 `WorktreeBatch`：修改一个文件后按新 OID 抽取、旧 OID 不再返回；目录事件；`rescan` 触发工作区比较；`meta.index` 后等新的文件列表再重建；连续事件并成一轮；从后端交出事件到覆盖部分更新完之前 kernel 不静止。
- 热启动（`warm_*`）：缓存里已有全部单元时重新打开项目，抽取函数一次也没被调用（计数钩子），只有存在检查；删掉一部分单元后只抽取这一部分。
- 跳转（`goto_*`）：四步的顺序与停下的条件；唯一 `Bound` 候选给出 `jump`；唯一 `NameOnly` 候选只有 `preselect`；兼容组、可见性、语法种类的过滤；分层与同层排序；审阅集合优先；结果只追加、已有下标不变；当前文本的 blob 已入索引时，第 1 步列出的本文件定义不在第 3 步重复出现，现场抽取过的 import 目标不在第 3 步和限时搜索里重复出现（`goto_no_duplicates_*`）；用测试里的 `Resolver` 实现检查第 2 步和 `Bound(Import)`；`Stale`、`NoSymbol`、`UnsupportedLanguage`；从对象库一侧（`TextOrigin::Object`）发起的查询，本文件的候选 `origin` 为 `Object`，由它发起的 `References` 读同一个 blob，不读工作区（`goto_object_origin_*`）。
- 预取与焦点（`prefetch_*`、`focus_*`）：映射换代或淘汰代数变化之后，2 秒内同样的查询不沿用预取结果；非当前 worktree 的焦点不进入第 1、2 层，切换到那个 worktree 后生效。
- 限时搜索（`search_*`）：只解析整词命中的文件；`ManualClock` 推进到 1 s 时停止并给出 `Partial` 的计数；命中文件的结果写入缓存；搜完时为 `Complete`。
- 引用（`refs_*`）：局部变量和参数的 `References` 找到它在作用域里的全部用处（这些位置只在 `@local.reference` 里，不在 tags 引用和 `ci_ref` 里），不查 `ci_ref`、没有 `same_name`；locals 记为程序作用域局部定义的顶层函数，它的同文件引用也被找到；同时被 tags 和 locals 捕获的导出顶层函数 `local` 为 false，另一个文件里对它的引用经 `ci_ref` 找到、未入索引的文件经限时搜索找到（`refs_toplevel_captured_by_locals`）；locals 证明的同文件引用是调用方；同一处引用、同一个文件在多个来源里出现时只列一次、只解析一次（`refs_no_duplicates_*`）；被局部变量遮住的同名引用被丢弃；没有依据的跨文件引用在 `same_name`；测试 `Resolver` 的 `bind` 让跨文件引用成为调用方；未入索引的部分经限时搜索找到。
- 名字表（`names_*`）：去重、NFC、只含当前 worktree 映射里的 OID（另一个 worktree 独有的名字不在其中）；第一次 `names` 返回 `Building`，`Names` 事件带着表，监听方保存后再调 `names` 返回 `Ready`、是同一个 `Arc`（`names_ready_after_build`）；构建期间重复调用不另起构建；代数落后时重建、期间返回旧表；所有持有者释放后再调用重新构建；表被持有时索引写入新的批次，`ManualClock` 推进过间隔后后台重建并发出带新 `Arc` 的 `Names`，间隔内的多次变化只触发一次重建，持有者释放后不再重建（`names_refresh_while_held`）。
- 升级（`escalate_*`）：Java 多候选和唯一 `NameOnly` 候选时调用提供者，其他语言不调用；提供者的 token 为 `Dead` 时不调用；结果相同时 `Same`、不同时 `Ready`；`head_only` 的提供者只在 `head_clean` 时被调用，返回的位置里内容与 HEAD 不同的文件被丢掉；只暂存、未提交的文件（工作区等于 index、不等于 HEAD）的 `head_clean` 为 false，提交之后为 true；index 变化之后、新的工作区比较完成之前为 false（`head_dirty_*`）；登记 drop 后不再调用；SCIP 与 jdtls 两个假提供者同时处理 Java 时，按两种回复顺序得到相同的合并结果，HEAD 相同的文件取 SCIP 的位置、有改动的文件取 jdtls 的位置，过时 ID 的回复被丢弃，查询关闭时未回复的请求收到 `cancel`（`escalate_arbitration_*`）；提供者接受请求后注销，只有它在请求中的查询 `semantic` 变为 `Failed`、不再停在 `Pending`，另有 SCIP 回复的查询按剩下的回复重新合并，注销之后到达的回复被丢弃（`escalate_unregister_*`）。
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
- B. `project`：文件列表带 OID，`plugin/project/resident_*` 的份额已更新。
- C. `cache`：「自定义表」的设计与实现。
- D. `kernel` 的 `BackgroundGuard`、`executor` 的 `Priority::Batch`（ADR 0029）和 `Executor::on_shutdown`。

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
   - `map_*`、`revalidate_*`、`overlay_*`、`schedule_*`、`grammar_*`、`battery_*`、`incremental_*`、`warm_*` 测试通过。
   - `rg -n 'std::fs::|File::open' crates/code_intel/src --glob '!**/tests*' --glob '!**/testing*'` 没有结果：工作区文件只经 `SafeRoot` 读取。
   - `rg -n 'Command::new|thread::spawn' crates/code_intel/src` 没有结果。
   - `code_intel/map_100k`、`code_intel/map_100k_bytes`、`code_intel/batch_peak` 在预算内。
4. **查询与插件**（前置 A）：`Cargo.toml` 加 `plugin = true`。
   - `goto_*`、`search_*`、`refs_*`、`names_*`、`escalate_*`、`plugin_*`、`watchdog_*` 测试通过。
   - `python3 scripts/check_architecture.py` 通过：本 crate 不依赖 GPUI、不依赖其他插件 crate，除 `app` 和 tool 层外没有 crate 依赖它。
   - `code_intel/goto_indexed`、`code_intel/search_stop`、`code_intel/references_100k`、`code_intel/names_100k`、`code_intel/names_100k_bytes`、`plugin/code-intel/resident_cloudprint`、`plugin/code-intel/resident_100k` 在预算内。
5. **端到端基准**（前置 #20 的抽屉和 `file_reader` 在探针里的里程碑）：bench 探针增加打开抽屉、跳转、覆盖层完成的 `Milestone`，以及读取 `coverage` 的方式。
   - `app/goto_candidates`、`app/goto_single`、`app/goto_unindexed`、`app/goto_unindexed_done`、`app/index_review_set`、`app/index_subproject`、`app/index_100k`、`app/idle_monorepo_indexed` 在 `crates/bench/src/budgets.rs` 里改为生效，macOS CI 上在预算内。
   - `app/idle_monorepo` 在后台工作暂停时仍 < 100 MB。
