use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use lectern_kernel::Service;

use super::*;

/// `Probe::<T>::IS_SERVICE` is true exactly when `T: Service`: the inherent
/// constant only applies when its bound holds, otherwise the blanket trait's
/// constant does.
struct Probe<T: ?Sized>(PhantomData<T>);

trait NotService {
    const IS_SERVICE: bool = false;
}

impl<T: ?Sized> NotService for T {}

impl<T: ?Sized + Service> Probe<T> {
    const IS_SERVICE: bool = true;
}

/// A source with one fixed change list and only the required methods, so
/// `refresh` and `fingerprint` use their default implementations.
struct Fixed {
    info: SourceInfo,
    list: Arc<ChangeList>,
    fetches: AtomicUsize,
}

impl ChangeSource for Fixed {
    fn info(&self) -> &SourceInfo {
        &self.info
    }
    fn fetch(&self, cancel: &AtomicBool) -> Result<Arc<ChangeList>, ChangeError> {
        self.fetches.fetch_add(1, Ordering::SeqCst);
        if cancel.load(Ordering::SeqCst) {
            return Err(ChangeError::Cancelled);
        }
        Ok(self.list.clone())
    }
    fn texts(
        &self,
        list: &ChangeList,
        file: u32,
        _max_bytes: u64,
    ) -> Result<SideTexts, ChangeError> {
        let not_mine = || ChangeError::Io(Arc::from("not this source's list"));
        let handle = list.handle.0.as_ref().ok_or_else(not_mine)?;
        let texts = handle
            .downcast_ref::<Vec<SideTexts>>()
            .ok_or_else(not_mine)?;
        texts
            .get(file as usize)
            .cloned()
            .ok_or_else(|| ChangeError::Io(Arc::from("no such file")))
    }
}

fn path(p: &str) -> Arc<Path> {
    Arc::from(PathBuf::from(p))
}

fn change_path(p: &[u8]) -> ChangePath {
    ChangePath(Arc::from(p))
}

fn shareable<T: Send + Sync + 'static>() {}

/// Serializes to JSON (the process plugin protocol is JSON-RPC, ADR 0018)
/// and back.
fn round_trip<T>(value: &T) -> T
where
    T: serde::Serialize + serde::de::DeserializeOwned,
{
    let json = serde_json::to_string(value).expect("serializes");
    serde_json::from_str(&json).expect("deserializes")
}

fn oid(byte: u8) -> BlobId {
    BlobId::Sha1([byte; 20])
}

fn info() -> SourceInfo {
    SourceInfo {
        kind: SourceKind::Worktree {
            project: ProjectKey::Git {
                common_dir: path("/r/.git"),
            },
            worktree: WorktreeKey(path("/r/.git")),
            base: BaseSpec::Head,
        },
        caps: Capabilities::default(),
    }
}

fn side(content: SideIdentity) -> Option<FileSide> {
    Some(FileSide {
        mode: FileMode::File,
        content,
    })
}

