//! `Driven`: the review engine without the plugin, on `Executor::manual()`
//! (docs/design/review.md「本 crate 的公开项」). Tasks run only inside
//! [`Driven::run`], on the calling thread, `Interactive` before
//! `Background`; the events are kept with the instant they were emitted.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, Weak};
use std::time::Instant;

use lectern_cache::{Budgets, Cache, Config as CacheConfig};
use lectern_executor::Executor;
use lectern_git::{HashKind, Oid};
use lectern_services::changes::{BaseSpec, ChangePath, RefreshHint};
use lectern_services::project::Projects;
use lectern_services::review::{
    FileRows, ReviewCommand, ReviewEvent, ReviewId, ReviewSnapshot, ReviewTarget, RowsKey, TextKey,
};
use lectern_services::session::{SessionKey, SessionSource, SourceId, Timestamp};

use super::fake::{FakeProjects, FakeSessions};
use crate::engine::round::Kind;
use crate::engine::{Config, Engine, Held, attribute};

/// The engine with its fakes, a cache and a log directory under `dir`.
pub struct Driven {
    pub(crate) engine: Arc<Engine>,
    executor: Executor,
    events: Arc<Mutex<Vec<(ReviewEvent, Instant)>>>,
    snapshots: Arc<Mutex<Vec<Arc<ReviewSnapshot>>>>,
    /// Whether `snapshots` is filled (off by default: a bench making
    /// thousands of snapshots would keep them all).
    capture: Arc<AtomicBool>,
    now: Arc<AtomicI64>,
    projects: Arc<FakeProjects>,
    cache: Cache,
}

impl Driven {
    /// Places the sessions among `projects` and builds the engine: the
    /// cache under `dir/cache`, the annotation logs under `dir/review`. The
    /// clock is `now` until [`Driven::set_now`].
    pub fn new(
        projects: Arc<FakeProjects>,
        sessions: Vec<Arc<FakeSessions>>,
        dir: &Path,
        now: Timestamp,
    ) -> Driven {
        Driven::with_budget(
            projects,
            sessions,
            dir,
            now,
            crate::cutoff::INTERACTIVE_BUDGET,
        )
    }

    /// [`Driven::new`] with another interactive budget for the cutoff
    /// misses (`Duration::ZERO`: every miss goes to the background).
    pub fn with_budget(
        projects: Arc<FakeProjects>,
        sessions: Vec<Arc<FakeSessions>>,
        dir: &Path,
        now: Timestamp,
        interactive_budget: std::time::Duration,
    ) -> Driven {
        for s in &sessions {
            s.locate(&*projects);
        }
        let executor = Executor::manual();
        let cache = Cache::new(CacheConfig {
            root: dir.join("cache"),
            budgets: Budgets::default(),
            ui_thread: None,
        });
        let events: Arc<Mutex<Vec<(ReviewEvent, Instant)>>> = Arc::default();
        let sink = Arc::clone(&events);
        let snapshots: Arc<Mutex<Vec<Arc<ReviewSnapshot>>>> = Arc::default();
        let kept = Arc::clone(&snapshots);
        let capture = Arc::new(AtomicBool::new(false));
        let capturing = Arc::clone(&capture);
        // The engine, for the snapshot each `Changed` published (the event
        // is emitted with no lock held).
        let me: Arc<OnceLock<Weak<Engine>>> = Arc::default();
        let get = Arc::clone(&me);
        let clock = Arc::new(AtomicI64::new(now.0));
        let read = Arc::clone(&clock);
        let sources: Vec<Arc<dyn SessionSource>> = sessions
            .into_iter()
            .map(|s| s as Arc<dyn SessionSource>)
            .collect();
        let held_projects = Arc::clone(&projects);
        let engine = Engine::new(Config {
            executor: executor.clone(),
            cache: cache.clone(),
            data_dir: dir.join("review"),
            git_ceilings: Arc::from(Vec::<PathBuf>::new()),
            projects: Arc::new(move || {
                Some(Held::new(Arc::clone(&held_projects) as Arc<dyn Projects>))
            }),
            sources: Arc::new(move || sources.iter().map(|s| Held::new(Arc::clone(s))).collect()),
            host: None,
            watcher: None,
            emit: Arc::new(move |e| {
                let at = Instant::now();
                if let ReviewEvent::Changed { id, .. } = &e
                    && capturing.load(Ordering::Relaxed)
                    && let Some(snapshot) = get
                        .get()
                        .and_then(Weak::upgrade)
                        .and_then(|engine| engine.snapshot(*id))
                {
                    kept.lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(snapshot);
                }
                sink.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push((e, at))
            }),
            clock: Arc::new(move || Timestamp(read.load(Ordering::Relaxed))),
            discover: Arc::new(lectern_git::testing::discover),
            interactive_budget,
        });
        let _ = me.set(Arc::downgrade(&engine));
        Driven {
            engine,
            executor,
            events,
            snapshots,
            capture,
            now: clock,
            projects,
            cache,
        }
    }

    /// From now on, keep the snapshot each `Changed` published, for
    /// [`Driven::take_snapshots`].
    pub fn capture_snapshots(&self) {
        self.capture.store(true, Ordering::Relaxed);
    }

    /// The snapshots published with each `Changed` since
    /// [`Driven::capture_snapshots`], in order; the list is emptied.
    pub fn take_snapshots(&self) -> Vec<Arc<ReviewSnapshot>> {
        std::mem::take(
            &mut *self
                .snapshots
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        )
    }

