//! In-process timing benchmarks, one module per crate under test
//! (docs/design/bench.md「基准目标」).

#[path = "time/agents.rs"]
mod agents;
#[path = "time/cache.rs"]
mod cache;
#[path = "time/code_intel.rs"]
mod code_intel;
#[path = "time/executor.rs"]
mod executor;
#[path = "time/fs_watch.rs"]
mod fs_watch;
#[path = "time/git.rs"]
mod git;
#[path = "fixtures/git_worktree.rs"]
mod git_worktree;
#[path = "time/i18n.rs"]
mod i18n;
#[path = "time/kernel.rs"]
mod kernel;
#[path = "time/platform.rs"]
mod platform;

use std::process::ExitCode;

use lectern_bench::{Bench, Target};

const BENCHES: &[Bench] = &[
    agents::PARSE_THROUGHPUT,
    agents::APPEND_4K,
    agents::ZST_THROUGHPUT,
    agents::ZST_LOAD_DECODED,
    agents::DISCOVER_1000,
    agents::TURNS_CACHED_50MB,
    cache::GET_4K,
    cache::GET_MANY_50,
    cache::GET_1M,
    cache::OPEN_EXISTING,
    cache::OPEN_NEW,
    cache::PUT_4K,
    cache::BATCH_1000,
    cache::EVICT,
    cache::ZSTD_1M,
    code_intel::EXTRACT_THROUGHPUT,
    executor::SPAWN_10K,
    executor::WAKE_1000X100,
    fs_watch::CLASSIFY_BURST,
    fs_watch::REFLOG_APPEND,
    git::OPEN,
    git::OPEN_HANDLE,
    git::DIFF_50_COLD,
    git::DIFF_50_WARM,
    git::CHANGES_CLEAN,
    git::CHANGES_50,
    git::REFRESH_FULL,
    git::REFRESH_ONE,
    git::CHANGES_CAPPED,
    i18n::FRAME_STRINGS,
    kernel::SERVICE_GET,
    kernel::SERVICE_FOR_EACH,
    kernel::SERVICE_CURRENT,
    kernel::EMIT,
    kernel::EMIT_NESTED,
    platform::SAFE_STAT,
    platform::SAFE_READ_4K,
];

fn main() -> ExitCode {
    lectern_bench::main(Target::Time, env!("CARGO_TARGET_TMPDIR"), BENCHES)
}
