//! Open project partitions and the directory sweep (`partitions_*`,
//! `sweep_*`).

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime};

use super::support::{Blob, Fixture, atime, files, key, minute, raw};
use crate::{Budgets, Entry, Partition, PartitionKind, PartitionState, Written};

const DAY: Duration = Duration::from_secs(24 * 60 * 60);

fn repo_path(i: usize) -> PathBuf {
    PathBuf::from(format!("/work/repo{i}/.git"))
}

fn state(f: &Fixture, p: &Partition) -> PartitionState {
    f.cache
        .stats()
        .partitions
        .iter()
        .find(|s| s.kind == p.slot.kind() && s.path.as_deref() == p.slot.path())
        .expect("a known partition")
        .state
}

// partitions_*

#[test]
fn partitions_files_follow_kind_and_path() {
    let f = Fixture::new();
    let a = f.cache.repo(Path::new("/work/lectern/.git"));
    let b = f.cache.repo(Path::new("/work/lectern/.git"));
    let folder = f.cache.folder(Path::new("/work/lectern/.git"));
    let other = f.cache.repo(Path::new("/work/other/.git"));
    assert_eq!(a.file(), b.file());
    assert_ne!(a.file(), folder.file());
    assert_ne!(a.file(), other.file());
    // Handles of the same partition share its state.
    assert_eq!(a.put(&key(1), &Blob(vec![1])), Written::Committed);
    assert_eq!(b.get::<Blob>(&key(1)), Some(Blob(vec![1])));
    assert_eq!(folder.get::<Blob>(&key(1)), None);
    assert_eq!(f.cache.stats().partitions.len(), 4);
}

#[test]
fn partitions_third_project_swaps_out_the_least_recently_used() {
    let f = Fixture::new();
    let app = f.cache.app();
    let repos: Vec<Partition> = (0..3).map(|i| f.cache.repo(&repo_path(i))).collect();
    let folder = f.cache.folder(Path::new("/work/notes"));
    for (i, p) in repos.iter().enumerate() {
        assert_eq!(p.put(&key(1), &Blob(vec![i as u8])), Written::Committed);
    }
    assert_eq!(app.put(&key(1), &Blob(vec![9])), Written::Committed);
    // Writing the third closed the first.
    assert_eq!(state(&f, &repos[0]), PartitionState::Closed);
    assert_eq!(state(&f, &repos[1]), PartitionState::Open);
    assert_eq!(state(&f, &repos[2]), PartitionState::Open);

    // Every partition with both connections: at most 6.
    assert!(app.get::<Blob>(&key(1)).is_some());
    assert!(repos[1].get::<Blob>(&key(1)).is_some());
    assert!(repos[2].get::<Blob>(&key(1)).is_some());
    assert_eq!(f.cache.stats().connections, 6);

    // repos[2] is used again, so repos[1] is the least recently used.
    assert!(repos[2].get::<Blob>(&key(1)).is_some());
    assert_eq!(repos[0].get::<Blob>(&key(1)), Some(Blob(vec![0])));
    assert_eq!(state(&f, &repos[1]), PartitionState::Closed);
    assert_eq!(state(&f, &repos[2]), PartitionState::Open);
    assert_eq!(state(&f, &repos[0]), PartitionState::Open);
    assert!(f.cache.stats().connections <= 6);

    // Folders count with repositories; the app partition stays open.
    assert_eq!(folder.get::<Blob>(&key(1)), None);
    assert_eq!(state(&f, &repos[2]), PartitionState::Closed);
    assert_eq!(state(&f, &app), PartitionState::Open);
    for p in &repos {
        assert!(p.get::<Blob>(&key(1)).is_some());
        assert!(f.cache.stats().connections <= 6);
    }
    assert_eq!(app.get::<Blob>(&key(1)), Some(Blob(vec![9])));
    let open = f
        .cache
        .stats()
        .partitions
        .iter()
        .filter(|s| s.kind != PartitionKind::App && s.state == PartitionState::Open)
        .count();
    assert_eq!(open, 2);
}

#[test]
fn partitions_open_projects_sets_the_limit() {
    let f = Fixture::with_budgets(Budgets {
        open_projects: 1,
        ..Budgets::default()
    });
    let a = f.cache.repo(&repo_path(0));
    let b = f.cache.repo(&repo_path(1));
    assert_eq!(a.put(&key(1), &Blob(vec![1])), Written::Committed);
    assert!(a.get::<Blob>(&key(1)).is_some());
    assert_eq!(b.get::<Blob>(&key(1)), None);
    assert_eq!(state(&f, &a), PartitionState::Closed);
    assert_eq!(f.cache.stats().connections, 2);
    assert!(a.get::<Blob>(&key(1)).is_some());
    assert_eq!(state(&f, &b), PartitionState::Closed);
}

