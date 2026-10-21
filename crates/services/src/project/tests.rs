use std::path::{Path, PathBuf};
use std::sync::Arc;

use lectern_kernel::{Event, Service, service};

use super::*;

/// A `Projects` that knows nothing, to check the trait is usable as a
/// shared trait object.
struct Empty;

impl Projects for Empty {
    fn open(&self, _path: &Path) -> OpenRequest {
        OpenRequest(1)
    }
    fn close(&self, _project: &ProjectKey) {}
    fn set_active_worktree(&self, _project: &ProjectKey, _worktree: &WorktreeKey) {}
    fn snapshot(&self, _project: &ProjectKey) -> Option<Arc<ProjectSnapshot>> {
        None
    }
    fn open_projects(&self) -> Vec<ProjectKey> {
        Vec::new()
    }
    fn locate(&self, _path: &Path) -> Option<Location> {
        None
    }
    fn resolve(&self, _path: &Path) -> Result<Resolved, ResolveError> {
        Err(OpenError::NotFound)
    }
    fn resolve_package(
        &self,
        _project: &ProjectKey,
        _worktree: &WorktreeKey,
        _rel: &str,
    ) -> Result<PackageMatch, ScanError> {
        Err(ScanError::RootMissing)
    }
}

fn path(p: &str) -> Arc<Path> {
    Arc::from(PathBuf::from(p))
}

fn shareable<T: Send + Sync + 'static>() {}

#[test]
fn projects_service_id_and_multiple() {
    assert_eq!(<dyn Projects as Service>::ID, "projects");
    const { assert!(!<dyn Projects as Service>::MULTIPLE) };
    let key = service::<dyn Projects>();
    assert_eq!(key.id(), "projects");
    assert!(!key.multiple());
}

#[test]
fn projects_is_a_shared_trait_object() {
    let projects: Arc<dyn Projects> = Arc::new(Empty);
    let key = ProjectKey::Folder { root: path("/x") };
    assert_eq!(projects.open(Path::new("/x")), OpenRequest(1));
    assert!(projects.snapshot(&key).is_none());
    assert!(projects.open_projects().is_empty());
    assert!(projects.locate(Path::new("/x")).is_none());
    assert_eq!(projects.resolve(Path::new("/x")), Err(OpenError::NotFound));
    let worktree = WorktreeKey(path("/x/.git"));
    assert_eq!(
        projects.resolve_package(&key, &worktree, ""),
        Err(ScanError::RootMissing)
    );
    // Snapshots and lookup results cross threads inside `Arc`s.
    shareable::<Arc<dyn Projects>>();
    shareable::<ProjectSnapshot>();
    shareable::<Location>();
    shareable::<Resolved>();
}

#[test]
fn project_event_shape() {
    fn event<E: Event + Clone>() {}
    event::<ProjectEvent>();

    let parts = ChangedParts::default();
    assert!(!parts.worktrees && !parts.active && !parts.files && !parts.packages && !parts.dirs);

    let key = ProjectKey::Git {
        common_dir: path("/r/.git"),
    };
    let events = [
        ProjectEvent::Opened {
            request: OpenRequest(7),
            result: Ok(key.clone()),
        },
        ProjectEvent::Opened {
            request: OpenRequest(8),
            result: Err(OpenError::RepoUnreadable(Arc::from("gitdir missing"))),
        },
        ProjectEvent::Changed {
            project: key.clone(),
            generation: 2,
            parts: ChangedParts {
                files: true,
                ..parts
            },
        },
        ProjectEvent::Closed {
            project: key.clone(),
        },
    ];
    // TestKernel::emitted needs `Clone`; listeners compare events.
    assert_eq!(events.clone(), events);
}

#[test]
fn project_keys_compare_by_kind_and_path() {
    let git = ProjectKey::Git {
        common_dir: path("/r/.git"),
    };
    let folder = ProjectKey::Folder {
        root: path("/r/.git"),
    };
    assert_ne!(git, folder, "the same path as a repository and a folder");
    assert_eq!(
        git,
        ProjectKey::Git {
            common_dir: path("/r/.git")
        }
    );
    assert_eq!(
        WorktreeKey(path("/r/.git/worktrees/a")),
        WorktreeKey(path("/r/.git/worktrees/a"))
    );
}

