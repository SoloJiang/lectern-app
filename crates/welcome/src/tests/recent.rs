//! The recent list: the pure operations, the file format, the entry for an
//! opened project (docs/design/welcome.md「测试」纯函数).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::support::*;
use crate::recent::store::{self, Read, Unreadable, WriteStep};
use crate::recent::{
    DecodeError, MAX_ENTRIES, RecentEntry, decode, encode, entry_from_snapshot, push_front, remove,
};

fn entry(path: &str, last_opened: i64) -> RecentEntry {
    RecentEntry {
        path: PathBuf::from(path),
        key: folder_key(path),
        name: Arc::from(path.rsplit('/').next().unwrap()),
        last_opened,
        missing: false,
        sessions: 0,
        running: 0,
    }
}

fn paths_of(entries: &[RecentEntry]) -> Vec<&str> {
    entries.iter().map(|e| e.path.to_str().unwrap()).collect()
}

#[test]
fn push_front_dedupes_by_path_and_caps_at_10() {
    let mut entries = Vec::new();
    for i in 0..12 {
        push_front(&mut entries, entry(&format!("/p/{i}"), i));
    }
    assert_eq!(entries.len(), MAX_ENTRIES);
    assert_eq!(entries[0].path, Path::new("/p/11"));
    assert_eq!(
        entries[9].path,
        Path::new("/p/2"),
        "the two oldest are gone"
    );

    let mut again = entry("/p/5", 100);
    again.name = Arc::from("renamed");
    again.missing = true;
    push_front(&mut entries, again);
    assert_eq!(entries.len(), MAX_ENTRIES, "the same path does not add one");
    assert_eq!(entries[0].path, Path::new("/p/5"));
    assert_eq!(&*entries[0].name, "renamed");
    assert_eq!(entries[0].last_opened, 100);
    assert!(!entries[0].missing, "an open that succeeded clears missing");
    assert_eq!(
        entries
            .iter()
            .filter(|e| e.path == Path::new("/p/5"))
            .count(),
        1
    );
}

#[test]
fn push_front_keeps_one_entry_per_path_not_per_key() {
    let key = git_key("/repos/app/.git");
    let mut entries = Vec::new();
    let mut a = entry("/repos/app", 1);
    a.key = key.clone();
    let mut b = entry("/repos/app/crates/x", 2);
    b.key = key;
    push_front(&mut entries, a);
    push_front(&mut entries, b);
    assert_eq!(
        paths_of(&entries),
        ["/repos/app/crates/x", "/repos/app"],
        "two places in one repository are two entries"
    );
}

#[test]
fn remove_keeps_order() {
    let mut entries = vec![entry("/a", 3), entry("/b", 2), entry("/c", 1)];
    assert!(remove(&mut entries, Path::new("/b")));
    assert_eq!(paths_of(&entries), ["/a", "/c"]);
    assert!(!remove(&mut entries, Path::new("/b")), "already gone");
    assert_eq!(paths_of(&entries), ["/a", "/c"]);
}

#[test]
fn decode_rejects_unknown_version() {
    assert_eq!(
        decode(FIXTURE_BAD_VERSION).unwrap_err(),
        DecodeError::Version(2)
    );
    assert_eq!(decode(FIXTURE_GARBAGE).unwrap_err(), DecodeError::Json);
    assert_eq!(decode(b"[]").unwrap_err(), DecodeError::Json);
    assert_eq!(decode(b"").unwrap_err(), DecodeError::Json);
}

#[test]
fn decode_ignores_unknown_fields() {
    let entries = decode(FIXTURE_V1).unwrap();
    assert_eq!(
        paths_of(&entries),
        [
            "/Users/example/work/lectern",
            "/Users/example/notes",
            "/Users/example/work/api/services/billing"
        ]
    );
    assert_eq!(&*entries[2].name, "api");
    assert_eq!(entries[2].key, git_key("/Users/example/work/api/.git"));
    assert_eq!(entries[1].key, folder_key("/Users/example/notes"));
    assert!(
        entries
            .iter()
            .all(|e| !e.missing && e.sessions == 0 && e.running == 0),
        "the memory-only fields are not in the file"
    );
}

