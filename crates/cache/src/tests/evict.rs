//! Eviction, free pages and access times (`evict_*`, `touch_*`).

use std::fs;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use super::support::{
    Blob, BlobV2, Fixture, Other, atime, count, files, key, meta_bytes, minute, noise, pages, raw,
    sum_sizes,
};
use crate::db::{ENTRY_OVERHEAD, EVICT_MAX, EVICT_MIN, VACUUM_FREE, VACUUM_PAGES};
use crate::slot::TOUCHED_MAX;
use crate::{Budgets, Entry, Key, Partition, Written};

const HOUR: Duration = Duration::from_secs(60 * 60);
const MINUTE: Duration = Duration::from_secs(60);

fn project_budget(bytes: u64) -> Budgets {
    Budgets {
        project: bytes,
        ..Budgets::default()
    }
}

/// `n` entries of `len` noise bytes from `start`, in batches within
/// `MAX_BATCH`.
fn fill(p: &Partition, start: u64, n: u64, len: usize) {
    let mut batch = p.batch();
    for i in start..start + n {
        if batch.bytes() + len as u64 + 64 > crate::MAX_BATCH {
            assert_eq!(batch.commit(), Written::Committed);
            batch = p.batch();
        }
        batch.put(&key(i), &Blob(noise(i, len)));
    }
    assert_eq!(batch.commit(), Written::Committed);
}

// evict_*

#[test]
fn evict_drops_the_oldest_and_keeps_recently_read() {
    let budget = 1 << 20;
    let f = Fixture::with_budgets(project_budget(budget));
    let p = f.repo();
    // 120 entries of 8 KiB, a minute apart: just under the budget.
    for i in 0..120 {
        assert_eq!(p.put(&key(i), &Blob(noise(i, 8 << 10))), Written::Committed);
        f.clock.advance(MINUTE);
    }
    let conn = raw(p.file());
    assert!(meta_bytes(&conn) < budget);
    assert_eq!(f.cache.stats().evicted_bytes, 0);

    // The oldest entry is read again; the next write goes over the budget.
    f.clock.advance(2 * HOUR);
    assert!(p.get::<Blob>(&key(0)).is_some());
    assert_eq!(
        p.put(&key(1000), &Blob(noise(1000, 64 << 10))),
        Written::Committed
    );
    let after = meta_bytes(&conn);
    assert!(after <= budget / 10 * 9, "{after}");
    assert_eq!(after, sum_sizes(&conn));
    assert!(f.cache.stats().evicted_bytes > 0);
    // Read two hours ago, so younger than every other entry.
    assert!(p.get::<Blob>(&key(0)).is_some());
    assert!(p.get::<Blob>(&key(1000)).is_some());
    let kept: Vec<u64> = (1..120)
        .filter(|i| p.get::<Blob>(&key(*i)).is_some())
        .collect();
    // The oldest went; what is left is the newest, without gaps.
    assert!(!kept.is_empty() && kept.len() < 119, "{kept:?}");
    assert_eq!(kept, (120 - kept.len() as u64..120).collect::<Vec<_>>());
}

/// One write transaction of `n` new 64 KiB entries from `start`; returns
/// the bytes it wrote and evicted.
fn write_and_evict(f: &Fixture, p: &Partition, start: u64, n: u64) -> (u64, u64) {
    let before = f.cache.stats().evicted_bytes;
    let mut batch = p.batch();
    let mut written = 0;
    for i in start..start + n {
        let value = Blob(noise(i, 64 << 10));
        written += key(i).len() as u64 + value.0.len() as u64 + ENTRY_OVERHEAD;
        batch.put(&key(i), &value);
    }
    assert_eq!(batch.commit(), Written::Committed);
    (written, f.cache.stats().evicted_bytes - before)
}

