//! `Scope::Entries`: the kqueue thread on temporary directories. kqueue
//! notes are posted synchronously by the system call that causes them, so
//! after `flush` returns everything written before it has been delivered;
//! the tests still wait with a deadline before asserting a positive result.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::thread;
use std::time::Duration;

use lectern_fs_watch::{BackendError, Flags};

use super::support::{Recorder, backend, entries, flags_of, has};
use crate::fs_events::kqueue::MAX_DESCRIPTORS;

fn append(path: &Path, bytes: &[u8]) {
    OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap()
        .write_all(bytes)
        .unwrap();
}

#[test]
fn kqueue_in_place_write_reports_the_entry() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("config"), b"a").unwrap();
    fs::write(dir.path().join("other"), b"a").unwrap();
    let recorder = Recorder::new();
    let backend = backend();
    let armed = backend
        .arm_with(&entries(dir.path(), &["config"]), recorder.deliver())
        .unwrap();

    append(&dir.path().join("config"), b"b");
    let events = recorder.wait_until(
        "config modified",
        || armed.flush().unwrap(),
        |events| has(events, "config", Flags::MODIFIED),
    );
    assert!(has(&events, "config", Flags::FILE), "{events:?}");
    assert_eq!(
        events
            .iter()
            .filter(|event| &*event.path != b"config")
            .count(),
        0,
        "{events:?}"
    );

    // An entry that is not named is not watched, and an in-place write does
    // not change the directory.
    recorder.take();
    append(&dir.path().join("other"), b"b");
    armed.flush().unwrap();
    assert_eq!(recorder.snapshot(), Vec::new());
}

#[test]
fn kqueue_atomic_rename_over_reports_and_rewatches() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config");
    fs::write(&config, b"old").unwrap();
    let recorder = Recorder::new();
    let backend = backend();
    let armed = backend
        .arm_with(&entries(dir.path(), &["config"]), recorder.deliver())
        .unwrap();

    // What `git config` does: write a lock file, rename it over the file.
    let lock = dir.path().join("config.lock");
    fs::write(&lock, b"new").unwrap();
    fs::rename(&lock, &config).unwrap();
    let events = recorder.wait_until(
        "config replaced",
        || armed.flush().unwrap(),
        |events| has(events, "config", Flags::FILE | Flags::CREATED),
    );
    assert!(flags_of(&events, "config.lock").is_none(), "{events:?}");

    // The new file is watched: an in-place write to it is reported.
    recorder.take();
    append(&config, b"more");
    recorder.wait_until(
        "the new config modified",
        || armed.flush().unwrap(),
        |events| has(events, "config", Flags::MODIFIED),
    );
}

#[test]
fn kqueue_named_symlink_is_watched_not_followed() {
    let dir = tempfile::tempdir().unwrap();
    let watched = dir.path().join("watched");
    let outside = dir.path().join("outside");
    fs::create_dir(&watched).unwrap();
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("target"), b"a").unwrap();
    fs::write(outside.join("other"), b"a").unwrap();
    let link = watched.join("link");
    symlink(outside.join("target"), &link).unwrap();
    let recorder = Recorder::new();
    let backend = backend();
    let armed = backend
        .arm_with(&entries(&watched, &["link"]), recorder.deliver())
        .unwrap();

    // A change of the target is not a change of the link.
    append(&outside.join("target"), b"b");
    armed.flush().unwrap();
    assert_eq!(recorder.snapshot(), Vec::new());

    // Pointing the link elsewhere is.
    fs::remove_file(&link).unwrap();
    symlink(outside.join("other"), &link).unwrap();
    recorder.wait_until(
        "the link replaced",
        || armed.flush().unwrap(),
        |events| has(events, "link", Flags::SYMLINK | Flags::CREATED),
    );
}

#[test]
fn kqueue_directory_deleted_reports_root_changed() {
    let dir = tempfile::tempdir().unwrap();
    let watched = dir.path().join("watched");
    fs::create_dir(&watched).unwrap();
    fs::write(watched.join("config"), b"a").unwrap();
    let recorder = Recorder::new();
    let backend = backend();
    let armed = backend
        .arm_with(&entries(&watched, &["config"]), recorder.deliver())
        .unwrap();

    fs::remove_dir_all(&watched).unwrap();
    let events = recorder.wait_until(
        "ROOT_CHANGED",
        || armed.flush().unwrap(),
        |events| has(events, "", Flags::ROOT_CHANGED),
    );
    // The named entry went first.
    assert!(has(&events, "config", Flags::REMOVED), "{events:?}");
}

#[test]
fn kqueue_directory_renamed_reports_root_changed() {
    let dir = tempfile::tempdir().unwrap();
    let watched = dir.path().join("watched");
    fs::create_dir(&watched).unwrap();
    let recorder = Recorder::new();
    let backend = backend();
    let armed = backend
        .arm_with(&entries(&watched, &[]), recorder.deliver())
        .unwrap();

    fs::rename(&watched, dir.path().join("moved")).unwrap();
    recorder.wait_until(
        "ROOT_CHANGED",
        || armed.flush().unwrap(),
        |events| has(events, "", Flags::ROOT_CHANGED),
    );
}

