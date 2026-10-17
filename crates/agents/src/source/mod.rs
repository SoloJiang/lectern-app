//! `SessionSource` 的实现：发现、监听、归位、轮次索引缓存与补齐（docs/design/agents.md 验收第 4 步）。
//!
//! 每种 agent 一个 [`Engine`]，包着一个适配器（[`Adapter`]）。所有 IO 都在 executor 的 `Background`
//! 任务里进行（插件里经 `PluginHandle::own_with` 提交，任务带一个 `BusyGuard`）；`sessions`、`resume`、
//! `complete` 只在内存里操作。没有宿主时（`testing`）任务在调用线程上同步运行。

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, Weak};

use lectern_cache::Partition;
use lectern_executor::{Executor, Priority};
use lectern_fs_watch::{Coalesce, DirBatch, DirSpec, DirWatch, Sink, Watcher};
use lectern_kernel::{BusyGuard, PluginHandle};
use lectern_services::project::ProjectEvent;
use lectern_services::session::{
    Agent, Batch, Content, ContentRef, Cursor, Diagnostics, Place, ResumeCommand, SessionError,
    SessionKey, SessionList, SessionSource, SessionSummary, SessionsChanged, SourceId, TurnIndex,
};

use crate::filter::SessionFilter;
use crate::index::IndexBuilder;
use crate::stats::ParseStats;

pub(crate) mod adapter;
mod cached;
pub(crate) mod discover;
mod index_codec;
pub(crate) mod place;

#[cfg(test)]
mod tests;

pub(crate) use adapter::Adapter;
use cached::{CachedIndex, CachedSummary, Indexes, index_key, summary_key};
use discover::{FileId, walk};
use place::{Places, ProjectsRef};

/// 列表发布之后主动补齐末尾字段的压缩会话数（按修改时间取最近的）。
pub(crate) const AUTO_COMPLETE: usize = 8;
/// 同时运行的补齐任务数。
pub(crate) const MAX_COMPLETING: usize = 2;

/// 构造 [`Engine`] 需要的外部资源。
pub(crate) struct Setup {
    /// 会话根目录（不需要存在）。
    pub(crate) roots: Vec<PathBuf>,
    /// 没有时（`testing`）任务在调用线程上同步运行。
    pub(crate) executor: Option<Executor>,
    /// 应用级分库；没有时不读写持久缓存。
    pub(crate) cache: Option<Partition>,
    /// 没有时不监听。
    pub(crate) watcher: Option<Watcher>,
    pub(crate) projects: ProjectsRef,
    pub(crate) filter: SessionFilter,
    /// 摘要与索引缓存键里的适配器版本。
    pub(crate) adapter_version: u32,
}

/// 一个会话来源。
pub(crate) struct Engine<A: Adapter> {
    me: Weak<Engine<A>>,
    adapter: A,
    setup: Setup,
    host: OnceLock<PluginHandle>,
    list: Mutex<Arc<SessionList>>,
    state: Mutex<State<A::File>>,
    places: Mutex<Places>,
    indexes: Mutex<Indexes>,
    /// 发布（组装列表、替换、发事件）一次只有一个。
    published: Mutex<Published>,
    watches: Mutex<Vec<DirWatch>>,
    /// 没有 executor 时（`testing`）还没写进 `cache` 的索引，drop 时写。
    deferred: Mutex<Vec<Arc<TurnIndex>>>,
    /// 测试用：适配器生成整份摘要的次数。
    #[cfg(test)]
    pub(crate) full_summaries: std::sync::atomic::AtomicUsize,
    /// 测试用：同时运行的补齐任务数的最大值。
    #[cfg(test)]
    pub(crate) max_completing: std::sync::atomic::AtomicUsize,
}