#[test]
fn manifest_kinds_by_file_name_and_ecosystem() {
    let cases = [
        ("package.json", ManifestKind::PackageJson, Ecosystem::Npm),
        (
            "pnpm-workspace.yaml",
            ManifestKind::PnpmWorkspace,
            Ecosystem::Npm,
        ),
        ("Cargo.toml", ManifestKind::CargoToml, Ecosystem::Cargo),
        ("pom.xml", ManifestKind::PomXml, Ecosystem::Maven),
        (
            "settings.gradle",
            ManifestKind::GradleSettings,
            Ecosystem::Gradle,
        ),
        (
            "settings.gradle.kts",
            ManifestKind::GradleSettings,
            Ecosystem::Gradle,
        ),
        ("build.gradle", ManifestKind::GradleBuild, Ecosystem::Gradle),
        (
            "build.gradle.kts",
            ManifestKind::GradleBuild,
            Ecosystem::Gradle,
        ),
        ("go.mod", ManifestKind::GoMod, Ecosystem::Go),
        ("go.work", ManifestKind::GoWork, Ecosystem::Go),
    ];
    for (name, kind, ecosystem) in cases {
        assert_eq!(ManifestKind::from_file_name(name), Some(kind), "{name}");
        assert_eq!(kind.ecosystem(), ecosystem, "{name}");
    }
    for name in ["Package.json", "cargo.toml", "a/pom.xml", "yarn.lock", ""] {
        assert_eq!(ManifestKind::from_file_name(name), None, "{name}");
    }
    assert_eq!(
        ManifestError::TooLarge.to_string(),
        format!("manifest is larger than {MAX_MANIFEST_BYTES} bytes")
    );
}

/// Serializes to JSON (the process plugin protocol is JSON-RPC, ADR 0018)
/// and back.
fn round_trip<T>(value: &T) -> T
where
    T: serde::Serialize + serde::de::DeserializeOwned,
{
    let json = serde_json::to_string(value).expect("serializes");
    serde_json::from_str(&json).expect("deserializes")
}

fn sample_snapshot() -> ProjectSnapshot {
    // A git project: every entry has its OID from the index.
    let mut files = FileList::builder();
    for (i, (p, kind)) in [
        ("Cargo.toml", FileKind::File),
        ("crates/a/Cargo.toml", FileKind::File),
        ("crates/a/run.sh", FileKind::Executable),
        ("link", FileKind::Symlink),
        ("vendor/sub", FileKind::Submodule),
    ]
    .into_iter()
    .enumerate()
    {
        files.push_with_oid(p, kind, BlobId::Sha1([i as u8; 20]));
    }
    files.skip();
    let files = Arc::new(files.build());
    let manifest = |path: &str, error| Manifest {
        kind: ManifestKind::CargoToml,
        path: Arc::from(path),
        name: None,
        partial: false,
        error,
    };
    let packages: Arc<[Package]> = Arc::from(vec![
        Package {
            dir: Arc::from(""),
            name: Arc::from("r"),
            manifests: Arc::from(vec![manifest("Cargo.toml", None)]),
            declared: false,
            workspace_root: true,
            parent: None,
            files: 5,
        },
        Package {
            dir: Arc::from("crates/a"),
            name: Arc::from("a"),
            manifests: Arc::from(vec![manifest(
                "crates/a/Cargo.toml",
                Some(ManifestError::Syntax { line: 3 }),
            )]),
            declared: true,
            workspace_root: false,
            parent: Some(0),
            files: 2,
        },
    ]);
    let main = WorktreeKey(path("/r/.git"));
    ProjectSnapshot {
        key: ProjectKey::Git {
            common_dir: path("/r/.git"),
        },
        generation: 3,
        name: Arc::from("r"),
        worktrees: Arc::from(vec![
            Worktree {
                key: main.clone(),
                root: path("/r"),
                is_main: true,
                head: Head::Branch(Arc::from("refs/heads/main")),
                locked: false,
                prunable: false,
            },
            Worktree {
                key: WorktreeKey(path("/r/.git/worktrees/gone")),
                root: path("/gone"),
                is_main: false,
                head: Head::Detached(Arc::from("0123456789abcdef0123456789abcdef01234567")),
                locked: true,
                prunable: true,
            },
        ]),
        active: Some(ActiveTree {
            root: path("/r"),
            worktree: Some(main),
            opened_at: Arc::from("crates/a"),
            dirs: Scan::Ready(Arc::new(DirTable::new(files.clone()))),
            files: Scan::Ready(files),
            packages: Scan::Ready(packages),
        }),
    }
}