#[test]
fn evict_per_transaction_follows_the_write() {
    let mut f = Fixture::new();
    let entry = key(0).len() as u64 + (64 << 10) + ENTRY_OVERHEAD;
    fill(&f.repo(), 0, 700, 64 << 10);
    let start = meta_bytes(&raw(f.repo().file()));
    assert!(start > 40 << 20, "{start}");

    // The next run has a much smaller budget.
    let budget = 4 << 20;
    f.reopen_with(project_budget(budget));
    let p = f.repo();
    let conn = raw(p.file());
    let mut bytes = start;

    // A small write deletes 1 MiB.
    let before = f.cache.stats().evicted_bytes;
    assert_eq!(p.put(&key(10_000), &Blob(vec![1])), Written::Committed);
    let evicted = f.cache.stats().evicted_bytes - before;
    assert!(
        (EVICT_MIN..EVICT_MIN + entry).contains(&evicted),
        "{evicted}"
    );
    assert!(meta_bytes(&conn) < bytes);
    bytes = meta_bytes(&conn);

    // A larger one deletes what it wrote, up to 16 MiB: the partition does
    // not grow while it is over the budget.
    for (start, n) in [(20_000, 64), (30_000, 250)] {
        let (written, evicted) = write_and_evict(&f, &p, start, n);
        assert!(evicted >= written.min(EVICT_MAX), "{evicted} for {written}");
        assert!(
            evicted < written.min(EVICT_MAX) + entry,
            "{evicted} for {written}"
        );
        assert!(meta_bytes(&conn) <= bytes);
        bytes = meta_bytes(&conn);
    }
    assert!(bytes > 16 << 20, "{bytes}");

    // Small writes drain the rest, 1 MiB each, until it is within the budget.
    let mut writes = 0;
    while meta_bytes(&conn) > budget {
        assert_eq!(
            p.put(&key(40_000 + writes), &Blob(vec![1])),
            Written::Committed
        );
        assert!(meta_bytes(&conn) < bytes);
        bytes = meta_bytes(&conn);
        writes += 1;
        assert!(writes < 100);
    }
    assert!(bytes > budget - EVICT_MIN - entry, "{bytes}");
    assert_eq!(bytes, sum_sizes(&conn));
}

#[test]
fn evict_retired_entries_before_the_budget() {
    let mut f = Fixture::new();
    let p = f.repo();
    // Old entries of a live namespace, then younger ones of a version about
    // to be retired.
    for i in 0..100 {
        assert_eq!(p.put(&key(i), &Other(vec![0; 1000])), Written::Committed);
    }
    f.clock.advance(2 * HOUR);
    let mut batch = p.batch();
    for i in 0..100 {
        batch.put(&key(i), &Blob(vec![1; 1000]));
    }
    assert_eq!(batch.commit(), Written::Committed);
    let total = meta_bytes(&raw(p.file()));

    // Over the budget after the next write, by less than the retired
    // entries hold.
    f.reopen_with(project_budget(total - 1000));
    let p = f.repo();
    assert_eq!(p.put(&key(0), &BlobV2(vec![2; 1000])), Written::Committed);
    let conn = raw(p.file());
    assert_eq!(count(&conn, &Blob::NAMESPACE), 0);
    assert_eq!(count(&conn, &Other::NAMESPACE), 100);
    assert_eq!(count(&conn, &BlobV2::NAMESPACE), 1);
    assert_eq!(meta_bytes(&conn), sum_sizes(&conn));
}

