//! A full disk, another process holding the lock, concurrent calls and a
//! crashing process (`disk_full_*`, `busy_*`, `concurrency_*`, `crash_*`).

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

use super::support::{Blob, Fixture, config, files, key, meta_bytes, noise, raw, sum_sizes};
use crate::{Budgets, Cache, Dropped, PartitionKind, PartitionState, Written};

// disk_full_*

#[test]
fn disk_full_drops_writes_pauses_and_lowers_the_budget() {
    let f = Fixture::new();
    let p = f.repo();
    let mut batch = p.batch();
    for i in 0..100 {
        batch.put(&key(i), &Blob(noise(i, 16 << 10)));
    }
    assert_eq!(batch.commit(), Written::Committed);
    let conn = raw(p.file());
    let full_at = meta_bytes(&conn);

    let transactions = Arc::new(AtomicUsize::new(0));
    let counter = transactions.clone();
    p.set_write_hook(Some(Arc::new(move || {
        counter.fetch_add(1, Ordering::Relaxed);
    })));
    // SQLite keeps the limit at the current size: nothing can grow.
    p.set_max_page_count(Some(1));
    assert_eq!(
        p.put(&key(1000), &Blob(noise(1000, 64 << 10))),
        Written::Dropped(Dropped::DiskFull)
    );
    // Reads go on.
    assert_eq!(p.get::<Blob>(&key(3)), Some(Blob(noise(3, 16 << 10))));
    assert_eq!(p.get::<Blob>(&key(1000)), None);
    let PartitionState::WritesSuspended { until } = state(&f) else {
        panic!("writes suspended: {:?}", state(&f));
    };
    assert_eq!(until, f.clock.now() + Duration::from_secs(60));

    // Within 60 seconds nothing is tried, even with room again.
    p.set_max_page_count(None);
    f.clock.advance(Duration::from_secs(59));
    let before = transactions.load(Ordering::Relaxed);
    assert_eq!(
        p.put(&key(1001), &Blob(vec![1])),
        Written::Dropped(Dropped::DiskFull)
    );
    let mut batch = p.batch();
    batch.put(&key(1002), &Blob(vec![2]));
    assert_eq!(batch.commit(), Written::Dropped(Dropped::DiskFull));
    assert_eq!(transactions.load(Ordering::Relaxed), before);
    assert_eq!(f.cache.stats().dropped(Dropped::DiskFull), 3);

    // Then the next write succeeds and evicts to 90% of 75% of the size at
    // the time the disk was full.
    f.clock.advance(Duration::from_secs(2));
    assert_eq!(p.put(&key(1001), &Blob(vec![1])), Written::Committed);
    assert_eq!(state(&f), PartitionState::Open);
    let after = meta_bytes(&conn);
    assert!(after <= full_at * 3 / 4 * 9 / 10, "{after} of {full_at}");
    assert!(after > full_at / 2, "{after} of {full_at}");
    assert_eq!(after, sum_sizes(&conn));
    assert_eq!(p.get::<Blob>(&key(1001)), Some(Blob(vec![1])));
    // The oldest entries went first.
    assert_eq!(p.get::<Blob>(&key(0)), None);
    assert_eq!(p.get::<Blob>(&key(99)), Some(Blob(noise(99, 16 << 10))));
    assert!(f.cache.stats().evicted_bytes > 0);
}

fn state(f: &Fixture) -> PartitionState {
    f.cache
        .stats()
        .partitions
        .iter()
        .find(|s| s.kind == PartitionKind::Repo)
        .unwrap()
        .state
}

#[test]
fn disk_full_in_the_middle_of_a_batch_writes_nothing() {
    let f = Fixture::new();
    let p = f.repo();
    assert_eq!(p.put(&key(0), &Blob(vec![0])), Written::Committed);
    p.set_max_page_count(Some(1));
    let mut batch = p.batch();
    for i in 1..=50 {
        batch.put(&key(i), &Blob(noise(i, 64 << 10)));
    }
    assert_eq!(batch.commit(), Written::Dropped(Dropped::DiskFull));
    for i in 1..=50 {
        assert_eq!(p.get::<Blob>(&key(i)), None);
    }
    assert_eq!(p.get::<Blob>(&key(0)), Some(Blob(vec![0])));
    let conn = raw(p.file());
    assert_eq!(meta_bytes(&conn), sum_sizes(&conn));
}

// busy_*

#[test]
fn busy_write_lock_held_elsewhere_drops_writes_but_not_reads() {
    let f = Fixture::new();
    let p = f.repo();
    assert_eq!(p.put(&key(1), &Blob(vec![1])), Written::Committed);
    // Another process holds the write lock.
    let other = raw(p.file());
    other.execute_batch("BEGIN IMMEDIATE").unwrap();
    let start = Instant::now();
    assert_eq!(
        p.put(&key(2), &Blob(vec![2])),
        Written::Dropped(Dropped::Busy)
    );
    let waited = start.elapsed();
    assert!(waited >= Duration::from_millis(90), "{waited:?}");
    assert!(waited < Duration::from_millis(1000), "{waited:?}");
    assert_eq!(p.get::<Blob>(&key(1)), Some(Blob(vec![1])));
    other.execute_batch("COMMIT").unwrap();
    assert_eq!(p.put(&key(2), &Blob(vec![2])), Written::Committed);
    assert_eq!(f.cache.stats().dropped(Dropped::Busy), 1);
    assert_eq!(state(&f), PartitionState::Open);
}

