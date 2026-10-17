//! Opening files of other versions, other partitions and damaged files
//! (`open_*`).

use std::fs;
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;

use super::support::{Blob, Fixture, files, key, noise, raw};
use crate::db::{APPLICATION_ID, SCHEMA_VERSION};
use crate::{Budgets, Cache, DisableReason, Dropped, Partition, PartitionState, Written};

const PAGE: u64 = 4096;

/// Writes 200 entries of 3 KiB, so the file has a few hundred pages, and
/// closes the cache so everything is checkpointed into the `.sqlite` file.
fn populate(f: &Fixture, p: &Partition) {
    let mut batch = p.batch();
    for i in 0..200 {
        batch.put(&key(i), &Blob(noise(i, 3 << 10)));
    }
    assert_eq!(batch.commit(), Written::Committed);
    f.cache.close();
    assert!(!files(p.file())[1].exists(), "checkpointed on close");
}

/// After a rebuild: nothing old is there and the partition works.
fn assert_rebuilt_and_working(f: &Fixture, p: &Partition) {
    for i in 0..200 {
        assert_eq!(p.get::<Blob>(&key(i)), None);
    }
    assert_eq!(p.put(&key(1), &Blob(vec![1])), Written::Committed);
    assert_eq!(p.get::<Blob>(&key(1)), Some(Blob(vec![1])));
    let conn = raw(p.file());
    let header: (i32, i32) = conn
        .query_row(
            "SELECT * FROM pragma_application_id, pragma_user_version",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(header, (APPLICATION_ID, SCHEMA_VERSION));
    let stats = f.cache.stats();
    assert_eq!(state_of(f, p), PartitionState::Open);
    assert!(stats.rebuilt >= 1);
}

fn state_of(f: &Fixture, p: &Partition) -> PartitionState {
    f.cache
        .stats()
        .partitions
        .iter()
        .find(|s| s.kind == p.slot.kind() && s.path.as_deref() == p.slot.path())
        .expect("partition in stats")
        .state
}

fn set_pragma(file: &Path, pragma: &str, value: i64) {
    raw(file)
        .execute_batch(&format!("PRAGMA {pragma} = {value}"))
        .unwrap();
}

#[test]
fn open_older_user_version_rebuilds() {
    let mut f = Fixture::new();
    let p = f.repo();
    populate(&f, &p);
    set_pragma(p.file(), "user_version", i64::from(SCHEMA_VERSION) - 1);
    f.reopen();
    let p = f.repo();
    assert_rebuilt_and_working(&f, &p);
    assert_eq!(f.cache.stats().rebuilt, 1);
}

#[test]
fn open_newer_user_version_rebuilds() {
    let mut f = Fixture::new();
    let p = f.repo();
    populate(&f, &p);
    set_pragma(p.file(), "user_version", i64::from(SCHEMA_VERSION) + 1);
    f.reopen();
    let p = f.repo();
    assert_rebuilt_and_working(&f, &p);
    assert_eq!(f.cache.stats().rebuilt, 1);
}

#[test]
fn open_foreign_application_id_rebuilds() {
    let mut f = Fixture::new();
    let p = f.cache.app();
    populate(&f, &p);
    set_pragma(p.file(), "application_id", 0x1234_5678);
    f.reopen();
    let p = f.cache.app();
    assert_rebuilt_and_working(&f, &p);
}

#[test]
fn open_plain_sqlite_file_rebuilds() {
    let f = Fixture::new();
    let p = f.cache.app();
    fs::create_dir_all(f.root()).unwrap();
    raw(p.file())
        .execute_batch("CREATE TABLE notes (text TEXT); INSERT INTO notes VALUES ('x');")
        .unwrap();
    assert_eq!(p.get::<Blob>(&key(1)), None);
    assert_rebuilt_and_working(&f, &p);
}

#[test]
fn open_file_of_another_partition_rebuilds() {
    let mut f = Fixture::new();
    let a = f.cache.repo(Path::new("/work/a/.git"));
    let b = f.cache.repo(Path::new("/work/b/.git"));
    populate(&f, &a);
    fs::copy(a.file(), b.file()).unwrap();
    f.reopen();
    let a = f.cache.repo(Path::new("/work/a/.git"));
    let b = f.cache.repo(Path::new("/work/b/.git"));
    assert_rebuilt_and_working(&f, &b);
    // The original is untouched.
    assert_eq!(a.get::<Blob>(&key(5)), Some(Blob(noise(5, 3 << 10))));
    assert_eq!(f.cache.stats().rebuilt, 1);
    // A folder partition never takes the file of a repository with the same
    // path.
    let folder = f.cache.folder(Path::new("/work/a/.git"));
    assert_eq!(folder.get::<Blob>(&key(5)), None);
    assert_ne!(folder.file(), a.file());
}

#[test]
fn open_random_bytes_rebuilds() {
    let f = Fixture::new();
    let p = f.repo();
    fs::create_dir_all(p.file().parent().unwrap()).unwrap();
    fs::write(p.file(), noise(3, 64 << 10)).unwrap();
    assert_eq!(p.put(&key(700), &Blob(vec![7])), Written::Committed);
    assert_eq!(p.get::<Blob>(&key(700)), Some(Blob(vec![7])));
    assert_rebuilt_and_working(&f, &p);
    assert_eq!(f.cache.stats().rebuilt, 1);
}

/// Reads every populated key; returns how many hits there were.
fn read_all(p: &Partition) -> usize {
    (0..200)
        .filter(|i| p.get::<Blob>(&key(*i)).is_some())
        .count()
}

#[test]
fn open_truncated_file_rebuilds() {
    let mut f = Fixture::new();
    let p = f.repo();
    populate(&f, &p);
    let len = fs::metadata(p.file()).unwrap().len();
    fs::OpenOptions::new()
        .write(true)
        .open(p.file())
        .unwrap()
        .set_len(len / 2 / PAGE * PAGE + 100)
        .unwrap();
    f.reopen();
    let p = f.repo();
    let hits = read_all(&p);
    assert!(hits < 200, "some entries were cut off");
    assert_eq!(f.cache.stats().rebuilt, 1);
    assert_rebuilt_and_working(&f, &p);
}

/// Overwrites the pages in the middle third of the file.
fn overwrite_middle_pages(file: &Path) {
    let pages = fs::metadata(file).unwrap().len() / PAGE;
    assert!(pages > 100, "{pages} pages");
    let mut out = fs::OpenOptions::new().write(true).open(file).unwrap();
    for page in pages / 3..pages * 2 / 3 {
        out.seek(SeekFrom::Start(page * PAGE)).unwrap();
        out.write_all(&noise(page, PAGE as usize)).unwrap();
    }
}

#[test]
fn open_overwritten_pages_rebuild_when_read() {
    let mut f = Fixture::new();
    let p = f.repo();
    populate(&f, &p);
    overwrite_middle_pages(p.file());
    f.reopen();
    let p = f.repo();
    let hits = read_all(&p);
    assert!(hits < 200);
    assert_eq!(f.cache.stats().rebuilt, 1);
    assert_rebuilt_and_working(&f, &p);
}

#[test]
fn open_second_corruption_in_a_run_disables() {
    let f = Fixture::new();
    let p = f.repo();
    // First corruption: rebuilt.
    fs::create_dir_all(p.file().parent().unwrap()).unwrap();
    fs::write(p.file(), noise(3, 64 << 10)).unwrap();
    assert_eq!(p.get::<Blob>(&key(1)), None);
    assert_eq!(f.cache.stats().rebuilt, 1);
    // Second, in the same run.
    populate(&f, &p);
    overwrite_middle_pages(p.file());
    read_all(&p);
    assert_eq!(
        state_of(&f, &p),
        PartitionState::Disabled {
            reason: DisableReason::Corrupt
        }
    );
    // The files are gone and no call creates them again.
    assert!(files(p.file()).iter().all(|f| !f.exists()));
    assert_eq!(p.get::<Blob>(&key(1)), None);
    assert_eq!(
        p.put(&key(1), &Blob(vec![1])),
        Written::Dropped(Dropped::Unavailable)
    );
    let mut batch = p.batch();
    batch.put(&key(2), &Blob(vec![2]));
    assert_eq!(batch.commit(), Written::Dropped(Dropped::Unavailable));
    assert!(files(p.file()).iter().all(|f| !f.exists()));
    assert_eq!(f.cache.stats().connections, 0);
    // Other partitions are unaffected.
    assert_eq!(
        f.cache.app().put(&key(1), &Blob(vec![1])),
        Written::Committed
    );
}

#[test]
fn open_unwritable_root_disables_without_panicking() {
    let dir = tempfile::tempdir().unwrap();
    // The root is a regular file: no directory can be created under it.
    let root = dir.path().join("cache");
    fs::write(&root, b"not a directory").unwrap();
    let cache = Cache::new(super::support::config(&root, Budgets::default()));
    let p = cache.repo(Path::new("/work/lectern/.git"));
    assert_eq!(p.get::<Blob>(&key(1)), None);
    assert_eq!(
        p.put(&key(1), &Blob(vec![1])),
        Written::Dropped(Dropped::Unavailable)
    );
    assert_eq!(
        cache.app().put(&key(1), &Blob(vec![1])),
        Written::Dropped(Dropped::Unavailable)
    );
    let stats = cache.stats();
    assert!(stats.partitions.iter().all(|s| s.state
        == PartitionState::Disabled {
            reason: DisableReason::NotWritable
        }));
    assert_eq!(stats.dropped(Dropped::Unavailable), 2);
    assert_eq!(fs::read(&root).unwrap(), b"not a directory");
}

#[test]
fn open_read_only_directory_disables() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("cache");
    fs::create_dir(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o555)).unwrap();
    if fs::write(root.join("probe"), b"").is_ok() {
        // Running with privileges that ignore the mode (root in a
        // container); `open_unwritable_root_disables_without_panicking`
        // covers the behavior.
        return;
    }
    let cache = Cache::new(super::support::config(&root, Budgets::default()));
    let p = cache.app();
    assert_eq!(p.get::<Blob>(&key(1)), None);
    assert_eq!(
        p.put(&key(1), &Blob(vec![1])),
        Written::Dropped(Dropped::Unavailable)
    );
    fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
}