#[test]
fn evict_returns_free_pages_over_32_mib() {
    let f = Fixture::new();
    let p = f.repo();
    let len = 64 << 10;
    fill(&p, 0, 800, len);
    let conn = raw(p.file());
    let (full, free) = pages(&conn);
    assert_eq!(free, 0);
    let page_size = 4096;

    // Deleting 40 MiB leaves more than 32 MiB free: the same transaction
    // returns 2 MiB of it.
    let mut batch = p.batch();
    for i in 0..640 {
        batch.remove::<Blob>(&key(i));
    }
    assert_eq!(batch.commit(), Written::Committed);
    let (count, free) = pages(&conn);
    // 512 free pages, and the pointer-map pages that covered them.
    let returned = full - count;
    assert!(
        (u64::from(VACUUM_PAGES)..u64::from(VACUUM_PAGES) + 8).contains(&returned),
        "{returned}"
    );
    assert!(free * page_size > VACUUM_FREE, "{free}");
    // Each write returns 2 MiB more, down to 32 MiB of free pages.
    let mut writes = 0;
    while pages(&conn).1 * page_size > VACUUM_FREE {
        assert_eq!(p.put(&key(10_000), &Blob(vec![1])), Written::Committed);
        writes += 1;
        assert!(writes < 20);
    }
    assert!(writes >= 3, "{writes}");
    let (count, free) = pages(&conn);
    assert!(free * page_size <= VACUUM_FREE);
    assert!(free * page_size > VACUUM_FREE - u64::from(VACUUM_PAGES) * page_size);
    assert!(
        count <= full - 2 * u64::from(VACUUM_PAGES),
        "{count} of {full}"
    );

    // The file itself shrinks at a checkpoint, at the latest when the last
    // connection closes.
    let [file, wal, _] = files(p.file());
    drop(conn);
    f.cache.close();
    assert!(!wal.exists());
    assert_eq!(fs::metadata(&file).unwrap().len(), count * page_size);
    for i in 640..800 {
        assert!(p.get::<Blob>(&key(i)).is_some());
    }
}

// touch_*

/// Counts write transactions of `p`.
fn count_writes(p: &Partition) -> Arc<AtomicUsize> {
    let writes = Arc::new(AtomicUsize::new(0));
    let counter = writes.clone();
    p.set_write_hook(Some(Arc::new(move || {
        counter.fetch_add(1, Ordering::Relaxed);
    })));
    writes
}

#[test]
fn touch_hits_within_an_hour_write_nothing() {
    let f = Fixture::new();
    let p = f.repo();
    assert_eq!(p.put(&key(1), &Blob(vec![1])), Written::Committed);
    let written = minute(&f.clock);
    let writes = count_writes(&p);
    let conn = raw(p.file());
    let [_, wal, _] = files(p.file());
    let wal_size = fs::metadata(&wal).unwrap().len();
    for _ in 0..6 {
        f.clock.advance(10 * MINUTE);
        assert!(p.get::<Blob>(&key(1)).is_some());
        assert!(p.get_many::<Blob>(&[key(1), key(2)])[0].is_some());
    }
    assert_eq!(p.slot.pending_touched(), 0);
    assert_eq!(writes.load(Ordering::Relaxed), 0);
    assert_eq!(fs::metadata(&wal).unwrap().len(), wal_size);
    assert_eq!(atime(&conn, &Blob::NAMESPACE, &key(1)), Some(written));
    // Nothing to write back either.
    assert_eq!(p.put(&key(2), &Blob(vec![2])), Written::Committed);
    assert_eq!(atime(&conn, &Blob::NAMESPACE, &key(1)), Some(written));
}

#[test]
fn touch_after_61_minutes_is_written_by_the_next_write() {
    let f = Fixture::new();
    let p = f.repo();
    assert_eq!(p.put(&key(1), &Blob(vec![1])), Written::Committed);
    let written = minute(&f.clock);
    let writes = count_writes(&p);
    let conn = raw(p.file());

    f.clock.advance(61 * MINUTE);
    assert!(p.get::<Blob>(&key(1)).is_some());
    assert!(p.get::<Blob>(&key(1)).is_some());
    // Recorded once, not written yet.
    assert_eq!(p.slot.pending_touched(), 1);
    assert_eq!(writes.load(Ordering::Relaxed), 0);
    assert_eq!(atime(&conn, &Blob::NAMESPACE, &key(1)), Some(written));

    f.clock.advance(5 * MINUTE);
    assert_eq!(p.put(&key(2), &Blob(vec![2])), Written::Committed);
    assert_eq!(writes.load(Ordering::Relaxed), 1);
    assert_eq!(p.slot.pending_touched(), 0);
    let now = minute(&f.clock);
    assert_eq!(atime(&conn, &Blob::NAMESPACE, &key(1)), Some(now));
    // Fresh again: the next hit within the hour records nothing.
    f.clock.advance(30 * MINUTE);
    assert!(p.get::<Blob>(&key(1)).is_some());
    assert_eq!(p.slot.pending_touched(), 0);
}

