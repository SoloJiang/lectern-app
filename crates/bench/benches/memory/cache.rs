//! `cache/memory` (docs/design/cache.md「性能」): the resident growth of a
//! process that opens the app partition and two repository partitions and
//! reads 50 MiB from each.
//!
//! SQLite allocates its page cache and connections with the C allocator,
//! which `CountingAlloc` does not see, so this is measured in a child
//! process (this benchmark binary again, `CHILD`): its resident size before
//! the cache is created and after the reads, from `/proc/self/status`
//! (`VmRSS`) on Linux and `platform::process::phys_footprint` on macOS
//! (docs/design/bench.md「进程内基准」). A fresh process, so the reads cannot
//! reuse heap freed by the writes that built the fixture.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use lectern_bench::{Bench, BenchError, Ctx, Measured, Value};
use lectern_cache::testing::{Raw, noise};
use lectern_cache::{Budgets, Cache, Config, Key, Partition, Written};

/// Set to the cache root, it makes this binary the child of `cache/memory`.
pub const CHILD: &str = "LECTERN_BENCH_CACHE_MEMORY_ROOT";
/// The child's report: `MEMORY <before> <after> <connections>`.
const LINE: &str = "MEMORY ";
/// 64 KiB entries read from each partition: 50 MiB in measure mode.
const LEN: usize = 64 << 10;

fn cache(root: &Path) -> Cache {
    Cache::new(Config {
        root: root.to_path_buf(),
        budgets: Budgets::default(),
        ui_thread: None,
    })
}

/// The app partition and two repository partitions.
fn partitions(cache: &Cache) -> [Partition; 3] {
    let repo = |i: u32| cache.repo(&PathBuf::from(format!("/work/repo{i}/.git")));
    [cache.app(), repo(0), repo(1)]
}

fn key(i: u64) -> Key {
    Key::new().str("memory").u64(i)
}

/// The resident size of this process (also `terminal/*_footprint`).
pub fn resident() -> Result<u64, String> {
    #[cfg(target_os = "linux")]
    {
        let status = std::fs::read_to_string("/proc/self/status").map_err(|e| e.to_string())?;
        let kib = status
            .lines()
            .find_map(|l| l.strip_prefix("VmRSS:"))
            .and_then(|v| v.split_whitespace().next())
            .and_then(|v| v.parse::<u64>().ok())
            .ok_or("no VmRSS in /proc/self/status")?;
        Ok(kib << 10)
    }
    #[cfg(not(target_os = "linux"))]
    {
        lectern_platform::process::phys_footprint(std::process::id()).map_err(|e| e.to_string())
    }
}

/// The child: reads `entries` values from each partition under `root` and
/// prints its resident size before and after.
pub fn child(root: &Path, entries: u64) -> ExitCode {
    let run = || -> Result<String, String> {
        let before = resident()?;
        let cache = cache(root);
        for p in partitions(&cache) {
            for i in 0..entries {
                if p.get::<Raw>(&key(i)).is_none() {
                    return Err(format!("entry {i} was not found"));
                }
            }
        }
        let after = resident()?;
        Ok(format!(
            "{LINE}{before} {after} {}",
            cache.stats().connections
        ))
    };
    match run() {
        Ok(line) => {
            println!("{line}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("cache/memory child: {e}");
            ExitCode::FAILURE
        }
    }
}

/// The number of entries the child reads, passed as its only argument.
pub fn entries(arg: Option<String>) -> u64 {
    arg.and_then(|a| a.parse().ok()).unwrap_or(0)
}

pub const MEMORY: Bench = Bench {
    id: "cache/memory",
    run: memory,
};

fn memory(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let root = ctx.scratch().join("cache");
    let entries = ctx.scale(800, 16);
    {
        let cache = cache(&root);
        for p in partitions(&cache) {
            let mut batch = p.batch();
            for i in 0..entries {
                batch.put(&key(i), &Raw(noise(i, LEN)));
                if batch.bytes() > 8 << 20 {
                    commit(std::mem::replace(&mut batch, p.batch()))?;
                }
            }
            commit(batch)?;
        }
    }
    let exe = std::env::current_exe()?;
    let mut grown = Vec::new();
    for _ in 0..ctx.scale(5, 1) {
        let out = Command::new(&exe)
            .arg(entries.to_string())
            .env(CHILD, &root)
            .output()?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        let line = stdout.lines().find_map(|l| l.strip_prefix(LINE));
        let (Some(line), true) = (line, out.status.success()) else {
            return Err(BenchError::new(format!(
                "the child failed ({}): {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        };
        let values: Vec<u64> = line
            .split(' ')
            .map(|v| v.parse())
            .collect::<Result<_, _>>()?;
        let [before, after, connections] = values[..] else {
            return Err(BenchError::new(format!("the child printed {line:?}")));
        };
        if connections != 6 {
            return Err(BenchError::new(format!(
                "the child had {connections} connections, not 6"
            )));
        }
        grown.push(after.saturating_sub(before));
    }
    Ok(vec![Measured::new("cache/memory", Value::Bytes(grown))])
}

fn commit(batch: lectern_cache::Batch) -> Result<(), BenchError> {
    match batch.commit() {
        Written::Committed => Ok(()),
        other => Err(BenchError::debug("fill", other)),
    }
}
