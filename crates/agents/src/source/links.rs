//! Codex 子 agent 与派出它的调用（docs/design/agents.md「Codex 子 agent 与派出它的调用」，验收第 5 步）。
//!
//! 子 rollout 里只有父线程 ID，子线程 ID 在父会话 `spawn_agent` 调用的输出里，所以关联从父会话一侧派生：
//! 父会话的轮次索引放进内存之后，读出索引里每个已结束的 `spawn_agent` 调用的输出（每个调用一次 `load`），
//! 得到「调用 → 子线程」，存进 `Engine::links` 和 `agents.spawns`；发布列表时按当时的关联填写子会话的
//! `Parent::tool_call`。发现时只为列出的子会话的父会话各查一次 `agents.spawns`，不读父会话的正文。

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::Ordering;
use std::sync::{Arc, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use lectern_services::session::{
    SessionKey, SessionSource, SessionSummary, ToolCall, ToolCallId, ToolKind, TurnIndex, TurnItem,
};

use super::cached::{CachedSpawns, spawns_key};
use super::{Adapter, Engine};

/// 活跃会话：`last_activity` 在这么多毫秒之内（与「调用方」里的定义相同）。
const ACTIVE_MS: i64 = 10 * 60 * 1000;
/// 同一父会话的主动建索引，这么多毫秒之内最多排一次。
const BUILD_EVERY_MS: i64 = 10 * 60 * 1000;

/// 按文件顺序的（`SpawnAgent` 调用，它派出的子线程 ID）。
pub(crate) type Spawns = Arc<[(ToolCallId, Option<Arc<str>>)]>;

/// 一个父会话的关联，派生自它的某一份轮次索引。
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Links {
    /// 派生时用的索引：`keep_index` 放进内存时给的序号（从 `agents.spawns` 读出的为 0）。
    pub(crate) seq: u64,
    /// 派生时索引游标的偏移（解压后的字节）。
    pub(crate) offset: u64,
    /// 索引里每个已结束的 `SpawnAgent` 调用一项，按文件顺序；输出里没有子线程 ID 的记为 None，下次不再读它。
    pub(crate) spawns: Spawns,
}

/// 活跃父会话的主动建索引：同时最多 1 个，同一父会话 10 分钟之内最多排一次。
#[derive(Default)]
pub(crate) struct Builds {
    running: bool,
    queue: VecDeque<SessionKey>,
    /// 每个父会话最近一次排队的时间（毫秒）。
    queued_at: HashMap<SessionKey, i64>,
    /// 测试用：已提交、还没结束的建索引任务数，和它的最大值。
    #[cfg(test)]
    in_flight: usize,
    #[cfg(test)]
    max_in_flight: usize,
}

/// 适配器读会话正文的调用（`read`、`turns_after`、`load`；`spawned_thread` 记为 `load`）。
#[derive(Clone, Copy)]
pub(crate) enum Call {
    Read,
    TurnsAfter,
    Load,
}

/// 适配器读会话正文的调用次数（测试与 bench 检查发现时没有读父会话）。
#[cfg(any(test, feature = "test-support"))]
#[derive(Default, Debug)]
pub(crate) struct Calls {
    pub(crate) read: std::sync::atomic::AtomicUsize,
    pub(crate) turns_after: std::sync::atomic::AtomicUsize,
    pub(crate) load: std::sync::atomic::AtomicUsize,
}

/// 索引里已结束、有结果引用的 `SpawnAgent` 调用，按文件顺序（继承前缀不进索引，所以只有父会话自己的）。
fn spawn_calls(index: &TurnIndex) -> impl Iterator<Item = &ToolCall> {
    index
        .turns
        .iter()
        .flat_map(|t| t.items.iter())
        .filter_map(|item| match item {
            TurnItem::Tool(call) if call.kind == ToolKind::SpawnAgent && call.result.is_some() => {
                Some(call)
            }
            _ => None,
        })
}

impl<A: Adapter> Engine<A> {
    pub(super) fn links(&self) -> MutexGuard<'_, HashMap<SessionKey, Option<Links>>> {
        self.links.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn builds(&self) -> MutexGuard<'_, Builds> {
        self.builds.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(super) fn next_index_seq(&self) -> u64 {
        self.index_seq.fetch_add(1, Ordering::Relaxed) + 1
    }

    pub(super) fn count(&self, call: Call) {
        #[cfg(any(test, feature = "test-support"))]
        {
            let counter = match call {
                Call::Read => &self.calls.read,
                Call::TurnsAfter => &self.calls.turns_after,
                Call::Load => &self.calls.load,
            };
            counter.fetch_add(1, Ordering::Relaxed);
        }
        #[cfg(not(any(test, feature = "test-support")))]
        let _ = call;
    }

    /// 当前时间（UTC 毫秒）。
    fn now_ms(&self) -> i64 {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
        #[cfg(test)]
        let now = now + self.clock_skew.load(Ordering::Relaxed);
        now
    }

    /// `read`、`turns` 放进内存的索引：在另一个 `Background` 任务里派生，调用方不等它（没有 executor 时同步）。
    /// 关联已经派生自同一位置的续接索引时什么也不做。
    pub(super) fn derive_later(
        &self,
        file: &A::File,
        index: Arc<TurnIndex>,
        seq: u64,
        continued: bool,
    ) {
        if !A::LINKS_SPAWNS {
            return;
        }
        if continued
            && let Some(Some(links)) = self.links().get(&index.session)
            && links.offset == index.cursor.offset()
        {
            return;
        }
        let file = file.clone();
        self.spawn(move |engine| {
            if engine.derive_links(&file, &index, seq, continued) {
                engine.publish_for(&index.session);
            }
        });
    }

    /// 从 `index` 派生父会话的关联（「派生的时机」）。续接的索引沿用已有的项（按调用 ID），只为新的调用
    /// 各 `load` 一次；从头建的索引全部重新读。`seq` 不大于已有关联的丢弃。关联变了时写进 `agents.spawns`，
    /// 返回 true。读输出失败时不改关联（下一份索引再试）。
    pub(super) fn derive_links(
        &self,
        file: &A::File,
        index: &TurnIndex,
        seq: u64,
        continued: bool,
    ) -> bool {
        if !A::LINKS_SPAWNS {
            return false;
        }
        let key = &index.session;
        // 这次运行还没查过 `agents.spawns`：先查，才知道缓存里的条目要不要改写。
        if !self.links().contains_key(key) {
            let found = self.cached_links(key, None);
            self.links().entry(key.clone()).or_insert(found);
        }
        let old: Option<Spawns> = self
            .links()
            .get(key)
            .and_then(Option::as_ref)
            .map(|l| l.spawns.clone());
        let reuse: HashMap<&ToolCallId, &Option<Arc<str>>> = match (&old, continued) {
            (Some(old), true) => old.iter().map(|(id, child)| (id, child)).collect(),
            _ => HashMap::new(),
        };
        let mut spawns = Vec::new();
        for call in spawn_calls(index) {
            let child = match reuse.get(&call.id) {
                Some(child) => (*child).clone(),
                None => {
                    self.count(Call::Load);
                    match self.adapter.spawned_thread(file, call) {
                        Ok(child) => child,
                        Err(_) => return false,
                    }
                }
            };
            spawns.push((call.id.clone(), child));
        }
        let spawns: Spawns = spawns.into();
        let offset = index.cursor.offset();
        let changed = {
            let mut links = self.links();
            let entry = links.entry(key.clone()).or_insert(None);
            if entry.as_ref().is_some_and(|l| l.seq >= seq) {
                // 先建的索引晚完成派生。
                return false;
            }
            let changed = match &entry {
                Some(l) => *l.spawns != *spawns,
                None => !spawns.is_empty(),
            };
            *entry = Some(Links {
                seq,
                offset,
                spawns: spawns.clone(),
            });
            changed
        };
        if changed && let Some(cache) = &self.setup.cache {
            let _ = cache.put(
                &spawns_key(key, self.setup.adapter_version),
                &CachedSpawns {
                    offset,
                    spawns: spawns
                        .iter()
                        .map(|(id, child)| (id.0.clone(), child.clone()))
                        .collect(),
                },
            );
        }
        changed
    }

    /// `agents.spawns` 里这个父会话的条目。`len` 是父会话文件的当前长度：条目的 `offset` 比它大时丢弃
    /// （`.jsonl.zst` 传 None，不判断）。
    fn cached_links(&self, parent: &SessionKey, len: Option<u64>) -> Option<Links> {
        let cache = self.setup.cache.as_ref()?;
        let hit: CachedSpawns = cache.get(&spawns_key(parent, self.setup.adapter_version))?;
        if len.is_some_and(|len| hit.offset > len) {
            return None;
        }
        Some(Links {
            seq: 0,
            offset: hit.offset,
            spawns: hit
                .spawns
                .into_iter()
                .map(|(id, child)| (ToolCallId(id), child))
                .collect(),
        })
    }

    /// 刷新任务在发布之前：列出的 Codex 子会话指向的父会话（在列表里、`links` 里还没有它）各查一次
    /// `agents.spawns`，没有条目也记下（「发现时的代价」）。只读几百字节的缓存条目，不读父会话。
    pub(super) fn lookup_spawns(&self) {
        if !A::LINKS_SPAWNS {
            return;
        }
        let wanted: Vec<(SessionKey, Option<u64>)> = {
            let state = self.state();
            let best = Self::best(&state.known);
            let links = self.links();
            let listed = |key: &SessionKey| {
                best.get(key).is_some_and(|(_, k)| k.summary.is_some())
                    && !self.setup.filter.is_hidden(key)
            };
            let mut seen = HashSet::new();
            let mut wanted = Vec::new();
            for (key, (_, known)) in &best {
                let Some((summary, _)) = &known.summary else {
                    continue;
                };
                let Some(parent) = &summary.parent else {
                    continue;
                };
                let parent = &parent.session;
                if parent.id.is_empty()
                    || links.contains_key(parent)
                    || !listed(key)
                    || !listed(parent)
                    || !seen.insert(parent.clone())
                {
                    continue;
                }
                let Some((_, file)) = best.get(parent) else {
                    continue;
                };
                // 未压缩的文件按这一轮已有的 `stat` 判断条目是否越过末尾。
                let len = (!A::compressed(&file.file)).then_some(file.id.len);
                wanted.push((parent.clone(), len));
            }
            wanted
        };
        for (parent, len) in wanted {
            let found = self.cached_links(&parent, len);
            self.links().entry(parent).or_insert(found);
        }
    }

    /// 发布时按当时的关联填写 Codex 子会话的 `Parent::tool_call`（「摘要的更新」）：父会话在这次发布的列表里，
    /// 取它的关联里第一个给出这个子线程的调用。
    pub(super) fn fill_tool_calls(&self, sessions: &mut [SessionSummary]) {
        if !A::LINKS_SPAWNS {
            return;
        }
        let listed: HashSet<SessionKey> = sessions.iter().map(|s| s.key.clone()).collect();
        let links = self.links();
        for s in sessions.iter_mut() {
            let Some(parent) = &mut s.parent else {
                continue;
            };
            if !listed.contains(&parent.session) {
                continue;
            }
            let Some(Some(l)) = links.get(&parent.session) else {
                continue;
            };
            parent.tool_call = l
                .spawns
                .iter()
                .find(|(_, child)| child.as_deref() == Some(&*s.key.id))
                .map(|(call, _)| call.clone());
        }
    }

    /// 派生任务之后：有列出的子会话指向这个父会话时发布一次（「派生的时机」）。
    fn publish_for(&self, parent: &SessionKey) {
        let list = self.sessions();
        let affected = list
            .sessions
            .iter()
            .any(|s| s.parent.as_ref().is_some_and(|p| p.session == *parent));
        if affected {
            self.publish(false);
        }
    }

    /// 刷新任务在发布之后：活跃的父会话（`open_turn`，或 `last_activity` 在 10 分钟之内）有列出的子会话还没有
    /// 关联、它的索引又不在内存里时，排一次后台建索引（「摘要的更新」）。
    pub(super) fn schedule_builds(&self) {
        if !A::LINKS_SPAWNS {
            return;
        }
        let list = self.sessions();
        let now = self.now_ms();
        let by_key: HashMap<&SessionKey, &SessionSummary> =
            list.sessions.iter().map(|s| (&s.key, s)).collect();
        let mut wanted: Vec<SessionKey> = Vec::new();
        for s in list.sessions.iter() {
            let Some(parent) = &s.parent else {
                continue;
            };
            if parent.tool_call.is_some() || wanted.contains(&parent.session) {
                continue;
            }
            let Some(p) = by_key.get(&parent.session) else {
                continue;
            };
            if !(p.open_turn || now - p.last_activity.0 < ACTIVE_MS) {
                continue;
            }
            if self.indexes().contains(&parent.session) {
                continue;
            }
            wanted.push(parent.session.clone());
        }
        if wanted.is_empty() {
            return;
        }
        {
            let mut builds = self.builds();
            for key in wanted {
                let recent = builds
                    .queued_at
                    .get(&key)
                    .is_some_and(|at| now - at < BUILD_EVERY_MS);
                if recent || builds.queue.contains(&key) {
                    continue;
                }
                builds.queued_at.insert(key.clone(), now);
                builds.queue.push_back(key);
            }
        }
        self.pump_builds();
    }

    fn pump_builds(&self) {
        if self.state().stopped {
            return;
        }
        let key = {
            let mut builds = self.builds();
            if builds.running {
                return;
            }
            let Some(key) = builds.queue.pop_front() else {
                return;
            };
            builds.running = true;
            #[cfg(test)]
            {
                builds.in_flight += 1;
                builds.max_in_flight = builds.max_in_flight.max(builds.in_flight);
            }
            key
        };
        if !self.spawn(move |engine| engine.run_build(&key)) {
            let mut builds = self.builds();
            builds.running = false;
            #[cfg(test)]
            {
                builds.in_flight -= 1;
            }
        }
    }

    /// 与 `turns` 相同：从内存或 `cache` 里的索引接着解析，没有时解析整个文件；派生随之进行。
    fn run_build(&self, key: &SessionKey) {
        let _ = self.turns(key);
        {
            let mut builds = self.builds();
            builds.running = false;
            #[cfg(test)]
            {
                builds.in_flight -= 1;
            }
        }
        self.pump_builds();
    }

    #[cfg(test)]
    pub(crate) fn max_builds_in_flight(&self) -> usize {
        self.builds().max_in_flight
    }

    #[cfg(test)]
    pub(crate) fn links_of(&self, parent: &SessionKey) -> Option<Option<Links>> {
        self.links().get(parent).cloned()
    }
}