#[test]
fn touch_failed_write_keeps_the_access_times() {
    let f = Fixture::new();
    let p = f.repo();
    assert_eq!(p.put(&key(1), &Blob(vec![1])), Written::Committed);
    f.clock.advance(2 * HOUR);
    assert!(p.get::<Blob>(&key(1)).is_some());
    assert_eq!(p.slot.pending_touched(), 1);
    p.set_max_page_count(Some(1));
    assert_eq!(
        p.put(&key(2), &Blob(noise(2, 64 << 10))),
        Written::Dropped(crate::Dropped::DiskFull)
    );
    assert_eq!(p.slot.pending_touched(), 1);
}

#[test]
fn touch_close_writes_access_times_back() {
    let f = Fixture::new();
    let p = f.cache.app();
    assert_eq!(p.put(&key(1), &Blob(vec![1])), Written::Committed);
    assert_eq!(p.put(&key(2), &Blob(vec![2])), Written::Committed);
    let written = minute(&f.clock);
    f.clock.advance(3 * HOUR);
    assert!(p.get::<Blob>(&key(1)).is_some());
    f.clock.advance(MINUTE);
    f.cache.close();
    let conn = raw(p.file());
    assert_eq!(
        atime(&conn, &Blob::NAMESPACE, &key(1)),
        Some(minute(&f.clock))
    );
    assert_eq!(atime(&conn, &Blob::NAMESPACE, &key(2)), Some(written));
}

#[test]
fn touch_dropping_the_cache_loses_pending_access_times() {
    let mut f = Fixture::new();
    let p = f.repo();
    assert_eq!(p.put(&key(1), &Blob(vec![1])), Written::Committed);
    let written = minute(&f.clock);
    f.clock.advance(2 * HOUR);
    assert!(p.get::<Blob>(&key(1)).is_some());
    drop(p);
    f.reopen();
    let conn = raw(f.repo().file());
    assert_eq!(atime(&conn, &Blob::NAMESPACE, &key(1)), Some(written));
}

#[test]
fn touch_pending_set_stops_at_4096() {
    let f = Fixture::new();
    let p = f.repo();
    let n = TOUCHED_MAX as u64 + 1000;
    let mut batch = p.batch();
    for i in 0..n {
        batch.put(&key(i), &Blob(vec![i as u8]));
    }
    assert_eq!(batch.commit(), Written::Committed);
    f.clock.advance(2 * HOUR);
    let keys: Vec<Key> = (0..n).map(key).collect();
    assert!(p.get_many::<Blob>(&keys).iter().all(Option::is_some));
    assert_eq!(p.slot.pending_touched(), TOUCHED_MAX);
    assert!(p.get::<Blob>(&key(n - 1)).is_some());
    assert_eq!(p.slot.pending_touched(), TOUCHED_MAX);

    assert_eq!(p.put(&key(n), &Blob(vec![0])), Written::Committed);
    assert_eq!(p.slot.pending_touched(), 0);
    let conn = raw(p.file());
    let now = minute(&f.clock);
    let touched: i64 = conn
        .query_row("SELECT count(*) FROM entry WHERE atime = ?1", [now], |r| {
            r.get(0)
        })
        .unwrap();
    // The 4096 recorded, and the entry just written.
    assert_eq!(touched, TOUCHED_MAX as i64 + 1);
    // The rest are recorded by their next hit.
    assert!(p.get::<Blob>(&key(n - 1)).is_some());
    assert_eq!(p.slot.pending_touched(), 1);
}
