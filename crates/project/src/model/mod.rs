//! The project model behind `dyn Projects` (docs/design/project.md「接口」
//! 「数据结构」「数据流与线程」).
//!
//! Every open project has one snapshot, replaced whole, and at most one
//! scan loop on the executor. Requests that arrive while it runs are kept
//! as `wanted` (the newest wins) and picked up when the current step ends;
//! fs_watch batches only mark the project dirty, and the loop refreshes it
//! once the requests are done (`watch.rs`).

use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, Weak};

use lectern_fs_watch::Watcher;
use lectern_git::{GitError, HashKind};
use lectern_kernel::{BusyGuard, WorkspaceFacts};
use lectern_services::project::{
    ActiveTree, Location, OpenError, OpenRequest, Package, PackageMatch, ProjectEvent, ProjectKey,
    ProjectSnapshot, Projects, RelPath, ResolveError, Resolved, Scan, ScanError, Worktree,
    WorktreeKey,
};

use crate::folder::FOLDER_LIMIT;
use crate::git_access::{Discovered, GitAccess, WorktreeInfo};

mod cache;
mod scan;
mod watch;

use cache::{OtherTrees, PARSE_CACHE_BYTES, ParseCache};
use watch::{Dirty, Watches};

/// Where the model runs its work and sends its results: the kernel and the
/// executor in the app (`plugin::KernelHost`), a queue in tests.
pub(crate) trait Host: Send + Sync {
    /// Runs `work` on the executor. The kernel stays busy from this call
    /// until `work` returns.
    fn spawn(&self, work: Box<dyn FnOnce() + Send>);
    fn emit(&self, event: ProjectEvent);
    fn workspace_changed(&self, facts: Arc<dyn WorkspaceFacts>);
    /// A guard for the fs_watch sinks (`Sink::busy`); None when the plugin
    /// is deactivated, and in tests.
    fn busy(&self) -> Option<BusyGuard>;
}

pub(crate) struct Model {
    me: Weak<Model>,
    git: Arc<dyn GitAccess>,
    home: PathBuf,
    host: OnceLock<Arc<dyn Host>>,
    /// None (tests that don't refresh): every scan opens its own worktree
    /// handle and nothing is watched, as before step 4.
    watcher: OnceLock<Watcher>,
    /// Ids of the active worktrees' watches; 0 is none.
    next_watch: AtomicU64,
    folder_limit: usize,
    next_request: AtomicU64,
    /// In the order they were opened.
    projects: Mutex<Vec<Arc<Project>>>,
    parsed: Mutex<ParseCache>,
    others: Mutex<OtherTrees>,
    /// Facts are built and submitted under this lock, so the last one
    /// submitted saw the newest file lists.
    facts: Mutex<()>,
    #[cfg(test)]
    pub(crate) parses: std::sync::atomic::AtomicUsize,
}

pub(crate) struct Project {
    key: ProjectKey,
    /// None for a folder project.
    repo: Option<RepoInfo>,
    snapshot: Mutex<Arc<ProjectSnapshot>>,
    control: Mutex<Control>,
    /// Held while a snapshot is replaced and its event emitted, and while
    /// `Closed` is emitted: no `Changed` follows `Closed`.
    emit: Mutex<()>,
    /// Set on close; stops an index read in progress.
    cancel: AtomicBool,
    /// The fs_watch subscriptions and what the active tree was built from.
    /// Only the scan loop changes it; never held across IO.
    watch: Mutex<Watches>,
    /// The id of the active worktree's watch (0: none). Its sink drops the
    /// batches of an older watch without taking a lock.
    tree_watch: AtomicU64,
}

#[derive(Clone)]
struct RepoInfo {
    common_dir: PathBuf,
    hash: HashKind,
}

#[derive(Default)]
struct Control {
    running: bool,
    closed: bool,
    wanted: Option<Wanted>,
    /// What fs_watch reported since the last refresh.
    dirty: Dirty,
    /// The batches' guards, dropped when the round handling them ends.
    guards: Vec<BusyGuard>,
}

/// One round of the scan loop.
enum Job {
    Apply(Wanted),
    Refresh(Dirty),
}