    pub fn set_now(&self, now: Timestamp) {
        self.now.store(now.0, Ordering::Relaxed);
    }

    /// Runs the queued tasks until none is left; returns how many steps.
    pub fn run(&self) -> usize {
        self.executor.run_until_idle()
    }

    pub fn open(&self, target: ReviewTarget) -> ReviewId {
        self.engine.open(target)
    }

    pub fn snapshot(&self, id: ReviewId) -> Option<Arc<ReviewSnapshot>> {
        self.engine.snapshot(id)
    }

    pub fn command(&self, id: ReviewId, command: ReviewCommand) {
        self.engine.command(id, command)
    }

    pub fn rows(&self, id: ReviewId, key: &RowsKey) -> Option<Arc<FileRows>> {
        self.engine.rows(id, key)
    }

    pub fn text(&self, id: ReviewId, key: &TextKey) -> Option<Arc<str>> {
        self.engine.text(id, key)
    }

    pub fn close(&self, id: ReviewId) {
        self.engine.close(id)
    }

    /// What a worktree batch with these changed paths asks for
    /// (`RefreshHint::Paths`); `None` is a full refresh. For
    /// `review/refresh_one`.
    pub fn refresh(&self, id: ReviewId, paths: Option<&[&str]>) {
        let hint = match paths {
            Some(paths) => RefreshHint::Paths(
                paths
                    .iter()
                    .map(|p| ChangePath(Arc::from(p.as_bytes())))
                    .collect(),
            ),
            None => RefreshHint::Full,
        };
        self.engine.request(id, Kind::Refresh(hint));
    }

    /// A `SessionsChanged` from the sources, as the plugin hears it.
    pub fn sessions_changed(&self, event: &lectern_services::session::SessionsChanged) {
        self.engine.sessions_changed(event);
    }

    /// The events so far, with when each was emitted; the list is emptied.
    pub fn take_events(&self) -> Vec<(ReviewEvent, Instant)> {
        std::mem::take(&mut *self.events.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// The session `id` of the source `source`.
    pub fn session(&self, source: &str, id: &str) -> ReviewTarget {
        ReviewTarget::Session(SessionKey {
            source: SourceId(source.into()),
            id: id.into(),
        })
    }

    /// The main worktree's changes outside any session.
    pub fn outside(&self) -> ReviewTarget {
        ReviewTarget::Outside {
            project: self.projects.key().clone(),
            worktree: self.projects.main(),
        }
    }

    /// The main worktree against `base`.
    pub fn range(&self, base: BaseSpec) -> ReviewTarget {
        ReviewTarget::Range {
            project: self.projects.key().clone(),
            worktree: self.projects.main(),
            base,
        }
    }

    /// The bytes of the replayed versions kept.
    pub fn version_bytes(&self) -> usize {
        self.engine.version_bytes()
    }

    /// Builds the chain of every file from `from` on again, without the
    /// cache (each step's edits loaded), and attributes it: the background
    /// attribution of a review whose `review.attribution` is empty, minus
    /// the cache's reads and writes. Returns how many files have line
    /// provenance. For `review/attribute_session_50`.
    pub fn attribute_uncached(&self, id: ReviewId, from: usize) -> usize {
        let Some(work) = self.engine.work(id) else {
            return 0;
        };
        let mut lines = 0;
        for file in work.files.iter().skip(from) {
            if file.history.is_empty() {
                continue;
            }
            let chain = attribute::build(&self.engine, &work, file);
            if matches!(chain.detail, crate::chain::Detail::Lines(_)) {
                lines += 1;
            }
        }
        lines
    }

    /// Closes the cache's connections (for a directory reused later).
    pub fn close_cache(&self) {
        self.cache.close();
    }

    /// Opens the cache's app partition and the repository partition of
    /// `common_dir` with a read that misses, as a running app has them open
    /// before any review: an `open` measured after it does not pay for
    /// creating the database files.
    pub fn touch_cache(&self, common_dir: &Path) {
        let key = lectern_cache::Key::new().str("touch");
        let _ = self.cache.app().get::<crate::claims::CachedClaims>(&key);
        let _ = self
            .cache
            .repo(common_dir)
            .get::<crate::cutoff::CachedTreeChanges>(&key);
    }
}

/// One line of a HEAD reflog for [`write_reflog`].
#[derive(Clone, Copy, Debug)]
pub struct ReflogLine<'a> {
    /// None: all zeros (the reference did not exist).
    pub old: Option<Oid>,
    pub new: Oid,
    /// Unix seconds.
    pub time: i64,
    pub message: &'a str,
}

/// Writes `<git_dir>/logs/HEAD` with these lines, oldest first, as git
/// appends them.
pub fn write_reflog(git_dir: &Path, hash: HashKind, lines: &[ReflogLine<'_>]) {
    let zeros = "0".repeat(hash.hex_len());
    let mut text = String::new();
    for l in lines {
        let old = l.old.map_or_else(|| zeros.clone(), |o| o.to_hex());
        text.push_str(&format!(
            "{old} {} Test <test@example.com> {} +0000\t{}\n",
            l.new.to_hex(),
            l.time,
            l.message
        ));
    }
    let path = git_dir.join("logs").join("HEAD");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}