#[test]
fn kqueue_named_entry_appears_and_vanishes() {
    let dir = tempfile::tempdir().unwrap();
    let recorder = Recorder::new();
    let backend = backend();
    let armed = backend
        .arm_with(&entries(dir.path(), &["later"]), recorder.deliver())
        .unwrap();

    // Other entries coming and going are not reported when names are given.
    fs::write(dir.path().join("unrelated"), b"x").unwrap();
    fs::remove_file(dir.path().join("unrelated")).unwrap();
    armed.flush().unwrap();
    assert_eq!(recorder.snapshot(), Vec::new());

    let later = dir.path().join("later");
    fs::create_dir(&later).unwrap();
    recorder.wait_until(
        "later created",
        || armed.flush().unwrap(),
        |events| has(events, "later", Flags::DIR | Flags::CREATED),
    );
    // Once it exists it is watched itself: a new entry in it changes it.
    recorder.take();
    fs::write(later.join("inside"), b"x").unwrap();
    recorder.wait_until(
        "later modified",
        || armed.flush().unwrap(),
        |events| has(events, "later", Flags::DIR | Flags::MODIFIED),
    );
    recorder.take();
    fs::remove_dir_all(&later).unwrap();
    recorder.wait_until(
        "later removed",
        || armed.flush().unwrap(),
        |events| has(events, "later", Flags::REMOVED),
    );
}

#[test]
fn kqueue_without_names_reports_the_directory() {
    let dir = tempfile::tempdir().unwrap();
    let recorder = Recorder::new();
    let backend = backend();
    let armed = backend
        .arm_with(&entries(dir.path(), &[]), recorder.deliver())
        .unwrap();

    fs::write(dir.path().join("new"), b"x").unwrap();
    let events = recorder.wait_until(
        "the directory changed",
        || armed.flush().unwrap(),
        |events| has(events, "", Flags::DIR | Flags::MODIFIED),
    );
    assert!(
        events.iter().all(|event| event.path.is_empty()),
        "{events:?}"
    );
}

#[test]
fn kqueue_missing_directory_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let backend = backend();
    let result = backend.arm_with(
        &entries(&dir.path().join("missing"), &["x"]),
        Recorder::new().deliver(),
    );
    assert_eq!(result.err(), Some(BackendError::NotFound));
}

#[test]
fn kqueue_descriptor_limit_is_exhausted() {
    let dir = tempfile::tempdir().unwrap();
    let backend = backend();
    let names: Vec<String> = (0..MAX_DESCRIPTORS).map(|n| format!("n{n}")).collect();
    let all: Vec<&str> = names.iter().map(String::as_str).collect();
    // The directory plus 256 names is one too many.
    let result = backend.arm_with(&entries(dir.path(), &all), Recorder::new().deliver());
    assert_eq!(result.err(), Some(BackendError::Exhausted));

    let half = &all[..MAX_DESCRIPTORS / 2];
    let first = backend
        .arm_with(&entries(dir.path(), half), Recorder::new().deliver())
        .unwrap();
    let result = backend.arm_with(&entries(dir.path(), half), Recorder::new().deliver());
    assert_eq!(result.err(), Some(BackendError::Exhausted));
    // Dropping a watch gives its descriptors back.
    drop(first);
    let second = backend.arm_with(&entries(dir.path(), half), Recorder::new().deliver());
    assert!(second.is_ok());
}

#[test]
fn kqueue_no_callbacks_after_drop() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config");
    fs::write(&config, b"a").unwrap();
    let recorder = Recorder::new();
    let backend = backend();
    let armed = backend
        .arm_with(&entries(dir.path(), &["config"]), recorder.deliver())
        .unwrap();

    let stop = Arc::new(AtomicBool::new(false));
    let writer = {
        let config = config.clone();
        let stop = stop.clone();
        thread::spawn(move || {
            while !stop.load(SeqCst) {
                append(&config, b"x");
                thread::sleep(Duration::from_millis(1));
            }
        })
    };
    recorder.wait_until(
        "the writer's events",
        || armed.flush().unwrap(),
        |events| has(events, "config", Flags::MODIFIED),
    );
    drop(armed);
    recorder.close();
    thread::sleep(Duration::from_millis(300));
    stop.store(true, SeqCst);
    writer.join().unwrap();
    assert_eq!(recorder.late(), 0);
}

#[test]
fn kqueue_watches_share_one_thread_and_stay_apart() {
    let dir = tempfile::tempdir().unwrap();
    let one = dir.path().join("one");
    let two = dir.path().join("two");
    fs::create_dir(&one).unwrap();
    fs::create_dir(&two).unwrap();
    fs::write(one.join("f"), b"1").unwrap();
    fs::write(two.join("f"), b"2").unwrap();
    let backend = backend();
    let first = Recorder::new();
    let second = Recorder::new();
    let a = backend
        .arm_with(&entries(&one, &["f"]), first.deliver())
        .unwrap();
    let b = backend
        .arm_with(&entries(&two, &["f"]), second.deliver())
        .unwrap();

    append(&one.join("f"), b"x");
    first.wait_until(
        "one/f",
        || a.flush().unwrap(),
        |events| has(events, "f", Flags::MODIFIED),
    );
    b.flush().unwrap();
    assert_eq!(second.snapshot(), Vec::new());

    drop(a);
    append(&two.join("f"), b"x");
    second.wait_until(
        "two/f",
        || b.flush().unwrap(),
        |events| has(events, "f", Flags::MODIFIED),
    );
}
