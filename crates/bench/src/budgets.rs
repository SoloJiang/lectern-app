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
    /// The benchmark only builds on macOS (it needs GPUI, which this
    /// workspace builds there alone): off macOS the entry is `skipped`
    /// (「只在 macOS 运行」), not an error for a missing benchmark.
    pub macos_only: bool,
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

const fn ns(n: u64) -> Duration {
    Duration::from_nanos(n)
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

const fn p95(limit: Limit) -> Check {
    Check {
        stat: Stat::P95,
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
const FILE_READER: &str = "docs/design/file_reader.md#性能";
const REVIEW: &str = "docs/design/review.md#性能";
const LOGGING: &str = "docs/design/logging.md#性能";
const USAGE: &str = "docs/design/usage.md#性能";
const UI: &str = "docs/design/ui.md#性能";

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
        macos_only: false,
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
        macos_only: false,
    }
}

/// A `Share` budget whose benchmark needs GPUI, so builds on macOS alone.
const fn share_macos(
    id: &'static str,
    checks: &'static [Check],
    source: &'static str,
    state: State,
    target: Target,
) -> Budget {
    Budget {
        macos_only: true,
        ..share(id, checks, source, state, target)
    }
}

const ACTIVE: State = State::Active;
const STEP_4: State = State::Planned("bench 第 4 步");
const STEP_5: State = State::Planned("bench 第 5 步");

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
        "agents/discover_codex_1000",
        &[median(Limit::TimeBelow(ms(500)))],
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
    // Recorded. The stat cache keeps no read contents allocated and the
    // comparison's parsed index is dropped when the call returns.
    share("git/changes_count/total_bytes", RECORD, GIT, ACTIVE, Memory),
    // The same per changed path: what ADR 0035 bounds the home's kept
    // change set by.
    share(
        "git/changes_count/per_path_bytes",
        &[median(Limit::BytesAtMost(1024))],
        GIT,
        ACTIVE,
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
    // The fs_watch filter rebuilt after an index-only change, given the
    // filter in place: index, case-folded order, the index compared with the
    // previous one, the untracked walk reused; batches wait for it. The full
    // build walked every directory: 678–766 ms on macOS CI, the walk alone
    // 473 ms (git design「index」「性能」).
    share(
        "git/filter_100k",
        &[median(Limit::TimeBelow(ms(400)))],
        GIT,
        ACTIVE,
        Time,
    ),
    // Recorded: the first build (no previous filter, the walk included), and
    // one listing of every directory of the worktree through `SafeRoot::list`.
    share("git/filter_100k/cold", RECORD, GIT, ACTIVE, Time),
    share("git/filter_100k/walk", RECORD, GIT, ACTIVE, Time),
    // A full comparison of a clean worktree, in the background after the
    // file list is shown: the index parsed (cold, a new `Worktree` each
    // sample: no parsed index is kept between calls, ADR 0035), every
    // tracked file skipped by stat, the untracked walk. Every
    // `refresh(Full)` does the same. Active again: macOS CI measured about
    // 3.7 s (base tree 1.84 s, stat 1.15 s, walk 645 ms); the cache-tree
    // skips the base tree and the stat pass runs on a few threads (ADR 0038).
    share(
        "git/changes_clean_100k",
        &[median(Limit::TimeBelow(ms(2000)))],
        GIT,
        ACTIVE,
        Time,
    ),
    // Recorded, to locate it: one `SafeRoot::stat` of every tracked path.
    // (`…/read_all`, one `SafeRoot::read` of each, is gone: 14.6 s a sample
    // on macOS CI, and the counts below say whether the comparison reads.)
    share("git/changes_clean_100k/stat_all", RECORD, GIT, ACTIVE, Time),
    // Recorded counts, to tell the hypotheses apart on macOS CI: the files
    // the first comparison read (1 when every stat matches: the root
    // `.gitignore` the untracked walk reads), and per stat field how many
    // index entries differ from `SafeRoot::stat` before any read.
    share("git/changes_clean_100k/reads", RECORD, GIT, ACTIVE, Time),
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
    // Recorded: one cold comparison on that repository, timed by phase
    // (`testing::compare_phases`), to find where macOS spends its ~9 s with
    // every stat matching; `phase_rest` is the call's time no phase covers.
    share(
        "git/changes_clean_100k/phase_open",
        RECORD,
        GIT,
        ACTIVE,
        Time,
    ),
    share(
        "git/changes_clean_100k/phase_index",
        RECORD,
        GIT,
        ACTIVE,
        Time,
    ),
    share(
        "git/changes_clean_100k/phase_base_tree",
        RECORD,
        GIT,
        ACTIVE,
        Time,
    ),
    share(
        "git/changes_clean_100k/phase_setup",
        RECORD,
        GIT,
        ACTIVE,
        Time,
    ),
    share(
        "git/changes_clean_100k/phase_stat",
        RECORD,
        GIT,
        ACTIVE,
        Time,
    ),
    share(
        "git/changes_clean_100k/phase_fold",
        RECORD,
        GIT,
        ACTIVE,
        Time,
    ),
    share(
        "git/changes_clean_100k/phase_untracked",
        RECORD,
        GIT,
        ACTIVE,
        Time,
    ),
    share(
        "git/changes_clean_100k/phase_attributes",
        RECORD,
        GIT,
        ACTIVE,
        Time,
    ),
    share(
        "git/changes_clean_100k/phase_classify",
        RECORD,
        GIT,
        ACTIVE,
        Time,
    ),
    share(
        "git/changes_clean_100k/phase_assemble",
        RECORD,
        GIT,
        ACTIVE,
        Time,
    ),
    share(
        "git/changes_clean_100k/phase_finish",
        RECORD,
        GIT,
        ACTIVE,
        Time,
    ),
    share(
        "git/changes_clean_100k/phase_rest",
        RECORD,
        GIT,
        ACTIVE,
        Time,
    ),
    // Recorded: `refresh(Paths)` of one unchanged tracked path there, what
    // one fs_watch event costs now that every refresh parses the index
    // again (ADR 0035). `git/refresh_one`'s 15 ms is for the cloudprint
    // shape.
    share("git/refresh_one_100k", RECORD, GIT, ACTIVE, Time),
    // Recorded: `canonical_oid` there on a new `Worktree` (one index parse,
    // as code_intel's git access calls it), and `canonical_oids` for 100
    // paths in one call. code_intel's design gives no share for it yet; it
    // counts towards its caller's budget (git design「规范内容的 OID」).
    share("git/canonical_oid_100k", RECORD, GIT, ACTIVE, Time),
    share(
        "git/canonical_oid_100k/batch_100",
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
    // no parsed index between calls (ADR 0035; it was 39 MB).
    share(
        "git/compare_100k/retained_bytes",
        &[median(Limit::BytesBelow(MB))],
        GIT,
        ACTIVE,
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
    // The directory table of Files, `project`'s share of the file reader:
    // the model's background job building and publishing it for 100k
    // files, and the table's own live bytes (the file list it shares not
    // counted); it is part of `project/resident_100k_bytes`.
    share(
        "project/dirs_100k",
        &[median(Limit::TimeAtMost(ms(20)))],
        FILE_READER,
        ACTIVE,
        Time,
    ),
    share(
        "project/dirs_100k_bytes",
        &[median(Limit::BytesAtMost(3 * MB / 2))],
        FILE_READER,
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
    // The yield switch comes on while a 1 MiB TypeScript file is parsed: the
    // extraction stops at its next slice boundary, its state kept (ADR 0033).
    share(
        "code_intel/yield_stop",
        &[max(Limit::TimeAtMost(ms(10)))],
        CODE_INTEL,
        ACTIVE,
        Time,
    ),
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
    // A checkout leaves 1000 CRLF files of a 10 000-file repository
    // (`core.autocrlf = true`) in the re-read set: from fs_watch's events
    // to the overlay installed, one `canonical_oids` per 256 files
    // (「规范内容 OID 的批量核对」).
    share(
        "code_intel/reread_crlf_1000",
        RECORD,
        CODE_INTEL,
        ACTIVE,
        Time,
    ),
    // An opened 16 MiB TypeScript file: from the focus to its window chain
    // written to the end (「大文件」, ADR 0034), and the peak of live bytes
    // meanwhile: the streamed hash's 1 MiB range or one 64 KiB window and
    // its tree, about 2 MiB plus 1 MiB, with twice the room.
    share(
        "code_intel/window_chain_16mib",
        RECORD,
        CODE_INTEL,
        ACTIVE,
        Time,
    ),
    share(
        "code_intel/window_peak",
        &[max(Limit::BytesAtMost(8 * MB))],
        CODE_INTEL,
        ACTIVE,
        Memory,
    ),
    // The dependency tier (「依赖层」, ADR 0033) of a synthetic TS project:
    // 21 direct dependencies, a declaration closure of about 5 MiB and a
    // 4 MiB `package-lock.json`. From an empty cache to the tier done
    // (recorded); with every path table and unit cached, the identity
    // tasks' own durations when the project is opened again (from each
    // task's start to its end, summed; the rest of the open is other
    // budgets'); the live bytes of the dependency map at its limits
    // (20 000 files offered, paths of about 48 bytes, SHA-256).
    share(
        "code_intel/deps_ts_project",
        RECORD,
        CODE_INTEL,
        ACTIVE,
        Time,
    ),
    share(
        "code_intel/deps_identity",
        &[median(Limit::TimeAtMost(ms(50)))],
        CODE_INTEL,
        ACTIVE,
        Time,
    ),
    share(
        "code_intel/deps_map_bytes",
        &[median(Limit::BytesAtMost(MB))],
        CODE_INTEL,
        ACTIVE,
        Memory,
    ),
    // A jump the index answers, from the query to its first batch with
    // candidates: 100 000 indexed files, a 100 KB current text whose parse
    // is kept (ADR 0037); this crate's part of the 50 ms candidates budget.
    // In the states a first jump meets: the accounting's walk not cached,
    // a name some 3000 files define, the namespace at its quota. To the
    // query's end recorded; within the 1 s time box once full.
    share(
        "code_intel/goto_indexed",
        &[median(Limit::TimeAtMost(ms(20)))],
        CODE_INTEL,
        ACTIVE,
        Time,
    ),
    share(
        "code_intel/goto_indexed/done",
        RECORD,
        CODE_INTEL,
        ACTIVE,
        Time,
    ),
    share(
        "code_intel/goto_uncached",
        &[median(Limit::TimeAtMost(ms(20)))],
        CODE_INTEL,
        ACTIVE,
        Time,
    ),
    share(
        "code_intel/goto_uncached/done",
        RECORD,
        CODE_INTEL,
        ACTIVE,
        Time,
    ),
    share(
        "code_intel/goto_popular",
        &[median(Limit::TimeAtMost(ms(20)))],
        CODE_INTEL,
        ACTIVE,
        Time,
    ),
    share(
        "code_intel/goto_popular/done",
        RECORD,
        CODE_INTEL,
        ACTIVE,
        Time,
    ),
    share(
        "code_intel/goto_full",
        &[median(Limit::TimeAtMost(ms(20)))],
        CODE_INTEL,
        ACTIVE,
        Time,
    ),
    share(
        "code_intel/goto_full/done",
        &[max(Limit::TimeAtMost(ms(1020)))],
        CODE_INTEL,
        ACTIVE,
        Time,
    ),
    // The first jump in a text before its parse is kept (the text parsed
    // for the site), the text just under 96 KiB: the 50 ms candidates less
    // the drawer's 18 ms (one frame and the event's delivery), ADR 0037.
    // Just under 128 KiB (the largest kept, where the 50 ms holds through
    // the kept parse only), a jump its own locals bind (a `File`
    // definition), and one from a diff's old side: recorded.
    share(
        "code_intel/goto_cold_parse",
        &[median(Limit::TimeAtMost(ms(32)))],
        CODE_INTEL,
        ACTIVE,
        Time,
    ),
    share(
        "code_intel/goto_cold_parse/at_128k",
        RECORD,
        CODE_INTEL,
        ACTIVE,
        Time,
    ),
    share(
        "code_intel/goto_same_file",
        RECORD,
        CODE_INTEL,
        ACTIVE,
        Time,
    ),
    share("code_intel/goto_object", RECORD, CODE_INTEL, ACTIVE, Time),
    // A jump in the middle of a 4 MiB current text whose identity and
    // checkpoints are recorded (「大文件」, ADR 0034): one range read around
    // the checkpoint, the window around the target parsed (8 ms deadline),
    // then as any jump. Without the record (streamed once) and from the
    // object database (the blob taken whole) recorded.
    share(
        "code_intel/goto_large",
        &[median(Limit::TimeAtMost(ms(20)))],
        CODE_INTEL,
        ACTIVE,
        Time,
    ),
    share(
        "code_intel/goto_large/cold",
        RECORD,
        CODE_INTEL,
        ACTIVE,
        Time,
    ),
    share(
        "code_intel/goto_large/object",
        RECORD,
        CODE_INTEL,
        ACTIVE,
        Time,
    ),
    // The kept parses at their bounds: two texts just under 128 KiB (ADR
    // 0037).
    share(
        "code_intel/parse_cache_bytes",
        &[max(Limit::BytesAtMost(12 * MB))],
        CODE_INTEL,
        ACTIVE,
        Memory,
    ),
    // The first jump of a fresh index: the project opened and settled (its
    // languages' queries compiled in the background), nothing warmed by an
    // earlier jump. Recorded against the 50 ms candidates budget.
    share("code_intel/goto_first", RECORD, CODE_INTEL, ACTIVE, Time),
    // `Named` for a name some 50 000 files define: from the query to its
    // first batch; to `Done`, and the candidates and unsearched files at
    // `Done` (the rest of the lookup takes most of the 1 s time box, so a
    // slow runner ends `Partial`: recorded, never an error).
    share("code_intel/named_50k", RECORD, CODE_INTEL, ACTIVE, Time),
    share(
        "code_intel/named_50k/done",
        RECORD,
        CODE_INTEL,
        ACTIVE,
        Time,
    ),
    share(
        "code_intel/named_50k/candidates",
        RECORD,
        CODE_INTEL,
        ACTIVE,
        Time,
    ),
    share(
        "code_intel/named_50k/unsearched",
        RECORD,
        CODE_INTEL,
        ACTIVE,
        Time,
    ),
    // The time-boxed search of unindexed hits stops one second after the
    // query; the last batch's event within 20 ms of that.
    share(
        "code_intel/search_stop",
        &[max(Limit::TimeAtMost(ms(1020)))],
        CODE_INTEL,
        ACTIVE,
        Time,
    ),
    // References of a function 300 of 100 000 indexed files use: all of
    // them, and the first batch.
    share(
        "code_intel/references_100k/all",
        &[median(Limit::TimeAtMost(ms(2000)))],
        CODE_INTEL,
        ACTIVE,
        Time,
    ),
    share(
        "code_intel/references_100k/first",
        &[median(Limit::TimeAtMost(ms(200)))],
        CODE_INTEL,
        ACTIVE,
        Time,
    ),
    // The same with the namespace at its quota: both gated all the same,
    // complete (references' own time box, `REFERENCES_FOR`). A round the
    // time box cut is a failing sample of both (ADR 0043); how many were
    // cut and the most files one left outside are recorded.
    share(
        "code_intel/references_full/all",
        &[median(Limit::TimeAtMost(ms(2000)))],
        CODE_INTEL,
        ACTIVE,
        Time,
    ),
    share(
        "code_intel/references_full/first",
        &[median(Limit::TimeAtMost(ms(200)))],
        CODE_INTEL,
        ACTIVE,
        Time,
    ),
    share(
        "code_intel/references_full/partial_rounds",
        RECORD,
        CODE_INTEL,
        ACTIVE,
        Time,
    ),
    share(
        "code_intel/references_full/outside",
        RECORD,
        CODE_INTEL,
        ACTIVE,
        Time,
    ),
    // The ⌘K name table of 100 000 files (synthetic, about 320 000 names):
    // its build and its live bytes.
    share(
        "code_intel/names_100k",
        &[median(Limit::TimeAtMost(ms(1000)))],
        CODE_INTEL,
        ACTIVE,
        Time,
    ),
    share(
        "code_intel/names_100k_bytes",
        &[median(Limit::BytesAtMost(8 * MB))],
        CODE_INTEL,
        ACTIVE,
        Memory,
    ),
    // This crate's live bytes with a project indexed (counting allocator,
    // tree-sitter's allocations routed through it), not the file list.
    share(
        "plugin/code-intel/resident_cloudprint",
        &[median(Limit::BytesAtMost(4 * MB))],
        CODE_INTEL,
        ACTIVE,
        Memory,
    ),
    share(
        "plugin/code-intel/resident_100k",
        &[median(Limit::BytesAtMost(10 * MB))],
        CODE_INTEL,
        ACTIVE,
        Memory,
    ),
    // ---- syntax (file_reader design, steps 2 to 5) ----
    // The first screen: open, the one `read_ranges`, the plan and the
    // window's parse and query with the 8 ms deadline, on synthetic
    // TypeScript, Rust and JSON files of 1–64 MiB, 40 random offsets each;
    // the same on 4 MiB of JavaScript minified into one line, record only.
    share(
        "syntax/first_screen",
        &[median(Limit::TimeAtMost(ms(8)))],
        FILE_READER,
        ACTIVE,
        Time,
    ),
    share(
        "syntax/first_screen/minified",
        RECORD,
        FILE_READER,
        ACTIVE,
        Time,
    ),
    // The peak of live bytes of one 64 KiB window of the chain (its tree,
    // the query's captures, the tokens), tree-sitter's allocations counted.
    share(
        "syntax/window_peak",
        &[max(Limit::BytesAtMost(8 * MB))],
        FILE_READER,
        ACTIVE,
        Memory,
    ),
    // Tier A's whole-file parse of 1 MiB in 8 ms slices, and tier B's whole
    // window chain over 16 MiB. Record only.
    share("syntax/full_parse_1mib", RECORD, FILE_READER, ACTIVE, Time),
    share("syntax/chain_16mib", RECORD, FILE_READER, ACTIVE, Time),
    // The first screen of object database content (ADR 0042): a 4 MiB
    // blob the review registered, at its middle; a 1 MiB blob nobody holds,
    // taken out on the first screen's path; a 4 MiB one nobody holds,
    // record only.
    share(
        "syntax/first_screen/object_shared",
        &[median(Limit::TimeAtMost(ms(8)))],
        FILE_READER,
        ACTIVE,
        Time,
    ),
    share(
        "syntax/first_screen/object_1mib",
        &[median(Limit::TimeAtMost(ms(8)))],
        FILE_READER,
        ACTIVE,
        Time,
    ),
    share(
        "syntax/first_screen/object_cold",
        RECORD,
        FILE_READER,
        ACTIVE,
        Time,
    ),
    // The highlight and text LRU at its fullest (ADR 0030, ADR 0035): the
    // live bytes the engine releases when dropped, after 24 tier A files.
    share(
        "syntax/lru_bytes",
        &[max(Limit::BytesAtMost(8 * MB))],
        FILE_READER,
        ACTIVE,
        Memory,
    ),
    // Tier C's streaming scan of 64 MiB, and find in 16 MiB. Record only.
    share("syntax/scan_64mib", RECORD, FILE_READER, ACTIVE, Time),
    share("syntax/find_16mib", RECORD, FILE_READER, ACTIVE, Time),
    // A prefetch's wait on the batch threads while the real code_intel
    // indexes 100 000 files and a 16 MiB file's window chain runs: from
    // the request to the prefetch task's turn. Each thread's batch (about
    // 350 ms) and one window task (50 ms), 25% over; the maximum is only
    // recorded (「批量线程上的先后」).
    share(
        "syntax/prefetch_wait",
        &[p95(Limit::TimeAtMost(ms(500)))],
        FILE_READER,
        ACTIVE,
        Time,
    ),
    // An edit session's tree at tier A's limit (1 MiB of TypeScript and of
    // Rust), with the buffer and its spans, after the first parse; released
    // when the session ends (ADR 0034).
    share(
        "syntax/edit_tree_bytes",
        &[max(Limit::BytesAtMost(41 * MB))],
        FILE_READER,
        ACTIVE,
        Memory,
    ),
    // ---- end syntax ----
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
    // ---- review (review design, step 1) ----
    // The annotation log read whole and its index built: 16 000 records
    // (about 5.3 MiB), in the background when a project opens.
    share(
        "review/log_load_16k",
        &[median(Limit::TimeBelow(ms(30)))],
        REVIEW,
        ACTIVE,
        Time,
    ),
    // (step 2) One file's change chain: 3000 lines, 40 `Replace` steps,
    // each with its `line_diff`, no `load`. Recorded.
    share("review/chain_3000_lines", RECORD, REVIEW, ACTIVE, Time),
    // (step 3) The review engine through `testing::Driven`, on the fixture
    // of「每天的仓库规模」(2000 commits, 300 reflog records, 50 sessions):
    // `open` to the first `Ready` snapshot with the `review.*` namespaces
    // empty / warm; to the last background snapshot; the cutoff table with
    // every `tree_changes` a miss; the other 49 files' attribution.
    share(
        "review/open_session_50",
        &[median(Limit::TimeBelow(ms(35)))],
        REVIEW,
        ACTIVE,
        Time,
    ),
    share(
        "review/open_session_50_warm",
        &[median(Limit::TimeBelow(ms(20)))],
        REVIEW,
        ACTIVE,
        Time,
    ),
    share("review/open_session_settled", RECORD, REVIEW, ACTIVE, Time),
    share("review/cutoffs_cold", RECORD, REVIEW, ACTIVE, Time),
    share("review/attribute_session_50", RECORD, REVIEW, ACTIVE, Time),
    // The first file's `Prepare` to `Prepared`; a seen mark on 5000 files.
    share(
        "review/rows_first",
        &[median(Limit::TimeBelow(ms(3)))],
        REVIEW,
        ACTIVE,
        Time,
    ),
    share(
        "review/mark_seen_5000",
        &[median(Limit::TimeBelow(us(200)))],
        REVIEW,
        ACTIVE,
        Time,
    ),
    // (step 5) The rule brief of 5000 files.
    share(
        "review/brief_5000",
        &[median(Limit::TimeBelow(ms(5)))],
        REVIEW,
        ACTIVE,
        Time,
    ),
    // (step 4) The agent's next turn and one file's change, from the batch
    // and the source's event to the first snapshot that shows both.
    share(
        "review/refresh_one",
        &[median(Limit::TimeBelow(ms(20)))],
        REVIEW,
        ACTIVE,
        Time,
    ),
    // (step 5) The second round of the overview: claims and cutoffs from
    // memory and the cache. The cloudprint round is recorded only.
    share(
        "review/overview_round_warm",
        &[median(Limit::TimeBelow(ms(20)))],
        REVIEW,
        ACTIVE,
        Time,
    ),
    share("review/overview_cloudprint", RECORD, REVIEW, ACTIVE, Time),
    // (step 5) This crate's resident bytes with an overview ready, the
    // count set not counted (docs/design/review.md「性能」).
    share(
        "review/resident_cloudprint_bytes",
        &[median(Limit::BytesBelow(MB))],
        REVIEW,
        ACTIVE,
        Memory,
    ),
    share(
        "review/resident_100k_bytes",
        &[median(Limit::BytesBelow(MB))],
        REVIEW,
        ACTIVE,
        Memory,
    ),
    // In process until the process-level plugin benchmarks (step 5): the
    // kernel's four timings under a `TestKernel`.
    share(
        "plugin/review/activate",
        &[median(Limit::TimeBelow(ms(1)))],
        KERNEL,
        ACTIVE,
        Time,
    ),
    // ---- end review ----
    // ---- usage (usage design, step 1) ----
    // `testing::Scenario` on `testing::heavy_days` (30 heavy days, 20
    // repositories, about 180 000 fact rows).
    share(
        "usage/ingest_append_4k",
        &[median(Limit::TimeBelow(ms(5)))],
        USAGE,
        ACTIVE,
        Time,
    ),
    share(
        "usage/backfill_batch",
        &[median(Limit::TimeBelow(ms(150)))],
        USAGE,
        ACTIVE,
        Time,
    ),
    // A 200 MB session in 8 MiB batches, each batch a sample.
    share(
        "usage/backfill_large_session",
        &[
            max(Limit::TimeBelow(ms(300))),
            median(Limit::TimeBelow(ms(150))),
        ],
        USAGE,
        ACTIVE,
        Time,
    ),
    share(
        "usage/project_30d",
        &[median(Limit::TimeBelow(ms(50)))],
        USAGE,
        ACTIVE,
        Time,
    ),
    share(
        "usage/all_projects_30d",
        &[median(Limit::TimeBelow(ms(80)))],
        USAGE,
        ACTIVE,
        Time,
    ),
    share(
        "usage/snapshot_increment",
        &[median(Limit::TimeBelow(ms(2)))],
        USAGE,
        ACTIVE,
        Time,
    ),
    // Each segment of a heavy month a sample; the compression ratio in the
    // note.
    share(
        "usage/archive_month",
        &[median(Limit::TimeBelow(ms(150)))],
        USAGE,
        ACTIVE,
        Time,
    ),
    share(
        "usage/cold_read_month",
        &[median(Limit::TimeBelow(ms(1000)))],
        USAGE,
        ACTIVE,
        Time,
    ),
    share("usage/disk_day_heavy", RECORD, USAGE, ACTIVE, Time),
    share(
        "usage/resident_bytes",
        &[median(Limit::BytesAtMost(2 * MB))],
        USAGE,
        ACTIVE,
        Memory,
    ),
    // In process until the process-level plugin benchmarks (step 5): the
    // kernel's four timings under a `TestKernel`.
    share(
        "plugin/usage/activate",
        &[median(Limit::TimeBelow(ms(1)))],
        KERNEL,
        ACTIVE,
        Time,
    ),
    // ---- end usage ----
    // ---- logging (logging design, step 1) ----
    // On the calling thread: a call below the global level; one the global
    // level lets through and its target rejects; a written info record with
    // 4 fields (each sample 256 calls after the writer emptied the buffer).
    share(
        "log/disabled_call",
        &[median(Limit::TimeAtMost(ns(2)))],
        LOGGING,
        ACTIVE,
        Time,
    ),
    share(
        "log/filtered_call",
        &[median(Limit::TimeAtMost(ns(5)))],
        LOGGING,
        ACTIVE,
        Time,
    ),
    share(
        "log/enabled_call",
        &[median(Limit::TimeAtMost(us(1)))],
        LOGGING,
        ACTIVE,
        Time,
    ),
    // Four threads writing at once, each call timed on its own: exactly
    // 10 000 calls in measure mode, so「at most 100 above 5 µs」is the p99
    // ≤ 5 µs of the design. No record dropped.
    share(
        "log/contended_call",
        &[Check {
            stat: Stat::CountAbove(us(5)),
            limit: Limit::CountAtMost(100),
        }],
        LOGGING,
        ACTIVE,
        Time,
    ),
    // A third-party record whose call site is over its limit.
    share(
        "log/third_party_limited",
        &[median(Limit::TimeAtMost(ns(30)))],
        LOGGING,
        ACTIVE,
        Time,
    ),
    share(
        "log/install_early",
        &[median(Limit::TimeAtMost(us(20)))],
        LOGGING,
        ACTIVE,
        Time,
    ),
    // Live bytes after 10 000 records and 5 s at rest, default
    // configuration; with verbose logging on, record only.
    share(
        "log/idle_memory",
        &[median(Limit::BytesAtMost(512 << 10))],
        LOGGING,
        ACTIVE,
        Memory,
    ),
    share("log/idle_memory/verbose", RECORD, LOGGING, ACTIVE, Memory),
    // ---- end logging ----
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
    // `ui/*` (docs/design/ui.md「性能」): GPUI, so macOS alone. Step 2's
    // two and step 4's `ui/semantic_disarmed`; `ui/init`, `ui/ready`,
    // `ui/palette_resolve`, `ui/mono_metrics_first`, `ui/idle_memory` come
    // with the `app` PR of step 1.
    share_macos(
        "ui/transition_tick",
        &[median(Limit::TimeAtMost(ns(100)))],
        UI,
        ACTIVE,
        Time,
    ),
    share_macos(
        "ui/bezier_eval",
        &[median(Limit::TimeAtMost(ns(50)))],
        UI,
        ACTIVE,
        Time,
    ),
    share_macos(
        "ui/semantic_disarmed",
        &[median(Limit::TimeAtMost(ns(2)))],
        UI,
        ACTIVE,
        Time,
    ),
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
