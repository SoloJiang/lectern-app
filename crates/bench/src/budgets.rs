//! The budget table: one entry per benchmark ID, with its checks, its tier
//! and where the number comes from (docs/design/bench.md「预算表」).
//!
//! The source is the authority: when it changes, this table and the
//! design's table change in the same PR. A `Top` number changes only with an
//! ADR; a `Share` changes with the crate design that grants it.

use std::time::Duration;

/// One budget.
#[derive(Debug)]
pub struct Budget {
    /// The same as `Measured::id`. A segment written `*` matches any one
    /// segment (a plugin or language server ID); a more specific entry wins.
    pub id: &'static str,
    /// All of them must hold; empty means record only.
    pub checks: &'static [Check],
    pub tier: Tier,
    /// Repository path plus anchor, e.g. "docs/adr/0017.md#决策".
    pub source: &'static str,
    pub state: State,
    /// The benchmark target producing this budget. Each budget belongs to
    /// exactly one target.
    pub target: Target,
}

/// Where a budget's result comes from. `Package` is not a benchmark target:
/// the packaging job produces `report-package.json`, `just bench` does not.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Target {
    Time,
    Memory,
    Process,
    Package,
}

impl Target {
    pub const ALL: [Target; 4] = [
        Target::Time,
        Target::Memory,
        Target::Process,
        Target::Package,
    ];

