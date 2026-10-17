//! `perf_smoke_cache`: the read and write items of the design's「性能」table,
//! measured in release until `bench` has the `cache/*` benchmarks.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::support::{Blob, Packed, config, key, noise};
use crate::{Budgets, Cache, Key, Written};

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

/// Run with `cargo test --release -p lectern-cache -- --ignored perf_smoke --nocapture`.
#[test]
#[ignore = "timing; run in release with `-- --ignored perf_smoke`"]
fn perf_smoke_cache() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("cache");
    let new_cache = || Cache::new(config(&root, Budgets::default()));
    let mut ok = true;

    // Opening: a new partition, and an existing one in the next run.
    let mut open_new = Timing::new("open_new", Duration::from_millis(10));
    {
        let cache = new_cache();
        for i in 0..50 {
            let p = cache.repo(&repo(i));
            open_new.time(|| p.get::<Blob>(&key(0)));
            let mut batch = p.batch();
            for k in 0..1000 {
                batch.put(&key(k), &Blob(noise(k, 1 << 10)));
            }
            assert_eq!(batch.commit(), Written::Committed);
        }
    }
    ok &= open_new.report();
    let mut open_existing = Timing::new("open_existing", Duration::from_millis(5));
    {
        let cache = new_cache();
        for i in 0..50 {
            let p = cache.repo(&repo(i));
            assert!(open_existing.time(|| p.get::<Blob>(&key(0))).is_some());
        }
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