#[test]
fn project_snapshot_round_trips() {
    let snapshot = sample_snapshot();
    let back = round_trip(&snapshot);
    assert_eq!(back, snapshot);
    let Some(ActiveTree {
        files: Scan::Ready(files),
        ..
    }) = &back.active
    else {
        panic!("active tree with files");
    };
    assert_eq!(files.skipped(), 1);
    assert_eq!(files.find("crates/a/run.sh"), Some(FileKind::Executable));
    assert_eq!(files.oid(2), Some(BlobId::Sha1([2; 20])));
    let Some(ActiveTree {
        dirs: Scan::Ready(dirs),
        ..
    }) = &back.active
    else {
        panic!("active tree with a directory table");
    };
    assert_eq!(dirs.files(), files, "rebuilt from the same list");
    assert_eq!(
        dirs.find("crates/a").map(|d| dirs.dir_files(d).len()),
        Some(2)
    );

    let folder = ProjectSnapshot {
        key: ProjectKey::Folder { root: path("/f") },
        worktrees: Arc::from(Vec::new()),
        active: Some(ActiveTree {
            root: path("/f"),
            worktree: None,
            opened_at: Arc::from(""),
            files: Scan::Unavailable(ScanError::IndexUnreadable(Arc::from("bad index"))),
            packages: Scan::Pending,
            dirs: Scan::Unavailable(ScanError::IndexUnreadable(Arc::from("bad index"))),
        }),
        ..snapshot
    };
    assert_eq!(round_trip(&folder), folder);

    let location = Location {
        project: folder.key.clone(),
        worktree: None,
        rel: Arc::from("a/b"),
        package: PackageMatch::Found(Arc::from("a")),
    };
    assert_eq!(round_trip(&location), location);
    let resolved = Resolved {
        project: folder.key,
        worktree: Some(WorktreeKey(path("/r/.git"))),
        rel: Arc::from(""),
    };
    assert_eq!(round_trip(&resolved), resolved);
}

#[test]
fn project_events_round_trip() {
    let key = ProjectKey::Git {
        common_dir: path("/r/.git"),
    };
    let events = [
        ProjectEvent::Opened {
            request: OpenRequest(1),
            result: Ok(key.clone()),
        },
        ProjectEvent::Opened {
            request: OpenRequest(2),
            result: Err(OpenError::NotFound),
        },
        ProjectEvent::Opened {
            request: OpenRequest(3),
            result: Err(OpenError::RepoUnreadable(Arc::from("gitdir missing"))),
        },
        ProjectEvent::Changed {
            project: key.clone(),
            generation: 9,
            parts: ChangedParts {
                worktrees: true,
                active: false,
                files: true,
                packages: false,
                dirs: true,
            },
        },
        ProjectEvent::Closed { project: key },
    ];
    for event in events {
        assert_eq!(round_trip(&event), event);
    }
}

