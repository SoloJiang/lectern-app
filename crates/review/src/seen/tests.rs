use std::sync::Arc;

use lectern_services::changes::{
    BaseSpec, ChangeDetail, ChangePath, ChangeStatus, FileChange, FileMode, FileSide, SideIdentity,
};
use lectern_services::project::BlobId;
use lectern_services::review::{FileKey, Progress, SeenState};
use lectern_services::session::Timestamp;

use super::{SeenKey, TargetKey, looks_good, mark_seen, mark_unseen, progress, state};
use crate::log::Index;

fn path(p: &str) -> ChangePath {
    ChangePath(Arc::from(p.as_bytes()))
}

fn blob(n: u8) -> SideIdentity {
    SideIdentity::Blob(BlobId::Sha1([n; 20]))
}

fn file(content: u8) -> Option<FileSide> {
    Some(FileSide {
        mode: FileMode::File,
        content: blob(content),
    })
}

fn session(id: &str) -> TargetKey {
    TargetKey::Session {
        source: Arc::from("session-claude-code"),
        id: Arc::from(id),
    }
}

fn change(
    status: ChangeStatus,
    p: &str,
    old: Option<FileSide>,
    new: Option<FileSide>,
) -> FileChange {
    FileChange {
        status,
        path: path(p),
        old,
        new,
        detail: ChangeDetail::NoDiff,
        generated: false,
        untracked: false,
        staged: None,
    }
}

fn modified(p: &str, old: u8, new: u8) -> FileChange {
    change(ChangeStatus::Modified, p, file(old), file(new))
}

fn added(p: &str, new: u8) -> FileChange {
    change(ChangeStatus::Added, p, None, file(new))
}

fn deleted(p: &str, old: u8) -> FileChange {
    change(ChangeStatus::Deleted, p, file(old), None)
}

fn renamed(from: &str, to: &str, old: u8, new: u8) -> FileChange {
    let status = ChangeStatus::Renamed {
        from: path(from),
        similarity: 90,
    };
    change(status, to, file(old), file(new))
}

const T1: Timestamp = Timestamp(1_760_000_000_000);
const T2: Timestamp = Timestamp(1_760_000_100_000);

fn seen(index: &mut Index, target: &TargetKey, change: &FileChange, at: Timestamp) {
    index.record(mark_seen(SeenKey::new(target.clone(), change), at));
}

fn of(index: &Index, target: &TargetKey, change: &FileChange) -> SeenState {
    state(index, &SeenKey::new(target.clone(), change))
}

#[test]
fn seen_three_states() {
    let s = session("a");
    let mut index = Index::default();
    assert_eq!(
        of(&index, &s, &modified("src/a.rs", 1, 2)),
        SeenState::Unseen
    );
    seen(&mut index, &s, &modified("src/a.rs", 1, 2), T1);
    assert_eq!(of(&index, &s, &modified("src/a.rs", 1, 2)), SeenState::Seen);
    assert_eq!(
        of(&index, &s, &modified("src/a.rs", 1, 3)),
        SeenState::ChangedSinceSeen { since: T1 }
    );
    // Marking the new change seen replaces the mark.
    seen(&mut index, &s, &modified("src/a.rs", 1, 3), T2);
    assert_eq!(of(&index, &s, &modified("src/a.rs", 1, 3)), SeenState::Seen);
    assert_eq!(
        of(&index, &s, &modified("src/a.rs", 1, 2)),
        SeenState::ChangedSinceSeen { since: T2 }
    );
}

#[test]
fn seen_every_key_value_counts() {
    let s = session("a");
    let mut index = Index::default();
    let base = modified("src/a.rs", 1, 2);
    seen(&mut index, &s, &base, T1);
    let changed = SeenState::ChangedSinceSeen { since: T1 };

    let mut old_side = base.clone();
    old_side.old = file(9);
    assert_eq!(of(&index, &s, &old_side), changed);

    let mut mode = base.clone();
    mode.new.as_mut().unwrap().mode = FileMode::Executable;
    assert_eq!(of(&index, &s, &mode), changed);

    let mut identity = base.clone();
    identity.new.as_mut().unwrap().content = SideIdentity::Unhashed {
        size: 10,
        mtime_ns: 5,
        inode: 7,
    };
    assert_eq!(of(&index, &s, &identity), changed);

    let mut status = base.clone();
    status.status = ChangeStatus::TypeChanged;
    assert_eq!(of(&index, &s, &status), changed);

    let mut conflicted = base.clone();
    conflicted.status = ChangeStatus::Conflicted;
    assert_eq!(of(&index, &s, &conflicted), changed);

    // The similarity of a rename is not part of the key.
    let r = renamed("a", "b", 1, 2);
    seen(&mut index, &s, &r, T1);
    let mut other_similarity = r.clone();
    other_similarity.status = ChangeStatus::Renamed {
        from: path("a"),
        similarity: 55,
    };
    assert_eq!(of(&index, &s, &other_similarity), SeenState::Seen);
}

