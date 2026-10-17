//! `Scope::Tree`: real FSEvents streams on temporary directories.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::thread;
use std::time::{Duration, Instant};

use lectern_fs_watch::{Armed, BackendError, Flags, RawEvent};
use tempfile::TempDir;

use super::support::{DEADLINE, Recorder, backend, flags_of, has, is_probe, tree, wait_live};
use crate::fs_events::stream::{Queue, Stream};

fn root_in(dir: &TempDir) -> PathBuf {
    let root = dir.path().join("root");
    fs::create_dir(&root).unwrap();
    root
}

fn append(path: &Path, bytes: &[u8]) {
    OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap()
        .write_all(bytes)
        .unwrap();
}

/// Every path is relative to the target: never absolute, never carrying the
/// canonical prefix, and nothing reported the root as changed.
fn assert_relative(events: &[RawEvent], root: &Path) {
    let canonical = fs::canonicalize(root).unwrap();
    for event in events {
        assert!(!event.path.starts_with(b"/"), "absolute path in {event:?}");
        assert!(
            !event
                .path
                .windows(canonical.as_os_str().len())
                .any(|window| window == canonical.as_os_str().as_bytes()),
            "canonical prefix left in {event:?}"
        );
        assert!(
            !event.flags.contains(Flags::ROOT_CHANGED),
            "unexpected ROOT_CHANGED in {event:?}"
        );
    }
}

#[test]
fn fsevents_reports_create_modify_rename_delete_as_relative_paths() {
    let dir = tempfile::tempdir().unwrap();
    let root = root_in(&dir);
    fs::write(root.join("existing.txt"), b"one").unwrap();
    let recorder = Recorder::new();
    let backend = backend();
    let armed = backend.arm_with(&tree(&root), recorder.deliver()).unwrap();
    wait_live(&root, &*armed, &recorder);

    append(&root.join("existing.txt"), b"two");
    fs::write(root.join("a.txt"), b"a").unwrap();
    fs::create_dir(root.join("sub")).unwrap();
    fs::write(root.join("sub/b.txt"), b"b").unwrap();
    fs::rename(root.join("a.txt"), root.join("c.txt")).unwrap();
    fs::remove_file(root.join("sub/b.txt")).unwrap();

    let events = recorder.wait_until(
        "create, modify, rename and delete",
        || armed.flush().unwrap(),
        |events| {
            has(events, "existing.txt", Flags::FILE | Flags::MODIFIED)
                && has(events, "a.txt", Flags::FILE | Flags::CREATED)
                && has(events, "a.txt", Flags::RENAMED)
                && has(events, "c.txt", Flags::FILE | Flags::RENAMED)
                && has(events, "sub", Flags::DIR | Flags::CREATED)
                && has(events, "sub/b.txt", Flags::FILE | Flags::REMOVED)
        },
    );
    assert_relative(&events, &root);
    for event in &events {
        let known = ["", "existing.txt", "a.txt", "c.txt", "sub", "sub/b.txt"]
            .iter()
            .any(|path| &*event.path == path.as_bytes());
        assert!(known || is_probe(&event.path), "unexpected {event:?}");
    }
}

#[test]
fn fsevents_strips_the_canonical_prefix_of_a_symlinked_root() {
    // `/tmp` is a symbolic link to `/private/tmp` on macOS.
    let tmp = tempfile::Builder::new()
        .prefix("lectern-fsevents")
        .tempdir_in("/tmp")
        .unwrap();
    let root = root_in(&tmp);
    assert_ne!(
        fs::canonicalize(&root).unwrap(),
        root,
        "/tmp is expected to be a symbolic link"
    );
    // And a root reached through a link of our own.
    let real = tmp.path().join("real");
    fs::create_dir(&real).unwrap();
    let link = tmp.path().join("link");
    symlink(&real, &link).unwrap();

    let backend = backend();
    for (watched, written) in [(&root, &root), (&link, &real)] {
        let recorder = Recorder::new();
        let armed = backend
            .arm_with(&tree(watched), recorder.deliver())
            .unwrap();
        wait_live(written, &*armed, &recorder);
        fs::write(written.join("x.txt"), b"x").unwrap();
        let events = recorder.wait_until(
            "x.txt",
            || armed.flush().unwrap(),
            |events| flags_of(events, "x.txt").is_some(),
        );
        assert_relative(&events, watched);
    }
}

