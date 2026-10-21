# file_reader 设计

状态：已接受，尚未实现。

对应 issue [#10](https://github.com/SoloJiang/lectern-app/issues/10)。本 issue 交付两样功能：按需层的 Files（侧栏里按需展开的文件树，文件列表来自 git index），以及收起层的完整文件阅读（⇧⌘O，默认只读）和轻量编辑（Edit… / ⌘E）（[交互规范「三层结构」](interaction.md#三层结构)、[ADR 0012](../adr/0012.md)）。打开任何大小的文件到首屏高亮 < 16 ms、滚动 120 fps 的做法由 [ADR 0034](../adr/0034.md) 规定，本文把它写成接口和数据结构。高亮的计算放在新的领域库 `syntax`（[ADR 0041](../adr/0041.md)），`file_reader` 只做界面。

## 范围

本 issue 跨两个新 crate 和几个已有 crate 的小改动：

| crate | 层 | 内容 |
|---|---|---|
| `syntax`（新，`lectern-syntax`） | 领域，非插件的库 | 语言表与高亮查询、首屏窗口、按窗口与整文件的高亮、着色区间、文本存储与行索引、行检查点（`syntax.lines`）、8 MiB 高亮与文本 LRU、打开文档的读取与刷新、空闲预取、编辑模式的增量高亮、保存与三方合并的领域部分、grammar 的卡死检测 |
| `file_reader`（本 crate） | 视图，插件 crate | 阅读视图（自绘的固定行高列表）、查找栏、前进后退、编辑模式（gpui-component `InputState` 与 `InputHighlighter` 适配器）、Files 文件树、`dyn Reader` 服务 |
| `services` | kernel | `reader` 模块（`dyn Reader`、`ReadTarget`、`Anchor`）；`project` 模块的 `DirTable` |
| `project` | 领域，插件 | 文件列表就绪后在后台建目录表（[ADR 0030](../adr/0030.md)：约 1 MB，计入 `project`） |
| `platform` | 基础设施 | `SafeRoot::replace_file`：保存用的原子替换 |
| `git` | 领域 | `merge3`：三方合并 |
| `executor` | 基础设施 | `Priority::BatchFront`：批量线程先取的一级，供预取和大文件的批量窗口 |

- 做：
  - 完整文件阅读：工作区文件、对象库里的 blob（Diff 的旧一侧）、依赖根里的文件（`TextOrigin::Dependency`，[ADR 0033](../adr/0033.md)）；按字节偏移、行号或文件开头定位；档位 A / B / C 与降级为纯文本；行号栏；长行折成显示行；查找（⌘F）；复制。
  - 前进、后退（⌘] / ⌘[）：本窗口最近 100 个阅读位置，只在内存里。
  - 编辑：只对档位 A 的工作区文件；撤销重做、查找替换、自动缩进、括号补全（`InputState` 自带）；保存（⌘S）；与 agent 同时改了文件时只在编辑过的文件上提示三方合并。
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
    /// 打开完整文件阅读，替换本窗口当前的文档（之前的位置进入后退历史）。立即返回，任何线程可调用；
    /// 界面切换排到主线程。审阅视图（⇧⌘O、从 hunk）、抽屉的候选、⌘K 的文件结果都经它打开。
    fn open(&self, target: ReadTarget);
    /// 交互静止时的预取（ADR 0033）：只取前 4 个，算出高亮区间放进 LRU，不打开、不发事件。立即返回。
    fn prefetch(&self, targets: Vec<ReadTarget>);
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
pub struct DirTable { /* dirs: Box<[Dir]>, names: Box<str>, subdirs: Box<[u32]>, files: Box<[u32]>, list: Arc<FileList> */ }

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct DirId(pub u32);

impl DirTable {
    pub fn files(&self) -> &Arc<FileList>;    // 建表用的那份文件列表
    pub fn root(&self) -> DirId;
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
- 不序列化内部布局：序列化为 `{ "dirs": [[路径, 父下标], …] }`，反序列化按 `FileList` 重建排序，与 `FileList` 的做法相同。

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
}

pub struct Lang { /* name, grammar, 高亮查询, 版本, 规则族 */ }
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
impl Token { pub fn start(self) -> u32; pub fn len(self) -> u32; pub fn kind(self) -> TokenKind; }

/// 一段文本 `[start, end)` 的区间，按行存放。行以行首的字节偏移标识，不需要行号（行号在后台扫描前只是估计）。
pub struct WindowSpans { /* start, end, line_starts: Box<[u32]>, offsets: Box<[u32]>, tokens: Box<[Token]>, exact: bool */ }
impl WindowSpans {
    pub fn range(&self) -> Range<u64>;
    /// `line_start` 处那一行的区间；不在这一段里时为空。
    pub fn line(&self, line_start: u64) -> &[Token];
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
        pub encoding: Encoding,          // Utf8 | Utf8Bom | NotText（二进制或不是 UTF-8）
        pub target_line_start: u64,
        /// 可见部分：目标之前至多 rows / 3 行、之后至多 rows 行，至多 64 KiB；长行按 8 KiB 计。
        pub visible: Range<u64>,
        /// 窗口：安全位置起、可见部分之后 4 KiB 止；64 KiB 内找不到安全位置、或 `NotText` 时为 None（先画纯文本）。
        pub window: Option<Range<u64>>,
        /// JSON 在窗口两端补的括号（ADR 0034）。
        pub json_pad: JsonPad,
    }

    /// 安全位置的规则（ADR 0034）：`Code` 取可见区域之前约 8 KiB 处往回 4 KiB 内第 0 列开始、不是右括号和注释续行的行，
    /// 没有时取缩进最少的行；长行取 `;` 或 `}` 之后；`Json` 取形如 `,"键":` 的成员开头（键 ≤ 256 字节、不含反斜杠）；
    /// `Markup` 取空行之后。只看给出的字节，不从文件开头扫描。
    pub fn safe_start(rules: Rules, bytes: &[u8], base: u64, before: u64) -> Option<u64>;
}

