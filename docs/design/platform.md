# platform 设计

状态：安全读取已实现（设计 [#73](https://github.com/SoloJiang/lectern-app/pull/73)，实现 [#74](https://github.com/SoloJiang/lectern-app/pull/74)，macOS 13+ 的 `stat` 改为一次系统调用 [#88](https://github.com/SoloJiang/lectern-app/pull/88)）。FSEvents 与 kqueue 后端（模块 `fs_events`，设计在 [fs_watch 设计「platform 后端」](fs_watch.md#platform-后端)）已实现（[#90](https://github.com/SoloJiang/lectern-app/pull/90)），实测结果见 fs_watch 设计的「实际验证」。进程的 `phys_footprint` 已实现（[#91](https://github.com/SoloJiang/lectern-app/pull/91)，见「进程」）。列目录（`SafeRoot::list`，[ADR 0028](../adr/0028.md)）随 [#87](https://github.com/SoloJiang/lectern-app/pull/87) 加入。`SafeRoot::open_dir`（[ADR 0033](../adr/0033.md)）已实现（[#124](https://github.com/SoloJiang/lectern-app/pull/124)）。范围读取（`SafeRoot::open_file`、`OpenFile`，[#103](https://github.com/SoloJiang/lectern-app/issues/103)，见「范围读取」）已实现（设计 [#125](https://github.com/SoloJiang/lectern-app/pull/125)，实现 [#PR](https://github.com/SoloJiang/lectern-app/pull/PR)）。

对应 issue [#14](https://github.com/SoloJiang/lectern-app/issues/14)。`platform` 收纳只能按操作系统实现的能力（架构规则 6）。本文目前只覆盖其中的安全读取：从一个目录根出发，按相对路径读取文件的元数据和内容，路径的任何一级都不跟随符号链接，也不越出根目录。`git` 读工作区文件只经这个接口（[git 设计「安全读取」](git.md#安全读取)、[ADR 0024](../adr/0024.md)），它也是 Analyze 快照「写出时不跟随符号链接，不读到 worktree 之外的文件」的基础（[ADR 0019](../adr/0019.md) 约束 2）。

## 范围

- 做（本文）：
  - `SafeRoot`：持有一个目录根的描述符，按 `RelPath` 做不跟随符号链接的 `stat`、读取和列目录（`list`，`git` 遍历未跟踪文件用，ADR 0028）。
  - 范围读取（`SafeRoot::open_file`、`OpenFile`）：打开一个普通文件、保留描述符，按字节区间用 `pread` 读取，返回文件的身份供之后的完整读取核对；`file_reader` 的首屏和超过 16 MiB 的文件的分页用它（[ADR 0034](../adr/0034.md)，见「范围读取」）。
  - `RelPath`：根目录之下的相对路径，构造时校验，排除 `..`、`.`、空名字、NUL 和绝对路径。
  - `Meta`、`FileKind`：git 的 stat 比较需要的全部字段。
  - 读取结果的分类：普通文件、符号链接（读出目标，不跟随）、超过上限、不读取的类型（目录、FIFO、socket、设备文件）；错误分为不存在、路径中间是符号链接、其他 IO 错误。
  - 三个实现：Linux 的 `openat2`、macOS 的 `O_NOFOLLOW_ANY` 与 `AT_SYMLINK_NOFOLLOW_ANY`、两个平台共用的逐级 `openat`（Linux 没有 `openat2` 时的后备，也是出错时判定原因的实现）。
  - `process::phys_footprint`：一个进程在活动监视器里的「内存」，供 `lectern-bench` 测量（见「进程」）。
- 不做：
  - 递归遍历、忽略规则和未跟踪文件的判定：`git` 在 `list` 之上实现（ADR 0028）；本模块只列一层目录。
  - 路径的大小写折叠与 Unicode 规范化：`git` 按 `core.ignoreCase`、`core.precomposeUnicode` 处理；本模块把字节原样交给文件系统。
  - 内置转换、OID 计算、stat 比较本身、按路径缓存：`git`。
  - 写文件、克隆文件：随 Analyze 快照（[#66](https://github.com/SoloJiang/lectern-app/issues/66)）设计，复用本模块的打开逻辑（见「数据流与线程」）。
- 本文不覆盖，以后在本文增加小节或另写设计文档，设计合并之前不写对应的代码：
  - FSEvents 与 kqueue 后端（实现 `fs_watch` 的接口，模块 `fs_events`）：设计、实际验证和与设计的差异都在 [fs_watch 设计「platform 后端」](fs_watch.md#platform-后端)。
  - `open -a`、钥匙串、代码签名校验、线程 QoS（[executor 设计](executor.md)的线程启动钩子）。
  - Seatbelt 配置、受限进程的启动与监护、磁盘映像、快照写出（[#27](https://github.com/SoloJiang/lectern-app/issues/27)、[#39](https://github.com/SoloJiang/lectern-app/issues/39)、[#66](https://github.com/SoloJiang/lectern-app/issues/66)，ADR 0019）。
  - Claude Code statusline 的安装与恢复（[#32](https://github.com/SoloJiang/lectern-app/issues/32)、[ADR 0014](../adr/0014.md)）。

## 依赖

| 依赖 | 用途 | 选用理由 |
|---|---|---|
| `rustix` 1.1（`default-features = false`，`std`、`fs`） | `openat2`、`openat`、`fstatat`、`fstat`、`readlinkat`、`read` | 见下 |
| `tempfile`（dev） | 测试用的临时目录 | 已在 Cargo.lock（3.27，经 `insta`），MIT OR Apache-2.0 |
| `libc`（只在 macOS 上） | `proc_pid_rusage`（`process::phys_footprint`）；测试里创建 FIFO（rustix 在 Apple 平台没有 `mkfifoat`） | 已在 Cargo.lock（0.2.190）；rustix 没有 `proc_pid_rusage` |
| `lectern-fs-watch` | `fs_events` 实现它的 `Backend`；事件转换是纯函数，在所有平台上编译和测试 | 基础设施层，方向向下（[fs_watch 设计「依赖」](fs_watch.md#依赖)） |
| `lectern-executor`（只在 macOS） | kqueue 线程经 `run_dedicated` 启动 | 后台线程只由 `executor` 管理（ADR 0024） |
| `rustix` 的 `event` feature（只在 macOS） | `fs_events` 的 kqueue | 同一个 crate 的另一个 feature，不新增 crate |
| `lectern-kernel`（dev，只在 macOS） | 经真实 `Watcher` 驱动后端的测试实现 `Sink` 要用 `BusyGuard` | kernel 层 |

选 `rustix`，不选 `libc`、`nix`、`cap-std`：

- 已在依赖图里：Cargo.lock 已有 rustix 1.1.5，两个 target（`aarch64-apple-darwin`、`x86_64-unknown-linux-gnu`）上都经 GPUI 的 `async-io` / `polling` 打开了 `fs` feature，所以发布构建不新增任何 crate；gix-index 在 unix 上也以同样的 feature（`std`、`fs`）依赖它，`git` 接入 gix 后同样共用。许可证 `Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT`，在 `deny.toml` 的允许列表里。
- 接口是安全的：返回 `OwnedFd`，错误是 `Errno`，`openat2` 和 `ResolveFlags` 有类型化的封装，Apple 平台有 `OFlags::NOFOLLOW_ANY`。`fstatat` 的 `AT_SYMLINK_NOFOLLOW_ANY`（`<sys/fcntl.h>` 里的 `0x0800`，`fstatat` 从 macOS 13 起接受，更早的版本返回 `EINVAL`）rustix 1.1 和 libc 0.2 都没有定义，用 `AtFlags::from_bits_retain(0x0800)` 传入：`AtFlags` 保留它没有命名的位（`const _ = !0`），原样交给 `fstatat`。本模块因此不需要 `unsafe`。`libc` 要对每个调用写 `unsafe`，`openat2` 只能经 `syscall(SYS_openat2, …)` 和手工填写的 `open_how`。
- Linux 上 rustix 默认直接发系统调用（`linux_raw` 后端），`openat2` 不依赖 glibc 是否提供包装函数。
- `nix`、`cap-std` 不在 Cargo.lock 里；`cap-std` 自带一套能力模型和路径解析，超出这里需要的四个系统调用。

不写 ADR：rustix 不改变架构和依赖方向，不进入任何公开接口（公开类型都是本 crate 自己定义的），发布构建不新增 crate，以后换成 `libc` 只改本模块。根 `Cargo.toml` 的 `[workspace.dependencies]` 加 `rustix = { version = "1.1", default-features = false, features = ["std", "fs"] }`，`crates/platform/Cargo.toml` 用 `workspace = true` 引用。

所在层与依赖方向见 [crate AGENTS.md](../../crates/platform/AGENTS.md)：基础设施层，不依赖 GPUI。安全读取不是 macOS 专属能力：模块在所有 unix target 上编译，macOS 专属的部分（`O_NOFOLLOW_ANY`、`AT_SYMLINK_NOFOLLOW_ANY`）放在 `#[cfg(target_os = "macos")]` 后面，Linux 专属的部分（`openat2`）放在 `#[cfg(target_os = "linux")]` 后面，两者都只出现在本 crate（架构规则 6）。

## 接口

模块 `lectern_platform::safe_read`，下列公开项在 crate 根重新导出。

```rust
/// 一个目录根的描述符。`Send + Sync`，不可克隆，调用方需要共享时放进 `Arc`。
/// 绑定的是打开时的那个目录：根路径之后被改名或替换，调用仍作用在原来的目录上（目录已删除时为 `NotFound`）。
#[derive(Debug)]
pub struct SafeRoot { /* fd: OwnedFd, backend: Backend */ }

impl SafeRoot {
    /// 打开根目录（`O_RDONLY | O_DIRECTORY | O_CLOEXEC`）。根路径本身按普通规则解析，其中的符号链接照常跟随
    /// （调用方传入规范路径，如 `/tmp` → `/private/tmp`）；限制只作用在根目录之下的相对路径上。
    /// Linux 上在这里选定实现（见「实现」）。
    pub fn open(root: &Path) -> io::Result<SafeRoot>;

    /// 不跟随任何一级符号链接的 stat。最后一级是符号链接时返回链接本身的元数据（`FileKind::Symlink`），
    /// 是目录时返回目录的元数据。
    pub fn stat(&self, rel: RelPath<'_>) -> Result<Meta, SafeError>;

    /// 读取一个条目。普通文件读出全部内容（不超过 `max` 字节）；最后一级是符号链接时读出目标，不跟随；
    /// 目录、FIFO、socket、设备文件不读内容。普通文件的类型判断和读取用同一个描述符。
    pub fn read(&self, rel: RelPath<'_>, max: u64) -> Result<Read, SafeError>;

    /// 列一个目录（`dir` 为 None 时是根目录）：名字（不含 `.`、`..`）和类型，不排序。符号链接作为条目返回，不跟随。
    /// 目录本身和路径的任何一级是符号链接时为 `PathThroughSymlink`；不是目录时为 `Io(NotADirectory)`。
    /// 名字从打开的那个描述符读出，路径在读取期间被替换也不会列出根目录之外的目录。
    pub fn list(&self, dir: Option<RelPath<'_>>) -> Result<Vec<DirEntry>, SafeError>;

    /// 在这个根之下打开一个子目录作为新的根（ADR 0033：工作区里的依赖根如 `node_modules`、`.venv`）。
    /// 与 `list` 共用同一个打开函数（一步打开、`fstat` 确认是目录、`Walk` 后备）：路径的任何一级（含这个目录本身）
    /// 是符号链接为 `PathThroughSymlink`，不是目录为 `Io(NotADirectory)`，不存在为 `NotFound`。
    /// 返回的根绑定到打开的那个目录描述符，实现与原来的根相同，之后的调用不再经过 `rel`；
    /// 在它之下的调用与原来的根遵守同样的规则（不跟随符号链接、`..` 和绝对路径不是 `RelPath`）。不读目录项。
    pub fn open_dir(&self, rel: RelPath<'_>) -> Result<SafeRoot, SafeError>;

    /// 打开一个条目供范围读取：打开、`fstat` 与类型判断同 `read`，普通文件返回持有描述符的 `OpenFile`，不读内容。
    /// 与其他方法不同，打开的描述符不在返回前关闭，而是转交给 `OpenFile`，在它 drop 时关闭（见「数据流与线程」的描述符生命周期）。
    /// 见「范围读取」。
    pub fn open_file(&self, rel: RelPath<'_>) -> Result<Opened, SafeError>;
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DirEntry {
    /// 原始字节，不是空名、`.`、`..`，不含 `/` 和 NUL。
    pub name: Vec<u8>,
    /// 来自目录项（`d_type`）；文件系统不记录时对同一个目录描述符 `fstatat(AT_SYMLINK_NOFOLLOW)`。
    pub kind: FileKind,
}

/// 根目录之下的相对路径，原始字节，以 `/` 分隔。不变量：非空；各级名字非空；没有 `.`、`..`；没有 NUL；
/// 不以 `/` 开头（所以也不以 `/` 结尾、没有连续的 `/`）。字段私有，只能经 `new` 构造。
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct RelPath<'a>(&'a [u8]);

impl<'a> RelPath<'a> {
    /// 一次线性扫描；不满足不变量时为 None。不检查长度（超长时系统调用返回错误）。
    pub fn new(bytes: &'a [u8]) -> Option<RelPath<'a>>;
    pub fn as_bytes(self) -> &'a [u8];
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Read {
    File { meta: Meta, bytes: Vec<u8> },
    /// `target` 是 `readlinkat` 读出的目标字节，不受 `max` 限制（长度受系统的路径长度上限约束）。
    Symlink { meta: Meta, target: Vec<u8> },
    /// 普通文件超过 `max`：没有返回内容。
    TooLarge { meta: Meta },
    /// 不读内容的类型：`meta.kind` 是 `Dir`、`Fifo`、`Socket`、`CharDevice`、`BlockDevice` 或 `Other`。
    Unsupported { meta: Meta },
}

#[derive(Debug)]
pub enum SafeError {
    /// 路径不存在：某一级不存在（ENOENT），或中间某一级存在但不是目录也不是符号链接（ENOTDIR）。
    /// 与 git 的 `is_missing_file_error` 相同，两种情况都按「工作区里没有这个文件」处理。
    NotFound,
    /// 中间某一级是符号链接。最后一级是符号链接不是错误（`stat` 返回它的元数据，`read` 返回 `Read::Symlink`）。
    PathThroughSymlink,
    /// 其他错误：权限（`PermissionDenied`）、名字过长、IO 错误、内存不足等，保留原始的 `io::Error`。
    Io(io::Error),
}

// 实现 `Display` 和 `std::error::Error`（`source` 是 `Io` 里的错误）。
// 实现 `From<SafeError> for io::Error`，供只需要传播错误的调用方用：`NotFound` → `ErrorKind::NotFound`，
// `PathThroughSymlink` → `ErrorKind::Other`（payload 仍是 `SafeError`），`Io(e)` → `e`。
```

`stat` 和 `read` 都阻塞（系统调用和磁盘 IO），在调用方的后台任务线程上执行，UI 线程上不调用（AGENTS.md「禁止事项」）。可以从多个线程同时调用同一个 `SafeRoot`。

### 错误类型的选择

用专门的 `SafeError` 枚举，不用带自定义 payload 的 `io::Error`：

- 调用方要区分的三种结果都有固定的处理（见下表），枚举让编译器检查每种情况都处理了；`io::ErrorKind` 不能扩展，`PathThroughSymlink` 只能放进 `ErrorKind::Other` 的 payload，调用方要 `get_ref()` 再 `downcast_ref`，漏判时编译器不报错。
- 同一个现象在不同实现里的 errno 不同（`openat2` 的 `RESOLVE_NO_SYMLINKS` 和 `O_NOFOLLOW_ANY` 返回 `ELOOP`，逐级 `openat` 遇到符号链接时可能是 `ELOOP` 或 `ENOTDIR`），本模块先判定原因再返回，调用方不看 errno。
- 「不存在」作为单独的变体：它是工作区比较里最常见的结果，不应该经过 `io::Error` 的分配和 kind 比较。

| 结果 | `git` 的处理（[git 设计「工作区比较」](git.md#工作区比较)） |
|---|---|
| `Err(NotFound)` | W 为没有 |
| `Err(PathThroughSymlink)` | `Unavailable::PathThroughSymlink` |
| `Err(Io(_))` | `Unavailable::Unreadable` |
| `Read::Unsupported`，`kind` 为 `Dir` | 按 index 条目区分：gitlink 按子模块，其余是路径被换成目录 |
| `Read::Unsupported`，其余 `kind` | `Mode::Special` + `UnsupportedType` |
| `Read::TooLarge` | `Content::Unhashed { size, mtime_ns, inode }`，取自 `meta` |

`SafeRoot::open` 返回普通的 `io::Result`：打开根目录是一次普通的打开，错误照常传播（根目录不存在时 `git` 按 prunable 的 worktree 处理）。

## 数据结构

```rust
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Meta {
    pub kind: FileKind,
    /// `st_mode` 的全部位（类型位和权限位）；macOS 的 `u16` 扩宽为 `u32`。
    pub mode: u32,
    /// `st_size`。符号链接是目标的字节数。
    pub size: u64,
    pub mtime: Timestamp,
    /// `st_ctime`：inode 状态的改变时间，不是创建时间（与 git 和 gix-index 相同）。
    pub ctime: Timestamp,
    /// `st_dev`；macOS 的 `dev_t` 是 `i32`，按 `as u32 as u64` 扩宽。
    pub dev: u64,
    pub ino: u64,
    pub uid: u32,
    pub gid: u32,
}

impl Meta {
    /// 普通文件且属主可执行（`mode & 0o100`），与 git 判断可执行位的规则相同。
    pub fn is_executable(&self) -> bool;
}

/// 自 1970-01-01 UTC 的时刻，与 `timespec` 相同：`nanos` 在 0..1_000_000_000 之内，1970 年之前 `secs` 为负。
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Timestamp { pub secs: i64, pub nanos: u32 }

impl Timestamp {
    pub fn as_nanos(self) -> i128;   // `git` 的 `Content::Unhashed::mtime_ns`
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum FileKind {
    File, Dir, Symlink, Fifo, Socket, CharDevice, BlockDevice,
    /// `S_IFMT` 不是以上任何一种（如 macOS 的 whiteout）。
    Other,
}
```

不变量：`kind` 由 `mode & S_IFMT` 得出，两者一致；`Meta` 总是来自一次 `fstat` 或 `fstatat(AT_SYMLINK_NOFOLLOW)`，同一个 `Meta` 的字段属于同一次调用。

字段按 gix-index 的 stat 比较（`gix_index::entry::Stat::matches`，受 `core.trustctime`、`core.checkStat` 控制）取全宽度，截断由 `git` 做。`git` 构造 `gix_index::entry::Stat` 时与 gix-index 的 `Stat::from_fs` 一致：

| `entry::Stat` 字段 | 由 `Meta` 得出 |
|---|---|
| `mtime`、`ctime`（`secs: u32`、`nsecs: u32`） | `secs < 0` 时为 `(0, 0)`；否则 `(secs as u32, nanos)` |
| `dev`、`ino` | `as u32` |
| `uid`、`gid` | 原值 |
| `size` | `as u32` |

## 实现

三个实现只在「一步解析根目录之下的一个条目或它的父目录」上不同，最后一级的处理（`fstatat`、打开、`fstat`、`readlinkat`、读取）共用：

| 实现 | 平台 | 一步打开 | 何时使用 |
|---|---|---|---|
| `Openat2` | Linux | `openat2(root, 路径, …, RESOLVE_BENEATH \| RESOLVE_NO_SYMLINKS)` | `SafeRoot::open` 的探测成功 |
| `NofollowAny` | macOS 13 起 | `read`：`openat(root, 路径, … \| O_NOFOLLOW_ANY)`；`stat`：`fstatat(root, 路径, AT_SYMLINK_NOFOLLOW \| AT_SYMLINK_NOFOLLOW_ANY)` | `SafeRoot::open` 的探测没有返回 `EINVAL`（见「macOS 的实现选择」） |
| `NofollowAnyParent` | macOS 11、12 | `read` 同 `NofollowAny`；`stat`：`openat(root, 父目录, O_RDONLY \| O_DIRECTORY \| O_NOFOLLOW_ANY)` | 探测返回 `EINVAL`：`fstatat` 不认识 `AT_SYMLINK_NOFOLLOW_ANY`。Apple Silicon 只运行 macOS 11 及以上，`O_NOFOLLOW_ANY` 从 11 起可用 |
| `Walk` | 所有 unix | 从根描述符出发逐级 `openat(dir, 名字, O_DIRECTORY \| O_NOFOLLOW)` | Linux 探测失败；以及上面的一步实现出错时，用它重做这次调用并判定原因 |

所有描述符都带 `O_CLOEXEC`（ADR 0019：子进程不继承 Lectern 打开的描述符），由 `OwnedFd` 持有，出错路径上也随 drop 关闭；转交给 `OpenFile` 和 `open_dir` 返回的 `SafeRoot` 的描述符同样如此（见「数据流与线程」的描述符生命周期）。

### 路径复核

每次调用在发出系统调用之前，按 `RelPath` 的规则把路径拆成各级名字（与 `new` 相同的一次线性扫描）；`Walk` 对每一级名字再确认不是 `.`、`..`、空名字。`RelPath` 的字段私有，正常情况下复核总是通过；不通过时返回 `Io(InvalidInput)`，不打开任何东西。Linux 上 `RESOLVE_BENEATH` 在内核里再拒绝一次越出根目录的解析（`..`、绝对路径）。macOS 没有对应 `RESOLVE_BENEATH` 的标志，越出根目录只由这里的复核和 `RelPath` 的不变量排除：不跟随符号链接之后，没有 `..` 的路径只能停在根目录之下。

### stat

`Openat2`、`NofollowAnyParent`：

1. 打开父目录：路径只有一级时父目录就是根描述符，不打开；否则用一步打开（`Openat2`：`O_PATH | O_DIRECTORY`；`NofollowAnyParent`：`O_RDONLY | O_DIRECTORY | O_NOFOLLOW_ANY`）。
2. `fstatat(父目录, 最后一级, AT_SYMLINK_NOFOLLOW)`，得到 `Meta`。
3. 第 1 步返回 `ENOENT` 时为 `NotFound`；返回其他错误（`ELOOP`、`ENOTDIR`、`EAGAIN`、`ENAMETOOLONG`、`EXDEV` 等）时，用 `Walk` 重做整个调用，以它的结果为准。第 2 步的 `ENOENT`、`ENOTDIR` 为 `NotFound`，其他为 `Io`。

`NofollowAny`：

1. 一次 `fstatat(root, 整个路径, AT_SYMLINK_NOFOLLOW | AT_SYMLINK_NOFOLLOW_ANY)`，路径只有一级时也一样。`AT_SYMLINK_NOFOLLOW_ANY` 让内核在要跟随任何一级符号链接时返回 `ELOOP`；`AT_SYMLINK_NOFOLLOW` 让最后一级与 `lstat` 一样不跟随，是符号链接时返回链接本身的元数据。
2. 成功时得到 `Meta`；`ENOENT` 为 `NotFound`；其他错误（中间一级是符号链接的 `ELOOP`、中间一级是普通文件的 `ENOTDIR`、`EACCES`、`ENAMETOOLONG` 等）用 `Walk` 重做整个调用，以它的结果为准，与 `read` 的一步打开出错时相同。内核若对最后一级的符号链接也返回 `ELOOP`，`Walk` 照样返回链接的元数据，结果不变。

系统调用数：`Openat2`、`NofollowAnyParent` 一级路径 1 次，更深的路径 3 次（打开、`fstatat`、关闭）；`NofollowAny` 任何深度都是 1 次。都与深度无关。

### read

1. 一步打开整个路径（`Openat2`：`O_RDONLY | O_NOFOLLOW | O_NONBLOCK | O_NOCTTY`，`RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS`；`NofollowAny`：`O_RDONLY | O_NOFOLLOW_ANY | O_NONBLOCK | O_NOCTTY`）。`O_NONBLOCK` 让 FIFO 的打开立即返回，不等写端。
2. 成功时对这个描述符 `fstat`：
   - 普通文件：按下文「读取上限」读取，返回 `File` 或 `TooLarge`。
   - 目录、FIFO、字符设备、块设备、其他：关闭描述符，不读，返回 `Unsupported { meta }`。
3. 返回 `ENOENT` 时为 `NotFound`。返回其他错误时用 `Walk` 重做：最后一级是符号链接（两个一步打开都返回 `ELOOP`，无法区分是中间还是最后一级）、socket（打开失败，Linux 为 `ENXIO`，macOS 为 `EOPNOTSUPP`）、中间一级不是目录、权限不足、路径超过 `PATH_MAX` 都走这里。

普通文件的 `read` 是 1 次打开、1 次 `fstat`、若干次 `read`、1 次关闭，与深度无关。

### list

1. `dir` 为 None：从根描述符重新打开 `.`（rustix 的 `Dir::read_from`），不用根描述符自己的文件偏移。
2. 否则用 `read` 的一步打开打开整个路径；成功时对这个描述符 `fstat`，不是目录为 `Io(NotADirectory)`，是目录就从它读目录项。
3. 一步打开返回 `ENOENT` 为 `NotFound`，其他错误交给 `Walk`：中间各级同 `Walk` 的第 1 步，最后一级 `openat(dir, 名字, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)`，失败时 `fstatat` 判定：符号链接为 `PathThroughSymlink`，目录且与错误矛盾时重做（同 `Walk`），其他类型为 `Io(NotADirectory)`。
4. 目录项的类型为 `DT_UNKNOWN` 时对同一个目录描述符 `fstatat`；这时条目已不存在就略过它。

### open_dir

与 `list` 的第 2、3 步相同地打开 `rel`（成功时 `fstat` 确认是目录），把得到的描述符作为新 `SafeRoot` 的根，后端与原来的根相同；不读目录项。`SafeRoot::open` 会跟随根路径本身里的符号链接，所以工作区之内的子目录（依赖根）一律经已打开的工作区根调用本方法，不按路径另开（[ADR 0033](../adr/0033.md)「依赖根的读取限制」）。测试 `open_dir_*`：普通子目录、子目录本身或中间一级是符号链接、不是目录、打开后原路径被换成指向根外的链接时读到的仍是原来的目录。

### Walk

一次调用内复用描述符：每一级用上一级的描述符打开，打开下一级之后关闭上一级，同时最多持有两个描述符；最后一级的各个步骤都用同一个父目录描述符。不跨调用保留描述符，只有 `open_file` 和 `open_dir` 最后一级打开的那个描述符转交给返回值（见「数据流与线程」）。

1. 中间的每一级：`openat(dir, 名字, O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)`，Linux 加 `O_PATH`（只需要目录的搜索权限，与 `lstat` 相同），macOS 用 `O_RDONLY`。
   - `ENOENT`：`NotFound`。
   - 其他错误：对同一个 `dir` 做 `fstatat(名字, AT_SYMLINK_NOFOLLOW)` 判定：符号链接为 `PathThroughSymlink`；`fstatat` 返回 `ENOENT`，或类型既不是符号链接也不是目录，为 `NotFound`；是目录（如没有权限）或 `fstatat` 返回其他错误，为 `Io(原来的错误)`。
2. 最后一级，`stat`：`fstatat(dir, 名字, AT_SYMLINK_NOFOLLOW)`。
3. 最后一级，`read`：`openat(dir, 名字, O_RDONLY | O_NOFOLLOW | O_NONBLOCK | O_NOCTTY | O_CLOEXEC)`。
   - 成功：同上文 `read` 的第 2 步。
   - `ENOENT`、`ENOTDIR`：`NotFound`。
   - 其他错误：`fstatat(dir, 名字, AT_SYMLINK_NOFOLLOW)` 判定：符号链接时 `readlinkat(dir, 名字)`，返回 `Symlink { meta, target }`；socket、设备文件、FIFO、其他类型返回 `Unsupported { meta }`；普通文件或目录（如没有读权限）为 `Io(原来的错误)`。

`fstatat` 的结果与它之前那次打开的错误矛盾时，条目在两次调用之间被替换了，这一级重做（打开、判定），最多 8 次，仍矛盾时才按上面的规则返回 `Io(原来的错误)`。矛盾指：类型是目录或普通文件，而打开返回的是只有符号链接（`ELOOP`；Linux 的 `O_PATH | O_DIRECTORY` 为 `ENOTDIR`）、以 `O_DIRECTORY` 打开的非目录（`ENOTDIR`）或 socket（`ENXIO`、`EOPNOTSUPP`）才会有的错误；以及类型是符号链接而 `readlinkat` 返回 `EINVAL`。否则目录 `a` 与符号链接 `a` 来回交换时，偶尔会在 `PathThroughSymlink` 和内容之外得到 `Io`（`race_swap_never_escapes`）。

`Symlink` 的 `meta` 和 `target` 来自同一个父目录描述符上的两次调用（`fstatat`、`readlinkat`），链接在两次调用之间被替换时两者可能属于不同的链接；两者都不跟随，不影响「不读到根目录之外」，`git` 在下一次刷新时按新的 stat 重新读取。

### Linux 的实现选择

`SafeRoot::open` 打开根目录后探测一次：`openat2(root, ".", O_PATH | O_DIRECTORY | O_CLOEXEC, RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS)`。返回 `ENOSYS`（5.6 之前的内核）、`EPERM`（seccomp 拦截了这个系统调用，部分容器运行时如此）、`EINVAL` 或 `E2BIG` 时，这个 `SafeRoot` 用 `Walk`；其余结果用 `Openat2`。选择存在 `SafeRoot` 自己的字段里，创建后不变，没有全局状态。运行中 `openat2` 再返回这些错误时，按「其他错误」交给 `Walk`，结果仍然正确。

### macOS 的实现选择

`SafeRoot::open` 打开根目录后探测一次：`fstatat(root, ".", AT_SYMLINK_NOFOLLOW | AT_SYMLINK_NOFOLLOW_ANY)`。`"."` 对打开着的目录总是合法的名字，路径里也没有可拒绝的符号链接，所以 `EINVAL` 只能表示内核不认识 `AT_SYMLINK_NOFOLLOW_ANY`（macOS 11、12），这个 `SafeRoot` 用 `NofollowAnyParent`；其余结果用 `NofollowAny`。与 Linux 一样，选择存在 `SafeRoot` 自己的字段里，创建后不变，没有全局状态；探测只看 `"."`，之后某次 `stat` 因为别的原因返回 `EINVAL` 时只交给 `Walk` 重做这一次调用，不改变选择。探测是每个 `SafeRoot` 1 次系统调用，`SafeRoot::open` 在 `git` 的后台任务里，每个工作区一次。

### 读取上限

- `fstat` 得到的 `size` 大于 `max`：返回 `TooLarge { meta }`，不读。
- 否则用 `try_reserve_exact` 预留 `size + 1` 字节（多 1 字节让发现 EOF 的那次 `read` 不必再分配；分配失败为 `Io(OutOfMemory)`），读满后按倍增扩容、总量不超过 `max + 1`，循环 `read` 直到 EOF，最多读 `max` 字节；读满 `max` 字节后再读 1 字节，读到数据（文件在 `fstat` 之后变长了）时丢弃已读内容，返回 `TooLarge { meta }`。`max` 包含在内：恰好 `max` 字节的文件是 `File`。`max = 0` 时只有空文件是 `File`。
- `EINTR` 时重试同一次 `read`。
- `size` 为 0 但有内容的文件（如 procfs）照样读到 EOF，受 `max` 约束。
- 文件在读取期间被截短时返回读到的字节，`bytes.len()` 可能小于 `meta.size`；`meta` 是读取之前的那次 `fstat`。调用方按 `bytes` 计算内容，按 `meta` 做 stat 缓存；内容变了的文件的 mtime 在下一次 stat 时不同，`git` 会重新读取（同一时刻内的写入由 racy-clean 规则处理）。
- 不用 mmap：并发截短映射中的文件会让读取触发 SIGBUS。

## 数据流与线程

- `git`：`Worktree` 持有工作区根的 `SafeRoot`；工作区比较的第 1、2 步调 `stat`，第 3 步调 `read`（上限 64 MiB），第 4 步（未跟踪文件）逐个目录调 `list`、用 `read` 读 `.gitignore`，`Worktree::read_file` 调 `read`。`git` 的 `RepoPath` 满足 `RelPath` 的不变量，调用时 `RelPath::new(path.as_bytes())` 总是 Some，不复制字节。
- `project`：经 `git` 的 `Worktree::read_file` 读清单文件（[project 设计「依赖 git 的接口」](project.md#依赖-git-的接口)）；没有 git 的文件夹项目若要同样的保证，可以直接用 `SafeRoot`，由 `project` 的设计决定。
- `file_reader`（[#10](https://github.com/SoloJiang/lectern-app/issues/10)）：首屏、完整读取和超过 16 MiB 的文件的分页经 `open_file` 与 `OpenFile`，见「范围读取」的「对其他 crate 的影响」。
- Analyze 快照（#66）：在本 crate 内复用同一套打开逻辑（`pub(crate)` 的一步打开和 `Walk`），叠加工作区文件时从根描述符打开、对同一个描述符判断类型后克隆，设计随 #66。

线程：`SafeRoot` 只有一个 `OwnedFd` 和一个创建后不变的实现选择，二者都是 `Send + Sync`；所有操作都是以描述符为起点的 `*at` 系统调用，不改当前目录，不用根描述符的文件偏移，不读环境变量，没有进程级的缓存或锁，所以多个线程可以同时调用同一个 `SafeRoot`。

描述符的生命周期：除 `open_file` 和 `open_dir` 之外，每次调用自己打开和关闭需要的描述符（最多两个），返回之前全部关闭，调用之间不持有。两个例外都把调用里打开的最后一个描述符转交给返回值，由返回值独占：

- `open_dir`：目录描述符转交给返回的 `SafeRoot`，与 `SafeRoot::open` 打开的根一样，在这个 `SafeRoot` drop 时关闭。
- `open_file`：结果为 `Opened::File` 时，普通文件的描述符转交给 `OpenFile`。`OpenFile` 只持有这一个描述符（带 `O_CLOEXEC`，子进程不继承），`read_ranges`、`read_all`、`stat` 都在它上面进行、不再打开别的描述符，drop 时关闭。`Opened::Symlink`、`Opened::Unsupported` 和错误返回之前关闭全部描述符，与 `read` 相同。`file_reader` 持有 `OpenFile` 的时间见「范围读取」的「对其他 crate 的影响」：不超过 16 MiB 的文件到完整读取结束，超过 16 MiB 的到阅读视图关闭或收到这个路径的 `fs_watch` 事件。

`fd_*` 测试按这条规则计数：存活的 `SafeRoot`（含 `open_dir` 返回的）和 `OpenFile` 各算一个描述符，其余调用返回之后描述符数回到调用之前。

本模块不缓存任何东西：stat 缓存和内容的 OID 缓存在 `git`（架构规则 5 的派生数据由调用方以 blob OID 为键缓存）。

## 性能

| 预算 | 本模块的份额 |
|---|---|
| 冷启动 20 ms（[ADR 0017](../adr/0017.md)） | 首帧之前不调用；`SafeRoot::open` 在 `git` 的后台任务里 |
| 空闲内存 | 每个存活的 `SafeRoot` 和 `OpenFile` 各一个描述符，没有缓存；一次 `read` 的内存是文件本身（不超过 `max`），`read_ranges` 不超过 `MAX_RANGE_BYTES` |
| 打开文件到首屏高亮 < 16 ms | 首屏的范围读取中位数 < 200 µs，见「范围读取」的「性能」 |
| 打开一个会话的 Diff < 50 ms | `git` 的 `changes` 对全部跟踪文件 `stat`：728 个文件的 `stat` 合计 < 5 ms（M 系列 Mac） |
| 安装包 ≤ 80 MB（[ADR 0018](../adr/0018.md)） | rustix 已在发布构建里（经 GPUI），不新增 crate；本模块的代码在几十 KB 量级 |

与 `std::fs` 的对比：`std::fs::symlink_metadata` 是 1 次 `lstat`，但它跟随中间一级的符号链接，不能直接用。`stat` 的路径解析工作与 `lstat` 相同（父目录一次解析、最后一级一次查找）：macOS 13 起与 `lstat` 一样是 1 次系统调用；Linux 和 macOS 11、12 上额外是 1 次打开和 1 次关闭，只有一级的路径是 1 次。普通文件的 `read` 与 `std::fs::read`（`open`、`statx` / `fstat`、`read`、`close`）的系统调用数相同。符号链接、socket、中间一级出错的路径走 `Walk`，系统调用数随深度增长，这些路径在仓库里少见。

预算（M 系列 Mac，macOS 13 及以上，路径深度 4，页缓存已热）：`stat` 的中位数不超过同一路径上 `symlink_metadata` 的 3 倍且 < 10 µs；4 KiB 文件的 `read` 不超过 `std::fs::read` 的 1.5 倍。超出时先考虑在一次 `changes` 内复用父目录描述符（index 按路径排序，相邻路径常在同一目录），这要改接口，另改本文。

基准：

- `bench` 的 `platform/safe_stat`、`platform/safe_read_4k`（[bench 设计「迁移」](bench.md#迁移)）：bench 自己生成 728 个 4 KiB 文件、深度 4 的目录树，与同一路径上的 `symlink_metadata`、`std::fs::read` 交替采样，按上面的比值判定；只测 `SafeRoot::open` 选中的生产后端（macOS 上是 `NofollowAny`，Linux 上是 `Openat2`），`Walk` 不设基准。`git` 的 `git/changes_clean` 包含它们的成本。
- `platform/safe_read_range`、`platform/safe_read_page`：范围读取的首屏调用和分页，预算与测量方式见「范围读取」的「性能」。基准在 `crates/bench/benches/time/platform.rs`，`crates/bench/src/budgets.rs` 里的这两条随范围读取的实现 PR 由 `Planned("#103")` 改为 `Active`。
- 实现 PR 里的 `#[ignore]` 测试 `perf_smoke`（对每个后端各做 10 000 次 `stat`、`read` 并打印比值）已由 bench 第 1 步删除，它在 Linux 上的结果见「实际验证」。

## 状态与异常

本模块没有界面。各种文件系统状态的结果：

| 情况 | `stat` | `read` |
|---|---|---|
| 普通文件 | `Meta`（`File`） | `File`；超过 `max` 时 `TooLarge` |
| 最后一级是符号链接（含悬空链接、指向根目录之外的链接） | `Meta`（`Symlink`） | `Symlink`，不跟随 |
| 中间一级是符号链接 | `PathThroughSymlink` | `PathThroughSymlink` |
| 目录 | `Meta`（`Dir`） | `Unsupported`（`Dir`） |
| FIFO | `Meta`（`Fifo`） | `Unsupported`，不阻塞，不读 |
| socket | `Meta`（`Socket`） | `Unsupported`（打开失败后经 `fstatat` 判定） |
| 设备文件 | `Meta` | `Unsupported`，打开后不读 |
| 不存在；中间一级是普通文件 | `NotFound` | `NotFound` |
| 没有权限 | `Io(PermissionDenied)` | `Io(PermissionDenied)` |
| 根目录已被删除 | `NotFound` | `NotFound` |
| 名字超过 `NAME_MAX` | `Io` | `Io` |

## 已知限制

- 并发修改：每次调用的结果对应调用期间某一时刻的状态。目录在一次调用的解析期间被移到根目录之外时，这次调用可能读到移走之后的内容。做到这一点要以用户身份在本机运行程序，不在威胁模型内（AGENTS.md「Review guidelines」、ADR 0024「已知限制」）；调用之间把目录换成符号链接的情况由下一次调用正确报告为 `PathThroughSymlink`。
- 挂载点：`RESOLVE_BENEATH` 不阻止跨越根目录之下的挂载点（没有加 `RESOLVE_NO_XDEV`），macOS 同样。根目录之下的挂载由用户自己设置，内容仍在根目录的路径之下。
- 设备文件在 `fstat` 之前被打开（`O_NONBLOCK | O_NOCTTY`，不读）。在工作区里创建设备文件需要 root 权限。
- macOS 的 `Walk` 和 `NofollowAnyParent` 的父目录用 `O_RDONLY` 打开，需要目录的读权限。只有搜索权限（如 `0711`）的目录下的条目，`NofollowAny` 的 `stat`（一次 `fstatat`）和 `read` 的一步打开只需要搜索权限，能成功；macOS 11、12 上的 `stat`，以及一步调用出错而交给 `Walk` 的（如这种目录下的符号链接、socket）为 `Io(PermissionDenied)`，而 `lstat` 能成功。git 检出的目录不会这样。
- macOS 11、12 上 `stat` 是 3 次系统调用（`NofollowAnyParent`），bench 只在 CI 的 macOS 版本（13 及以上）上判定预算；11、12 上的比值预计与改为一次 `fstatat` 之前在 macOS CI 上测得的 3.94–4.16 倍相近，高于 3 倍预算，没有测量。
- 路径超过 `PATH_MAX`（macOS 1024、Linux 4096）时一步打开失败，交给 `Walk` 逐级打开，照常成功；单个名字超过 `NAME_MAX`（255）时为 `Io`。

## 测试

全部写在 `crates/platform/src/safe_read/tests.rs`（`#[cfg(test)] mod tests;`）里，用 `tempfile` 的临时目录。行为测试对当前平台可用的每个实现各运行一次：Linux 上是 `Openat2` 和 `Walk`，macOS 上是 `NofollowAny`、`NofollowAnyParent` 和 `Walk`（测试用 `pub(crate)` 的 `SafeRoot::open_with(root, Backend)` 强制选择；强制 `NofollowAnyParent` 让 macOS 11、12 的路径在更新版本的 CI 上也有测试）。

在 Linux CI 和 macOS CI 上都运行：

- `relpath_*`：拒绝 `""`、`"/a"`、`"a/"`、`"a//b"`、`"."`、`".."`、`"a/../b"`、`"a/./b"`、`"../a"`、含 NUL 的路径；接受 `"a"`、`"a/b"`、`".git"`、`"..a"`、`"a.."`、`".hidden/x"` 和非 UTF-8 的字节。
- `stat_*`：普通文件、可执行文件（`is_executable`）、目录、根目录下一级和多级路径；`Meta` 的 `size`、`mtime`（含纳秒）、`ctime`、`ino`、`dev`、`mode`、`uid`、`gid` 与同一路径上 `std::fs::symlink_metadata` 的结果一致；不存在、中间一级是普通文件为 `NotFound`。
- `symlink_*`：中间一级是符号链接（指向根目录之内和之外的目录）为 `PathThroughSymlink`；最后一级是符号链接时 `stat` 为 `FileKind::Symlink`，`read` 为 `Symlink` 且 `target` 与 `std::fs::read_link` 相同；悬空链接和指向 `/etc/passwd` 的链接都只返回目标字节，不读目标。
- `read_*`：小文件、空文件、恰好 `max` 字节为 `File`；`max + 1` 字节为 `TooLarge`；1 TiB 的稀疏文件立即返回 `TooLarge`，不按 `size` 分配缓冲；`max = 0` 时空文件为 `File`、非空文件为 `TooLarge`；目录为 `Unsupported`（`Dir`）。
- `fifo_*`：FIFO 的 `read` 立即返回 `Unsupported`（`Fifo`），没有写端也不阻塞（测试在单独的线程里调用，2 秒内必须返回）；`stat` 为 `FileKind::Fifo`。
- `socket_*`：`UnixListener::bind` 创建的 socket，`read` 为 `Unsupported`（`Socket`）。
- `device_*`：以 `/dev` 为根，`read("null")` 为 `Unsupported`（`CharDevice`）。
- `race_swap_between_calls`：`stat("a/b.txt")` 成功后，把 `a` 改名并在原处建指向根目录之外的目录的符号链接，下一次 `stat` 和 `read` 为 `PathThroughSymlink`；换回目录后恢复正常。
- `race_swap_never_escapes`：一个线程反复在「目录 `a`」和「指向根目录之外、含同名文件的目录的符号链接 `a`」之间交换，另一个线程读 10 000 次 `a/secret`；每次结果只能是根目录之内的内容、`NotFound` 或 `PathThroughSymlink`，从不出现根目录之外那份文件的内容。
- `list_*`：根目录和子目录的名字与类型（普通文件、目录、符号链接、FIFO、socket），同一个根连续列两次结果相同；目录本身或中间一级是符号链接为 `PathThroughSymlink`，不存在为 `NotFound`，普通文件为 `Io(NotADirectory)`。`race_list_swap_never_escapes`：目录 `a` 与指向根目录之外的符号链接来回交换时列 5000 次 `a`，结果只能是根目录之内的名字、`NotFound` 或 `PathThroughSymlink`。
- `open_dir_*`：一级和多级的子目录，从打开的根再打开一层，新根的实现与原来的根相同，读到的内容、元数据和目录项都在子目录之内，原来的根里子目录之外的条目为 `NotFound`；子目录本身或中间一级是符号链接（指向根目录之内的目录和普通文件、根目录之外、悬空）为 `PathThroughSymlink`；普通文件、FIFO、socket 为 `Io(NotADirectory)`，不存在、中间一级是普通文件为 `NotFound`；打开后把原路径换成指向根目录之外的符号链接，新根的 `read`、`stat`、`list`、`open_dir` 仍作用在原来的目录上，按路径再打开为 `PathThroughSymlink`，原来的目录删除后为 `NotFound`；新根之下上面的 `symlink_*` 照样成立，`..`、绝对路径不是 `RelPath`，绕过 `RelPath::new` 直接传入时四个方法都在系统调用之前返回 `Io(InvalidInput)`。`race_open_dir_swap_never_escapes`：目录 `a` 与指向根目录之外的符号链接来回交换时打开 5000 次 `a` 并经新根读 `secret`，结果只能是根目录之内的内容、`NotFound` 或 `PathThroughSymlink`；交换开始之前打开的新根每次都读到原来的内容。`fd_*` 也覆盖 `open_dir` 和它返回的根。
- `fd_*`：反复调用（含各种出错路径）之后，进程打开的描述符数不变（Linux 读 `/proc/self/fd`，macOS 读 `/dev/fd`）；存活的 `SafeRoot` 和 `OpenFile` 按「数据流与线程」的描述符生命周期各计一个，drop 之后数目回落；`SafeRoot` 的描述符设了 `FD_CLOEXEC`。
- `send_sync`：编译期断言 `SafeRoot`、`RelPath`、`Meta`、`Read`、`SafeError` 是 `Send + Sync`；8 个线程同时对同一个 `SafeRoot` 调用 `stat` 和 `read`，结果与单线程相同。
- `walk_*`：`Walk` 的判定在一步打开失败的每种情况下（中间是符号链接、中间是普通文件、最后一级是符号链接、socket、路径超过 `PATH_MAX`）与上表一致。

只在 Linux 上运行：

- `openat2_rejects_escape`：绕过 `RelPath`、直接把 `"../x"`、`"/etc/passwd"`、`"a/../../x"` 交给 `Openat2` 的一步打开，内核拒绝（`EXDEV`），不打开根目录之外的文件。`open_dir_openat2_rejects_escape` 对 `open_dir` 返回的根做同样的检查，`..` 指向原来的根里的条目时也拒绝。
- `linux_backend_choice`：探测结果到实现的映射是纯函数，`ENOSYS`、`EPERM`、`EINVAL`、`E2BIG` 选 `Walk`，成功和其他错误选 `Openat2`；在 CI 的内核上 `SafeRoot::open` 选的是 `Openat2`。

只在 macOS 上运行：

- `nofollow_any_*`：上面的 `symlink_*`、`race_*`、`walk_*` 在 `NofollowAny` 和 `NofollowAnyParent` 上各自通过；探测结果到实现的映射是纯函数（`EINVAL` 选 `NofollowAnyParent`，成功和其他错误选 `NofollowAny`），`sw_vers` 报告 13 及以上时 `SafeRoot::open` 选的是 `NofollowAny`，更早时是 `NofollowAnyParent`；`O_NOFOLLOW_ANY` 打开父目录时对符号链接返回错误；`O_NOFOLLOW_ANY` 的一步打开对中间一级和最后一级的符号链接都返回错误，不跟随；`stat` 的一步 `fstatat` 对多级路径上的普通文件和目录一次成功（不落到 `Walk`），元数据与 `symlink_metadata` 一致，对中间一级的符号链接（含悬空链接）返回 `ENOENT` 以外的错误，最后一级是符号链接时返回链接本身的元数据或 `ENOENT` 以外的错误，从不返回链接目标的元数据。

`git` 的工作区测试（FIFO 为 `UnsupportedType`、中间一级是符号链接为 `PathThroughSymlink`）在 Linux 上经本模块的 Linux 实现运行。

范围读取的测试见「范围读取」的「测试」。

## 范围读取

对应 issue [#103](https://github.com/SoloJiang/lectern-app/issues/103)，是 `file_reader`（[#10](https://github.com/SoloJiang/lectern-app/issues/10)）的前置条件。打开文件到首屏高亮 < 16 ms（[性能预算](tech-research.md#性能预算)）要求首屏只读要显示的那一段：文件开头，或跳转目标附近（[ADR 0034](../adr/0034.md)「文本存储与定位」）。`read` 读完整个文件、超过 `max` 时为 `TooLarge`，不能用在首屏上。

范围读取先打开文件并保留描述符，再按字节区间用 `pread` 读取。不超过 16 MiB 的文件，首屏读完之后，后台在同一个描述符上读完整个文件，按文件的身份核对两次读取之间文件没有变化；超过 16 MiB 的文件不整份读入，按 64 KiB 的页在同一个描述符上按需读取。

### 接口

```rust
impl SafeRoot {
    /// 打开一个条目供范围读取。一步打开、`Walk` 后备、`fstat` 与类型判断都与 `read` 相同（见「read」「Walk」），
    /// 打开标志也相同：普通文件返回 `Opened::File`，保留打开的描述符，不读内容，不受大小上限约束；
    /// 最后一级是符号链接时读出目标，不跟随；目录、FIFO、socket、设备文件关闭描述符，不读。
    /// 错误与 `read` 相同（`NotFound`、`PathThroughSymlink`、`Io`）。
    pub fn open_file(&self, rel: RelPath<'_>) -> Result<Opened, SafeError>;
}

// `open_file` 用 `read` 的打开路径（打开条目本身、对描述符 `fstat` 后按类型分支），
// 不用 `list` 与 `open_dir` 共用的目录打开函数（那个函数要求结果是目录，不是目录时为 `Io(NotADirectory)`）。
// 在 `open_dir` 返回的根上调用时规则不变：从那个根的目录描述符出发、用同一个实现，路径不经过打开它时的 `rel`；
// 打开之后原路径被换成符号链接，`open_file` 仍作用在原来的目录上，与新根的 `read` 相同。

#[derive(Debug)]
pub enum Opened {
    File(OpenFile),
    /// 同 `Read::Symlink`。
    Symlink { meta: Meta, target: Vec<u8> },
    /// 同 `Read::Unsupported`。
    Unsupported { meta: Meta },
}

/// 一个已打开的普通文件。`Send + Sync`，不可克隆，需要共享时放进 `Arc`；drop 时关闭描述符。
/// 所有读取都用 `pread`，不用也不改描述符的文件偏移，多个线程可以同时调用。
#[derive(Debug)]
pub struct OpenFile { /* fd: OwnedFd, meta: Meta */ }

impl OpenFile {
    /// 打开之后对这个描述符的那次 `fstat`，`kind` 总是 `File`。不发系统调用。
    pub fn meta(&self) -> &Meta;

    /// `self.meta().identity()`：打开时的身份，之后每次读取返回的身份都与它比较（见「文件身份」）。
    pub fn identity(&self) -> FileIdentity;

    /// 按字节区间读取，读完之后对同一个描述符 `fstat` 一次。区间 1 到 `MAX_RANGES` 个，
    /// 各自满足 `start <= end <= i64::MAX`，长度之和不超过 `MAX_RANGE_BYTES`；不满足时为
    /// `ErrorKind::InvalidInput`，不发系统调用。区间可以重叠，不必有序。
    pub fn read_ranges(&self, ranges: &[Range<u64>]) -> io::Result<RangeRead>;

    /// 从偏移 0 读到 EOF，最多 `max` 字节，规则同「读取上限」；读完之后 `fstat` 一次。
    pub fn read_all(&self, max: u64) -> io::Result<FullRead>;

    /// 对同一个描述符 `fstat`。
    pub fn stat(&self) -> io::Result<Meta>;
}

pub const MAX_RANGES: usize = 4;
pub const MAX_RANGE_BYTES: u64 = 1 << 20;   // 1 MiB

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RangeRead {
    /// 与请求的区间一一对应：`pieces[i]` 是从 `ranges[i].start` 起读到的字节，在 EOF 处截止，
    /// 长度不超过区间的长度；起点在 EOF 或之后时为空。
    pub pieces: Vec<Vec<u8>>,
    /// 读完全部区间之后的 `fstat`。
    pub meta: Meta,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum FullRead {
    /// `meta` 是读完之后的 `fstat`。
    File { bytes: Vec<u8>, meta: Meta },
    /// 打开时的 `size` 已超过 `max`（不读，`meta` 是 `OpenFile::meta()`），
    /// 或读取中超过 `max`（`meta` 是读完之后的 `fstat`）。
    TooLarge { meta: Meta },
}
```

所有方法都阻塞（系统调用和磁盘 IO），在调用方的后台任务线程上执行，UI 线程上不调用；首屏的调用在 `file_reader` 的后台任务里（ADR 0034：可见窗口在 `Interactive` 上）。

用持有描述符的 `OpenFile`，不用一次性的 `read_range(rel, offset, len)`：

- 首屏读取与完整读取用同一个描述符，读的是同一个 inode。路径在两次读取之间被替换（编辑器的原子保存先写临时文件再 `rename`）时，完整读取读到的仍是首屏的那个文件，身份比较只需判断这个文件的内容有没有变；路径指向哪个文件的变化由 `fs_watch` 的事件报告。
- 超过 16 MiB 的文件分页时，每页 1 次 `pread` 和 1 次 `fstat`，不重新解析路径。
- 一次性的按路径读取就是 `open_file` 加一次 `read_ranges`，不另设接口。

`read` 不变：`git` 的工作区比较仍用它，它的 `meta` 是读取之前的 `fstat`（stat 缓存要的是决定读不读的那一次）；`OpenFile` 上的读取返回读完之后的 `fstat`，用于身份核对。两者共用一步打开、`Walk` 和读取上限的代码。

### 文件身份

```rust
/// 判断两次读取之间文件是否变化的依据。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct FileIdentity {
    pub dev: u64,
    pub ino: u64,
    pub size: u64,
    pub mtime: Timestamp,
    pub ctime: Timestamp,
}

impl Meta {
    pub fn identity(&self) -> FileIdentity;
}
```

| 字段 | 发现的变化 |
|---|---|
| `dev`、`ino` | 换成了另一个文件：按路径重新打开时，路径被原子保存、`git checkout` 重写或换成别的文件 |
| `size` | 截短、追加 |
| `mtime` | 写入内容 |
| `ctime` | 用户态不能设置：`utimensat` 改 `mtime` 时内核同时把 `ctime` 更新为当前时间，所以把 `mtime` 改回原值的工具（`touch -d`、`rsync -t`、`cp -p` 覆盖已有文件）也会被发现；写入、截短、`chmod` 同样更新它 |

`mode`、`uid`、`gid`、`nlink` 不在身份里：它们不改变内容。`chmod` 这类操作仍会经 `ctime` 让身份不同，调用方按变化处理，多一次重读。

核对规则：`OpenFile` 上的一次读取返回的 `meta.identity()` 等于 `OpenFile::identity()` 时，这次读到的字节属于打开时的那个版本，即从打开时的 `fstat` 到读完之后的 `fstat` 之间没有完成过改变内容的操作（时间戳的精度见「已知限制」）。不相等时，字节属于未知的版本，可能混有新旧内容，调用方不能把它当作打开时那个版本的一部分。本模块只报告身份，不重试，也不因身份变化返回错误：文件正在被持续写入（如 agent 追加的日志）时，丢弃、重新打开还是先显示由调用方决定。

完整读取的核对：首屏 `open_file` 之后记下 `identity()`，把 `OpenFile` 交给后台任务调用 `read_all`。`FullRead::File` 的 `meta.identity()` 等于首屏记下的身份时，首屏读到的每一段都是完整内容里同一位置的字节，首屏的结果可以保留，只换成完整内容。按路径重新 `open_file` 的调用方用同样的比较：路径被换成别的文件时 `dev`、`ino` 不同。

### 变化与截短

| 两次读取之间或读取期间 | `read_ranges` | `read_all` | 读完后的身份 |
|---|---|---|---|
| 没有变化 | 各段在 EOF 处截止 | `File` | 相同 |
| 截短 | 起点在新的 EOF 之后的段为空，跨过它的段在新的 EOF 处截止，不报错 | 读到新的 EOF 为止，`bytes.len()` 小于打开时的 `size` | 不同（`size`、`mtime`） |
| 追加 | 区间内的新字节照常读出 | 总量超过 `max` 时为 `TooLarge`，否则读到新的 EOF | 不同 |
| 大小不变的原地改写 | 读出的字节可能混有新旧内容 | 同左 | 不同（`mtime`、`ctime`） |
| 路径被删除，或被 `rename` 覆盖 | 照常读原来的文件 | 同左 | 描述符上的身份不一定变化；`SafeRoot::stat(rel)` 为 `NotFound` 或 `dev`、`ino` 不同，`fs_watch` 报告这个路径的事件 |
| IO 错误（`EIO` 等） | `Err`，已读的部分丢弃 | `Err` | — |

不用 mmap（见「读取上限」）：读取期间被截短时 `pread` 只返回较短的结果，不会触发 SIGBUS。

### 读取的规则

`read_ranges`：

1. 校验区间的个数、`start <= end <= i64::MAX`（`pread` 的偏移是有符号的 `off_t`）和长度之和，不满足时为 `InvalidInput`。
2. 按起点排序，重叠或相邻的区间合并为一段，每段一次 `pread`，各区间的字节从所在段的缓冲里取出。首屏的文件头 8 KiB 与从文件开头附近开始的窗口因此只用一次 `pread`。
3. 每段的缓冲按 `min(段长, 打开时的 size − 起点)` 用 `try_reserve_exact` 预留，分配失败为 `Io(OutOfMemory)`；稀疏的大文件不按 `size` 分配。文件在打开之后变长时按需扩容，不超过段长。
4. `pread` 返回少于请求的字节数时从新的偏移继续，返回 0 时这一段在 EOF 处截止；`EINTR` 时重试同一次调用。
5. 全部段读完之后 `fstat` 一次，作为 `RangeRead::meta`。

`read_all` 按「读取上限」的规则读取，区别有三处：用 `pread` 从偏移 0 开始（不依赖描述符的文件偏移，所以可以从多个线程同时调用）；`size > max` 的判断用 `OpenFile::meta()`；读完之后 `fstat` 一次。

描述符：`open_file` 的打开标志与 `read` 相同（`O_RDONLY | O_NOFOLLOW | O_NONBLOCK | O_NOCTTY | O_CLOEXEC`，macOS 用 `O_NOFOLLOW_ANY`），所以 FIFO 不阻塞，子进程不继承（ADR 0019）。普通文件的这个描述符是本模块里唯一在返回之后仍然打开的文件描述符（`open_dir` 转交的是目录描述符）：`OpenFile` 独占它，drop 时关闭，`OpenFile` 上的读取不再打开别的描述符。`O_NONBLOCK` 留在描述符上，对普通文件的 `pread` 没有影响：Linux 和 macOS 上普通文件的读取忽略这个标志。`rustix::io::pread` 不需要另开 feature，依赖不变。

系统调用数：`open_file` 对普通文件是 1 次打开、1 次 `fstat`（与 `read` 相同，与深度无关，`Walk` 后备除外）；`read_ranges` 是每段 1 次 `pread`（短读时多几次）加 1 次 `fstat`；drop 时 1 次关闭。

### 上限

- `MAX_RANGES = 4`、`MAX_RANGE_BYTES = 1 MiB`：一次调用的内存和耗时都有上限，范围读取不能代替 `read_all` 绕过 `max`。首屏约 140 KiB、按行检查点定位约 204 KiB（见下两节）、分页的 64 KiB 页都在其内；后台按顺序扫描超过 16 MiB 的文件时每次读 1 MiB。
- 区间的终点不超过 `i64::MAX`。
- `OpenFile` 的个数和持有时间不由本模块限制：每个独占一个描述符，drop 时关闭（「数据流与线程」的描述符生命周期）；`file_reader` 的持有时间见「对其他 crate 的影响」，同时存活的 `OpenFile` 不超过打开着的阅读视图数。

### 首屏的一次调用

首屏是 `open_file` 加一次 `read_ranges`，两个区间。`size` 是 `OpenFile::meta().size`，`t` 是目标的字节偏移先截到 `size`（`t = t.min(size)`；目标在文件开头时为 0，这是最常见的初始视图）：

| 区间 | 用途（ADR 0034「首屏路径的准备工作」） |
|---|---|
| `0 .. min(8 KiB, size)` | 编码、BOM 和二进制的判断只看文件头 8 KiB 和窗口 |
| `t.saturating_sub(64 KiB) .. min(t + 68 KiB, size)` | 安全位置只在目标之前至多 64 KiB 里找；目标所在的行首从目标往前至多 4 KiB；可见部分从行首往后至多 64 KiB；窗口到可见部分之后 4 KiB 结束 |

截取规则，适用于本节和下一节的每个区间：

- 下界一律用 `saturating_sub`，截到 0。`t < 64 KiB`（包括 `t = 0`）时普通的 `u64` 减法会下溢；截到 0 之后窗口从文件开头开始，`read_ranges` 收到的仍是合法区间（`start <= end`）。
- 上界一律截到 `size`。EOF 处的短读本来也会把各段截短，截到 `size` 是为了让请求的长度就是实际要读的长度，`MAX_RANGE_BYTES` 按截过的长度计算；打开之后变长的部分不在请求里，变长会让身份不同（「文件身份」）。`t <= size <= i64::MAX`，所以 `t + 68 KiB` 不会溢出，截过的终点也满足 `end <= i64::MAX`。
- 先截取、再交给 `read_ranges`，由它按「读取的规则」第 2 步合并重叠或相邻的区间。`t <= 72 KiB` 时窗口的起点不大于 8 KiB，与文件头相邻或重叠，两个区间合并为一次 `pread`；`t < 64 KiB` 时窗口从 0 开始，文件头整个落在窗口里；文件小于 8 KiB 时两段都是整个文件。

合计约 140 KiB，截取之后只会更少。区间的确切常量由 `file_reader` 的设计决定，本模块只要求一次调用不超过 `MAX_RANGE_BYTES`，并要求 `file_reader` 的设计按上面的截取规则写出区间。首屏需要的字节超出这两段时（如 64 KiB 内找不到安全位置），按 ADR 0034 首屏先画纯文本，后台再算，不扩大这次读取。

### 按行定位

决定：本模块只按字节偏移读取，不建行索引，也不提供「找第 N 行」。超出读取范围的行号由 `file_reader` 的领域库按 ADR 0034 的行检查点或完整读取解决：

- 知道偏移的一方给出偏移（`code_intel` 的 `Location.byte`，`git` 的 hunk 与展示行的 `*_byte`，保存的阅读位置），首屏直接按偏移读取，不需要行号。
- 只有行号时，查 `syntax.lines` 里按 blob OID 缓存的行检查点（[cache 设计「命名空间」](cache.md#命名空间)，每 64 KiB 一个）。检查点 `c` 是目标行之前最近的那个，目标行在 `c` 之后 64 KiB 之内，所以一次 `read_ranges` 读文件头 `0 .. min(8 KiB, size)` 和 `c.saturating_sub(64 KiB) .. min(c + 132 KiB, size)`（合计约 204 KiB，按上一节的截取规则截到 0 和 `size`；`c = 0` 即第一个检查点时下界为 0，与文件头合并），数换行得到目标的偏移，窗口也在这一段里，不需要第二次读取。
- 没有检查点时：不超过 16 MiB 的文件，后台的 `read_all` 之后建行索引、写检查点；超过 16 MiB 的文件，后台以 1 MiB 的 `read_ranges`（第 k 次读 `k × 1 MiB .. min((k + 1) × 1 MiB, size)`）顺序扫描整个文件、写检查点。在那之前首屏显示文件开头，扫描到那一行后再滚过去，这种情况不在 16 ms 的保证内（ADR 0034「预算」）。

不放在本模块的原因：数换行是对内容的计算；行检查点是以 blob OID 为键的派生数据，按架构规则 5 由领域层缓存。本模块按路径和描述符工作，不知道 OID，在这里建行索引只能按路径缓存或每次从头扫描，后者正是 ADR 0034 要避免的。

### 对其他 crate 的影响

- `file_reader`（#10）的设计按以下约束编写：
  - 首屏：`open_file` 加一次 `read_ranges`（「首屏的一次调用」）。不超过 16 MiB 的文件，完整读取用同一个 `OpenFile` 在后台调用 `read_all(16 MiB)`，按「文件身份」核对：身份相同时保留首屏的结果；不同时首屏的结果作废，按完整读取的内容重新显示，重新定位的规则由 `file_reader` 决定。
  - 超过 16 MiB 的文件：持有 `OpenFile`（放进 `Arc`，可见的页和后台扫描共用），每页一次 `read_ranges`；返回的身份与打开时不同时，已读的页全部作废，重新打开。
  - 持有时间：不超过 16 MiB 的文件在完整读取之后 drop `OpenFile`；超过 16 MiB 的在阅读视图关闭或收到这个路径的 `fs_watch` 事件时 drop，收到事件时重新打开。
  - 偏移的口径：范围读取的偏移和字节都是磁盘上的原始字节。`git` 的 `*_byte` 和按 blob OID 缓存的行检查点按规范内容计（[git 设计「展示行」](git.md#展示行)），文件有 to-git 方向的转换（`core.autocrlf` 下的 CRLF、`working-tree-encoding`、`ident`）时两种偏移不同。工作区文件按哪种口径显示、偏移怎样换算，由 `file_reader` 的设计决定；本模块不做转换。
  - 工作区文件的 OID：查行检查点要先知道 OID。`OpenFile::meta()` 是完整的 `Meta`，可以按 git 的 stat 比较规则与 index 条目比较（一致且不是 racy-clean 时就是 index 里的 OID）；需要的接口由 `file_reader` 的设计向 `git` 提出。
  - 取得 `SafeRoot` 的途径（经 `git` 的 `Worktree`，或 `project`）由 `file_reader` 的设计决定。
- `git`：不改，`read` 的行为不变。`file_reader` 经 `Worktree` 打开文件时，`git` 增加一个转发 `open_file` 的方法，随 `file_reader` 的设计。
- `bench`：预算表增加 `platform/safe_read_range`、`platform/safe_read_page`，[bench 设计「预算表」](bench.md#预算表)和 `crates/bench/src/budgets.rs` 里都已加上（设计 PR 登记为 `Planned("#103")`），实现 PR 加了基准并改为 `Active`，见下文「性能」。
- [技术调研「性能预算」](tech-research.md#性能预算)「打开文件到首屏高亮」一行的「`platform` 待补的接口」改为指向本节（本 PR 已改）。
- `code_intel`、`cache`、`fs_watch`：不改。

### 性能

| 预算 | 范围读取的份额 |
|---|---|
| 打开文件到首屏高亮 < 16 ms | `open_file` 加首屏的 `read_ranges`（文件头 8 KiB、目标附近 132 KiB，页缓存已热）中位数 < 200 µs，约占一帧的 1.3% |
| 滚动与输入 120 fps | 超过 16 MiB 的文件分页：持有的 `OpenFile` 上一次 64 KiB 的 `read_ranges` 中位数 < 50 µs，在后台任务里，不在渲染函数里 |
| 空闲内存 | 不缓存；一个 `OpenFile` 是一个描述符和一个 `Meta`，读到的字节归调用方 |
| 冷启动 20 ms | 首帧之前不调用 |

预计耗时：首屏的调用是 4–5 次系统调用（打开一次几微秒，其余各约 1 µs）加从页缓存复制约 140 KiB（十几微秒），合计几十微秒，预算留有数倍的余量。页缓存是冷的时候要读磁盘，不设预算。

基准（M 系列 Mac，macOS 13 及以上，路径深度 4，64 MiB 的文件，页缓存已热，只测 `SafeRoot::open` 选中的生产后端）：

- `platform/safe_read_range`：目标在文件中部，`open_file`、按「首屏的一次调用」截取的 `read_ranges([0 .. 8 KiB, t.saturating_sub(64 KiB) .. min(t + 68 KiB, size)])`、drop（`t` 在中部，截取不起作用，两段不合并）；中位数不超过同一路径上 `File::open`、`metadata`、两次 `FileExt::read_at`、`metadata` 的 1.5 倍，且 < 200 µs。
- `platform/safe_read_page`：同一个 `OpenFile` 上依次读 64 KiB 的页；中位数不超过 `std::fs::File` 上 `read_at` 加 `metadata` 的 1.5 倍，且 < 50 µs。

### 已知限制

- 时间戳的精度：大小不变的改写只能由 `mtime`、`ctime` 发现。Linux 上没有 multigrain 时间戳的内核和文件系统按粗粒度时钟记录时间戳（一个 tick，约 1–4 ms），同一个 tick 内的两次改写可能得到相同的时间戳；HFS+ 的精度是 1 秒，FAT 是 2 秒。这些情况下身份不变而字节可能混有新旧内容，直到 `fs_watch` 报告事件、调用方重新打开。与 git 的 racy-clean 是同一类问题。
- 打开时正在进行的写入：Linux 的通用写路径先更新时间戳再复制数据。时间戳的更新早于打开时的 `fstat`、数据在读取期间才写入、大小又不变时，前后两次 `fstat` 相同，字节可能混有新旧内容；写入完成后 `fs_watch` 报告事件。
- 持有描述符：被删除的文件在 `OpenFile` drop 之前仍占磁盘空间；外接卷上有打开的 `OpenFile` 时卷不能推出。持有时间见「对其他 crate 的影响」。
- 一次 `read_ranges` 的多段之间没有原子性，每段对应它那次 `pread` 时的内容；身份相同时各段属于同一个版本。

### 测试

写在 `crates/platform/src/safe_read/tests.rs`，与 `read` 的行为测试一样对当前平台的每个后端各运行一次（Linux：`Openat2`、`Walk`；macOS：`NofollowAny`、`NofollowAnyParent`、`Walk`）：

- `open_file_*`：普通文件为 `Opened::File`，`meta()` 与 `SafeRoot::stat` 一致，1 TiB 的稀疏文件也立即返回（不读、不分配）；最后一级是符号链接为 `Symlink`，`target` 与 `std::fs::read_link` 相同；中间一级是符号链接为 `PathThroughSymlink`；目录、FIFO（单独的线程里调用，2 秒内返回）、socket、以 `/dev` 为根的 `null` 为 `Unsupported`；不存在、中间一级是普通文件为 `NotFound`。在 `read_*`、`symlink_*` 用到的同一组路径上，结果的分类与 `read` 逐一对应。`open_file_on_open_dir_root`：在 `open_dir` 返回的根上，上面这些情况的结果相同；打开新根之后把原路径换成指向根目录之外的符号链接，`open_file` 读到的仍是原来目录里的文件。
- `race_open_file_never_escapes`：同 `race_swap_never_escapes`，每次用 `open_file` 加 `read_ranges` 读 `a/secret`，10 000 次里只出现根目录之内的内容、`NotFound` 或 `PathThroughSymlink`。
- `range_*`：随机内容的文件上，各段等于文件的对应切片；跨过 EOF 的段在 EOF 处截止；起点在 EOF 或之后的段为空；空区间为空；重叠、相邻、乱序的区间结果正确；1 TiB 稀疏文件末尾附近的区间读出 0。0 个或 5 个区间、`end < start`、`end > i64::MAX`、长度之和超过 1 MiB 为 `InvalidInput`。`range_plan_*`：合并区间的纯函数把重叠和相邻的区间合成一段（文件头 8 KiB 与 `4 KiB .. 76 KiB` 合成 `0 .. 76 KiB`），不相邻的保持两段。`range_first_screen_at_file_start`：按「首屏的一次调用」的截取规则，`t = 0`、`t = 10 KiB`、`t = 72 KiB` 时窗口的下界截到 0 或不大于 8 KiB，两段合并为一次读取，各段内容正确；`t = 73 KiB` 时为两段；文件小于 8 KiB 和空文件时两段都是整个文件或为空，不报错。
- `identity_*`：没有变化时 `read_ranges`、`read_all`、`stat` 返回的身份都等于 `identity()`；追加、截短（段变短、`size` 变小）、大小不变的改写之后再用 `utimensat` 把 `mtime` 改回原值（`ctime` 仍不同），身份都不同。改写之前测试先轮询到系统的粗粒度时钟前进（至多 1 秒），避开同一个 tick。路径被 `rename` 覆盖之后，原来的 `OpenFile` 读到的仍是原内容，`SafeRoot::stat(rel)` 的 `identity()` 里 `ino` 不同；删除路径之后原来的 `OpenFile` 照常读取。
- `read_all_*`：小文件、空文件、恰好 `max` 字节、`max + 1` 字节、读取中变长的结果与 `read` 相同；打开时就超过 `max` 的为 `TooLarge`，不读内容；`meta` 是读完之后的 `fstat`。
- `open_file_concurrent`：8 个线程在同一个 `Arc<OpenFile>` 上各读 1000 次不同的区间，结果与单线程相同。
- `fd_*`、`send_sync` 补充：持有 n 个 `OpenFile` 时进程打开的描述符数比之前多 n，在它们上面反复 `read_ranges`、`read_all`、`stat` 不再增加，全部 drop 之后回到之前；`open_file` 得到 `Symlink`、`Unsupported` 或错误时（含每种类型和出错路径）返回之后描述符数不变；`OpenFile` 的描述符设了 `FD_CLOEXEC`；`OpenFile`、`Opened`、`RangeRead`、`FullRead`、`FileIdentity` 是 `Send + Sync`。
- 实现 PR 增加的测试：`read_all_procfs_file_with_size_zero_to_eof`（只在 Linux 上运行）：`fstat` 报告大小为 0 的 procfs 文件，`read_all` 读到 EOF 并受 `max` 约束，`read_ranges` 读出打开时的 `size` 之外的内容。`fd_open_files_hold_one_descriptor_each` 是上一条描述符计数的测试。`walk_*` 也对超过 `PATH_MAX` 的路径调用 `open_file` 和 `read_all`。

## 进程

`lectern_platform::process`，供 `lectern-bench` 测量进程的内存（[bench 设计「需要其他 crate 提供的接口」](bench.md#需要其他-crate-提供的接口)）。目前只有 `phys_footprint`，随 `cache` 第 2 步的 `cache/memory` 加入；`start_time`、`children` 随 bench 第 2 步在本节补充。

```rust
pub mod process {
    /// 进程 `pid` 的 physical footprint，即活动监视器里的「内存」。macOS 上是
    /// `proc_pid_rusage(pid, RUSAGE_INFO_V2)` 的 `ri_phys_footprint`；进程不存在或无权查看时返回系统的错误。
    /// 其他平台返回 `ErrorKind::Unsupported`。不分配、不建线程，任何线程可调用。
    pub fn phys_footprint(pid: u32) -> io::Result<u64>;
}
```

- 模块在所有平台上编译，macOS 的实现在 `#[cfg(target_os = "macos")]` 后面，是本 crate 里除测试外唯一的 `unsafe`（一次 FFI 调用和读取它填好的结构体）。
- 测试（`process_*`）：macOS 上，分配并写满 64 MiB 后本进程的 `phys_footprint` 至少增加 60 MiB；不存在的 pid 返回错误。Linux 上返回 `Unsupported`。

## 未决问题

没有需要产品决定的问题。技术问题都已在上文决定。

## 验收

一个实现 PR，写 `Refs #14`（#14 由 `review_view` 的最后一个 PR 关闭）。合并后 `git` 设计的验收第 1 步的前置条件成立。

- 根 `Cargo.toml` 的 `[workspace.dependencies]` 有 `rustix`（`default-features = false`，`std`、`fs`），`crates/platform/Cargo.toml` 用 `workspace = true` 引用；`git diff origin/main -- Cargo.lock | rg '^\+name = '` 没有结果（不新增 crate）。
- `cargo tree -p lectern-platform -e normal --depth 1` 只列出 `rustix`（安全读取这一步；FSEvents 后端加入后另有 `lectern-fs-watch`，macOS 上还有 `lectern-executor`，见「依赖」）。
- `just test -p lectern-platform` 在 Linux CI 和 macOS CI 上都通过，其中有 `relpath_*`、`stat_*`、`symlink_*`、`read_*`、`fifo_*`、`socket_*`、`device_*`、`race_*`、`fd_*`、`send_sync`、`walk_*`，Linux 上另有 `openat2_rejects_escape`、`linux_backend_choice`，macOS 上另有 `nofollow_any_*`。
- `rg -l 'unsafe' crates/platform/src/safe_read` 只列出 `tests.rs`（测试里创建 FIFO 的 `libc::mkfifo`）：实现代码没有 `unsafe`。
- `rg 'O_NOFOLLOW_ANY|NOFOLLOW_ANY' crates --glob '!crates/platform/**'` 没有结果（架构规则 6）。
- `just check`、`just deny` 通过；`python3 scripts/check_architecture.py` 通过（`platform` 不依赖 GPUI）。
- `just test -p lectern-platform -- --ignored perf_smoke` 在 macOS 上运行一次，PR 描述里记录 `stat` 与 `symlink_metadata`、`read` 与 `std::fs::read` 的比值，在「性能」的预算之内。（实现 PR 的验收。这个测试已由 bench 第 1 步删除，现在由 `just bench platform/` 测量，macOS CI 判定。）
- 本文的状态行改为「已实现」并附 PR 编号，填写「实际验证」；`crates/platform/Cargo.toml` 的 `description` 加上安全读取。

范围读取（[#103](https://github.com/SoloJiang/lectern-app/issues/103)）一个实现 PR，写 `Closes #103`，在 `file_reader`（#10）的实现之前合并：

- `just test -p lectern-platform` 在 Linux CI 和 macOS CI 上都通过，其中有 `open_file_*`（含 `open_file_on_open_dir_root`）、`race_open_file_never_escapes`、`range_*`（含 `range_first_screen_at_file_start`）、`range_plan_*`、`identity_*`、`read_all_*`、`open_file_concurrent`，`fd_*`、`send_sync` 覆盖新增的类型；原有的 `read_*`、`symlink_*`、`race_*` 结果不变。
- `rg -n 'mmap' crates/platform/src` 没有结果；`rg -l 'unsafe' crates/platform/src/safe_read` 只列出 `tests.rs`。
- `git diff origin/main -- Cargo.lock | rg '^\+name = '` 没有结果（`pread` 来自已有的 `rustix`）。
- `platform/safe_read_range`、`platform/safe_read_page` 的基准加在 `crates/bench/benches/time/platform.rs`，`crates/bench/src/budgets.rs` 里的这两条由 `Planned("#103")` 改为 `Active`（[bench 设计「预算表」](bench.md#预算表)）；macOS CI 的 `just bench platform/` 在「范围读取」的「性能」预算之内，数字写进「实际验证」。
- 本文的状态行改为已实现并附 PR 编号，填写「实际验证」和「与设计的差异」。

## 实际验证

在 Linux（x86_64，内核 6.18，`openat2` 可用）上运行。macOS 专属的部分（`NofollowAny`、`nofollow_any_*`、macOS 上的 `perf_smoke`）没有在 macOS 上运行过，由 macOS CI 验证：

- `cargo check -p lectern-platform --target aarch64-apple-darwin` 和 `cargo clippy -p lectern-platform --all-targets --all-features --target aarch64-apple-darwin -- -D warnings` 在 Linux 上交叉检查通过：macOS 的代码（`OFlags::NOFOLLOW_ANY`、`dev_t` 为 `i32`、`mode_t` 为 `u16`、测试里的 `libc::mkfifo`）通过了类型检查和 clippy，但没有运行。
- `just check`、`just test`（整个工作区 331 个测试通过，跳过 1 个 `#[ignore]`）、`just doctest`、`just deny`、`python3 scripts/check_architecture.py` 全部通过。
- `just test -p lectern-platform`：30 个测试通过，跳过 `perf_smoke`。其中 27 个在两个平台上都运行，`openat2_rejects_escape`、`linux_backend_choice`、`read_procfs_file_with_size_zero_to_eof` 只在 Linux 上运行；macOS 上另有 6 个 `nofollow_any_*`（共 33 个）。行为测试对 `Openat2` 和 `Walk` 各运行一次；`linux_backend_choice` 确认这台机器的内核上 `SafeRoot::open` 选的是 `Openat2`。
- `race_*`、`fifo_*`、`send_sync` 连续运行 40 次都通过；`race_swap_never_escapes` 每次运行中三种结果（根目录之内的内容、`NotFound`、`PathThroughSymlink`）都出现。
- 测试环境以 root 运行，`walk_permission_denied_is_io` 在 root 下不检查（root 不受权限位限制）；用 `setpriv` 以 `nobody` 运行测试二进制时它通过，其余测试也都通过。
- 验收的检查：`git diff origin/main -- Cargo.lock | rg '^\+name = '` 没有结果（只给 `lectern-platform` 加了 `libc`、`rustix`、`tempfile` 三条依赖，都是 Cargo.lock 里已有的版本）；`cargo tree -p lectern-platform -e normal --depth 1` 只列出 `rustix v1.1.5`；`rg -l 'unsafe' crates/platform/src/safe_read` 只列出 `tests.rs`（macOS 上的 `libc::mkfifo`；Linux 的测试用 rustix 的 `mknodat` 创建 FIFO）。`rg 'O_NOFOLLOW_ANY|NOFOLLOW_ANY' crates --glob '!crates/platform/**'` 只匹配 `crates/git/AGENTS.md` 里解释这条规则的一句文字，`rg -t rust` 没有结果：代码里只有本 crate 用到这个标志。
- `perf_smoke`（release，728 个 4 KiB 文件，深度 4，各 10 000 次，连续 3 次运行）在 Linux 上的结果：

  | 实现 | `stat` | `symlink_metadata` | 比值 | `read` 4 KiB | `std::fs::read` | 比值 |
  |---|---|---|---|---|---|---|
  | `Openat2` | 1.13–1.18 µs | 0.54–0.59 µs | 2.0–2.1 | 2.05–2.53 µs | 2.37–2.53 µs | 0.87–1.05 |
  | `Walk` | 2.20–2.37 µs | 0.54–0.59 µs | 4.0–4.1 | 3.92–4.11 µs | 2.37–2.53 µs | 1.6–1.7 |

  Linux 默认的 `Openat2` 在「性能」的预算之内。`Walk` 只在 `openat2` 不可用或一步打开出错时使用，不在预算的范围内。预算按 M 系列 Mac 制定，macOS 上的数字随 macOS 上的那次运行写进 PR 描述。
- macOS 的 `stat` 改为一次 `fstatat`（见「与设计的差异」）之后：`cargo check -p lectern-platform --target aarch64-apple-darwin` 和 `cargo clippy -p lectern-platform --all-targets --all-features --target aarch64-apple-darwin -- -D warnings` 在 Linux 上交叉检查通过，`just check`、`just test`、`just deny` 在 Linux 上通过（Linux 的实现没有改）。新增的 `nofollow_any_one_step_stat_follows_no_symlink`、`nofollow_any_backend_choice` 和其余 `nofollow_any_*` 没有在 macOS 上运行过，由 macOS CI 运行；比值由 macOS CI 的 `platform/safe_stat` 判定。macOS 11、12 的 `NofollowAnyParent` 由测试强制选择后在 CI 上运行，没有在 11、12 上运行过；`nofollow_any_backend_choice` 在 CI 上确认探测成功、选中 `NofollowAny`。
- `SafeRoot::open_dir`（[#124](https://github.com/SoloJiang/lectern-app/pull/124)）在 Linux（x86_64，内核 6.18，`openat2` 可用）上：`just test -p lectern-platform` 47 个测试通过，其中新增 7 个（`open_dir_*` 6 个，`race_open_dir_swap_never_escapes` 1 个；`open_dir_openat2_rejects_escape` 只在 Linux 上运行），对 `Openat2` 和 `Walk` 各运行一次。`race_*`、`open_dir_*`、`fd_*` 连续运行 40 次都通过；`race_open_dir_swap_never_escapes` 每次运行中打开成功和被拒绝（`NotFound`、`PathThroughSymlink`）两类结果都出现（一次运行：`Openat2` 1237 次成功、3763 次被拒绝，`Walk` 2184 次、2816 次）。`just check`、`just test`（整个工作区 1270 个测试通过，跳过 1 个）、`just doctest`、`just deny` 通过。`cargo clippy -p lectern-platform --target aarch64-apple-darwin --all-features --all-targets -- -D warnings` 在 Linux 上交叉检查通过；macOS 的 `NofollowAny`、`NofollowAnyParent` 上的 `open_dir_*` 和 race 测试没有在 macOS 上运行过，由 macOS CI 运行。没有新增依赖，`rg -l 'unsafe' crates/platform/src/safe_read` 仍只列出 `tests.rs`。
- 范围读取（[#103](https://github.com/SoloJiang/lectern-app/issues/103)，实现 [#PR](https://github.com/SoloJiang/lectern-app/pull/PR)）在 Linux（x86_64，内核 6.18，`openat2` 可用，4 个 vCPU 的虚拟机）上：
  - `just test -p lectern-platform` 72 个测试通过，其中新增 25 个：`open_file_*` 7 个（含 `open_file_on_open_dir_root`、`open_file_concurrent`）、`race_open_file_never_escapes`、`range_*` 7 个（含 `range_plan_merges_overlapping_and_adjacent`、`range_first_screen_at_file_start`）、`identity_*` 5 个、`read_all_*` 4 个（`read_all_procfs_file_with_size_zero_to_eof` 只在 Linux 上运行）、`fd_open_files_hold_one_descriptor_each`。行为测试对 `Openat2` 和 `Walk` 各运行一次；`fd_count_is_unchanged_after_many_calls`、`send_sync`、`walk_matches_the_table_when_the_one_step_open_fails` 也覆盖 `open_file` 和新增的类型。原有的 47 个测试结果不变。用 `cargo test` 在同一个进程里运行，72 个也都通过。
  - `race_*`、`fd_*`、`open_file_*`、`identity_*`、`send_sync` 连续运行 40 次都通过。`race_open_file_never_escapes` 每次运行中三种结果都出现，一次运行的分布：`Openat2` 932 次读到根目录之内的内容、7585 次 `NotFound`、1483 次 `PathThroughSymlink`；`Walk` 依次为 1600、2667、5733 次。
  - `just check`、`just test`（整个工作区 1312 个测试通过，跳过 1 个）、`just doctest`、`just deny` 通过。`cargo clippy -p lectern-platform --target aarch64-apple-darwin --all-features --all-targets -- -D warnings` 在 Linux 上交叉检查通过。macOS 的 `NofollowAny`、`NofollowAnyParent` 上的范围读取测试没有在 macOS 上运行过，由 macOS CI 运行。
  - 验收的检查：`rg -n 'mmap' crates/platform/src` 没有结果；`rg -l 'unsafe' crates/platform/src/safe_read` 只列出 `tests.rs`；`git diff origin/main -- Cargo.lock` 为空（`pread` 来自已有的 `rustix`）。
  - `just bench --measure platform/`（release，64 MiB 的文件，路径深度 4，页缓存已热）运行 4 次。测量期间机器上有其他构建，负载在 14 左右，绝对时间的波动主要来自这一点，std 的对照随之同涨同落：

    | 基准 | `SafeRoot` 中位数 | std 对照中位数 | 比值 | 预算 |
    |---|---|---|---|---|
    | `platform/safe_read_range` | 84.2–147 µs | 83.9–178 µs | 0.83–1.10 | ≤ 1.5，< 200 µs |
    | `platform/safe_read_page` | 28.4–61.3 µs | 30.1–63.5 µs | 0.94–1.03 | ≤ 1.5，< 50 µs |

    比值 4 次都在预算之内。`safe_read_page` 的绝对时间有 1 次（61.3 µs，std 对照 63.5 µs）超过 50 µs，其余 3 次（28.4、34.6、41.2 µs）在预算之内。预算按 M 系列 Mac 制定，由 macOS CI 判定；Linux 上只报告。同一次运行里原有的 `platform/safe_stat`（比值 0.88，1.25 µs）、`platform/safe_read_4k`（比值 0.77）也在预算之内。

## 与设计的差异

- `Walk` 在 `fstatat` 的结果与之前那次打开的错误矛盾时重做这一级，最多 8 次（见「Walk」）。设计写的是直接返回 `Io(原来的错误)`：目录与符号链接来回交换时，这种做法偶尔会让 `race_swap_never_escapes` 得到 `Io`，而这个测试只允许三种结果。重做不改变「不读到根目录之外」，只让并发替换时的结果与某一时刻的状态一致。
- `read` 预留 `size + 1` 字节，不是 `size`：读完 `size` 字节后还要再读一次才能确认 EOF，多 1 字节让这次读取不必重新分配。文件在 `fstat` 之后变长（或像 procfs 那样报告大小为 0）时按倍增扩容，总量不超过 `max + 1`，避免 `max` 很大时按固定大小的块反复重新分配。
- `Read` 派生 `Clone, PartialEq, Eq, Debug`，`SafeRoot` 派生 `Debug`（设计没有写派生）：测试要比较多个线程的结果，调用方也需要打印它们。已改在「接口」里。
- `fd_count_is_unchanged_after_many_calls`：在 nextest（`just test`，每个测试一个进程）里断言描述符数完全不变；在 `cargo test` 里，其他测试在同一进程的线程上同时打开描述符，改为断言增加的数量小于调用轮数的一半（任何一条路径泄漏，每轮至少多一个描述符）。
- `SafeRoot::open_with` 只在 `#[cfg(test)]` 下编译：除测试外没有调用方。Analyze 快照（#66）复用的是 `walk` 和一步打开的 `pub(crate)` 函数，不需要它。
- `SafeRoot::list` 和 `DirEntry` 随 [ADR 0028](../adr/0028.md) 加入（PR #87）：原设计把目录遍历留给 gix，gix 按路径遍历，被审阅的进程能在遍历期间把目录换成指向工作区之外的符号链接。「范围」「接口」「实现」「测试」已改。Linux 上 `list_*` 和 `race_list_swap_never_escapes` 对 `Openat2` 和 `Walk` 都通过；macOS 的实现与 `read` 共用 `O_NOFOLLOW_ANY` 的一步打开，没有在 macOS 上运行。
- macOS 的 `stat` 改为一次 `fstatat(root, 路径, AT_SYMLINK_NOFOLLOW | AT_SYMLINK_NOFOLLOW_ANY)`（见「stat」），原设计是以 `O_NOFOLLOW_ANY` 打开父目录、`fstatat`、关闭，共 3 次系统调用。bench 第 1 步（[#83](https://github.com/SoloJiang/lectern-app/pull/83)）的 `platform/safe_stat` 在 macOS CI 的 arm64 托管机器上测得原做法的中位数为 8.1–8.8 µs，是同一路径上 `symlink_metadata` 的 3.94–4.16 倍，超出「性能」的 3 倍预算；Linux 的 `Openat2` 为 1.19 倍，没有改。成功时的 `Meta`、`ENOENT` 为 `NotFound`、其他错误交给 `Walk` 重做，都与原做法相同；结果只在一种情况下不同：只有搜索权限的目录（如 `0711`）下的条目由 `Io(PermissionDenied)` 变为成功，与 `lstat` 一致（「已知限制」已改）。`fstatat` 从 macOS 13 起才接受 `AT_SYMLINK_NOFOLLOW_ANY`，macOS 11、12 上返回 `EINVAL`；那里的 `stat` 保留原做法，作为单独的实现 `NofollowAnyParent`，由 `SafeRoot::open` 探测选择（见「macOS 的实现选择」），不交给 `Walk`。`Target`（一步打开父目录还是条目）随之只在 Linux 上使用，移进 `linux.rs`。
- `open_dir` 的实现与「open_dir」一致，接口没有改。`list` 和 `open_dir` 共用一个打开函数（一步打开、`fstat` 确认是目录、出错时交给 `Walk`），`Walk` 里列目录的那一步随之改为只打开并返回目录描述符，由 `list` 读目录项；`list` 的行为不变。增加了设计没有列出的测试：`open_dir_root_follows_the_same_rules_beneath_it`（新根之下的符号链接规则，`..` 和绝对路径被拒绝）、`open_dir_openat2_rejects_escape`（Linux）、`race_open_dir_swap_never_escapes`，`fd_*` 也覆盖 `open_dir`；「测试」已补上。
- 增加了设计没有列出的测试：`stat_after_the_root_is_deleted_is_not_found`、`stat_overlong_name_is_io`、`walk_permission_denied_is_io`、`read_procfs_file_with_size_zero_to_eof`（Linux）、`errors_convert_to_io_errors`、`timestamp_as_nanos_handles_times_before_1970`、`file_kind_follows_the_mode`，分别对应「状态与异常」「读取上限」里的条目和 `SafeError` 的转换。
- 范围读取（[#103](https://github.com/SoloJiang/lectern-app/issues/103)，实现 [#PR](https://github.com/SoloJiang/lectern-app/pull/PR)）的接口和行为与「范围读取」一致，没有差异。实现上需要说明的有三处，都不改变接口：
  - `read` 与 `open_file` 共用一个内部的打开函数（一步打开、对描述符 `fstat` 分类、出错时交给 `Walk`），`Walk` 里读最后一级的那一步随之改为只打开并分类、返回描述符或 `Symlink`、`Unsupported`，由 `read` 读内容、由 `open_file` 转交给 `OpenFile`；`read` 的行为不变，原有测试的结果不变。`read_all` 与 `read` 共用「读取上限」的函数，前者用 `pread` 从偏移 0 读，后者用 `read`。
  - 「读取的规则」第 3 步的按需扩容：一段的缓冲读满而这段还没读完时（读到了打开时的 `size`），先用栈上 4 KiB 的缓冲 `pread` 一次，返回 0 就是 EOF，不分配；读到数据（文件变长了）才按倍增扩容，不超过段长。所以分配总是不超过 `min(段长, 打开时的 size − 起点)`，除非文件确实变长。终点到达 EOF 的段因此多一次返回 0 的 `pread`，与「系统调用数」里「短读时多几次」一致。
  - 一段里最后取字节的那个区间覆盖了这段读到的全部字节时，这段的缓冲直接移给它的 `pieces[i]`，不复制；其余区间从段的缓冲复制。首屏在文件中部时两段不合并，两个区间都不复制。
- 范围读取的测试里对 `clippy::single_range_in_vec_init` 加了 `allow`（附理由）：`read_ranges` 的参数是区间的切片，`&[0..100]` 是正常的单区间调用，这个 lint 把它当作误写的 `Vec`。bench 里用 `std::slice::from_ref` 传单个区间。
