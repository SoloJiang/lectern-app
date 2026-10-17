//! `cache/*` (docs/design/cache.md「性能」): reads and writes on an open
//! partition, and opening a partition, with the entry types and values of
//! `lectern_cache::testing`. The system file cache is warm.

use std::cell::Cell;
use std::hint::black_box;
use std::path::{Path, PathBuf};

use lectern_bench::{Bench, BenchError, Ctx, Measured, SampleConfig, Value};
use lectern_cache::testing::{Raw, Zstd, noise, text};
use lectern_cache::{Budgets, Cache, Config, Key, MAX_BATCH, Partition, Written};

fn cache(root: &Path) -> Cache {
    Cache::new(Config {
        root: root.to_path_buf(),
        budgets: Budgets::default(),
        ui_thread: None,
    })
}

/// A repository partition's `common_dir`. Only names the partition: the
/// cache never reads it.
fn repo(i: u64) -> PathBuf {
    PathBuf::from(format!("/work/repo{i}/.git"))
}

fn key(space: &str, i: u64) -> Key {
    Key::new().str(space).u64(i)
}

/// Records the first write that was not committed; checked after sampling.
struct Writes(Cell<Option<Written>>);

impl Writes {
    fn new() -> Writes {
        Writes(Cell::new(None))
    }

    fn check(&self, written: Written) -> Written {
        if written != Written::Committed && self.0.get().is_none() {
            self.0.set(Some(written));
        }
        written
    }

    fn result(&self) -> Result<(), BenchError> {
        match self.0.get() {
            None => Ok(()),
            Some(written) => Err(BenchError::debug("write", written)),
        }
    }
}

/// `n` entries of `len` noise bytes under `space`, in batches of at most
/// 1000 entries and half of `MAX_BATCH`.
fn fill(p: &Partition, space: &str, n: u64, len: usize) -> Result<(), BenchError> {
    let per_batch = (MAX_BATCH / 2 / len as u64).clamp(1, 1000);
    for start in (0..n).step_by(per_batch as usize) {
        let mut batch = p.batch();
        for i in start..(start + per_batch).min(n) {
            batch.put(&key(space, i), &Raw(noise(i, len)));
        }
        match batch.commit() {
            Written::Committed => {}
            other => return Err(BenchError::debug("fill", other)),
        }
    }
    Ok(())
}

/// The first `get` on a new partition: creates the directory, the file and
/// the schema. Each sample is a new cache on a new partition.
pub const OPEN_NEW: Bench = Bench {
    id: "cache/open_new",
    run: open_new,
};

fn open_new(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let root = ctx.scratch().join("cache");
    let mut next = 0;
    let time = ctx.time_with_setup(
        || {
            next += 1;
            let cache = cache(&root);
            let partition = cache.repo(&repo(next));
            (cache, partition)
        },
        // The cache is returned so that closing it is not timed.
        |(cache, partition)| (partition.get::<Raw>(&key("k", 0)), cache),
    );
    Ok(vec![Measured::new("cache/open_new", Value::Time(time))])
}

/// The first `get` on an existing partition, as in the next run: a new
/// cache on the same root each sample.
pub const OPEN_EXISTING: Bench = Bench {
    id: "cache/open_existing",
    run: open_existing,
};

fn open_existing(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let root = ctx.scratch().join("cache");
    let partitions = ctx.scale(50, 3);
    {
        let cache = cache(&root);
        for i in 0..partitions {
            fill(&cache.repo(&repo(i)), "k", 1000, 1 << 10)?;
        }
    }
    let mut next = 0;
    let found = Cell::new(true);
    let time = ctx.time_with_setup(
        || {
            next += 1;
            let cache = cache(&root);
            let partition = cache.repo(&repo(next % partitions));
            (cache, partition)
        },
        |(cache, partition)| {
            let value = partition.get::<Raw>(&key("k", 0));
            found.set(found.get() && value.is_some());
            (value, cache)
        },
    );
    if !found.get() {
        return Err(BenchError::new("an existing entry was not found"));
    }
    Ok(vec![Measured::new(
        "cache/open_existing",
        Value::Time(time),
    )])
}

/// Caps the samples of a write benchmark, so that what it writes stays far
/// below the partition's 512 MiB budget and no sample pays for eviction
/// (`cache/evict_16m` measures that).
fn writes_at_most(ctx: &mut Ctx, samples: usize) {
    ctx.config(SampleConfig {
        max_samples: samples,
        ..SampleConfig::default()
    });
}

/// A partition opened by a first read.
fn open(ctx: &Ctx) -> (Cache, Partition) {
    let cache = cache(&ctx.scratch().join("cache"));
    let partition = cache.repo(Path::new("/work/lectern/.git"));
    let _ = partition.get::<Raw>(&key("open", 0));
    (cache, partition)
}