#[test]
fn file_list_deserializes_through_the_builder() {
    // Unsorted, duplicated and invalid entries from another process.
    let json = r#"{"entries":[["b","File",null],["a","Executable",null],["a","File",null],["../x","File",null],["","File",null]],"skipped":1,"truncated":true}"#;
    let files: FileList = serde_json::from_str(json).expect("deserializes");
    assert_eq!(
        files.iter().collect::<Vec<_>>(),
        [("a", FileKind::Executable), ("b", FileKind::File)]
    );
    assert_eq!(files.skipped(), 3);
    assert!(files.truncated());
    assert_eq!(files.range_under(""), 0..2);
    assert_eq!(files.oid(0), None);

    // The same with OIDs: the first of equal paths keeps its OID.
    let a = "aa".repeat(20);
    let a2 = "a2".repeat(20);
    let b = "bb".repeat(20);
    let json = format!(
        r#"{{"entries":[["b","File","{b}"],["a","Executable","{a}"],["a","File","{a2}"]],"skipped":0,"truncated":false}}"#
    );
    let files: FileList = serde_json::from_str(&json).expect("deserializes");
    assert_eq!(files.oid(0), Some(BlobId::Sha1([0xaa; 20])));
    assert_eq!(files.oid(1), Some(BlobId::Sha1([0xbb; 20])));
    assert_eq!(files.oid(2), None);

    // A bad OID fails the whole list rather than being dropped.
    let json = r#"{"entries":[["a","File","xyz"]],"skipped":0,"truncated":false}"#;
    assert!(serde_json::from_str::<FileList>(json).is_err());
}

#[test]
fn file_list_serializes_oids_in_hex() {
    let mut git = FileList::builder();
    git.push_with_oid("b", FileKind::File, BlobId::Sha1([0x0f; 20]));
    git.push_with_oid("a", FileKind::Symlink, BlobId::Sha1([0xab; 20]));
    let git = git.build();
    let json = serde_json::to_string(&git).unwrap();
    assert_eq!(
        json,
        format!(
            r#"{{"entries":[["a","Symlink","{}"],["b","File","{}"]],"skipped":0,"truncated":false}}"#,
            "ab".repeat(20),
            "0f".repeat(20)
        )
    );
    assert_eq!(round_trip(&git), git);

    let mut folder = FileList::builder();
    folder.push("a", FileKind::File);
    let folder = folder.build();
    assert_eq!(
        serde_json::to_string(&folder).unwrap(),
        r#"{"entries":[["a","File",null]],"skipped":0,"truncated":false}"#
    );
    assert_eq!(round_trip(&folder), folder);
}

#[test]
fn file_list_oids_follow_entries() {
    let sha256 = |b: u8| BlobId::Sha256([b; 32]);
    let mut b = FileList::builder();
    b.push_with_oid("z", FileKind::File, sha256(3));
    b.push_with_oid("m/x", FileKind::Executable, sha256(2));
    b.push_with_oid("bad/../p", FileKind::File, sha256(9));
    b.push_with_oid("a", FileKind::File, sha256(1));
    let files = b.build();
    let entries = (0..files.len())
        .map(|i| (files.get(i).unwrap().0, files.oid(i)))
        .collect::<Vec<_>>();
    assert_eq!(
        entries,
        [
            ("a", Some(sha256(1))),
            ("m/x", Some(sha256(2))),
            ("z", Some(sha256(3)))
        ]
    );
    assert_eq!(files.skipped(), 1);
    assert_eq!(files.oid(3), None);
    assert_eq!(files.oid(usize::MAX), None);

    // A folder project's list has no OIDs.
    let mut b = FileList::builder();
    b.push("a", FileKind::File);
    let folder = b.build();
    assert_eq!(folder.oid(0), None);

    // Mixed lists (an entry without an OID, or two hash widths) keep none.
    let mut b = FileList::builder();
    b.push_with_oid("a", FileKind::File, sha256(1));
    b.push("b", FileKind::File);
    let mixed = b.build();
    assert_eq!((mixed.len(), mixed.oid(0), mixed.oid(1)), (2, None, None));
    let mut b = FileList::builder();
    b.push_with_oid("a", FileKind::File, sha256(1));
    b.push_with_oid("b", FileKind::File, BlobId::Sha1([1; 20]));
    assert_eq!(b.build().oid(0), None);

    // OIDs count in equality.
    let one = |oid| {
        let mut b = FileList::builder();
        b.push_with_oid("a", FileKind::File, oid);
        b.build()
    };
    assert_ne!(one(sha256(1)), one(sha256(2)));
    assert_ne!(one(sha256(1)), folder);
}