/// What the scan loop should bring the snapshot to.
#[derive(Clone)]
struct Wanted {
    /// A new worktree list (from a reopen); None keeps the current one.
    worktrees: Option<Arc<[Worktree]>>,
    /// The worktree to make active (git projects).
    active: Option<WorktreeKey>,
    opened_at: RelPath,
}

impl Model {
    pub(crate) fn new(git: Arc<dyn GitAccess>, home: PathBuf) -> Arc<Model> {
        Model::with_folder_limit(git, home, FOLDER_LIMIT)
    }

    /// `new` with another folder walk limit, for tests.
    pub(crate) fn with_folder_limit(
        git: Arc<dyn GitAccess>,
        home: PathBuf,
        folder_limit: usize,
    ) -> Arc<Model> {
        Arc::new_cyclic(|me| Model {
            me: me.clone(),
            git,
            home,
            host: OnceLock::new(),
            watcher: OnceLock::new(),
            next_watch: AtomicU64::new(1),
            folder_limit,
            next_request: AtomicU64::new(1),
            projects: Mutex::new(Vec::new()),
            parsed: Mutex::new(ParseCache::new(PARSE_CACHE_BYTES)),
            others: Mutex::new(OtherTrees::default()),
            facts: Mutex::new(()),
            #[cfg(test)]
            parses: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    /// Connects the model to the kernel once the activation has started.
    pub(crate) fn attach(&self, host: Arc<dyn Host>) {
        let _ = self.host.set(host);
    }

    /// Refreshes open git projects from `watcher` (docs/design/project.md
    /// 「数据流与线程」). Set once, before the first `open`; no IO.
    pub(crate) fn watch_with(&self, watcher: Watcher) {
        let _ = self.watcher.set(watcher);
    }

    /// The plugin is deactivated: stop every loop and drop every snapshot,
    /// watch and cache. Emits nothing.
    pub(crate) fn shutdown(&self) {
        let projects = std::mem::take(&mut *lock(&self.projects));
        for project in projects {
            project.close();
            drop(project.take_watches());
        }
        lock(&self.parsed).clear();
        lock(&self.others).clear();
    }

    /// What `resolve_package` cached for the worktree at `git_dir`.
    #[cfg(test)]
    pub(crate) fn cached_packages(&self, git_dir: &Path) -> Option<Arc<[Package]>> {
        lock(&self.others).packages(git_dir)
    }

    fn host(&self) -> Option<&Arc<dyn Host>> {
        self.host.get()
    }

    fn busy(&self) -> Option<BusyGuard> {
        self.host()?.busy()
    }

    fn spawn(&self, work: impl FnOnce(&Arc<Model>) + Send + 'static) {
        let (Some(host), Some(me)) = (self.host(), self.me.upgrade()) else {
            return;
        };
        host.spawn(Box::new(move || work(&me)));
    }

    fn emit(&self, event: ProjectEvent) {
        if let Some(host) = self.host() {
            host.emit(event);
        }
    }

    fn find(&self, key: &ProjectKey) -> Option<Arc<Project>> {
        lock(&self.projects).iter().find(|p| p.key == *key).cloned()
    }

    /// Discovers what `path` opens. Blocking; on the executor.
    fn target(&self, path: &Path) -> Result<Target, OpenError> {
        let canonical = std::fs::canonicalize(path).map_err(|e| open_error(&e))?;
        let meta = std::fs::metadata(&canonical).map_err(|e| open_error(&e))?;
        if !meta.is_dir() {
            return Err(OpenError::NotADirectory);
        }
        let found = self
            .git
            .discover(&canonical)
            .map_err(|e| OpenError::RepoUnreadable(e.to_string().into()))?;
        let Some(found) = found.filter(|d| !self.is_home_repo(d, &canonical)) else {
            return Ok(Target::Folder { root: canonical });
        };
        let infos = self
            .git
            .worktrees(&found.common_dir)
            .map_err(|e| OpenError::RepoUnreadable(e.to_string().into()))?;
        Ok(Target::Git {
            path: canonical,
            found,
            worktrees: worktree_list(infos),
        })
    }

    /// The main worktree root is the home directory and `path` is somewhere
    /// below it: a dotfiles repository, so `path` opens as a folder.
    fn is_home_repo(&self, found: &Discovered, path: &Path) -> bool {
        let home = std::fs::canonicalize(&self.home).unwrap_or_else(|_| self.home.clone());
        found.git_dir == found.common_dir
            && found.worktree_root.as_deref() == Some(home.as_path())
            && path != home
    }

    fn open_task(&self, request: OpenRequest, path: &Path) {
        let target = match self.target(path) {
            Ok(target) => target,
            Err(error) => {
                self.emit(ProjectEvent::Opened {
                    request,
                    result: Err(error),
                });
                return;
            }
        };
        let (key, repo, wanted, initial) = match target {
            Target::Folder { root } => {
                let key = ProjectKey::Folder {
                    root: root.as_path().into(),
                };
                let snapshot = ProjectSnapshot {
                    key: key.clone(),
                    generation: 1,
                    name: dir_name(&root),
                    worktrees: Arc::from([]),
                    active: Some(ActiveTree {
                        root: root.as_path().into(),
                        worktree: None,
                        opened_at: "".into(),
                        files: Scan::Pending,
                        packages: Scan::Pending,
                    }),
                };
                let wanted = Wanted {
                    worktrees: None,
                    active: None,
                    opened_at: "".into(),
                };
                (key, None, wanted, snapshot)
            }
            Target::Git {
                path,
                found,
                worktrees,
            } => {
                let key = ProjectKey::Git {
                    common_dir: found.common_dir.as_path().into(),
                };
                let active = initial_active(&found, &worktrees);
                let opened_at = match (&active, &found.worktree_root) {
                    (Some(w), Some(root)) if *w.root == **root => {
                        rel_path(&path, root).unwrap_or_else(|| "".into())
                    }
                    _ => "".into(),
                };
                let snapshot = ProjectSnapshot {
                    key: key.clone(),
                    generation: 1,
                    name: repo_name(&found, &worktrees),
                    worktrees: worktrees.clone(),
                    active: active.map(|w| ActiveTree {
                        root: w.root.clone(),
                        worktree: Some(w.key.clone()),
                        opened_at: opened_at.clone(),
                        files: Scan::Pending,
                        packages: Scan::Pending,
                    }),
                };
                let wanted = Wanted {
                    active: active.map(|w| w.key.clone()),
                    worktrees: Some(worktrees.clone()),
                    opened_at,
                };
                let repo = RepoInfo {
                    common_dir: found.common_dir,
                    hash: found.hash,
                };
                (key, Some(repo), wanted, snapshot)
            }
        };
        let project = {
            let mut projects = lock(&self.projects);
            match projects.iter().find(|p| p.key == key) {
                Some(open) => open.clone(),
                None => {
                    let project = Arc::new(Project {
                        key: key.clone(),
                        repo,
                        snapshot: Mutex::new(Arc::new(initial)),
                        control: Mutex::new(Control::default()),
                        emit: Mutex::new(()),
                        cancel: AtomicBool::new(false),
                        watch: Mutex::new(Watches::default()),
                        tree_watch: AtomicU64::new(0),
                    });
                    projects.push(project.clone());
                    project
                }
            }
        };
        // `Opened` first: the loop's `Changed` must follow it.
        self.emit(ProjectEvent::Opened {
            request,
            result: Ok(key),
        });
        self.kick(&project, wanted);
    }

    /// Hands `wanted` to the project's loop, starting it when idle.
    fn kick(&self, project: &Arc<Project>, wanted: Wanted) {
        let mut control = lock(&project.control);
        if control.closed {
            return;
        }
        control.wanted = Some(match control.wanted.take() {
            // A newer target wins, but a newer worktree list survives it.
            Some(older) => Wanted {
                worktrees: wanted.worktrees.or(older.worktrees),
                ..wanted
            },
            None => wanted,
        });
        if control.running {
            return;
        }
        control.running = true;
        drop(control);
        let project = project.clone();
        self.spawn(move |model| model.run(&project));
    }

    /// Adds what an fs_watch batch reported to the project's dirty state
    /// and starts the loop when idle. `busy` stays held until the round
    /// that handles it ends. No IO: called on the fs_watch thread.
    fn mark(&self, project: &Arc<Project>, busy: Option<BusyGuard>, add: impl FnOnce(&mut Dirty)) {
        let mut control = lock(&project.control);
        if control.closed {
            return;
        }
        add(&mut control.dirty);
        control.guards.extend(busy);
        if control.running {
            return;
        }
        control.running = true;
        drop(control);
        let project = project.clone();
        self.spawn(move |model| model.run(&project));
    }

    /// Requests first (a full scan covers what is dirty in the active
    /// tree), then refreshes; one round per iteration.
    fn run(&self, project: &Arc<Project>) {
        loop {
            let (job, guards) = {
                let mut control = lock(&project.control);
                if control.closed {
                    control.running = false;
                    control.guards.clear();
                    return;
                }
                let guards = std::mem::take(&mut control.guards);
                let job = if let Some(wanted) = control.wanted.take() {
                    control.dirty.covered_by_scan();
                    Job::Apply(wanted)
                } else if !control.dirty.is_empty() {
                    Job::Refresh(std::mem::take(&mut control.dirty))
                } else {
                    control.running = false;
                    return;
                };
                (job, guards)
            };
            match job {
                Job::Apply(wanted) => self.apply(project, wanted),
                Job::Refresh(dirty) => self.refresh(project, dirty),
            }
            drop(guards);
        }
    }

    /// The deepest sub-project of the active tree containing `rel`.
    fn active_match(active: &ActiveTree, rel: &str) -> PackageMatch {
        match &active.packages {
            Scan::Ready(packages) => deepest(packages, rel),
            Scan::Pending => PackageMatch::NotLoaded,
            Scan::Unavailable(_) => PackageMatch::None,
        }
    }

    fn submit_facts(&self) {
        let Some(host) = self.host() else {
            return;
        };
        let _serial = lock(&self.facts);
        let projects = lock(&self.projects).clone();
        let trees = projects
            .iter()
            .filter_map(|p| match &p.snapshot().active.as_ref()?.files {
                Scan::Ready(files) => Some(files.clone()),
                _ => None,
            })
            .collect();
        host.workspace_changed(crate::workspace_facts(trees));
    }
}

impl Project {
    pub(crate) fn snapshot(&self) -> Arc<ProjectSnapshot> {
        lock(&self.snapshot).clone()
    }

    fn close(&self) {
        let mut control = lock(&self.control);
        control.closed = true;
        control.wanted = None;
        control.dirty = Dirty::default();
        control.guards.clear();
        drop(control);
        self.tree_watch.store(0, Ordering::Relaxed);
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// The watches, to drop off the UI thread: the active worktree's handle
    /// holds its parsed index.
    fn take_watches(&self) -> Watches {
        std::mem::take(&mut *lock(&self.watch))
    }

    /// A newer request or a close is waiting: the current step's result
    /// would be stale.
    fn superseded(&self) -> bool {
        let control = lock(&self.control);
        control.closed || control.wanted.is_some()
    }
}

impl Projects for Model {
    fn open(&self, path: &Path) -> OpenRequest {
        let request = OpenRequest(self.next_request.fetch_add(1, Ordering::Relaxed));
        let path = path.to_path_buf();
        self.spawn(move |model| model.open_task(request, &path));
        request
    }

    fn close(&self, project: &ProjectKey) {
        let removed = {
            let mut projects = lock(&self.projects);
            let i = projects.iter().position(|p| p.key == *project);
            i.map(|i| projects.remove(i))
        };
        let Some(removed) = removed else {
            return;
        };
        removed.close();
        self.spawn(move |model| {
            drop(removed.take_watches());
            {
                let _emit = lock(&removed.emit);
                model.emit(ProjectEvent::Closed {
                    project: removed.key.clone(),
                });
            }
            if let Some(repo) = &removed.repo {
                lock(&model.others).forget(&repo.common_dir);
            }
            model.submit_facts();
        });
    }

    fn set_active_worktree(&self, project: &ProjectKey, worktree: &WorktreeKey) {
        let Some(project) = self.find(project) else {
            return;
        };
        let snapshot = project.snapshot();
        if !snapshot
            .worktrees
            .iter()
            .any(|w| w.key == *worktree && !w.prunable)
        {
            return;
        }
        {
            let control = lock(&project.control);
            let current = snapshot.active.as_ref().and_then(|a| a.worktree.as_ref());
            if !control.running && control.wanted.is_none() && current == Some(worktree) {
                return;
            }
        }
        self.kick(
            &project,
            Wanted {
                worktrees: None,
                active: Some(worktree.clone()),
                opened_at: "".into(),
            },
        );
    }

    fn snapshot(&self, project: &ProjectKey) -> Option<Arc<ProjectSnapshot>> {
        self.find(project).map(|p| p.snapshot())
    }

    fn open_projects(&self) -> Vec<ProjectKey> {
        lock(&self.projects).iter().map(|p| p.key.clone()).collect()
    }

    fn locate(&self, path: &Path) -> Option<Location> {
        let projects = lock(&self.projects).clone();
        let mut best: Option<Candidate> = None;
        for project in &projects {
            let snapshot = project.snapshot();
            let roots: Vec<(Arc<Path>, Option<WorktreeKey>)> = match &project.key {
                ProjectKey::Folder { root } => vec![(root.clone(), None)],
                ProjectKey::Git { .. } => snapshot
                    .worktrees
                    .iter()
                    .filter(|w| !w.prunable)
                    .map(|w| (w.root.clone(), Some(w.key.clone())))
                    .collect(),
            };
            for (root, worktree) in roots {
                if !path.starts_with(&root) {
                    continue;
                }
                let depth = root.components().count();
                if best.as_ref().is_none_or(|(d, ..)| depth > *d) {
                    best = Some((depth, snapshot.clone(), root, worktree));
                }
            }
        }
        let (_, snapshot, root, worktree) = best?;
        let rel = rel_path(path, &root)?;
        let package = match &snapshot.active {
            Some(active) if active.worktree == worktree && *active.root == *root => {
                Model::active_match(active, &rel)
            }
            _ => PackageMatch::NotLoaded,
        };
        Some(Location {
            project: snapshot.key.clone(),
            worktree,
            rel,
            package,
        })
    }

    fn resolve(&self, path: &Path) -> Result<Resolved, ResolveError> {
        let canonical = std::fs::canonicalize(path).map_err(|e| open_error(&e))?;
        let found = self
            .git
            .discover(&canonical)
            .map_err(|e| OpenError::RepoUnreadable(e.to_string().into()))?;
        if let Some(found) = found.filter(|d| !self.is_home_repo(d, &canonical)) {
            let (worktree, rel) = match &found.worktree_root {
                Some(root) => (
                    Some(WorktreeKey(found.git_dir.as_path().into())),
                    rel_path(&canonical, root).unwrap_or_else(|| "".into()),
                ),
                None => (None, "".into()),
            };
            return Ok(Resolved {
                project: ProjectKey::Git {
                    common_dir: found.common_dir.as_path().into(),
                },
                worktree,
                rel,
            });
        }
        // Not in a repository: the deepest open folder project holding it,
        // else the directory itself (or the file's directory).
        let open = lock(&self.projects)
            .iter()
            .filter_map(|p| match &p.key {
                ProjectKey::Folder { root } if canonical.starts_with(root) => Some(root.clone()),
                _ => None,
            })
            .max_by_key(|root| root.components().count());
        let root: Arc<Path> = match open {
            Some(root) => root,
            None if canonical.is_dir() => canonical.as_path().into(),
            None => canonical
                .parent()
                .map_or_else(|| canonical.as_path().into(), Arc::from),
        };
        Ok(Resolved {
            project: ProjectKey::Folder { root: root.clone() },
            worktree: None,
            rel: rel_path(&canonical, &root).unwrap_or_else(|| "".into()),
        })
    }

    fn resolve_package(
        &self,
        project: &ProjectKey,
        worktree: &WorktreeKey,
        rel: &str,
    ) -> Result<PackageMatch, ScanError> {
        let project = self.find(project).ok_or(ScanError::RootMissing)?;
        let snapshot = project.snapshot();
        let tree = snapshot
            .worktrees
            .iter()
            .find(|w| w.key == *worktree)
            .ok_or(ScanError::RootMissing)?;
        if let Some(active) = &snapshot.active
            && active.worktree.as_ref() == Some(worktree)
        {
            return match &active.packages {
                Scan::Unavailable(error) => Err(error.clone()),
                _ => Ok(Model::active_match(active, rel)),
            };
        }
        let repo = project.repo.as_ref().ok_or(ScanError::RootMissing)?;
        if tree.prunable {
            return Err(ScanError::RootMissing);
        }
        let active = snapshot.active.as_ref().and_then(|a| match &a.packages {
            Scan::Ready(packages) => Some(packages),
            _ => None,
        });
        let packages = self.other_packages(repo, tree, &snapshot.name, active)?;
        Ok(deepest(&packages, rel))
    }
}

/// A root containing the located path: (depth of the root, snapshot, root,
/// worktree).
type Candidate = (usize, Arc<ProjectSnapshot>, Arc<Path>, Option<WorktreeKey>);

enum Target {
    Folder {
        root: PathBuf,
    },
    Git {
        path: PathBuf,
        found: Discovered,
        worktrees: Arc<[Worktree]>,
    },
}

/// The main worktree first, the rest by root path.
fn worktree_list(infos: Vec<WorktreeInfo>) -> Arc<[Worktree]> {
    let mut list: Vec<Worktree> = infos
        .into_iter()
        .map(|info| Worktree {
            key: WorktreeKey(info.git_dir.as_path().into()),
            root: info.root.as_path().into(),
            is_main: info.is_main,
            head: info.head,
            locked: info.locked,
            prunable: info.prunable,
        })
        .collect();
    list.sort_by(|a, b| b.is_main.cmp(&a.is_main).then_with(|| a.root.cmp(&b.root)));
    list.into()
}

/// The worktree the opened path is in; else (a bare repository, its
/// common directory, a prunable worktree's administrative directory) the
/// first one that is not prunable.
fn initial_active<'a>(found: &Discovered, worktrees: &'a [Worktree]) -> Option<&'a Worktree> {
    worktrees
        .iter()
        .find(|w| !w.prunable && *w.key.0 == *found.git_dir)
        .or_else(|| worktrees.iter().find(|w| !w.prunable))
}

/// The main worktree root's name; for a bare repository the common
/// directory's name without `.git`.
fn repo_name(found: &Discovered, worktrees: &[Worktree]) -> Arc<str> {
    if let Some(main) = worktrees.iter().find(|w| w.is_main) {
        return dir_name(&main.root);
    }
    let name = dir_name(&found.common_dir);
    match name.strip_suffix(".git") {
        Some(stem) if !stem.is_empty() => stem.into(),
        _ => name,
    }
}

fn dir_name(path: &Path) -> Arc<str> {
    match path.file_name() {
        Some(name) => name.to_string_lossy().into(),
        None => path.to_string_lossy().into(),
    }
}

/// `path` relative to `root`, `/`-separated; None when it is not below
/// `root` or not UTF-8.
pub(crate) fn rel_path(path: &Path, root: &Path) -> Option<RelPath> {
    let rest = path.strip_prefix(root).ok()?;
    let mut out = String::new();
    for component in rest.components() {
        let Component::Normal(part) = component else {
            return None;
        };
        if !out.is_empty() {
            out.push('/');
        }
        out.push_str(part.to_str()?);
    }
    Some(out.into())
}

/// The deepest package whose directory contains `rel`.
pub(crate) fn deepest(packages: &[Package], rel: &str) -> PackageMatch {
    packages
        .iter()
        .filter(|p| contains(&p.dir, rel))
        .max_by_key(|p| p.dir.len())
        .map_or(PackageMatch::None, |p| PackageMatch::Found(p.dir.clone()))
}

fn contains(dir: &str, rel: &str) -> bool {
    dir.is_empty()
        || rel
            .strip_prefix(dir)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

fn open_error(err: &io::Error) -> OpenError {
    match err.kind() {
        io::ErrorKind::NotFound => OpenError::NotFound,
        io::ErrorKind::PermissionDenied => OpenError::PermissionDenied,
        io::ErrorKind::NotADirectory => OpenError::NotADirectory,
        _ => OpenError::Io(err.to_string().into()),
    }
}

/// Opening a worktree failed: its root is gone, or git could not read it.
fn worktree_error(err: &GitError) -> ScanError {
    match err {
        GitError::Io(e) if e.kind() == io::ErrorKind::NotFound => ScanError::RootMissing,
        GitError::Io(e) => ScanError::Io(e.to_string().into()),
        other => ScanError::IndexUnreadable(other.to_string().into()),
    }
}

fn index_error(err: &GitError) -> ScanError {
    ScanError::IndexUnreadable(err.to_string().into())
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