#[test]
fn seen_not_retained_is_the_same_blob() {
    let s = session("a");
    let mut index = Index::default();
    let kept = modified("src/a.rs", 1, 2);
    seen(&mut index, &s, &kept, T1);
    let mut dropped = kept.clone();
    dropped.new.as_mut().unwrap().content = SideIdentity::NotRetained(BlobId::Sha1([2; 20]));
    assert_eq!(of(&index, &s, &dropped), SeenState::Seen);
}

#[test]
fn seen_rename_matches_as_a_pair() {
    let s = session("a");
    let mut index = Index::default();
    seen(&mut index, &s, &renamed("old.rs", "new.rs", 1, 2), T1);
    assert_eq!(
        of(&index, &s, &renamed("old.rs", "new.rs", 1, 2)),
        SeenState::Seen
    );
    // Edited again after the rename.
    assert_eq!(
        of(&index, &s, &renamed("old.rs", "new.rs", 1, 3)),
        SeenState::ChangedSinceSeen { since: T1 }
    );
    // The target path now holds a plain modification of it.
    assert_eq!(
        of(&index, &s, &modified("new.rs", 2, 3)),
        SeenState::ChangedSinceSeen { since: T1 }
    );
}

#[test]
fn seen_old_path_taken_by_an_unrelated_file_is_unseen() {
    let s = session("a");
    let mut index = Index::default();
    seen(&mut index, &s, &renamed("a.rs", "b.rs", 1, 2), T1);
    assert_eq!(of(&index, &s, &added("a.rs", 7)), SeenState::Unseen);
    // The rename itself is still seen.
    assert_eq!(
        of(&index, &s, &renamed("a.rs", "b.rs", 1, 2)),
        SeenState::Seen
    );
}

#[test]
fn seen_deleted_then_recreated_is_unseen() {
    let s = session("a");
    let mut index = Index::default();
    seen(&mut index, &s, &deleted("a.rs", 1), T1);
    assert_eq!(of(&index, &s, &deleted("a.rs", 1)), SeenState::Seen);
    assert_eq!(of(&index, &s, &added("a.rs", 2)), SeenState::Unseen);
}

#[test]
fn seen_then_renamed_or_deleted_is_changed() {
    let s = session("a");
    let mut index = Index::default();
    seen(&mut index, &s, &modified("a.rs", 1, 2), T1);
    let changed = SeenState::ChangedSinceSeen { since: T1 };
    assert_eq!(of(&index, &s, &renamed("a.rs", "b.rs", 1, 2)), changed);
    assert_eq!(of(&index, &s, &deleted("a.rs", 1)), changed);
    // A new file seen, then edited again.
    seen(&mut index, &s, &added("n.rs", 1), T1);
    assert_eq!(of(&index, &s, &added("n.rs", 2)), changed);
}

#[test]
fn seen_unseen_of_a_rename_is_unseen() {
    let s = session("a");
    let r = renamed("a.rs", "b.rs", 2, 3);
    let unseen = |index: &mut Index| {
        let file = SeenKey::new(s.clone(), &r).file();
        index.record(mark_unseen(s.clone(), file, T2));
    };
    // Seen, renamed, seen again, then unseen.
    let mut index = Index::default();
    seen(&mut index, &s, &modified("a.rs", 1, 2), T1);
    let changed = SeenState::ChangedSinceSeen { since: T1 };
    assert_eq!(of(&index, &s, &r), changed);
    seen(&mut index, &s, &r, T2);
    assert_eq!(of(&index, &s, &r), SeenState::Seen);
    unseen(&mut index);
    assert_eq!(of(&index, &s, &r), SeenState::Unseen);
    // Unseen straight from changed since seen.
    let mut index = Index::default();
    seen(&mut index, &s, &modified("a.rs", 1, 2), T1);
    unseen(&mut index);
    assert_eq!(of(&index, &s, &r), SeenState::Unseen);
}

#[test]
fn seen_new_file_at_the_old_path_after_a_seen_rename_is_unseen() {
    let s = session("a");
    let mut index = Index::default();
    seen(&mut index, &s, &modified("a.rs", 1, 2), T1);
    seen(&mut index, &s, &renamed("a.rs", "b.rs", 2, 3), T2);
    assert_eq!(of(&index, &s, &added("a.rs", 7)), SeenState::Unseen);
    let r = renamed("a.rs", "b.rs", 2, 3);
    assert_eq!(of(&index, &s, &r), SeenState::Seen);
}