#[test]
fn blob_id_bytes_and_hex() {
    let sha1 = BlobId::from_bytes(&[0x01; 20]).unwrap();
    assert_eq!(sha1, BlobId::Sha1([0x01; 20]));
    assert_eq!(sha1.as_bytes(), &[0x01; 20]);
    let sha256 = BlobId::from_bytes(&[0xfe; 32]).unwrap();
    assert_eq!(sha256.as_bytes().len(), 32);
    for len in [0, 19, 21, 31, 33] {
        assert_eq!(BlobId::from_bytes(&vec![0; len]), None, "{len}");
    }

    let hex = "0123456789abcdef0123456789abcdef01234567";
    let id = BlobId::from_hex(hex).unwrap();
    assert_eq!(id.to_string(), hex);
    assert_eq!(BlobId::from_hex(&hex.to_uppercase()), Some(id));
    assert_eq!(format!("{id:?}"), format!("BlobId({hex})"));
    assert_eq!(
        BlobId::from_hex(&"e3".repeat(32)),
        Some(BlobId::Sha256([0xe3; 32]))
    );
    for bad in [
        "",
        "0123",
        &hex[..39],
        &"0".repeat(41),
        &"g".repeat(40),
        &"é".repeat(20),
    ] {
        assert_eq!(BlobId::from_hex(bad), None, "{bad}");
    }

    assert_eq!(serde_json::to_string(&id).unwrap(), format!("\"{hex}\""));
    assert_eq!(round_trip(&id), id);
    assert_eq!(round_trip(&sha256), sha256);
    assert!(serde_json::from_str::<BlobId>("\"abc\"").is_err());
    assert!(serde_json::from_str::<BlobId>("[1,2]").is_err());
}

fn dir_table(paths: &[&str]) -> DirTable {
    let mut b = FileList::builder();
    for p in paths {
        b.push(p, FileKind::File);
    }
    DirTable::new(Arc::new(b.build()))
}

#[test]
fn dir_table_serializes_dirs_and_files() {
    let table = dir_table(&["src/b.rs", "README", "src/a/x.rs"]);
    assert_eq!(
        serde_json::to_string(&table).unwrap(),
        concat!(
            r#"{"dirs":[["",null],["src",0],["src/a",1]],"#,
            r#""files":{"entries":[["README","File",null],["src/a/x.rs","File",null],["src/b.rs","File",null]],"skipped":0,"truncated":false}}"#,
        )
    );
    assert_eq!(round_trip(&table), table);
    assert_eq!(round_trip(&DirId(7)), DirId(7));

    let empty = dir_table(&[]);
    assert_eq!(round_trip(&empty), empty);
    assert_eq!(round_trip(&empty).len(), 1, "only the root");
}

#[test]
fn dir_table_deserializes_from_the_file_list() {
    // Shuffled, wrong and missing directories from another process: the
    // table is rebuilt from `files`, which goes through the builder.
    let json = concat!(
        r#"{"dirs":[["src/a",0],["zzz",5],["",null]],"#,
        r#""files":{"entries":[["src/b.rs","File",null],["src/a/x.rs","File",null],["../x","File",null],["docs/y.md","File",null]],"skipped":0,"truncated":false}}"#,
    );
    let table: DirTable = serde_json::from_str(json).expect("deserializes");
    assert_eq!(
        format!("{table:?}"),
        format!("{:?}", dir_table(&["src/a/x.rs", "src/b.rs", "docs/y.md"]))
    );
    assert_eq!(
        format!("{table:?}"),
        r#"DirTable { dirs: ["", "docs", "src", "src/a"], files: 3 }"#
    );
    assert_eq!(table.files().skipped(), 1);
    assert_eq!(table.find("zzz"), None);
    assert!(table.find("docs").is_some());

    // `files` is required; a bad file list fails the whole table.
    assert!(serde_json::from_str::<DirTable>(r#"{"dirs":[]}"#).is_err());
    let bad =
        r#"{"dirs":[],"files":{"entries":[["a","File","xyz"]],"skipped":0,"truncated":false}}"#;
    assert!(serde_json::from_str::<DirTable>(bad).is_err());
}
