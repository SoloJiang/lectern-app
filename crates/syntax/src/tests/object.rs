//! `object_*`: object database content (ADR 0042). A registered OID opens
//! without a blob read; the registry holds the content only while the
//! handle lives; content up to 16 MiB nobody holds is taken out whole on
//! the first screen; more is unreadable.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use lectern_services::reader::Anchor;

use super::rig::Rig;
use crate::{DocState, Editable, Highlighting, NotEditable, ReadError, Tier, testing};

fn blob_calls(rig: &Rig) -> usize {
    rig.git.blob_calls.load(Ordering::SeqCst)
}

#[test]
fn object_registered_content_opens_without_a_blob_read() {
    let rig = Rig::new();
    let bytes: Arc<[u8]> = testing::ts(300 << 10, 0).into();
    let oid = rig.git.add_blob(&bytes);
    let share = rig.engine.share_object(oid, bytes.clone());
    let (doc, s) = rig.open_settled(rig.object(
        "old.ts",
        oid,
        Anchor::Byte {
            byte: 150_000,
            line: 0,
            column: 0,
        },
    ));
    assert_eq!(blob_calls(&rig), 0);
    assert_eq!(s.state, DocState::Ready);
    assert_eq!(s.oid, Some(oid));
    assert_eq!(s.size, bytes.len() as u64);
    assert_eq!(s.tier, Some(Tier::A));
    assert_eq!(s.highlight, Highlighting::On);
    assert_eq!(s.editable, Editable::No(NotEditable::ReadOnlySource));
    // The text is the registered buffer, not a copy.
    let crate::doc::content::Content::Text(text) = &s.content else {
        panic!("full content");
    };
    assert!(Arc::ptr_eq(text.shared_bytes(), &bytes));
    // Unregistering leaves the open document as it is.
    drop(share);
    assert!(rig.engine.inner().shares.is_empty());
    assert_eq!(doc.snapshot().size, bytes.len() as u64);
}

#[test]
fn object_registry_holds_the_content_only_while_the_handle_lives() {
    let rig = Rig::new();
    let bytes: Arc<[u8]> = Arc::from(&b"shared content\n"[..]);
    let oid = rig.git.add_blob(&bytes);
    let before = Arc::strong_count(&bytes);
    let first = rig.engine.share_object(oid, bytes.clone());
    let second = rig.engine.share_object(oid, bytes.clone());
    let shares = &rig.engine.inner().shares;
    assert_eq!(shares.count(&oid), 2);
    assert_eq!(Arc::strong_count(&bytes), before + 2);
    // Dropped on another thread; one registration left, still used.
    std::thread::scope(|s| {
        s.spawn(move || drop(first));
    });
    assert_eq!(shares.count(&oid), 1);
    assert_eq!(Arc::strong_count(&bytes), before + 1);
    let (doc, s) = rig.open_settled(rig.object("a.txt", oid, Anchor::Start));
    assert_eq!(s.state, DocState::Ready);
    assert_eq!(blob_calls(&rig), 0);
    drop(doc);
    rig.settle();
    drop(second);
    assert!(shares.is_empty());
    // Only the LRU's text (at most 1 MiB, within its 8 MiB) and the caller
    // hold it now; the registry holds nothing.
    let lru_holds = usize::from(rig.engine.lru_len() > 0);
    assert_eq!(Arc::strong_count(&bytes), before + lru_holds);
}

#[test]
fn object_content_nobody_holds_is_taken_out_on_the_first_screen() {
    let rig = Rig::new();
    // Up to 1 MiB: the LRU keeps the text.
    let bytes = testing::json(900 << 10, 0);
    let oid = rig.git.add_blob(&bytes);
    let first = rig.engine.first_screen(
        rig.object("package-lock.json", oid, Anchor::Start),
        rig.root.clone(),
        super::rig::VIEW,
    );
    assert_eq!(blob_calls(&rig), 1);
    assert_eq!(first.state, DocState::Ready);
    assert_eq!(first.highlight, Highlighting::On);
    // Opened in full, then again: the LRU's text, no second read.
    let (doc, s) = rig.open_settled(rig.object("package-lock.json", oid, Anchor::Start));
    assert_eq!(s.oid, Some(oid));
    assert_eq!(blob_calls(&rig), 2);
    drop(doc);
    rig.settle();
    let (_again, t) = rig.open_settled(rig.object("package-lock.json", oid, Anchor::Start));
    assert_eq!(t.state, DocState::Ready);
    assert_eq!(blob_calls(&rig), 2);
}

#[test]
fn object_over_sixteen_mebibytes_nobody_holds_is_unreadable() {
    let rig = Rig::new();
    let oid = rig.git.add_blob(&vec![b'a'; (16 << 20) + 1]);
    let (_d, s) = rig.open_settled(rig.object("big.txt", oid, Anchor::Start));
    assert_eq!(s.state, DocState::Unreadable(ReadError::ObjectTooLarge));
    let missing = crate::git_access::blob_id(&lectern_git::Oid::for_blob(
        lectern_git::HashKind::Sha1,
        b"absent",
    ));
    let (_e, m) = rig.open_settled(rig.object("gone.txt", missing, Anchor::Start));
    assert_eq!(m.state, DocState::Unreadable(ReadError::ObjectMissing));
    let mut no_oid = rig.object("x.txt", missing, Anchor::Start);
    no_oid.expect = None;
    let (_f, n) = rig.open_settled(no_oid);
    assert_eq!(n.state, DocState::Unreadable(ReadError::ObjectMissing));
}