// ---- 解析与高亮（阻塞，后台线程）----

pub mod highlight {
    /// 截止时刻、取消标志和让路探针（`BackgroundGuard::yield_requested`）。
    pub struct Budget<'a> { pub until: Instant, pub cancel: &'a AtomicBool, pub yield_probe: Option<&'a dyn Fn() -> bool> }

    pub enum Outcome<T> { Done(T), TimedOut, Cancelled, Yielded }

    /// 窗口单独解析，只算 `visible` 的区间。首屏用，截止时间 8 ms（`FIRST_WINDOW_DEADLINE`）。
    pub fn window(lang: &Lang, src: &dyn Source, window: Range<u64>, pad: JsonPad, visible: Range<u64>,
                  budget: Budget<'_>) -> Outcome<WindowSpans>;

    /// 档位 A 的整文件解析：每次 `step` 至多一片（8 ms），解析完一次性算出整个文件的区间、释放语法树。
    /// 解析器在两片之间保留状态；`src` 换了时调用方新建一个（tree-sitter 要 `reset`，大文件调研第 3 节）。
    pub struct FullParse { /* parser, src */ }
    impl FullParse {
        pub fn new(lang: &Lang, src: Arc<dyn Source>) -> FullParse;
        pub fn step(&mut self, budget: Budget<'_>) -> Outcome<WindowSpans>;  // TimedOut 表示这一片用完、还没解析完
    }

    /// 档位 B / C 的窗口链：从 `start` 解析约 64 KiB，按树切（与 code_intel「大文件」的「窗口」规则相同），
    /// 返回这一段的区间和下一个窗口的起点；找不到切点时扩到 256 KiB 再找一次，仍没有时切在最后一个 token 之后。
    pub fn chain_window(lang: &Lang, src: &dyn Source, start: u64, budget: Budget<'_>) -> Outcome<(WindowSpans, u64)>;

    /// 按字节区间读文本，不要求连续存放（分块、页）。
    pub trait Source: Send + Sync {
        fn len(&self) -> u64;
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
    /// 项目关闭：丢弃它的 `SafeRoot`、`Repo` 和订阅。
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
    pub struct Health { /* 在途解析表：(语言, grammar 版本) → 开始时刻、截止时刻 */ }
    impl Health {
        /// 这个 grammar 版本本次运行是否仍可用。不阻塞。
        pub fn allows(&self, lang: &str, grammar_version: u64) -> bool;
        /// 登记一次在途解析，drop 时注销。登记时先检查同一 grammar 版本在途的解析：
        /// 有一次已经超过它的截止时刻 1 s（`STUCK_GRACE`）仍没返回，就把这个版本记为停用，返回 None。
        pub fn enter(&self, lang: &str, grammar_version: u64, deadline: Instant) -> Option<ParseGuard>;
    }
    pub struct ParseGuard { /* … */ }
}

pub mod limits {
    pub const TIER_A_MAX: u64 = 1 << 20;          // 1 MiB：整文件解析、可以编辑
    pub const LONG_LINE: u64 = 4 << 10;           // 4 KiB：长行，折成显示行；有这样的行就不是档位 A
    pub const TIER_B_MAX: u64 = 16 << 20;         // 16 MiB：整份读入；更大的按页读
    pub const FIRST_WINDOW_DEADLINE: Duration = Duration::from_millis(8);
    pub const SLICE: Duration = Duration::from_millis(8);
    pub const SLOW_WINDOW: Duration = Duration::from_millis(128);   // 连续两次：这个文件本次不高亮
    pub const WINDOW: u64 = 64 << 10;
    pub const WINDOW_MAX: u64 = 256 << 10;
    pub const PAGE: u64 = 64 << 10;
    pub const PAGE_LRU: u64 = 8 << 20;
    pub const LRU: u64 = 8 << 20;
    pub const CHECKPOINT_STRIDE: u64 = 64 << 10;
    pub const CHECKPOINT_MIN: u64 = 1 << 20;      // 小于 1 MiB 的内容不写检查点
    pub const OBJECT_MAX: u64 = 16 << 20;         // 对象库里的 blob 整份取出的上限
    pub const PREFETCH_MAX: usize = 4;
    pub const STUCK_GRACE: Duration = Duration::from_secs(1);
    pub const HISTORY_MAX: usize = 100;
    pub const FIND_MAX: usize = 10_000;
}
```

常量的来源与校准状态：1 MiB、16 MiB、128 ms 取自[大文件调研第 3 节](../research/large-files-rendering.md#3-大文件的解析与高亮)在 Linux 容器上的实测，4 KiB 是估计值，8 ms 是一帧的一半；都在 M 系列上由「性能」里的基准校准，校准结果改动常量时在同一个 PR 里改本节。`STUCK_GRACE` 是窗口解析实测最差值（15 ms）和 `SLOW_WINDOW` 的数倍。

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
// started：`Engine::start(handle)`，订阅 `ProjectEvent`（项目关闭时 `close_project`）。不提交任何任务。
```

`dyn Reader::open` 把目标排到主线程：在 `token.enter()` 的准入内找到本窗口的阅读视图（没有时经 `dyn Contributions` 切到它），调 `Engine::open`，把上一个文档的位置压进后退历史；然后 `CodeIntel::set_focus(Current(Some(TextRef)))`，`TextRef` 在快照的 `oid` 有值之后才设（见「数据流与线程」）。

## 数据结构

### 档位与降级

| 档位 | 条件 | 文本 | 首屏之后 | 守卫与线程 |
|---|---|---|---|---|
| A | ≤ 1 MiB，没有超过 4 KiB 的行 | 首屏一次读完（一段 `read_ranges`） | `FullParse`，8 ms 一片，完成后用整文件的区间替换首屏的，释放语法树 | `BusyGuard`、`Interactive`（1 MiB 约 165 ms，Linux） |
| B | 1–16 MiB，或有超过 4 KiB 的行 | 首屏两段，之后同一个 `OpenFile` 上 `read_all(16 MiB)` 成为 `Text` | 窗口链从文件开头顺序计算，记下每个窗口的起点（每个 8 字节）；区间只为可见附近的窗口留在 LRU 里 | 可见窗口和前后各一个：`BusyGuard`、`Interactive`；其余：`background_yielding()`、`BatchFront`（ADR 0033 的只存内存的一类，见「对其他 crate 的要求」的 `executor`），每个文件同时一个任务 |
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

- 上表的读取针对 `OpenFile`（工作区文件、依赖文件）。对象库的内容由 `Repo::blob` 整份取出，之后按同样的规则在内存里定位；它的 OID 就是 `expect`，不需要核对。
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
- 一遍扫描 `FileList`（已按字节序，同一目录下的文件连续）得出目录和区间，再逐个目录排序；10 万个文件、约 1 万个目录时估算 5–20 ms、约 0.7 MB（每个文件 4 字节的下标，每个目录约 24 字节加名字）。
- 子模块（`FileKind::Submodule`）是一个文件行，不展开。

## 数据流与线程

```
dyn Reader::open ─▶ [主线程] 阅读视图 ─▶ Engine::open
   ─▶ [Interactive + BusyGuard] SafeRoot::open_file → read_ranges → window::plan → highlight::window（8 ms）─▶ 快照（首屏）
   ─▶ [Background + BusyGuard] read_all / 流式扫描：BlobHasher、行索引、行检查点 → syntax.lines ─▶ 快照（完整、OID）
   ─▶ 档位 A：[Interactive + BusyGuard] FullParse 8 ms 一片 ─▶ 快照（整文件区间）
      档位 B/C：可见窗口 ±1 [Interactive + BusyGuard]；其余 [BatchFront + background_yielding] ─▶ LRU ─▶ 快照
WorktreeBatch（这个路径）─▶ [Background + BusyGuard] stat 比较身份 ─▶ 重新打开（阅读）或标记 dirty_on_disk（编辑）
```

| 工作 | 守卫 | 优先级 |
|---|---|---|
| 首屏：打开、首屏读取、`plan`、窗口解析、可见行的查询 | `BusyGuard` | `Interactive` |
| 档位 A 的整文件解析；档位 B、C 可见窗口和前后各一个；档位 C 可见页的读取 | `BusyGuard` | `Interactive` |
| 完整读取（≤ 16 MiB）、求 OID、行索引、写 `syntax.lines`；档位 C 的流式扫描（每次 1 MiB 的 `read_ranges`，块之间检查取消） | `BusyGuard` | `Background` |
| 工作区文件的规范内容 OID（原始字节的 OID 与 `FileList` 的不同时调 `Worktree::canonical_oid`，给 `TextRef`） | `BusyGuard` | `Background` |
| 档位 B、C 的其余窗口 | `background_yielding()` | `BatchFront`，每个文件同时一个任务，每个窗口之后让出一次 |
| 空闲预取（≤ 4 个文件；≤ 1 MiB 的算整文件，更大的只算第一屏的窗口） | `background_yielding()` | `BatchFront`，每个文件一个任务 |
| 查找（按 1 MiB 分块，块之间检查取消；新的查询取消旧的） | `BusyGuard` | `Interactive` |
| 编辑：增量解析、保存、三方合并 | `BusyGuard` | `Interactive` |
| `fs_watch` 事件：stat 比较、触发重新打开 | `BusyGuard`（`Sink::busy`） | `Background` |

- UI 线程只调用 `Engine::open`、`DocHandle` 的命令和 `snapshot`、`EditSession::changed` / `spans`：入队、取锁复制 `Arc`，不做 IO、解析和 git 操作（AGENTS.md「禁止事项」）。`DocSnapshot::rows` 只做与请求行数成正比的工作：分块里取字节、按区间切片、长行用格数检查点。
- 档位 B、C 的批量窗口满足 ADR 0033「两类只存内存的批量工作」：持有让路的 `BackgroundGuard`、只在批量线程上、结果只进 LRU，暂停、取消或退出最多丢掉在途的一个窗口。解析的进度回调读 `yield_requested()`，在分片边界停下、保留解析器的状态（大文件调研第 3 节「分片解析与恢复」）。
- `OpenFile` 的持有时间（platform 设计「对其他 crate 的影响」）：≤ 16 MiB 的在完整读取之后 drop；> 16 MiB 的到文档关闭或收到这个路径的事件为止，收到事件时重新打开。完整读取的身份与首屏的不同时，首屏结果作废，按完整内容重新定位到同一行号。
- 根目录：`Engine` 按根路径保留一个 `Arc<SafeRoot>`（工作区根经 `SafeRoot::open`，与 `code_intel` 相同），项目关闭时释放。对象库：每个打开的 git 项目保留一个 `git::Repo`（不持有 gix 句柄），`Repo::blob(oid, OBJECT_MAX)` 整份取出。依赖根：`DepRoot::Worktree { dir }` 经工作区根的 `SafeRoot::open_dir(dir)` 打开，路径必须以 `dir` 开头，扩展名必须在 ADR 0033 的允许列表里（共用 `services::code_intel::DEPENDENCY_EXTENSIONS`），否则 `Unreadable`。
- 监听：有文档打开时，为它所在的 worktree 订阅一份 `watch_repo` + `watch_worktree`（`Coalesce::WORKTREE`，`source` 用 `git::Worktree::watch_source`，与 `project`、`code_intel` 共用同一份状态），文档关闭时 drop。`WorktreeBatch` 里有这个路径（或它的上级目录）、或 `rescan` 时，`SafeRoot::stat` 比较身份；保存后自己那次写入的身份不算变化。
- 派生数据的键：区间按 (OID、语言、grammar 版本、查询版本、窗口起点)，行检查点按 OID（架构规则 5，[#42](https://github.com/SoloJiang/lectern-app/issues/42) 的「grammar 版本进键」）。grammar 或查询升级后旧条目不再命中。
- `code_intel` 的焦点：快照的 `oid` 有值后，工作区文件的 `TextRef::oid` 取规范内容的 OID（原始 OID 等于 `FileList` 的就是它，否则 `canonical_oid`，有外部过滤器时不设焦点），对象库取 `expect`，依赖取原始 OID；文档关闭时 `set_focus(Current(None))`。指针在标识符上停留 150 ms 后调 `prefetch(Definition)`。
- 首帧之前不做任何事：插件 `AfterFirstFrame` 激活，`activate` 不加载 grammar、不编译查询、不开 `SafeRoot`；第一次 `open` 时才编译这种语言的查询。

### 编辑

- 进入（⌘E / Edit…）：`editable == Yes` 才可用，即档位 A、完整内容已装入、内容是 UTF-8、换行全是 LF 或全是 CRLF、来源是 `Worktree`。`EditSession` 新建一个 `Parser` 并解析整个文本一次（`Interactive`，档位 A 的上限处约 165 ms，Linux；期间先用阅读视图已有的整文件区间）。视图把 `initial_text` 交给 `InputState`（代码模式，不开软换行），高亮经本 crate 的 `InputHighlighter` 适配器读 `spans()`。
- 输入：适配器在每帧至多一次把缓冲区复制成 `Arc<str>`（档位 A 的上限处约 0.1 ms）连同累计的编辑交给 `changed`；增量解析在后台，换输入时 `Parser::reset`（大文件调研第 3 节）。`spans()` 在 UI 线程上只读结果。
- 保存（⌘S）：`replace_file(rel, bytes, expect)` 在同一目录里写临时文件、`fsync`、比较当前身份、`renameat` 替换（见「对其他 crate 的要求」）。身份不同（agent 在这期间写过）时不写，`SaveResult::Conflict`，视图显示「磁盘上的文件已改动」的一行和三个按钮（见「界面」）。
- 磁盘上的文件变了：缓冲区没有改动时静默重新载入（光标按行号保留）；有改动时只标记 `dirty_on_disk`、显示同一行提示，不自动覆盖任何一方（[交互规范「状态与异常」](interaction.md#状态与异常)）。
- 合并：`git::merge3(base, ours, theirs)`，基准是进入编辑（或上次保存、合并）时的内容。没有冲突时结果写回缓冲区，仍是未保存；有冲突时冲突段带标记写回（`<<<<<<< editor`、`=======`、`>>>>>>> disk`，它们是文件内容、不走 i18n），视图滚到第一个冲突，用户改完再保存。
- 退出（⌘E、Done、Esc）：有未保存的改动时弹出系统的确认（保存 / 不保存 / 取消）。退出后 drop `EditSession`，语法树随之释放（ADR 0034：不计入空闲内存），阅读视图按保存后的内容重新打开同一位置。

## 性能

影响的预算（[性能预算](tech-research.md#性能预算)，M 系列 Mac，系统文件缓存已热）：

| 预算 | 本设计的份额 | 基准 |
|---|---|---|
| 冷启动 20 ms | 首帧之前不做任何事；插件激活只建 `Engine`、登记界面 | `plugin/file-reader/activate`（kernel 的 < 1 ms） |
| 打开文件到首屏高亮 < 16 ms（ADR 0034 的条件：字节偏移、或有检查点、或文件开头） | `syntax` 的首屏（`open_file`、首屏读取、`plan`、窗口解析、可见行的查询）中位数 ≤ 8 ms，最大值由 8 ms 截止时间约束（超时先画文本）；其中首屏读取 < 0.2 ms（`platform/safe_read_range`），`plan` < 0.1 ms。排版和绘制可见的约 60 行 ≤ 6 ms，事件送到 UI 线程 ≤ 2 ms | `syntax/first_screen`（合成 TS、Rust、JSON 1–64 MiB 和压缩成一行的 4 MiB JS，各 40 个随机偏移）、`syntax/first_screen/minified` 只记录；端到端 `app/open_file`（全部场景） |
| 跳转：唯一候选的目标首屏高亮 ≤ 100 ms | 目标带 `Location::byte`，首屏同上 | `app/goto_single`（`code_intel` 第 5 步） |
| 滚动与输入 120 fps | 渲染只做与可见行数成正比的工作；每行排版按（OID 或文档代数、行首字节、显示行、列宽）的哈希用 `shape_line_by_hash`，本 crate 另留约 4 倍可见行数的最近行缓存；档位 B、C 的窗口在后台算好 | `app/scroll_frames`（含 64 MiB、压缩成一行、编辑模式 1 MiB 的场景）、`app/scroll_frames_indexing` |
| 后退、前进 < 16 ms | 历史记下 OID、身份和字节偏移；回到 ≤ 1 MiB 的文件时 stat 比较身份相同就用 LRU 里的文本和区间，不读文件 | `app/nav_back` |
| 打开 Files、展开目录 < 16 ms | 目录表已建好时，打开只取快照、按展开状态算可见行；展开 2,000 个子项的目录是一次插入 | `app/files_open`、`app/files_expand` |
| 进入编辑 ≤ 100 ms | 1 MiB、没有长行的文件：`InputState::set_value` 与换行信息在 UI 线程上约 3 ms（ADR 0034），首次整文件解析在后台，不等它 | `app/enter_edit` |
| 空闲内存（采样状态：没有打开文件） | 插件常驻 ≤ 0.5 MB：`Engine`、空的 LRU、语言表（未编译的查询）；LRU ≤ 8 MiB、编译过的查询、打开的文档只因阅读才有，不在采样状态里（ADR 0035） | `plugin/file-reader/resident`；`syntax/lru_bytes` |
| 阅读时的内存（不设顶层预算） | 打开的文档：≤ 16 MiB 的文本，或 ≤ 8 MiB 的页；行索引每 1024 行 8 字节加长行的格数检查点；窗口计算的临时内存（一个窗口和它的树）≤ 8 MiB；编辑模式的语法树 ≤ 41 MiB，退出即释放 | `syntax/window_peak` ≤ 8 MiB、`syntax/edit_tree_bytes` ≤ 41 MiB |
| 目录表（`project` 的份额） | 10 万文件 ≤ 20 ms、≤ 1.5 MB，计入 `plugin/project/resident_100k` < 16 MB | `project/dirs_100k`、`project/dirs_100k_bytes` |
| 安装包 ≤ 80 MB | 四个新 grammar 的解析表（`tree-sitter-md` 最大），由 [#38](https://github.com/SoloJiang/lectern-app/issues/38) 测量 | `app/bundle_size` |

只记录的基准：`syntax/full_parse_1mib`（档位 A 的整文件解析）、`syntax/chain_16mib`（档位 B 的整条窗口链）、`syntax/scan_64mib`（档位 C 的流式扫描）、`syntax/find_16mib`。

- 以上分项是 `Tier::Share`，实现它们的 PR 把条目加进 [bench 设计「预算表」](bench.md#预算表)和 `crates/bench/src/budgets.rs`，出处指向本节。顶层条目 `app/open_file`、`app/enter_edit`、`app/files_open`、`app/files_expand`、`app/nav_back`、`app/scroll_frames` 已在预算表里列为计划中，由「验收」第 7、8 步改为生效。
- 首屏的 8 + 6 + 2 ms 是从 16 ms 分出的份额：首屏窗口在 Linux 上平均 2.5–3.3 ms（合成源码）、压缩 JS 8–12 ms（大文件调研「首屏路径的准备工作」），M 系列按 1.2–1.7 倍估计。

## 界面

### 阅读视图

- 收起层（[交互规范「三层结构」](interaction.md#三层结构)）：⇧⌘O 从审阅、抽屉、⌘K、Files 打开，占据内容区（不受阅读栏 760pt 的宽度限制，代码按行不折）；Esc 回到打开它的视图。
- 工具栏：标题是文件名，副标题是相对路径（依赖文件另加包名和版本，如 `react 19.3.1`；对象库的内容另加 `file-reader-from-history` 的来源说明）；左侧是后退 / 前进的两段按钮（⌘[ / ⌘]，与审阅页左侧的返回按钮同一位置）；右侧是 Edit…，编辑时换成 Done 和 Save，不超过 3 个控件。
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
| `file_reader.unsaved.save`、`.dont_save`、`.cancel` | 对话框按钮 | — |
| `files.tree` | 文件树 | 展开的目录数 |
| `files.tree.row[<路径>]` | 目录或文件行 | 展开、选中、是否折叠的目录链 |

### Action 与快捷键

| Action | 键 | 说明 |
|---|---|---|
| `file_reader::Edit` | ⌘E | 进入或退出编辑 |
| `file_reader::Save` | ⌘S | 只在编辑时 |
| `file_reader::Find`、`FindNext`、`FindPrevious` | ⌘F、⌘G、⇧⌘G | |
| `file_reader::Back`、`Forward` | ⌘[、⌘] | 后退到历史的起点之后，⌘[ 回到打开阅读视图之前的视图（[交互规范「快捷键」](interaction.md#快捷键)） |
| `file_reader::Close` | Esc | 关闭查找栏；没有时回到打开它的视图 |
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
- `deadline_*`：`highlight::window` 在截止时间后返回 `TimedOut`；`FullParse` 分片后的区间与一次解析相同；`Yielded` 之后继续得到相同的结果；取消标志生效。
- `degrade_*`：NUL、无效 UTF-8、`SLOW_WINDOW` 连续两次（用可控的时钟）都得到 `Plain`。
- `text_*`：行索引（`line_start`、`line_of` 与朴素扫描对照）、CRLF、BOM、长行的格数检查点（CJK、制表符）、`rows` 在 64 MiB 的合成内容上只访问可见行（计数的 `Source`）。
- `lines_*`：`LineCheckpoints` 的编码、≥ 1 MiB 才写、`len` 不同时不用。
- `doc_*`：用临时目录的真实文件和 `Executor::manual()`：四种 `Anchor`、完整读取的身份与首屏不同时作废首屏、`Byte` 的 OID 与 `expect` 不同时按行号重新定位、档位 C 的页 LRU 上限、`Symlink` / `Unsupported` / `Missing` / `PathThroughSymlink`、依赖根越界（`..`、指向根外的符号链接、不允许的扩展名）、对象库的 blob、fs_watch 的 `FakeBackend` 注入事件后重新打开、文档 drop 后任务取消和 `OpenFile` 关闭（描述符计数）。
- `lru_*`：8 MiB 上限、预取与批量窗口先淘汰、正在显示的条目不受淘汰影响。
- `prefetch_*`：至多 4 个、让路开关打开时停在分片边界（kernel 的 `set_yield`、`ManualClock`）、结果只进 LRU。
- `health_*`：一个永不返回的假解析（测试里阻塞在条件变量上的 `Source`）超过截止时刻 1 s 后，同一 grammar 版本的 `enter` 返回 None、`allows` 为 false，其他语言不受影响。
- `edit_*`：增量解析的区间与整文件解析相同；`changed` 之后 `spans` 立即返回平移后的区间；CRLF 与 BOM 在保存时恢复。
- `save_*`、`merge_*`：身份变化时 `Conflict` 且文件内容不变；`save_anyway` 照常写入；合并结果的快照（无冲突、有冲突、两边改同一行）。
- `rg -n 'std::fs::|File::open|mmap' crates/syntax/src --glob '!**/tests*'` 没有结果；`rg -n 'thread::spawn' crates/syntax/src` 没有结果。

`file_reader`（依赖 GPUI，只在 macOS 上）：

- `#[gpui::test]`：元素只为可见的显示行排版（计数的排版钩子）；64 MiB 文件的滚动位置在 `RowPos` 与像素之间换算、拖动滚动条到末尾；长行折行与改变宽度后的重新折行；⌘F 的结果跳转；⌘[ / ⌘] 的历史；进入编辑、改动后 Esc 弹出确认、保存冲突时出现提示行；Files 的展开、折叠、目录链、定位当前文件、Finder 顺序。测试直接构造 `DocSnapshot` 和 `DirTable`，不做文件 IO。
- UI 探针完成后（[#6](https://github.com/SoloJiang/lectern-app/issues/6)），阅读视图、编辑模式、Files 的语义树用 insta 快照验收；减弱动态效果时查找栏的展开没有位移。

`project`：`dirs_*`（目录表与朴素构造对照、Finder 顺序、折叠链、子模块、空列表、反序列化乱序的输入）；`finder_cmp` 的表驱动测试（`file2` < `file10`、`A` 与 `a`、数字前导零）。`platform`：`replace_file_*`（见「对其他 crate 的要求」）。`git`：`merge3_*`。

夹具：只用合成的源码，由 `bench` 的生成器按大文件调研的形状生成（TS、Rust、JSON、压缩 JS、64 MiB），放在 `target/` 下，不提交。

## 对其他 crate 的要求

以下改动各由对应 crate 的 PR 完成，写 `Refs #10`，在「验收」里列为前置条件。

- `services`：`reader` 模块（「接口」）；`project` 模块的 `DirTable`、`DirId`、`finder_cmp`、`ActiveTree::dirs`、`ChangedParts::dirs`；`code_intel` 模块增加 `pub const DEPENDENCY_EXTENSIONS: &[&str]`（ADR 0033 允许的扩展名，`code_intel` 的依赖层改用它，两边只有一份）。
- `project`：文件列表换代之后，在 `Background` 任务里（`BusyGuard`）建目录表、单独一代发布；`plugin/project/resident_100k` 的份额在同一个 PR 里核对（目录表 ≤ 1.5 MB，合计仍 < 16 MB）；新增 `project/dirs_100k`、`project/dirs_100k_bytes`。
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

  比较身份和 `renameat` 之间仍有一个很短的窗口（「已知限制」）。测试 `replace_file_*`：内容与权限、身份变化时不替换且临时文件不留下、中间一级是符号链接、`rel` 是符号链接或目录、并发的 `race_replace_never_escapes`。platform 设计「范围」的「写文件」一条改为由这个接口和 #66 共用打开逻辑。
- `git`：三方合并，纯计算，用已有的 `gix::diff::blob`（Histogram），不新增依赖。

  ```rust
  /// 按行的三方合并。两边改了同一段时为冲突，冲突段按 `markers` 写出两边的内容。纯计算，任何线程。
  pub fn merge3(base: &[u8], ours: &[u8], theirs: &[u8], markers: &Markers<'_>) -> Merge3;
  pub struct Markers<'a> { pub ours: &'a str, pub theirs: &'a str }
  pub struct Merge3 { pub bytes: Vec<u8>, pub conflicts: Vec<Range<usize>> }
  ```

  测试 `merge3_*` 对照 `git merge-file` 在同一组合成输入上的结果（测试里的假命令按夹具回放，见 AGENTS.md「工作方式」）。
- `executor`：批量线程上的先后。ADR 0033 要求空闲预取排在 `code_intel` 第 3 层之前，大文件的批量窗口与它们在同一个队列里；`Batch` 队列现在是先进先出，批量线程又被 `code_intel` 的长期任务占着。增加 `Priority::BatchFront`：只在批量线程上运行（与 `Batch` 相同的线程组、QoS 和闸门规则），批量线程取任务时先取它。`syntax` 的预取和批量窗口用它，每个任务做完一个窗口或一个文件就结束或让出（返回一次 `Pending`）。测试：`BatchFront` 排在已入队的 `Batch` 之前被取走，工作线程从不取它。
- `code_intel`（后续 PR，不阻塞本 issue 的各步）：
  - 批量任务在每个批次边界让出一次线程（返回一次 `Pending`、重新入队），所以排队的 `BatchFront` 至多等一批（≤ 50 ms，ADR 0029 的批次上限）就能开始。已经这样做的不用改。
  - grammar 的卡死检测共用 `syntax::health`：抽取和 `ParsedFile::parse` 之前 `enter`，返回 None 时按语言不可用处理（[#42](https://github.com/SoloJiang/lectern-app/issues/42) 的 scanner 一条，code_intel 设计「对其他 crate 的要求」的高亮方一条）。
  - 测试 `lang_extensions_agree_with_code_intel`：两边共有的语言，扩展名表相同。
  - 依赖层的扩展名改用 `services::code_intel::DEPENDENCY_EXTENSIONS`。
- `workspace`（#30 的设计）：`dyn Contributions` 提供侧栏面板（Files）、内容区视图（阅读视图）的注册，以及「回到打开它的视图」的导航栈；本 crate 按它的接口注册。
- `ui`（#6 的设计）：`TokenKind` 到系统色的映射（浅色 / 深色各一组，与 agent 标识色一样通过色觉辨识校验），等宽字体 SF Mono 12pt 的格宽与行高，展开原语。
- `bench`：探针增加阅读视图首屏画出、编辑可输入、Files 展开完成的 `Milestone`；生成器增加大文件调研的合成形状。

## 未决问题

1. **保存时是否运行项目的 formatter。** [技术调研「编辑范围」](tech-research.md#编辑范围第三层)写了「保存时调用项目已有的 formatter」。formatter 来自工作区（如 `node_modules/.bin/prettier`），被审阅的 agent 可以改写它，保存一次就会以用户的身份运行工作区里的程序。选项：(a) 保存时自动运行；(b) 只在用户执行 Format 命令时运行，命令列出将要运行的程序；(c) 本 issue 不做，保存只写缓冲区。默认 (c)：本设计的保存不运行任何程序，(a) 或 (b) 等人决定后另开 issue。
2. **Files 是否列出未跟踪的文件。** issue 写明文件列表读 git index，agent 新建、还没 `git add` 的文件因此不在 Files 里（它们在审阅的改动里能看到，⌘K 的语料也有同样的问题，[#102](https://github.com/SoloJiang/lectern-app/issues/102)）。选项：(a) 只列 index 里的文件；(b) 另开 issue，由 `project` 在 `FileList` 之外给出当前 worktree 的未跟踪、未被忽略的路径，Files 和 ⌘K 共用。默认 (a)，倾向之后做 (b)。

## 验收

前置条件（其他 crate 的 PR，写 `Refs #10`）：

- A. `services`：`reader` 模块、`DirTable`、`ActiveTree::dirs`、`DEPENDENCY_EXTENSIONS`；`rg -n 'pub trait Reader' crates/services/src` 有一处，`dyn Reader` 的 `ID == "reader"`、`MULTIPLE == false` 的测试通过。
- B. `project`：目录表；`dirs_*` 测试通过，`project/dirs_100k`、`project/dirs_100k_bytes`、`plugin/project/resident_100k` 在预算内。
- C. `platform`：`SafeRoot::replace_file`；`replace_file_*` 测试通过。
- D. `git`：`merge3`；`merge3_*` 测试通过。
- E. `workspace` 的 `dyn Contributions`（#30）、`ui` 的代码配色与等宽度量（#6）：本 crate 第 7 步之前。
- F. `executor`：`Priority::BatchFront`；`code_intel` 的批量任务在批次边界让出线程（已经这样做的只补测试）。第 4 步之前。

本 issue 分八步实现，每步一个 PR，写 `Refs #10`，最后一步写 `Closes #10`。每一步都要 `just check`、`just test`（`syntax` 的步骤加 `-p lectern-syntax`）、`just deny` 通过，并把这一步的 bench 条目加进 `budgets.rs`、改为生效。

1. **`syntax` 骨架、语言表与整文件高亮**（不需要前置条件）：新建 `crates/syntax`（`layer = "domain"`，带 `AGENTS.md` 与 `CLAUDE.md -> AGENTS.md`，[cargo-workspace 设计](cargo-workspace.md)的 crate 表加一行）；根 `Cargo.toml` 按 ADR 0041 加四个 grammar；高亮查询放 `crates/syntax/queries/<语言>/highlights.scm`，文件头写上游版本、commit 和 Lectern 的改动。
   - `lang_*`、`spans_*`、`health_*` 测试通过；每种核心语言至少一份 insta 快照。
   - `cargo tree -e features -i tree-sitter` 里没有 `wasm`；`cargo tree --duplicates -e normal` 里没有 `tree-sitter`（只有 0.27.1 一份）；`cargo tree -e features -i tree-sitter-md` 里没有 `parser`。
   - `python3 scripts/check_architecture.py` 通过：`syntax` 不依赖 GPUI。
2. **窗口**：`window::*`、`highlight::window`、`FullParse`、`chain_window`、降级。
   - `window_*`、`consistency_*`、`deadline_*`、`degrade_*` 测试通过。
   - `syntax/first_screen`（只算 `plan` 与解析的部分，读取在第 3 步加上）、`syntax/window_peak` 在预算内；`syntax/full_parse_1mib`、`syntax/chain_16mib` 有结果。
3. **文本与文档**（前置：无新的；用已有的 `open_file`、`BlobHasher`）：`text::*`、`LineCheckpoints` 与 `syntax.lines`、`Engine`、`DocHandle`、`DocSnapshot`、LRU、监听与重新打开、查找。
   - `text_*`、`lines_*`、`doc_*`、`lru_*` 测试通过；[cache 设计「命名空间」](cache.md#命名空间)的 `syntax.lines` 一行与实现一致。
   - `syntax/first_screen`（含首屏读取）、`syntax/lru_bytes` 在预算内；`syntax/scan_64mib`、`syntax/find_16mib` 有结果。
   - `rg -n 'std::fs::|File::open|mmap' crates/syntax/src --glob '!**/tests*'` 没有结果。
4. **预取与让路**（前置 F）：`Engine::prefetch`，档位 B、C 的批量窗口接 `background_yielding()` 和 `BatchFront`。
   - `prefetch_*` 测试通过，其中 `prefetch_runs_before_queued_batch_work`：批量线程被 `Batch` 的长期任务占满时，预取在一批之内开始；让路开关打开后在途窗口在一片之内停下（同 `code_intel/yield_stop` 的测法，≤ 10 ms）。
5. **编辑的领域部分**（前置 C、D）：`EditSession`、保存、冲突、合并。
   - `edit_*`、`save_*`、`merge_*` 测试通过；`syntax/edit_tree_bytes` ≤ 41 MiB。
6. **插件与服务**（前置 A）：本 crate 加 `plugin = true`，`FileReaderPlugin`、`dyn Reader`、`set_focus` / `prefetch` 的调用。
   - `python3 scripts/check_architecture.py` 通过：本 crate 不依赖其他插件 crate，只有 `app` 和 tool 层依赖它；`rg -n 'tree-sitter' crates/file_reader/Cargo.toml` 没有结果。
   - `plugin/file-reader/activate` < 1 ms、`plugin/file-reader/resident` ≤ 0.5 MB。
7. **阅读视图与 Files**（前置 B、E）：自绘元素、查找栏、前进后退、Files 面板、Fluent 资源、探针节点。
   - macOS 上 `just test -p lectern-file-reader` 的 `#[gpui::test]` 通过；`rg -n 'std::fs|SafeRoot|read_ranges' crates/file_reader/src` 没有结果（视图不做 IO）。
   - `app/open_file`（全部场景）、`app/scroll_frames`（64 MiB、压缩成一行）、`app/scroll_frames_indexing`、`app/nav_back`、`app/files_open`、`app/files_expand` 在 macOS CI 上改为生效并在预算内。
8. **编辑模式**（前置 C、D，第 5 步）：`InputState`、`InputHighlighter` 适配器、提示行与确认对话框。写 `Closes #10`。
   - `#[gpui::test]` 的编辑场景通过；`rg -n 'features.*tree-sitter' Cargo.toml crates/file_reader/Cargo.toml` 没有 gpui-component 的 `tree-sitter` feature。
   - `app/enter_edit` ≤ 100 ms、编辑模式的 `app/scroll_frames` 在预算内；超标时按 ADR 0034 降低 `TIER_A_MAX`，在本文「接口」改常量。

## 已知限制

- 首屏窗口的起点是启发式的，落在多行注释或模板字符串里时首屏高亮可能出错，直到整文件或窗口链的区间到达（ADR 0034）。
- 需要规范化转换的工作区文件（CRLF、`working-tree-encoding`、`ident`）：`Byte` 目标先按规范内容的偏移打开，完整读取后按行号重新定位一次；行检查点查不到，只给行号的首屏退回文件开头。
- 外部 scanner 卡住时，那个 executor 线程一直被占用，直到 scanner 返回；之后同一 grammar 版本本次运行只显示纯文本（ADR 0041）。
- Markdown 只高亮块级结构，行内的强调、代码段和注入的语言（代码块里的代码、模板字符串里的 HTML）不高亮。
- `finder_cmp` 与 Finder 的 `localizedStandardCompare` 在重音字符和区域规则上可能不同。
- 制表符固定按 4 格计，不读 `.editorconfig`。
- 对象库里的 blob 只能整份取出（`Repo::blob`）：首屏要等解压完，几 MiB 的 blob 可能超过 16 ms；超过 16 MiB 的不打开。
- 保存时比较身份与 `renameat` 之间有一个很短的窗口，这期间别的进程写入的内容会被覆盖；原子替换会断开硬链接、不保留扩展属性。
- 文件夹项目的文件在阅读时不自动刷新（`project` 不监听文件夹项目）。
- 查找只对 ASCII 字母不区分大小写，最多 10,000 个结果。
