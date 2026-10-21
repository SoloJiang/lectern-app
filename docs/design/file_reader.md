# file_reader 设计

状态：实现中。前置条件 A（`services` 的 `reader` 模块、`DirTable`、`ActiveTree::dirs`、`DEPENDENCY_EXTENSIONS`，[#148](https://github.com/SoloJiang/lectern-app/pull/148)）、C 与 G（`platform`，[#147](https://github.com/SoloJiang/lectern-app/pull/147)）、D（`git`，[#146](https://github.com/SoloJiang/lectern-app/pull/146)）、F（`code_intel`，[#149](https://github.com/SoloJiang/lectern-app/pull/149)）已实现，见「实际验证」「与设计的差异」；「验收」第 1 步（`syntax` 骨架、语言表与整文件高亮，[#150](https://github.com/SoloJiang/lectern-app/pull/150)）已实现；前置条件 B（`project` 在后台建目录表、单独一代发布，`services` 的 `DirTable::new` 提速，`project/dirs_100k`、`project/dirs_100k_bytes`，见 [project 设计「实际验证」](project.md#实际验证)）已实现；第 2 步（首屏窗口、`FullParse`、`ChainWindow` 与降级）已实现；前置条件 E 和第 3–8 步尚未实现。

对应 issue [#10](https://github.com/SoloJiang/lectern-app/issues/10)。本 issue 交付两样功能：按需层的 Files（侧栏里按需展开的文件树，文件列表来自 git index），以及收起层的完整文件阅读（⇧⌘O，默认只读）和轻量编辑（Edit… / ⌘E）（[交互规范「三层结构」](interaction.md#三层结构)、[ADR 0012](../adr/0012.md)）。打开任何大小的文件到首屏高亮 < 16 ms、滚动 120 fps 的做法由 [ADR 0034](../adr/0034.md) 规定，本文把它写成接口和数据结构。高亮的计算放在新的领域库 `syntax`（[ADR 0041](../adr/0041.md)），`file_reader` 只做界面。

## 范围

本 issue 跨两个新 crate 和几个已有 crate 的小改动：

| crate | 层 | 内容 |
|---|---|---|
| `syntax`（新，`lectern-syntax`） | 领域，非插件的库 | 语言表与高亮查询、首屏窗口、按窗口与整文件的高亮、着色区间、文本存储与行索引、行检查点（`syntax.lines`）、8 MiB 高亮与文本 LRU、打开文档的读取与刷新、空闲预取、编辑模式的增量高亮、保存与三方合并的领域部分、grammar 的卡死检测 |
| `file_reader`（本 crate） | 视图，插件 crate | 阅读视图（自绘的固定行高列表）、查找栏、前进后退、编辑模式（gpui-component `InputState` 与 `InputHighlighter` 适配器）、Files 文件树、`dyn Reader` 服务 |
| `services` | kernel | `reader` 模块（`dyn Reader`、`ReadTarget`、`Anchor`）；`project` 模块的 `DirTable` |
| `project` | 领域，插件 | 文件列表就绪后在后台建目录表（[ADR 0030](../adr/0030.md)：约 1 MB，计入 `project`） |
| `platform` | 基础设施 | `SafeRoot::replace_file`：保存用的原子替换；`ThreadCpuClock`：grammar 卡死检测用的线程 CPU 时钟 |
| `git` | 领域 | `merge3`：三方合并；`Text::shared`：取 diff 文本内部的 `Arc` |
| `code_intel` | 领域，插件 | 补测试：批量循环在每个批次边界重新入队（已实现，测试已补）；grammar 卡死检测的登记 |
| `workspace`、`app` | 视图、入口 | 离开守卫：关闭阅读视图、切换 worktree、关闭项目、关闭窗口和退出之前问本 crate |

- 做：
  - 完整文件阅读：工作区文件、对象库里的 blob（Diff 的旧一侧）、依赖根里的文件（`TextOrigin::Dependency`，[ADR 0033](../adr/0033.md)）；按字节偏移、行号或文件开头定位；档位 A / B / C 与降级为纯文本；行号栏；长行折成显示行；查找（⌘F）；复制。
  - 前进、后退（⌘] / ⌘[，只在阅读时）：本窗口最近 100 个阅读位置，只在内存里。
  - 编辑：只对档位 A 的工作区文件；撤销重做、查找替换、自动缩进、括号补全（`InputState` 自带）；保存（⌘S）；与 agent 同时改了文件时只在编辑过的文件上提示三方合并；未保存的编辑只在用户经离开守卫确认之后才丢弃。
  - Files：当前 worktree 的受跟踪文件（`FileList`，来自 git index），目录在前、Finder 的排序，单子目录的目录链折成一行，打开时定位到当前在看的文件。
  - `services::reader`：其他视图（审阅、抽屉、⌘K）经它打开文件和请求预取，不依赖本 crate（架构规则 7）。
  - 首屏、整文件高亮、预取和编辑期间的语法树都按 ADR 0033、0034 的守卫与线程运行。
- 不做：
  - 符号抽取、跳转与查找引用：`code_intel`。本 crate 打开文件时调 `set_focus(Current)`，指针停留时调 `prefetch`（[code_intel 设计「接口」](code_intel.md#接口)的调用方一节）。Space 和 ⌘ 点击把位置交给上下文抽屉（[#20](https://github.com/SoloJiang/lectern-app/issues/20)），交接接口随 #20 的设计。
  - Diff 的绘制和 diff 两侧按窗口的高亮：`review_view`。它直接调用 `syntax` 的纯函数（`window::plan`、`highlight::window`），是否共用本文的 LRU 由 review_view 的设计决定，要共用时经 `dyn Reader` 增加接口。
  - 窗口骨架、侧栏条目与内容区的切换：`workspace`（[#30](https://github.com/SoloJiang/lectern-app/issues/30)），本 crate 经它的 `dyn Contributions` 注册（见「对其他 crate 的要求」）。代码配色与等宽字体的度量：`ui`（[#6](https://github.com/SoloJiang/lectern-app/issues/6)）。
  - 审阅意见（C）：只在审阅的改动叙事里写，阅读视图不提供。
  - 语言包的高亮：kernel 的 grammar 加载器（[#39](https://github.com/SoloJiang/lectern-app/issues/39)）给出的 `LoadedGrammar` 还没有高亮查询，见「本 issue 之外」。
- 本 issue 之外：
  - 语言包（ADR 0018）的高亮：#39 / #40 实现加载器时，`LoadedGrammar` 增加 `highlights_query`，`GrammarSource` 移到 `syntax`（非插件，`code_intel` 和本 crate 都能用），`syntax::Languages::with_pack` 接收它。
  - Files 里的未跟踪文件和被忽略的目录（[代码导航调研第 8 节](../research/code-navigation.md#8-文件树浏览)的「显示被忽略的文件」）：见「未决问题」2。
  - 保存时调用项目的 formatter：见「未决问题」1。
  - 依赖缓存根（`DepRoot::Cache`）的读取：TS / JS 不用它（code_intel 设计 `DepRoot`），Cargo、Go 的依赖层加入时，根路径的解析从 `code_intel` 移到 `platform`（读本进程环境的函数），两边共用。之前打开 `Cache` 根的位置显示「无法读取」。
  - 文件夹项目的变化通知：`project` 在 P1 不监听文件夹项目（[project 设计「范围」](project.md#范围)），本 crate 也收不到它们的事件，见「状态与异常」。

## 依赖

`syntax`：

| 依赖 | 用途 | 选用理由 |
|---|---|---|
| `tree-sitter` `=0.27.1`（不开 `wasm`） | 解析、高亮查询（`QueryCursor::set_byte_range`）、进度回调 | 与 `code_intel` 同一份（ADR 0030），链接进二进制的只有一份 |
| `tree-sitter-typescript`、`-javascript`、`-rust`、`-java`、`-python`、`-go` | 与 `code_intel` 相同的 grammar crate | `[workspace.dependencies]` 里已 `=` 锁定，共用 |
| `tree-sitter-json =0.24.8`、`tree-sitter-yaml =0.7.2`、`tree-sitter-toml-ng =0.7.0`、`tree-sitter-md =0.5.3`（不开 `parser`） | ADR 0018 核心高亮语言里 `code_intel` 没有的四种 | 新增依赖，见 [ADR 0041](../adr/0041.md)：MIT，正常依赖只有 `tree-sitter-language`，`tree-sitter-md` 的 `parser` feature 会带进 tree-sitter 0.26，不开 |
| `lectern-platform` | `SafeRoot`、`open_file`、`OpenFile`、`replace_file` | 工作区文件只经 `SafeRoot` 读写，不用 mmap |
| `lectern-git` | `BlobHasher`、`Oid::for_blob`、`Repo::blob`、`Worktree::canonical_oid`、`merge3` | gix 只在 `git` 里使用 |
| `lectern-cache` | `syntax.lines` 命名空间 | 派生数据按 OID 存（架构规则 5） |
| `lectern-executor`、`lectern-kernel` | 任务、`BusyGuard`、`BackgroundGuard`、`PluginHandle` | 后台线程只由 `executor` 管理；守卫由使用方插件的句柄取得 |
| `lectern-fs-watch` | 打开文档所在 worktree 的 `WorktreeBatch` | 与 `project`、`code_intel` 共用同一份 worktree 状态 |
| `lectern-services` | `BlobId`、`RelPath`、`TextOrigin`、`DepRoot`、`ReadTarget` | 数据类型 |
| `memchr`、`unicode-width` 0.2 | 找换行、查找；长行按字符格数折行 | 都已在 Cargo.lock，发布构建不新增 crate；`unicode-width` 是 MIT OR Apache-2.0 |
| `insta`、`tempfile`（dev） | 区间快照、临时工作区 | 工作区统一依赖 |

`syntax` 不依赖 GPUI（架构规则 2）、`gix`、`rusqlite`、`ropey`。

`file_reader`：

| 依赖 | 用途 | 选用理由 |
|---|---|---|
| `gpui`、`gpui-component =0.7.3`（不开 `tree-sitter` feature） | 自绘元素、`uniform_list`、`InputState` | ADR 0016、0034 |
| `lectern-ui` | 系统色、代码配色、等宽字体度量、按钮、分段控件、动效原语 | ADR 0015 |
| `lectern-syntax` | 文档快照、区间、编辑会话 | 视图只读快照（架构规则 4） |
| `lectern-kernel`、`lectern-services` | 插件、`dyn Projects`、`dyn CodeIntel`、`dyn Contributions`，提供 `dyn Reader` | 插件之间只经 `services`（架构规则 7） |
| `lectern-i18n` | 界面字符串 | ADR 0013 |
| `lectern-executor`、`lectern-cache`、`lectern-fs-watch` | 由 `app` 创建，构造插件时传入，转交给 `syntax` | ADR 0025「进程级的共享资源」 |

本 crate 是插件 crate（`plugin = true`，[ADR 0025](../adr/0025.md)）：插件 "file-reader"，只依赖 kernel 层和非插件的库 crate，只有 `app` 和 tool 层依赖它。本 crate 不依赖 `tree-sitter`，不做 IO 和解析。

## 接口

### 服务（`lectern-services` 的 `reader` 模块）

```rust
/// 单实现服务，`file-reader` 插件提供。
/// impl Service for dyn Reader { const ID = "reader"; const MULTIPLE = false; }
pub trait Reader: Send + Sync {
    /// 请求打开完整文件阅读。立即返回，任何线程可调用；请求排到主线程，经「离开守卫」（见「编辑」）之后才替换
    /// 本窗口当前的文档：当前文档有未保存的编辑时先问保存 / 不保存 / 取消，选「取消」时这次请求作废。
    /// 结果以 `ReaderEvent::Open` 送达。审阅视图（⇧⌘O、从 hunk）、抽屉的候选、⌘K 的文件结果都经它打开。
    fn open(&self, target: ReadTarget) -> OpenTicket;
    /// 交互静止时的预取（ADR 0033）：只取前 4 个，算出高亮区间放进 LRU，不打开、不发事件。立即返回。
    fn prefetch(&self, targets: Vec<ReadTarget>);
    /// 登记一份调用方正在持有的对象库内容（审阅视图显示的 diff 两侧文本）。登记表持有这份 `Arc` 的一个强引用，
    /// 只到返回的 `ObjectShare` drop 为止；drop 即注销。调用方在自己持有这份内容期间保留它、放下内容时一起放下，
    /// 所以登记不延长内容的生命期（不用 `Weak`：`Arc<[u8]>` 的计数和字节在同一次分配里，`Weak` 也会让整块分配一直存活，
    /// ADR 0035）。打开这个 OID 的 `Object` 目标时，有登记就直接用，不再从对象库解压（见「定位」）。立即返回。
    fn share_object(&self, oid: BlobId, bytes: Arc<[u8]>) -> ObjectShare;
}

/// `share_object` 的登记。drop 时注销（任何线程，不阻塞）；同一个 OID 登记多次时，最后一个 drop 才移除。
/// 已经打开的文档自己持有它用到的那份 `Arc`，到文档关闭为止，不受注销影响。
/// `services` 不知道登记表的类型：本 crate 用 `new` 构造，传入的注销回调持有 `Weak<登记表>`、OID 和登记序号，
/// drop 时调用一次（与 `code_intel::ProviderRegistration` 相同）。不序列化。
pub struct ObjectShare { /* 注销回调 */ }
impl ObjectShare {
    pub fn new(release: impl FnOnce() + Send + 'static) -> Self;
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct OpenTicket(pub u64);

/// 在主线程上、阅读视图切换之后（或请求作废之后）发出。
pub enum ReaderEvent {
    Open { ticket: OpenTicket, outcome: OpenOutcome },
}
impl Event for ReaderEvent {}

pub enum OpenOutcome {
    Shown,
    /// 用户在离开守卫的提示里选了「取消」，或保存时发生冲突：当前文档和它的编辑保持不变。
    Cancelled,
}

pub struct ReadTarget {
    pub project: ProjectKey,
    pub worktree: Option<WorktreeKey>,   // 文件夹项目为 None
    pub path: RelPath,
    /// `services::code_intel::TextOrigin`：工作区、对象库或依赖根。
    pub origin: TextOrigin,
    /// 调用方看到的那份内容的 OID（hunk 的新旧一侧、`Location::oid`）。`Object` 必须有。
    pub expect: Option<BlobId>,
    pub at: Anchor,
}

pub enum Anchor {
    Start,
    /// 行和列从 0 起，列是行内的字节偏移。agent 输出里的 `file:line` 只有行号。
    Line { line: u32, column: u32 },
    /// `byte` 是 `expect` 那份内容里的字节偏移（`git` 的 `*_byte`、`Location::byte`），同时给出行号：
    /// 读到的内容的 OID 与 `expect` 不同时按行号定位（见「定位」）。
    Byte { byte: u64, line: u32, column: u32 },
}
```

数据类型派生 `Clone`、`PartialEq`、`Eq`、`Debug` 和 serde（与 `services` 的其余模块相同）。

### 服务（`lectern-services` 的 `project` 模块）：目录表

```rust
/// 当前 worktree 的目录表，由 `project` 在 `FileList` 就绪之后在后台建（ADR 0030），与它来自同一份文件列表。
/// 只读、不做 IO，任何线程可调用。
pub struct DirTable { /* dirs: Box<[Dir]>, names: Box<str>, subdirs: Box<[DirId]>, files: Box<[u32]>, list: Arc<FileList> */ }

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct DirId(pub u32);   // 表里的下标，根为 0

impl DirTable {
    /// 建表（「数据结构」的「目录表」）。纯计算，`project` 在 `Background` 任务里调用；反序列化也经它。
    pub fn new(list: Arc<FileList>) -> DirTable;
    pub fn files(&self) -> &Arc<FileList>;    // 建表用的那份文件列表
    pub fn root(&self) -> DirId;
    pub fn len(&self) -> usize;               // 目录数，含根；`is_empty` 总是 false
    pub fn find(&self, dir: &str) -> Option<DirId>;          // 二分查找
    pub fn path(&self, dir: DirId) -> &str;                  // 根为 ""
    pub fn name(&self, dir: DirId) -> &str;
    pub fn parent(&self, dir: DirId) -> Option<DirId>;
    /// 直接子目录，Finder 的顺序（见下）。
    pub fn subdirs(&self, dir: DirId) -> &[DirId];
    /// 直接子文件在 `FileList` 里的下标，Finder 的顺序。
    pub fn dir_files(&self, dir: DirId) -> &[u32];
    /// 只有一个子目录、没有文件的目录链的末端（`src/main/java/com/example`），不折叠时返回自己。
    pub fn fold_end(&self, dir: DirId) -> DirId;
    /// 子树里的文件数（行的次要文字不显示它，供探针和测试）。
    pub fn file_count(&self, dir: DirId) -> u32;
}

/// Finder 的比较：不区分大小写（逐字符 `to_lowercase`），连续的十进制数字按数值比较，其余相等时按字节序。
/// 与 `localizedStandardCompare` 不完全相同（重音、区域规则），见「已知限制」。纯函数。
pub fn finder_cmp(a: &str, b: &str) -> Ordering;
```

- `ActiveTree` 增加 `dirs: Scan<Arc<DirTable>>`：`files` 为 `Ready` 之后才可能 `Ready`；`ChangedParts` 增加 `dirs: bool`。目录表单独一代发布，不推迟 `files` 的就绪（`project/open_100k/files` 不变）。
- 不序列化内部布局：序列化为 `{ "dirs": [[路径, 父下标], …], "files": FileList }`（根的父下标为 null），反序列化只读 `files`（经 `FileListBuilder`），用 `DirTable::new` 重建，不信任 `dirs`，与 `FileList` 的做法相同。

### `syntax` 的公开项

所有阻塞函数都只在 executor 的任务里调用；标为「纯计算」的不做 IO、任何线程可调用，但都不在 UI 线程上调用解析。

```rust
// ---- 语言与区间 ----

/// 高亮的语言表。核心语言静态链接，表在第一次用到时建（不做 IO、不编译查询）。
pub struct Languages { /* … */ }
impl Languages {
    pub fn core() -> Arc<Languages>;
    /// 按扩展名和少数整文件名（`Cargo.lock` → TOML，`package-lock.json` → JSON）。没有时为 None：显示纯文本。
    pub fn for_path(&self, path: &str) -> Option<&Lang>;
    pub fn get(&self, name: &str) -> Option<&Lang>;
    pub fn iter(&self) -> impl Iterator<Item = &Lang> + '_;
}

/// 捕获变成区间的规则（捕获名的映射、同一节点的优先规则、按行切分）改变时加一，进 `query_version`。
pub const SPANS_VERSION: u32;

/// 同一节点被几个模式捕获时取查询里的第一个还是最后一个，每种语言一条，写在查询文件头（见「与设计的差异」）。
pub struct Lang { /* name, grammar, 高亮查询, 版本, 规则族, 优先规则 */ }
impl Lang {
    /// 与 `code_intel` 的 `LangName` 相同的短名（"ts"、"rust"……），加上 "json"、"yaml"、"toml"、"markdown"。
    pub fn name(&self) -> &str;
    /// 与 code_intel「语言与抽取键」同一规则：crate 名 @ 版本的 64 位 FNV-1a。
    pub fn grammar_version(&self) -> u64;
    /// 高亮查询文本、tree-sitter 版本和 `SPANS_VERSION` 合在一起的 64 位 FNV-1a。
    pub fn query_version(&self) -> u64;
    pub fn rules(&self) -> Rules;   // 安全位置的规则族
    /// 编译高亮查询，只编译一次、之后常驻。阻塞，后台线程；失败时这种语言本次运行显示纯文本。
    pub fn prepare(&self) -> bool;
}

pub enum Rules { Code, Json, Markup }

/// 高亮查询的捕获名归成的种类，`ui` 把它映射到系统色（见「对其他 crate 的要求」）。
#[repr(u8)]
pub enum TokenKind {
    Keyword, String, Number, Comment, Function, Type, Constant, Property, Variable,
    Operator, Punctuation, Tag, Attribute, Escape, Heading, Link, Emphasis, Other,
}

/// 一个着色区间：行内起点（u32）、长度（24 位，超过时拆开）、种类（8 位），共 8 字节。
#[derive(Clone, Copy)]
pub struct Token(u64);
impl Token {
    pub const MAX_LEN: u32 = (1 << 24) - 1;
    pub fn new(start: u32, len: u32, kind: TokenKind) -> Option<Token>;   // len 为 0 或超过 MAX_LEN 时为 None
    pub fn start(self) -> u32; pub fn len(self) -> u32; pub fn end(self) -> u32; pub fn kind(self) -> TokenKind;
}

/// 一段文本 `[start, end)` 的区间，按行存放。行以行首的字节偏移标识，不需要行号（行号在后台扫描前只是估计）。
/// 只存有区间的行：`line_starts` 是这些行的行首（相对 `start`），区间不含行尾的 `\n`。
pub struct WindowSpans { /* start, end, line_starts: Box<[u32]>, offsets: Box<[u32]>, tokens: Box<[Token]>, exact: bool */ }
impl WindowSpans {
    pub fn range(&self) -> Range<u64>;
    /// `line_start` 处那一行的区间；不在这一段里时为空。
    pub fn line(&self, line_start: u64) -> &[Token];
    /// 有区间的行，按顺序：行首（绝对偏移）和这一行的区间。
    pub fn lines(&self) -> impl Iterator<Item = (u64, &[Token])> + '_;
    /// 起点由语法树确认（整文件、按树切的窗口链）为 true；首屏的启发式窗口为 false，之后被替换。
    pub fn exact(&self) -> bool;
    pub fn bytes(&self) -> usize;
}

// ---- 首屏窗口（纯计算）----

pub mod window {
    /// 首屏的一次 `read_ranges`：`[0, min(8 KiB, size))` 与 `[t − 64 KiB, t + 68 KiB)`，按
    /// platform 设计「首屏的一次调用」截取；`size ≤ 1 MiB` 时只有一段 `[0, size)`。
    pub fn first_read(size: u64, target: u64) -> Vec<Range<u64>>;
    /// 只有行号、有行检查点 `c` 时：`[0, 8 KiB)` 与 `[c − 64 KiB, c + 132 KiB)`（platform 设计「按行定位」）。
    pub fn checkpoint_read(size: u64, checkpoint: u64) -> Vec<Range<u64>>;

    /// 从首屏读到的字节算出可见部分和窗口。`rows` 是视口能放下的行数。
    pub fn plan(rules: Rules, head: &[u8], around: &[u8], around_start: u64, size: u64,
                target: Target, rows: u32) -> Plan;

    pub enum Target { Byte(u64), LineFrom { checkpoint_byte: u64, checkpoint_line: u32, line: u32 } }

    pub struct Plan {
        pub encoding: Encoding,          // Utf8 | Utf8Bom | NotText（文件头 8 KiB 或窗口里有 NUL，或不是 UTF-8）
        /// 目标所在行的行首；往前 4 KiB 内找不到换行（长行）时就是目标本身，首屏的显示行从这里开始折。
        pub target_line_start: u64,
        /// 可见部分：目标之前至多 rows / 3 行、之后至多 rows 行，至多 64 KiB；长行按 8 KiB 计。
        pub visible: Range<u64>,
        /// 窗口：安全位置起、可见部分之后 4 KiB 止（JSON 截到可见部分之后最后一个完整的值）；
        /// 目标之前 64 KiB 内找不到安全位置（不超过 1 MiB 的文件整份读入，也只看这 64 KiB）、或 `NotText` 时为 None（先画纯文本）。
        pub window: Option<Range<u64>>,
        /// JSON 在窗口两端补的括号（ADR 0034）。
        pub json_pad: JsonPad,
    }

    /// JSON 窗口单独解析时前后补的字节：窗口里出现的、在窗口之前打开的容器按出现的顺序在前面打开
    /// （父容器是对象时带空键 `"":`），窗口结束时仍开着的在后面关上。其他语言和从文件开头开始的窗口为空。
    pub struct JsonPad { pub before: Box<[u8]>, pub after: Box<[u8]> }

    /// 安全位置的规则（ADR 0034）：`Code` 取可见区域之前约 8 KiB 处往回 4 KiB 内第 0 列开始、不是右括号和注释续行的行，
    /// 没有时取缩进最少的行；长行取 `;` 或 `}` 之后；`Json` 取形如 `,"键":` 的成员开头（键 ≤ 256 字节、不含反斜杠）；
    /// `Markup` 取空行之后。只看给出的字节，不从文件开头扫描。给出的字节从文件开头开始而又找不到时取 0。
    pub fn safe_start(rules: Rules, bytes: &[u8], base: u64, before: u64) -> Option<u64>;
}

// ---- 解析与高亮（阻塞，后台线程）----

pub mod highlight {
    /// 这一次调用的运行预算（从调用开始计的时长，不是绝对时刻）、取消标志和让路探针（`BackgroundGuard::yield_requested`）。
    /// 每次调用都按 `budget` 向 `health` 登记，返回时（任何结果）注销，见「grammar 的卡死检测」。
    pub struct Budget<'a> { pub budget: Duration, pub cancel: &'a AtomicBool, pub yield_probe: Option<&'a dyn Fn() -> bool> }

    /// `TimedOut`：这一片用完；`Yielded`：让路探针要求停下。两者（以及 `Cancelled`）都保留解析器的状态，
    /// 下一次 `step` 从停下处继续。`Unavailable`：这个 grammar 版本已被 `health` 停用，或查询编译不了，调用方改为纯文本。
    pub enum Outcome<T> { Done(T), TimedOut, Cancelled, Yielded, Unavailable }

    /// 窗口单独解析，只算 `visible` 的区间（`exact` 为 false）。首屏用，预算 8 ms（`FIRST_WINDOW_DEADLINE`），不续：
    /// 超时就先画文本，这个窗口交给档位的后续计算。第一次用到时编译查询的时间不计入预算。
    pub fn window(lang: &Lang, src: &dyn Source, window: Range<u64>, pad: &JsonPad, visible: Range<u64>,
                  budget: Budget<'_>) -> Outcome<WindowSpans>;

    /// 档位 A 的整文件解析：每次 `step` 至多一片（8 ms）加至多一个 8 KiB 的查询块，解析完按块跑高亮查询，
    /// 最后一片给出整个文件的区间（`exact`）、释放语法树。解析器和查询的进度在两片之间保留；
    /// `src` 换了时调用方新建一个（tree-sitter 要 `reset`，大文件调研第 3 节）。`new` 在第一次用到时编译查询，不计入预算。
    /// `Done` 之后或 `Unavailable` 之后再调 `step` 返回 `Unavailable`。
    pub struct FullParse { /* 语言、health、src、解析器或查询的进度 */ }
    impl FullParse {
        pub fn new(lang: &Lang, src: Arc<dyn Source>) -> FullParse;
        pub fn step(&mut self, budget: Budget<'_>) -> Outcome<WindowSpans>;
    }

    /// 档位 B / C 的窗口链里的一个窗口：从 `start` 解析约 64 KiB，按树切（与 code_intel「大文件」的「窗口」规则相同，
    /// `ERROR` 节点里的最后一个子节点见「与设计的差异」第 2 步），完成时给出这一段的区间（`exact`）和下一个窗口的起点；
    /// 找不到切点时扩到 256 KiB 再找一次，仍没有时切在最后一个 token 之后，连 token 都没有时切在之后的第一个标识符边界。
    /// JSON 的窗口切在最后一个逗号之后，带 `JsonPad` 解析。与 `FullParse` 一样可以分片：`TimedOut`、`Yielded` 之后
    /// 对同一个对象再调 `step` 接着解析（批量任务在两次之间可以停下、让出线程，对象留在任务里）。
    pub struct ChainWindow { /* 语言、health、src、start、读入和解析的范围、JSON 的补齐、进度 */ }
    impl ChainWindow {
        pub fn new(lang: &Lang, src: Arc<dyn Source>, start: u64) -> ChainWindow;
        pub fn step(&mut self, budget: Budget<'_>) -> Outcome<(WindowSpans, u64)>;
        /// 这个窗口到目前为止的解析用量，交给 `SlowWindows`：各次 `step` 里 tree-sitter 解析的线程 CPU 时间之和
        /// （不含查询、切点和两次之间的等待），以及解析过的字节数。
        pub fn spent(&self) -> Spent;
    }

    /// 窗口的解析用量。只计解析：ADR 0034 的 128 ms 是解析吞吐（不到 0.5 MiB/s）的界限。
    pub struct Spent {
        pub parsed: u64,      // 解析过的字节：这个窗口的，扩到 256 KiB 时加上第一次的
        pub cpu: Duration,    // 解析时这个线程的 CPU 时间（health 的同一套时钟，`platform::ThreadCpuClock`）
        pub wall: Duration,   // 读不到 CPU 时钟的那些 step 的墙上时间
    }

    /// ADR 0034 的降级：一个窗口的解析按每 64 KiB 计用了超过 `SLOW_WINDOW`（128 ms）的 CPU 时间
    /// （读不到 CPU 时钟时按墙上时间、对照 `SLOW_WINDOW_WALL`），连续两次，这个文件本次打开不再高亮。
    /// 批量线程在 utility QoS 下被并发的构建挤掉时 CPU 时间不增长，所以不会因此降级。调用方每完成一个窗口记一次。
    pub struct SlowWindows { /* 连续的慢窗口数 */ }
    impl SlowWindows {
        /// 一个完成的窗口的 `spent`；返回 true 之后一直是纯文本。
        pub fn record(&mut self, spent: Spent) -> bool;
        pub fn plain(&self) -> bool;
    }

    /// 按字节区间读文本，不要求连续存放（分块、页）。`Vec<u8>` 实现它（测试、基准、对象库的整份内容）。
    pub trait Source: Send + Sync {
        fn len(&self) -> u64;
        fn is_empty(&self) -> bool { self.len() == 0 }
        /// `at` 起的一段连续字节；不在内存里时返回 None（解析按到达文件末尾处理，调用方只对已读入的部分调用）。
        fn chunk(&self, at: u64) -> Option<&[u8]>;
    }
}

// ---- 文本 ----

pub mod text {
    /// 不超过 16 MiB 的完整内容：不可变的 64 KiB 分块，加每 1024 行一个检查点的行索引。
    pub struct Text { /* chunks: Arc<[Arc<[u8]>]>, len, index: LineIndex */ }
    impl Text {
        pub fn from_bytes(bytes: Vec<u8>) -> Text;   // 阻塞（建行索引，16 MiB 约 5 ms），后台线程
        pub fn len(&self) -> u64;
        pub fn index(&self) -> &LineIndex;
    }
    impl highlight::Source for Text { /* … */ }

    pub struct LineIndex { /* 每 1024 行的行首偏移, 长行（> 4 KiB）的位置与每 4 KiB 一个的格数检查点, widest_cells, line_count */ }
    impl LineIndex {
        pub fn line_count(&self) -> u64;
        /// 第 `line` 行的行首：查检查点再向后至多扫 1024 行（约 40 KiB，微秒级）。
        pub fn line_start(&self, text: &Text, line: u64) -> u64;
        pub fn line_of(&self, text: &Text, byte: u64) -> u64;
        pub fn has_long_line(&self) -> bool;
        pub fn widest_cells(&self) -> u32;
    }

    /// 行检查点（ADR 0034）：每 64 KiB 处的行号。按内容 OID 存进 `syntax.lines`，只为 ≥ 1 MiB 的内容写。
    pub struct LineCheckpoints {
        pub len: u64,              // 内容长度：首屏用候选 OID 查到时先与 `OpenFile::meta().size` 比较
        pub lines: Box<[u32]>,     // lines[k] 是字节 k × 64 KiB 处之前的换行数
        pub line_count: u64,
        pub has_long_line: bool,
        pub widest_cells: u32,
    }
    impl lectern_cache::Entry for LineCheckpoints {
        const NAMESPACE: Namespace = Namespace { name: "syntax.lines", version: 1, compression: Compression::None };
        /* 键：Key::new().bytes(oid) */
    }

    /// 超过 16 MiB 的文件：同一个 `Arc<OpenFile>` 上按 64 KiB 的页读，页的 LRU ≤ 8 MiB。
    pub struct Pages { /* … */ }
    impl highlight::Source for Pages { /* 只返回已读入的页 */ }
}

// ---- 打开的文档（领域模型）----

/// 每个进程一个，由 file-reader 插件创建；持有语言表、LRU、根目录的 `SafeRoot`、预取队列。
pub struct Engine { /* … */ }

pub struct EngineDeps {
    pub executor: Executor,
    pub cache: Cache,
    pub watcher: Watcher,
    pub languages: Arc<Languages>,
}

impl Engine {
    /// 不做 IO，不建线程（ADR 0017）。
    pub fn new(deps: EngineDeps) -> Engine;
    /// 插件的 `started` 里调用一次：之后的任务经这个句柄的 `own_with` 提交、取守卫。
    pub fn start(&self, handle: PluginHandle);
    /// 打开一个文档。立即返回，UI 线程可调用；首屏的工作在 `Interactive` 任务里，完成后调用 `notify`
    /// （任意线程；视图在里面只安排一次重绘）。`root` 是 worktree 根或文件夹根（来自项目快照）。
    pub fn open(&self, target: ReadTarget, root: Arc<Path>, viewport: Viewport,
                notify: Arc<dyn Fn() + Send + Sync>) -> DocHandle;
    /// 预取（`Reader::prefetch` 转到这里）。立即返回。
    pub fn prefetch(&self, targets: Vec<(ReadTarget, Arc<Path>)>);
    /// 项目关闭：丢弃它的 `SafeRoot`、`Repo` 和订阅。正在编辑、有未保存改动的文档不丢，等用户放弃（见「编辑」的离开守卫）。
    pub fn close_project(&self, project: &ProjectKey);
}

pub struct Viewport { pub rows: u32, pub cols: u32 }

/// 一个打开的文档。drop 时取消它的全部任务、释放 `OpenFile` 和订阅；区间和 ≤ 1 MiB 的文本留在 LRU 里。
pub struct DocHandle { /* Arc<DocState> */ }

impl DocHandle {
    /// 当前快照：取锁复制一个 `Arc`，不阻塞，UI 线程上每帧可调用。
    pub fn snapshot(&self) -> Arc<DocSnapshot>;
    /// 以下都是命令：立即返回，结果出现在之后的快照里。
    pub fn set_viewport(&self, viewport: Viewport);
    /// 视图滚到的位置。档位 B 据此重排「可见窗口和前后各一个」，档位 C 据此读页。
    pub fn scrolled(&self, top: RowPos);
    pub fn find(&self, query: FindQuery);
    pub fn clear_find(&self);
    /// 进入编辑（只对 `editable == Yes`）。返回编辑会话，见「编辑」。
    pub fn edit(&self) -> Result<EditSession, NotEditable>;
}

/// 视图读取的不可变快照。所有方法只做与请求的行数成正比的工作。
pub struct DocSnapshot {
    pub generation: u64,
    pub target: ReadTarget,
    pub state: DocState,
    pub size: u64,
    /// 读到的内容的 OID（原始字节按 git blob 格式求得）；完整读取或流式扫描之前为 None。
    pub oid: Option<BlobId>,
    pub tier: Option<Tier>,
    pub highlight: Highlighting,
    pub lines: LineCount,
    pub widest_cells: u32,
    pub editable: Editable,
    /// 视图滚到的初始位置（打开、重新定位时变化，`generation` 随之加一）。
    pub anchor: RowPos,
    pub find: Option<Arc<FindResults>>,
}

pub enum DocState {
    /// 首屏还没读到：视图按 100 ms 规则显示骨架。
    Opening,
    /// 有内容可画（首屏、或完整内容）。
    Ready,
    /// 最后一级是符号链接：显示链接目标，不跟随（与 `SafeRoot::open_file` 相同）。
    Symlink { target: Arc<[u8]> },
    /// 目录、FIFO 等不读内容的类型。
    Unsupported,
    Missing,
    /// 读不出（权限、路径中间是符号链接、依赖根越界、对象库里没有这个 blob、超过对象库的 16 MiB 上限）。
    Unreadable(ReadError),
}

pub enum Tier { A, B, C }
pub enum Highlighting { Pending, On, Plain }
pub enum LineCount { Estimated(u64), Exact(u64) }
pub enum Editable { Yes, No(NotEditable) }
pub enum NotEditable { NotLoaded, TooLarge, LongLine, NotText, MixedLineEndings, ReadOnlySource, Reloading }

/// 显示行的位置：所在行的行首字节、行内第几个显示行（只有超过 4 KiB 的行才有多个）。滚动位置是它加像素偏移。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RowPos { pub line_start: u64, pub sub_row: u32 }

impl DocSnapshot {
    /// 从 `from` 起至多 `n` 个显示行，长行按 `cols` 个字符格折行。内容不在内存里的行（档位 C 还没读到的页）
    /// 为 `Row::Pending`，`scrolled` 会让它们被读入。
    pub fn rows(&self, from: RowPos, n: usize, cols: u32) -> Vec<Row<'_>>;
    /// 从 `from` 前进或后退 `delta` 个显示行（滚轮、键盘）。
    pub fn advance(&self, from: RowPos, delta: i64, cols: u32) -> RowPos;
    /// 滚动条拖动：按字节比例定位，落到所在行的行首。
    pub fn at_fraction(&self, fraction: f64) -> RowPos;
    /// 估计的总显示行数（滚动条用）：行数加上已知长行多出的行。
    pub fn total_rows(&self, cols: u32) -> u64;
}

pub enum Row<'a> {
    Text {
        pos: RowPos,
        /// 行号从 0 起；`Estimated` 时行号栏显示估计值，后台扫描后改正。
        line: LineNo,
        /// 这个显示行的字节在文件里的区间。
        bytes: Range<u64>,
        /// 不是有效 UTF-8 的字节显示为 U+FFFD，控制字符显示为 U+2400 段的符号；行尾的 `\r` 不显示。
        text: Cow<'a, str>,
        /// 相对 `text` 的区间；没有高亮时为空。
        tokens: Cow<'a, [Token]>,
        /// 与这一行相交的查找结果（`FindResults::ranges` 的一段，文件里的字节区间），视图换算到行内。
        matches: &'a [Range<u64>],
    },
    Pending { pos: RowPos },
}

pub enum LineNo { Exact(u64), Estimated(u64) }

pub struct FindQuery { pub text: Arc<str> }       // ASCII 字母不区分大小写，其余按字节相等
pub struct FindResults { pub ranges: Arc<[Range<u64>]>, pub done: bool, pub capped: bool } // 至多 10,000 个

// ---- 编辑（见「编辑」）----

/// 编辑会话。持有进入编辑时的内容（三方合并的基准）和一棵语法树；drop 时释放树。
pub struct EditSession { /* … */ }

impl EditSession {
    /// 进入编辑时的文本（BOM 已去掉，CRLF 已换成 LF），交给 `InputState::set_value`。
    pub fn initial_text(&self) -> Arc<str>;
    /// 缓冲区变了：新文本的快照和 tree-sitter 的编辑描述。立即返回；增量解析在 `Interactive` 任务里。
    pub fn changed(&self, text: Arc<str>, edits: &[TextEdit]);
    /// 最新的区间（编辑到达但还没重新解析时，旧区间按编辑平移）。UI 线程上读，不解析。
    pub fn spans(&self) -> Arc<WindowSpans>;
    /// 保存：恢复 BOM 和原来的换行，经 `SafeRoot::replace_file` 原子替换，期望的身份是上次读取或保存时的。
    /// 磁盘上的文件已被别人改过时不写，返回 `SaveResult::Conflict`。立即返回，结果经 `notify` 交给视图。
    pub fn save(&self, text: Arc<str>);
    /// 用户选了「强制保存」：不比较身份，照常原子替换。
    pub fn save_anyway(&self, text: Arc<str>);
    /// 三方合并：基准是进入编辑（或上次保存、合并）时的内容，一方是缓冲区，另一方是磁盘上的当前内容。
    pub fn merge(&self, ours: Arc<str>);
    pub fn state(&self) -> Arc<EditState>;
}

pub struct EditState {
    pub dirty_on_disk: bool,                      // 收到事件、磁盘内容与基准不同
    pub save: Option<SaveResult>,
    pub merged: Option<Arc<MergeOutcome>>,         // 合并结果交给视图写回缓冲区
}
pub enum SaveResult { Saved, Conflict, Failed(Arc<str>) }
pub struct MergeOutcome { pub text: Arc<str>, pub conflicts: Arc<[Range<usize>]> }

// ---- grammar 的卡死检测（ADR 0041）----

pub mod health {
    /// 进程里唯一的一份。`code_intel` 也用它（见「对其他 crate 的要求」）。
    pub fn global() -> &'static Health;
    /// 在途表：每一项是一次正在某个线程上运行的解析调用（语言、grammar 版本、这次调用的预算、
    /// 这个线程的 CPU 时钟和登记时的读数）。只有正在运行的调用在表里：分片返回、让路停下、
    /// code_intel 的 `Parked` 都先注销，续做时重新登记。
    pub struct Health { /* … */ }
    impl Health {
        /// 这个 grammar 版本本次运行是否仍可用。不阻塞。
        pub fn allows(&self, lang: &str, grammar_version: u64) -> bool;
        /// 在调用解析的线程上、紧挨着进入 tree-sitter 之前登记，`budget` 是这一次调用剩下的预算
        /// （续做时是剩余的那部分，不是最初的截止时刻）。返回的守卫在调用返回时 drop（任何结果都一样），
        /// 不得跨过 `await`、跨过停下或带到别的线程。登记时检查同一 grammar 版本的在途项：
        /// 有一项的线程 CPU 时间自登记以来超过它的 `budget` 加 `STUCK_GRACE`，就把这个版本记为停用、返回 None。
        pub fn enter(&self, lang: &str, grammar_version: u64, budget: Duration) -> Option<ParseGuard>;
    }
    /// 不可 `Send`：只在登记它的线程上存在。
    pub struct ParseGuard { /* … */ }
}

pub mod limits {
    pub const TIER_A_MAX: u64 = 1 << 20;          // 1 MiB：整文件解析、可以编辑
    pub const LONG_LINE: u64 = 4 << 10;           // 4 KiB：长行，折成显示行；有这样的行就不是档位 A
    pub const TIER_B_MAX: u64 = 16 << 20;         // 16 MiB：整份读入；更大的按页读
    pub const FIRST_WINDOW_DEADLINE: Duration = Duration::from_millis(8);
    pub const SLICE: Duration = Duration::from_millis(8);
    pub const SLOW_WINDOW: Duration = Duration::from_millis(128);   // 线程 CPU 时间，连续两次：这个文件本次不高亮
    pub const SLOW_WINDOW_WALL: Duration = Duration::from_secs(1);  // 读不到 CPU 时钟、按墙上时间计时的上限
    pub const WINDOW: u64 = 64 << 10;
    pub const WINDOW_MAX: u64 = 256 << 10;
    pub const PAGE: u64 = 64 << 10;
    pub const PAGE_LRU: u64 = 8 << 20;
    pub const LRU: u64 = 8 << 20;
    pub const CHECKPOINT_STRIDE: u64 = 64 << 10;
    pub const CHECKPOINT_MIN: u64 = 1 << 20;      // 小于 1 MiB 的内容不写检查点
    pub const OBJECT_MAX: u64 = 16 << 20;         // 对象库里的 blob 整份取出的上限
    pub const OBJECT_INLINE: u64 = 1 << 20;       // 没有人持有时，首屏路径上直接解压的上限
    pub const PREFETCH_MAX: usize = 4;
    pub const STUCK_GRACE: Duration = Duration::from_secs(1);
    pub const STUCK_GRACE_WALL: Duration = Duration::from_secs(10);   // 读不到线程 CPU 时钟、按墙上时间计时的宽限
    pub const HISTORY_MAX: usize = 100;
    pub const FIND_MAX: usize = 10_000;
}
```

常量的来源与校准状态：1 MiB、16 MiB、128 ms 取自[大文件调研第 3 节](../research/large-files-rendering.md#3-大文件的解析与高亮)在 Linux 容器上的实测，4 KiB 是估计值，8 ms 是一帧的一半；都在 M 系列上由「性能」里的基准校准，校准结果改动常量时在同一个 PR 里改本节。`STUCK_GRACE` 是窗口解析实测最差值（15 ms）和 `SLOW_WINDOW` 的数倍；它按线程的 CPU 时间计（`platform::ThreadCpuClock`，见「对其他 crate 的要求」），线程被挤掉、在 utility QoS 下等不到 CPU 时不增长，所以并发的构建不会让它误判。CPU 时钟读不到时（`ThreadCpuClock` 返回 `Unsupported`）按墙上时间计，宽限放大到 10 s。

### 本 crate 的公开项

```rust
/// `app` 装配时构造，放进 `PluginSet`（id "file-reader"）。
pub struct FileReaderPlugin { /* deps */ }

pub struct FileReaderDeps {
    pub executor: Executor,
    pub cache: Cache,
    pub watcher: Watcher,
}

impl FileReaderPlugin {
    /// 不做 IO。
    pub fn new(deps: FileReaderDeps) -> Self;
}

// Manifest { provides: [service::<dyn Reader>()],
//            requires: [service::<dyn Projects>(), service::<dyn CodeIntel>(), service::<dyn Contributions>()],
//            activation: [AfterFirstFrame] }
// activate：建 Engine（不做 IO），provide `dyn Reader`；`ctx.own_staged` 交出界面注册（侧栏的 Files、阅读视图、Action 与快捷键）。
// started：`Engine::start(handle)`，订阅 `ProjectEvent`（项目关闭时 `close_project`，有未保存编辑的文档不随之丢弃，见「编辑」）。
//   经 `dyn Contributions` 登记离开守卫（`register_leave_guard`）。不提交任何任务。
```

`dyn Reader::open` 把目标排到主线程：在 `token.enter()` 的准入内找到本窗口的阅读视图（没有时经 `dyn Contributions` 切到它），先过离开守卫（「编辑」），通过之后调 `Engine::open`，把上一个文档的位置压进后退历史，发出 `ReaderEvent::Open { outcome: Shown }`；守卫被取消时发出 `Cancelled`，什么都不换。然后 `CodeIntel::set_focus(Current(Some(TextRef)))`，`TextRef` 在快照的 `oid` 有值之后才设（见「数据流与线程」）。

## 数据结构

### 档位与降级

| 档位 | 条件 | 文本 | 首屏之后 | 守卫与线程 |
|---|---|---|---|---|
| A | ≤ 1 MiB，没有超过 4 KiB 的行 | 首屏一次读完（一段 `read_ranges`） | `FullParse`，8 ms 一片，完成后用整文件的区间替换首屏的，释放语法树 | `BusyGuard`、`Interactive`（1 MiB 约 165 ms，Linux） |
| B | 1–16 MiB，或有超过 4 KiB 的行 | 首屏两段，之后同一个 `OpenFile` 上 `read_all(16 MiB)` 成为 `Text` | 窗口链从文件开头顺序计算，记下每个窗口的起点（每个 8 字节）；区间只为可见附近的窗口留在 LRU 里 | 可见窗口和前后各一个：`BusyGuard`、`Interactive`；其余：`background_yielding()`、`Batch`（ADR 0033 的只存内存的一类），同一时刻至多一个任务、一个任务至多一段窗口（「批量线程上的先后」） |
| C | > 16 MiB | `Pages`，页 LRU ≤ 8 MiB | 只为可见窗口前后各一个算区间，随滚动按需算；窗口起点的顺序计算同档位 B 的「其余」，算到之前用启发式的安全位置 | 同档位 B |
| 纯文本 | 二进制（文件头 8 KiB 或窗口里有 NUL）、不是 UTF-8、没有这种语言、语言停用（`health`），或一个 64 KiB 的窗口解析连续两次超过 128 ms | 同所在档位 | 不高亮，照常滚动、查找；档位 A 的纯文本文件（不是 `NotText`）仍可编辑 | — |

- 档位在首屏只按 `size` 粗分（≤ 1 MiB 为 A 的候选），「有没有超过 4 KiB 的行」在完整读取或扫描之后才确定：首屏不需要它（ADR 0034「首屏路径的准备工作」）。档位 A 的候选发现长行时改为 B，已算出的首屏区间保留。
- 窗口链按 `(oid, lang, grammar, query)` 在 LRU 里找已有的窗口起点表，有就从断开处继续；起点表本身也是 LRU 的一类条目（见「LRU」，16 MiB 约 2 KiB）。
- 降级不提示（[大文件调研「决定」](../research/large-files-rendering.md#决定)第 3 条）。

### 定位

打开时的目标按以下顺序换成字节偏移 `t`，再走首屏：

| 目标 | 首屏读取 | 说明 |
|---|---|---|
| `Start` | `first_read(size, 0)` | 一段或两段合并为一次 `pread` |
| `Byte` | `first_read(size, byte)` | 直接用；完整读取之后若读到内容的 OID 与 `expect` 不同，按 `line` 重新定位 |
| `Line`，≤ 1 MiB | `first_read(size, 0)`（整个文件） | 在读到的内容里数换行 |
| `Line`，> 1 MiB，有检查点 | `checkpoint_read(size, c)` | 候选 OID 查 `syntax.lines`：工作区文件取 `FileList` 里这个路径的 OID（不做 stat 比较），对象库和依赖文件取 `expect`；值里的 `len` 与 `meta.size` 不同时不用 |
| `Line`，> 1 MiB，没有检查点 | `first_read(size, 0)` | 先显示文件开头；完整读取（≤ 16 MiB）或流式扫描（> 16 MiB）到那一行后滚过去，不在 16 ms 的保证里（ADR 0034「预算」） |

- 上表的读取针对 `OpenFile`（工作区文件、依赖文件）。对象库的内容（`TextOrigin::Object`）不能按区间读，按以下顺序取得，之后按同样的规则在内存里定位；它的 OID 就是 `expect`，不需要核对：
  1. 有人持有：`Reader::share_object` 登记着的内容，直接用，首屏不做 IO。审阅视图显示一个文件的 diff 时为两侧的文本（`ChangeSet::texts`，它本来就持有）各登记一次、diff 不再显示时 drop 登记，所以从 diff 的旧一侧打开（⇧⌘O、hunk）、从旧一侧发起的跳转候选（抽屉）打开的都是这一类，首屏与工作区文件同样在 16 ms 之内（`app/open_file` 的 Object 场景）。登记表里的内容都是审阅视图同时持有的那几份，在它之外不另占内存：工作区一侧的内容在 ADR 0035 的「打开 Diff 时改动集保留的工作区内容 ≤ 32 MiB」之内，旧一侧来自对象库（`ChangeSet::texts`），不在那个上限里，由审阅视图自己的设计约束它持有多少。没有 Diff 显示时（包括 ADR 0035 的采样状态）登记表是空的。
  2. LRU：≤ 1 MiB 的内容在 LRU 的文本条目里（8 MiB 上限之内）。后退历史只记 OID、来源和位置，不持有内容。
  3. 都没有：≤ 1 MiB（`OBJECT_INLINE`）的 blob 在首屏任务里 `Repo::blob` 解压（约 1–3 ms，`syntax/first_screen/object_1mib`），仍在 16 ms 之内；更大的要等整份解压，不在 16 ms 的保证里，这是 [ADR 0042](../adr/0042.md) 对 ADR 0034「预算」的修订。现有的入口都落在第 1 类里，只有 Diff 关闭之后的后退、前进会走到这里。
  超过 16 MiB（`OBJECT_MAX`）而又没有人持有的不打开。
- 偏移的口径：本 crate 显示和编辑磁盘上的原始字节。`git` 的 `*_byte` 和 `Location::byte` 按规范内容计，两者只在原始字节就是规范内容时相同；所以 `Byte` 先按偏移打开，完整读取或扫描算出原始字节的 OID 之后与 `expect` 比较，不同（CRLF、`working-tree-encoding`、文件已改）时按行号重新定位，`anchor` 换代，视图滚到新位置（「已知限制」）。
- 行检查点的键是原始字节的 OID；需要规范化转换的文件，`FileList` 的 OID 查不到它，首屏退回文件开头（「已知限制」）。
- 文件夹项目没有 `FileList` 的 OID：本次运行内按 `(路径, FileIdentity)` 记下扫描得到的 OID（内存里，与文档一起最多 64 项），之后的打开才能用检查点。

### 文本与显示行

- 换行只认 `\n`；行尾的 `\r` 属于这一行，不显示。BOM 计入偏移，不显示。
- 长行（> 4 KiB）按字符格数折成显示行：`unicode-width` 的宽度，制表符按到下一个 4 的倍数计，不测量字形。完整内容或扫描之后，行索引为每个长行记下每 4 KiB 一个（字节偏移、累计格数），所以在任意长行里找第 k 个显示行是一次二分加至多 4 KiB 的扫描，与行长无关。完整读取或扫描之前没有格数检查点：长行里的显示行从首屏窗口的起点开始折，扫描后按行首重新对齐（这一行的折行位置会变一次）。
- 滚动位置是 `RowPos` 加像素偏移，不是整份内容的像素高度（GPUI 的 f32 坐标在约 93 万行之后不精确，ADR 0034）。滚动条的总高度用 `total_rows` 估计。
- 横向滚动的宽度取 `widest_cells`（扫描时记下），不量全部行；首屏之前取可见行的最大值。

### LRU

| 条目 | 键 | 内容 | 淘汰的先后 |
|---|---|---|---|
| 区间 | (OID、语言、grammar 版本、查询版本、窗口起点；整文件为 `u64::MAX`) | `Arc<WindowSpans>` | 预取的和档位 B、C 的批量窗口先于用户打开过的（ADR 0033） |
| 文本 | OID | ≤ 1 MiB 的 `Arc<Text>`（后退、前进不再读文件） | 同上 |
| 窗口起点表 | (OID、语言、grammar 版本、查询版本) | `Arc<[u64]>` | 同上 |

- 合计 ≤ 8 MiB，按条目的 `bytes()` 计。这是 ADR 0030 和 ADR 0035 里的「`file_reader` 的 8 MB 高亮与文本 LRU」：只因阅读才填充，不在 ADR 0035 的采样状态里。
- OID 还不知道时（首屏到扫描完成之间），区间只属于这个文档，不进 LRU；OID 有了之后一次放进去。
- 正在打开的文档自己持有它用到的条目的 `Arc`，LRU 淘汰不影响正在显示的内容。

### 目录表

- 每个目录一项：名字在 `names` 里的区间、父目录、子目录在 `subdirs` 里的区间、文件在 `files` 里的区间、子树文件数。`subdirs`、`files` 是按 `finder_cmp` 排好的下标数组。
- 一遍扫描 `FileList`（已按字节序，同一目录下的文件连续）得出目录和区间，再逐个目录排序；10 万个文件、约 1 万个目录时估算 5–20 ms、约 0.7 MB（每个文件 4 字节的下标，每个目录约 24 字节加名字）。实现与实测见「与设计的差异」前置条件 B。
- 子模块（`FileKind::Submodule`）是一个文件行，不展开。

## 数据流与线程

```
dyn Reader::open ─▶ [主线程] 阅读视图 ─▶ Engine::open
   ─▶ [Interactive + BusyGuard] SafeRoot::open_file → read_ranges → window::plan → highlight::window（8 ms）─▶ 快照（首屏）
   ─▶ [Background + BusyGuard] read_all / 流式扫描：BlobHasher、行索引、行检查点 → syntax.lines ─▶ 快照（完整、OID）
   ─▶ 档位 A：[Interactive + BusyGuard] FullParse 8 ms 一片 ─▶ 快照（整文件区间）
      档位 B/C：可见窗口 ±1 [Interactive + BusyGuard]；其余 [Batch + background_yielding，一个任务至多一段窗口] ─▶ LRU ─▶ 快照
Reader::prefetch ─▶ [Batch + background_yielding，至多一个] ─▶ LRU
WorktreeBatch（这个路径）─▶ [Background + BusyGuard] stat 比较身份 ─▶ 重新打开（阅读）或标记 dirty_on_disk（编辑）
```

| 工作 | 守卫 | 优先级 |
|---|---|---|
| 首屏：打开、首屏读取、`plan`、窗口解析、可见行的查询 | `BusyGuard` | `Interactive` |
| 档位 A 的整文件解析；档位 B、C 可见窗口和前后各一个；档位 C 可见页的读取 | `BusyGuard` | `Interactive` |
| 完整读取（≤ 16 MiB）、求 OID、行索引、写 `syntax.lines`；档位 C 的流式扫描（每次 1 MiB 的 `read_ranges`，块之间检查取消） | `BusyGuard` | `Background` |
| 工作区文件的规范内容 OID（原始字节的 OID 与 `FileList` 的不同时调 `Worktree::canonical_oid`，给 `TextRef`） | `BusyGuard` | `Background` |
| 档位 B、C 的其余窗口 | `background_yielding()` | `Batch`，同一时刻至多一个任务；一个任务至多一个窗口、50 ms 的解析，之后重新入队，排在已在队列里的批量工作之后 |
| 空闲预取（≤ 4 个文件；≤ 1 MiB 的算整文件，更大的只算第一屏的窗口） | `background_yielding()` | `Batch`，同一时刻至多一个任务，一个任务做一个文件 |
| 查找（按 1 MiB 分块，块之间检查取消；新的查询取消旧的） | `BusyGuard` | `Interactive` |
| 编辑：增量解析、保存、三方合并 | `BusyGuard` | `Interactive` |
| `fs_watch` 事件：stat 比较、触发重新打开 | `BusyGuard`（`Sink::busy`） | `Background` |

- UI 线程只调用 `Engine::open`、`DocHandle` 的命令和 `snapshot`、`EditSession::changed` / `spans`：入队、取锁复制 `Arc`，不做 IO、解析和 git 操作（AGENTS.md「禁止事项」）。`DocSnapshot::rows` 只做与请求行数成正比的工作：分块里取字节、按区间切片、长行用格数检查点。
- 档位 B、C 的批量窗口满足 ADR 0033「两类只存内存的批量工作」：持有让路的 `BackgroundGuard`、只在批量线程上、结果只进 LRU，暂停、取消或退出最多丢掉在途的一个窗口。解析的进度回调读 `yield_requested()`，`ChainWindow::step` 返回 `Yielded`、保留解析器的状态（大文件调研第 3 节「分片解析与恢复」），任务在 `proceed_item` 上等，下一次从停下处继续。
- grammar 的卡死检测：每次 `step`、`window` 调用在进入 tree-sitter 之前 `health::global().enter`，返回之前 drop 守卫；`Yielded`、`TimedOut` 之后等待保活时段或下一片时不在表里，续做时以剩余预算重新登记。登记得到 None（这个 grammar 版本已停用）时这个文档改为纯文本。
- `OpenFile` 的持有时间（platform 设计「对其他 crate 的影响」）：≤ 16 MiB 的在完整读取之后 drop；> 16 MiB 的到文档关闭或收到这个路径的事件为止，收到事件时重新打开。完整读取的身份与首屏的不同时，首屏结果作废，按完整内容重新定位到同一行号。
- 根目录：`Engine` 按根路径保留一个 `Arc<SafeRoot>`（工作区根经 `SafeRoot::open`，与 `code_intel` 相同），项目关闭时释放。对象库：每个打开的 git 项目保留一个 `git::Repo`（不持有 gix 句柄），`Repo::blob(oid, OBJECT_MAX)` 整份取出。依赖根：`DepRoot::Worktree { dir }` 经工作区根的 `SafeRoot::open_dir(dir)` 打开，路径必须以 `dir` 开头，扩展名必须在 ADR 0033 的允许列表里（共用 `services::code_intel::DEPENDENCY_EXTENSIONS`），否则 `Unreadable`。
- 监听：有文档打开时，为它所在的 worktree 订阅一份 `watch_repo` + `watch_worktree`（`Coalesce::WORKTREE`，`source` 用 `git::Worktree::watch_source`，与 `project`、`code_intel` 共用同一份状态），文档关闭时 drop。`WorktreeBatch` 里有这个路径（或它的上级目录）、或 `rescan` 时，`SafeRoot::stat` 比较身份；保存后自己那次写入的身份不算变化。
- 派生数据的键：区间按 (OID、语言、grammar 版本、查询版本、窗口起点)，行检查点按 OID（架构规则 5，[#42](https://github.com/SoloJiang/lectern-app/issues/42) 的「grammar 版本进键」）。grammar 或查询升级后旧条目不再命中。
- `code_intel` 的焦点：快照的 `oid` 有值后，工作区文件的 `TextRef::oid` 取规范内容的 OID（原始 OID 等于 `FileList` 的就是它，否则 `canonical_oid`，有外部过滤器时不设焦点），对象库取 `expect`，依赖取原始 OID；文档关闭时 `set_focus(Current(None))`。指针在标识符上停留 150 ms 后调 `prefetch(Definition)`。
- 首帧之前不做任何事：插件 `AfterFirstFrame` 激活，`activate` 不加载 grammar、不编译查询、不开 `SafeRoot`；第一次 `open` 时才编译这种语言的查询。

### 批量线程上的先后

`executor` 只有一个先进先出的 `Batch` 队列。`code_intel` 每个批量线程上有一个长期任务，它在每个批次边界（`Step::Worked` 之后）调用 `yield_now()`：唤醒自己、返回一次 `Pending`，所以每一批之后都重新入队、排到队尾（已实现，`crates/code_intel/src/index/batch.rs` 的批量循环；本设计要求的测试已补，见「对其他 crate 的要求」）。`syntax` 的批量工作都用普通的 `Batch`，不需要新的优先级：

- 空闲预取：同一时刻至多一个任务，一个任务做一个文件（≤ 1 MiB 的整文件，或更大的文件的第一屏窗口），每次进入交互静止至多 4 个。
- 档位 B、C 的批量窗口：同一时刻至多一个任务；一个任务至多做一个窗口、至多 50 ms 的解析（`ChainWindow::step` 的预算），然后结束，下一段重新入队。它与 `code_intel` 的批次轮流使用批量线程，不会让 64 MiB 文件的约 1000 个窗口（Linux 11–18 s）整段挡在建索引前面。
- 等待的上限：批量队列和批量线程上至多有 N 个 `code_intel` 任务（N 是批量线程数）、一个窗口任务和一个预取任务。预取入队时排在它前面的，只有已在队列里的 `code_intel` 任务和至多一个窗口任务；正在运行的 `code_intel` 任务做完这一批后排到它后面。一批由三段组成（[code_intel 设计「调度、批次与取消」](code_intel.md#调度批次与取消)）：抽取的循环在开始 50 ms 之后的第一个文件边界结束，那个文件至多运行到后台的单文件截止时间 250 ms，截止由看门狗每 10 ms 检查一次，所以这一段 ≤ 约 310 ms；之后是一次提交：读到的 OID 与排队时不同的文件做一次 `canonical_oids`（至多解析一次 index，10 万文件约 22 ms），再一个写事务（`cache/tables_write` < 15 ms）。一批因此约 350 ms，窗口任务一次 ≤ 50 ms。预取至多等每个批量线程上的一批加上一个窗口任务：名义上约 400 ms。窗口任务的等待相同。这些是估计的上界，没有在 M 系列上测过，`syntax/prefetch_wait` 按 p95 ≤ 500 ms 判定（名义值加 25% 的余量），最大值只记录：共享的 CI 机器上偶尔一次调度延迟不让它失败。在 `LargeSlot::acquire` 上阻塞的等待方不在队列里，不改变这个上限（下一条）。
- 与 [ADR 0033](../adr/0033.md)「空闲预取排在第 3 层之前」：这里是近似地做到，ADR 0041 把它改写为上面的上限。预取不插到已经入队的第 3 层批次前面；它至多等每个线程正在运行的一批（约 350 ms，含提交）和一个窗口任务，期间第 3 层至多前进这一批。理由：预取在交互静止（300 ms 没有输入）时才发出，算的是用户下一步可能要看的文件，晚几百毫秒开始通常仍在用户的下一个动作之前；严格的先后要给执行器加一级优先级（ADR 0041「放弃的方案」），而这一点等待不值得。
- 与 `code_intel` 的 `LargeSlot`（超过 256 KiB 的一项同时只在一个批量任务里）：
  - 交还线程不要求放下 `LargeSlot`。让路时停下的超过 256 KiB 的一项连同它的 `LargeTurn` 留在任务的 `Parked` 里，从一个批次边界带到下一个（[code_intel 设计「交互让路」](code_intel.md#交互让路)），重新入队的任务照样带着它，状态不丢，同一时刻也仍只有一棵大树。
  - 不让路的请求方（窗口链、第 3 层的大文件）照旧当场接手停下的持有者（`LargeSlot::acquire` 的 `near` 一支：丢掉 `LargeTurn::park` 登记的状态、作废那一轮），不需要持有者运行，与它是否在队列里无关。
  - 让路的请求方在 `LargeSlot::acquire` 里阻塞自己的线程，等持有者交还（每 10 ms 检查一次停止）。活性：`code_intel` 的批量任务数不超过批量线程数 N，持有者是其中之一，所以阻塞的等待方至多占 N − 1 个线程，至少还有一个线程不被等待方占着。这个线程上运行的只可能是：没有在等的 `code_intel` 任务（一批之后重新入队，排在已在队列里的持有者之后）、`syntax` 的任务（从不等 `LargeSlot`、不阻塞，同一时刻至多一个窗口任务和一个预取任务，一段窗口或一个文件就结束，之后重新入队、排在持有者之后），或者持有者本身。所以持有者在有限的几项之后就会被取到。持有者被取到后若开关仍开着，它在 `proceed_item` 上返回 `Pending`、不占线程，等开关关闭才继续，与交还线程之前相同。
  - 重新入队不增加任务数，「批量任务数不多于批量线程数」这一条照旧成立。
- `syntax` 的任务不取 `LargeSlot`：同一时刻至多一个窗口任务和一个预取任务，64 KiB 窗口的语法树 2–5 MiB（`syntax/window_peak` 在这种窗口上判定 ≤ 8 MiB），找不到切点、扩到 256 KiB 的最多约 21 MiB（大文件调研实测，压缩 JS），与 `code_intel` 的 `LargeSlot` 内存各自有上限。

### 编辑

- 进入（⌘E / Edit…）：`editable == Yes` 才可用，即档位 A、完整内容已装入、内容是 UTF-8、换行全是 LF 或全是 CRLF、来源是 `Worktree`。`EditSession` 新建一个 `Parser` 并解析整个文本一次（`Interactive`，档位 A 的上限处约 165 ms，Linux；期间先用阅读视图已有的整文件区间）。视图把 `initial_text` 交给 `InputState`（代码模式，不开软换行），高亮经本 crate 的 `InputHighlighter` 适配器读 `spans()`。
- 输入：适配器在每帧至多一次把缓冲区复制成 `Arc<str>`（档位 A 的上限处约 0.1 ms）连同累计的编辑交给 `changed`；增量解析在后台，换输入时 `Parser::reset`（大文件调研第 3 节）。`spans()` 在 UI 线程上只读结果。
- 保存（⌘S）：`replace_file(rel, bytes, expect)` 在同一目录里写临时文件、`fsync`、比较当前身份、`renameat` 替换（见「对其他 crate 的要求」）。身份不同（agent 在这期间写过）时不写，`SaveResult::Conflict`，视图显示「磁盘上的文件已改动」的一行和三个按钮（见「界面」）。
- 磁盘上的文件变了：缓冲区没有改动时静默重新载入（光标按行号保留）；有改动时只标记 `dirty_on_disk`、显示同一行提示，不自动覆盖任何一方（[交互规范「状态与异常」](interaction.md#状态与异常)）。
- 合并：`git::merge3(base, ours, theirs)`，基准是进入编辑（或上次保存、合并）时的内容。没有冲突时结果写回缓冲区，仍是未保存；有冲突时冲突段带标记写回（`<<<<<<< editor`、`=======`、`>>>>>>> disk`，它们是文件内容、不走 i18n），视图滚到第一个冲突，用户改完再保存。
- 快捷键：编辑时焦点在 `InputState` 上，⌘[ / ⌘] 归 gpui-component 在 `Input` 上下文里的绑定（⌘[ 是 Outdent，⌘] 是 Indent，gpui-base 0.7.3 `src/input/base/state.rs`），后退 / 前进不可用（按钮置灰，`file_reader::Back` / `Forward` 只绑定在不编辑时的阅读视图上下文里）；Esc 先交给 `InputState`（关闭它的查找、取消选区），没有可关闭的才算「离开编辑」。

**离开守卫**

编辑模式是一个受守卫的状态：缓冲区有未保存的改动（视图记下的「上次载入或保存之后改过」）时，任何会替换或丢掉这个文档的操作都先经同一个守卫，不直接 drop `EditSession`。守卫在阅读视图里（本 crate 的 `edit::LeaveGuard`），每个窗口一个。

| 触发 | 来自 | 守卫之前的处理 |
|---|---|---|
| 退出编辑（⌘E、Done、Esc） | 本 crate | — |
| 打开另一个文件（`Reader::open`：审阅、抽屉、⌘K、Files） | 本 crate 的服务 | 请求排队，守卫通过后才换文档；取消时这次请求作废，`ReaderEvent::Open { Cancelled }` |
| 后退、前进 | 本 crate | 编辑时不可用（见上）；从工具栏之外触发（探针）时同样经守卫 |
| 关闭阅读视图、回到打开它的视图 | 本 crate、`workspace` 的导航栈 | `workspace` 在弹出视图之前问守卫 |
| 切换当前 worktree、关闭项目 | `worktrees`、`project_home` 等经 `workspace` | `workspace` 在调 `Projects::set_active_worktree` / `close` 之前问守卫 |
| 关闭窗口、退出应用（⌘W、⌘Q 和应用菜单的 Quit） | `workspace`、`app` | 见下；系统发起的退出拦不住 |

- 守卫的结果：没有未保存的改动时立即通过。有时弹出系统的确认（`file-reader-unsaved-*`：保存 / 不保存 / 取消）：保存 → `EditSession::save`，`Saved` 才通过，`Conflict` 或 `Failed` 时不通过（显示提示行，留在编辑里）；不保存 → 通过，drop `EditSession`；取消 → 不通过。
- 接口：`dyn Contributions` 提供 `register_leave_guard(guard: Arc<dyn LeaveGuard>) -> 守卫`（`workspace` #30 的设计，见「对其他 crate 的要求」）。`LeaveGuard::ask(scope: LeaveScope, done: Box<dyn FnOnce(bool)>)` 在主线程上调用，不阻塞，结果经 `done` 交回；`LeaveScope` 是 `View`（关闭或离开阅读视图）、`Project(ProjectKey)`、`Worktree(ProjectKey)`、`Window`、`App`。`workspace` 对一个操作按登记顺序逐个问，都通过才执行；本 crate 只对与这个范围相交、而且有未保存改动的文档回答「问用户」。
- 关闭窗口：`workspace` 在 GPUI 的窗口关闭回调里先返回「不关闭」、问守卫、通过后再关闭。
- 退出：锁定的 gpui-pre-macos 0.3.9 的应用代理只实现了 `applicationWillTerminate:`，`on_quit` 的返回值被忽略，退出一旦开始就不能取消；改 GPUI 要人确认（[ADR 0016](../adr/0016.md)）。所以 `app` 自己定义 `Quit` Action（纯 GPUI 代码，不碰 AppKit），⌘Q 和应用菜单的 Quit 都绑定到它。退出只有这一个顺序：
  1. 经 `workspace` 问 `LeaveScope::App` 的离开守卫；取消时什么都不做，到此为止。
  2. 通过后调用 `Kernel::shutdown(done)`，按依赖的相反顺序停用全部插件（[kernel 设计「内核」](kernel.md#内核)）。等待期间主线程照常运行，需要在主线程上 drop 的守卫不会死锁。
  3. `done` 被调用之后，在后台线程上做两件可选的收尾：`Executor::shutdown(剩余时间)`（调用 `on_shutdown` 登记的回调、取消未结束的任务，[code_intel 设计「调度、批次与取消」](code_intel.md#调度批次与取消)的看门狗靠它退出）；`Cache::close()`（写回访问时间、关闭连接，[cache 设计「接口」](cache.md#接口)，不调用也安全）。
  4. 第 2、3 步合计至多 2 秒：全部完成或到了 2 秒，在主线程上调用 `cx.quit()`。到时还没做完的那一步就不再等，进程退出时随之丢弃。

  由系统发起的退出（注销、关机、Dock 菜单里的退出）不经这个 Action：有未保存的编辑时会丢失，插件也不按顺序停用（「已知限制」，kernel 设计同样记下）；以后要拦住它们，得在 `platform` 里给应用代理加 `applicationShouldTerminate:`，涉及 GPUI 的应用代理，那时另行决定。
- 窗口关闭回调和 `Quit` Action 分别由 `workspace`（#30）和 `app` 提供，是本 crate 编辑模式的前提（「验收」前置条件 E）。
- 守卫管不到的丢失：项目根目录消失、`project` 自己关闭了项目、由系统发起的退出（见上）、进程崩溃。前两种情况本 crate 不 drop 有未保存改动的 `EditSession`，缓冲区留在阅读视图里，提示行说明文件已不可用（`file-reader-unavailable-while-editing`），只能复制内容或放弃；`Engine::close_project` 跳过这样的文档，等用户放弃后再释放。系统发起的退出和崩溃时未保存的改动丢失（「已知限制」：不做自动保存）。
- 退出编辑之后 drop `EditSession`，语法树随之释放（ADR 0034：不计入空闲内存），阅读视图按保存后的内容重新打开同一位置。

## 性能

影响的预算（[性能预算](tech-research.md#性能预算)，M 系列 Mac，系统文件缓存已热）：

| 预算 | 本设计的份额 | 基准 |
|---|---|---|
| 冷启动 20 ms | 首帧之前不做任何事；插件激活只建 `Engine`、登记界面 | `plugin/file-reader/activate`（kernel 的 < 1 ms） |
| 打开文件到首屏高亮 < 16 ms（ADR 0034 的条件：字节偏移、或有检查点、或文件开头） | `syntax` 的首屏（`open_file`、首屏读取、`plan`、窗口解析、可见行的查询）中位数 ≤ 8 ms，最大值由 8 ms 截止时间约束（超时先画文本）；其中首屏读取 < 0.2 ms（`platform/safe_read_range`），`plan` < 0.1 ms。排版和绘制可见的约 60 行 ≤ 6 ms，事件送到 UI 线程 ≤ 2 ms | `syntax/first_screen`（合成 TS、Rust、JSON 1–64 MiB 和压缩成一行的 4 MiB JS，各 40 个随机偏移）、`syntax/first_screen/minified` 只记录；对象库：`syntax/first_screen/object_shared`（4 MiB 的 blob 已经 `share_object`，中部的偏移）判定同样 ≤ 8 ms，`syntax/first_screen/object_1mib`（没有人持有、首屏路径上解压 1 MiB 的 blob）≤ 8 ms，`syntax/first_screen/object_cold`（4 MiB、没有人持有，ADR 0042 的例外）只记录；端到端 `app/open_file`（全部场景，含从 diff 的旧一侧打开 4 MiB blob 中部的一个 hunk） |
| 跳转：唯一候选的目标首屏高亮 ≤ 100 ms | 目标带 `Location::byte`，首屏同上 | `app/goto_single`（`code_intel` 第 5 步） |
| 滚动与输入 120 fps | 渲染只做与可见行数成正比的工作；每行排版按（OID 或文档代数、行首字节、显示行、列宽）的哈希用 `shape_line_by_hash`，本 crate 另留约 4 倍可见行数的最近行缓存；档位 B、C 的窗口在后台算好 | `app/scroll_frames`（含 64 MiB、压缩成一行、编辑模式 1 MiB 的场景）、`app/scroll_frames_indexing` |
| 后退、前进 < 16 ms | 历史记下 OID、身份和字节偏移；回到 ≤ 1 MiB 的文件时 stat 比较身份相同就用 LRU 里的文本和区间，不读文件；更大的文件按字节偏移走首屏。没有人持有的、超过 1 MiB 的对象库内容是 ADR 0042 的例外 | `app/nav_back` |
| 打开 Files、展开目录 < 16 ms | 目录表已建好时，打开只取快照、按展开状态算可见行；展开 2,000 个子项的目录是一次插入 | `app/files_open`、`app/files_expand` |
| 进入编辑 ≤ 100 ms | 1 MiB、没有长行的文件：`InputState::set_value` 与换行信息在 UI 线程上约 3 ms（ADR 0034），首次整文件解析在后台，不等它 | `app/enter_edit` |
| 空闲内存（采样状态：没有打开文件） | 插件常驻 ≤ 0.5 MB：`Engine`、空的 LRU、语言表（未编译的查询）；LRU ≤ 8 MiB、编译过的查询、打开的文档只因阅读才有，不在采样状态里（ADR 0035） | `plugin/file-reader/resident`；`syntax/lru_bytes` |
| 阅读时的内存（不设顶层预算） | 打开的文档：≤ 16 MiB 的文本，或 ≤ 8 MiB 的页；行索引每 1024 行 8 字节加长行的格数检查点；窗口计算的临时内存（一个窗口和它的树）≤ 8 MiB；编辑模式的语法树 ≤ 41 MiB，退出即释放 | `syntax/window_peak` ≤ 8 MiB、`syntax/edit_tree_bytes` ≤ 41 MiB |
| 目录表（`project` 的份额） | 10 万文件 ≤ 20 ms、≤ 1.5 MB，计入 `plugin/project/resident_100k` < 16 MB | `project/dirs_100k`、`project/dirs_100k_bytes` |
| 安装包 ≤ 80 MB | 四个新 grammar 的解析表（`tree-sitter-md` 最大），由 [#38](https://github.com/SoloJiang/lectern-app/issues/38) 测量 | `app/bundle_size` |

批量线程上的等待：`syntax/prefetch_wait`（真实的 `code_intel` 建 10 万文件的索引时，预取从入队到开始）p95 ≤ 500 ms，最大值只记录：每个批量线程上的一批（抽取约 310 ms 加一次提交，约 350 ms）加一个窗口任务（≤ 50 ms），名义约 400 ms，留 25% 的余量，见「批量线程上的先后」。

只记录的基准：`syntax/full_parse_1mib`（档位 A 的整文件解析）、`syntax/chain_16mib`（档位 B 的整条窗口链）、`syntax/scan_64mib`（档位 C 的流式扫描）、`syntax/find_16mib`。

- 以上分项是 `Tier::Share`，实现它们的 PR 把条目加进 [bench 设计「预算表」](bench.md#预算表)和 `crates/bench/src/budgets.rs`，出处指向本节。顶层条目 `app/open_file`、`app/enter_edit`、`app/files_open`、`app/files_expand`、`app/nav_back`、`app/scroll_frames` 已在预算表里列为计划中，由「验收」第 7、8 步改为生效。
- 首屏的 8 + 6 + 2 ms 是从 16 ms 分出的份额：首屏窗口在 Linux 上平均 2.5–3.3 ms（合成源码）、压缩 JS 8–12 ms（大文件调研「首屏路径的准备工作」），M 系列按 1.2–1.7 倍估计。

## 界面

### 阅读视图

- 收起层（[交互规范「三层结构」](interaction.md#三层结构)）：⇧⌘O 从审阅、抽屉、⌘K、Files 打开，占据内容区（不受阅读栏 760pt 的宽度限制，代码按行不折）；Esc 回到打开它的视图。
- 工具栏：标题是文件名，副标题是相对路径（依赖文件另加包名和版本，如 `react 19.3.1`；对象库的内容另加 `file-reader-from-history` 的来源说明）；左侧是后退 / 前进的两段按钮（⌘[ / ⌘]，与审阅页左侧的返回按钮同一位置；编辑时置灰）；右侧是 Edit…，编辑时换成 Done 和 Save，不超过 3 个控件。
- 代码区：SF Mono 12pt，行高由 `ui` 给出；行号栏用 secondaryLabel；颜色只来自 `ui` 的代码配色（`TokenKind` → 系统色），不在视图里定义颜色。
- 查找栏（⌘F）：在代码区上方展开，用 `ui` 的「展开」原语（280 ms 高度、200 ms 透明度）；⌘G / ⇧⌘G 下一处 / 上一处，次要文字显示「3 / 41」。
- 文件在磁盘上变了（只在编辑时）：工具栏下方一行次要文字加三个按钮：Merge、Discard My Changes、Keep Editing；Keep Editing 之后再保存时 Save 变为 Save Anyway。
- 动效只用 `ui` 的原语；减弱动态效果时按系统设置。

### 编辑模式

- gpui-component 的 `InputState` 代码模式，不开软换行，代码补全关闭；高亮经适配器（`edit::Highlighter`，实现 `InputHighlighter`）读 `EditSession::spans()`。
- 不能编辑时 Edit… 置灰，悬停显示原因（[大文件调研「决定」](../research/large-files-rendering.md#决定)第 4 条）。

### Files

- 按需层：项目侧栏的 Files 条目（[交互规范「屏幕」](interaction.md#屏幕) 02）打开，侧栏内容换成文件树，Esc 或返回箭头回到项目侧栏。
- `uniform_list`，每行：展开箭头、SF Symbols 的文件夹或文档图标（经 `ui`）、名字；折叠的目录链显示为 `src/main/java/com/example`。快照是「当前展开状态下的可见行」，每行一个目录或文件下标加缩进层级，展开或折叠只插入或删除这一段（[代码导航调研第 8 节](../research/code-navigation.md#8-文件树浏览)）。
- 打开时定位并展开到当前在看的文件。改动圆点等 `review` 经 `services` 给出当前 worktree 的改动路径（[#15](https://github.com/SoloJiang/lectern-app/issues/15) 的设计）后再加，之前不显示。
- ↑ ↓ 选择，→ / ← 展开 / 折叠，↵ 打开文件。

### Fluent 键（`crates/i18n/locales/{en,zh-CN}/file-reader.ftl`）

| 键 | en | zh-CN |
|---|---|---|
| `file-reader-edit` | Edit… | 编辑… |
| `file-reader-edit.tooltip` | Edit this file (⌘E) | 编辑这个文件（⌘E） |
| `file-reader-done` | Done | 完成 |
| `file-reader-save` | Save | 保存 |
| `file-reader-save-anyway` | Save Anyway | 仍然保存 |
| `file-reader-back.tooltip`、`file-reader-forward.tooltip` | Back (⌘[) / Forward (⌘]) | 后退（⌘[）/ 前进（⌘]） |
| `file-reader-back-editing.tooltip` | Finish editing to go back | 完成编辑后才能后退 |
| `file-reader-unavailable-while-editing` | This file is no longer available. Copy your changes or discard them. | 这个文件已不可用。请复制你的修改，或放弃它们。 |
| `file-reader-unsaved-message` | Your changes will be lost if you don't save them. | 不保存的话，你的修改会丢失。 |
| `file-reader-not-editable-not-loaded` | Loading the whole file… | 正在载入整个文件… |
| `file-reader-not-editable-too-large` | Files over 1 MB open read-only | 超过 1 MB 的文件只能阅读 |
| `file-reader-not-editable-long-line` | Files with lines over 4 KB open read-only | 有超过 4 KB 的行的文件只能阅读 |
| `file-reader-not-editable-not-text` | This file isn't UTF-8 text | 这个文件不是 UTF-8 文本 |
| `file-reader-not-editable-line-endings` | This file mixes line endings | 这个文件混用了换行符 |
| `file-reader-not-editable-read-only` | Files from history and dependencies are read-only | 历史版本和依赖里的文件只能阅读 |
| `file-reader-not-editable-reloading` | The file changed on disk and is reloading | 磁盘上的文件已改动，正在重新载入 |
| `file-reader-changed-on-disk` | This file changed on disk while you were editing. | 编辑期间磁盘上的文件已改动。 |
| `file-reader-merge` | Merge | 合并 |
| `file-reader-discard-mine` | Discard My Changes | 放弃我的改动 |
| `file-reader-keep-editing` | Keep Editing | 继续编辑 |
| `file-reader-unsaved-title` | Save changes to { $name }? | 要保存对 { $name } 的修改吗？ |
| `file-reader-unsaved-save`、`-dont-save`、`-cancel` | Save / Don't Save / Cancel | 保存 / 不保存 / 取消 |
| `file-reader-save-failed` | Couldn't save { $name }. | 无法保存 { $name }。 |
| `file-reader-find.placeholder` | Find | 查找 |
| `file-reader-find-count` | { $current } of { $total } | 第 { $current } 处，共 { $total } 处 |
| `file-reader-find-capped` | { $current } of 10,000+ | 第 { $current } 处，共 10,000+ 处 |
| `file-reader-missing` | This file no longer exists. | 这个文件已不存在。 |
| `file-reader-unreadable` | Lectern can't read this file. | Lectern 无法读取这个文件。 |
| `file-reader-unsupported` | This item isn't a regular file. | 这一项不是普通文件。 |
| `file-reader-symlink` | Symbolic link to { $target } | 指向 { $target } 的符号链接 |
| `file-reader-from-history` | Version from { $source } | 来自 { $source } 的版本 |
| `file-reader-files-title` | Files | 文件 |
| `file-reader-files-empty` | No tracked files | 没有受跟踪的文件 |
| `file-reader-files-unavailable` | Lectern can't read this worktree's index. | Lectern 无法读取这个 worktree 的 index。 |

路径、符号链接目标、包名和版本经 `Args::str` 传入（i18n 设计的方向隔离），不翻译。

### UI 探针的语义节点

| ID | 角色 | 状态 |
|---|---|---|
| `file_reader.document[<worktree>:<路径>]` | 文档 | 档位、高亮（开 / 纯文本）、行数是否精确、只读或编辑中 |
| `file_reader.lines` | 代码列表 | 首个显示行的 `RowPos`、可见行数 |
| `file_reader.lines.row[<行首字节>.<显示行>]` | 文本行（只导出可见的） | 行号、是否估计 |
| `file_reader.toolbar.edit`、`.done`、`.save`、`.back`、`.forward` | 按钮 | 可用；不可用时带原因的键 |
| `file_reader.find.field`、`.next`、`.previous`、`.count` | 查找栏 | 结果数、是否封顶 |
| `file_reader.disk.banner`、`.merge`、`.discard`、`.keep` | 提示行与按钮 | — |
| `file_reader.unsaved.save`、`.dont_save`、`.cancel` | 离开守卫的对话框按钮 | 触发的范围（`LeaveScope`） |
| `files.tree` | 文件树 | 展开的目录数 |
| `files.tree.row[<路径>]` | 目录或文件行 | 展开、选中、是否折叠的目录链 |

### Action 与快捷键

| Action | 键 | 说明 |
|---|---|---|
| `file_reader::Edit` | ⌘E | 进入或退出编辑 |
| `file_reader::Save` | ⌘S | 只在编辑时 |
| `file_reader::Find`、`FindNext`、`FindPrevious` | ⌘F、⌘G、⇧⌘G | |
| `file_reader::Back`、`Forward` | ⌘[、⌘] | 只在阅读（不编辑）时绑定、可用；后退到历史的起点之后，⌘[ 回到打开阅读视图之前的视图（[交互规范「快捷键」](interaction.md#快捷键)）。编辑时这两个键归 `InputState`（⌘[ Outdent、⌘] Indent） |
| `file_reader::Close` | Esc | 关闭查找栏；没有时回到打开它的视图。编辑时 `InputState` 先处理，余下的 Esc 是「离开编辑」，经离开守卫 |
| `file_reader::Merge`、`DiscardMine`、`KeepEditing`、`SaveAnyway` | — | 提示行的按钮 |
| `file_reader::ShowFiles` | — | 侧栏的 Files 条目 |
| `file_reader::RevealCurrent` | — | Files 打开时定位当前文件 |

⇧⌘O 由打开它的视图（审阅、抽屉、⌘K、Files）在自己的上下文里处理，取到目标后调 `dyn Reader::open`。

## 状态与异常

对照[交互规范「状态与异常」](interaction.md#状态与异常)：

| 情况 | 表现 |
|---|---|
| 首屏没到 | 100 ms 内不显示加载态；100 ms – 1 s 显示骨架；超过 1 s（冷的页缓存、网络卷）显示进度，Esc 取消 |
| 首屏窗口超过 8 ms | 这一帧画没有高亮的文本，解析完成后上色，不提示 |
| 纯文本（二进制、不是 UTF-8、没有语言、grammar 停用、窗口两次超过 128 ms） | 照常显示和查找，不提示 |
| 文件不存在、读不出、不是普通文件、是符号链接 | 内容区显示对应的一行文字（`file-reader-missing` 等），不弹窗 |
| 依赖根越界、扩展名不允许、`DepRoot::Cache` | `Unreadable`，同上 |
| 对象库的 blob 超过 16 MiB | `Unreadable`（「已知限制」） |
| 文件在阅读时被改 | 收到事件后重新打开，停在同一行号，不提示 |
| 文件在阅读时被删 | 显示 `file-reader-missing`；文件回来（事件）后重新打开 |
| 文件在编辑时被改 | 见「编辑」：没有改动时静默重新载入，有改动时提示合并 |
| 保存失败（权限、磁盘满） | 提示行显示 `file-reader-save-failed`，缓冲区保持未保存 |
| 文件夹项目 | 没有变化事件：阅读时不自动刷新，重新打开才看到；编辑时保存的身份比较仍会发现冲突 |
| Files：文件列表还没就绪、目录表还没建好 | 按 100 ms 规则显示骨架 |
| Files：index 读不出（`ScanError`） | 一行文字 `file-reader-files-unavailable` |

## 测试

`syntax`（不依赖 GPUI，Linux CI 覆盖），测试按前缀分文件：

- `lang_*`：每种核心语言的扩展名；`lang_grammar_versions_match_lock`（版本常量对照 Cargo.lock，与 code_intel 同名测试的做法相同）；`lang_abi_in_range`（每个 grammar 的 ABI 在 tree-sitter 0.27.1 支持的范围内）；`lang_extensions_agree_with_code_intel` 放在 `code_intel` 里（见「对其他 crate 的要求」）。
- `spans_*`：每种语言一份合成源码的整文件区间 insta 快照；`Token` 的编码边界（长度超过 24 位时拆开）。
- `window_*`：`first_read`、`checkpoint_read` 的截取（`t = 0`、`t < 64 KiB`、`t` 在末尾、空文件、小于 8 KiB）；`safe_start` 的规则：多行注释、模板字符串、一行的 JSON（补括号之后与整文件一致）、压缩 JS 的 `;` / `}`、64 KiB 内找不到时为 None；`plan` 只看给出的字节（传入的切片之外的偏移从不访问）。
- `consistency_*`：首屏窗口和窗口链的区间与整文件解析逐字节比较（大文件调研的方法：只比非空白字节，每个字节取最内层的捕获），合成文件 100% 一致，窗口链在合成文件上 100%。
- `deadline_*`：`highlight::window` 在预算用完后返回 `TimedOut`；`FullParse`、`ChainWindow` 分片后的区间与一次解析相同；`Yielded` 之后对同一个对象再 `step` 得到相同的结果；取消标志生效。
- `degrade_*`：NUL、无效 UTF-8、`SLOW_WINDOW` 连续两次（用可控的时钟）都得到 `Plain`；墙上时间增长而 CPU 时间不增长（线程被挤掉）时不降级。
- `text_*`：行索引（`line_start`、`line_of` 与朴素扫描对照）、CRLF、BOM、长行的格数检查点（CJK、制表符）、`rows` 在 64 MiB 的合成内容上只访问可见行（计数的 `Source`）。
- `lines_*`：`LineCheckpoints` 的编码、≥ 1 MiB 才写、`len` 不同时不用。
- `doc_*`：用临时目录的真实文件和 `Executor::manual()`：四种 `Anchor`、完整读取的身份与首屏不同时作废首屏、`Byte` 的 OID 与 `expect` 不同时按行号重新定位、档位 C 的页 LRU 上限、`Symlink` / `Unsupported` / `Missing` / `PathThroughSymlink`、依赖根越界（`..`、指向根外的符号链接、不允许的扩展名）、对象库的 blob、fs_watch 的 `FakeBackend` 注入事件后重新打开、文档 drop 后任务取消和 `OpenFile` 关闭（描述符计数）。
- `lru_*`：8 MiB 上限、预取与批量窗口先淘汰、正在显示的条目不受淘汰影响。
- `prefetch_*`：至多 4 个、同一时刻至多一个预取任务、让路开关打开时停在分片边界（kernel 的 `set_yield`、`ManualClock`）、结果只进 LRU。
- `batch_order_*`：用 `Executor` 的真实批量线程（1 个和 3 个）和模拟 `code_intel` 的长期任务（与 `index/batch.rs` 相同：每个批次边界 `yield_now`，批次时长可控）：预取在每个线程的一批加一个窗口任务之内开始；窗口任务一次不超过 50 ms 的解析、与模拟的批次轮流运行；64 MiB 文件的窗口链运行期间模拟任务照常前进。
- `health_*`：`ThreadCpuClock` 换成测试时钟。在途的一次调用 CPU 时间超过预算加 1 s 之后，同一 grammar 版本的 `enter` 返回 None、`allows` 为 false，其他语言不受影响；`Yielded`、`TimedOut` 之后不在表里的调用，不论墙上时间过了多久都不会触发；线程被挤掉、墙上时间前进而 CPU 时间不变时不触发；续做以剩余预算重新登记。`ParseGuard` 不是 `Send`（编译失败测试）。
- `object_*`：有 `ObjectShare` 时打开 `Object` 不调 `Repo::blob`（计数的 git 替身）；登记 drop 后登记表不再持有那份内容（计数分配器：调用方也放下后字节被释放），≤ 1 MiB 的在首屏任务里解压；同一 OID 登记两次、drop 一次仍可用；超过 16 MiB 又没有登记的为 `Unreadable`；后退历史里的条目不持有内容。
- `edit_*`：增量解析的区间与整文件解析相同；`changed` 之后 `spans` 立即返回平移后的区间；CRLF 与 BOM 在保存时恢复。
- `save_*`、`merge_*`：身份变化时 `Conflict` 且文件内容不变；`save_anyway` 照常写入；合并结果的快照（无冲突、有冲突、两边改同一行）。
- `rg -n 'std::fs::|File::open|mmap' crates/syntax/src --glob '!**/tests*'` 没有结果；`rg -n 'thread::spawn' crates/syntax/src` 没有结果。

`file_reader`（依赖 GPUI，只在 macOS 上）：

- `#[gpui::test]`：元素只为可见的显示行排版（计数的排版钩子）；64 MiB 文件的滚动位置在 `RowPos` 与像素之间换算、拖动滚动条到末尾；长行折行与改变宽度后的重新折行；⌘F 的结果跳转；⌘[ / ⌘] 的历史；进入编辑、改动后 Esc 弹出确认、保存冲突时出现提示行；`leave_guard_*`：有未保存改动时 `Reader::open`、后退前进（探针触发）、`LeaveScope::Worktree` / `Project` / `Window` / `App` 都先弹出确认，取消后文档和缓冲区不变、`ReaderEvent::Open` 为 `Cancelled`，保存冲突时不通过，不保存时通过，没有改动时不弹出；编辑时 ⌘[ / ⌘] 改变缩进、不触发后退；Files 的展开、折叠、目录链、定位当前文件、Finder 顺序。测试直接构造 `DocSnapshot` 和 `DirTable`，不做文件 IO。
- UI 探针完成后（[#6](https://github.com/SoloJiang/lectern-app/issues/6)），阅读视图、编辑模式、Files 的语义树用 insta 快照验收；减弱动态效果时查找栏的展开没有位移。

`services`（目录表的构建在 `DirTable::new` 里）：`dirs_*`（目录表与朴素构造对照、Finder 顺序、折叠链、子模块、空列表）、`dir_table_*`（序列化形式、反序列化乱序和错误的 `dirs`）；`finder_cmp` 的表驱动测试（`file2` < `file10`、`A` 与 `a`、数字前导零）。`project`：文件列表换代后目录表单独一代发布（`ChangedParts::dirs`），`dirs` 只在 `files` 就绪之后就绪、与它来自同一份列表。`platform`：`replace_file_*`（见「对其他 crate 的要求」）。`git`：`merge3_*`。

夹具：只用合成的源码，由 `bench` 的生成器按大文件调研的形状生成（TS、Rust、JSON、压缩 JS、64 MiB），放在 `target/` 下，不提交。

## 对其他 crate 的要求

以下改动各由对应 crate 的 PR 完成，写 `Refs #10`，在「验收」里列为前置条件。

- `services`：`reader` 模块（「接口」）；`project` 模块的 `DirTable`、`DirId`、`finder_cmp`、`ActiveTree::dirs`、`ChangedParts::dirs`；`code_intel` 模块增加 `pub const DEPENDENCY_EXTENSIONS: &[&str]`（ADR 0033 允许的扩展名，`code_intel` 的依赖层改用它，两边只有一份）。
- `project`：文件列表换代之后，在 `Background` 任务里（`BusyGuard`）用 `DirTable::new` 建目录表、单独一代发布；`plugin/project/resident_100k` 的份额在同一个 PR 里核对（目录表 ≤ 1.5 MB，合计仍 < 16 MB）；新增 `project/dirs_100k`、`project/dirs_100k_bytes`。
- `platform`：保存用的原子替换。

  ```rust
  impl SafeRoot {
      /// 用 `bytes` 原子替换 `rel` 这个普通文件：在同一目录里 `openat(O_CREAT | O_EXCL | O_NOFOLLOW)` 建临时文件，
      /// 写入、`fsync`、`fchmod` 成原文件的权限位，再对 `rel` 做不跟随符号链接的 `fstatat`：`expect` 有值且当前身份与它不同时
      /// 删除临时文件、返回 `Changed`；否则 `renameat` 替换，返回新文件的身份。路径的任何一级是符号链接为 `PathThroughSymlink`，
      /// `rel` 不是普通文件为 `Unsupported`。阻塞，后台线程。
      pub fn replace_file(&self, rel: RelPath<'_>, bytes: &[u8], expect: Option<FileIdentity>) -> Result<FileIdentity, ReplaceError>;
  }
  pub enum ReplaceError { Changed { now: FileIdentity }, NotFound, PathThroughSymlink, Unsupported, Io(io::Error) }
  ```

  比较身份和 `renameat` 之间仍有一个很短的窗口（「已知限制」）。测试 `replace_file_*`：内容与权限、身份变化时不替换且临时文件不留下、中间一级是符号链接、`rel` 是符号链接或目录、并发的 `race_replace_never_escapes`。platform 设计「范围」的「写文件」一条改为由这个接口和 #66 共用打开逻辑。已实现，见 [platform 设计「原子替换」](platform.md#原子替换)：权限位是 `0o777`，`fchmod` 在 `fsync` 之前，`rel` 不存在时为 `NotFound`、不创建文件。
- `git`：三方合并，纯计算，用已有的 `gix::diff::blob`（Histogram），不新增依赖。

  ```rust
  /// 按行的三方合并。两边改了同一段时为冲突，冲突段按 `markers` 写出两边的内容。纯计算，任何线程。
  pub fn merge3(base: &[u8], ours: &[u8], theirs: &[u8], markers: &Markers<'_>) -> Merge3;
  pub struct Markers<'a> { pub ours: &'a str, pub theirs: &'a str }
  pub struct Merge3 { pub bytes: Vec<u8>, pub conflicts: Vec<Range<usize>> }
  ```

  测试 `merge3_*` 是纯计算：期望的字节写在测试里，与 `git merge-file` 在同一组合成输入上的结果核对过，测试不运行 git（[git 设计「与设计的差异」](git.md#与设计的差异)）。调用前由本 crate 把基准、缓冲区和磁盘内容统一成同样的行尾和 BOM 处理。
- `platform`：线程的 CPU 时钟，供 `syntax::health`（「grammar 的卡死检测」）。

  ```rust
  /// 当前线程的 CPU 时钟。`current()` 在要测量的线程上调用，得到的句柄可以在任何线程上读。
  /// macOS 用 mach 线程端口的 `thread_info(THREAD_BASIC_INFO)`，Linux 用 `pthread_getcpuclockid` 加 `clock_gettime`。
  /// 读不到时返回 `ErrorKind::Unsupported`，调用方改用墙上时间。不阻塞。
  pub struct ThreadCpuClock { /* … */ }
  impl ThreadCpuClock {
      pub fn current() -> io::Result<ThreadCpuClock>;
      /// 这个线程到目前为止用掉的 CPU 时间（用户态加内核态）。线程已退出时返回错误。
      pub fn cpu_time(&self) -> io::Result<Duration>;
  }
  ```

  测试 `thread_cpu_*`：忙循环的线程 CPU 时间增长，阻塞在条件变量上的不增长，句柄能在另一个线程上读。已实现，见 [platform 设计「线程的 CPU 时钟」](platform.md#线程的-cpu-时钟)。
- `executor`：不改。`syntax` 的批量工作用已有的 `Priority::Batch`。
- `code_intel`：
  - 批量循环（前置条件 F，只补测试）：循环已经在每个 `Step::Worked` 之后调用 `yield_now()`，返回一次 `Pending` 并重新入队（`crates/code_intel/src/index/batch.rs`），带着它的 `Parked`（含停下的大项的 `LargeTurn`），不放下 `LargeSlot`，批量任务数不变；活性的论证见「批量线程上的先后」。本设计依赖这一点，所以要测试把它固定下来：批量线程全被 `code_intel` 的任务占着时入队一个 `Batch` 任务，它不晚于每个线程的下一个批次边界开始；一个任务带着停下的大项重新入队、另一个让路的任务在 `LargeSlot::acquire` 上等时，持有者仍能被取到，开关关闭后两者都完成（单个批量线程和两个批量线程各一次）。已完成：`code_intel` 的 `requeue_*`，单个批量线程只有一个任务、没有等 `acquire` 的一方，由外部的任务在持有者的两次轮询之间运行代替（[code_intel 设计「实际验证」](code_intel.md#实际验证)）。
  - grammar 的卡死检测共用 `syntax::health`（后续 PR，不阻塞本 issue 的各步）：在调用 tree-sitter 和 tree-sitter-tags 的线程上、每次进入之前以这次调用的剩余预算 `enter`，返回时（`Done`、`TimedOut`、`Cancelled`、`Parked`）drop 守卫；`Parked` 的抽取续做时重新登记。返回 None 时按语言不可用处理（[#42](https://github.com/SoloJiang/lectern-app/issues/42) 的 scanner 一条，code_intel 设计「对其他 crate 的要求」的高亮方一条）。
  - 测试 `lang_extensions_agree_with_code_intel`：两边共有的语言，扩展名表相同。
  - 依赖层的扩展名改用 `services::code_intel::DEPENDENCY_EXTENSIONS`。
- `git`：`Text` 增加 `pub fn shared(&self) -> &Arc<[u8]>`，返回它内部的那份 `Arc`（`Text::new` 收的就是 `Arc<[u8]>`），不复制。审阅视图用它取得要登记的内容。
- `review_view`（它的设计）：显示一个文件的 diff 时，为 `ChangeSet::texts` 两侧的 `Text` 各调一次 `Reader::share_object(oid, text.shared().clone())`，`ObjectShare` 与这两份 `Text` 放在一起，diff 不再显示（滚出保留范围、审阅关闭）时一起 drop；从旧一侧打开时 `ReadTarget::origin` 为 `Object`、`expect` 为旧一侧的 OID。
- `workspace`（#30 的设计）：`dyn Contributions` 提供侧栏面板（Files）、内容区视图（阅读视图）的注册，「回到打开它的视图」的导航栈，以及离开守卫：`register_leave_guard`、`LeaveGuard::ask(scope, done)`、`LeaveScope`（「编辑」）。`workspace` 在弹出阅读视图、切换当前 worktree、关闭项目和关闭窗口之前按顺序问守卫，都通过才执行；窗口关闭在 GPUI 的窗口关闭回调里先拒绝、问完再关。
- `app`：Lectern 自己的 `Quit` Action，⌘Q 和应用菜单的 Quit 绑定到它；它按「编辑」里的顺序退出：经 `workspace` 问 `LeaveScope::App` 的守卫 → `Kernel::shutdown(done)` → 可选的 `Executor::shutdown` 与 `Cache::close`（后台线程）→ 全部完成或合计 2 秒之后 `cx.quit()`（`Kernel::shutdown` 只给 `app`，所以这个 Action 在 `app` 里）。只用 GPUI 的 Action、菜单和 `quit`，不接 AppKit（macOS 专属能力只在 `platform`，架构规则 6），不改 GPUI（ADR 0016）。测试：有未保存编辑时 ⌘Q 弹出确认，取消后应用不退出、插件仍是 `Active`；确认后插件按相反顺序停用，之后才退出；`shutdown` 卡住时 2 秒后仍退出。[kernel 设计「内核」](kernel.md#内核)里 `shutdown` 的说明已按这个顺序改写。
- `ui`（#6 的设计）：`TokenKind` 到系统色的映射（浅色 / 深色各一组，与 agent 标识色一样通过色觉辨识校验），等宽字体 SF Mono 12pt 的格宽与行高，展开原语。
- `bench`：探针增加阅读视图首屏画出、编辑可输入、Files 展开完成的 `Milestone`；生成器增加大文件调研的合成形状。

## 未决问题

1. **保存时是否运行项目的 formatter。** [技术调研「编辑范围」](tech-research.md#编辑范围第三层)写了「保存时调用项目已有的 formatter」。formatter 来自工作区（如 `node_modules/.bin/prettier`），被审阅的 agent 可以改写它，保存一次就会以用户的身份运行工作区里的程序。选项：(a) 保存时自动运行；(b) 只在用户执行 Format 命令时运行，命令列出将要运行的程序；(c) 本 issue 不做，保存只写缓冲区。默认 (c)：本设计的保存不运行任何程序，(a) 或 (b) 等人决定后另开 issue。
2. **Files 是否列出未跟踪的文件。** issue 写明文件列表读 git index，agent 新建、还没 `git add` 的文件因此不在 Files 里（它们在审阅的改动里能看到，⌘K 的语料也有同样的问题，[#102](https://github.com/SoloJiang/lectern-app/issues/102)）。选项：(a) 只列 index 里的文件；(b) 另开 issue，由 `project` 在 `FileList` 之外给出当前 worktree 的未跟踪、未被忽略的路径，Files 和 ⌘K 共用。默认 (a)，倾向之后做 (b)。

## 验收

前置条件（其他 crate 的 PR，写 `Refs #10`）：

- A. `services`：`reader` 模块、`DirTable`、`ActiveTree::dirs`、`DEPENDENCY_EXTENSIONS`；`rg -n 'pub trait Reader' crates/services/src` 有一处，`dyn Reader` 的 `ID == "reader"`、`MULTIPLE == false` 的测试通过。
- B. `project`：目录表的构建与发布；「测试」里 `project` 的一条通过，`project/dirs_100k`、`project/dirs_100k_bytes`、`plugin/project/resident_100k` 在预算内。
- C. `platform`：`SafeRoot::replace_file`；`replace_file_*` 测试通过。已完成：接口、步骤和错误见 [platform 设计「原子替换」](platform.md#原子替换)。
- D. `git`：`merge3`、`Text::shared`；`merge3_*` 测试通过，`Text::shared` 返回的 `Arc` 与 `Text::new` 收到的是同一份（`Arc::ptr_eq`）。
- E. `workspace` 的 `dyn Contributions`（#30）、`ui` 的代码配色与等宽度量（#6）：第 7 步之前。离开守卫（`register_leave_guard`、窗口关闭回调）和 `Quit` Action：第 8 步之前，没有它们编辑模式不上线。
- F. `code_intel`：固定「批量循环在每个批次边界重新入队、带着停下的项」的测试（行为已实现，「对其他 crate 的要求」）：第 4 步之前。已完成：`requeue_*` 测试通过。
- G. `platform` 的 `ThreadCpuClock`：第 1 步之前（`health_*` 用它）。已完成：见 [platform 设计「线程的 CPU 时钟」](platform.md#线程的-cpu-时钟)。

本 issue 分八步实现，每步一个 PR，写 `Refs #10`，最后一步写 `Closes #10`。每一步都要 `just check`、`just test`（`syntax` 的步骤加 `-p lectern-syntax`）、`just deny` 通过，并把这一步的 bench 条目加进 `budgets.rs`、改为生效。

1. **`syntax` 骨架、语言表与整文件高亮**（前置 G）：新建 `crates/syntax`（`layer = "domain"`，带 `AGENTS.md` 与 `CLAUDE.md -> AGENTS.md`，[cargo-workspace 设计](cargo-workspace.md)的 crate 表加一行）；根 `Cargo.toml` 按 ADR 0041 加四个 grammar；高亮查询放 `crates/syntax/queries/<语言>/highlights.scm`，文件头写上游版本、commit 和 Lectern 的改动。
   - `lang_*`、`spans_*`、`health_*` 测试通过；每种核心语言至少一份 insta 快照。
   - `cargo tree -e features -i tree-sitter` 里没有 `wasm`；`cargo tree --duplicates -e normal` 里没有 `tree-sitter`（只有 0.27.1 一份）；`cargo tree -e features -i tree-sitter-md` 里没有 `parser`。
   - `python3 scripts/check_architecture.py` 通过：`syntax` 不依赖 GPUI。
2. **窗口**：`window::*`、`highlight::window`、`FullParse`、`ChainWindow`、降级。
   - `window_*`、`consistency_*`、`deadline_*`、`degrade_*` 测试通过。
   - `syntax/first_screen`（只算 `plan` 与解析的部分，读取在第 3 步加上）、`syntax/window_peak` 在预算内；`syntax/full_parse_1mib`、`syntax/chain_16mib` 有结果。
3. **文本与文档**（前置：无新的；用已有的 `open_file`、`BlobHasher`）：`text::*`、`LineCheckpoints` 与 `syntax.lines`、`Engine`、`DocHandle`、`DocSnapshot`、LRU、监听与重新打开、查找。
   - `text_*`、`lines_*`、`doc_*`、`lru_*`、`object_*` 测试通过；`syntax/first_screen/object_shared`、`syntax/first_screen/object_1mib` 在预算内，`syntax/first_screen/object_cold` 有结果；[cache 设计「命名空间」](cache.md#命名空间)的 `syntax.lines` 一行与实现一致。
   - `syntax/first_screen`（含首屏读取）、`syntax/lru_bytes` 在预算内；`syntax/scan_64mib`、`syntax/find_16mib` 有结果。
   - `rg -n 'std::fs::|File::open|mmap' crates/syntax/src --glob '!**/tests*'` 没有结果。
4. **预取与让路**（前置 F）：`Engine::prefetch`（`Batch`，至多一个任务），档位 B、C 的批量窗口接 `background_yielding()`、一个任务至多一段窗口（50 ms）。
   - `prefetch_*`、`batch_order_*` 测试通过；让路开关打开后在途窗口在一片之内停下（同 `code_intel/yield_stop` 的测法，≤ 10 ms）。
   - 合起来的检查：真实的 `code_intel` 在 10 万文件的夹具上建索引时，预取从入队到开始的 p95 ≤ 500 ms（`syntax/prefetch_wait`，最大值只记录）。
5. **编辑的领域部分**（前置 C、D）：`EditSession`、保存、冲突、合并。
   - `edit_*`、`save_*`、`merge_*` 测试通过；`syntax/edit_tree_bytes` ≤ 41 MiB。
6. **插件与服务**（前置 A）：本 crate 加 `plugin = true`，`FileReaderPlugin`、`dyn Reader`、`set_focus` / `prefetch` 的调用。
   - `python3 scripts/check_architecture.py` 通过：本 crate 不依赖其他插件 crate，只有 `app` 和 tool 层依赖它；`rg -n 'tree-sitter' crates/file_reader/Cargo.toml` 没有结果。
   - `plugin/file-reader/activate` < 1 ms、`plugin/file-reader/resident` ≤ 0.5 MB。
7. **阅读视图与 Files**（前置 B、E）：自绘元素、查找栏、前进后退、Files 面板、Fluent 资源、探针节点。
   - macOS 上 `just test -p lectern-file-reader` 的 `#[gpui::test]` 通过；`rg -n 'std::fs|SafeRoot|read_ranges' crates/file_reader/src` 没有结果（视图不做 IO）。
   - `app/open_file`（全部场景）、`app/scroll_frames`（64 MiB、压缩成一行）、`app/scroll_frames_indexing`、`app/nav_back`、`app/files_open`、`app/files_expand` 在 macOS CI 上改为生效并在预算内。
8. **编辑模式**（前置 C、D、E 的离开守卫与退出钩子，第 5 步）：`InputState`、`InputHighlighter` 适配器、离开守卫、提示行与确认对话框。写 `Closes #10`。
   - `#[gpui::test]` 的编辑场景和 `leave_guard_*`（「测试」列出的每一种触发）通过；`rg -n 'features.*tree-sitter' Cargo.toml crates/file_reader/Cargo.toml` 没有 gpui-component 的 `tree-sitter` feature。
   - `app/enter_edit` ≤ 100 ms、编辑模式的 `app/scroll_frames` 在预算内；超标时按 ADR 0034 降低 `TIER_A_MAX`，在本文「接口」改常量。

## 已知限制

- 首屏窗口的起点是启发式的，落在多行注释或模板字符串里时首屏高亮可能出错，直到整文件或窗口链的区间到达（ADR 0034）。
- 需要规范化转换的工作区文件（CRLF、`working-tree-encoding`、`ident`）：`Byte` 目标先按规范内容的偏移打开，完整读取后按行号重新定位一次；行检查点查不到，只给行号的首屏退回文件开头。
- 外部 scanner 卡住时，那个 executor 线程一直被占用，直到 scanner 返回；之后同一 grammar 版本本次运行只显示纯文本（ADR 0041）。
- Markdown 只高亮块级结构，行内的强调、代码段和注入的语言（代码块里的代码、模板字符串里的 HTML）不高亮。
- `finder_cmp` 与 Finder 的 `localizedStandardCompare` 在重音字符和区域规则上可能不同。
- 制表符固定按 4 格计，不读 `.editorconfig`。
- 对象库里的 blob 只能整份取出（`Repo::blob`）：没有人持有、超过 1 MiB 的（Diff 关闭之后的后退、前进）首屏要等解压完，不在 16 ms 的保证里（ADR 0042）；超过 16 MiB 又没有人持有的不打开。
- 不做自动保存：进程崩溃时未保存的编辑丢失。
- 由系统发起的退出（注销、关机、Dock 菜单里的退出）不经 `Quit` Action，锁定的 GPUI 也不能取消或推迟退出：有未保存的编辑时会丢失，插件也不按顺序停用；拦住它们要在 `platform` 里给应用代理加 `applicationShouldTerminate:`，以后另行决定。
- 保存时比较身份与 `renameat` 之间有一个很短的窗口，这期间别的进程写入的内容会被覆盖；原子替换会断开硬链接、不保留扩展属性。
- 文件夹项目的文件在阅读时不自动刷新（`project` 不监听文件夹项目）。
- 查找只对 ASCII 字母不区分大小写，最多 10,000 个结果。
- 高亮查询里的 `#is-not? local` 不求值（没有 locals 查询）：JavaScript 的 `console`、`require` 等内置名，同名的局部变量也按内置名着色。

## 实际验证

第 1 步（`syntax` 骨架、语言表与整文件高亮），在 Linux（x86_64 容器）上运行：

- `just test -p lectern-syntax`：`lang_*`、`spans_*`、`health_*` 全部通过；11 种核心语言各有一份整文件区间的 insta 快照（`crates/syntax/src/tests/snapshots/`），快照逐个看过。`just doctest` 里 `ParseGuard` 的 `compile_fail` 示例通过，`health_parse_guard_is_not_send` 在编译期检查同一点。
- `cargo tree -e features -i tree-sitter` 里没有 `wasm`；`cargo tree --duplicates -e normal` 里没有 `tree-sitter`（只有 0.27.1 一份，四个新 grammar 没有带进别的版本）；`cargo tree -e features -i tree-sitter-md` 里没有 `parser`。
- `python3 scripts/check_architecture.py` 通过（含在 `just check` 里）：`syntax` 是领域层、不依赖 GPUI。`just check`、`just deny`、`just test`（不依赖 GPUI 的全部 crate）、`just doctest` 通过。
- `rg -n 'std::fs::|File::open|mmap' crates/syntax/src --glob '!**/tests*'` 与 `rg -n 'thread::spawn' crates/syntax/src` 没有结果。
- 没有在 macOS 上运行：`health` 在 macOS 上读 `ThreadCpuClock` 的 `thread_info` 实现，只由 macOS CI 覆盖（`health_global_on_real_clocks`）；本步没有 bench 条目（「验收」第 1 步没有列出，`syntax/*` 的基准从第 2 步开始）。

第 2 步（首屏窗口、`FullParse`、`ChainWindow` 与降级），在 Linux（x86_64 容器）上运行：

- `just test -p lectern-syntax`：83 个测试通过，新增 `window_*`、`consistency_*`、`deadline_*`、`degrade_*`（`SlowWindows` 在注入的时钟上：线程被挤掉时不降级，CPU 时间连续两次超限时降级，没有 CPU 时钟时按墙上时间）。`consistency_*`：合成的 TypeScript、Rust、多行和一行的 JSON、压缩成一行的 JavaScript（各约 1.1 MiB，首屏读取是两段），每种 44 个目标的首屏窗口、以及整条窗口链，与整文件解析逐字节比较（非空白字节、最内层的捕获）全部一致。`deadline_*`：`FullParse` 和 `ChainWindow` 按 0 到 1 ms 的预算分成多片、在让路和取消之后续做，得到的 token 与一次解析、一次查询整棵树（`one_shot`）逐个相同；每一片在 `health` 里登记的预算就是这一片的预算，两片之间表里没有它，期间这个线程的 CPU 时间和墙上时间前进几十秒也不会被判为卡住。第 1 步的 11 份区间快照经 `FullParse` 生成，没有变化；新增 `window_plan_snapshot`（五种形状各两个目标的窗口起点、可见部分与 JSON 补齐），逐条看过。
- `just bench --measure syntax/`（Linux 只报告、不判定）：`syntax/first_screen` 中位数 2.27 ms（预算 ≤ 8 ms），`syntax/first_screen/minified` 中位数 8.6 ms，`syntax/window_peak` 最大值 6.86 MB（预算 ≤ 8 MiB，四种形状各 4 个窗口里的最大值），`syntax/full_parse_1mib` 中位数 292 ms（解析加整份查询，8 ms 一片），`syntax/chain_16mib` 中位数 4.41 s（每片 50 ms）。`spent` 改按 CPU 时间计、安全位置限在目标之前 64 KiB 之后重测：2.26 ms、8.69 ms、6.86 MB、318 ms、4.71 s，后两项在这台共享机器的噪声范围内。`just test -p lectern-bench` 通过（预算表的出处、目标与检查）。
- `just check`、`just deny`、`just doctest`、`just test`（Linux 上不依赖 GPUI 的全部 crate，1679 个）通过；`rg -n 'std::fs::|File::open|mmap' crates/syntax/src --glob '!**/tests*'` 与 `rg -n 'thread::spawn' crates/syntax/src` 没有结果。
- 没有在 macOS 上运行：上面的基准在 M 系列上由 macOS CI 判定。

前置条件 A（`services`），在 Linux（x86_64 容器）上运行（`services` 不依赖 GPUI）：

- `just test -p lectern-services`：50 个测试通过。新增：`reader_service_id_and_multiple`（`ID == "reader"`、`MULTIPLE == false`，`service::<dyn Reader>()` 的键相同）、`reader_is_a_shared_trait_object`、`object_share_releases_once_on_drop`（在另一个线程上 drop，回调只调用一次）、`reader_targets_round_trip`（两种来源、三种 `Anchor`、SHA-1 与 SHA-256 的 `expect`、文件夹项目）、`reader_event_round_trips`；`dirs_match_a_naive_construction`、`dirs_children_are_in_finder_order`、`dirs_fold_single_child_chains`、`dirs_submodules_are_files`、`dirs_empty_list_has_only_the_root`、`finder_cmp_cases`；`dir_table_serializes_dirs_and_files`、`dir_table_deserializes_from_the_file_list`（乱序、错误、缺少的目录都按 `files` 重建，不合法的路径计入 `skipped`）；`dependency_extensions_are_the_adr_0033_list`。`project_snapshot_round_trips` 的样本带上目录表，检查反序列化后 `dirs.files()` 与 `files` 相等。
- `rg -n 'pub trait Reader' crates/services/src` 只有一处（`crates/services/src/reader/mod.rs`）。
- 改到 `ActiveTree` 和 `ChangedParts` 字面量的 crate：`just test -p lectern-project`（123 个）、`just test -p lectern-code-intel`（344 个）全部通过，`project` 的快照没有变化。
- `just check`（格式、架构规则、文档链接、clippy `-D warnings`，Linux 上不依赖 GPUI 的全部 crate 的全部目标）、`just deny` 通过；`services`、`project` 的文档测试（0 个）通过。没有新增依赖。

前置条件 B（`project`，连同 `services` 里 `DirTable::new` 的提速），在 Linux（x86_64 容器，4 核）上运行，测法与其余验证见 [project 设计「实际验证」](project.md#实际验证)：

- `project/dirs_100k` 中位数 9.45 ms（负载 1.8 时）、9.48 ms（负载约 3），预算 ≤ 20 ms；提速之前 26.5 ms、26.2 ms。`project/dirs_100k_bytes` 1.46 MB（≤ 1.5 MB），提速前后相同。`project/resident_100k_bytes` 14.1 MB（< 16 MB），含目录表。
- `just test -p lectern-services`：52 个测试通过。新增 `dirs_match_a_naive_construction_on_interleaved_trees`（`a`、`a-b`、`a.c` 与 `a/…` 交错、深链、非 ASCII 名字，与朴素构造逐目录对照，并检查子目录和文件按 `finder_cmp` 排序）、`finder_cmp_ascii_shortcut_matches_the_character_comparison`（38 个含大小写、数字、非 ASCII 字符的词两两组合，原样和拼接后各比一次，与逐字符解码、`to_lowercase` 的比较结果相同）。提速前后在 10 万文件的形状上建出的表逐字段相等（一次性对照，没有留在测试里）。
- 没有在 macOS 上运行。

## 与设计的差异

第 1 步：

- 整文件高亮是一次调用的 `highlight::whole_file`（「接口」已补上）：不分片、不能续做。设计里的 `FullParse`（分片、续做）列在第 2 步，它复用同一套捕获解析；`whole_file` 是否保留为公开项由第 2 步决定。`Budget`、`Outcome` 按设计实现，`Outcome::done()` 是取出结果的便捷方法。
- 捕获怎样变成区间，设计没有写，按以下规则实现：同一节点（同一字节范围）被几个模式捕获时，取查询里的第一个还是最后一个由语言决定，写在每个查询文件头。上游查询两种写法都有：Rust、Go、JSON 按 tree-sitter-highlight 的规则（第一个优先）写，其余按「最后一个优先」写，逐个改顺序会让查询与上游难以对照，所以保留上游顺序、按语言记下规则。嵌套的捕获内层优先；`@none`、`@embedded`（模板字符串里的 `${…}`）让所在范围不着色。捕获名到 `TokenKind` 的映射在 `lang::classify`（`constructor` 归 `Type`，`variable.builtin` 归 `Keyword`，`boolean` 归 `Constant`，`label` 归 `Other`，Markdown 的 `text.*` 归 `Heading`、`String`、`Link`），`lang_captures_known` 检查核心查询里没有不认识的捕获名。这些规则进 `SPANS_VERSION`。
- JavaScript 系的查询拆成 `ecmascript`（tree-sitter-javascript 的 highlights.scm，三种语言共用）、`js`（highlights-params.scm，只给 JavaScript：TypeScript 的参数包在 `required_parameter` 里，这些模式在 TS 的 grammar 上编译不了）、`jsx`、`ts`，由 `src/lang.rs` 拼接。各查询对上游的改动写在文件头：Rust 修正全大写常量正则里多余的 `'`、数字改为 `@number`、`(field_identifier) @property` 移到调用模式之后；Go 的内置函数调用移到一般调用之前；TOML 的 `(pair (bare_key) @property)` 只捕获键；Markdown 去掉 `(code_fence_content) @none`；TS 的首字母大写类型模式不再覆盖全大写常量，并补上 `type_parameters` 的尖括号。MIT 的许可声明集中在 `crates/syntax/queries/NOTICE`，各文件头写版权行并指向它。
- `WindowSpans` 只存有区间的行，`line()` 对没有区间的行返回空（设计没有规定是否存每一行）；增加 `lines()`。`Token` 增加 `new`、`end`、`MAX_LEN`，`Languages` 增加 `iter()`，`SPANS_VERSION` 公开（「接口」已补上）。
- `limits` 第 1 步只有 `STUCK_GRACE` 和新增的 `STUCK_GRACE_WALL`（正文里的「宽限放大到 10 s」写成常量）；其余常量随用到它们的步骤加入。
- `health` 按（语言、grammar 版本）登记和停用：TS 和 TSX 来自同一个 grammar crate、grammar 版本相同，各自判定。每个线程第一次登记时打开 `ThreadCpuClock`、之后复用（线程局部）；检查在途项时读不到某个线程的 CPU 时钟（线程已退出）就当作它不在运行。测试经内部的 `Clocks` 换成测试时钟。
- 依赖：第 1 步只用 `tree-sitter`、十个 grammar crate、`lectern-platform`（`ThreadCpuClock`）、`memchr`，`insta`（dev）；「依赖」表里的其余依赖随后续步骤加入。查询结果的迭代用 `tree-sitter` 再导出的 `StreamingIterator`，不直接依赖 `streaming-iterator`。
- 扩展名与整文件名：JSON 另认 `jsonc`、`jsonl`；`Cargo.lock`、`poetry.lock`、`uv.lock`、`Pipfile` 是 TOML，`package-lock.json`、`.babelrc`、`.eslintrc` 是 JSON；扩展名先按原样找，找不到再按小写找（与 `code_intel` 相同）。规则族：JSON 为 `Json`，Markdown 为 `Markup`，其余（含 YAML、TOML）为 `Code`。

第 2 步：

- `highlight::whole_file` 不再是公开项，由 `FullParse` 取代。档位 A 和空闲预取都要分片，没有调用方需要不能续做的整文件高亮；一次做完就是 `FullParse::new(..).step(足够的预算)`，结果相同。两个公开入口给出同一份结果，以后容易分叉。第 1 步的实现留在测试里（`highlight::one_shot`，只在 `cfg(test)` 下）：整段解析、整棵树一次查询，`spans_*` 和 `deadline_*` 拿分片的结果逐个 token 和它对照。已改「接口」。
- 分片的查询：解析完成后高亮查询按 8 KiB 的字节块跑（`QueryCursor::set_byte_range`），一片在块的边界停下，下一片从下一块继续。一个字节的颜色只取决于与它相交的捕获，这些捕获都与它所在的块相交，所以每块的解析结果与整棵树一次查询相同；被块尾截断的区间在下一块开头接回同一个 token（只在上一块的最后一段确实越过块尾时接）。每一片的第一块不计时（取消和让路照常生效），保证极小的预算下每片也有进展，所以一片至多超出预算一个块的查询（普通代码约 1 ms，压缩代码 2–4 ms）；首屏的 `window` 不续做，每块都计时。设计没有写查询怎样分片。
- `Outcome::Cancelled` 之后对象同样保留状态，再调 `step` 接着做；`Done` 或 `Unavailable` 之后再调 `step` 返回 `Unavailable`。`FullParse::new`、`ChainWindow::new` 在第一次用到这种语言时编译查询（阻塞，不计入任何一片的预算）。
- `window` 收 `&JsonPad`（设计写的是按值），调用方不必复制 `Plan`。`JsonPad` 的形状设计没有写：`before`、`after` 两段字节。窗口之前的容器的种类从窗口本身推出：这一层出现 `:` 是对象，它的右括号说明它是什么，只有不带键的元素是数组；只有父容器是对象时才补空键 `"":`。设计和大文件调研写的是每个右括号「各补一个 `"":{` 或 `"":[`」，父容器是数组时这样补出来的 JSON 不合法；窗口在最外层补出的容器关上之后还有内容时，也要再补一层。JSON 窗口的终点截到可见部分之后最后一个完整的值（右括号之后或逗号之前），补上的括号因此不会落进半个值里；到文件末尾的窗口不截。
- JSON 的窗口链不按树切：窗口切在最后一个不在字符串里的逗号之后，逗号之前的部分带 `JsonPad` 解析，下一个窗口从逗号之后开始（一个成员或元素的开头）。按 code_intel 的规则切 JSON 时，窗口的根是 `ERROR`，成员后面同一行紧跟着逗号，没有一个子节点算完整，每个窗口都扩到 256 KiB 再切在某个 token 之后，下一个窗口从结构中间开始，键被当成普通字符串。逗号里没有的窗口（一个超过 64 KiB 的字符串）退回按树切。
- `ERROR` 节点里最后一个子节点的判定与 code_intel 不同：code_intel 把后面没有别的子节点的那一个算作完整，这里要求它后面在这个节点里还有换行。窗口截在一行压缩代码中间时，`run:function(x){return f(x)}` 截成 `…{return ` ，自动分号让 `return` 成为一条完整的语句，切在这里就把对象字面量分到两个窗口里，`run` 在前一个窗口里不再是方法名。`consistency_chain_minified` 覆盖这一点。code_intel 的窗口规则不在本步改动。
- 降级：「一个 64 KiB 的窗口解析连续两次超过 128 ms」写成 `SlowWindows`，调用方（第 3、4 步的文档和批量任务）每完成一个窗口记一次 `ChainWindow::spent()`。设计没有说按什么时间计。`spent` 按线程的 CPU 时间计，与 `STUCK_GRACE` 同一个理由：档位 B、C 的窗口在 utility QoS 的批量线程上，并发的构建会把一个 10–20 ms 的窗口的墙上时间拉过 128 ms，按墙上时间计时，构建期间打开的 `package-lock.json` 连续两个窗口就会降级；128 ms 指的是病态的输入，不是等不到 CPU 的线程。计时在调用的线程上读 `health` 的同一套时钟（`platform::ThreadCpuClock`，`Health::stopwatch`），只包住 tree-sitter 的解析，不含查询和切点：ADR 0034 的界限是解析吞吐。读不到 CPU 时钟时按墙上时间、对照 `SLOW_WINDOW_WALL`（1 s，`SLOW_WINDOW` 的 8 倍：仍慢于 64 KiB/s 的窗口在繁忙的机器上也算病态，被挤得更厉害的窗口宁可保留高亮）。上限按解析过的字节数放宽（每 64 KiB 一份，扩到 256 KiB 的窗口连第一次的 64 KiB 一起算），不足 64 KiB 的末尾窗口按一份计。`degrade_*` 用 `ManualClocks` 在 tree-sitter 的进度回调里推进时钟：墙上时间增长、CPU 时间不增长时不降级，CPU 时间连续两次超过上限时降级，没有 CPU 时钟时按墙上时间。已改「接口」。
- `Source` 只为 `Vec<u8>` 实现。为 `Box<[u8]>`、`Arc<[u8]>` 实现时，`Source::len` 在方法解析里排在 `[u8]::len` 之前，导入 `Source` 的代码里 `.len()` 会变成 `u64`；`Text`、`Pages` 随第 3 步加入。另有默认的 `is_empty`（clippy 的 `len_without_is_empty`）。
- `plan`：目标往前 4 KiB 内找不到换行时（长行），`target_line_start` 取目标本身，可见部分是目标起的 8 KiB。传入的切片不从文件开头开始时，切片的第一个字节不当作行首。`safe_start` 在切片从文件开头开始、规则又没找到位置时取 0。`plan` 只把目标之前 64 KiB 起的字节交给 `safe_start`：不超过 1 MiB 的文件首屏整份读入，不截的话往回找会一直扫到文件开头，与 ADR 0034「选安全位置只看目标之前至多 64 KiB」不符。编码检查允许切片两端被截断的 UTF-8 序列。
- `limits` 加入 `TIER_A_MAX`、`LONG_LINE`、`FIRST_WINDOW_DEADLINE`、`SLICE`、`SLOW_WINDOW`、`WINDOW`、`WINDOW_MAX`，值与「接口」相同；新增 `SLOW_WINDOW_WALL`（见上一条）。
- 合成源码：「测试」写的是由 `bench` 的生成器生成，但 `syntax` 的测试不能依赖 tool 层的 `bench`。生成器放在 `syntax` 的 `testing` 模块（`test-support` feature）：TypeScript 和 Rust 是带多行注释、模板字符串或原始字符串、嵌套块的单元重复拼接，JSON 是两格缩进的 `package-lock.json` 形状，另有压缩成一行的 JavaScript；`bench` 经它取得夹具（bench 设计「夹具与 `test-support`」的做法）。
- `syntax/first_screen` 本步只计 `plan` 与窗口的解析和查询，字节从内存里切出；视口按 60 行。

前置条件 A（`services`）。下面只记上文「接口」没有定下、实现时定下的形状：

- `DirTable::new` 在 `services` 里。上文只写了「由 `project` 建」，没有构造器；`DirTable` 的字段是私有的（与 `FileList` 一样），反序列化又要重建，所以构建只能放在 `services`，`project`（前置条件 B）在后台任务里调用它。目录表本身的测试（`dirs_*`、`finder_cmp`）因此也在 `services`，前置条件 B 只剩发布与基准。已改「接口」「测试」「对其他 crate 的要求」「验收」。
- 序列化形式多了 `files`。上文写的是 `{ "dirs": [[路径, 父下标], …] }`，但目录的文件是 `FileList` 里的下标，没有文件列表就不能重建，反序列化的结果也不能只靠 `dirs` 保证不变式。实现写出 `dirs` 和 `files`，读回时只用 `files` 经 `DirTable::new` 重建，`dirs` 不读。`ActiveTree` 序列化时文件列表因此出现两次；读回之后 `dirs.files()` 与 `ActiveTree::files` 相等，但不是同一个 `Arc`。已改「接口」。
- 布局：`DirId` 是按路径字节序排好的下标，根为 0，父目录排在子目录之前，`find` 二分查找；每个目录 20 字节（路径、子目录、文件三段区间的终点，父目录，子树文件数；起点取上一个目录的终点），`names` 存完整路径，`path` 直接返回切片；`subdirs` 存为 `Box<[DirId]>`（`DirId` 是 `repr(transparent)`）。另有 `len`、`is_empty`，供序列化和探针遍历。`fold_end` 对根也按同一规则，视图决定根要不要折叠。
- `ObjectShare` 包着实现方传入的注销回调（`ObjectShare::new`），与 `code_intel::ProviderRegistration` 相同：`services` 不知道登记表的类型。已改「接口」。
- 派生：`ReadTarget`、`Anchor`、`OpenTicket`、`OpenOutcome` 另派生 `Hash`，`Anchor`、`OpenOutcome` 另派生 `Copy`；`ReaderEvent` 派生 `Clone`、`PartialEq`、`Eq`、`Debug` 和 serde（`TestKernel::emitted` 要 `Clone`）。
- `TextOrigin` 现在只有 `Worktree`、`Object`。`Dependency` 和 `DepRoot` 是 code_intel「验收」第 9 步的前置条件「`services` 的依赖类型」，随它加入；`ReadTarget::origin` 直接用 `TextOrigin`，到时不用改 `reader` 模块。
- `DEPENDENCY_EXTENSIONS` 取 ADR 0033 的全部条目（TS、Rust、Go、Python 四组共 13 项），不只是 TS 依赖层用到的六项；以 `.` 开头的一项是文件名的后缀，其余是完整的文件名，按字节比较。`code_intel` 还没有依赖层（第 9 步），没有现成的列表可以移过来，依赖层实现时直接用这一份。
- `project` 在前置条件 B 之前：构造 `ActiveTree` 的三处填 `Scan::Pending`，`changed` 比较新旧快照的 `dirs`，所以 `dirs` 一直是 `Pending`（[project 设计「与设计的差异」](project.md#与设计的差异)）。`code_intel` 的测试辅助和测试同样填 `Pending`。

前置条件 B（`project`，以及 `services`）：

- `project` 的构建与发布、`files` 不可用时 `dirs` 取同一个错误、按 `Arc` 判断列表换代，记在 [project 设计「与设计的差异」](project.md#与设计的差异)。
- `DirTable::new` 在本 PR 里改写，不改接口、布局、序列化形式和不变式。前置条件 A 的实现在 10 万文件的形状（21,065 个目录）上约 26 ms，超出「性能」的 20 ms。耗时分三段：每段连续文件都把整条上级链压入、再对约 8 万条路径排序去重（约 8.5 ms）；按目录分组时逐个分配 `Vec`、逐条二分查找（约 15 ms）；`finder_cmp` 逐字符解码并 `to_lowercase` 的排序（约 12.7 ms）。改为：
  - 每段只压入与上一段不共有的上级目录，压入后已基本有序，排序前先检查；
  - 父目录取自上一个目录的上级栈，栈里没有时才二分查找；
  - 子目录和文件按计数分组，填进一整块数组，不再为每个目录分配；
  - 每个文件的名字在第一遍扫描里取出，目录内的文件已按 `finder_cmp` 有序时不排序；
  - `finder_cmp` 两边都是 ASCII 字节时直接比较小写字节，不解码；结果与逐字符比较相同（`finder_cmp_ascii_shortcut_matches_the_character_comparison`）。
- 改写后 `project/dirs_100k` 约 9.5 ms，见「实际验证」。
