//! The documents' test rig: real files in a temporary worktree, a manual
//! executor, a kernel host, a temporary cache, the fs_watch manual watcher
//! with `FakeBackend`, and a git stand-in that keeps blobs in memory and
//! counts its calls.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lectern_cache::{Budgets, Cache, Config as CacheConfig};
use lectern_executor::Executor;
use lectern_fs_watch::testing::{Driver, FakeBackend, FakeSource, manual};
use lectern_fs_watch::{Config as WatchConfig, Flags, WorktreeSource};
use lectern_git::{GitError, HashKind, Oid};
use lectern_services::code_intel::TextOrigin;
use lectern_services::project::{BlobId, ProjectKey, WorktreeKey};
use lectern_services::reader::{Anchor, ReadTarget};

use crate::git_access::{GitAccess, Tree, blob_id};
use crate::testing::{TestHost, host};
use crate::{DocHandle, DocSnapshot, Engine, EngineDeps, Languages, Viewport};

/// The git stand-in.
#[derive(Default)]
pub(crate) struct FakeGit {
    pub blobs: Mutex<HashMap<BlobId, Arc<[u8]>>>,
    pub blob_calls: AtomicUsize,
    pub source: Mutex<Option<Arc<FakeSource>>>,
}

impl FakeGit {
    /// Stores `bytes` as a blob; its SHA-1 OID.
    pub(crate) fn add_blob(&self, bytes: &[u8]) -> BlobId {
        let oid = blob_id(&Oid::for_blob(HashKind::Sha1, bytes));
        self.blobs.lock().unwrap().insert(oid, Arc::from(bytes));
        oid
    }
}

impl GitAccess for FakeGit {
    fn hash(&self, _common_dir: &Path) -> Result<HashKind, GitError> {
        Ok(HashKind::Sha1)
    }

    fn blob(&self, _common_dir: &Path, oid: BlobId, max: u64) -> Result<Arc<[u8]>, GitError> {
        self.blob_calls.fetch_add(1, Ordering::SeqCst);
        let blobs = self.blobs.lock().unwrap();
        let bytes = blobs
            .get(&oid)
            .ok_or_else(|| GitError::ObjectMissing(crate::git_access::git_oid(oid)))?;
        if bytes.len() as u64 > max {
            return Err(GitError::TooLarge {
                size: bytes.len() as u64,
            });
        }
        Ok(bytes.clone())
    }

    fn watch_source(&self, _tree: &Tree) -> Result<Arc<dyn WorktreeSource>, GitError> {
        let source = self.source.lock().unwrap().clone();
        Ok(source.unwrap_or_default())
    }

    fn forget(&self, _common_dir: &Path) {}
}

pub(crate) struct Rig {
    _dir: tempfile::TempDir,
    _cache_dir: tempfile::TempDir,
    _host: TestHost,
    pub engine: Engine,
    pub git: Arc<FakeGit>,
    pub backend: Arc<FakeBackend>,
    pub driver: Driver,
    pub source: Arc<FakeSource>,
    pub root: Arc<Path>,
    pub common: Arc<Path>,
    pub notified: Arc<AtomicUsize>,
}

pub(crate) const VIEW: Viewport = Viewport {
    rows: 40,
    cols: 100,
};

