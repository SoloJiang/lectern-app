# platform 设计

状态：安全读取已实现（设计 [#73](https://github.com/SoloJiang/lectern-app/pull/73)，实现 [#74](https://github.com/SoloJiang/lectern-app/pull/74)）。FSEvents 与 kqueue 后端（模块 `fs_events`，设计在 [fs_watch 设计「platform 后端」](fs_watch.md#platform-后端)）已实现（[#PR](https://github.com/SoloJiang/lectern-app/pull/PR)），实测结果见 fs_watch 设计的「实际验证」。

对应 issue [#14](https://github.com/SoloJiang/lectern-app/issues/14)。`platform` 收纳只能按操作系统实现的能力（架构规则 6）。本文目前只覆盖其中的安全读取：从一个目录根出发，按相对路径读取文件的元数据和内容，路径的任何一级都不跟随符号链接，也不越出根目录。`git` 读工作区文件只经这个接口（[git 设计「安全读取」](git.md#安全读取)、[ADR 0024](../adr/0024.md)），它也是 Analyze 快照「写出时不跟随符号链接，不读到 worktree 之外的文件」的基础（[ADR 0019](../adr/0019.md) 约束 2）。

## 范围

- 做（本文）：
  - `SafeRoot`：持有一个目录根的描述符，按 `RelPath` 做不跟随符号链接的 `stat` 和读取。
  - `RelPath`：根目录之下的相对路径，构造时校验，排除 `..`、`.`、空名字、NUL 和绝对路径。
  - `Meta`、`FileKind`：git 的 stat 比较需要的全部字段。
  - 读取结果的分类：普通文件、符号链接（读出目标，不跟随）、超过上限、不读取的类型（目录、FIFO、socket、设备文件）；错误分为不存在、路径中间是符号链接、其他 IO 错误。
  - 三个实现：Linux 的 `openat2`、macOS 的 `O_NOFOLLOW_ANY` 与 `AT_SYMLINK_NOFOLLOW_ANY`、两个平台共用的逐级 `openat`（Linux 没有 `openat2` 时的后备，也是出错时判定原因的实现）。
- 不做：
  - 目录遍历和未跟踪文件的枚举：`git` 用 gix 的遍历（ADR 0024「已知限制」）。
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
| `libc`（dev） | 测试里在 macOS 上创建 FIFO（rustix 在 Apple 平台没有 `mkfifoat`） | 已在 Cargo.lock（0.2.190）；只在测试代码里用 |
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

所有描述符都带 `O_CLOEXEC`（ADR 0019：子进程不继承 Lectern 打开的描述符），由 `OwnedFd` 持有，出错路径上也随 drop 关闭。

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

### Walk

一次调用内复用描述符：每一级用上一级的描述符打开，打开下一级之后关闭上一级，同时最多持有两个描述符；最后一级的各个步骤都用同一个父目录描述符。不跨调用保留描述符。

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

- `git`：`Worktree` 持有工作区根的 `SafeRoot`；工作区比较的第 1、2 步调 `stat`，第 3 步调 `read`（上限 64 MiB），`Worktree::read_file` 调 `read`。`git` 的 `RepoPath` 满足 `RelPath` 的不变量，调用时 `RelPath::new(path.as_bytes())` 总是 Some，不复制字节。
- `project`：经 `git` 的 `Worktree::read_file` 读清单文件（[project 设计「依赖 git 的接口」](project.md#依赖-git-的接口)）；没有 git 的文件夹项目若要同样的保证，可以直接用 `SafeRoot`，由 `project` 的设计决定。
- Analyze 快照（#66）：在本 crate 内复用同一套打开逻辑（`pub(crate)` 的一步打开和 `Walk`），叠加工作区文件时从根描述符打开、对同一个描述符判断类型后克隆，设计随 #66。

线程：`SafeRoot` 只有一个 `OwnedFd` 和一个创建后不变的实现选择，二者都是 `Send + Sync`；所有操作都是以描述符为起点的 `*at` 系统调用，不改当前目录，不用根描述符的文件偏移，不读环境变量，没有进程级的缓存或锁，所以多个线程可以同时调用同一个 `SafeRoot`。每次调用自己打开和关闭需要的描述符（最多两个），调用之间不持有。

本模块不缓存任何东西：stat 缓存和内容的 OID 缓存在 `git`（架构规则 5 的派生数据由调用方以 blob OID 为键缓存）。

## 性能

| 预算 | 本模块的份额 |
|---|---|
| 冷启动 20 ms（[ADR 0017](../adr/0017.md)） | 首帧之前不调用；`SafeRoot::open` 在 `git` 的后台任务里 |
| 空闲内存 | 每个 `SafeRoot` 一个描述符，没有缓存；一次 `read` 的内存是文件本身（不超过 `max`） |
| 打开一个会话的 Diff < 50 ms | `git` 的 `changes` 对全部跟踪文件 `stat`：728 个文件的 `stat` 合计 < 5 ms（M 系列 Mac） |
| 安装包 ≤ 80 MB（[ADR 0018](../adr/0018.md)） | rustix 已在发布构建里（经 GPUI），不新增 crate；本模块的代码在几十 KB 量级 |

与 `std::fs` 的对比：`std::fs::symlink_metadata` 是 1 次 `lstat`，但它跟随中间一级的符号链接，不能直接用。`stat` 的路径解析工作与 `lstat` 相同（父目录一次解析、最后一级一次查找）：macOS 13 起与 `lstat` 一样是 1 次系统调用；Linux 和 macOS 11、12 上额外是 1 次打开和 1 次关闭，只有一级的路径是 1 次。普通文件的 `read` 与 `std::fs::read`（`open`、`statx` / `fstat`、`read`、`close`）的系统调用数相同。符号链接、socket、中间一级出错的路径走 `Walk`，系统调用数随深度增长，这些路径在仓库里少见。

预算（M 系列 Mac，macOS 13 及以上，路径深度 4，页缓存已热）：`stat` 的中位数不超过同一路径上 `symlink_metadata` 的 3 倍且 < 10 µs；4 KiB 文件的 `read` 不超过 `std::fs::read` 的 1.5 倍。超出时先考虑在一次 `changes` 内复用父目录描述符（index 按路径排序，相邻路径常在同一目录），这要改接口，另改本文。

基准：

- `bench` 的 `platform/safe_stat`、`platform/safe_read_4k`（[bench 设计「迁移」](bench.md#迁移)）：bench 自己生成 728 个 4 KiB 文件、深度 4 的目录树，与同一路径上的 `symlink_metadata`、`std::fs::read` 交替采样，按上面的比值判定；只测 `SafeRoot::open` 选中的生产后端（macOS 上是 `NofollowAny`，Linux 上是 `Openat2`），`Walk` 不设基准。`git` 的 `git/changes_clean` 包含它们的成本。
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
- `fd_*`：反复调用（含各种出错路径）之后，进程打开的描述符数不变（Linux 读 `/proc/self/fd`，macOS 读 `/dev/fd`）；`SafeRoot` 的描述符设了 `FD_CLOEXEC`。
- `send_sync`：编译期断言 `SafeRoot`、`RelPath`、`Meta`、`Read`、`SafeError` 是 `Send + Sync`；8 个线程同时对同一个 `SafeRoot` 调用 `stat` 和 `read`，结果与单线程相同。
- `walk_*`：`Walk` 的判定在一步打开失败的每种情况下（中间是符号链接、中间是普通文件、最后一级是符号链接、socket、路径超过 `PATH_MAX`）与上表一致。

只在 Linux 上运行：

- `openat2_rejects_escape`：绕过 `RelPath`、直接把 `"../x"`、`"/etc/passwd"`、`"a/../../x"` 交给 `Openat2` 的一步打开，内核拒绝（`EXDEV`），不打开根目录之外的文件。
- `linux_backend_choice`：探测结果到实现的映射是纯函数，`ENOSYS`、`EPERM`、`EINVAL`、`E2BIG` 选 `Walk`，成功和其他错误选 `Openat2`；在 CI 的内核上 `SafeRoot::open` 选的是 `Openat2`。

只在 macOS 上运行：

- `nofollow_any_*`：上面的 `symlink_*`、`race_*`、`walk_*` 在 `NofollowAny` 和 `NofollowAnyParent` 上各自通过；探测结果到实现的映射是纯函数（`EINVAL` 选 `NofollowAnyParent`，成功和其他错误选 `NofollowAny`），`sw_vers` 报告 13 及以上时 `SafeRoot::open` 选的是 `NofollowAny`，更早时是 `NofollowAnyParent`；`O_NOFOLLOW_ANY` 打开父目录时对符号链接返回错误；`O_NOFOLLOW_ANY` 的一步打开对中间一级和最后一级的符号链接都返回错误，不跟随；`stat` 的一步 `fstatat` 对多级路径上的普通文件和目录一次成功（不落到 `Walk`），元数据与 `symlink_metadata` 一致，对中间一级的符号链接（含悬空链接）返回 `ENOENT` 以外的错误，最后一级是符号链接时返回链接本身的元数据或 `ENOENT` 以外的错误，从不返回链接目标的元数据。

`git` 的工作区测试（FIFO 为 `UnsupportedType`、中间一级是符号链接为 `PathThroughSymlink`）在 Linux 上经本模块的 Linux 实现运行。

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

## 与设计的差异

- `Walk` 在 `fstatat` 的结果与之前那次打开的错误矛盾时重做这一级，最多 8 次（见「Walk」）。设计写的是直接返回 `Io(原来的错误)`：目录与符号链接来回交换时，这种做法偶尔会让 `race_swap_never_escapes` 得到 `Io`，而这个测试只允许三种结果。重做不改变「不读到根目录之外」，只让并发替换时的结果与某一时刻的状态一致。
- `read` 预留 `size + 1` 字节，不是 `size`：读完 `size` 字节后还要再读一次才能确认 EOF，多 1 字节让这次读取不必重新分配。文件在 `fstat` 之后变长（或像 procfs 那样报告大小为 0）时按倍增扩容，总量不超过 `max + 1`，避免 `max` 很大时按固定大小的块反复重新分配。
- `Read` 派生 `Clone, PartialEq, Eq, Debug`，`SafeRoot` 派生 `Debug`（设计没有写派生）：测试要比较多个线程的结果，调用方也需要打印它们。已改在「接口」里。
- `fd_count_is_unchanged_after_many_calls`：在 nextest（`just test`，每个测试一个进程）里断言描述符数完全不变；在 `cargo test` 里，其他测试在同一进程的线程上同时打开描述符，改为断言增加的数量小于调用轮数的一半（任何一条路径泄漏，每轮至少多一个描述符）。
- `SafeRoot::open_with` 只在 `#[cfg(test)]` 下编译：除测试外没有调用方。Analyze 快照（#66）复用的是 `walk` 和一步打开的 `pub(crate)` 函数，不需要它。
- macOS 的 `stat` 改为一次 `fstatat(root, 路径, AT_SYMLINK_NOFOLLOW | AT_SYMLINK_NOFOLLOW_ANY)`（见「stat」），原设计是以 `O_NOFOLLOW_ANY` 打开父目录、`fstatat`、关闭，共 3 次系统调用。bench 第 1 步（[#83](https://github.com/SoloJiang/lectern-app/pull/83)）的 `platform/safe_stat` 在 macOS CI 的 arm64 托管机器上测得原做法的中位数为 8.1–8.8 µs，是同一路径上 `symlink_metadata` 的 3.94–4.16 倍，超出「性能」的 3 倍预算；Linux 的 `Openat2` 为 1.19 倍，没有改。成功时的 `Meta`、`ENOENT` 为 `NotFound`、其他错误交给 `Walk` 重做，都与原做法相同；结果只在一种情况下不同：只有搜索权限的目录（如 `0711`）下的条目由 `Io(PermissionDenied)` 变为成功，与 `lstat` 一致（「已知限制」已改）。`fstatat` 从 macOS 13 起才接受 `AT_SYMLINK_NOFOLLOW_ANY`，macOS 11、12 上返回 `EINVAL`；那里的 `stat` 保留原做法，作为单独的实现 `NofollowAnyParent`，由 `SafeRoot::open` 探测选择（见「macOS 的实现选择」），不交给 `Walk`。`Target`（一步打开父目录还是条目）随之只在 Linux 上使用，移进 `linux.rs`。
- 增加了设计没有列出的测试：`stat_after_the_root_is_deleted_is_not_found`、`stat_overlong_name_is_io`、`walk_permission_denied_is_io`、`read_procfs_file_with_size_zero_to_eof`（Linux）、`errors_convert_to_io_errors`、`timestamp_as_nanos_handles_times_before_1970`、`file_kind_follows_the_mode`，分别对应「状态与异常」「读取上限」里的条目和 `SafeError` 的转换。
