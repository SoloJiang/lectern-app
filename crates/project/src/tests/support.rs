//! A host that queues work for the test to run, and a readable rendering of
//! snapshots for insta.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lectern_executor::Executor;
use lectern_fs_watch::testing::{Driver, FakeBackend, manual};
use lectern_fs_watch::{Config, Watcher};
use lectern_git::{HashKind, Oid};
use lectern_kernel::{BusyGuard, WorkspaceFacts};
use lectern_services::project::{
    BlobId, DirTable, FileList, OpenError, OpenRequest, PackageMatch, ProjectEvent, ProjectKey,
    ProjectSnapshot, Projects, Scan,
};

use crate::git_access::{GitAccess, blob_id};
use crate::model::{Host, Model};
use crate::testing::FakeGit;

type Job = Box<dyn FnOnce() + Send>;

/// Work runs only when the test calls `run` or `step`, on the test thread.
#[derive(Default)]
pub(crate) struct QueueHost {
    jobs: Mutex<VecDeque<Job>>,
    events: Mutex<Vec<ProjectEvent>>,
    facts: Mutex<Vec<Arc<dyn WorkspaceFacts>>>,
}

impl QueueHost {
    /// Runs queued work, and the work it queues, until none is left.
    pub(crate) fn run(&self) {
        while self.step() {}
    }

    /// Runs the oldest queued job. False when there was none.
    pub(crate) fn step(&self) -> bool {
        let job = self.jobs.lock().unwrap().pop_front();
        match job {
            Some(job) => {
                job();
                true
            }
            None => false,
        }
    }

    /// Takes the oldest queued job out without running it, so the test
    /// decides when (and whether) it runs.
    pub(crate) fn hold(&self) -> Option<Job> {
        self.jobs.lock().unwrap().pop_front()
    }

    pub(crate) fn queued(&self) -> usize {
        self.jobs.lock().unwrap().len()
    }

    pub(crate) fn take_events(&self) -> Vec<ProjectEvent> {
        std::mem::take(&mut *self.events.lock().unwrap())
    }

    /// The facts submitted last.
    pub(crate) fn facts(&self) -> Option<Arc<dyn WorkspaceFacts>> {
        self.facts.lock().unwrap().last().cloned()
    }

    pub(crate) fn facts_count(&self) -> usize {
        self.facts.lock().unwrap().len()
    }
}

impl Host for QueueHost {
    fn spawn(&self, work: Job) {
        self.jobs.lock().unwrap().push_back(work);
    }

    fn emit(&self, event: ProjectEvent) {
        self.events.lock().unwrap().push(event);
    }

    fn workspace_changed(&self, facts: Arc<dyn WorkspaceFacts>) {
        self.facts.lock().unwrap().push(facts);
    }

    fn busy(&self) -> Option<BusyGuard> {
        None
    }
}

/// Long enough for every fs_watch coalescing window to end.
pub(crate) const LONG: Duration = Duration::from_secs(5);

/// A manually driven fs_watch over a fake backend, for the refresh tests.
pub(crate) struct Watch {
    pub backend: Arc<FakeBackend>,
    pub watcher: Watcher,
    pub driver: Arc<Driver>,
}

impl Watch {
    pub(crate) fn new() -> Watch {
        let backend = FakeBackend::new();
        let (watcher, driver) = manual(backend.clone(), Executor::manual(), Config::default());
        Watch {
            backend,
            watcher,
            driver: Arc::new(driver),
        }
    }

    /// Runs fs_watch (its queue and loads, then the clock past every
    /// window) and the model's queued work in turn until neither has any
    /// left.
    pub(crate) fn settle(&self, host: &QueueHost) {
        loop {
            let before = self.driver.run_until_idle();
            self.driver.advance(LONG);
            let mut ran = false;
            while host.step() {
                ran = true;
            }
            let after = self.driver.run_until_idle();
            if before == 0 && !ran && after == 0 {
                return;
            }
        }
    }
}

/// A model over a `FakeGit` and a `QueueHost`.
pub(crate) struct Harness {
    pub git: Arc<FakeGit>,
    pub host: Arc<QueueHost>,
    pub model: Arc<Model>,
}

impl Harness {
    pub(crate) fn new() -> Harness {
        let git = Arc::new(FakeGit::new());
        let home = git.base().join("home");
        Harness::with_model(git.clone(), |g| Model::new(g, home))
    }

    pub(crate) fn with_model(
        git: Arc<FakeGit>,
        model: impl FnOnce(Arc<dyn GitAccess>) -> Arc<Model>,
    ) -> Harness {
        let host = Arc::new(QueueHost::default());
        let model = model(git.clone());
        model.attach(host.clone());
        Harness { git, host, model }
    }