#[test]
fn partitions_swap_out_writes_access_times_back() {
    let f = Fixture::new();
    let a = f.cache.repo(&repo_path(0));
    assert_eq!(a.put(&key(1), &Blob(vec![1])), Written::Committed);
    f.clock.advance(2 * 60 * 60 * Duration::from_secs(1));
    assert!(a.get::<Blob>(&key(1)).is_some());
    assert_eq!(a.slot.pending_touched(), 1);
    f.clock.advance(Duration::from_secs(60));
    for i in 1..3 {
        assert_eq!(f.cache.repo(&repo_path(i)).get::<Blob>(&key(1)), None);
    }
    assert_eq!(state(&f, &a), PartitionState::Closed);
    let conn = raw(a.file());
    assert_eq!(
        atime(&conn, &Blob::NAMESPACE, &key(1)),
        Some(minute(&f.clock))
    );
}

#[test]
fn partitions_swap_out_waits_for_calls_in_progress() {
    let f = Fixture::new();
    let a = f.cache.repo(&repo_path(0));
    let b = f.cache.repo(&repo_path(1));
    let c = f.cache.repo(&repo_path(2));
    assert_eq!(a.put(&key(1), &Blob(vec![1])), Written::Committed);
    let (entered_tx, entered) = mpsc::channel();
    let (release, release_rx) = mpsc::channel::<()>();
    let release_rx = Mutex::new(release_rx);
    a.set_write_hook(Some(Arc::new(move || {
        entered_tx.send(()).unwrap();
        release_rx.lock().unwrap().recv().unwrap();
    })));
    thread::scope(|s| {
        let writer = s.spawn(|| a.put(&key(2), &Blob(vec![2])));
        entered.recv().unwrap();
        // b is used after a, so opening c swaps out a, after its write.
        assert_eq!(b.get::<Blob>(&key(1)), None);
        let opener = s.spawn(|| c.get::<Blob>(&key(1)));
        thread::sleep(Duration::from_millis(100));
        assert!(!opener.is_finished());
        assert_eq!(state(&f, &a), PartitionState::Open);
        release.send(()).unwrap();
        assert_eq!(writer.join().unwrap(), Written::Committed);
        assert_eq!(opener.join().unwrap(), None);
    });
    a.set_write_hook(None);
    assert_eq!(state(&f, &a), PartitionState::Closed);
    assert_eq!(state(&f, &c), PartitionState::Open);
    assert_eq!(a.get::<Blob>(&key(2)), Some(Blob(vec![2])));
}

#[test]
fn partitions_concurrent_calls_stay_within_the_connection_limit() {
    let f = Fixture::new();
    let app = f.cache.app();
    thread::scope(|s| {
        for t in 0..6 {
            let cache = &f.cache;
            let app = &app;
            s.spawn(move || {
                for round in 0..40u64 {
                    let p = cache.repo(&repo_path((t + round as usize) % 5));
                    let k = key(t as u64 * 1000 + round);
                    let value = Blob(vec![t as u8; 100]);
                    assert_eq!(p.put(&k, &value), Written::Committed);
                    assert_eq!(p.get::<Blob>(&k), Some(value));
                    assert!(app.get::<Blob>(&key(0)).is_none());
                    assert!(cache.stats().connections <= 6);
                }
            });
        }
    });
    let stats = f.cache.stats();
    assert!(stats.connections <= 6);
    let open = stats
        .partitions
        .iter()
        .filter(|s| s.kind != PartitionKind::App && s.state == PartitionState::Open)
        .count();
    assert!(open <= 2, "{open}");
    // Every write is still there, through any number of swaps.
    for t in 0..6 {
        for round in 0..40u64 {
            let p = f.cache.repo(&repo_path((t + round as usize) % 5));
            assert!(p.get::<Blob>(&key(t as u64 * 1000 + round)).is_some());
        }
    }
}

// sweep_*

/// Sets the modification time of a partition's files that exist.
fn set_modified(file: &Path, at: SystemTime) {
    for path in files(file) {
        if path.exists() {
            File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(at)
                .unwrap();
        }
    }
}

/// Writes one entry to each partition, then closes the cache.
fn populate(f: &Fixture, partitions: &[&Partition], len: usize) {
    for p in partitions {
        assert_eq!(p.put(&key(1), &Blob(vec![7; len])), Written::Committed);
    }
    f.cache.close();
}