/// One file per status, side identity and detail.
fn files() -> Vec<FileChange> {
    let hunks: Arc<[DiffHunk]> = Arc::from(vec![
        DiffHunk {
            old: 0..0,
            new: 0..3,
            old_byte: 0,
            new_byte: 0,
        },
        DiffHunk {
            old: 10..12,
            new: 13..13,
            old_byte: 412,
            new_byte: u64::MAX,
        },
    ]);
    let base = FileChange {
        status: ChangeStatus::Modified,
        path: change_path(b"src/lib.rs"),
        old: side(SideIdentity::Blob(oid(1))),
        new: side(SideIdentity::Blob(oid(2))),
        detail: ChangeDetail::Text {
            hunks,
            added: 3,
            removed: 2,
        },
        generated: false,
        untracked: false,
        staged: None,
    };
    vec![
        FileChange {
            status: ChangeStatus::Added,
            path: change_path(b"new.txt"),
            old: None,
            new: side(SideIdentity::NotRetained(BlobId::Sha256([7; 32]))),
            detail: ChangeDetail::Truncated {
                added: 9000,
                removed: 0,
            },
            untracked: true,
            ..base.clone()
        },
        FileChange {
            status: ChangeStatus::Deleted,
            path: change_path(b"gone.bin"),
            old: side(SideIdentity::Blob(oid(3))),
            new: None,
            detail: ChangeDetail::Binary {
                old_size: Some(1 << 20),
                new_size: None,
            },
            ..base.clone()
        },
        FileChange {
            status: ChangeStatus::Renamed {
                from: change_path(b"old/name.rs"),
                similarity: 87,
            },
            path: change_path(b"new/name.rs"),
            staged: side(SideIdentity::Blob(oid(4))),
            ..base.clone()
        },
        FileChange {
            status: ChangeStatus::TypeChanged,
            path: change_path(b"link"),
            new: Some(FileSide {
                mode: FileMode::Symlink,
                content: SideIdentity::Unhashed {
                    size: 1 << 40,
                    mtime_ns: -1,
                    inode: u64::MAX,
                },
            }),
            detail: ChangeDetail::TooLarge,
            ..base.clone()
        },
        FileChange {
            status: ChangeStatus::Conflicted,
            path: change_path(b"filtered.lfs"),
            new: side(SideIdentity::Filtered(RawIdentity::Sha256([0xfe; 32]))),
            detail: ChangeDetail::Listed,
            generated: true,
            ..base.clone()
        },
        FileChange {
            path: change_path(b"huge.lfs"),
            new: side(SideIdentity::Filtered(RawIdentity::Stat {
                size: 5,
                mtime_ns: i64::MAX,
                inode: 2,
            })),
            detail: ChangeDetail::ModeOnly,
            ..base.clone()
        },
        FileChange {
            // Not UTF-8: git allows any bytes but `/` and NUL in names.
            path: change_path(b"bad\xff/name"),
            old: Some(FileSide {
                mode: FileMode::Executable,
                content: SideIdentity::Unavailable(UnavailableKind::UnsupportedType),
            }),
            detail: ChangeDetail::StagedOnly,
            ..base.clone()
        },
        FileChange {
            path: change_path(b"vendor/sub"),
            old: Some(FileSide {
                mode: FileMode::Gitlink,
                content: SideIdentity::Unavailable(UnavailableKind::NestedRepository),
            }),
            new: Some(FileSide {
                mode: FileMode::Special,
                content: SideIdentity::Unavailable(UnavailableKind::PathThroughSymlink),
            }),
            detail: ChangeDetail::NoDiff,
            ..base.clone()
        },
        FileChange {
            path: change_path(b"x"),
            old: side(SideIdentity::Unavailable(UnavailableKind::Missing)),
            new: side(SideIdentity::Unavailable(UnavailableKind::Unreadable)),
            ..base.clone()
        },
        base,
    ]
}

fn list(handle: ListHandle) -> ChangeList {
    ChangeList {
        generation: 3,
        base: BlobId::Sha256([0x4b; 32]),
        files: Arc::from(files()),
        totals: ChangeTotals {
            files: 10,
            added: u64::MAX,
            removed: 7,
            truncated: 1,
            too_large: 1,
            listed: 1,
            not_retained: 1,
            untracked_capped: true,
            invalid_paths: Arc::from(vec![Arc::from(&b"a//b"[..]), Arc::from(&b"\xff"[..])]),
        },
        handle,
    }
}

#[test]
fn change_source_is_not_a_service() {
    // Local sources are built by `review` with `git`, MR sources come from
    // `forge`'s service; neither is looked up from the kernel.
    const { assert!(!Probe::<dyn ChangeSource>::IS_SERVICE) };
    const { assert!(Probe::<dyn crate::reader::Reader>::IS_SERVICE) };
}

