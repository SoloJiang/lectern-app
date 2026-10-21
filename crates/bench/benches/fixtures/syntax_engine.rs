//! An `lectern_syntax::Engine` for the `syntax/*` benchmarks: a real
//! executor, a cache in the benchmark's scratch directory, a watcher on a
//! fake backend (worktree targets of folder projects are not watched), and
//! a kernel host whose handle the engine's tasks go through.
#![allow(dead_code, reason = "each target uses its own subset of the rig")]

use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use lectern_bench::BenchError;
use lectern_cache::{Budgets, Cache, Config as CacheConfig};
use lectern_executor::{Config as ExecutorConfig, Executor};
use lectern_fs_watch::testing::FakeBackend;
use lectern_fs_watch::{Config as WatchConfig, Watcher};
use lectern_services::code_intel::TextOrigin;
use lectern_services::project::{BlobId, ProjectKey};
use lectern_services::reader::{Anchor, ReadTarget};
use lectern_syntax::testing::{TestHost, host};
use lectern_syntax::{DocHandle, DocSnapshot, Engine, EngineDeps, Languages, Viewport};

/// The viewport of the first screen: about 60 lines.
pub const VIEW: Viewport = Viewport {
    rows: 60,
    cols: 120,
};

pub struct Rig {
    pub engine: Engine,
    pub root: Arc<Path>,
    _host: TestHost,
    _cache: Cache,
}

impl Rig {
    /// The engine over `root` (a folder project: no watching).
    pub fn new(root: &Path) -> Result<Rig, BenchError> {
        let root: Arc<Path> = Arc::from(
            root.canonicalize()
                .map_err(|e| BenchError::new(e.to_string()))?,
        );
        let executor = Executor::new(ExecutorConfig::default());
        let cache = Cache::new(CacheConfig {
            root: root.join(".cache"),
            budgets: Budgets::default(),
            ui_thread: None,
        });
        let watcher = Watcher::new(FakeBackend::new(), executor.clone(), WatchConfig::default());
        let engine = Engine::new(EngineDeps {
            executor,
            cache: cache.clone(),
            watcher,
            languages: Languages::core(),
        });
        let host = host();
        engine.start(host.handle.clone());
        Ok(Rig {
            engine,
            root,
            _host: host,
            _cache: cache,
        })
    }

    pub fn target(&self, path: &str, at: Anchor) -> ReadTarget {
        ReadTarget {
            project: ProjectKey::Folder {
                root: self.root.clone(),
            },
            worktree: None,
            path: path.into(),
            origin: TextOrigin::Worktree,
            expect: None,
            at,
        }
    }

    /// An object target of a repository whose common directory is
    /// `common_dir`.
    pub fn object(&self, common_dir: &Path, oid: BlobId, at: Anchor) -> ReadTarget {
        ReadTarget {
            project: ProjectKey::Git {
                common_dir: Arc::from(common_dir),
            },
            worktree: None,
            // TypeScript, highlighted like the review's old side.
            path: "old.ts".into(),
            origin: TextOrigin::Object,
            expect: Some(oid),
            at,
        }
    }

    /// Opens `target`; the receiver gets a message after every snapshot.
    pub fn open(&self, target: ReadTarget) -> (DocHandle, Receiver<()>) {
        let (tx, rx) = channel();
        let doc = self.engine.open(
            target,
            self.root.clone(),
            VIEW,
            Arc::new(move || {
                let _ = tx.send(());
            }),
        );
        (doc, rx)
    }

    /// Waits until the engine's tasks have all ended.
    pub fn idle(&self) -> Result<(), BenchError> {
        let deadline = Instant::now() + Duration::from_secs(120);
        while self.engine.tasks_in_flight() > 0 {
            if Instant::now() > deadline {
                return Err(BenchError::new("the engine's tasks did not end"));
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        Ok(())
    }
}

/// Waits for a snapshot of `doc` that satisfies `done`.
pub fn wait(
    doc: &DocHandle,
    notified: &Receiver<()>,
    done: impl Fn(&DocSnapshot) -> bool,
) -> Result<Arc<DocSnapshot>, BenchError> {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let snapshot = doc.snapshot();
        if done(&snapshot) {
            return Ok(snapshot);
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(BenchError::new("the document did not get there"));
        }
        let _ = notified.recv_timeout(left.min(Duration::from_millis(50)));
    }
}