    pub(crate) fn base(&self) -> &Path {
        self.git.base()
    }

    /// Opens `path`, runs everything and returns the `Opened` result.
    pub(crate) fn open(&self, path: &Path) -> Result<ProjectKey, OpenError> {
        let request = self.model.open(path);
        self.host.run();
        opened(&self.host.take_events(), request)
    }

    pub(crate) fn snapshot(&self, key: &ProjectKey) -> Arc<ProjectSnapshot> {
        self.model.snapshot(key).expect("the project is open")
    }

    pub(crate) fn render(&self, key: &ProjectKey) -> String {
        render(&self.snapshot(key), self.base())
    }
}

/// The result of the `Opened` event for `request`.
pub(crate) fn opened(
    events: &[ProjectEvent],
    request: OpenRequest,
) -> Result<ProjectKey, OpenError> {
    events
        .iter()
        .find_map(|e| match e {
            ProjectEvent::Opened { request: r, result } if *r == request => Some(result.clone()),
            _ => None,
        })
        .expect("an Opened event for the request")
}

/// Events as one line each, paths under `base` shortened.
pub(crate) fn render_events(events: &[ProjectEvent], base: &Path) -> String {
    let mut out = String::new();
    for event in events {
        let line = match event {
            ProjectEvent::Opened { result, .. } => format!("Opened {result:?}"),
            ProjectEvent::Changed {
                generation, parts, ..
            } => {
                let mut names = Vec::new();
                for (on, name) in [
                    (parts.worktrees, "worktrees"),
                    (parts.active, "active"),
                    (parts.files, "files"),
                    (parts.packages, "packages"),
                    (parts.dirs, "dirs"),
                ] {
                    if on {
                        names.push(name);
                    }
                }
                format!("Changed {generation} [{}]", names.join(", "))
            }
            ProjectEvent::Closed { project } => format!("Closed {project:?}"),
        };
        out.push_str(&redact(&line, base));
        out.push('\n');
    }
    out
}

/// A snapshot as readable text, paths under `base` shortened.
pub(crate) fn render(snapshot: &ProjectSnapshot, base: &Path) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "key: {:?}", snapshot.key);
    let _ = writeln!(out, "name: {}", snapshot.name);
    let _ = writeln!(out, "worktrees:");
    for w in snapshot.worktrees.iter() {
        let mut flags = Vec::new();
        if w.is_main {
            flags.push("main");
        }
        if w.locked {
            flags.push("locked");
        }
        if w.prunable {
            flags.push("prunable");
        }
        let _ = writeln!(
            out,
            "  {} key={} head={:?} [{}]",
            w.root.display(),
            w.key.0.display(),
            w.head,
            flags.join(" ")
        );
    }
    match &snapshot.active {
        None => {
            let _ = writeln!(out, "active: none");
        }
        Some(a) => {
            let _ = writeln!(
                out,
                "active: {} worktree={:?} opened_at={:?}",
                a.root.display(),
                a.worktree.as_ref().map(|w| w.0.display().to_string()),
                a.opened_at
            );
            match &a.files {
                Scan::Ready(files) => {
                    let _ = writeln!(
                        out,
                        "files: {} (skipped {}, truncated {})",
                        files.len(),
                        files.skipped(),
                        files.truncated()
                    );
                    for (path, kind) in files.iter() {
                        let _ = writeln!(out, "  {path} {kind:?}");
                    }
                }
                other => {
                    let _ = writeln!(out, "files: {other:?}");
                }
            }
            match &a.packages {
                Scan::Ready(packages) => {
                    let _ = writeln!(out, "packages:");
                    for p in packages.iter() {
                        let manifests: Vec<String> = p
                            .manifests
                            .iter()
                            .map(|m| match &m.error {
                                Some(e) => format!("{} ({e:?})", m.path),
                                None => m.path.to_string(),
                            })
                            .collect();
                        let _ = writeln!(
                            out,
                            "  {:?} name={} declared={} workspace_root={} parent={:?} files={} manifests=[{}]",
                            p.dir,
                            p.name,
                            p.declared,
                            p.workspace_root,
                            p.parent,
                            p.files,
                            manifests.join(", ")
                        );
                    }
                }
                other => {
                    let _ = writeln!(out, "packages: {other:?}");
                }
            }
            check_dirs(snapshot);
            match &a.dirs {
                Scan::Ready(table) => {
                    let _ = writeln!(out, "dirs: {}", table.len());
                }
                other => {
                    let _ = writeln!(out, "dirs: {other:?}");
                }
            }
        }
    }
    redact(&out, base)
}