struct State<F> {
    stopped: bool,
    /// 有刷新任务已提交或正在运行。
    scheduled: bool,
    work: Work,
    /// 每个会话文件（含同一会话的其他候选路径）。
    known: HashMap<Arc<Path>, Known<F>>,
    /// 每个会话根目录下读不了的目录。
    unreadable: HashMap<PathBuf, Vec<Arc<Path>>>,
    /// 最近一次发布时每个会话选中的路径。
    chosen: HashMap<SessionKey, Arc<Path>>,
    /// 第一次发布之后才有主动补齐。
    published_once: bool,
    completion: Completion,
    /// 读过之后重新生成过摘要的会话（Codex 分叉：读取时才扫描来源，摘要据此更新一次）。
    resummarized: HashSet<(SessionKey, FileId)>,
}

#[derive(Default)]
struct Work {
    /// 要重新枚举的目录（会话根目录，或事件里的目录）。
    rescan: BTreeSet<PathBuf>,
    /// 事件里的路径。
    paths: BTreeSet<PathBuf>,
    /// 身份没变也要重新生成摘要的路径。
    forced: BTreeSet<PathBuf>,
    projects: Vec<ProjectEvent>,
    guards: Vec<BusyGuard>,
}

impl Work {
    fn is_empty(&self) -> bool {
        self.rescan.is_empty()
            && self.paths.is_empty()
            && self.forced.is_empty()
            && self.projects.is_empty()
    }
}

struct Known<F> {
    file: F,
    id: FileId,
    /// 读不到 cwd、格式认不出（`Unrecognized`）或读失败的文件为 None：不列出。
    summary: Option<(SessionSummary, ParseStats)>,
}

#[derive(Default)]
struct Completion {
    queue: VecDeque<SessionKey>,
    queued: HashSet<SessionKey>,
    running: usize,
}

#[derive(Default)]
struct Published {
    generation: u64,
    sessions: HashMap<SessionKey, SessionSummary>,
}

/// 要检查的路径，`forced` 时身份没变也重新生成摘要。
struct Check {
    path: PathBuf,
    forced: bool,
}

