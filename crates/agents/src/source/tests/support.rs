//! 发现、监听、归位与插件测试的夹具：临时目录里的会话目录、真实的 `Kernel` 加 `Executor::manual()`、
//! `fs_watch` 的内存后端与手动驱动、临时目录里的 `cache`，以及模拟的 `Projects`。

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lectern_cache::Cache;
use lectern_executor::Executor;
use lectern_fs_watch::testing::{Driver, FakeBackend, manual};
use lectern_fs_watch::{Config, Flags};
use lectern_kernel::{
    Activation, Active, Admitted, Context, Kernel, Manifest, Plugin, PluginError, PluginSet,
    ServiceSet, service,
};
use lectern_services::project::{
    Location, OpenError, OpenRequest, PackageMatch, ProjectKey, ProjectSnapshot, Projects,
    ResolveError, Resolved, ScanError, WorktreeKey,
};
use lectern_services::session::{
    Place, SessionKey, SessionList, SessionSource, SessionsChanged, SourceId,
};

use crate::claude::ClaudeCode;
use crate::codex::Codex;
use crate::filter::SessionFilter;
use crate::roots::AgentRoots;
use crate::source::Engine;
use crate::{AgentsDeps, claude, codex, plugins};

pub(crate) const F: Flags = Flags::FILE;
pub(crate) const D: Flags = Flags::DIR;

pub(crate) fn claude_source() -> SourceId {
    SourceId(claude::SOURCE_ID.into())
}

pub(crate) fn codex_source() -> SourceId {
    SourceId(codex::SOURCE_ID.into())
}

pub(crate) fn key(source: &SourceId, id: &str) -> SessionKey {
    SessionKey {
        source: source.clone(),
        id: id.into(),
    }
}

/// 一次收到的 `SessionsChanged` 和收到时的列表。
#[derive(Clone)]
pub(crate) struct Seen {
    pub(crate) event: SessionsChanged,
    pub(crate) list: Arc<SessionList>,
}

type Sources = Arc<Mutex<Option<ServiceSet<dyn SessionSource>>>>;

/// 依赖 `dyn SessionSource`、记录每个 `SessionsChanged` 和当时的列表。
struct Consumer {
    sources: Sources,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Plugin for Consumer {
    fn manifest(&self) -> Manifest {
        Manifest {
            provides: Vec::new(),
            requires: vec![service::<dyn SessionSource>()],
            activation: vec![Activation::AfterFirstFrame],
        }
    }

    fn activate(&self, ctx: &mut Context<'_>) -> Result<Box<dyn Active>, PluginError> {
        let set = ctx.service_set::<dyn SessionSource>();
        *self.sources.lock().unwrap() = Some(set.clone());
        let seen = self.seen.clone();
        ctx.on::<SessionsChanged>(move |event, _| {
            let mut list = None;
            set.for_each(|s| {
                if s.id() == event.source {
                    list = Some(s.sessions());
                }
            });
            seen.lock().unwrap().push(Seen {
                event: event.clone(),
                list: list.expect("the emitting source is live"),
            });
        });
        Ok(Box::new(()))
    }
}

/// 提供模拟的 `dyn Projects`。
struct ProjectsPlugin(Arc<FakeProjects>);

impl Plugin for ProjectsPlugin {
    fn manifest(&self) -> Manifest {
        Manifest {
            provides: vec![service::<dyn Projects>()],
            requires: Vec::new(),
            activation: vec![Activation::AfterFirstFrame],
        }
    }

    fn activate(&self, ctx: &mut Context<'_>) -> Result<Box<dyn Active>, PluginError> {
        ctx.provide::<dyn Projects>(self.0.clone())?;
        Ok(Box::new(()))
    }
}

/// 一次运行（一个 kernel）。
pub(crate) struct Run {
    pub(crate) kernel: Arc<Kernel>,
    pub(crate) executor: Executor,
    pub(crate) driver: Driver,
    pub(crate) backend: Arc<FakeBackend>,
    pub(crate) seen: Arc<Mutex<Vec<Seen>>>,
    sources: Sources,
    engines: [EngineSlot; 2],
}

type EngineSlot = Arc<Mutex<Option<Arc<dyn std::any::Any + Send + Sync>>>>;

pub(crate) struct Harness {
    _dir: tempfile::TempDir,
    /// 临时目录，规范路径。
    pub(crate) base: PathBuf,
    pub(crate) roots: AgentRoots,
    pub(crate) projects: Arc<FakeProjects>,
    pub(crate) filter: SessionFilter,
    pub(crate) run: Option<Run>,
}

impl Harness {
    /// `<base>/claude/projects/` 与 `<base>/codex/sessions/` 已建好。
    pub(crate) fn new() -> Harness {
        let dir = tempfile::tempdir().expect("temporary directory");
        let base = std::fs::canonicalize(dir.path()).expect("canonical");
        let roots = AgentRoots {
            claude: vec![base.join("claude")],
            codex: vec![base.join("codex")],
        };
        std::fs::create_dir_all(base.join("claude/projects")).unwrap();
        std::fs::create_dir_all(base.join("codex/sessions")).unwrap();
        Harness {
            _dir: dir,
            base,
            roots,
            projects: Arc::new(FakeProjects::default()),
            filter: SessionFilter::new(),
            run: None,
        }
    }