/// The directory table agrees with the file list: ready only when `files`
/// is, built from that very list; unavailable with the same error.
pub(crate) fn check_dirs(snapshot: &ProjectSnapshot) {
    let Some(a) = &snapshot.active else {
        return;
    };
    match (&a.files, &a.dirs) {
        (Scan::Ready(files), Scan::Ready(table)) => assert!(
            Arc::ptr_eq(files, table.files()),
            "generation {}: the table is from another file list",
            snapshot.generation
        ),
        (_, Scan::Ready(_)) => panic!("generation {}: a table without files", snapshot.generation),
        (Scan::Unavailable(f), Scan::Unavailable(d)) => assert_eq!(f, d),
        (_, Scan::Unavailable(_)) => panic!("dirs unavailable, files {:?}", a.files),
        (Scan::Unavailable(_), Scan::Pending) => panic!("dirs pending, files unavailable"),
        (_, Scan::Pending) => {}
    }
}

/// The active tree's directory table; panics unless it is ready.
pub(crate) fn active_dirs(snapshot: &ProjectSnapshot) -> Arc<DirTable> {
    match &snapshot.active.as_ref().expect("no active tree").dirs {
        Scan::Ready(table) => table.clone(),
        other => panic!("dirs {other:?}"),
    }
}

pub(crate) fn redact(text: &str, base: &Path) -> String {
    text.replace(&*base.to_string_lossy(), "[base]")
}

/// The active tree's file list; panics unless it is ready.
pub(crate) fn active_files(snapshot: &ProjectSnapshot) -> Arc<FileList> {
    match &snapshot.active.as_ref().expect("no active tree").files {
        Scan::Ready(files) => files.clone(),
        other => panic!("files {other:?}"),
    }
}

/// The OID of `path` in `files`; panics when the path is not listed.
pub(crate) fn oid_of(files: &FileList, path: &str) -> Option<BlobId> {
    let i = files
        .iter()
        .position(|(p, _)| p == path)
        .unwrap_or_else(|| panic!("{path} not listed"));
    files.oid(i)
}

/// The `FileList` OID git records for `content` staged in a `hash`
/// repository.
pub(crate) fn blob(hash: HashKind, content: &[u8]) -> Option<BlobId> {
    Some(blob_id(&Oid::for_blob(hash, content)))
}

pub(crate) fn found(dir: &str) -> PackageMatch {
    PackageMatch::Found(dir.into())
}

/// A yarn workspace with two packages and a Maven module, the shape most
/// tests use: `package.json` (workspaces `apps/*`), `apps/web`,
/// `apps/console` (also a Maven module of the root pom), `README.md`.
pub(crate) fn monorepo(git: &FakeGit, git_dir: &Path) {
    git.write(
        git_dir,
        "package.json",
        br#"{ "name": "root", "workspaces": ["apps/*"] }"#,
    );
    git.write(
        git_dir,
        "pom.xml",
        b"<project><artifactId>parent</artifactId><modules><module>apps/console</module></modules></project>",
    );
    git.write(git_dir, "README.md", b"# root\n");
    git.write(git_dir, "apps/web/package.json", br#"{ "name": "web" }"#);
    git.write(git_dir, "apps/web/src/index.ts", b"export {};\n");
    git.write(
        git_dir,
        "apps/console/package.json",
        br#"{ "name": "console" }"#,
    );
    git.write(git_dir, "apps/console/src/main.ts", b"export {};\n");
}

/// No `Changed` before the `Opened` of its project (when the list has one),
/// and generations strictly increasing per project.
pub(crate) fn check_event_order(events: &[ProjectEvent]) {
    use std::collections::HashMap;
    let mut last: HashMap<ProjectKey, u64> = HashMap::new();
    for event in events {
        match event {
            ProjectEvent::Opened {
                result: Ok(key), ..
            } => {
                // A reopen may follow `Changed` events of the same project.
                if !events
                    .iter()
                    .take_while(|e| *e != event)
                    .any(|e| matches!(e, ProjectEvent::Opened { result: Ok(k), .. } if k == key))
                {
                    assert!(!last.contains_key(key), "Changed before Opened: {key:?}");
                }
            }
            ProjectEvent::Changed {
                project,
                generation,
                ..
            } => {
                let previous = last.insert(project.clone(), *generation).unwrap_or(1);
                assert!(
                    *generation > previous,
                    "generation went from {previous} to {generation}"
                );
            }
            ProjectEvent::Opened { .. } | ProjectEvent::Closed { .. } => {}
        }
    }
}