impl<A: Adapter> Engine<A> {
    pub(crate) fn new(adapter: impl FnOnce(&Weak<Engine<A>>) -> A, setup: Setup) -> Arc<Engine<A>> {
        Arc::new_cyclic(|me| Engine {
            me: me.clone(),
            adapter: adapter(me),
            setup,
            host: OnceLock::new(),
            list: Mutex::new(Arc::new(SessionList::default())),
            state: Mutex::new(State {
                stopped: false,
                scheduled: false,
                work: Work::default(),
                known: HashMap::new(),
                unreadable: HashMap::new(),
                chosen: HashMap::new(),
                published_once: false,
                completion: Completion::default(),
                resummarized: HashSet::new(),
            }),
            places: Mutex::new(Places::default()),
            indexes: Mutex::new(Indexes::default()),
            published: Mutex::new(Published::default()),
            watches: Mutex::new(Vec::new()),
            deferred: Mutex::new(Vec::new()),
            #[cfg(test)]
            full_summaries: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            max_completing: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    fn state(&self) -> MutexGuard<'_, State<A::File>> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn indexes(&self) -> MutexGuard<'_, Indexes> {
        self.indexes.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 插件的 `started`：先注册每个会话根目录的监听，再排一次全部根目录的枚举（「数据流与线程」第 2 步）。
    /// 枚举期间收到的事件进同一个待办，枚举结束后与扫描结果一起处理，然后才发布。
    pub(crate) fn start(&self, handle: Option<PluginHandle>) {
        if let Some(handle) = handle {
            let _ = self.host.set(handle);
        }
        if let Some(watcher) = &self.setup.watcher {
            let watches: Vec<DirWatch> = self
                .setup
                .roots
                .iter()
                .map(|root| {
                    watcher.watch_dir(
                        DirSpec {
                            root: root.clone(),
                            coalesce: Coalesce::DIR,
                        },
                        Arc::new(DirSink {
                            engine: self.me.clone(),
                            root: root.clone(),
                        }),
                    )
                })
                .collect();
            *self.watches.lock().unwrap_or_else(|e| e.into_inner()) = watches;
        }
        let guard = self.busy();
        let mut state = self.state();
        state.work.rescan.extend(self.setup.roots.iter().cloned());
        state.work.guards.extend(guard);
        drop(state);
        self.schedule();
    }

    /// 插件停用：不再提交任务，drop 目录监听。已提交的任务由 kernel 取消。
    pub(crate) fn stop(&self) {
        self.state().stopped = true;
        let watches = std::mem::take(&mut *self.watches.lock().unwrap_or_else(|e| e.into_inner()));
        drop(watches);
    }

    fn busy(&self) -> Option<BusyGuard> {
        self.host.get().and_then(PluginHandle::busy)
    }

    /// 在 executor 上运行 `work`，带一个 `BusyGuard`；没有宿主时在调用线程上同步运行。
    /// 返回 false：插件已停用，没有运行。
    fn spawn(&self, work: impl FnOnce(&Engine<A>) + Send + 'static) -> bool {
        let Some(executor) = &self.setup.executor else {
            work(self);
            return true;
        };
        let Some(handle) = self.host.get() else {
            return false;
        };
        let Some(busy) = handle.busy() else {
            return false;
        };
        let executor = executor.clone();
        let me = self.me.clone();
        handle.own_with(move |gate| {
            executor.run_gated(&gate, Priority::Background, move || {
                let _busy = busy;
                if let Some(engine) = me.upgrade() {
                    work(&engine);
                }
            })
        })
    }

    /// 有待办时提交一个刷新任务（同时只有一个）。
    fn schedule(&self) {
        {
            let mut state = self.state();
            if state.stopped || state.work.is_empty() {
                // 没有要做的事：随批次交来的守卫不再需要。
                if !state.scheduled {
                    state.work.guards.clear();
                }
                return;
            }
            if state.scheduled {
                return;
            }
            state.scheduled = true;
        }
        if !self.spawn(|engine| engine.refresh()) {
            self.state().scheduled = false;
        }
    }

    fn on_batch(&self, root: &Path, batch: DirBatch, busy: Option<BusyGuard>) {
        let mut state = self.state();
        if batch.rescan || batch.root.is_some() {
            state.work.rescan.insert(root.to_path_buf());
        }
        state.work.paths.extend(batch.paths);
        state.work.guards.extend(busy);
        drop(state);
        self.schedule();
    }

    /// `ProjectEvent` 的监听：只记下，归位在刷新任务里。
    pub(crate) fn on_project_event(&self, event: &ProjectEvent) {
        let guard = self.busy();
        let mut state = self.state();
        state.work.projects.push(event.clone());
        state.work.guards.extend(guard);
        drop(state);
        self.schedule();
    }

    /// 刷新任务：处理待办直到没有，每一轮之后发布一次。
    fn refresh(&self) {
        loop {
            let work = {
                let mut state = self.state();
                if state.stopped || state.work.is_empty() {
                    state.scheduled = false;
                    state.work.guards.clear();
                    return;
                }
                std::mem::take(&mut state.work)
            };
            let first = !self.state().published_once;
            self.process(&work);
            self.publish(first);
            if first {
                self.auto_complete();
            }
            drop(work.guards);
        }
    }

    fn process(&self, work: &Work) {
        let mut checks: Vec<Check> = Vec::new();
        let mut seen = HashSet::new();
        let mut add = |checks: &mut Vec<Check>, path: PathBuf, forced: bool| {
            if seen.insert(path.clone()) {
                checks.push(Check { path, forced });
            } else if forced && let Some(c) = checks.iter_mut().find(|c| c.path == path) {
                c.forced = true;
            }
        };
        let wanted = |p: &Path| self.adapter.session_file(p).is_some();

        // 枚举目录：目录下的文件都检查一遍，没有再出现的已知文件也检查（删除）。
        let mut dirs: Vec<PathBuf> = work.rescan.iter().cloned().collect();
        for path in &work.paths {
            if std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir()) {
                dirs.push(path.clone());
            }
        }
        for dir in &dirs {
            let listing = walk(dir, &wanted);
            if self.setup.roots.iter().any(|r| r == dir) {
                self.state()
                    .unreadable
                    .insert(dir.clone(), listing.unreadable);
            }
            for file in listing.files {
                add(&mut checks, file, false);
            }
        }
        // 事件里的路径和目录之下的已知文件（目录被删除、改名时只报告目录）。
        {
            let state = self.state();
            let under = |p: &Path| {
                work.paths
                    .iter()
                    .chain(dirs.iter())
                    .any(|d| p.starts_with(d))
            };
            let mut known: Vec<PathBuf> = state
                .known
                .keys()
                .filter(|p| under(p))
                .map(|p| p.to_path_buf())
                .collect();
            known.sort();
            drop(state);
            for path in known {
                add(&mut checks, path, false);
            }
        }
        for path in &work.paths {
            add(&mut checks, path.clone(), false);
        }
        for path in &work.forced {
            add(&mut checks, path.clone(), true);
        }
        for check in checks {
            if let Some(target) = self.adapter.related(&check.path) {
                if self.state().known.contains_key(target.as_path()) {
                    self.check(&target, true);
                }
                continue;
            }
            self.check(&check.path, check.forced);
        }

        // 归位：事件影响的 cwd，以及还没有归位的 cwd。
        let cwds: Vec<Arc<Path>> = self
            .state()
            .known
            .values()
            .filter_map(|k| k.summary.as_ref().map(|(s, _)| s.cwd.clone()))
            .collect();
        let mut places = self.places.lock().unwrap_or_else(|e| e.into_inner());
        for event in &work.projects {
            places.replace(&self.setup.projects, event);
        }
        let keep: HashSet<&Path> = cwds.iter().map(|c| &**c).collect();
        places.retain(&|cwd| keep.contains(cwd));
        places.place_new(&self.setup.projects, cwds.iter());
    }

    /// 检查一个路径：新文件读摘要，变化的文件重新生成摘要，消失的文件移除。
    fn check(&self, path: &Path, forced: bool) {
        let Some(file) = self.adapter.session_file(path) else {
            return;
        };
        let meta = std::fs::metadata(path);
        let key = A::key(&file).clone();
        let id = match meta {
            Ok(m) if m.is_file() => FileId::of(&m),
            _ => {
                let removed = self.state().known.remove(path);
                if let Some(old) = removed {
                    if A::compressed(&old.file) {
                        self.adapter.removed(&key);
                    }
                    self.forget_index_if_gone(&key);
                }
                return;
            }
        };
        let prev = {
            let state = self.state();
            state.known.get(path).map(|k| (k.id, k.summary.clone()))
        };
        if !forced && prev.as_ref().is_some_and(|(old, _)| *old == id) {
            return;
        }
        let summary = self.summarize(&file, &id, prev, forced);
        let mut state = self.state();
        if state.stopped {
            return;
        }
        state
            .known
            .insert(A::path(&file).clone(), Known { file, id, summary });
    }

    /// 会话的全部候选都不在了：内存里的索引不再需要。
    fn forget_index_if_gone(&self, key: &SessionKey) {
        let gone = !self.state().known.values().any(|k| A::key(&k.file) == key);
        if gone {
            self.indexes().remove(key);
        }
    }

    /// 生成摘要。压缩会话先查摘要缓存；在增量读取中的文件（内存里有它的轮次索引）只读索引游标之后追加的部分，
    /// 末尾字段取解析器状态，不再读末尾（「数据流与线程」第 2 步、第 5 步）。
    fn summarize(
        &self,
        file: &A::File,
        id: &FileId,
        prev: Option<(FileId, Option<(SessionSummary, ParseStats)>)>,
        forced: bool,
    ) -> Option<(SessionSummary, ParseStats)> {
        let key = A::key(file);
        if A::compressed(file) {
            if let Some(cache) = &self.setup.cache
                && let Some(hit) =
                    cache.get::<CachedSummary>(&summary_key(key, id, self.setup.adapter_version))
            {
                let (mut summary, stats) = hit.into_parts();
                if summary.key == *key {
                    summary.file = A::path(file).clone();
                    return Some((summary, stats));
                }
            }
        } else if !forced
            && let Some((old_id, Some((old, stats)))) = &prev
            && old_id.same_file(id)
            && id.len >= old_id.len
            && old.title.is_some()
            && let Some(updated) = self.follow(file, old)
        {
            return Some((updated, stats.clone()));
        }
        #[cfg(test)]
        self.full_summaries
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.adapter.summarize(file).ok()
    }

    /// 追加之后：内存里的索引在这个文件上、游标仍有效时接着解析，末尾字段取新游标里的解析器状态。
    fn follow(&self, file: &A::File, old: &SessionSummary) -> Option<SessionSummary> {
        let key = A::key(file);
        if self.state().chosen.get(key) != Some(A::path(file)) {
            return None;
        }
        let (index, _) = self.indexes().get(key)?;
        let now = FileId::stat(A::path(file)).ok()?;
        let next = self.adapter.turns_after(file, Some(&index)).ok()?;
        if next.cursor.offset() < index.cursor.offset() {
            // 文件被改写：重新生成整份摘要。
            return None;
        }
        let activity = self.adapter.activity(&next.cursor)?;
        let next = Arc::new(next);
        self.indexes().put(next.clone(), Some(now));
        let mut summary = old.clone();
        if let Some(at) = activity.last_activity {
            summary.last_activity = at;
        }
        summary.open_turn = activity.open_turn;
        Some(summary)
    }

    /// 组装列表并发布（整体替换、发出 `SessionsChanged`）。没有变化时不发布，第一次总是发布。
    fn publish(&self, first: bool) {
        let mut published = self.published.lock().unwrap_or_else(|e| e.into_inner());
        let places = self.places.lock().unwrap_or_else(|e| e.into_inner());
        let mut state = self.state();

        // 每个会话选一个候选路径（「不变量」）。
        let mut best: HashMap<&SessionKey, (&Arc<Path>, &Known<A::File>)> = HashMap::new();
        for (path, known) in &state.known {
            let key = A::key(&known.file);
            let better = match best.get(key) {
                None => true,
                Some((p, k)) => {
                    (
                        A::rank(&known.file),
                        std::cmp::Reverse(known.id.mtime_ns),
                        path,
                    ) < (A::rank(&k.file), std::cmp::Reverse(k.id.mtime_ns), *p)
                }
            };
            if better {
                best.insert(key, (path, known));
            }
        }
        let mut chosen = HashMap::new();
        let mut sessions: Vec<SessionSummary> = Vec::new();
        let mut stats = ParseStats::default();
        for (key, (path, known)) in best {
            chosen.insert(key.clone(), path.clone());
            let Some((summary, s)) = &known.summary else {
                continue;
            };
            if self.setup.filter.is_hidden(key) {
                continue;
            }
            let mut summary = summary.clone();
            summary.place = places.get(&summary.cwd).cloned().unwrap_or(Place::Pending);
            // 压缩之后原文件被删：同一份内容，末尾字段沿用上一次发布的值。
            if summary.tail_pending
                && let Some(prev) = published.sessions.get(key)
                && !prev.tail_pending
            {
                summary.last_activity = prev.last_activity;
                summary.open_turn = prev.open_turn;
                summary.tail_pending = false;
            }
            stats.merge(s);
            sessions.push(summary);
        }
        drop(places);
        sessions.sort_by(|a, b| {
            b.last_activity
                .cmp(&a.last_activity)
                .then_with(|| a.key.cmp(&b.key))
        });
        let mut unreadable: Vec<Arc<Path>> = state.unreadable.values().flatten().cloned().collect();
        unreadable.sort();
        state.chosen = chosen;
        state.published_once = true;
        drop(state);

        let mut changed = Vec::new();
        let mut now = HashMap::with_capacity(sessions.len());
        for s in &sessions {
            if published.sessions.get(&s.key) != Some(s) {
                changed.push(s.key.clone());
            }
            now.insert(s.key.clone(), s.clone());
        }
        let mut removed: Vec<SessionKey> = published
            .sessions
            .keys()
            .filter(|k| !now.contains_key(*k))
            .cloned()
            .collect();
        changed.sort();
        removed.sort();
        let diagnostics = Diagnostics {
            unreadable_dirs: unreadable.into(),
            ..stats.diagnostics()
        };
        let old_diagnostics = self.sessions().diagnostics.clone();
        if !first && changed.is_empty() && removed.is_empty() && diagnostics == old_diagnostics {
            return;
        }
        published.generation += 1;
        published.sessions = now;
        let generation = published.generation;
        let list = Arc::new(SessionList {
            generation,
            sessions: sessions.into(),
            diagnostics,
        });
        *self.list.lock().unwrap_or_else(|e| e.into_inner()) = list;
        if let Some(handle) = self.host.get() {
            handle.emitter().emit(SessionsChanged {
                source: self.adapter.source().clone(),
                generation,
                changed: changed.into(),
                removed: removed.into(),
            });
        }
    }

    /// 第一次发布之后：主动补齐修改时间最近的几个压缩会话（「数据流与线程」第 2 步）。
    fn auto_complete(&self) {
        let list = self.sessions();
        let mut pending: Vec<&SessionSummary> =
            list.sessions.iter().filter(|s| s.tail_pending).collect();
        // `tail_pending` 的 `last_activity` 是文件修改时间。
        pending.sort_by_key(|s| std::cmp::Reverse(s.last_activity));
        let keys: Vec<SessionKey> = pending
            .into_iter()
            .take(AUTO_COMPLETE)
            .map(|s| s.key.clone())
            .collect();
        self.enqueue(&keys);
    }

    fn enqueue(&self, keys: &[SessionKey]) {
        let list = self.sessions();
        let mut state = self.state();
        if state.stopped {
            return;
        }
        for key in keys {
            if list.get(key).is_some_and(|s| s.tail_pending)
                && state.completion.queued.insert(key.clone())
            {
                state.completion.queue.push_back(key.clone());
            }
        }
        drop(state);
        self.pump_completion();
    }

    fn pump_completion(&self) {
        loop {
            let key = {
                let mut state = self.state();
                if state.stopped || state.completion.running >= MAX_COMPLETING {
                    return;
                }
                let Some(key) = state.completion.queue.pop_front() else {
                    return;
                };
                state.completion.running += 1;
                #[cfg(test)]
                self.max_completing.fetch_max(
                    state.completion.running,
                    std::sync::atomic::Ordering::Relaxed,
                );
                key
            };
            let spawned = self.spawn(move |engine| engine.run_completion(&key));
            if !spawned {
                self.state().completion.running -= 1;
                return;
            }
        }
    }

    fn run_completion(&self, key: &SessionKey) {
        self.complete_one(key);
        {
            let mut state = self.state();
            state.completion.running -= 1;
            state.completion.queued.remove(key);
        }
        self.pump_completion();
    }

    fn complete_one(&self, key: &SessionKey) {
        let Some((file, id)) = self.chosen_file(key) else {
            return;
        };
        let pending = self
            .state()
            .known
            .get(A::path(&file))
            .and_then(|k| k.summary.as_ref())
            .is_some_and(|(s, _)| s.tail_pending);
        if !pending || !A::compressed(&file) {
            return;
        }
        let Ok((summary, stats)) = self.adapter.complete(&file) else {
            return;
        };
        if let Some(cache) = &self.setup.cache {
            let _ = cache.put(
                &summary_key(key, &id, self.setup.adapter_version),
                &CachedSummary::new(&summary, &stats),
            );
        }
        {
            let mut state = self.state();
            match state.known.get_mut(A::path(&file)) {
                Some(known) if known.id == id => known.summary = Some((summary, stats)),
                _ => return,
            }
        }
        self.publish(false);
    }

    /// 最近一次发布时这个会话选中的文件和它当时的身份。被隐藏的会话为 None。
    fn chosen_file(&self, key: &SessionKey) -> Option<(A::File, FileId)> {
        if self.setup.filter.is_hidden(key) {
            return None;
        }
        let state = self.state();
        let path = state.chosen.get(key)?;
        let known = state.known.get(path)?;
        Some((known.file.clone(), known.id))
    }

    /// Codex 的 `Lineage`：按线程 ID 找来源会话的文件。
    pub(crate) fn file_by_id(&self, id: &str) -> Option<A::File> {
        let key = SessionKey {
            source: self.adapter.source().clone(),
            id: id.into(),
        };
        let state = self.state();
        let path = state.chosen.get(&key)?;
        Some(state.known.get(path)?.file.clone())
    }

    fn file(&self, key: &SessionKey) -> Result<(A::File, FileId), SessionError> {
        let (file, _) = self.chosen_file(key).ok_or(SessionError::NotFound)?;
        let id = FileId::stat(A::path(&file)).map_err(crate::read::io_error)?;
        Ok((file, id))
    }

    /// 读过之后：继承边界未定的摘要（Codex 分叉，来源的轮次集合要读取时才建立）重新生成一次。
    fn after_parse(&self, key: &SessionKey, file: &A::File, id: FileId) {
        let unknown = self
            .sessions()
            .get(key)
            .is_some_and(|s| s.inheritance_unknown);
        if !unknown {
            return;
        }
        {
            let mut state = self.state();
            if !state.resummarized.insert((key.clone(), id)) {
                return;
            }
            let guard = self.busy();
            state.work.forced.insert(A::path(file).to_path_buf());
            state.work.guards.extend(guard);
        }
        self.schedule();
    }

    /// 把新的索引放进内存；`persist` 时也写进 `cache`。写入在另一个后台任务里，不让 `turns` 的调用方等它；
    /// 没有 executor 时（`testing`）留到 drop。
    fn keep_index(&self, index: Arc<TurnIndex>, file: Option<FileId>, persist: bool) {
        if persist && self.setup.cache.is_some() {
            if self.setup.executor.is_some() {
                let index = index.clone();
                self.spawn(move |engine| engine.write_index(&index));
            } else {
                self.deferred
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(index.clone());
            }
        }
        self.indexes().put(index, file);
    }

    fn write_index(&self, index: &TurnIndex) {
        if let Some(cache) = &self.setup.cache {
            let key = index_key(&index.session, self.setup.adapter_version);
            let _ = cache.put(&key, &CachedIndex(index.clone()));
        }
    }

    fn cached_index(&self, key: &SessionKey) -> Option<TurnIndex> {
        let cache = self.setup.cache.as_ref()?;
        let CachedIndex(index) = cache.get(&index_key(key, self.setup.adapter_version))?;
        (index.session == *key).then_some(index)
    }

    #[cfg(test)]
    pub(crate) fn completion_running(&self) -> usize {
        self.state().completion.running
    }

    #[cfg(test)]
    pub(crate) fn memory_indexes(&self) -> MutexGuard<'_, Indexes> {
        self.indexes()
    }
}

impl<A: Adapter> Drop for Engine<A> {
    fn drop(&mut self) {
        let deferred =
            std::mem::take(&mut *self.deferred.lock().unwrap_or_else(|e| e.into_inner()));
        for index in deferred {
            self.write_index(&index);
        }
    }
}

impl<A: Adapter> SessionSource for Engine<A> {
    fn id(&self) -> SourceId {
        self.adapter.source().clone()
    }

    fn agent(&self) -> Agent {
        self.adapter.agent()
    }

    fn sessions(&self) -> Arc<SessionList> {
        self.list.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// 顺带更新轮次索引（「数据流与线程」第 7 步）：`from` 等于内存里索引的游标时在它之后追加；从头读
    /// （还没有索引）或回放（`reset`）时从头建。读到末尾时写进 `cache`。
    fn read(
        &self,
        session: &SessionKey,
        from: &Cursor,
        max_bytes: u64,
    ) -> Result<Batch, SessionError> {
        let (file, id) = self.file(session)?;
        let held = self.indexes().get(session);
        let (mut builder, follows) = match &held {
            Some((index, _)) if index.cursor == *from => (IndexBuilder::from_index(index), true),
            None if from.is_start() => (IndexBuilder::default(), true),
            _ => (IndexBuilder::default(), false),
        };
        let parsed = self.adapter.read(&file, from, max_bytes, &mut builder)?;
        let batch = parsed.batch;
        if follows || batch.reset {
            let index = Arc::new(builder.build(session.clone(), batch.cursor.clone()));
            let changed = held
                .as_ref()
                .is_none_or(|(old, _)| old.cursor != index.cursor);
            // 没读到末尾时文件身份不说明索引完整：下一次 `turns` 接着解析。
            let file_id = batch.at_end.then_some(id);
            self.keep_index(index, file_id, batch.at_end && changed);
        }
        self.after_parse(session, &file, id);
        Ok(batch)
    }

    /// 内存命中且文件没变时只复制 `Arc`；否则从内存或 `cache` 里的索引接着解析追加的部分，都没有时解析整个文件。
    fn turns(&self, session: &SessionKey) -> Result<Arc<TurnIndex>, SessionError> {
        let (file, id) = self.file(session)?;
        let held = self.indexes().get(session);
        if let Some((index, Some(built))) = &held
            && *built == id
        {
            return Ok(index.clone());
        }
        let prev: Option<Arc<TurnIndex>> = match held {
            Some((index, _)) => Some(index),
            None => self.cached_index(session).map(Arc::new),
        };
        let index = Arc::new(self.adapter.turns_after(&file, prev.as_deref())?);
        let changed = prev.as_ref().is_none_or(|p| p.cursor != index.cursor);
        self.keep_index(index.clone(), Some(id), changed);
        self.after_parse(session, &file, id);
        Ok(index)
    }

    fn load(&self, session: &SessionKey, at: &ContentRef) -> Result<Content, SessionError> {
        let (file, _) = self.chosen_file(session).ok_or(SessionError::NotFound)?;
        self.adapter.load(&file, at)
    }

    fn resume(&self, session: &SessionKey) -> Option<ResumeCommand> {
        if self.setup.filter.is_hidden(session) {
            return None;
        }
        let list = self.sessions();
        self.adapter.resume(list.get(session)?)
    }

    fn complete(&self, sessions: &[SessionKey]) {
        self.enqueue(sessions);
    }
}

/// 一个会话根目录的监听回调：只记下路径并提交刷新任务。
struct DirSink<A: Adapter> {
    engine: Weak<Engine<A>>,
    root: PathBuf,
}

impl<A: Adapter> Sink<DirBatch> for DirSink<A> {
    fn deliver(&self, batch: DirBatch, busy: Option<BusyGuard>) {
        if let Some(engine) = self.engine.upgrade() {
            engine.on_batch(&self.root, batch, busy);
        }
    }

    fn busy(&self) -> Option<BusyGuard> {
        self.engine.upgrade()?.busy()
    }
}