    pub(crate) fn claude_root(&self) -> PathBuf {
        self.base.join("claude/projects")
    }

    pub(crate) fn codex_root(&self) -> PathBuf {
        self.base.join("codex/sessions")
    }

    pub(crate) fn cache_dir(&self) -> PathBuf {
        self.base.join("cache")
    }

    /// 启动 kernel（两个插件、模拟的 `Projects` 与记录事件的消费方），处理完第一次发现。
    pub(crate) fn start(&mut self) {
        self.start_without_settling();
        self.settle();
    }

    /// 启动，但不运行 executor（第一次发现还在队列里）。
    pub(crate) fn start_without_settling(&mut self) {
        assert!(self.run.is_none(), "already running");
        let backend = FakeBackend::new();
        let executor = Executor::manual();
        let (watcher, driver) = manual(backend.clone(), executor.clone(), Config::default());
        let cache = Cache::new(lectern_cache::Config {
            root: self.cache_dir(),
            budgets: lectern_cache::Budgets::default(),
            ui_thread: None,
        });
        let deps = AgentsDeps {
            executor: executor.clone(),
            cache,
            watch: watcher,
            decoded_dir: self.base.join("decoded"),
            roots: self.roots.clone(),
            filter: self.filter.clone(),
        };
        let sources = Sources::default();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let [(claude_id, claude), (codex_id, codex)] = plugins(deps);
        let engines = [claude.engine.clone(), codex.engine.clone()];
        let set = PluginSet::new()
            .with("projects", ProjectsPlugin(self.projects.clone()))
            .with(claude_id, claude)
            .with(codex_id, codex)
            .with(
                "consumer",
                Consumer {
                    sources: sources.clone(),
                    seen: seen.clone(),
                },
            );
        let kernel = Kernel::start(set);
        kernel.barrier();
        self.run = Some(Run {
            kernel,
            executor,
            driver,
            backend,
            seen,
            sources,
            engines,
        });
    }

    pub(crate) fn claude_engine(&self) -> Arc<Engine<ClaudeCode>> {
        self.engine(0)
    }

    pub(crate) fn codex_engine(&self) -> Arc<Engine<Codex>> {
        self.engine(1)
    }

    fn engine<T: Send + Sync + 'static>(&self, i: usize) -> Arc<T> {
        let any = self.run().engines[i]
            .lock()
            .unwrap()
            .clone()
            .expect("activated");
        any.downcast::<T>().expect("engine type")
    }

    /// 关掉 kernel（模拟退出）；`cache` 与会话目录留着，下一次 `start` 是重启。
    pub(crate) fn stop(&mut self) {
        // Let the background writes (turn indexes into the cache) finish first, as an exit would.
        if self.run.is_some() {
            self.settle();
        }
        if let Some(run) = self.run.take() {
            let (done, wait) = std::sync::mpsc::channel();
            run.kernel.shutdown(move || {
                let _ = done.send(());
            });
            let _ = wait.recv_timeout(Duration::from_secs(10));
            run.executor.shutdown(Duration::from_secs(10));
        }
    }

    pub(crate) fn run(&self) -> &Run {
        self.run.as_ref().expect("running")
    }

    /// 处理完 fs_watch 队列和 executor 里的全部工作。
    pub(crate) fn settle(&self) {
        let run = self.run();
        loop {
            run.driver.run_until_idle();
            run.kernel.barrier();
            if run.driver.run_until_idle() == 0 {
                return;
            }
        }
    }

    /// 推进假时钟，让 250 ms / 1 s 的合并窗口结束，再处理完。
    pub(crate) fn advance(&self) {
        // The raw events enter the queue first; their window starts when they are processed.
        self.settle();
        self.run().driver.advance(Duration::from_secs(2));
        self.settle();
    }