#[test]
fn decode_dedupes_and_caps_a_hand_edited_file() {
    let mut entries = Vec::new();
    for i in 0..14 {
        entries.push(entry(&format!("/p/{}", i % 12), i));
    }
    let (bytes, _) = encode(&entries);
    let decoded = decode(&bytes).unwrap();
    assert_eq!(decoded.len(), MAX_ENTRIES);
    assert_eq!(decoded[0].path, Path::new("/p/0"));
    assert_eq!(
        decoded
            .iter()
            .filter(|e| e.path == Path::new("/p/0"))
            .count(),
        1
    );
}

#[test]
fn encode_roundtrips() {
    let entries = decode(FIXTURE_V1).unwrap();
    let (bytes, skipped) = encode(&entries);
    assert_eq!(skipped, 0);
    assert_eq!(decode(&bytes).unwrap(), entries);
    insta::assert_snapshot!(String::from_utf8(bytes).unwrap());
}

#[test]
fn encode_leaves_out_memory_only_fields() {
    let mut e = entry("/p/a", 1);
    e.missing = true;
    e.sessions = 7;
    e.running = 2;
    let (bytes, _) = encode(&[e]);
    let text = String::from_utf8(bytes).unwrap();
    assert!(!text.contains("missing") && !text.contains("sessions") && !text.contains("running"));
}

#[cfg(unix)]
#[test]
fn encode_skips_entries_whose_path_is_not_utf8() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let bad = PathBuf::from(OsString::from_vec(b"/p/\xff\xfe".to_vec()));
    let mut odd = entry("/p/odd", 2);
    odd.path = bad;
    let kept = entry("/p/ok", 1);
    let (bytes, skipped) = encode(&[odd, kept.clone()]);
    assert_eq!(skipped, 1);
    assert_eq!(decode(&bytes).unwrap(), vec![kept]);
}

#[test]
fn entry_from_snapshot_joins_root_and_opened_at() {
    let git = git_key("/repos/app/.git");
    // The root opened.
    let e = entry_from_snapshot(&snapshot(&git, "app", "/repos/app", ""), NOW);
    assert_eq!(e.path, Path::new("/repos/app"));
    assert_eq!(
        (e.key.clone(), &*e.name, e.last_opened),
        (git.clone(), "app", NOW)
    );
    // A subdirectory opened: back to the same place.
    let e = entry_from_snapshot(&snapshot(&git, "app", "/repos/app", "crates/review"), NOW);
    assert_eq!(e.path, Path::new("/repos/app/crates/review"));
    // A linked worktree: its root, not the main one.
    let e = entry_from_snapshot(&snapshot(&git, "app", "/repos/app-feature", "docs"), NOW);
    assert_eq!(e.path, Path::new("/repos/app-feature/docs"));
    // A bare repository has no current worktree: the common directory.
    let bare = git_key("/repos/bare.git");
    let e = entry_from_snapshot(&bare_snapshot(&bare, "bare"), NOW);
    assert_eq!(e.path, Path::new("/repos/bare.git"));
    // A folder project.
    let folder = folder_key("/notes");
    let e = entry_from_snapshot(&snapshot(&folder, "notes", "/notes", ""), NOW);
    assert_eq!(e.path, Path::new("/notes"));
}

#[test]
fn store_reads_what_it_wrote() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("welcome");
    assert!(matches!(store::read(&dir), Read::Missing), "no file yet");
    let (bytes, _) = encode(&decode(FIXTURE_V1).unwrap());
    store::write(&dir, &bytes).unwrap();
    match store::read(&dir) {
        Read::Loaded(entries) => assert_eq!(entries.len(), 3),
        other => panic!("{other:?}"),
    }
    assert!(
        !store::testing::temp_exists(&dir),
        "the temporary file is renamed away"
    );
}

#[test]
fn store_reports_unreadable_files_by_reason() {
    let dir = tempfile::tempdir().unwrap();
    store::testing::seed(dir.path(), FIXTURE_GARBAGE);
    assert!(matches!(
        store::read(dir.path()),
        Read::Unreadable(Unreadable::Json)
    ));
    store::testing::seed(dir.path(), FIXTURE_BAD_VERSION);
    assert!(matches!(
        store::read(dir.path()),
        Read::Unreadable(Unreadable::Version(2))
    ));
}

#[test]
fn store_write_names_the_failed_step() {
    let dir = tempfile::tempdir().unwrap();
    let blocked = dir.path().join("welcome");
    store::testing::block_dir(&blocked);
    let (step, _) = store::write(&blocked, b"{}").unwrap_err();
    assert_eq!(step, WriteStep::CreateDir);
}