fn bytes(file: &Path) -> u64 {
    files(file)
        .iter()
        .filter_map(|p| fs::metadata(p).ok())
        .map(|m| m.len())
        .sum()
}

#[test]
fn sweep_deletes_project_partitions_unused_for_90_days() {
    let mut f = Fixture::new();
    let old = f.cache.repo(&repo_path(0));
    let recent = f.cache.repo(&repo_path(1));
    let folder = f.cache.folder(Path::new("/work/notes"));
    let app = f.cache.app();
    populate(&f, &[&old, &recent, &folder, &app], 100);
    let now = f.clock.now();
    set_modified(old.file(), now - 91 * DAY);
    set_modified(folder.file(), now - 91 * DAY);
    set_modified(recent.file(), now - 89 * DAY);
    set_modified(app.file(), now - 365 * DAY);
    // A file that is not a partition is left alone.
    let stray = f.root().join("repo").join("notes.txt");
    fs::write(&stray, b"x").unwrap();
    File::options()
        .write(true)
        .open(&stray)
        .unwrap()
        .set_modified(now - 365 * DAY)
        .unwrap();
    let files_of = |p: &Partition| p.file().to_path_buf();
    let (old_file, recent_file, folder_file) =
        (files_of(&old), files_of(&recent), files_of(&folder));

    f.reopen();
    // The first open of the run sweeps, whichever partition it is.
    assert!(f.cache.app().get::<Blob>(&key(1)).is_some());
    for path in files(&old_file).iter().chain(&files(&folder_file)) {
        assert!(!path.exists(), "{}", path.display());
    }
    assert!(recent_file.exists());
    assert!(stray.exists());
    assert!(f.cache.repo(&repo_path(1)).get::<Blob>(&key(1)).is_some());
    assert_eq!(f.cache.repo(&repo_path(0)).get::<Blob>(&key(1)), None);
}

#[test]
fn sweep_runs_once_per_run() {
    let mut f = Fixture::new();
    let a = f.cache.repo(&repo_path(0));
    let b = f.cache.repo(&repo_path(1));
    populate(&f, &[&a, &b], 100);
    f.reopen();
    assert_eq!(f.cache.app().get::<Blob>(&key(1)), None);
    set_modified(b.file(), f.clock.now() - 100 * DAY);
    assert!(f.cache.repo(&repo_path(0)).get::<Blob>(&key(1)).is_some());
    assert!(b.file().exists());
    assert!(f.cache.repo(&repo_path(1)).get::<Blob>(&key(1)).is_some());
}

#[test]
fn sweep_over_the_total_deletes_the_oldest_first() {
    let mut f = Fixture::new();
    let repos: Vec<Partition> = (0..4).map(|i| f.cache.repo(&repo_path(i))).collect();
    let app = f.cache.app();
    let mut all: Vec<&Partition> = repos.iter().collect();
    all.push(&app);
    populate(&f, &all, 256 << 10);
    let now = f.clock.now();
    // repos[3] is the oldest, then repos[0], repos[1], repos[2].
    for (i, days) in [(3, 40), (0, 30), (1, 20), (2, 10)] {
        set_modified(repos[i].file(), now - days * DAY);
    }
    let sizes: Vec<u64> = repos.iter().map(|p| bytes(p.file())).collect();
    let total = sizes.iter().sum::<u64>() + bytes(app.file());
    // Room for all but the two oldest.
    f.reopen_with(Budgets {
        total: total - sizes[3] - sizes[0] + 1,
        ..Budgets::default()
    });
    assert!(f.cache.app().get::<Blob>(&key(1)).is_some());
    assert!(!repos[3].file().exists());
    assert!(!repos[0].file().exists());
    assert!(repos[1].file().exists());
    assert!(repos[2].file().exists());
    assert!(app.file().exists());
}

#[test]
fn sweep_keeps_the_partition_being_opened() {
    let mut f = Fixture::new();
    let a = f.cache.repo(&repo_path(0));
    let b = f.cache.repo(&repo_path(1));
    let app = f.cache.app();
    populate(&f, &[&a, &b, &app], 256 << 10);
    let now = f.clock.now();
    set_modified(a.file(), now - 120 * DAY);
    set_modified(b.file(), now - 100 * DAY);
    // Over the total even without the app partition's files.
    f.reopen_with(Budgets {
        total: 1,
        ..Budgets::default()
    });
    // The oldest is the one opened first: it is kept, the other is deleted.
    assert!(f.cache.repo(&repo_path(0)).get::<Blob>(&key(1)).is_some());
    assert!(!b.file().exists());
    assert!(app.file().exists());
    assert!(f.cache.app().get::<Blob>(&key(1)).is_some());
}
