//! `cache/*` (docs/design/cache.md「性能」): reads and writes on an open
//! partition, opening a partition, a write that evicts, and custom tables,
//! with the entry types, tables and values of `lectern_cache::testing`. The
//! system file cache is warm. `cache/memory` is in
//! `benches/memory/cache.rs`.

use std::cell::Cell;
use std::hint::black_box;
use std::path::{Path, PathBuf};

use lectern_bench::{Bench, BenchError, Ctx, Measured, SampleConfig, Value};
use lectern_cache::testing::{ManualClock, Raw, Zstd, noise, tags, text};
use lectern_cache::{Budgets, Cache, Config, Key, MAX_BATCH, Param, Partition, TableSet, Written};

fn cache(root: &Path) -> Cache {
    cache_with(root, Budgets::default())
}

fn cache_with(root: &Path, budgets: Budgets) -> Cache {
    Cache::new(Config {
        root: root.to_path_buf(),
        budgets,
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
/// (`cache/evict` measures that).
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

const MIB: u64 = 1 << 20;

/// A small `put` into a partition over its budget: the write transaction
/// also evicts 1 MiB of the oldest 1 KiB entries and, once more than 32 MiB
/// of pages are free, returns 2 MiB of them to the file system. Filled under
/// the default budget; the next run's budget is 32 MiB, so every sample
/// evicts.
pub const EVICT: Bench = Bench {
    id: "cache/evict",
    run: evict,
};

fn evict(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let root = ctx.scratch().join("cache");
    let len = 1 << 10;
    let (filled, budget) = ctx.scale((320 * MIB, 32 * MIB), (3 * MIB, MIB));
    fill(
        &cache(&root).repo(&repo(0)),
        "evict",
        filled / (len as u64 + 80),
        len,
    )?;
    let cache = cache_with(
        &root,
        Budgets {
            project: budget,
            ..Budgets::default()
        },
    );
    let p = cache.repo(&repo(0));
    if p.get::<Raw>(&key("evict", 0)).is_none() {
        return Err(BenchError::new("a stored entry was not found"));
    }
    // Each call evicts 1 MiB: stay well within the 288 MiB over the budget,
    // warm-up included.
    ctx.config(SampleConfig {
        max_samples: 150,
        warmup_time: std::time::Duration::from_millis(50),
        ..SampleConfig::default()
    });
    let writes = Writes::new();
    let value = Raw(vec![1]);
    let mut next = 0;
    let time = ctx.time_with_setup(
        || {
            next += 1;
            key("write", next)
        },
        |k| writes.check(p.put(&k, black_box(&value))),
    );
    writes.result()?;
    // Still over the budget: every call evicted.
    let left = cache
        .stats()
        .partitions
        .iter()
        .map(|s| s.bytes)
        .max()
        .unwrap_or(0);
    if left <= budget {
        return Err(BenchError::new(
            "the partition got within its budget while sampling",
        ));
    }
    Ok(vec![Measured::new("cache/evict", Value::Time(time))])
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

/// Units of `tags` from `start`, 64 per write (code_intel's batch).
fn fill_tags(set: &TableSet, start: u64, n: u64) -> Result<(), BenchError> {
    for first in (start..start + n).step_by(64) {
        let mut w = set.write();
        for i in first..(first + 64).min(start + n) {
            tags::add(&mut w, i);
        }
        match w.commit() {
            Written::Committed => {}
            other => return Err(BenchError::debug("fill", other)),
        }
    }
    Ok(())
}

/// One `TableWrite` of 64 new units with their rows (20 definitions and 40
/// references each, about 100 KB of parameters): code_intel's batch. The
/// write is recorded in the setup; the commit is timed.
pub const TABLES_WRITE: Bench = Bench {
    id: "cache/tables_write",
    run: tables_write,
};

fn tables_write(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let (_cache, p) = open(ctx);
    let set = p.tables(&tags::TABLES);
    // Creates the tables outside the samples.
    if set.read(|_| ()).is_none() {
        return Err(BenchError::new("the tables were not created"));
    }
    // 200 writes of about 270 KB declared: far below the quota.
    writes_at_most(ctx, 200);
    let writes = Writes::new();
    let mut next = 0u64;
    let time = ctx.time_with_setup(
        || {
            let mut w = set.write();
            for _ in 0..64 {
                tags::add(&mut w, next);
                next += 1;
            }
            w
        },
        |w| writes.check(w.commit()),
    );
    writes.result()?;
    Ok(vec![Measured::new("cache/tables_write", Value::Time(time))])
}

/// One `read` running one query by name over 10 000 units (200 000
/// definitions), finding one row: the cache's share of a lookup.
pub const TABLES_READ: Bench = Bench {
    id: "cache/tables_read",
    run: tables_read,
};

fn tables_read(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let (_cache, p) = open(ctx);
    let set = p.tables(&tags::TABLES);
    let n = ctx.scale(10_000, 128);
    fill_tags(&set, 0, n)?;
    let lookup = |i: u64| {
        let name = tags::def_name(i % n, i % tags::DEFS);
        set.read(|r| {
            let mut found = 0;
            let ok = r.query(tags::DEFS_BY_NAME, &[Param::Text(&name)], |row| {
                found += usize::from(row.bytes(2).is_some());
                true
            });
            ok.then_some(found)
        })
        .flatten()
    };
    if lookup(1) != Some(1) {
        return Err(BenchError::new("a stored definition was not found"));
    }
    let mut next = 0;
    let time = ctx.time(|| {
        next += 7919;
        lookup(black_box(next))
    });
    Ok(vec![Measured::new("cache/tables_read", Value::Time(time))])
}

/// `touch` of 5000 units whose access times are over an hour old: 4096 are
/// written back in the call (the pending set is full), the rest wait for the
/// next write, which the setup of the next sample makes.
pub const TABLES_TOUCH: Bench = Bench {
    id: "cache/tables_touch",
    run: tables_touch,
};

fn tables_touch(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let clock = ManualClock::new(std::time::SystemTime::now());
    let cache = Cache::with_clock(
        Config {
            root: ctx.scratch().join("cache"),
            budgets: Budgets::default(),
            ui_thread: None,
        },
        clock.clone(),
    );
    let p = cache.repo(Path::new("/work/lectern/.git"));
    let set = p.tables(&tags::TABLES);
    let n = 5000;
    fill_tags(&set, 0, n)?;
    let units = set
        .read(|r| {
            let mut units = Vec::new();
            let ok = r.query(tags::ALL_UNITS, &[], |row| {
                units.extend(row.unit(0));
                true
            });
            ok.then_some(units)
        })
        .flatten()
        .unwrap_or_default();
    if units.len() as u64 != n {
        return Err(BenchError::new("the units were not all found"));
    }
    writes_at_most(ctx, 100);
    let writes = Writes::new();
    let mut next = 0;
    let time = ctx.time_with_setup(
        || {
            // Writes back what the last sample left, then ages everything.
            next += 1;
            let _ = writes.check(p.put(&key("touch", next), &Raw(vec![1])));
            clock.advance(std::time::Duration::from_secs(2 * 60 * 60));
        },
        |()| set.touch(black_box(&units)),
    );
    writes.result()?;
    Ok(vec![Measured::new("cache/tables_touch", Value::Time(time))])
}

/// `cache/tables_write`'s batch of 64 units into a namespace over its
/// quota: the write also evicts as many bytes of the oldest units, with
/// their rows (about 64 units, 4 000 rows). Filled through `tags::TABLES`
/// to 12 MB; written through `tags::SMALL_QUOTA`, the same tables with an
/// 8 MiB quota. Each sample deletes what it writes, so every sample evicts.
pub const TABLES_EVICT: Bench = Bench {
    id: "cache/tables_evict",
    run: tables_evict,
};

fn tables_evict(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let (_cache, p) = open(ctx);
    let filled = 2_800;
    fill_tags(&p.tables(&tags::TABLES), 0, filled)?;
    let set = p.tables(&tags::SMALL_QUOTA);
    writes_at_most(ctx, 200);
    let writes = Writes::new();
    let before = set.evictions();
    let mut next = filled;
    let time = ctx.time_with_setup(
        || {
            let mut w = set.write();
            for _ in 0..64 {
                tags::add(&mut w, next);
                next += 1;
            }
            w
        },
        |w| writes.check(w.commit()),
    );
    writes.result()?;
    if set.evictions() == before {
        return Err(BenchError::new("nothing was evicted"));
    }
    Ok(vec![Measured::new("cache/tables_evict", Value::Time(time))])
}
