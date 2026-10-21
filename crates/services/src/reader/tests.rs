use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use lectern_kernel::{Event, Service, service};

use super::*;

/// A `Reader` that opens nothing, to check the trait is usable as a shared
/// trait object.
struct Empty;

impl Reader for Empty {
    fn open(&self, _target: ReadTarget) -> OpenTicket {
        OpenTicket(1)
    }
    fn prefetch(&self, _targets: Vec<ReadTarget>) {}
    fn share_object(&self, _oid: BlobId, _bytes: Arc<[u8]>) -> ObjectShare {
        ObjectShare::new(|| {})
    }
}

fn path(p: &str) -> Arc<Path> {
    Arc::from(PathBuf::from(p))
}

fn shareable<T: Send + Sync + 'static>() {}

fn sendable<T: Send + 'static>() {}

/// Serializes to JSON (the process plugin protocol is JSON-RPC, ADR 0018)
/// and back.
fn round_trip<T>(value: &T) -> T
where
    T: serde::Serialize + serde::de::DeserializeOwned,
{
    let json = serde_json::to_string(value).expect("serializes");
    serde_json::from_str(&json).expect("deserializes")
}

fn target(origin: TextOrigin, expect: Option<BlobId>, at: Anchor) -> ReadTarget {
    ReadTarget {
        project: ProjectKey::Git {
            common_dir: path("/r/.git"),
        },
        worktree: Some(WorktreeKey(path("/r/.git"))),
        path: Arc::from("src/main.rs"),
        origin,
        expect,
        at,
    }
}

#[test]
fn reader_service_id_and_multiple() {
    assert_eq!(<dyn Reader as Service>::ID, "reader");
    const { assert!(!<dyn Reader as Service>::MULTIPLE) };
    let key = service::<dyn Reader>();
    assert_eq!(key.id(), "reader");
    assert!(!key.multiple());
}

#[test]
fn reader_is_a_shared_trait_object() {
    let reader: Arc<dyn Reader> = Arc::new(Empty);
    let t = target(TextOrigin::Worktree, None, Anchor::Start);
    reader.prefetch(vec![t.clone()]);
    assert_eq!(reader.open(t), OpenTicket(1));
    drop(reader.share_object(BlobId::Sha1([1; 20]), Arc::from(&b"fn main() {}"[..])));
    // Targets cross threads; the registration is handed to the owner.
    shareable::<Arc<dyn Reader>>();
    shareable::<ReadTarget>();
    sendable::<ObjectShare>();
}

#[test]
fn object_share_releases_once_on_drop() {
    let count = Arc::new(AtomicUsize::new(0));
    let c = count.clone();
    let share = ObjectShare::new(move || {
        c.fetch_add(1, Ordering::SeqCst);
    });
    assert_eq!(count.load(Ordering::SeqCst), 0);
    assert_eq!(format!("{share:?}"), "ObjectShare { .. }");
    // Dropped on another thread, as the review view's state may be.
    std::thread::spawn(move || drop(share)).join().unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[test]
fn reader_targets_round_trip() {
    let oid = BlobId::Sha1([0xab; 20]);
    let targets = [
        target(TextOrigin::Worktree, None, Anchor::Start),
        target(
            TextOrigin::Worktree,
            Some(oid),
            Anchor::Line {
                line: 41,
                column: 0,
            },
        ),
        target(
            TextOrigin::Object,
            Some(BlobId::Sha256([0xcd; 32])),
            Anchor::Byte {
                byte: u64::MAX,
                line: u32::MAX,
                column: 7,
            },
        ),
        ReadTarget {
            project: ProjectKey::Folder { root: path("/f") },
            worktree: None,
            path: Arc::from("notes.md"),
            ..target(TextOrigin::Worktree, None, Anchor::Start)
        },
    ];
    for t in targets {
        assert_eq!(round_trip(&t), t);
    }
}

#[test]
fn reader_event_round_trips() {
    fn event<E: Event + Clone>() {}
    event::<ReaderEvent>();

    for outcome in [OpenOutcome::Shown, OpenOutcome::Cancelled] {
        let e = ReaderEvent::Open {
            ticket: OpenTicket(9),
            outcome,
        };
        assert_eq!(round_trip(&e), e);
    }
    assert_eq!(round_trip(&OpenTicket(u64::MAX)), OpenTicket(u64::MAX));
}
