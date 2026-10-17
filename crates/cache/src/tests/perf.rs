//! `perf_smoke_cache`: the items of the design's「性能」table, measured in
//! release until `bench` has the `cache/*` benchmarks.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use super::support::{Blob, Packed, config, key, noise};
use crate::{Budgets, Cache, Key, MAX_BATCH, Partition, Written};

struct Timing {
    name: &'static str,
    budget: Duration,
    samples: Vec<Duration>,
}

impl Timing {
    fn new(name: &'static str, budget: Duration) -> Timing {
        Timing {
            name,
            budget,
            samples: Vec::new(),
        }
    }

    fn time<T>(&mut self, f: impl FnOnce() -> T) -> T {
        let start = Instant::now();
        let out = f();
        self.samples.push(start.elapsed());
        out
    }

    fn report(mut self) -> bool {
        self.samples.sort();
        let at = |q: f64| self.samples[((self.samples.len() - 1) as f64 * q) as usize];
        let (median, p95, max) = (at(0.5), at(0.95), at(1.0));
        let ok = median <= self.budget;
        println!(
            "{:<16} n={:<5} median {:>9.1?}  p95 {:>9.1?}  max {:>9.1?}  budget {:>7.1?}  {}",
            self.name,
            self.samples.len(),
            median,
            p95,
            max,
            self.budget,
            if ok { "ok" } else { "OVER" }
        );
        ok
    }
}

fn repo(i: usize) -> PathBuf {
    PathBuf::from(format!("/work/repo{i}/.git"))
}

/// `n` entries of `len` bytes under `prefix`, in batches within `MAX_BATCH`.
fn fill(p: &Partition, prefix: &str, n: u64, len: usize) {
    let mut batch = p.batch();
    for i in 0..n {
        if batch.bytes() + len as u64 + 64 > MAX_BATCH {
            assert_eq!(batch.commit(), Written::Committed);
            batch = p.batch();
        }
        batch.put(&Key::new().str(prefix).u64(i), &Blob(noise(i, len)));
    }
    assert_eq!(batch.commit(), Written::Committed);
}

const MIB: u64 = 1 << 20;
/// The root of `perf_memory_child`.
const MEMORY_ROOT: &str = "LECTERN_CACHE_PERF_MEMORY_ROOT";
/// Entries of 64 KiB read from each partition: 50 MiB.
const MEMORY_ENTRIES: u64 = 800;

/// Resident set size in bytes (`VmRSS`); Linux only.
fn rss() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("VmRSS:"))?;
    let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kib << 10)
}

/// The child process of `cache/memory`: in a fresh process, opens the app
/// partition and two project partitions, reads 50 MiB from each and prints
/// the resident growth. Does nothing unless the parent set `MEMORY_ROOT`.
#[test]
fn perf_memory_child() {
    let Some(root) = std::env::var_os(MEMORY_ROOT) else {
        return;
    };
    let before = rss().expect("VmRSS");
    let cache = Cache::new(config(root.as_ref(), Budgets::default()));
    for p in [cache.app(), cache.repo(&repo(0)), cache.repo(&repo(1))] {
        for i in 0..MEMORY_ENTRIES {
            let k = Key::new().str("memory").u64(i);
            assert!(p.get::<Blob>(&k).is_some());
        }
    }
    let after = rss().expect("VmRSS");
    println!("MEMORY {before} {after} {}", cache.stats().connections);
}

/// `cache/memory`: the resident growth of `perf_memory_child`, with the
/// partitions written beforehand by this process. Budget 8 MiB.
fn memory(root: &Path) -> bool {
    if rss().is_none() {
        println!("memory           not measured: no /proc/self/status");
        return true;
    }
    {
        let cache = Cache::new(config(root, Budgets::default()));
        for p in [cache.app(), cache.repo(&repo(0)), cache.repo(&repo(1))] {
            fill(&p, "memory", MEMORY_ENTRIES, 64 << 10);
        }
    }
    let mut grown = Vec::new();
    for _ in 0..5 {
        let out = Command::new(std::env::current_exe().unwrap())
            .args([
                "tests::perf::perf_memory_child",
                "--exact",
                "--test-threads=1",
                "--nocapture",
            ])
            .env(MEMORY_ROOT, root)
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        let stdout = String::from_utf8_lossy(&out.stdout);
        let line = stdout
            .lines()
            .find_map(|l| l.split_once("MEMORY ").map(|(_, rest)| rest))
            .unwrap_or_else(|| panic!("the child's MEMORY line: {stdout}"));
        // libtest may print the test's name and result on the same line.
        let v: Vec<u64> = line
            .split(' ')
            .take(3)
            .map(|n| n.trim().parse().unwrap())
            .collect();
        assert_eq!(v[2], 6, "connections");
        grown.push(v[1].saturating_sub(v[0]));
    }
    grown.sort();
    let median = grown[grown.len() / 2];
    let ok = median <= 8 * MIB;
    println!(
        "memory           n=5     median {:.2} MiB  max {:.2} MiB  budget 8 MiB  {}",
        median as f64 / MIB as f64,
        grown[grown.len() - 1] as f64 / MIB as f64,
        if ok { "ok" } else { "OVER" }
    );
    ok
}