// concurrency_*

#[test]
fn concurrency_readers_see_only_committed_versions() {
    let f = Fixture::new();
    let p = f.repo();
    let value = |version: u32| Blob([version.to_le_bytes(); 1024].concat());
    assert_eq!(p.put(&key(1), &value(0)), Written::Committed);
    let last = 200u32;
    thread::scope(|s| {
        for _ in 0..4 {
            s.spawn(|| {
                let mut seen = 0;
                while seen < last {
                    let Some(Blob(bytes)) = p.get::<Blob>(&key(1)) else {
                        panic!("the key is always there");
                    };
                    let version = u32::from_le_bytes(bytes[..4].try_into().unwrap());
                    assert_eq!(bytes, value(version).0, "a torn value");
                    assert!(version >= seen, "went back from {seen} to {version}");
                    seen = version;
                }
            });
        }
        s.spawn(|| {
            for version in 1..=last {
                let mut batch = p.batch();
                batch.put(&key(1), &value(version));
                batch.put(&key(2), &value(version));
                assert_eq!(batch.commit(), Written::Committed);
            }
        });
    });
    assert_eq!(
        f.cache.stats().dropped.iter().map(|(_, n)| n).sum::<u64>(),
        0
    );
}

#[test]
fn concurrency_reads_proceed_while_a_write_is_paused() {
    let f = Fixture::new();
    let p = f.repo();
    assert_eq!(p.put(&key(1), &Blob(b"old".to_vec())), Written::Committed);
    assert_eq!(p.get::<Blob>(&key(1)), Some(Blob(b"old".to_vec())));
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let (e, r) = (entered.clone(), release.clone());
    p.set_write_hook(Some(Arc::new(move || {
        e.wait();
        r.wait();
    })));
    thread::scope(|s| {
        let writer = s.spawn(|| p.put(&key(1), &Blob(b"new".to_vec())));
        entered.wait();
        // The write transaction is open, its changes not committed.
        let start = Instant::now();
        assert_eq!(p.get::<Blob>(&key(1)), Some(Blob(b"old".to_vec())));
        assert_eq!(p.get_many::<Blob>(&[key(1), key(2)]).len(), 2);
        assert!(start.elapsed() < Duration::from_millis(500));
        release.wait();
        assert_eq!(writer.join().unwrap(), Written::Committed);
    });
    p.set_write_hook(None);
    assert_eq!(p.get::<Blob>(&key(1)), Some(Blob(b"new".to_vec())));
}

// crash_*

const CRASH_ROOT: &str = "LECTERN_CACHE_TEST_CRASH_ROOT";
const CRASH_ENTRIES: u64 = 300;

/// The child process of `crash_committed_entries_survive_an_abort`. Does
/// nothing unless the parent set `CRASH_ROOT`.
#[test]
fn crash_child_commits_then_aborts() {
    let Some(root) = std::env::var_os(CRASH_ROOT) else {
        return;
    };
    let cache = Cache::new(config(root.as_ref(), Budgets::default()));
    let p = cache.repo(std::path::Path::new("/work/lectern/.git"));
    for i in 0..CRASH_ENTRIES / 2 {
        assert_eq!(p.put(&key(i), &Blob(noise(i, 2 << 10))), Written::Committed);
    }
    let mut batch = p.batch();
    for i in CRASH_ENTRIES / 2..CRASH_ENTRIES {
        batch.put(&key(i), &Blob(noise(i, 2 << 10)));
    }
    assert_eq!(batch.commit(), Written::Committed);
    std::process::abort();
}

#[test]
fn crash_committed_entries_survive_an_abort() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("cache");
    let status = Command::new(std::env::current_exe().unwrap())
        .args([
            "tests::faults::crash_child_commits_then_aborts",
            "--exact",
            "--test-threads=1",
        ])
        .env(CRASH_ROOT, &root)
        .status()
        .unwrap();
    assert!(!status.success(), "the child aborts: {status:?}");

    let cache = Cache::new(config(&root, Budgets::default()));
    let p = cache.repo(std::path::Path::new("/work/lectern/.git"));
    // Nothing checkpointed the WAL: recovery happens on this open.
    assert!(files(p.file())[1].exists());
    for i in 0..CRASH_ENTRIES {
        assert_eq!(
            p.get::<Blob>(&key(i)),
            Some(Blob(noise(i, 2 << 10))),
            "entry {i}"
        );
    }
    let conn = raw(p.file());
    let check: String = conn
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .unwrap();
    assert_eq!(check, "ok");
    assert_eq!(meta_bytes(&conn), sum_sizes(&conn));
    assert_eq!(cache.stats().rebuilt, 0);
}