    /// 在会话根目录 `root` 的监听上注入事件（路径相对 `root`），然后推进时钟、处理完。
    pub(crate) fn emit(&self, root: &Path, events: &[(&str, Flags)]) {
        self.run().backend.emit(root, events);
        self.advance();
    }

    pub(crate) fn source(&self, id: &SourceId) -> Admitted<dyn SessionSource> {
        let sources = self.run().sources.lock().unwrap();
        let set = sources.as_ref().expect("consumer is active");
        set.current()
            .into_iter()
            .find(|s| s.id() == *id)
            .expect("the source is live")
    }

    pub(crate) fn list(&self, id: &SourceId) -> Arc<SessionList> {
        self.source(id).sessions()
    }

    /// 收到的事件，按来源过滤，取走。
    pub(crate) fn take_seen(&self, id: &SourceId) -> Vec<Seen> {
        let mut seen = self.run().seen.lock().unwrap();
        let (mine, rest): (Vec<Seen>, Vec<Seen>) =
            seen.drain(..).partition(|s| s.event.source == *id);
        *seen = rest;
        mine
    }

    /// 路径里的临时目录换成 `[base]`。
    pub(crate) fn rel(&self, path: &Path) -> String {
        path.display()
            .to_string()
            .replace(&self.base.display().to_string(), "[base]")
    }

    /// `path` 相对 `root` 的写法（注入事件用）。
    pub(crate) fn rel_to(&self, root: &Path, path: &Path) -> String {
        path.strip_prefix(root)
            .expect("under the root")
            .display()
            .to_string()
    }

    /// 每个会话一行，供快照阅读。
    pub(crate) fn render(&self, list: &SessionList) -> String {
        let mut out = String::new();
        for s in list.sessions.iter() {
            let parent = s
                .parent
                .as_ref()
                .map(|p| {
                    format!(
                        " parent={}{}",
                        p.session.id,
                        p.tool_call
                            .as_ref()
                            .map(|t| format!("@{}", t.0))
                            .unwrap_or_default()
                    )
                })
                .unwrap_or_default();
            let flags = [
                (s.open_turn, " open"),
                (s.tail_pending, " tail_pending"),
                (s.archived, " archived"),
                (s.inheritance_unknown, " inheritance_unknown"),
            ]
            .iter()
            .filter(|(on, _)| *on)
            .map(|(_, name)| *name)
            .collect::<String>();
            writeln!(
                out,
                "{} file={} cwd={} place={} title={:?} last={}{parent}{flags} {:?}",
                s.key.id,
                self.rel(&s.file),
                self.rel(&s.cwd),
                self.place(&s.place),
                s.title.as_deref().unwrap_or(""),
                s.last_activity.0,
                s.format,
            )
            .unwrap();
        }
        out
    }

    pub(crate) fn place(&self, place: &Place) -> String {
        match place {
            Place::Pending => "Pending".into(),
            Place::Missing => "Missing".into(),
            Place::Unresolvable(e) => format!("Unresolvable({e:?})"),
            Place::Located(l) => format!(
                "Located({} wt={} rel={:?} {:?})",
                self.project(&l.project),
                l.worktree
                    .as_ref()
                    .map(|w| self.rel(&w.0))
                    .unwrap_or_else(|| "-".into()),
                l.rel,
                l.package
            ),
            Place::Resolved(r) => format!(
                "Resolved({} wt={} rel={:?})",
                self.project(&r.project),
                r.worktree
                    .as_ref()
                    .map(|w| self.rel(&w.0))
                    .unwrap_or_else(|| "-".into()),
                r.rel
            ),
        }
    }

    fn project(&self, key: &ProjectKey) -> String {
        match key {
            ProjectKey::Git { common_dir } => format!("git:{}", self.rel(common_dir)),
            ProjectKey::Folder { root } => format!("folder:{}", self.rel(root)),
        }
    }

    /// 复制一份 fixture 文件到 `to`（建好上级目录）。
    pub(crate) fn copy(&self, from: &Path, to: &Path) {
        std::fs::create_dir_all(to.parent().unwrap()).unwrap();
        std::fs::copy(from, to).unwrap();
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.stop();
    }
}

/// 写一个合成的 Claude Code 会话：每个 prompt 一轮（`user` 加 `end_turn` 的 `assistant`）。
pub(crate) fn claude_session(id: &str, cwd: &Path, prompts: &[&str], start_sec: u32) -> String {
    let mut out = String::new();
    for (i, prompt) in prompts.iter().enumerate() {
        out.push_str(&claude_turn(id, cwd, prompt, start_sec + 2 * i as u32, i));
    }
    out
}