/// `put` of one 4 KiB value under a new key.
pub const PUT_4K: Bench = Bench {
    id: "cache/put_4k",
    run: put_4k,
};

fn put_4k(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let (_cache, p) = open(ctx);
    let value = Raw(noise(1, 4 << 10));
    writes_at_most(ctx, 200);
    let writes = Writes::new();
    let mut next = 0;
    let time = ctx.time(|| {
        next += 1;
        writes.check(p.put(&key("put", next), black_box(&value)))
    });
    writes.result()?;
    Ok(vec![Measured::new("cache/put_4k", Value::Time(time))])
}

/// One batch of 1000 values of 1 KiB under new keys.
pub const BATCH_1000: Bench = Bench {
    id: "cache/batch_1000",
    run: batch_1000,
};

fn batch_1000(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let (_cache, p) = open(ctx);
    writes_at_most(ctx, 60);
    let writes = Writes::new();
    let mut round = 0u64;
    let time = ctx.time_with_setup(
        || {
            round += 1;
            (0..1000u64)
                .map(|i| (key("batch", round * 1000 + i), Raw(noise(i, 1 << 10))))
                .collect::<Vec<_>>()
        },
        |values| {
            let mut batch = p.batch();
            for (k, v) in &values {
                batch.put(k, v);
            }
            writes.check(batch.commit())
        },
    );
    writes.result()?;
    Ok(vec![Measured::new("cache/batch_1000", Value::Time(time))])
}

/// `get` of one 4 KiB value.
pub const GET_4K: Bench = Bench {
    id: "cache/get_4k",
    run: get_4k,
};

fn get_4k(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let (_cache, p) = open(ctx);
    let n = ctx.scale(2000, 20);
    fill(&p, "get", n, 4 << 10)?;
    let mut next = 0;
    let time = ctx.time(|| {
        next += 1;
        p.get::<Raw>(black_box(&key("get", next % n)))
    });
    if p.get::<Raw>(&key("get", 0)).is_none() {
        return Err(BenchError::new("a stored entry was not found"));
    }
    Ok(vec![Measured::new("cache/get_4k", Value::Time(time))])
}

/// `get_many` of 50 values of 1 KiB, as the hunks of 50 files.
pub const GET_MANY_50: Bench = Bench {
    id: "cache/get_many_50",
    run: get_many_50,
};

fn get_many_50(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let (_cache, p) = open(ctx);
    let n = ctx.scale(30_000, 100);
    fill(&p, "many", n, 1 << 10)?;
    let rounds = n / 50;
    let keys: Vec<Vec<Key>> = (0..rounds)
        .map(|r| (0..50).map(|i| key("many", r * 50 + i)).collect())
        .collect();
    if !p.get_many::<Raw>(&keys[0]).iter().all(Option::is_some) {
        return Err(BenchError::new("a stored entry was not found"));
    }
    let mut next = 0;
    let time = ctx.time(|| {
        next += 1;
        p.get_many::<Raw>(black_box(&keys[next % keys.len()]))
    });
    Ok(vec![Measured::new("cache/get_many_50", Value::Time(time))])
}

/// `get` of one 1 MiB value, as a session's turn index.
pub const GET_1M: Bench = Bench {
    id: "cache/get_1m",
    run: get_1m,
};

fn get_1m(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let (_cache, p) = open(ctx);
    let n = ctx.scale(20, 2);
    fill(&p, "large", n, 1 << 20)?;
    if p.get::<Raw>(&key("large", 0)).is_none() {
        return Err(BenchError::new("a stored entry was not found"));
    }
    let mut next = 0;
    let time = ctx.time(|| {
        next += 1;
        p.get::<Raw>(black_box(&key("large", next % n)))
    });
    Ok(vec![Measured::new("cache/get_1m", Value::Time(time))])
}

/// `put` and `get` of 1 MiB of text in a `Zstd` namespace: compression and
/// decompression. Record only.
pub const ZSTD_1M: Bench = Bench {
    id: "cache/zstd_1m",
    run: zstd_1m,
};

fn zstd_1m(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let (_cache, p) = open(ctx);
    let value = Zstd(text(1 << 20));
    let writes = Writes::new();
    let mut next = 0;
    let put = ctx.time(|| {
        next += 1;
        writes.check(p.put(&key("zstd", next), black_box(&value)))
    });
    writes.result()?;
    let stored = next.max(1);
    if p.get::<Zstd>(&key("zstd", 1)).as_ref() != Some(&value) {
        return Err(BenchError::new("the compressed value did not read back"));
    }
    let mut at = 0;
    let get = ctx.time(|| {
        at += 1;
        p.get::<Zstd>(black_box(&key("zstd", at % stored + 1)))
    });
    Ok(vec![
        Measured::new("cache/zstd_1m/put", Value::Time(put)),
        Measured::new("cache/zstd_1m/get", Value::Time(get)),
    ])
}