/// `cache/evict`: a small write into a partition over its budget, which
/// evicts `EVICT_MIN` of `len` byte entries, and returns `VACUUM_PAGES` free
/// pages once more than 32 MiB is free. Filled under the default budget;
/// the next run's budget is 32 MiB.
fn evict(root: &Path, name: &'static str, len: usize) -> bool {
    {
        let cache = Cache::new(config(root, Budgets::default()));
        fill(
            &cache.repo(&repo(0)),
            "evict",
            (160 * MIB) / (len as u64 + 80),
            len,
        );
    }
    let cache = Cache::new(config(
        root,
        Budgets {
            project: 32 * MIB,
            ..Budgets::default()
        },
    ));
    let p = cache.repo(&repo(0));
    assert!(p.get::<Blob>(&Key::new().str("evict").u64(0)).is_some());
    let mut timing = Timing::new(name, Duration::from_millis(30));
    for i in 0.. {
        let before = cache.stats().evicted_bytes;
        let k = Key::new().str("write").u64(i);
        assert_eq!(
            timing.time(|| p.put(&k, &Blob(vec![1]))),
            Written::Committed
        );
        if cache.stats().evicted_bytes - before < crate::db::EVICT_MIN {
            // The last one got within the budget.
            timing.samples.pop();
            break;
        }
    }
    timing.report()
}

/// Informational: a 16 MiB batch of 1 KiB entries, below the budget and
/// into a partition over it (evicting 16 MiB in the same transaction).
fn evict_16m(root: &Path) {
    let batch = |p: &Partition, round: u64| {
        let mut batch = p.batch();
        for i in 0..15_000 {
            let k = Key::new().str("batch").u64(round * 100_000 + i);
            batch.put(&k, &Blob(noise(i, 1 << 10)));
        }
        batch.commit()
    };
    let mut below = Timing::new("batch_16m", Duration::from_secs(1));
    let mut over = Timing::new("evict_16m", Duration::from_secs(1));
    {
        let cache = Cache::new(config(root, Budgets::default()));
        let p = cache.repo(&repo(0));
        for round in 0..10 {
            assert_eq!(below.time(|| batch(&p, round)), Written::Committed);
        }
    }
    let cache = Cache::new(config(
        root,
        Budgets {
            project: 32 * MIB,
            ..Budgets::default()
        },
    ));
    let p = cache.repo(&repo(0));
    for round in 10..20 {
        let before = cache.stats().evicted_bytes;
        assert_eq!(over.time(|| batch(&p, round)), Written::Committed);
        assert!(cache.stats().evicted_bytes - before >= 15 * MIB);
    }
    below.report();
    over.report();
}

/// Opening a project partition while two are open: the least recently used
/// one is checkpointed and closed in the same call. Informational.
fn swap_out(root: &Path) {
    let cache = Cache::new(config(root, Budgets::default()));
    let mut timing = Timing::new("swap_out", Duration::from_secs(1));
    for i in 0..50 {
        let p = cache.repo(&repo(i));
        if i >= 2 {
            timing.time(|| p.get::<Blob>(&key(0)));
        } else {
            let _ = p.get::<Blob>(&key(0));
        }
        let mut batch = p.batch();
        for k in 0..1000 {
            batch.put(&key(k), &Blob(noise(k, 1 << 10)));
        }
        assert_eq!(batch.commit(), Written::Committed);
    }
    timing.report();
}