/// 一轮：`user` 加 `end_turn` 的 `assistant`，各一行。
pub(crate) fn claude_turn(id: &str, cwd: &Path, prompt: &str, sec: u32, n: usize) -> String {
    let cwd = cwd.display();
    let ts = |s: u32| format!("2026-10-09T09:{:02}:{:02}.000Z", s / 60 % 60, s % 60);
    format!(
        concat!(
            r#"{{"parentUuid":null,"isSidechain":false,"userType":"external","cwd":"{cwd}","sessionId":"{id}","version":"2.1.302","gitBranch":"main","type":"user","message":{{"role":"user","content":"{prompt}"}},"origin":{{"kind":"human"}},"uuid":"u-{id}-{n}","timestamp":"{t0}"}}"#,
            "\n",
            r#"{{"parentUuid":"u-{id}-{n}","isSidechain":false,"userType":"external","cwd":"{cwd}","sessionId":"{id}","version":"2.1.302","gitBranch":"main","type":"assistant","requestId":"req_{n}","message":{{"id":"msg_{id}_{n}","type":"message","role":"assistant","model":"fixture-model-a","content":[{{"type":"text","text":"Done: {prompt}"}}],"stop_reason":"end_turn","usage":{{"input_tokens":1,"output_tokens":1}}}},"uuid":"a-{id}-{n}","timestamp":"{t1}"}}"#,
            "\n"
        ),
        cwd = cwd,
        id = id,
        prompt = prompt,
        n = n,
        t0 = ts(sec),
        t1 = ts(sec + 1),
    )
}

/// 合成的会话 ID（UUID 的规范写法）。
pub(crate) fn uuid(n: u32) -> String {
    format!("{n:08x}-0000-4000-8000-000000000000")
}

/// 用 `ruzstd` 压缩一个文件到 `<path>.zst`。
pub(crate) fn compress(path: &Path) -> PathBuf {
    let data = std::fs::read(path).unwrap();
    let zst =
        ruzstd::encoding::compress_to_vec(&data[..], ruzstd::encoding::CompressionLevel::Fastest);
    let out = PathBuf::from(format!("{}.zst", path.display()));
    std::fs::write(&out, zst).unwrap();
    out
}

// --- 模拟的 Projects -------------------------------------------------------------

/// 一个已打开项目里的 worktree（或文件夹项目的根）。
#[derive(Clone)]
pub(crate) struct FakeTree {
    pub(crate) root: PathBuf,
    /// 文件夹项目为 None。
    pub(crate) key: Option<WorktreeKey>,
    pub(crate) active: bool,
    /// 子项目目录（相对路径）；None：还在加载（`Pending`）。
    pub(crate) packages: Option<Vec<String>>,
}

#[derive(Clone)]
pub(crate) struct FakeProject {
    pub(crate) key: ProjectKey,
    pub(crate) trees: Vec<FakeTree>,
}

#[derive(Default)]
struct FakeState {
    /// 已打开的项目。
    open: Vec<FakeProject>,
    /// `resolve` 认得的项目（打开与否都在这里）。
    known: Vec<FakeProject>,
    resolves: Vec<PathBuf>,
    packages: Vec<(PathBuf, String)>,
}

/// 只在内存里的 `Projects`：`locate` 按已打开项目的树根做前缀比较，`resolve` 按认得的项目归位，
/// 其余路径归为它自己的文件夹项目；记录每次 `resolve` 与 `resolve_package`。
#[derive(Default)]
pub(crate) struct FakeProjects(Mutex<FakeState>);

impl FakeProjects {
    /// 加一个 `resolve` 认得的项目；`open` 为 true 时也算已打开。
    pub(crate) fn add(&self, project: FakeProject, open: bool) {
        let mut s = self.0.lock().unwrap();
        s.known.push(project.clone());
        if open {
            s.open.push(project);
        }
    }

    pub(crate) fn open(&self, key: &ProjectKey) {
        let mut s = self.0.lock().unwrap();
        let p = s.known.iter().find(|p| p.key == *key).cloned().unwrap();
        s.open.retain(|o| o.key != *key);
        s.open.push(p);
    }

    pub(crate) fn set_packages(&self, root: &Path, packages: Option<Vec<String>>) {
        let mut s = self.0.lock().unwrap();
        let FakeState { open, known, .. } = &mut *s;
        for p in open.iter_mut().chain(known.iter_mut()) {
            for t in &mut p.trees {
                if t.root == root {
                    t.packages.clone_from(&packages);
                }
            }
        }
    }

