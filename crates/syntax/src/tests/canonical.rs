//! `canonical_*`: the canonical-content OID of a worktree file of a git
//! project (`DocHandle::canonical_oid`): asked once per content OID, answered
//! in a `Background` task over the whole text, dropped when the content has
//! changed, and never for what has none.

use lectern_git::{HashKind, Oid};
use lectern_services::reader::Anchor;

use super::rig::{Rig, modified};
use crate::git_access::blob_id;
use crate::{Canonical, DocState};

fn blob(bytes: &[u8]) -> lectern_services::project::BlobId {
    blob_id(&Oid::for_blob(HashKind::Sha1, bytes))
}

#[test]
fn canonical_is_not_asked_until_asked_and_follows_the_conversion() {
    let rig = Rig::new();
    rig.write("a.txt", b"one\r\ntwo\r\n");
    let (doc, before) = rig.open_settled(rig.target("a.txt", Anchor::Start));
    assert_eq!(before.state, DocState::Ready);
    assert_eq!(before.canonical, Canonical::NotAsked);
    assert!(rig.git.canonical_calls.lock().unwrap().is_empty());

    doc.canonical_oid();
    // Asking again before the answer, and after it, does nothing more.
    doc.canonical_oid();
    rig.settle();
    doc.canonical_oid();
    rig.settle();
    let after = doc.snapshot();
    assert_eq!(after.canonical, Canonical::Known(blob(b"one\ntwo\n")));
    assert_ne!(after.oid, Some(blob(b"one\ntwo\n")), "the raw OID differs");
    assert_eq!(
        *rig.git.canonical_calls.lock().unwrap(),
        vec![("a.txt".to_owned(), 10)]
    );
}

#[test]
fn canonical_with_an_external_filter_is_unavailable() {
    let rig = Rig::new();
    rig.write("a.txt", b"payload\n");
    *rig.git.canonical.lock().unwrap() = Some(Ok(None));
    let (doc, _) = rig.open_settled(rig.target("a.txt", Anchor::Start));
    doc.canonical_oid();
    rig.settle();
    assert_eq!(doc.snapshot().canonical, Canonical::Unavailable);
}

#[test]
fn canonical_after_a_git_error_is_unavailable() {
    let rig = Rig::new();
    rig.write("a.txt", b"payload\n");
    *rig.git.canonical.lock().unwrap() = Some(Err("broken index".to_owned()));
    let (doc, _) = rig.open_settled(rig.target("a.txt", Anchor::Start));
    doc.canonical_oid();
    rig.settle();
    assert_eq!(doc.snapshot().canonical, Canonical::Unavailable);
}

#[test]
fn canonical_is_asked_for_each_content() {
    let rig = Rig::new();
    rig.write("a.txt", b"one\r\n");
    let (doc, first) = rig.open_settled(rig.target("a.txt", Anchor::Start));
    doc.canonical_oid();
    rig.settle();
    assert_eq!(doc.snapshot().canonical, Canonical::Known(blob(b"one\n")));

    // The file changes on disk: the document reads it again, and the answer
    // for the old content is not the new content's.
    rig.write("a.txt", b"two\r\n");
    rig.touch("a.txt", modified());
    rig.settle();
    let second = doc.snapshot();
    assert_ne!(second.oid, first.oid);
    assert_eq!(second.canonical, Canonical::NotAsked);
    doc.canonical_oid();
    rig.settle();
    assert_eq!(doc.snapshot().canonical, Canonical::Known(blob(b"two\n")));
    assert_eq!(rig.git.canonical_calls.lock().unwrap().len(), 2);
}

#[test]
fn canonical_is_only_for_worktree_files_of_git_projects() {
    let rig = Rig::new();
    rig.write("a.txt", b"one\r\n");
    // An object of the repository: the caller's OID is the content's.
    let oid = rig.git.add_blob(b"one\r\n");
    let (object, _) = rig.open_settled(rig.object("a.txt", oid, Anchor::Start));
    object.canonical_oid();
    rig.settle();
    assert_eq!(object.snapshot().canonical, Canonical::NotAsked);
    assert!(rig.git.canonical_calls.lock().unwrap().is_empty());
}

#[test]
fn canonical_of_a_document_read_in_pages_is_unavailable() {
    let rig = Rig::new();
    // Over 16 MiB: tier C, read in pages, no whole text to convert.
    let big = vec![b'x'; (crate::limits::TIER_B_MAX + 4096) as usize];
    rig.write("big.txt", &big);
    let (doc, snapshot) = rig.open_settled(rig.target("big.txt", Anchor::Start));
    assert_eq!(snapshot.tier, Some(crate::Tier::C));
    doc.canonical_oid();
    rig.settle();
    assert_eq!(doc.snapshot().canonical, Canonical::Unavailable);
    assert!(rig.git.canonical_calls.lock().unwrap().is_empty());
}
