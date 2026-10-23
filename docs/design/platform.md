# platform 设计

状态：安全读取已实现（设计 [#73](https://github.com/SoloJiang/lectern-app/pull/73)，实现 [#74](https://github.com/SoloJiang/lectern-app/pull/74)，macOS 13+ 的 `stat` 改为一次系统调用 [#88](https://github.com/SoloJiang/lectern-app/pull/88)）。FSEvents 与 kqueue 后端（模块 `fs_events`，设计在 [fs_watch 设计「platform 后端」](fs_watch.md#platform-后端)）已实现（[#90](https://github.com/SoloJiang/lectern-app/pull/90)），实测结果见 fs_watch 设计的「实际验证」。进程的 `phys_footprint` 已实现（[#91](https://github.com/SoloJiang/lectern-app/pull/91)，见「进程」）。列目录（`SafeRoot::list`，[ADR 0028](../adr/0028.md)）随 [#87](https://github.com/SoloJiang/lectern-app/pull/87) 加入。`SafeRoot::open_dir`（[ADR 0033](../adr/0033.md)）已实现（[#124](https://github.com/SoloJiang/lectern-app/pull/124)）。范围读取（`SafeRoot::open_file`、`OpenFile`，[#103](https://github.com/SoloJiang/lectern-app/issues/103)，见「范围读取」）已实现（设计 [#125](https://github.com/SoloJiang/lectern-app/pull/125)，实现 [#126](https://github.com/SoloJiang/lectern-app/pull/126)）。`file_reader`（[#10](https://github.com/SoloJiang/lectern-app/issues/10)）的两个前置条件已实现（[#147](https://github.com/SoloJiang/lectern-app/pull/147)）：原子替换 `SafeRoot::replace_file`（前置条件 C，见「原子替换」）和线程的 CPU 时钟 `ThreadCpuClock`（前置条件 G，见「线程的 CPU 时钟」），接口按 [file_reader 设计「对其他 crate 的要求」](file_reader.md#对其他-crate-的要求)。应用数据目录 `dirs::app_data`（review 前置条件 D 的 `platform` 部分，见「应用数据目录」）已实现（[#166](https://github.com/SoloJiang/lectern-app/pull/166)）。日志目录 `dirs::logs` 与系统版本 `system::os_version`（[logging 设计](logging.md)第 2 步，见「日志目录与系统版本」）已实现（[#173](https://github.com/SoloJiang/lectern-app/pull/173)）。本地时区的偏移 `time::utc_offset`（[usage 设计](usage.md)的前置条件 B，见「本地时区的偏移」）已实现（[#176](https://github.com/SoloJiang/lectern-app/pull/176)）。系统首选语言 `system::preferred_languages`（[app 设计](app.md)的前置条件 B，见「系统首选语言」）已实现。kqueue 线程停止时的记录接入 `lectern-logging`（[logging 设计「对其他 crate 的要求」](logging.md#对其他-crate-的要求)第 9 项的 `platform` 部分，见「日志与诊断」）已实现（[#192](https://github.com/SoloJiang/lectern-app/pull/192)）。

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
  - 原子替换（`SafeRoot::replace_file`）：编辑模式的保存，在同一目录里写临时文件、比较身份后 `renameat` 替换（见「原子替换」）。
  - 线程的 CPU 时钟（`ThreadCpuClock`）：`syntax::health` 判定 grammar 卡死（[ADR 0041](../adr/0041.md)，见「线程的 CPU 时钟」）。
  - 应用数据目录（`dirs::app_data`）：不能重建的用户数据的位置，review 的标注日志用它（[ADR 0044](../adr/0044.md)，见「应用数据目录」）。
  - 日志目录（`dirs::logs`）与系统版本（`system::os_version`）：`logging` 落盘的位置和文件头里的系统版本（[ADR 0046](../adr/0046.md)，见「日志目录与系统版本」）。
  - 本地时区的偏移（`time::utc_offset`）：`usage` 的 `LocalZone` 按它把时间窗口分成本地自然日（[ADR 0020](../adr/0020.md)，见「本地时区的偏移」）。
  - 系统首选语言（`system::preferred_languages`）：`app` 在首帧之前用它选界面语言（[ADR 0013](../adr/0013.md)，见「系统首选语言」）。
- 不做：
  - 递归遍历、忽略规则和未跟踪文件的判定：`git` 在 `list` 之上实现（ADR 0028）；本模块只列一层目录。
  - 路径的大小写折叠与 Unicode 规范化：`git` 按 `core.ignoreCase`、`core.precomposeUnicode` 处理；本模块把字节原样交给文件系统。
  - 内置转换、OID 计算、stat 比较本身、按路径缓存：`git`。
  - 原子替换以外的写文件、克隆文件：随 Analyze 快照（[#66](https://github.com/SoloJiang/lectern-app/issues/66)）设计，复用本模块的打开逻辑（见「数据流与线程」）。`replace_file` 打开父目录用的就是这套逻辑（`list` 与 `open_dir` 共用的目录打开函数），#66 的写出与它共用。
- 本文不覆盖，以后在本文增加小节或另写设计文档，设计合并之前不写对应的代码：
  - FSEvents 与 kqueue 后端（实现 `fs_watch` 的接口，模块 `fs_events`）：设计、实际验证和与设计的差异都在 [fs_watch 设计「platform 后端」](fs_watch.md#platform-后端)。
  - `open -a`、钥匙串、代码签名校验、线程 QoS（[executor 设计](executor.md)的线程启动钩子）。
  - Seatbelt 配置、受限进程的启动与监护、磁盘映像、快照写出（[#27](https://github.com/SoloJiang/lectern-app/issues/27)、[#39](https://github.com/SoloJiang/lectern-app/issues/39)、[#66](https://github.com/SoloJiang/lectern-app/issues/66)，ADR 0019）。
  - Claude Code statusline 的安装与恢复（[#32](https://github.com/SoloJiang/lectern-app/issues/32)、[ADR 0014](../adr/0014.md)）。

## 依赖

| 依赖 | 用途 | 选用理由 |
|---|---|---|
| `rustix` 1.1（`default-features = false`，`std`、`fs`） | `openat2`、`openat`、`fstatat`、`fstat`、`readlinkat`、`read`；`replace_file` 的 `write`、`fchmod`、`fsync`、`renameat`、`unlinkat` | 见下 |
| `tempfile`（dev） | 测试用的临时目录 | 已在 Cargo.lock（3.27，经 `insta`），MIT OR Apache-2.0 |
| `libc`（Linux 和 macOS） | `ThreadCpuClock`：Linux 的 `pthread_getcpuclockid`、`clock_gettime`，macOS 的 `pthread_mach_thread_np`、`thread_info`；两个平台的 `localtime_r`（`time::utc_offset`）；macOS 上还有 `proc_pid_rusage`（`process::phys_footprint`）、`sysctlbyname`（`system::os_version`）和测试里创建 FIFO（rustix 在 Apple 平台没有 `mkfifoat`） | 已在 Cargo.lock（0.2.190），原来只在 macOS 上依赖，加上 Linux 不改 Cargo.lock；rustix 没有这几个调用（它的 `clock_gettime` 只接受固定的几种时钟） |
| `lectern-fs-watch` | `fs_events` 实现它的 `Backend`；事件转换是纯函数，在所有平台上编译和测试 | 基础设施层，方向向下（[fs_watch 设计「依赖」](fs_watch.md#依赖)） |
| `lectern-executor`（只在 macOS） | kqueue 线程经 `run_dedicated` 启动 | 后台线程只由 `executor` 管理（ADR 0024） |
| `rustix` 的 `event` feature（只在 macOS） | `fs_events` 的 kqueue | 同一个 crate 的另一个 feature，不新增 crate |
| `lectern-kernel`（dev，只在 macOS） | 经真实 `Watcher` 驱动后端的测试实现 `Sink` 要用 `BusyGuard` | kernel 层 |
| `lectern-logging`（只在 macOS） | kqueue 线程停止时的记录（「日志与诊断」）；dev 依赖开启 `test-support`，测试用 `testing::capture()` 检查这条记录 | kernel 层，只依赖 `log`（[ADR 0046](../adr/0046.md)）；本 crate 只有这一处记录，它只在 macOS 上编译 |

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

    /// 原子替换一个普通文件，比较身份之后 `renameat`。错误类型是 `ReplaceError`，见「原子替换」。
    pub fn replace_file(&self, rel: RelPath<'_>, bytes: &[u8], expect: Option<FileIdentity>)
        -> Result<FileIdentity, ReplaceError>;
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

## 日志与诊断

本节覆盖整个 crate，规则见 [ADR 0046](../adr/0046.md) 和 [logging 设计](logging.md)。

- 默认记录：`safe_read`、`dirs`、`system`、`time`、`process`、`ThreadCpuClock` 不记日志，结果和错误都返回给调用方，由调用方按各自设计文档的「日志与诊断」记录（如 `git` 读工作区出错、`file_reader` 保存失败）。唯一的运行时记录在 `fs_events`，经 `lectern-logging` 的宏写出，target 为 `platform`；订阅回调里的 panic 由 `fs_watch` 处理和记录（[fs_watch 设计](fs_watch.md)）。

  | 事件 | 级别 | `msg` | 字段 |
  |---|---|---|---|
  | kqueue 线程因 `kevent` 出错而停止（`Kqueue::fail`），这个 kqueue 上的 `Entries` 订阅不再收到事件，下一次 `Entries` 的 arm 重建 kqueue 和线程 | error | `the kqueue thread stopped on a kevent error; its watches get no more events` | `watches`（这个 kqueue 上的订阅数）、`io`（`Value::io`：`Errno` 的种类和错误码） |

  - 级别按 [ADR 0046](../adr/0046.md)「默认日志」：这些订阅之后的事件被放弃，不是降级后继续，所以是 error。
  - 这条记录只在 macOS 上编译（kqueue 只在 macOS 上），`lectern-logging` 因此只是 macOS 上的依赖。记录在 `fail` 放开状态锁之后写出，调用线程上不做 IO。
- 详细级别：不多记。kqueue 线程每次唤醒、FSEvents 的每个回调都是热路径，不加 debug、trace 记录。
- 关联 ID：本 crate 不设作用域。kqueue 线程经 `executor.run_dedicated` 启动，作用域由 `executor` 决定。
- 不记录：`safe_read` 读到的文件内容和符号链接目标；`HOME`、`XDG_DATA_HOME`、`XDG_STATE_HOME` 的值（`dirs` 只返回路径，写进日志时由 `logging` 把主目录写成 `~`）。系统版本只进日志文件头，由 `logging` 写出，本 crate 只返回字符串，字符集限于 ASCII 数字和点。`time` 不读也不返回时区名和 `TZ` 的值，只返回偏移的秒数。kqueue 的记录不写监听的目录、具名条目的名字和 `io::Error` 的消息，只写订阅数和 `Value::io`。
- 金丝雀测试：`kqueue_stop_is_logged_without_paths`（只在 macOS 上编译和运行，在 `fs_events/tests/kqueue.rs`）：在 `testing::capture()` 下，金丝雀放在监听目录的名字、具名条目的名字和它的内容里，kqueue 线程交付这个条目的修改之后，以 `EBADF` 调用 `Kqueue::fail`（`kevent` 无法按需出错，线程的 `run` 在出错时调用的就是它）；检查记录只有一条，级别为 error、target 为 `platform`、`watches` 为 1、`io` 带 `EBADF` 的错误码，线程停止、`flush` 返回错误，之后 `assert_absent`。Linux 上没有 kqueue，这条记录和测试都不编译；`dirs` 和 `system` 不经日志器输出，不需要金丝雀测试。

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
  - 偏移的口径：范围读取的偏移和字节都是磁盘上的原始字节。`git` 的 `*_byte` 按规范内容计（[git 设计「展示行」](git.md#展示行)），文件有 to-git 方向的转换（`core.autocrlf` 下的 CRLF、`working-tree-encoding`、`ident`）时两种偏移不同。行检查点按原始字节计数，键是原始字节按 git blob 格式求得的 OID（[file_reader 设计「定位」](file_reader.md#定位)、[cache 设计「命名空间」](cache.md#命名空间)），与范围读取同一口径。工作区文件按原始字节显示，规范内容的偏移怎样使用见 file_reader 设计「定位」；本模块不做转换。
  - 工作区文件的 OID：查行检查点要先知道 OID。file_reader 的设计取 `FileList` 里这个路径的 OID 作候选、用检查点里记下的内容长度与 `OpenFile::meta().size` 比较，完整读取或扫描之后再核对，不需要向 `git` 增加 stat 比较的接口（file_reader 设计「定位」）。
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

- 模块在所有平台上编译，macOS 的实现在 `#[cfg(target_os = "macos")]` 后面，`unsafe` 只有一次 FFI 调用和读取它填好的结构体。本 crate 里另外用到 `unsafe` 的是 `fs_events` 的绑定、`ThreadCpuClock`（见「线程的 CPU 时钟」）、`system::os_version` 和 `time::utc_offset`；`safe_read`（含 `replace_file`）的实现代码没有 `unsafe`。
- 测试（`process_*`）：macOS 上，分配并写满 64 MiB 后本进程的 `phys_footprint` 至少增加 60 MiB；不存在的 pid 返回错误。Linux 上返回 `Unsupported`。

## 原子替换

`file_reader`（[#10](https://github.com/SoloJiang/lectern-app/issues/10)）的前置条件 C：编辑模式的保存（⌘S）经 `EditSession::save` 调用它（[file_reader 设计「编辑」](file_reader.md#编辑)）。读者（阅读视图、`git` 的工作区比较、agent）在任何时刻看到的都是旧文件或完整的新文件，不会是写了一半的文件；磁盘上的文件在用户读入之后被别人改过时不写。

### 接口

模块 `safe_read`（`safe_read/replace.rs`），`ReplaceError` 在 crate 根重新导出。

```rust
impl SafeRoot {
    /// 用 `bytes` 原子替换 `rel` 这个普通文件。在 `rel` 所在的目录里建临时文件，写入、`fchmod` 成原文件的权限位、`fsync`，
    /// 再对 `rel` 做不跟随符号链接的 `fstatat`：`expect` 有值且当前身份与它不同时删除临时文件、返回 `Changed`；
    /// 否则 `renameat` 替换，返回新文件的身份（下次保存的 `expect`）。不创建原来不存在的文件。
    /// 阻塞，在后台线程上调用；多个线程可以同时调用同一个 `SafeRoot`。
    pub fn replace_file(&self, rel: RelPath<'_>, bytes: &[u8], expect: Option<FileIdentity>)
        -> Result<FileIdentity, ReplaceError>;
}

#[derive(Debug)]
pub enum ReplaceError {
    /// `expect` 有值，磁盘上的文件已不是这个身份：用户读入之后被写入、截短、`chmod` 或换成了别的文件。
    /// `now` 是比较时的身份。没有写入。
    Changed { now: FileIdentity },
    /// `rel` 不存在，或中间某一级不存在、不是目录（与 `SafeError::NotFound` 相同）。
    NotFound,
    /// 路径的任何一级是符号链接，包括最后一级。
    PathThroughSymlink,
    /// `rel` 不是普通文件：目录、FIFO、socket、设备文件。
    Unsupported,
    /// 其他错误，保留原始的 `io::Error`。
    Io(io::Error),
}
// 实现 `Display` 和 `std::error::Error`（`source` 是 `Io` 里的错误）。
```

身份就是「范围读取」的 `FileIdentity`（`dev`、`ino`、`size`、`mtime`、`ctime`），比较规则与「文件身份」相同：`expect` 来自 `OpenFile::identity()`（读入时）或上一次 `replace_file` 的返回值。`expect` 为 None 时不比较，只检查类型，对应 file_reader 的「强制保存」（`save_anyway`）。

### 步骤

1. 复核并拆分 `rel`（同「路径复核」）；不通过为 `Io(InvalidInput)`，不发系统调用。
2. 打开父目录：`rel` 只有一级时就是根描述符；否则用 `list` 与 `open_dir` 共用的目录打开函数（一步打开、`fstat` 确认是目录、`Walk` 后备）。之后的调用都是这个目录描述符上的 `*at` 调用，名字只有一级，不再解析路径。
3. `fstatat(dir, 名字, AT_SYMLINK_NOFOLLOW)`：确认是普通文件，记下它的权限位。在建临时文件之前检查，不存在、符号链接、其他类型都不在目录里留下任何东西。
4. 建临时文件：`openat(dir, 临时名, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC, 0600)`。名字已被占用（`EEXIST`，是文件还是符号链接都一样，不打开它）时换下一个名字，最多 16 个。
5. 写入全部字节（`EINTR` 时重试），`fchmod` 成第 3 步的权限位，`fsync`。
6. 再做一次第 3 步的 `fstatat`，类型检查相同；`expect` 有值且这次的 `identity()` 与它不同时为 `Changed { now }`。
7. `renameat(dir, 临时名, dir, 名字)`。
8. 对临时文件的描述符 `fstat`，返回它的 `identity()`，然后关闭描述符。在 `renameat` 之后取：有的文件系统（如 ext4）改名时更新被改名的 inode 的 `ctime`。

第 4 步之后任何一步出错（含 `Changed`），先 `unlinkat` 删除临时文件再返回；删除本身失败时仍返回原来的错误。系统调用数：父目录在根目录之下时 1 次打开父目录（一步打开时，加 1 次 `fstat`），2 次 `fstatat`，打开、若干次 `write`、`fchmod`、`fsync`、`renameat`、`fstat`、2 次关闭；耗时主要是 `fsync`。

### 关键约束

- 不写到根目录之外，不经过符号链接写：父目录的描述符不跟随任何一级符号链接；临时文件用 `O_CREAT | O_EXCL`，从不打开已有的条目（`O_NOFOLLOW` 再挡一次）；`renameat` 替换的是目录项本身，名字在第 6 步之后被换成符号链接时被替换的是这个链接，不写它指向的文件。
- 权限：只带过权限位 `0o777`（属主、组、其他的读写执行，POSIX 的 file permission bits），setuid、setgid、sticky 不带过。写入期间是 `0600`，内容完整之前其他用户读不到。属主和组是当前进程的，不 `fchown`（与 `git checkout` 写出的文件相同）。
- 落盘：`fsync` 在 `fchmod` 之后、`renameat` 之前，覆盖内容和权限位。崩溃之后路径上是旧文件或完整的新文件。
- 临时名：`.lectern-save-<pid>-<序号>-<纳秒>.tmp`（十六进制），点开头的隐藏文件，与原文件在同一目录（同一文件系统，`renameat` 不会是 `EXDEV`）。序号是进程内递增的计数，同一进程内不重复；pid 和时钟让它与其他进程、以前运行留下的文件不同。不含原文件名：长度固定，不受 `NAME_MAX` 影响，也不会截断 UTF-8 的名字（APFS 拒绝非法的 UTF-8）。
- 线程：同一个 `rel` 的两次保存由调用方串行（`EditSession` 同一时刻只有一个保存），本模块不加锁。

### 结果

| 结果 | 情况 | `rel` | 临时文件 |
|---|---|---|---|
| `Ok(identity)` | 替换成功 | 新内容，权限位同原文件 | 已改名为 `rel` |
| `Err(Changed { now })` | `expect` 有值，身份不同：磁盘上的文件已改动（file_reader 显示「磁盘上的文件已改动」，`SaveResult::Conflict`） | 不变 | 已删除 |
| `Err(NotFound)` | `rel` 不存在（含两次 `fstatat` 之间被删除）；中间一级不存在或不是目录；根目录已被删除 | 不创建 | 没有或已删除 |
| `Err(PathThroughSymlink)` | 任何一级是符号链接（含两次 `fstatat` 之间被换成链接） | 不变，链接不变 | 没有或已删除 |
| `Err(Unsupported)` | 目录、FIFO、socket、设备文件 | 不变 | 没有或已删除 |
| `Err(Io(InvalidInput))` | 绕过 `RelPath::new` 传入不合法的路径 | — | 没有 |
| `Err(Io(AlreadyExists))` | 16 个临时名都已被占用 | 不变 | 没有 |
| `Err(Io(_))` 其他 | 目录没有写权限（`PermissionDenied`）、只读文件系统、空间或配额不足（`StorageFull`、`QuotaExceeded`）、`EIO`、`fsync` 失败 | 不变 | 已删除 |
| `Err(Io(_))`，在第 8 步 | `renameat` 之后的 `fstat` 失败：新内容已经就位，身份未知；本地文件系统上对打开的描述符 `fstat` 不会失败 | 新内容 | 已改名 |

### 已知限制

- 第 6 步的比较与第 7 步的 `renameat` 之间有一个很短的窗口，这期间别的进程写入的内容会被覆盖（file_reader 设计「已知限制」）。
- 替换会断开硬链接，不保留扩展属性、ACL 和 setuid、setgid、sticky 位，属主和组变为当前进程的。
- 只读文件（如 `0444`）也会被替换：`renameat` 只要目录的写权限。是否允许编辑只读文件由调用方决定。
- 进程在第 4 到 7 步之间崩溃时临时文件留下，以后的保存不清理它（名字不同）。
- 不对目录 `fsync`：断电时 `renameat` 可能还没落盘，路径上仍是旧文件，不会是半个文件。macOS 的 `fsync` 不让磁盘刷写自己的缓存（没有用 `F_FULLFSYNC`），断电时刚保存的内容可能丢失。
- 挂载点与并发移动目录的限制同「已知限制」：父目录在打开之后被移到根目录之外时，写入的是移走之后的那个目录，要以用户身份在本机运行程序才能做到。

### 测试

写在 `crates/platform/src/safe_read/tests/replace.rs`（`safe_read/tests.rs` 的子模块，共用它的目录树和辅助函数），对当前平台的每个后端各运行一次（Linux：`Openat2`、`Walk`；macOS：`NofollowAny`、`NofollowAnyParent`、`Walk`）：

- `replace_file_replaces_content_and_keeps_permissions`：根目录下一级和多级路径、`0o640`、`0o751`、`0o604` 的文件；替换之后内容是新的，权限位不变，`ino` 不同，返回的身份等于 `stat` 的 `identity()`，用它作 `expect` 连续再存 3 MiB 和空内容都成功；`expect` 为 None 时照样替换；目录里没有多余的条目。
- `replace_file_changed_on_disk_is_not_replaced`：追加、大小不变的改写（先等到粗粒度时钟前进）、只 `chmod`、被另一个文件 `rename` 覆盖之后为 `Changed`，`now` 等于当前身份，磁盘上是别人写的内容，没有临时文件留下；删除之后为 `NotFound`，不重新创建。
- `replace_file_through_a_symlink_is_refused`：最后一级是符号链接（指向根目录之内、之外、悬空、`/etc/passwd`），中间一级是符号链接（之内、之外、指回根目录），`expect` 为 None 和有值都为 `PathThroughSymlink`；根目录之内和之外的文件、各个链接都不变，没有临时文件。
- `replace_file_of_other_types_and_missing_paths`：目录、FIFO、socket 为 `Unsupported`；不存在、中间一级不存在、中间一级是普通文件或 FIFO 为 `NotFound`，不创建文件。
- `replace_file_temp_name_collision`：前几个临时名已被普通文件、指向根目录之外文件的链接、悬空链接（跟随它会在根目录之外建文件）占用时跳过它们、用下一个名字，占用的条目和它们指向的文件都不变；名字全被占用时第 16 个之后为 `Io(AlreadyExists)`，原文件不变。
- `replace_file_parent_outside_the_root`：绕过 `RelPath::new` 传入 `../secret`、`a/../../secret`、绝对路径、`..`、`a//b.txt`、`a/./b.txt`，在系统调用之前为 `Io(InvalidInput)`；`open_dir` 返回的根上指向原来根目录的链接 `a/up` 为 `PathThroughSymlink`；打开之后把原路径换成指向根目录之外的链接，保存仍写进原来的目录。
- `race_replace_never_escapes`：同 `race_swap_never_escapes`，一个线程反复交换目录 `a` 与指向根目录之外的符号链接，另一个线程保存 500 次 `a/secret`；结果只有成功、`NotFound`、`PathThroughSymlink`，根目录之外的文件不变、那个目录里没有临时文件，`a` 里只剩 `secret`。
- `fd_replace_file_keeps_no_descriptor`：每种结果（成功、`Changed`、`NotFound`、`PathThroughSymlink`、`Unsupported`、临时名全被占用）反复调用之后，进程打开的描述符数不变。
- `replace_error_is_send_sync_and_displays`：`ReplaceError` 是 `Send + Sync + Error`，`Io` 的 `source` 是原来的错误。

## 线程的 CPU 时钟

`file_reader`（#10）的前置条件 G：`syntax::health` 按线程的 CPU 时间判定一次解析调用是否卡住（[ADR 0041](../adr/0041.md)「external scanner 的处理」，[file_reader 设计「本 crate 的公开项」](file_reader.md#本-crate-的公开项)的 `STUCK_GRACE`）。线程在 utility QoS 下等不到 CPU 时 CPU 时间不增长，所以并发的构建不会让它误判。

### 接口

模块 `thread_clock`（私有），`ThreadCpuClock` 在 crate 根导出。

```rust
/// 一个线程的 CPU 时钟。`current()` 在要测量的线程上调用，得到的句柄可以在任何线程上读。`Send + Sync`，不可克隆。
#[derive(Debug)]
pub struct ThreadCpuClock { /* … */ }

impl ThreadCpuClock {
    /// 调用线程的时钟。平台没有线程的 CPU 时钟，或者读不到（这里先读一次）时为 `ErrorKind::Unsupported`
    /// （系统的错误作为它的 source），调用方改用墙上时间。不阻塞。
    pub fn current() -> io::Result<ThreadCpuClock>;
    /// 这个线程到目前为止用掉的 CPU 时间（用户态加内核态）。线程已退出时返回错误。不阻塞。
    pub fn cpu_time(&self) -> io::Result<Duration>;
}
```

墙上时间的回退和 10 s 的宽限由调用方（`syntax::health`）实现，本模块只报告读不到。

### 实现

| 平台 | `current` | `cpu_time` | 精度 |
|---|---|---|---|
| Linux | `pthread_getcpuclockid(pthread_self())` 得到 `clockid_t` | `clock_gettime(clockid)` | 纳秒 |
| macOS | `pthread_mach_thread_np(pthread_self())` 得到线程的端口名（不增加引用），`mach_port_mod_refs(…, MACH_PORT_RIGHT_SEND, +1)` 为这个时钟加一个用户引用 | `thread_info(port, THREAD_BASIC_INFO)` 的 `user_time + system_time` | 微秒 |
| 其他 | `Unsupported` | — | — |

- macOS 的引用：线程退出时 libpthread 释放它的端口名，之后同一个名字可能分给别的端口，包括别的线程的端口，读到的就是别的线程的时间。时钟自己持有一个用户引用时，线程退出后这个名字变成 dead name，`thread_info` 返回错误；drop 时 `mach_port_deallocate` 只释放这一个引用。不用 `mach_thread_self()`：它每次调用都加一个引用，不释放就泄漏。
- `current()` 先读一次，读不到在创建时就报告，调用方之后只在 `cpu_time` 出错（线程已退出）时处理。
- `unsafe` 只在这几次 FFI 调用上，每处写明成立的理由。`mach_port_mod_refs`、`mach_port_deallocate` 和 `mach_task_self_` 在本模块里手写声明：libc 0.2 没有前两个，它的 `mach_task_self()` 已标为弃用。
- 开销：Linux 上线程 CPU 时钟的 `clock_gettime` 不经 vDSO，是一次系统调用，macOS 的 `thread_info` 是一次 Mach 调用，都在微秒以下到几微秒；`syntax::health` 只在登记时读，不在渲染路径上。

### 已知限制

- Linux：glibc 把线程 id 编进 `clockid_t`。线程退出之后、这个 id 又分给本进程的新线程时，读到的是新线程的时间（内核只接受同一线程组里的线程）。`syntax::health` 只在调用登记期间读，线程这时正在调用里，不受影响。
- macOS：`THREAD_BASIC_INFO` 是微秒精度；正在另一个 CPU 上运行的线程，时间只更新到内核上一次为它记账的时刻。卡死的宽限是 1 s，不受影响。
- macOS 上每个存活的时钟占线程端口的一个用户引用，同一个线程同时存活的时钟不超过 65 535 个。

### 测试

写在 `crates/platform/src/thread_clock/tests.rs`：

- `thread_cpu_busy_thread_grows`：忙循环的线程的时钟在另一个线程上读，CPU 时间 20 秒内增长到 100 ms 以上。
- `thread_cpu_blocked_thread_barely_moves`：阻塞在 channel 上的线程，主线程忙循环 300 ms 期间它的 CPU 时间增长不到 20 ms；在第三个线程上读同一个时钟。
- `thread_cpu_after_the_thread_exits_is_an_error`：线程 `join` 之后（2 秒内）`cpu_time` 返回错误。
- `thread_cpu_clocks_are_released`：在同一个线程上依次创建并 drop 70 000 个时钟都成功（macOS 上超过一个端口的 65 535 个用户引用，引用没有泄漏）。
- `thread_cpu_clock_is_send_and_sync`；其他平台上 `thread_cpu_is_unsupported_elsewhere`。

## 应用数据目录

review（[#15](https://github.com/SoloJiang/lectern-app/issues/15)）的前置条件 D 的 `platform` 部分（[review 设计「对其他 crate 的要求」](review.md#对其他-crate-的要求)、[ADR 0044](../adr/0044.md)）。应用数据目录存放不能重建的用户数据：`app` 取其中的 `review/` 作为 `ReviewDeps::data_dir`（标注日志），`usage` 的数据库以后也放在这里。可以随时删除重建的派生数据归 `cache`，不放这里。

### 接口

模块 `lectern_platform::dirs`（`dirs.rs`）。

```rust
pub mod dirs {
    /// 应用数据目录。macOS 是 `$HOME/Library/Application Support/Lectern`；其他平台（Linux，供 CI 测试）是
    /// `$XDG_DATA_HOME/lectern`，`XDG_DATA_HOME` 未设置、为空或是相对路径时是 `$HOME/.local/share/lectern`。
    /// 用到的 `HOME` 未设置、为空或是相对路径时为 None。
    /// 只读这两个环境变量：不创建目录，不检查目录是否存在，不做其他 IO，任何线程可调用。
    pub fn app_data() -> Option<PathBuf>;
}
```

### 规则

- 目录名：macOS 用 `Lectern`，与 [ADR 0018](../adr/0018.md) 的插件目录 `~/Library/Application Support/Lectern/plugins/` 是同一个目录。Linux 按 XDG 的惯例用小写的 `lectern`。
- `HOME` 只取环境变量。`std::env::home_dir` 在变量缺失时还会查询用户数据库（`getpwuid_r`），那一步是 IO，这里不做；从 Finder、Dock 或终端启动的进程都有 `HOME`。
- 相对路径视为未设置：XDG Base Directory 规范要求忽略相对路径；`HOME` 是相对路径时拼出的目录随进程的工作目录变化，同样忽略。
- 路径的字节原样保留，不要求是 UTF-8。
- 返回 None 时的处理由调用方决定，`app` 的处理随前置条件 D 的 `app` 部分规定。
- 两个平台的拼接是纯函数（`mac_app_data`、`xdg_app_data`），测试构建在所有平台上都编译这两个函数；`app_data` 读环境变量，按 `cfg(target_os = "macos")` 选其中一个。没有 `unsafe`，不新增依赖。

### 测试

写在 `crates/platform/src/dirs/tests.rs`。测试把变量的值直接传给两个纯函数，不修改进程的环境变量：edition 2024 里 `std::env::set_var` 是 `unsafe`，`cargo test` 在同一进程的多个线程上并行运行测试。

- `app_data_xdg_data_home_set_is_used`：`XDG_DATA_HOME` 是绝对路径时为它下面的 `lectern`，末尾的 `/` 不产生空的一级；这时不需要 `HOME`。
- `app_data_xdg_data_home_unset_empty_or_relative_falls_back_to_home`：`XDG_DATA_HOME` 未设置、为空、是相对路径（`data/share`、`./share`、`~/share`）时为 `$HOME/.local/share/lectern`。
- `app_data_without_a_usable_home_is_none`：`HOME` 未设置、为空、是相对路径（含 `~`）时，`XDG_DATA_HOME` 不可用的 Linux 规则和 macOS 规则都为 None。
- `app_data_macos_is_under_application_support`：`/Users/dev` 和 `/Users/dev/` 都为 `/Users/dev/Library/Application Support/Lectern`。
- `app_data_keeps_non_utf8_bytes`：`HOME`、`XDG_DATA_HOME` 里非 UTF-8 的字节原样出现在结果里。
- `app_data_does_not_create_the_directory`：`HOME` 和 `XDG_DATA_HOME` 指向临时目录下不存在的路径，三种结果都正确，临时目录之后仍是空的。
- `app_data_reads_the_process_environment`：只读当前进程的 `HOME`、`XDG_DATA_HOME`，`app_data()` 与对应平台的纯函数的结果相同。

## 日志目录与系统版本

[logging 设计「对其他 crate 的要求」](logging.md#对其他-crate-的要求)第 2 项（[#170](https://github.com/SoloJiang/lectern-app/issues/170)，[ADR 0046](../adr/0046.md)「默认日志」）。`app` 在 `main` 的第一行把 `dirs::logs()` 放进 `EarlyConfig::dir`，启动屏障放开后把 `system::os_version()` 放进 `Header::os`。日志目录的布局、权限、滚动与清理归 `logging`（[logging 设计「文件」](logging.md#文件)），本模块只给出路径。

### 接口

```rust
pub mod dirs {
    /// 日志目录。macOS 是 `$HOME/Library/Logs/Lectern`；其他平台（Linux，供 CI 测试）是
    /// `$XDG_STATE_HOME/lectern/logs`，`XDG_STATE_HOME` 未设置、为空或是相对路径时是
    /// `$HOME/.local/state/lectern/logs`。用到的 `HOME` 未设置、为空或是相对路径时为 None。
    /// 规则同 `app_data`：只读这两个环境变量，不创建目录，不做其他 IO，首帧前可以调用。
    pub fn logs() -> Option<PathBuf>;
}

pub mod system {
    /// 系统名和版本，如 "macOS 15.1"。macOS 上读 `sysctl kern.osproductversion`。
    /// 读不到、或读到的不是版本号（只允许 ASCII 数字和点、以数字开头、不超过 32 字节）时为 None；
    /// 其他平台总是 None。一次系统调用，不读文件，任何线程可调用。
    pub fn os_version() -> Option<String>;
}
```

### 规则

- 日志目录：macOS 用 `~/Library/Logs/` 下的 `Lectern`，这是 Apple 文件系统编程指南给应用日志规定的位置（ADR 0046「来源」）；Linux 按 XDG Base Directory 规范放在 state 目录（规范把日志列为 state 数据），用小写的 `lectern/logs`。日志目录与应用数据目录是两棵不相交的树，`logging` 的清理和导出只处理日志目录。
- 环境变量的处理与「应用数据目录」相同：`HOME` 只取环境变量，不查询用户数据库；相对路径视为未设置；路径的字节原样保留。拼接是纯函数（`mac_logs`、`xdg_logs`），测试构建在所有平台上都编译；`logs` 按 `cfg(target_os = "macos")` 选其中一个。返回 None 时 `logging` 只留内存缓冲（[logging 设计「状态与异常」](logging.md#状态与异常)）。
- 系统版本：`sysctlbyname("kern.osproductversion")` 读进栈上 64 字节的缓冲（大于接受的上限，过长的值整个读出后被拒绝，而不是因 `ENOMEM` 失败），去掉末尾的 NUL 后检查字符集，拼成 `macOS <版本>`。检查是纯函数（`mac_os_version`），在所有平台上编译和测试；`unsafe` 只有这一次 FFI 调用。字符集的限制让文件头里只出现版本号，读到意外的值时整个丢掉，不截取。
- 其他平台返回 None：Linux 上的构建只供 CI，`app` 只在 macOS 上构建，没有调用方需要 Linux 的版本；需要时再加。

### 测试

`logs_*` 写在 `crates/platform/src/dirs/tests.rs`，与 `app_data_*` 覆盖相同的情况，同样不修改进程的环境变量：

- `logs_xdg_state_home_set_is_used`：`XDG_STATE_HOME` 是绝对路径时为它下面的 `lectern/logs`，末尾的 `/` 不产生空的一级；这时不需要 `HOME`。
- `logs_xdg_state_home_unset_empty_or_relative_falls_back_to_home`：`XDG_STATE_HOME` 未设置、为空、是相对路径（`data/state`、`./state`、`~/state`）时为 `$HOME/.local/state/lectern/logs`。
- `logs_without_a_usable_home_is_none`：`HOME` 未设置、为空、是相对路径（含 `~`）时，`XDG_STATE_HOME` 不可用的 Linux 规则和 macOS 规则都为 None。
- `logs_macos_is_under_library_logs`：`/Users/dev` 和 `/Users/dev/` 都为 `/Users/dev/Library/Logs/Lectern`。
- `logs_keeps_non_utf8_bytes`：`HOME`、`XDG_STATE_HOME` 里非 UTF-8 的字节原样出现在结果里。
- `logs_does_not_create_the_directory`：`HOME` 和 `XDG_STATE_HOME` 指向临时目录下不存在的路径，三种结果都正确，临时目录之后仍是空的。
- `logs_reads_the_process_environment`：只读当前进程的 `HOME`、`XDG_STATE_HOME`，`logs()` 与对应平台的纯函数的结果相同。

`os_version_*` 写在 `crates/platform/src/system/tests.rs`：

- `os_version_formats_the_product_version`：`15.1`、`26.0.1`、`14`、`13.7.2`，带一个、多个或不带末尾的 NUL，都拼成 `macOS <版本>`。
- `os_version_rejects_anything_but_a_version`：空值、只有 NUL、以点开头、带字母、带空格、中间有 NUL、带换行、非 UTF-8 的字节、非 ASCII 的数字都为 None。
- `os_version_length_is_capped`：32 字节的版本号接受，33 字节的为 None。
- macOS 上 `os_version_reads_the_running_macos`：读到 `macOS ` 开头的版本，主版本不小于 11；其他平台上 `os_version_is_none_elsewhere`。

## 本地时区的偏移

[usage 设计「对其他 crate 的要求」](usage.md#对其他-crate-的要求)的 B（[#31](https://github.com/SoloJiang/lectern-app/issues/31)）。`app` 用它实现 `usage::LocalZone`（usage 的前置条件 C），`usage` 的 `window::days` 按它把时间窗口分成本地自然日（[ADR 0020](../adr/0020.md)）。这不是 macOS 专属能力：两个平台都用 POSIX 的 `localtime_r`，与 `ThreadCpuClock` 一样放在本 crate，在 Linux CI 上也能测试。

### 接口

模块 `lectern_platform::time`（`time.rs`）。

```rust
pub mod time {
    /// `unix_secs` 这一刻本地时间相对 UTC 的偏移（秒，东正西负），含夏令时。任何线程可调用；第一次调用可能读时区文件（见「规则」的 IO），不在 UI 线程上、首帧之前调用。
    /// `localtime_r` 失败（年份放不进 `struct tm` 的 `int tm_year`，如 `i64::MAX`）、
    /// 偏移超出 TZif 能表示的范围（RFC 8536：−89 999 到 93 599 秒）时为 None；macOS 和 Linux 以外的平台总是 None。
    pub fn utc_offset(unix_secs: i64) -> Option<i32>;
}
```

### 规则

- 取值：一次 `localtime_r`，返回 `struct tm` 的 `tm_gmtoff`。偏移按给定的时刻求，夏令时切换前后的时刻各得到各自的偏移，不拿「现在」的偏移去推算过去或将来。
- 时区：C 库的本地时区，即 `TZ`，`TZ` 未设置时是系统时区。本模块不读 `TZ`，不调用 `tzset`，自己不缓存任何东西：两个 C 库的 `localtime_r` 都在自己的锁下初始化时区状态（glibc 的 `tzset_internal`，macOS 的 `tzset_basic`），第一次调用时读入时区规则并留在内存里。macOS 上 `TZ` 未设置时，`localtime_r` 经系统通知（`com.apple.system.timezone`）发现系统时区的更改并重新读入（Apple Libc 的 `localtime.c`），所以用户在运行中改了时区之后，`usage` 的下一次查询就用新规则（usage 设计「取整与时间窗口」：过去的日子随之重算）。
- 线程：`localtime_r` 是可重入的版本，不用静态的结果缓冲；C 库的时区状态由它自己的锁保护，任何线程可以同时调用。会与它竞争的只有另一个线程同时修改环境变量（C 库的 `getenv("TZ")` 与 `setenv`）；edition 2024 里 `std::env::set_var` 是 `unsafe`，库代码不调用它。
- IO：第一次调用（以及 macOS 上系统时区改变之后的第一次调用）由 C 库读一次时区文件（几 KiB），之后只在内存里查找。所以调用方都不在 UI 线程上、首帧之前调用；`usage` 在后台线程上查询。
- 范围：`time_t` 在 Lectern 构建的 64 位 target 上就是 `i64`，转换不会失败（32 位 target 上超出范围的为 None）。`localtime_r` 只在年份放不进 `int tm_year` 时失败，约 ±6.8 × 10¹⁶ 秒；毫秒时间戳除以 1000 后绝对值不超过 9.3 × 10¹⁵ 秒，所以 `LocalZone::offset_at` 收到的任何时刻都有偏移。None 时的处理由 `app` 的包装决定（usage 的前置条件 C）。
- 毫秒换成秒由调用方做，用向下取整的 `div_euclid(1000)`：截断除法让 1970 年之前的时刻多出最多 999 毫秒，落在切换点之前的最后一秒里的时刻会取到切换之后的偏移。
- 偏移的检查：`tm_gmtoff` 超出 TZif 的范围时为 None（纯函数 `offset_from_gmtoff`，在所有平台上编译和测试）。真实的时区不会给出这样的值，这一步保证返回值放得进 `i32`，加到时刻上也不溢出。
- 不是整分钟的偏移原样返回：一些地方平时（LMT）不是 15 分钟的整数倍，如 Asia/Kathmandu 1920 年以前的 +05:41:16、Africa/Monrovia 到 1972 年的 −00:44:30。`usage` 按 15 分钟的 UTC 桶求和，依赖的「偏移都是 15 分钟的整数倍」只对现行规则成立；它的记录都在 2020 年之后，不受影响。
- `unsafe` 只有两处：`localtime_r` 的调用和读取它填好的 `struct tm`（初值全零，全零也是合法的 `struct tm`）。
- 开销：Linux（glibc，release，x86_64）上 `TZ=UTC` 每次约 34 ns，`TZ=America/New_York` 约 280 ns（glibc 在转换表里顺序查找）。macOS 上没有测过；`window::days` 每个本地日调用几次，30 天的窗口在微秒级。

### 已知限制

- Linux：glibc 只在第一次调用时读入系统时区，之后 `/etc/localtime` 改变不会被发现（`tzset` 也只在 `TZ` 的值变化时重读）。Linux 上的构建只供 CI，不影响用户。
- `TZ` 指向不存在的时区时，两个 C 库都按 UTC 处理，返回 0 而不是 None，分不出「时区就是 UTC」和「时区文件缺失」。系统时区的文件总是存在；只有手动设了错误的 `TZ` 才会这样。

### 测试

写在 `crates/platform/src/time/tests.rs`，不修改本进程的环境变量（edition 2024 里 `set_var` 是 `unsafe`，`cargo test` 在同一进程的多个线程上并行运行测试）。除最后一个以外都在 Linux CI 和 macOS CI 上运行：

- `utc_offset_follows_the_zone_table`：对表里的每个 `TZ` 把本测试二进制作为子进程运行一次（`current_exe` 加 `--exact`，与 `cache` 的 `crash_*` 相同），`TZ` 只设在子进程上；子进程的入口 `utc_offset_child_prints_the_rows_of_its_zone` 打印每个时刻的偏移，没有标记变量时直接返回。表：`UTC`；`Asia/Shanghai`（+08:00）；`America/New_York` 2024 年夏令时开始和结束前后各一秒，以及 1970 年之前的 −1；`Asia/Kathmandu`（+05:45，1986 年由 +05:30 改为 +05:45 的前后各一秒）；POSIX 规则串 `EST5EDT,M3.2.0,M11.1.0` 和 `<+0545>-5:45`（不依赖系统的时区文件）。期望值与 Python `zoneinfo` 对同一批时刻给出的相同。
- `utc_offset_out_of_range_is_none`：`i64::MAX`、`i64::MIN` 为 None。
- `utc_offset_covers_every_millisecond_timestamp`：从 `i64::MIN.div_euclid(1000)` 到 `i64::MAX.div_euclid(1000)` 的几个时刻在本进程的时区里都为 Some，且在 TZif 的范围之内。
- `utc_offset_from_many_threads_agrees`：8 个线程同时各调用 20 000 次，结果与单线程相同。
- `utc_offset_gmtoff_range`：`offset_from_gmtoff` 接受范围的两端和不是整分钟的偏移，拒绝范围之外和 `i64` 的两端。
- 其他平台上 `utc_offset_is_none_elsewhere`。

## 系统首选语言

[app 设计「对其他 crate 的要求」](app.md#对其他-crate-的要求)的 B（[#199](https://github.com/SoloJiang/lectern-app/issues/199)）。`app` 在首帧之前把它的结果交给 `i18n::Locale::negotiate`，选出界面语言（[i18n 设计「数据流与线程」](i18n.md#数据流与线程)里启动时用的 `preferred_languages()`）。i18n 设计里设置页用的 `system_languages()`、`language_override()` 随语言设置实现，不在这里。

### 接口

```rust
pub mod system {
    /// 本进程生效的首选语言，最优先的在前，BCP 47 标签，如 "zh-Hans-CN"。macOS 上用
    /// `CFLocaleCopyPreferredLanguages`，结果包含应用域里 `AppleLanguages` 的覆盖。
    /// 不是 ASCII 字母、数字和连字符的、以连字符开头的、超过 35 字节的标签跳过；最多 16 个。
    /// 读不到时为空；其他平台总是空。系统偏好在内存里，不读 Lectern 自己的文件，首帧前可以调用；任何线程可调用。
    pub fn preferred_languages() -> Vec<String>;
}
```

### 规则

- CoreFoundation 的五个函数（`CFLocaleCopyPreferredLanguages`、`CFArrayGetCount`、`CFArrayGetValueAtIndex`、`CFStringGetCString`、`CFRelease`）手写在 `system.rs` 的 macOS 实现里，不引入绑定 crate；CoreFoundation 已由 AppKit 链接。数组按 Copy 规则在返回前释放，数组里的字符串按 Get 规则不释放。
- 每个标签用 `CFStringGetCString` 以 UTF-8 写进栈上 37 字节的缓冲（比接受的上限多出一个字节和 NUL）：写不下时 CoreFoundation 返回失败，这个标签跳过。字符集检查是纯函数（`language_tag`），在所有平台上编译和测试；读到意外的值时整个丢掉，不截取。
- 16 个的上限：`Locale::negotiate` 只要第一个能对应上的标签，用户实际设置的首选语言很少超过几个。
- 不记日志：读不到时返回空列表，`Locale::negotiate` 退回英文。

### 测试

`preferred_languages_*` 写在 `crates/platform/src/system/tests.rs`：

- `preferred_languages_keeps_language_tags`：`en`、`en-US`、`zh-Hans-CN`、`es-419`，不带 NUL 的和 NUL 之后还有字节的都按 NUL 之前的部分接受。
- `preferred_languages_rejects_anything_but_a_tag`：空值、只有 NUL、以连字符开头、带下划线、带空格、带换行、非 ASCII 的字节都为 None。
- `preferred_languages_tag_length_is_capped`：35 字节的标签接受，36 字节的为 None。
- macOS 上 `preferred_languages_reads_the_running_macos`：结果不为空、不超过 16 个、每一项都能通过字符集检查；其他平台上 `preferred_languages_is_empty_elsewhere`。

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

原子替换与线程的 CPU 时钟（file_reader 的前置条件 C、G）一个实现 PR，写 `Refs #10`，在 file_reader 的第 1 步之前合并：

- `just test -p lectern-platform` 在 Linux CI 和 macOS CI 上都通过，其中有「原子替换」的 `replace_file_*`、`race_replace_never_escapes`、`fd_replace_file_keeps_no_descriptor`、`replace_error_is_send_sync_and_displays` 和「线程的 CPU 时钟」的 `thread_cpu_*`；原有的测试结果不变。
- `rg -l 'unsafe' crates/platform/src/safe_read` 仍只列出 `tests.rs`；`rg -n 'mach_|thread_info|pthread_' crates --glob '!crates/platform/**' -t rust` 没有结果（架构规则 6）。
- `git diff origin/main -- Cargo.lock` 为空（`libc` 已在 Cargo.lock，也已是本 crate 在 macOS 上的依赖）。
- `cargo clippy -p lectern-platform --target aarch64-apple-darwin --all-features --all-targets -- -D warnings` 在 Linux 上交叉检查通过。
- 本文的状态行、「实际验证」「与设计的差异」，file_reader 设计「验收」的前置条件 C、G 标为已完成。

应用数据目录（review 前置条件 D 的 `platform` 部分）一个实现 PR，写 `Refs #15`，在 review 第 4 步之前合并：

- `just test -p lectern-platform` 在 Linux CI 和 macOS CI 上都通过，其中有「应用数据目录」的 `app_data_*`；原有的测试结果不变。
- `git diff origin/main -- Cargo.lock` 为空；`rg -n 'set_var|remove_var' crates/platform/src` 没有结果。
- `cargo clippy -p lectern-platform --target aarch64-apple-darwin --all-features --all-targets -- -D warnings` 在 Linux 上交叉检查通过。
- 本文的状态行、「实际验证」，review 设计的状态行写明前置条件 D 的 `platform` 部分已实现。

日志目录与系统版本（logging 第 2 步）一个实现 PR，写 `Refs #170`，在 logging 第 3 步（`app`）之前合并：

- `just test -p lectern-platform` 在 Linux CI 和 macOS CI 上都通过，其中有「日志目录与系统版本」的 `logs_*`、`os_version_*`；原有的测试结果不变。
- `git diff origin/main -- Cargo.lock` 为空（`sysctlbyname` 来自已有的 `libc`）；`rg -n 'set_var|remove_var' crates/platform/src` 没有结果；`rg -n 'sysctl' crates --glob '!crates/platform/**' -t rust` 没有结果（架构规则 6）。
- `cargo clippy -p lectern-platform --target aarch64-apple-darwin --all-features --all-targets -- -D warnings` 在 Linux 上交叉检查通过。
- 本文的状态行、「实际验证」，logging 设计的状态行写明第 2 步已实现。

系统首选语言（app 的前置条件 B）一个实现 PR，写 `Refs #199`，在 app 第 1 步之前合并：

- `just test -p lectern-platform` 在 Linux CI 和 macOS CI 上都通过，其中有「系统首选语言」的 `preferred_languages_*`；原有的测试结果不变。
- `git diff origin/main -- Cargo.lock` 为空；`rg -n 'CFLocale' crates --glob '!crates/platform/**' -t rust` 没有结果（架构规则 6）。
- 本文的状态行和「实际验证」写明已实现。

本地时区的偏移（usage 的前置条件 B）一个实现 PR，写 `Refs #31`，在 usage 第 1 步之前合并：

- `just test -p lectern-platform` 在 Linux CI 和 macOS CI 上都通过，其中有「本地时区的偏移」的 `utc_offset_*`；原有的测试结果不变。
- `git diff origin/main -- Cargo.lock` 为空（`localtime_r` 来自已有的 `libc`）；`rg -n 'set_var|remove_var' crates/platform/src` 没有结果；`rg -n 'localtime|tm_gmtoff' crates --glob '!crates/platform/**' -t rust` 没有结果。
- `cargo clippy -p lectern-platform --target aarch64-apple-darwin --all-features --all-targets -- -D warnings` 在 Linux 上交叉检查通过。
- 本文的状态行、「实际验证」，usage 设计的状态行写明前置条件 B 已实现。

kqueue 线程的日志接入（[logging 设计「对其他 crate 的要求」](logging.md#对其他-crate-的要求)第 9 项的 `platform` 部分）一个实现 PR，写 `Refs #170`：

- `rg -n 'eprintln!|eprint!|println!|util::log' crates/platform/src --glob '!**/tests*'` 没有结果（测试里打印测量值的 `eprintln!` 和 `time` 子进程的 `println!` 不算）。
- `cargo clippy -p lectern-platform --target aarch64-apple-darwin --all-features --all-targets -- -D warnings` 在 Linux 上交叉检查通过；`kqueue_stop_is_logged_without_paths` 和原有的 `kqueue_*` 在 macOS CI 上通过。
- Cargo.lock 只在 `lectern-platform` 的依赖里多了 `lectern-logging`，没有新的外部 crate。
- 本文的状态行、「实际验证」，logging 设计的状态行写明第 9 项的 `platform` 部分已实现。

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
- 范围读取（[#103](https://github.com/SoloJiang/lectern-app/issues/103)，实现 [#126](https://github.com/SoloJiang/lectern-app/pull/126)）在 Linux（x86_64，内核 6.18，`openat2` 可用，4 个 vCPU 的虚拟机）上：
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
- 原子替换与线程的 CPU 时钟（file_reader 的前置条件 C、G）在 Linux（x86_64，内核 6.18，`openat2` 可用，`/tmp` 是 ext4）上：
  - `just test -p lectern-platform` 86 个测试通过，其中新增 14 个：「原子替换」的 9 个（`replace_file_*` 6 个、`race_replace_never_escapes`、`fd_replace_file_keeps_no_descriptor`、`replace_error_is_send_sync_and_displays`），对 `Openat2` 和 `Walk` 各运行一次；「线程的 CPU 时钟」的 5 个（`thread_cpu_*`，`thread_cpu_is_unsupported_elsewhere` 只在其他平台上编译）。原有的 72 个结果不变；用 `cargo test` 在同一个进程里运行，86 个也都通过。
  - 新增的测试和 `race_*`、`fd_*` 连续运行 30 次都通过。`race_replace_never_escapes` 每次运行中三种结果都出现，一次运行的分布：`Openat2` 89 次保存成功、398 次 `NotFound`、13 次 `PathThroughSymlink`；`Walk` 依次为 78、337、85 次。`thread_cpu_busy_thread_grows` 在约 100 ms 的墙上时间里读到 101 ms 的 CPU 时间。
  - `just check`、`just test`（整个工作区 1563 个测试通过，跳过 7 个）、`just doctest`、`just deny` 通过。`cargo clippy -p lectern-platform --target aarch64-apple-darwin --all-features --all-targets -- -D warnings` 在 Linux 上交叉检查通过：macOS 的 `thread_info`、`pthread_mach_thread_np`、手写的 `mach_port_mod_refs`、`mach_port_deallocate`、`mach_task_self_` 通过了类型检查和 clippy，没有运行；macOS 上的 `thread_cpu_*` 和三个后端上的 `replace_file_*` 由 macOS CI 运行。
  - 验收的检查：`git diff origin/main -- Cargo.lock` 为空；`rg -l 'unsafe' crates/platform/src/safe_read` 只列出 `tests.rs`；`rg -n 'mach_|thread_info|pthread_' crates --glob '!crates/platform/**' -t rust` 没有结果。
  - 测试环境以 root 运行，目录没有写权限时的 `Io(PermissionDenied)` 没有测试（root 不受权限位限制）。
- 应用数据目录（review 前置条件 D 的 `platform` 部分，[#166](https://github.com/SoloJiang/lectern-app/pull/166)）在 Linux（x86_64 容器）上：
  - `just test -p lectern-platform` 93 个测试通过，其中新增 7 个 `app_data_*`，macOS 规则的 `app_data_macos_is_under_application_support` 等也在 Linux 上运行；原有的 86 个结果不变。用 `cargo test` 在同一个进程里运行，7 个也都通过（测试不修改环境变量）。
  - `just check`、`just doctest`、`just deny` 通过。`cargo clippy -p lectern-platform --target aarch64-apple-darwin --all-features --all-targets -- -D warnings` 在 Linux 上交叉检查通过：macOS 上 `app_data` 选 `mac_app_data`、非测试构建不编译 `xdg_app_data`，没有未使用的函数；macOS 上的 `app_data_reads_the_process_environment` 由 macOS CI 运行。
  - 验收的检查：`git diff origin/main -- Cargo.lock` 为空；`rg -n 'set_var|remove_var' crates/platform/src` 没有结果。
- 日志目录与系统版本（logging 第 2 步，[#173](https://github.com/SoloJiang/lectern-app/pull/173)）在 Linux（x86_64 容器）上：
  - `just test -p lectern-platform` 104 个测试通过，其中新增 11 个：7 个 `logs_*`（macOS 规则的 `logs_macos_is_under_library_logs` 等也在 Linux 上运行）和 4 个 `os_version_*`（`os_version_is_none_elsewhere` 只在其他平台上编译）；原有的 93 个结果不变。
  - `just check`、`just test`（整个工作区 2013 个测试通过，跳过 7 个）、`just doctest`、`just deny` 通过。`cargo check --target aarch64-apple-darwin -p lectern-platform` 和 `cargo clippy -p lectern-platform --target aarch64-apple-darwin --all-features --all-targets -- -D warnings` 在 Linux 上交叉检查通过：macOS 上 `logs` 选 `mac_logs`、非测试构建不编译 `xdg_logs`，`sysctlbyname` 的调用通过了类型检查和 clippy（`undocumented_unsafe_blocks`）。macOS 上 `sysctl` 的读取和 `os_version_reads_the_running_macos` 没有运行过，由 macOS CI 运行。
  - 验收的检查：`git diff origin/main -- Cargo.lock` 为空；`rg -n 'set_var|remove_var' crates/platform/src` 没有结果；`rg -n 'sysctl' crates --glob '!crates/platform/**' -t rust` 没有结果。
- 本地时区的偏移（usage 的前置条件 B，[#176](https://github.com/SoloJiang/lectern-app/pull/176)）在 Linux（x86_64 容器，glibc，系统 tzdata 在 `/usr/share/zoneinfo`）上：
  - `just test -p lectern-platform` 110 个测试通过，其中新增 6 个 `utc_offset_*`（`utc_offset_is_none_elsewhere` 只在其他平台上编译）；原有的 104 个结果不变。用 `cargo test` 在同一个进程里运行，110 个也都通过（测试不修改环境变量，子进程的入口没有标记变量时直接返回）。
  - `utc_offset_follows_the_zone_table` 的 6 个 `TZ`（4 个 IANA 时区、2 个 POSIX 规则串）在子进程里的结果都与表相同，表的期望值先用 Python `zoneinfo` 核对过；`i64::MAX`、`i64::MIN` 为 None，`i64::MIN.div_euclid(1000)` 到 `i64::MAX.div_euclid(1000)` 的时刻都为 Some。
  - 开销（release，临时的计时循环，没有提交）：`TZ=UTC` 每次约 34 ns，`TZ=America/New_York` 约 280 ns。
  - `just check`、`just test`（整个工作区 2050 个测试通过，跳过 7 个）、`just doctest`、`just deny` 通过。`cargo clippy -p lectern-platform --target aarch64-apple-darwin --all-features --all-targets -- -D warnings` 在 Linux 上交叉检查通过：macOS 上的 `localtime_r` 调用通过了类型检查和 clippy，没有运行；macOS 上的 `utc_offset_*`（含 Apple Libc 对超出范围的年份返回 null）由 macOS CI 运行。运行中改系统时区之后 macOS 重新读入规则，是按 Apple Libc 的源码写的，没有测试。
  - 验收的检查：`git diff origin/main -- Cargo.lock` 为空；`rg -n 'set_var|remove_var' crates/platform/src` 没有结果；`rg -n 'localtime|tm_gmtoff' crates --glob '!crates/platform/**' -t rust` 没有结果。
- kqueue 线程的日志接入（logging 设计第 9 项的 `platform` 部分，[#192](https://github.com/SoloJiang/lectern-app/pull/192)）在 Linux（x86_64 容器）上：
  - 改动的代码（`Kqueue::fail`）和新测试 `kqueue_stop_is_logged_without_paths` 只在 macOS 上编译，Linux 上没有编译也没有运行。`cargo check -p lectern-platform --target aarch64-apple-darwin` 和 `cargo clippy -p lectern-platform --target aarch64-apple-darwin --all-features --all-targets -- -D warnings` 在 Linux 上交叉检查通过：`error!` 的展开、`Value::io`、macOS 上的 `lectern-logging` 依赖和测试里的 `testing::capture()` 通过了类型检查和 clippy，没有链接、没有运行；这个测试和原有的 `kqueue_*` 由 macOS CI 运行。
  - `just test -p lectern-platform` 110 个测试通过，与改动前相同（Linux 上的代码没有改）。
  - `just check`、`just doctest`、`just deny` 通过。`just test`（整个工作区 2251 个测试，跳过 7 个）里 2250 个通过，`lectern-syntax` 的计时测试 `batch_order_window_tasks_take_turns_with_batches` 在四个 agent 同时构建的负载下超时一次（第 4 步 168 ms），单独重跑 `batch_order` 的 5 个测试都通过；它不经过本 crate 改动的代码。
  - 验收的检查：`rg -n 'eprintln!|eprint!|println!|util::log' crates/platform/src --glob '!**/tests*'` 没有结果；Cargo.lock 只在 `lectern-platform` 的依赖里多了 `lectern-logging`。

- 系统首选语言（app 的前置条件 B）在 macOS（Apple 芯片，macOS 27.0）上：
  - `just test -p lectern-platform` 137 个测试通过，其中新增 4 个 `preferred_languages_*`（`preferred_languages_reads_the_running_macos` 读到本机的首选语言列表，每一项通过字符集检查）；原有的测试结果不变。
  - `cargo clippy -p lectern-platform --all-targets --all-features -- -D warnings` 通过。验收的检查：`git diff origin/main -- Cargo.lock` 为空；`rg -n 'CFLocale' crates --glob '!crates/platform/**' -t rust` 没有结果。
  - 没有在 Linux 上运行：`preferred_languages_is_empty_elsewhere` 和纯函数的测试由 Linux CI 运行。

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
- 范围读取（[#103](https://github.com/SoloJiang/lectern-app/issues/103)，实现 [#126](https://github.com/SoloJiang/lectern-app/pull/126)）的接口和行为与「范围读取」一致，没有差异。实现上需要说明的有三处，都不改变接口：
  - `read` 与 `open_file` 共用一个内部的打开函数（一步打开、对描述符 `fstat` 分类、出错时交给 `Walk`），`Walk` 里读最后一级的那一步随之改为只打开并分类、返回描述符或 `Symlink`、`Unsupported`，由 `read` 读内容、由 `open_file` 转交给 `OpenFile`；`read` 的行为不变，原有测试的结果不变。`read_all` 与 `read` 共用「读取上限」的函数，前者用 `pread` 从偏移 0 读，后者用 `read`。
  - 「读取的规则」第 3 步的按需扩容：一段的缓冲读满而这段还没读完时（读到了打开时的 `size`），先用栈上 4 KiB 的缓冲 `pread` 一次，返回 0 就是 EOF，不分配；读到数据（文件变长了）才按倍增扩容，不超过段长。所以分配总是不超过 `min(段长, 打开时的 size − 起点)`，除非文件确实变长。终点到达 EOF 的段因此多一次返回 0 的 `pread`，与「系统调用数」里「短读时多几次」一致。
  - 一段里最后取字节的那个区间覆盖了这段读到的全部字节时，这段的缓冲直接移给它的 `pieces[i]`，不复制；其余区间从段的缓冲复制。首屏在文件中部时两段不合并，两个区间都不复制。
- 范围读取的测试里对 `clippy::single_range_in_vec_init` 加了 `allow`（附理由）：`read_ranges` 的参数是区间的切片，`&[0..100]` 是正常的单区间调用，这个 lint 把它当作误写的 `Vec`。bench 里用 `std::slice::from_ref` 传单个区间。
- 原子替换与线程的 CPU 时钟的接口与 [file_reader 设计「对其他 crate 的要求」](file_reader.md#对其他-crate-的要求)一致，这里补上了那里没有写的约束（「原子替换」「线程的 CPU 时钟」），有四处与那里的文字不同：
  - `fchmod` 在 `fsync` 之前（那里的顺序是写入、`fsync`、`fchmod`）：这样 `fsync` 同时落盘内容和权限位，崩溃之后不会出现内容是新的、权限还是 `0600` 的文件。
  - 建临时文件之前先对 `rel` 做一次不跟随符号链接的 `fstatat`（那里只写了替换之前的那一次）：`rel` 不存在、是符号链接或不是普通文件时不在目录里建任何东西，要带过的权限位也从这次得到。替换之前的那一次照常做，身份在那里比较。
  - 「原文件的权限位」定为 `0o777`，setuid、setgid、sticky 不带过；临时名被占用时换名重试，最多 16 个，之后为 `Io(AlreadyExists)`。那里没有写。
  - `ThreadCpuClock::current()` 创建时先读一次，读不到在这里返回 `Unsupported`；macOS 的时钟为线程端口多持有一个用户引用，线程退出后 `cpu_time` 因此可靠地返回错误（「线程的 CPU 时钟」的「实现」）。阻塞线程的测试用 channel 的 `recv`，不用条件变量，两者都让线程睡在内核里。
- 增加了那里没有列出的测试：`replace_file_temp_name_collision`、`replace_file_parent_outside_the_root`、`fd_replace_file_keeps_no_descriptor`、`replace_error_is_send_sync_and_displays`、`thread_cpu_after_the_thread_exits_is_an_error`、`thread_cpu_clocks_are_released`。`libc` 原来只在 macOS 上依赖，现在 Linux 也依赖（`pthread_getcpuclockid`、`clock_gettime`），Cargo.lock 不变；「依赖」已改。
- 日志目录与系统版本按 [logging 设计「对其他 crate 的要求」](logging.md#对其他-crate-的要求)第 2 项实现，那里没有写的约束补在「日志目录与系统版本」：系统版本的接口（`system::os_version() -> Option<String>`，值为 `macOS <版本>`，与 logging 设计 `Header::os` 的示例一致）、字符集与长度的检查、其他平台返回 None。第 2 项里的写盘线程 utility QoS 不在这个 PR：logging 设计「验收」的第 2 步只要求 `dirs::logs()` 和系统版本，而 QoS 要沿用的 executor 线程启动钩子在本 crate 还没有对应的接口（「范围」里「本文不覆盖」的线程 QoS），随它的设计一起做；`Config::on_writer_start` 是 `Option`，接口不受影响。
- 本地时区的偏移按 [usage 设计「对其他 crate 的要求」](usage.md#对其他-crate-的要求)的 B 实现，接口没有改，那里没有写的约束补在「本地时区的偏移」。与那里的文字不同的有三处：
  - B 写的「不做 IO」不成立：第一次调用（以及 macOS 上系统时区改变之后的第一次调用）由 C 库读时区文件。usage 设计 B 与 `LocalZone::offset_at` 的注释同时改为不在 UI 线程上、首帧之前调用。
  - None 多了一种情况：`tm_gmtoff` 超出 TZif 的范围。B 写的「超出 `time_t` 的范围」改为「年份放不进 `tm_year`」：64 位的 `time_t` 能放下任何 `i64`，`localtime_r` 失败只因为年份；usage 设计 B 的注释同时改正。
  - 测试在 B 的表之外加了 Kathmandu 1986 年的切换、两个 POSIX 规则串（不依赖系统的时区文件）、`utc_offset_out_of_range_is_none`、`utc_offset_covers_every_millisecond_timestamp`、`utc_offset_from_many_threads_agrees` 和 `utc_offset_gmtoff_range`。子进程的入口不标 `#[ignore]`，没有标记变量时直接返回，与 `cache` 的 `crash_*` 相同，`just test` 不因此多出跳过的测试。
- kqueue 线程的日志接入（logging 设计第 9 项的 `platform` 部分）按原来「日志与诊断」写的做：error、target `platform`、`Value::io`，删掉 `eprintln!`（它写的是 `io::Error` 的 `Display` 文字，现在不再写系统给的消息）。与那里的文字不同的有两处：
  - 多了字段 `watches`（这个 kqueue 上的订阅数）：原来只写了错误。计数说明有多少订阅从此收不到事件，不含路径和名字。
  - 金丝雀测试不让 `kevent` 真的出错（测试里造不出来），而是在线程运行时直接调用 `run` 出错时调用的 `Kqueue::fail`，为此 `fail` 由私有改为 `pub(super)`。