    pub(crate) fn resolves(&self) -> Vec<PathBuf> {
        self.0.lock().unwrap().resolves.clone()
    }

    pub(crate) fn package_calls(&self) -> Vec<(PathBuf, String)> {
        self.0.lock().unwrap().packages.clone()
    }
}

fn rel_of(path: &Path, root: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    Some(
        rel.components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/"),
    )
}

fn package_of(packages: &[String], rel: &str) -> PackageMatch {
    packages
        .iter()
        .filter(|p| rel == p.as_str() || rel.starts_with(&format!("{p}/")))
        .max_by_key(|p| p.len())
        .map_or(PackageMatch::None, |p| {
            PackageMatch::Found(p.as_str().into())
        })
}

fn deepest<'a>(
    projects: &'a [FakeProject],
    path: &Path,
) -> Option<(&'a FakeProject, &'a FakeTree, String)> {
    projects
        .iter()
        .flat_map(|p| p.trees.iter().map(move |t| (p, t)))
        .filter_map(|(p, t)| rel_of(path, &t.root).map(|rel| (p, t, rel)))
        .max_by_key(|(_, t, _)| t.root.as_os_str().len())
}

impl Projects for FakeProjects {
    fn open(&self, _: &Path) -> OpenRequest {
        OpenRequest(0)
    }

    fn close(&self, project: &ProjectKey) {
        self.0.lock().unwrap().open.retain(|p| p.key != *project);
    }

    fn set_active_worktree(&self, _: &ProjectKey, _: &WorktreeKey) {}

    fn snapshot(&self, _: &ProjectKey) -> Option<Arc<ProjectSnapshot>> {
        None
    }

    fn open_projects(&self) -> Vec<ProjectKey> {
        self.0
            .lock()
            .unwrap()
            .open
            .iter()
            .map(|p| p.key.clone())
            .collect()
    }

    fn locate(&self, path: &Path) -> Option<Location> {
        let s = self.0.lock().unwrap();
        let (project, tree, rel) = deepest(&s.open, path)?;
        let package = match (&tree.packages, tree.active) {
            (Some(packages), true) => package_of(packages, &rel),
            _ => PackageMatch::NotLoaded,
        };
        Some(Location {
            project: project.key.clone(),
            worktree: tree.key.clone(),
            rel: rel.into(),
            package,
        })
    }

    fn resolve(&self, path: &Path) -> Result<Resolved, ResolveError> {
        let mut s = self.0.lock().unwrap();
        s.resolves.push(path.to_path_buf());
        if !path.exists() {
            return Err(OpenError::NotFound);
        }
        if let Some((project, tree, rel)) = deepest(&s.known, path) {
            return Ok(Resolved {
                project: project.key.clone(),
                worktree: tree.key.clone(),
                rel: rel.into(),
            });
        }
        Ok(Resolved {
            project: ProjectKey::Folder { root: path.into() },
            worktree: None,
            rel: "".into(),
        })
    }

    fn resolve_package(
        &self,
        project: &ProjectKey,
        worktree: &WorktreeKey,
        rel: &str,
    ) -> Result<PackageMatch, ScanError> {
        let mut s = self.0.lock().unwrap();
        s.packages.push((worktree.0.to_path_buf(), rel.to_owned()));
        let tree = s
            .open
            .iter()
            .filter(|p| p.key == *project)
            .flat_map(|p| p.trees.iter())
            .find(|t| t.key.as_ref() == Some(worktree))
            .ok_or(ScanError::RootMissing)?;
        Ok(match (&tree.packages, tree.active) {
            (None, true) => PackageMatch::NotLoaded,
            (None, false) => PackageMatch::None,
            (Some(packages), _) => package_of(packages, rel),
        })
    }
}

/// 一个 git 项目：主 worktree 在 `root`，另有若干 linked worktree。
pub(crate) fn git_project(root: &Path, linked: &[&Path]) -> FakeProject {
    let common = root.join(".git");
    let mut trees = vec![FakeTree {
        root: root.to_path_buf(),
        key: Some(WorktreeKey(common.clone().into())),
        active: true,
        packages: Some(Vec::new()),
    }];
    for l in linked {
        trees.push(FakeTree {
            root: l.to_path_buf(),
            key: Some(WorktreeKey(
                common.join("worktrees").join(l.file_name().unwrap()).into(),
            )),
            active: false,
            packages: None,
        });
    }
    FakeProject {
        key: ProjectKey::Git {
            common_dir: common.into(),
        },
        trees,
    }
}