    /// The name in `report-<name>.json`.
    pub fn name(self) -> &'static str {
        match self {
            Target::Time => "time",
            Target::Memory => "memory",
            Target::Process => "process",
            Target::Package => "package",
        }
    }

    pub fn from_name(name: &str) -> Option<Target> {
        Target::ALL.into_iter().find(|t| t.name() == name)
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Check {
    pub stat: Stat,
    pub limit: Limit,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Stat {
    Median,
    P95,
    Max,
    /// The number of samples above this duration, with `Limit::CountAtMost`
    /// (cold start: at most 1 of 20 above 20 ms).
    CountAbove(Duration),
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Limit {
    /// `<`
    TimeBelow(Duration),
    /// `≤`
    TimeAtMost(Duration),
    /// Bytes per second, `≥`.
    ThroughputAtLeast(u64),
    /// `<`
    BytesBelow(u64),
    /// `≤`
    BytesAtMost(u64),
    /// `≤`, only for `Value::Ratio`.
    RatioAtMost(f64),
    CountAtMost(u32),
    /// Only for the `intervals` of `Value::Frames`: an interval of at most
    /// this many refresh periods.
    RefreshMultipleAtMost(f64),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tier {
    /// A number from tech-research.md「性能预算」or an ADR. The source must be
    /// tech-research.md or a file under docs/adr/; changing it takes an ADR.
    Top,
    /// A share granted by a crate design's「性能」. Changed together with
    /// that design, which shows the shares under the same top-level budget
    /// still add up.
    Share,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    Active,
    /// The subject is not implemented yet. Reported as `planned`; no
    /// benchmark has to produce it. The argument names the crate, step or
    /// issue that makes it `Active`.
    Planned(&'static str),
}

/// MB in this table are 2^20 bytes, as Activity Monitor and `du -k` count.
pub const MB: u64 = 1 << 20;

const fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

const fn us(n: u64) -> Duration {
    Duration::from_micros(n)
}

const fn median(limit: Limit) -> Check {
    Check {
        stat: Stat::Median,
        limit,
    }
}

const fn max(limit: Limit) -> Check {
    Check {
        stat: Stat::Max,
        limit,
    }
}

const RECORD: &[Check] = &[];

const TECH: &str = "docs/design/tech-research.md#性能预算";
const ADR_0017_DECISION: &str = "docs/adr/0017.md#决策";
const ADR_0017_IMPACT: &str = "docs/adr/0017.md#影响";
const ADR_0018_DECISION: &str = "docs/adr/0018.md#决策";
const ADR_0018_IMPACT: &str = "docs/adr/0018.md#影响";
const ADR_0029_DECISION: &str = "docs/adr/0029.md#决策";
const ADR_0035_DECISION: &str = "docs/adr/0035.md#决策";
const AGENTS: &str = "docs/design/agents.md#性能";
const GIT: &str = "docs/design/git.md#性能";
const CACHE: &str = "docs/design/cache.md#性能";
const KERNEL: &str = "docs/design/kernel.md#性能";
const PROJECT: &str = "docs/design/project.md#性能";
const PLATFORM: &str = "docs/design/platform.md#性能";
const EXECUTOR: &str = "docs/design/executor.md#性能";
const I18N: &str = "docs/design/i18n.md#性能";
const FS_WATCH: &str = "docs/design/fs_watch.md#性能";
const CODE_INTEL: &str = "docs/design/code_intel.md#性能";

/// Every frame's work (`completed - requested`) at most 8.3 ms, and every
/// interval between two frames' `started` at most 1.5 refresh periods.
const FRAMES: &[Check] = &[
    max(Limit::TimeAtMost(us(8300))),
    max(Limit::RefreshMultipleAtMost(1.5)),
];

const fn top(
    id: &'static str,
    checks: &'static [Check],
    source: &'static str,
    state: State,
    target: Target,
) -> Budget {
    Budget {
        id,
        checks,
        tier: Tier::Top,
        source,
        state,
        target,
    }
}

const fn share(
    id: &'static str,
    checks: &'static [Check],
    source: &'static str,
    state: State,
    target: Target,
) -> Budget {
    Budget {
        id,
        checks,
        tier: Tier::Share,
        source,
        state,
        target,
    }
}

const ACTIVE: State = State::Active;
const STEP_4: State = State::Planned("bench 第 4 步");
const STEP_5: State = State::Planned("bench 第 5 步");
const MACOS_100K: State = State::Planned("git：macOS 上 10 万文件的比较与过滤器重建达标");

use Target::{Memory, Package, Process, Time};

/// The budget table (docs/design/bench.md「预算表」).
pub static BUDGETS: &[Budget] = &[
    // ---- top level (Tier::Top) -------------------------------------------
    top(
        "app/cold_start",
        &[
            Check {
                stat: Stat::CountAbove(ms(20)),
                limit: Limit::CountAtMost(1),
            },
            max(Limit::TimeAtMost(ms(30))),
        ],
        ADR_0017_DECISION,
        STEP_5,
        Process,
    ),
    top(
        "app/cold_start_purged",
        RECORD,
        ADR_0017_IMPACT,
        STEP_5,
        Process,
    ),
    top(
        "app/idle_empty",
        &[median(Limit::BytesBelow(60 * MB))],
        ADR_0017_DECISION,
        STEP_5,
        Process,
    ),
    top(
        "app/idle_monorepo",
        &[median(Limit::BytesBelow(100 * MB))],
        ADR_0017_DECISION,
        State::Planned("project_home"),
        Process,
    ),
    // After all pausable background work (ADR 0029) has finished; the
    // `app/idle_monorepo` sample is taken with it paused.
    top(
        "app/idle_monorepo_indexed",
        &[median(Limit::BytesBelow(100 * MB))],
        ADR_0029_DECISION,
        State::Planned("#17"),
        Process,
    ),
    // The same two on the large repository (100 000 files, 17 worktrees),
    // ADR 0035; `app/idle_100k_indexed` is sampled in the launches of
    // `app/index_100k`, once indexing is done and the app is at rest.
    top(
        "app/idle_100k",
        &[median(Limit::BytesBelow(120 * MB))],
        ADR_0035_DECISION,
        State::Planned("project_home"),
        Process,
    ),
    top(
        "app/idle_100k_indexed",
        &[median(Limit::BytesBelow(120 * MB))],
        ADR_0035_DECISION,
        State::Planned("#17"),
        Process,
    ),
    top(
        "app/open_project",
        &[median(Limit::TimeBelow(ms(100)))],
        TECH,
        State::Planned("project_home"),
        Process,
    ),
    top(
        "app/open_file",
        &[median(Limit::TimeBelow(ms(16)))],
        TECH,
        State::Planned("file_reader"),
        Process,
    ),
    top(
        "app/open_diff",
        &[median(Limit::TimeBelow(ms(50)))],
        TECH,
        State::Planned("review_view"),
        Process,
    ),
    top(
        "app/switch_worktree",
        &[median(Limit::TimeBelow(ms(100)))],
        TECH,
        State::Planned("worktrees"),
        Process,
    ),
    // ---- code navigation (ADR 0030) ----
    top(
        "app/palette_open",
        &[median(Limit::TimeBelow(ms(16)))],
        TECH,
        State::Planned("#102"),
        Process,
    ),
    top(
        // The ≤ 16 ms tier covers up to 200k paths: measured at its upper bound.
        "app/palette_key_200k",
        &[median(Limit::TimeAtMost(ms(16)))],
        TECH,
        State::Planned("#102"),
        Process,
    ),
    top(
        "app/palette_key_1m",
        &[median(Limit::TimeAtMost(ms(50)))],
        TECH,
        State::Planned("#102"),
        Process,
    ),
    top(
        "app/files_open",
        &[median(Limit::TimeBelow(ms(16)))],
        TECH,
        State::Planned("#10"),
        Process,
    ),
    top(
        "app/files_expand",
        &[median(Limit::TimeBelow(ms(16)))],
        TECH,
        State::Planned("#10"),
        Process,
    ),
    top(
        "app/nav_back",
        &[median(Limit::TimeBelow(ms(16)))],
        TECH,
        State::Planned("#10"),
        Process,
    ),
    top(
        "app/goto_candidates",
        &[median(Limit::TimeAtMost(ms(50)))],
        TECH,
        State::Planned("#17"),
        Process,
    ),
    top(
        "app/goto_single",
        &[median(Limit::TimeAtMost(ms(100)))],
        TECH,
        State::Planned("#17"),
        Process,
    ),
    top(
        "app/goto_unindexed",
        &[median(Limit::TimeAtMost(ms(300)))],
        TECH,
        State::Planned("#17"),
        Process,
    ),
    // Until the bounded search ends (all candidates searched or the 1 s
    // limit hit) and its last batch is drawn: 1 s plus 0.1 s for
    // cancellation and drawing, on a fixture too large to finish in 1 s.
    top(
        "app/goto_unindexed_done",
        &[max(Limit::TimeAtMost(ms(1100)))],
        TECH,
        State::Planned("#17"),
        Process,
    ),
    top(
        "app/references_first",
        &[median(Limit::TimeAtMost(ms(200)))],
        TECH,
        State::Planned("#20"),
        Process,
    ),
    top(
        "app/references_100k",
        &[median(Limit::TimeAtMost(ms(2000)))],
        TECH,
        State::Planned("#20"),
        Process,
    ),
    // From opening the project until the tier is fully indexed, on power.
    top(
        "app/index_review_set",
        &[median(Limit::TimeAtMost(ms(2000)))],
        TECH,
        State::Planned("#17"),
        Process,
    ),
    top(
        "app/index_subproject",
        &[median(Limit::TimeAtMost(ms(10_000)))],
        TECH,
        State::Planned("#17"),
        Process,
    ),
    top(
        "app/index_100k",
        &[median(Limit::TimeAtMost(ms(120_000)))],
        TECH,
        State::Planned("#17"),
        Process,
    ),
    // ---- end code navigation ----
    top(
        "app/scroll_frames",
        FRAMES,
        TECH,
        State::Planned("file_reader"),
        Process,
    ),
    top(
        "app/input_frames",
        FRAMES,
        TECH,
        State::Planned("terminal_view"),
        Process,
    ),
    top(
        "app/animation_frames",
        FRAMES,
        "docs/adr/0015.md",
        State::Planned("ui"),
        Process,
    ),
    // The 50 MB target is only shown in the report.
    top(
        "app/bundle_size",
        &[max(Limit::BytesAtMost(80 * MB))],
        ADR_0018_DECISION,
        State::Planned("#38"),
        Package,
    ),
    top(
        "app/dmg_size",
        &[max(Limit::BytesAtMost(40 * MB))],
        ADR_0018_DECISION,
        State::Planned("#38"),
        Package,
    ),
    top(
        "plugin/process_total",
        &[median(Limit::BytesAtMost(50 * MB))],
        ADR_0018_IMPACT,
        State::Planned("kernel 的进程插件宿主"),
        Process,
    ),
    top(
        "lsp/*/resident",
        RECORD,
        ADR_0018_IMPACT,
        State::Planned("code_intel"),
        Process,
    ),
    top(
        "lsp/total",
        RECORD,
        ADR_0018_IMPACT,
        State::Planned("code_intel"),
        Process,
    ),
    top(
        "app/unclassified_children",
        RECORD,
        ADR_0018_IMPACT,
        STEP_5,
        Process,
    ),
    top(
        "baseline/empty_window_cold_start",
        RECORD,
        ADR_0017_IMPACT,
        STEP_4,
        Process,
    ),
    top(
        "baseline/empty_window_idle",
        RECORD,
        ADR_0017_IMPACT,
        STEP_4,
        Process,
    ),
    // ---- shares (Tier::Share) --------------------------------------------
    share(
        "plugin/*/activate",
        &[median(Limit::TimeBelow(ms(1)))],
        KERNEL,
        STEP_5,
        Process,
    ),
    share(
        "plugin/after_first_frame",
        &[median(Limit::TimeBelow(ms(5)))],
        KERNEL,
        STEP_5,
        Process,
    ),
    share("plugin/*/resident", RECORD, KERNEL, STEP_5, Process),
    // `phys_footprint` of two launches (kernel「性能」), with the app's
    // measurement build; until then `project/resident_*_bytes` measures the
    // same in live bytes, in process.
    share(
        "plugin/project/resident_cloudprint",
        &[median(Limit::BytesBelow(2 * MB))],
        PROJECT,
        STEP_5,
        Process,
    ),
    // 12 MB before the file list carried blob OIDs; 100k × 32 bytes (a
    // SHA-256 repository) adds 3.05 MiB, rounded up to 16 MB.
    share(
        "plugin/project/resident_100k",
        &[median(Limit::BytesBelow(16 * MB))],
        PROJECT,
        STEP_5,
        Process,
    ),
    share(
        "agents/parse_throughput",
        &[median(Limit::ThroughputAtLeast(200 * MB))],
        AGENTS,
        ACTIVE,
        Time,
    ),
    share(
        "agents/append_4k",
        &[median(Limit::TimeBelow(ms(1)))],
        AGENTS,
        ACTIVE,
        Time,
    ),
    share(
        "agents/discover_1000",
        &[median(Limit::TimeBelow(ms(500)))],
        AGENTS,
        ACTIVE,
        Time,
    ),
    share(
        "agents/turns_cached_50mb",
        &[median(Limit::TimeBelow(ms(10)))],
        AGENTS,
        ACTIVE,
        Time,
    ),
    share(
        "agents/zst_throughput",
        &[median(Limit::ThroughputAtLeast(100 * MB))],
        AGENTS,
        ACTIVE,
        Time,
    ),
    share(
        "agents/zst_load_decoded",
        &[median(Limit::TimeBelow(ms(1)))],
        AGENTS,
        ACTIVE,
        Time,
    ),
    share(
        "git/open",
        &[median(Limit::TimeBelow(ms(15)))],
        GIT,
        ACTIVE,
        Time,
    ),
    share(
        "git/open_handle",
        &[median(Limit::TimeBelow(ms(1)))],
        GIT,
        ACTIVE,
        Time,
    ),
    share(
        "git/changes_clean",
        &[median(Limit::TimeBelow(ms(15)))],
        GIT,
        ACTIVE,
        Time,
    ),
    share(
        "git/changes_50",
        &[median(Limit::TimeBelow(ms(15)))],
        GIT,
        ACTIVE,
        Time,
    ),
    // `refresh(Full)`'s share; refreshing one path is never slower.
    share(
        "git/refresh_one",
        &[median(Limit::TimeBelow(ms(15)))],
        GIT,
        ACTIVE,
        Time,
    ),
    share(
        "git/refresh_full",
        &[median(Limit::TimeBelow(ms(15)))],
        GIT,
        ACTIVE,
        Time,
    ),
    share(
        "git/diff_50_cold",
        &[median(Limit::TimeBelow(ms(20)))],
        GIT,
        ACTIVE,
        Time,
    ),
    share(
        "git/diff_50_warm",
        &[median(Limit::TimeBelow(ms(2)))],
        GIT,
        ACTIVE,
        Time,
    ),
    share(
        "git/changes_capped",
        &[median(Limit::TimeBelow(Duration::from_secs(1)))],
        GIT,
        ACTIVE,
        Time,
    ),
    // Live bytes at most the sum of the `Limits` defaults; the git
    // implementation writes the number into the git design and here.
    share("git/changes_capped_bytes", RECORD, GIT, ACTIVE, Memory),
    // The project home's count of changes outside sessions (ADR 0035):
    // 1000 modified tracked and 1000 untracked files of 10–50 KB and one
    // 16 MiB untracked file, with `retained_bytes = 0` and
    // `total_patch_bytes = 0`; the change set and what its `Worktree` holds.
    // Recorded. The stat cache no longer keeps read contents allocated;
    // the comparison's parsed index is still kept (git step 4).
    share("git/changes_count/total_bytes", RECORD, GIT, ACTIVE, Memory),
    // The same per changed path once the comparison index is released as
    // well (git design「与设计的差异」): 1111 B measured with the stat
    // cache fixed alone, so active with git step 4.
    share(
        "git/changes_count/per_path_bytes",
        &[median(Limit::BytesAtMost(1024))],
        GIT,
        State::Planned("git 第 4 步（释放比较用的 index；stat 缓存不持有内容已完成）"),
        Memory,
    ),
    // Hunk LRU 16 MB plus one `ChangeSet` 32 MB; no handle held while idle.
    share(
        "git/resident",
        &[median(Limit::BytesAtMost(48 * MB))],
        GIT,
        ACTIVE,
        Memory,
    ),
    // The current worktree's fs_watch filter with its `Worktree`: the
    // parsed index shared with `project`'s kept handle, the case-folded
    // order, the ignore rules. git's share (ADR 0035); `fs_watch/resident*`
    // and `project/resident_*` exclude it. `cloudprint` shape here, the
    // large repository below.
    share(
        "git/filter_cloudprint_bytes",
        &[median(Limit::BytesBelow(MB))],
        GIT,
        ACTIVE,
        Memory,
    ),
    // ---- the large repository (100 000 tracked files, ADR 0035) ----
    // `git/open` there: the index parsed with nothing cached; part of
    // `project/open_100k/files`.
    share(
        "git/open_100k",
        &[median(Limit::TimeBelow(ms(40)))],
        GIT,
        ACTIVE,
        Time,
    ),
    // The fs_watch filter rebuilt after an index change: index, case-folded
    // order, untracked walk and `.gitignore` files; batches wait for it.
    // Planned: macOS CI measured 766 and 678 ms (Linux 214–276 ms); a git
    // PR locates and fixes the cost (git design「与设计的差异」).
    share(
        "git/filter_100k",
        &[median(Limit::TimeBelow(ms(400)))],
        GIT,
        MACOS_100K,
        Time,
    ),
    // Recorded, to locate that cost: one listing of every directory of the
    // worktree through `SafeRoot::list`.
    share("git/filter_100k/walk", RECORD, GIT, ACTIVE, Time),
    // A full comparison of a clean worktree, in the background after the
    // file list is shown: the index parsed (cold, a new `Worktree` each
    // sample: no parsed index is kept between calls, ADR 0035), every
    // tracked file skipped by stat, the untracked walk. Every
    // `refresh(Full)` does the same. Planned: macOS CI measured 9.05 and
    // 9.54 s (Linux about 1.0 s), as above.
    share(
        "git/changes_clean_100k",
        &[median(Limit::TimeBelow(ms(2000)))],
        GIT,
        MACOS_100K,
        Time,
    ),
    // Recorded, to locate it: one `SafeRoot::stat`, and one `SafeRoot::read`,
    // of every tracked path.
    share("git/changes_clean_100k/stat_all", RECORD, GIT, ACTIVE, Time),
    share("git/changes_clean_100k/read_all", RECORD, GIT, ACTIVE, Time),
    // Recorded counts, to tell the hypotheses apart on macOS CI: the files
    // the first comparison read (1 when every stat matches: the root
    // `.gitignore` the untracked walk reads), the same after
    // `read_all` read every file, and per stat field how many index entries
    // differ from `SafeRoot::stat` before any read.
    share("git/changes_clean_100k/reads", RECORD, GIT, ACTIVE, Time),
    share(
        "git/changes_clean_100k/reads_after_read_all",
        RECORD,
        GIT,
        ACTIVE,
        Time,
    ),
    share(
        "git/changes_clean_100k/stat_racy",
        RECORD,
        GIT,
        ACTIVE,
        Time,
    ),
    share(
        "git/changes_clean_100k/stat_unmatched",
        RECORD,
        GIT,
        ACTIVE,
        Time,
    ),
    share(
        "git/changes_clean_100k/stat_mtime",
        RECORD,
        GIT,
        ACTIVE,
        Time,
    ),
    share(
        "git/changes_clean_100k/stat_size",
        RECORD,
        GIT,
        ACTIVE,
        Time,
    ),
    share(
        "git/changes_clean_100k/stat_ctime",
        RECORD,
        GIT,
        ACTIVE,
        Time,
    ),
    share("git/changes_clean_100k/stat_ino", RECORD, GIT, ACTIVE, Time),
    share(
        "git/changes_clean_100k/stat_owner",
        RECORD,
        GIT,
        ACTIVE,
        Time,
    ),
    // That comparison's peak of live bytes (temporary memory, not idle):
    // the comparison's own parsed index and case-folded copy, the walk,
    // the change set.
    share(
        "git/compare_100k/peak_bytes",
        &[median(Limit::BytesAtMost(72 * MB))],
        GIT,
        ACTIVE,
        Memory,
    ),
    // What the `Worktree` still holds after it, the change set dropped:
    // no parsed index between calls (ADR 0035); 39 MB until git step 4.
    share(
        "git/compare_100k/retained_bytes",
        &[median(Limit::BytesBelow(MB))],
        GIT,
        State::Planned("git 第 4 步"),
        Memory,
    ),
    // `git/filter_cloudprint_bytes` there.
    share(
        "git/filter_100k_bytes",
        &[median(Limit::BytesAtMost(16 * MB))],
        GIT,
        ACTIVE,
        Memory,
    ),
    // The partition is open and the system file cache warm.
    share(
        "cache/get_4k",
        &[median(Limit::TimeBelow(us(50)))],
        CACHE,
        ACTIVE,
        Time,
    ),
    share(
        "cache/get_many_50",
        &[median(Limit::TimeBelow(ms(1)))],
        CACHE,
        ACTIVE,
        Time,
    ),
    share(
        "cache/get_1m",
        &[median(Limit::TimeBelow(ms(2)))],
        CACHE,
        ACTIVE,
        Time,
    ),
    share(
        "cache/open_existing",
        &[median(Limit::TimeBelow(ms(5)))],
        CACHE,
        ACTIVE,
        Time,
    ),
    share(
        "cache/open_new",
        &[median(Limit::TimeBelow(ms(10)))],
        CACHE,
        ACTIVE,
        Time,
    ),
    share(
        "cache/put_4k",
        &[median(Limit::TimeBelow(ms(1)))],
        CACHE,
        ACTIVE,
        Time,
    ),
    share(
        "cache/batch_1000",
        &[median(Limit::TimeBelow(ms(15)))],
        CACHE,
        ACTIVE,
        Time,
    ),
    // A small write into a partition over its budget: evicts 1 MiB, and
    // returns 2 MiB of free pages once more than 32 MiB is free.
    share(
        "cache/evict",
        &[median(Limit::TimeBelow(ms(30)))],
        CACHE,
        ACTIVE,
        Time,
    ),
    // Checked before a namespace picks `Zstd`, against the user's budget.
    share("cache/zstd_1m/put", RECORD, CACHE, ACTIVE, Time),
    share("cache/zstd_1m/get", RECORD, CACHE, ACTIVE, Time),
    // Custom tables: one write of 64 units with their rows (code_intel's
    // batch); one read with one indexed query; a touch of 5000 units over an
    // hour old (4096 written back in the call); the same batch into a
    // namespace over its quota (evicts as much as it writes).
    share(
        "cache/tables_write",
        &[median(Limit::TimeBelow(ms(15)))],
        CACHE,
        ACTIVE,
        Time,
    ),
    share(
        "cache/tables_read",
        &[median(Limit::TimeBelow(us(50)))],
        CACHE,
        ACTIVE,
        Time,
    ),
    share(
        "cache/tables_touch",
        &[median(Limit::TimeBelow(ms(20)))],
        CACHE,
        ACTIVE,
        Time,
    ),
    share(
        "cache/tables_evict",
        &[median(Limit::TimeBelow(ms(30)))],
        CACHE,
        ACTIVE,
        Time,
    ),
    // The app partition and 2 repository partitions open, 50 MiB read from
    // each: resident growth of a child process (VmRSS on Linux,
    // phys_footprint on macOS), as SQLite's memory bypasses CountingAlloc.
    share(
        "cache/memory",
        &[median(Limit::BytesAtMost(8 * MB))],
        CACHE,
        ACTIVE,
        Memory,
    ),
    share(
        "project/open_cloudprint",
        &[median(Limit::TimeBelow(ms(20)))],
        PROJECT,
        ACTIVE,
        Time,
    ),
    share(
        "project/packages_cloudprint",
        &[median(Limit::TimeBelow(ms(15)))],
        PROJECT,
        ACTIVE,
        Time,
    ),
    share(
        "project/open_100k/files",
        &[median(Limit::TimeBelow(ms(80)))],
        PROJECT,
        ACTIVE,
        Time,
    ),
    share(
        "project/open_100k/packages",
        &[median(Limit::TimeBelow(ms(150)))],
        PROJECT,
        ACTIVE,
        Time,
    ),
    share(
        "project/switch_worktree",
        &[median(Limit::TimeBelow(ms(30)))],
        PROJECT,
        ACTIVE,
        Time,
    ),
    share(
        "project/resolve_package/hit",
        &[median(Limit::TimeBelow(ms(1)))],
        PROJECT,
        ACTIVE,
        Time,
    ),
    share(
        "project/resolve_package/miss_cloudprint",
        &[median(Limit::TimeBelow(ms(20)))],
        PROJECT,
        ACTIVE,
        Time,
    ),
    share(
        "project/resolve_package/miss_100k",
        &[median(Limit::TimeBelow(ms(150)))],
        PROJECT,
        ACTIVE,
        Time,
    ),
    // The model's refresh round only: the new index is parsed by the
    // fs_watch filter reload (git's work) before the batch arrives.
    share(
        "project/refresh_index/cloudprint",
        &[median(Limit::TimeBelow(ms(15)))],
        PROJECT,
        ACTIVE,
        Time,
    ),
    share(
        "project/refresh_index/100k",
        &[median(Limit::TimeBelow(ms(60)))],
        PROJECT,
        ACTIVE,
        Time,
    ),
    // Uncached: the pattern's result is not cached yet; each pattern asked
    // once per new set of facts, taking turns at being first.
    share(
        "project/facts_contains/name",
        &[median(Limit::TimeBelow(us(1)))],
        PROJECT,
        ACTIVE,
        Time,
    ),
    // The first pattern asked of facts just built.
    share(
        "project/facts_contains/name_first",
        RECORD,
        PROJECT,
        ACTIVE,
        Time,
    ),
    share(
        "project/facts_contains/path",
        &[median(Limit::TimeBelow(ms(5)))],
        PROJECT,
        ACTIVE,
        Time,
    ),
    // The plugin at rest, in live bytes: a kernel with it against one
    // without, in one process (kernel「性能」's two launches); the numbers
    // of `plugin/project/resident_*`. The one without arms the same watches,
    // so the fs_watch filter and the parsed index it shares with the
    // plugin's worktree handle (git's, `git/filter_*_bytes`) and
    // fs_watch's own state cancel out, not counted here.
    share(
        "project/resident_cloudprint_bytes",
        &[median(Limit::BytesBelow(2 * MB))],
        PROJECT,
        ACTIVE,
        Memory,
    ),
    share(
        "project/resident_100k_bytes",
        &[median(Limit::BytesBelow(16 * MB))],
        PROJECT,
        ACTIVE,
        Memory,
    ),
    // ---- fs_watch (docs/design/fs_watch.md「性能」) ----
    // Single-thread extraction, about 1 MiB of each first-release language as
    // 8 KiB files (repository-shaped), by the core languages' one-walk
    // extractors (ADR 0031).
    share(
        "code_intel/extract_throughput",
        &[median(Limit::ThroughputAtLeast(4 * MB))],
        CODE_INTEL,
        ACTIVE,
        Time,
    ),
    // One 1 MiB file per language, against the 250 ms per-file deadline.
    share("code_intel/extract_1mib", RECORD, CODE_INTEL, ACTIVE, Time),
    // A warm start's existence checks over 100 000 stored files: 200 queries
    // of 500 OIDs, read only (access marking is deferred off this path).
    share(
        "code_intel/warm_100k",
        &[median(Limit::TimeBelow(ms(500)))],
        CODE_INTEL,
        ACTIVE,
        Time,
    ),
    // The deferred access marking of those 100 000 hits, 1024 at a time,
    // every access time over an hour old.
    share(
        "code_intel/warm_100k_touch",
        RECORD,
        CODE_INTEL,
        ACTIVE,
        Time,
    ),
    // Rebuilding the blob → path map of 100 000 files (base part and hints
    // carried by path), and its live bytes without the file list.
    share(
        "code_intel/map_100k",
        &[median(Limit::TimeAtMost(ms(20)))],
        CODE_INTEL,
        ACTIVE,
        Time,
    ),
    share(
        "code_intel/map_100k_bytes",
        &[median(Limit::BytesAtMost(3 * MB))],
        CODE_INTEL,
        ACTIVE,
        Memory,
    ),
    // The peak of live bytes while the batch tiers index a synthetic
    // repository (8 KiB files and files near 1 MiB), syntax trees included.
    share(
        "code_intel/batch_peak",
        &[max(Limit::BytesAtMost(40 * MB))],
        CODE_INTEL,
        ACTIVE,
        Memory,
    ),
    // The partition's size after the units of 100 000 synthetic files.
    share("code_intel/disk_100k", RECORD, CODE_INTEL, ACTIVE, Time),
    // FakeBackend and the manual driver, without backend latency.
    share(
        "fs_watch/classify_burst",
        &[median(Limit::TimeBelow(ms(100)))],
        FS_WATCH,
        ACTIVE,
        Time,
    ),
    share(
        "fs_watch/reflog_append",
        &[median(Limit::TimeBelow(ms(1)))],
        FS_WATCH,
        ACTIVE,
        Time,
    ),
    // The real backend on macOS, once `app` injects it.
    share(
        "fs_watch/flush_idle",
        &[median(Limit::TimeBelow(ms(5)))],
        FS_WATCH,
        State::Planned("fs_watch 第 4 步"),
        Time,
    ),
    share(
        "fs_watch/write_to_deliver",
        &[median(Limit::TimeBelow(ms(120)))],
        FS_WATCH,
        State::Planned("fs_watch 第 4 步"),
        Time,
    ),
    // `watch_repo` plus the current worktree's `watch_worktree` on an
    // 11-worktree repository, at rest: fs_watch's own state. The filter
    // and the parsed index it shares with `project`'s worktree handle are
    // git's (ADR 0035), measured apart and not counted here.
    share(
        "fs_watch/resident",
        &[median(Limit::BytesBelow(MB))],
        FS_WATCH,
        State::Planned("fs_watch 第 4 步"),
        Memory,
    ),
    // The same on the large repository (100 000 files, 17 worktrees).
    share(
        "fs_watch/resident_100k",
        &[median(Limit::BytesBelow(MB))],
        FS_WATCH,
        State::Planned("fs_watch 第 4 步"),
        Memory,
    ),
    // ---- end fs_watch ----
    share(
        "platform/safe_stat",
        &[
            median(Limit::RatioAtMost(3.0)),
            median(Limit::TimeBelow(us(10))),
        ],
        PLATFORM,
        ACTIVE,
        Time,
    ),
    share(
        "platform/safe_read_4k",
        &[median(Limit::RatioAtMost(1.5))],
        PLATFORM,
        ACTIVE,
        Time,
    ),
    // Range read (platform design「范围读取」): the first-screen call on a
    // 64 MiB file (8 KiB head and 132 KiB around a mid-file target), and
    // 64 KiB pages on a held `OpenFile`; ratios against the std equivalents.
    share(
        "platform/safe_read_range",
        &[
            median(Limit::RatioAtMost(1.5)),
            median(Limit::TimeBelow(us(200))),
        ],
        PLATFORM,
        ACTIVE,
        Time,
    ),
    share(
        "platform/safe_read_page",
        &[
            median(Limit::RatioAtMost(1.5)),
            median(Limit::TimeBelow(us(50))),
        ],
        PLATFORM,
        ACTIVE,
        Time,
    ),
    share("executor/spawn_10k", RECORD, EXECUTOR, ACTIVE, Time),
    share("executor/wake_1000x100", RECORD, EXECUTOR, ACTIVE, Time),
    share("kernel/service_get", RECORD, KERNEL, ACTIVE, Time),
    share("kernel/service_for_each/1", RECORD, KERNEL, ACTIVE, Time),
    share("kernel/service_for_each/4", RECORD, KERNEL, ACTIVE, Time),
    share("kernel/service_for_each/16", RECORD, KERNEL, ACTIVE, Time),
    share("kernel/service_current/1", RECORD, KERNEL, ACTIVE, Time),
    share("kernel/service_current/4", RECORD, KERNEL, ACTIVE, Time),
    share("kernel/service_current/16", RECORD, KERNEL, ACTIVE, Time),
    share("kernel/emit", RECORD, KERNEL, ACTIVE, Time),
    share("kernel/emit_nested", RECORD, KERNEL, ACTIVE, Time),
    // Every message of the en resources once; those with variables
    // formatted with sample arguments.
    share("i18n/frame_strings", RECORD, I18N, ACTIVE, Time),
];

/// The budget for a result ID: the entry with the same ID, otherwise the
/// matching entry with the fewest `*` segments.
pub fn lookup<'a>(budgets: &'a [Budget], id: &str) -> Option<&'a Budget> {
    budgets
        .iter()
        .filter(|b| matches(b.id, id))
        .min_by_key(|b| b.id.split('/').filter(|s| *s == "*").count())
}

/// Whether the pattern `pattern` (segments, `*` for any one) matches `id`.
pub fn matches(pattern: &str, id: &str) -> bool {
    let mut p = pattern.split('/');
    let mut i = id.split('/');
    loop {
        match (p.next(), i.next()) {
            (None, None) => return true,
            (Some("*"), Some(seg)) if !seg.is_empty() => {}
            (Some(a), Some(b)) if a == b => {}
            _ => return false,
        }
    }
}

/// Whether `prefix` is `id` or a whole-segment prefix of it.
pub fn is_prefix(prefix: &str, id: &str) -> bool {
    id == prefix
        || id
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('/'))
}

impl Budget {
    pub fn is_wildcard(&self) -> bool {
        self.id.split('/').any(|s| s == "*")
    }

    /// The checks in words, e.g. "throughput ≥ 200 MB/s (median)".
    pub fn describe(&self) -> String {
        if self.checks.is_empty() {
            return "record".to_owned();
        }
        self.checks
            .iter()
            .map(Check::describe)
            .collect::<Vec<_>>()
            .join(", ")
    }
}

impl Check {
    pub fn describe(&self) -> String {
        let stat = self.stat.name();
        match self.limit {
            Limit::TimeBelow(d) => format!("{stat} < {}", fmt_duration(d)),
            Limit::TimeAtMost(d) => format!("{stat} ≤ {}", fmt_duration(d)),
            Limit::ThroughputAtLeast(b) => format!("throughput ≥ {}/s ({stat})", fmt_bytes(b)),
            Limit::BytesBelow(b) => format!("{stat} < {}", fmt_bytes(b)),
            Limit::BytesAtMost(b) => format!("{stat} ≤ {}", fmt_bytes(b)),
            Limit::RatioAtMost(r) => format!("ratio ≤ {} ({stat})", fmt_number(r)),
            Limit::CountAtMost(n) => format!("{stat} ≤ {n}"),
            Limit::RefreshMultipleAtMost(r) => {
                format!("interval ≤ {} refresh periods ({stat})", fmt_number(r))
            }
        }
    }
}

impl Stat {
    pub fn name(&self) -> String {
        match self {
            Stat::Median => "median".to_owned(),
            Stat::P95 => "p95".to_owned(),
            Stat::Max => "max".to_owned(),
            Stat::CountAbove(d) => format!("count > {}", fmt_duration(*d)),
        }
    }
}

/// Up to three significant digits, trailing zeros dropped: `8.3`, `200`,
/// `0.123`.
pub fn fmt_number(x: f64) -> String {
    if x == 0.0 || !x.is_finite() {
        return format!("{x}");
    }
    let digits = (2 - x.abs().log10().floor() as i32).max(0) as usize;
    let s = format!("{x:.digits$}");
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_owned()
    } else {
        s
    }
}

/// In the largest unit (s, ms, µs, ns) that keeps the number at least 1.
pub fn fmt_duration(d: Duration) -> String {
    fmt_seconds(d.as_secs_f64())
}

pub fn fmt_seconds(s: f64) -> String {
    let (value, unit) = if s >= 1.0 {
        (s, "s")
    } else if s >= 1e-3 {
        (s * 1e3, "ms")
    } else if s >= 1e-6 {
        (s * 1e6, "µs")
    } else {
        (s * 1e9, "ns")
    };
    format!("{} {unit}", fmt_number(value))
}

/// In MB (2^20), KB (2^10) or B.
pub fn fmt_bytes(b: u64) -> String {
    fmt_bytes_f(b as f64)
}

pub fn fmt_bytes_f(b: f64) -> String {
    if b >= MB as f64 {
        format!("{} MB", fmt_number(b / MB as f64))
    } else if b >= 1024.0 {
        format!("{} KB", fmt_number(b / 1024.0))
    } else {
        format!("{} B", fmt_number(b))
    }
}

#[cfg(test)]
mod tests;