/// Run with `cargo test --release -p lectern-cache -- --ignored perf_smoke --nocapture`.
#[test]
#[ignore = "timing; run in release with `-- --ignored perf_smoke`"]
fn perf_smoke_cache() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("cache");
    let new_cache = || Cache::new(config(&root, Budgets::default()));
    let mut ok = memory(&dir.path().join("memory"));
    ok &= evict(&dir.path().join("evict_1k"), "evict 1k", 1 << 10);
    ok &= evict(&dir.path().join("evict_64k"), "evict 64k", 64 << 10);
    evict_16m(&dir.path().join("evict_16m"));
    swap_out(&dir.path().join("swap_out"));

    // Opening: a new partition, and an existing one in the next run, each
    // the first of its run (with the directory sweep, without swapping out).
    let mut open_new = Timing::new("open_new", Duration::from_millis(10));
    for i in 0..50 {
        let cache = new_cache();
        let p = cache.repo(&repo(i));
        open_new.time(|| p.get::<Blob>(&key(0)));
        let mut batch = p.batch();
        for k in 0..1000 {
            batch.put(&key(k), &Blob(noise(k, 1 << 10)));
        }
        assert_eq!(batch.commit(), Written::Committed);
    }
    ok &= open_new.report();
    let mut open_existing = Timing::new("open_existing", Duration::from_millis(5));
    for i in 0..50 {
        let cache = new_cache();
        let p = cache.repo(&repo(i));
        assert!(open_existing.time(|| p.get::<Blob>(&key(0))).is_some());
    }
    ok &= open_existing.report();

    let cache = new_cache();
    let p = cache.repo(Path::new("/work/lectern/.git"));

    let mut put_4k = Timing::new("put_4k", Duration::from_millis(1));
    for i in 0..2000 {
        let value = Blob(noise(i, 4 << 10));
        let k = Key::new().str("put").u64(i);
        assert_eq!(put_4k.time(|| p.put(&k, &value)), Written::Committed);
    }
    ok &= put_4k.report();

    let mut batch_1000 = Timing::new("batch_1000", Duration::from_millis(15));
    for round in 0..30u64 {
        let values: Vec<(Key, Blob)> = (0..1000u64)
            .map(|i| {
                (
                    Key::new().str("batch").u64(round * 1000 + i),
                    Blob(noise(i, 1 << 10)),
                )
            })
            .collect();
        let written = batch_1000.time(|| {
            let mut batch = p.batch();
            for (k, v) in &values {
                batch.put(k, v);
            }
            batch.commit()
        });
        assert_eq!(written, Written::Committed);
    }
    ok &= batch_1000.report();

    let mut get_4k = Timing::new("get_4k", Duration::from_micros(50));
    for i in 0..2000 {
        let k = Key::new().str("put").u64(i);
        assert!(get_4k.time(|| p.get::<Blob>(&k)).is_some());
    }
    ok &= get_4k.report();

    let mut get_many_50 = Timing::new("get_many_50", Duration::from_millis(1));
    for round in 0..200u64 {
        let keys: Vec<Key> = (0..50)
            .map(|i| Key::new().str("batch").u64((round * 50 + i) % 30_000))
            .collect();
        let found = get_many_50.time(|| p.get_many::<Blob>(&keys));
        assert!(found.iter().all(Option::is_some));
    }
    ok &= get_many_50.report();

    let mut get_1m = Timing::new("get_1m", Duration::from_millis(2));
    for i in 0..20 {
        let k = Key::new().str("large").u64(i);
        assert_eq!(p.put(&k, &Blob(noise(i, 1 << 20))), Written::Committed);
    }
    for round in 0..200 {
        let k = Key::new().str("large").u64(round % 20);
        assert!(get_1m.time(|| p.get::<Blob>(&k)).is_some());
    }
    ok &= get_1m.report();

    // Informational: a namespace that picks Zstd checks its own budget.
    const LINE: &[u8] = b"{\"type\":\"assistant\",\"text\":\"lorem ipsum\"}\n";
    let text: Vec<u8> = (0..1 << 20).map(|i: usize| LINE[i % LINE.len()]).collect();
    let mut zstd_put = Timing::new("zstd_1m put", Duration::from_secs(1));
    let mut zstd_get = Timing::new("zstd_1m get", Duration::from_secs(1));
    for i in 0..20 {
        let k = Key::new().str("zstd").u64(i);
        assert_eq!(
            zstd_put.time(|| p.put(&k, &Packed(text.clone()))),
            Written::Committed
        );
        assert!(zstd_get.time(|| p.get::<Packed>(&k)).is_some());
    }
    zstd_put.report();
    zstd_get.report();

    assert!(ok, "an item is over its budget");
}