#[test]
fn change_source_is_a_shared_trait_object() {
    let texts = vec![SideTexts {
        old: SideText::Absent,
        new: SideText::Text(Arc::from(&b"fn main() {}\n"[..])),
    }];
    let handle = ListHandle(Some(Arc::new(texts.clone())));
    let source: Arc<dyn ChangeSource> = Arc::new(Fixed {
        info: info(),
        list: Arc::new(list(handle)),
        fetches: AtomicUsize::new(0),
    });
    assert_eq!(*source.info(), info());

    let cancel = AtomicBool::new(false);
    let first = source.fetch(&cancel).unwrap();
    // Without the `refresh` capability, refreshing fetches again.
    let hint = RefreshHint::Paths(Arc::from(vec![change_path(b"src")]));
    let second = source.refresh(&first, &hint, &cancel).unwrap();
    assert!(Arc::ptr_eq(&first, &second));
    // Without the `fingerprint` capability there is none.
    assert_eq!(source.fingerprint(&first), None);
    // Only the source that made the list reads its handle.
    assert_eq!(source.texts(&first, 0, 1 << 20).unwrap(), texts[0]);
    assert!(matches!(
        source.texts(&first, 1, 1 << 20),
        Err(ChangeError::Io(_))
    ));

    cancel.store(true, Ordering::SeqCst);
    assert_eq!(
        source.refresh(&first, &RefreshHint::Full, &cancel),
        Err(ChangeError::Cancelled)
    );

    // Lists and sources cross to executor threads.
    shareable::<Arc<dyn ChangeSource>>();
    shareable::<ChangeList>();
}

#[test]
fn change_path_as_str_only_for_utf8() {
    assert_eq!(change_path(b"src/main.rs").as_str(), Some("src/main.rs"));
    assert_eq!(
        change_path("目录/文件".as_bytes()).as_str(),
        Some("目录/文件")
    );
    assert_eq!(change_path(b"bad\xff").as_str(), None);
    // Byte order, as the list is sorted.
    assert!(change_path(b"a/b") < change_path(b"a0"));
}

#[test]
fn list_handle_is_opaque() {
    let with = list(ListHandle(Some(Arc::new(42_u32))));
    let without = list(ListHandle::default());
    // The handle is not part of the value: comparisons and `Debug` ignore it.
    assert_eq!(with, without);
    assert_eq!(format!("{:?}", with.handle), "ListHandle { .. }");
    assert_eq!(
        with.handle.0.as_ref().unwrap().downcast_ref::<u32>(),
        Some(&42)
    );
    // It is not serialized: a deserialized list has no handle.
    let json = serde_json::to_string(&with).unwrap();
    assert!(!json.contains("handle"), "{json}");
    let back: ChangeList = serde_json::from_str(&json).unwrap();
    assert!(back.handle.0.is_none());
    assert_eq!(back, with);
}

#[test]
fn change_list_round_trips() {
    let l = list(ListHandle::default());
    let back = round_trip(&l);
    assert_eq!(back, l);
    // Field by field, since `ChangeList` equality skips the handle only.
    assert_eq!(back.files, l.files);
    assert_eq!(back.totals, l.totals);
    assert_eq!(
        round_trip(&ChangeTotals::default()),
        ChangeTotals::default()
    );
}

#[test]
fn source_types_round_trip() {
    let bases = [
        BaseSpec::Head,
        BaseSpec::Commit(oid(9)),
        BaseSpec::MergeBase(Arc::from("refs/heads/main")),
    ];
    for base in bases {
        let i = SourceInfo {
            kind: SourceKind::Worktree {
                project: ProjectKey::Git {
                    common_dir: path("/r/.git"),
                },
                worktree: WorktreeKey(path("/r/.git/worktrees/w")),
                base,
            },
            caps: Capabilities {
                refresh: true,
                fingerprint: true,
            },
        };
        assert_eq!(round_trip(&i), i);
    }

    let hints = [
        RefreshHint::Paths(Arc::from(vec![change_path(b"src"), change_path(b"\xfe")])),
        RefreshHint::Full,
    ];
    for h in hints {
        assert_eq!(round_trip(&h), h);
    }

    let texts = [
        SideTexts {
            old: SideText::Absent,
            new: SideText::Text(Arc::from(&b"a\r\nb\xff"[..])),
        },
        SideTexts {
            old: SideText::TooLarge { size: u64::MAX },
            new: SideText::NotRetained,
        },
        SideTexts {
            old: SideText::Unavailable,
            new: SideText::Text(Arc::from(&b""[..])),
        },
    ];
    for t in texts {
        assert_eq!(round_trip(&t), t);
    }

    let errors = [
        ChangeError::Unsupported(Arc::from("reftable")),
        ChangeError::NoMergeBase,
        ChangeError::TooManyEntries,
        ChangeError::Io(Arc::from("permission denied")),
        ChangeError::Cancelled,
    ];
    for e in errors {
        assert_eq!(round_trip(&e), e);
    }
}