impl Rig {
    pub(crate) fn new() -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = tempfile::tempdir().unwrap();
        let root: Arc<Path> = Arc::from(dir.path().canonicalize().unwrap());
        let common: Arc<Path> = Arc::from(root.join(".git"));
        std::fs::create_dir_all(&common).unwrap();
        let executor = Executor::manual();
        let backend = FakeBackend::new();
        let (watcher, driver) = manual(backend.clone(), executor.clone(), WatchConfig::default());
        let cache = Cache::new(CacheConfig {
            root: cache_dir.path().to_path_buf(),
            budgets: Budgets::default(),
            ui_thread: None,
        });
        let git = Arc::new(FakeGit::default());
        let source = FakeSource::new();
        *git.source.lock().unwrap() = Some(source.clone());
        let engine = Engine::with_git(
            EngineDeps {
                executor,
                cache,
                watcher,
                languages: Languages::core(),
            },
            git.clone(),
        );
        let host = host();
        engine.start(host.handle.clone());
        Rig {
            _dir: dir,
            _cache_dir: cache_dir,
            _host: host,
            engine,
            git,
            backend,
            driver,
            source,
            root,
            common,
            notified: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub(crate) fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    pub(crate) fn write(&self, rel: &str, bytes: &[u8]) {
        let path = self.path(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, bytes).unwrap();
        self.source.track(&[rel]);
    }

    pub(crate) fn project(&self) -> ProjectKey {
        ProjectKey::Git {
            common_dir: self.common.clone(),
        }
    }

    pub(crate) fn target(&self, rel: &str, at: Anchor) -> ReadTarget {
        ReadTarget {
            project: self.project(),
            worktree: Some(WorktreeKey(self.common.clone())),
            path: rel.into(),
            origin: TextOrigin::Worktree,
            expect: None,
            at,
        }
    }

    pub(crate) fn object(&self, rel: &str, oid: BlobId, at: Anchor) -> ReadTarget {
        ReadTarget {
            origin: TextOrigin::Object,
            expect: Some(oid),
            ..self.target(rel, at)
        }
    }

    pub(crate) fn open(&self, target: ReadTarget) -> DocHandle {
        let notified = self.notified.clone();
        self.engine.open(
            target,
            self.root.clone(),
            VIEW,
            Arc::new(move || {
                notified.fetch_add(1, Ordering::SeqCst);
            }),
        )
    }

    /// Runs the executor and the watcher until neither has work.
    pub(crate) fn settle(&self) {
        self.driver.run_until_idle();
    }

    /// Opens and settles: the snapshot after every task.
    pub(crate) fn open_settled(&self, target: ReadTarget) -> (DocHandle, Arc<DocSnapshot>) {
        let doc = self.open(target);
        self.settle();
        let snapshot = doc.snapshot();
        (doc, snapshot)
    }

    /// A worktree event on `rel`, delivered.
    pub(crate) fn touch(&self, rel: &str, flags: Flags) {
        self.backend.emit(&self.root, &[(rel, flags)]);
        // Taken in now, delivered once the window is quiet.
        self.settle();
        self.driver.advance(Duration::from_secs(1));
        self.settle();
    }
}

/// `FILE | MODIFIED`.
pub(crate) fn modified() -> Flags {
    Flags(Flags::FILE.0 | Flags::MODIFIED.0)
}

/// `FILE | REMOVED`.
pub(crate) fn removed() -> Flags {
    Flags(Flags::FILE.0 | Flags::REMOVED.0)
}

/// `FILE | CREATED`.
pub(crate) fn created() -> Flags {
    Flags(Flags::FILE.0 | Flags::CREATED.0)
}

/// The open descriptors of this process: `/proc/self/fd` on Linux, the
/// fdescfs `/dev/fd` on macOS (the same choice as platform's `fd_*` tests).
/// The listing's own descriptor is in every count alike. A missing
/// directory panics: a silent 0 would make every comparison wrong. Counts
/// are exact under `just test` (nextest runs each test in its own process).
pub(crate) fn open_fds() -> usize {
    let dir = if cfg!(target_os = "linux") {
        "/proc/self/fd"
    } else {
        "/dev/fd"
    };
    std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{dir}: {e}"))
        .count()
}

/// The text of the rows, `|`-joined.
pub(crate) fn texts(
    snapshot: &DocSnapshot,
    from: crate::RowPos,
    n: usize,
    cols: u32,
) -> Vec<String> {
    snapshot
        .rows(from, n, cols)
        .into_iter()
        .map(|row| match row {
            crate::Row::Text { text, .. } => text.into_owned(),
            crate::Row::Pending { .. } => "<pending>".to_owned(),
        })
        .collect()
}