#[test]
fn seen_most_recent_mark_gives_since() {
    let s = session("a");
    let mut index = Index::default();
    seen(&mut index, &s, &modified("a.rs", 1, 2), T1);
    seen(&mut index, &s, &modified("b.rs", 1, 2), T2);
    // a.rs renamed onto b.rs: both marks are of the same change.
    assert_eq!(
        of(&index, &s, &renamed("a.rs", "b.rs", 1, 3)),
        SeenState::ChangedSinceSeen { since: T2 }
    );
}

#[test]
fn seen_unseen_cancels_the_mark() {
    let s = session("a");
    let mut index = Index::default();
    let c = modified("a.rs", 1, 2);
    seen(&mut index, &s, &c, T1);
    let key = SeenKey::new(s.clone(), &c);
    index.record(mark_unseen(s.clone(), key.file(), T2));
    assert_eq!(of(&index, &s, &c), SeenState::Unseen);
    // Not "changed since seen" either once the content changes.
    assert_eq!(of(&index, &s, &modified("a.rs", 1, 3)), SeenState::Unseen);
    seen(&mut index, &s, &c, T2);
    assert_eq!(of(&index, &s, &c), SeenState::Seen);
}

#[test]
fn seen_review_targets_are_independent() {
    let a = session("a");
    let c = modified("a.rs", 1, 2);
    let mut index = Index::default();
    seen(&mut index, &a, &c, T1);
    let others = [
        session("b"),
        TargetKey::Session {
            source: Arc::from("session-codex"),
            id: Arc::from("a"),
        },
        TargetKey::Outside {
            worktree: Arc::from(""),
        },
        TargetKey::Range {
            worktree: Arc::from(""),
            base: BaseSpec::MergeBase(Arc::from("refs/heads/main")),
        },
        TargetKey::Range {
            worktree: Arc::from("worktrees/feature"),
            base: BaseSpec::MergeBase(Arc::from("refs/heads/main")),
        },
    ];
    for t in &others {
        assert_eq!(of(&index, t, &c), SeenState::Unseen, "{t:?}");
    }
    let range = TargetKey::Range {
        worktree: Arc::from(""),
        base: BaseSpec::Head,
    };
    seen(&mut index, &range, &c, T1);
    let other_base = TargetKey::Range {
        worktree: Arc::from(""),
        base: BaseSpec::Commit(BlobId::Sha1([3; 20])),
    };
    assert_eq!(of(&index, &range, &c), SeenState::Seen);
    assert_eq!(of(&index, &other_base, &c), SeenState::Unseen);
    assert_eq!(of(&index, &a, &c), SeenState::Seen);
}

#[test]
fn seen_looks_good_marks_the_rest() {
    let s = session("a");
    let files = [
        modified("a.rs", 1, 2),
        modified("b.rs", 1, 2),
        renamed("c.rs", "d.rs", 1, 2),
        deleted("e.rs", 1),
    ];
    let keys: Vec<SeenKey> = files.iter().map(|c| SeenKey::new(s.clone(), c)).collect();
    let mut index = Index::default();
    seen(&mut index, &s, &files[0], T1);
    seen(&mut index, &s, &modified("b.rs", 1, 9), T1);
    let states: Vec<SeenState> = keys.iter().map(|k| state(&index, k)).collect();
    assert_eq!(
        progress(&states),
        Progress {
            total: 4,
            seen: 1,
            changed_since_seen: 1
        }
    );

    let records = looks_good(&index, &keys, T2);
    assert_eq!(records.len(), 3);
    for r in records {
        index.record(r);
    }
    let states: Vec<SeenState> = keys.iter().map(|k| state(&index, k)).collect();
    assert!(states.iter().all(|s| *s == SeenState::Seen));
    assert_eq!(
        progress(&states),
        Progress {
            total: 4,
            seen: 4,
            changed_since_seen: 0
        }
    );
    // Nothing left to mark.
    assert!(looks_good(&index, &keys, T2).is_empty());
}

#[test]
fn seen_file_keys() {
    let key = |c: &FileChange| SeenKey::new(session("a"), c).file();
    let k = |old: Option<&str>, new: Option<&str>| FileKey {
        old: old.map(path),
        new: new.map(path),
    };
    assert_eq!(key(&added("a", 1)), k(None, Some("a")));
    assert_eq!(key(&deleted("a", 1)), k(Some("a"), None));
    assert_eq!(key(&modified("a", 1, 2)), k(Some("a"), Some("a")));
    assert_eq!(key(&renamed("a", "b", 1, 2)), k(Some("a"), Some("b")));
}