#[test]
fn fsevents_flush_delivers_pending_events() {
    let dir = tempfile::tempdir().unwrap();
    let root = root_in(&dir);
    let recorder = Recorder::new();
    // A latency far beyond the wait: whatever arrives in time came through
    // FSEventStreamFlushSync, not the latency timer.
    let latency = Duration::from_secs(120);
    let stream = Stream::start_with(
        &root,
        Arc::new(Queue::new().unwrap()),
        recorder.deliver(),
        latency,
    )
    .unwrap();
    wait_live(&root, &stream, &recorder);

    let started = Instant::now();
    fs::write(root.join("pending.txt"), b"x").unwrap();
    // FlushSync delivers what the FSEvents service already has; a write a
    // few milliseconds old may not have reached it yet, so flush again
    // until it shows up.
    recorder.wait_until(
        "flush to deliver pending.txt",
        || stream.flush().unwrap(),
        |events| flags_of(events, "pending.txt").is_some(),
    );
    assert!(started.elapsed() < latency);
    assert!(DEADLINE < latency);
}

#[test]
fn fsevents_no_callbacks_after_drop() {
    let dir = tempfile::tempdir().unwrap();
    let root = root_in(&dir);
    let recorder = Recorder::new();
    let backend = backend();
    let armed = backend.arm_with(&tree(&root), recorder.deliver()).unwrap();
    wait_live(&root, &*armed, &recorder);

    let stop = Arc::new(AtomicBool::new(false));
    let writer = {
        let root = root.clone();
        let stop = stop.clone();
        thread::spawn(move || {
            let mut n = 0u64;
            while !stop.load(SeqCst) {
                n += 1;
                fs::write(root.join(format!("w-{}", n % 64)), n.to_le_bytes()).unwrap();
                thread::sleep(Duration::from_millis(1));
            }
        })
    };
    recorder.wait_until(
        "the writer's events",
        || armed.flush().unwrap(),
        |events| events.iter().any(|event| event.path.starts_with(b"w-")),
    );
    drop(armed);
    recorder.close();
    // Keep writing for a while: a callback still scheduled would show up.
    thread::sleep(Duration::from_millis(500));
    stop.store(true, SeqCst);
    writer.join().unwrap();
    assert_eq!(recorder.late(), 0);
}

#[test]
fn fsevents_root_deleted_reports_root_changed() {
    let dir = tempfile::tempdir().unwrap();
    let root = root_in(&dir);
    fs::create_dir(root.join("sub")).unwrap();
    fs::write(root.join("sub/file"), b"x").unwrap();
    let recorder = Recorder::new();
    let backend = backend();
    let armed = backend.arm_with(&tree(&root), recorder.deliver()).unwrap();
    wait_live(&root, &*armed, &recorder);

    fs::remove_dir_all(&root).unwrap();
    recorder.wait_until(
        "ROOT_CHANGED",
        || armed.flush().unwrap(),
        |events| has(events, "", Flags::ROOT_CHANGED),
    );
}

#[test]
fn fsevents_root_renamed_reports_root_changed() {
    let dir = tempfile::tempdir().unwrap();
    let root = root_in(&dir);
    let recorder = Recorder::new();
    let backend = backend();
    let armed = backend.arm_with(&tree(&root), recorder.deliver()).unwrap();
    wait_live(&root, &*armed, &recorder);

    fs::rename(&root, dir.path().join("moved")).unwrap();
    recorder.wait_until(
        "ROOT_CHANGED",
        || armed.flush().unwrap(),
        |events| has(events, "", Flags::ROOT_CHANGED),
    );
}

#[test]
fn fsevents_arm_missing_path_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let backend = backend();
    let result = backend.arm_with(
        &tree(&dir.path().join("missing")),
        Recorder::new().deliver(),
    );
    assert_eq!(result.err(), Some(BackendError::NotFound));
}

#[test]
fn fsevents_streams_share_one_queue_and_stay_apart() {
    let dir = tempfile::tempdir().unwrap();
    let one = dir.path().join("one");
    let two = dir.path().join("two");
    fs::create_dir(&one).unwrap();
    fs::create_dir(&two).unwrap();
    let backend = backend();
    let first = Recorder::new();
    let second = Recorder::new();
    let a = backend.arm_with(&tree(&one), first.deliver()).unwrap();
    let b = backend.arm_with(&tree(&two), second.deliver()).unwrap();
    wait_live(&one, &*a, &first);
    wait_live(&two, &*b, &second);

    fs::write(one.join("only-one"), b"1").unwrap();
    fs::write(two.join("only-two"), b"2").unwrap();
    first.wait_until(
        "only-one",
        || a.flush().unwrap(),
        |events| flags_of(events, "only-one").is_some(),
    );
    second.wait_until(
        "only-two",
        || b.flush().unwrap(),
        |events| flags_of(events, "only-two").is_some(),
    );
    assert!(flags_of(&first.snapshot(), "only-two").is_none());
    assert!(flags_of(&second.snapshot(), "only-one").is_none());

    // Dropping one stream leaves the other working.
    drop(a);
    fs::write(two.join("after"), b"3").unwrap();
    second.wait_until(
        "after",
        || b.flush().unwrap(),
        |events| flags_of(events, "after").is_some(),
    );
}
