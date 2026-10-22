//! Window snapshots from fact sums (docs/design/usage.md「取整与时间窗口」):
//! buckets go to local days, sums of millionths become whole tokens with the
//! largest remainder method in each kind separately, so every group of rows
//! adds up to its total.
//!
//! A watched window keeps its accumulators ([`Watched`]): a full compute
//! fills them from the sums of the facts, and each later ingest commit adds
//! the fact rows it inserted and takes away those it deleted
//! (docs/design/usage.md「数据流与线程」: 新的事实行到达时按增量更新). Both
//! go through the same accumulators, so an incremental snapshot is the one a
//! full compute would give.

use std::collections::{BTreeMap, HashMap};
use std::ops::Range;
use std::sync::Arc;

use lectern_services::project::ProjectKey;
use lectern_services::session::Agent;
use lectern_services::usage::{
    AllUsage, DayTotals, HistoryUsage, LocalDate, ProjectInfo, ProjectStatus, ProjectUsage,
    SnapshotHeader, Tokens, UsageSnapshot, UsageState, Window,
};

use crate::round::largest_remainder;
use crate::store::cold::{self, HistoryTotals};
use crate::store::under;
use crate::store::{FactDelta, FolderRoot, ProjectRow, Totals};
use crate::window::BUCKET_MS;

/// Agents in a fixed order: Claude Code, Codex, then the others by name.
fn agent_rank(agent: &Agent) -> (u8, &str) {
    match agent {
        Agent::ClaudeCode => (0, ""),
        Agent::Codex => (1, ""),
        Agent::Other(name) => (2, name),
    }
}

/// The order of agents, owned.
type AgentKey = (u8, String);

fn agent_key(agent: &Agent) -> AgentKey {
    let (rank, name) = agent_rank(agent);
    (rank, name.to_owned())
}

fn agent_text(texts: &HashMap<i64, Vec<u8>>, id: i64) -> Agent {
    crate::store::agent_of(texts.get(&id).map_or(&[][..], Vec::as_slice))
}

fn model_text(texts: &HashMap<i64, Vec<u8>>, id: i64) -> Arc<str> {
    String::from_utf8_lossy(texts.get(&id).map_or(&[][..], Vec::as_slice))
        .as_ref()
        .into()
}

/// The day a bucket falls in; None outside the window.
fn day_of(days: &[(LocalDate, Range<i64>)], bucket: i64) -> Option<usize> {
    let at = bucket * BUCKET_MS;
    let i = days.partition_point(|(_, r)| r.end <= at);
    days.get(i).filter(|(_, r)| r.contains(&at)).map(|_| i)
}

/// Rows of millionths rounded kind by kind.
fn round_rows(rows: &[[u64; 4]]) -> Vec<Tokens> {
    let kind = |k: usize| largest_remainder(&rows.iter().map(|r| r[k]).collect::<Vec<_>>());
    let (input, output, read, write) = (kind(0), kind(1), kind(2), kind(3));
    (0..rows.len())
        .map(|i| Tokens {
            input: input[i],
            output: output[i],
            cache_read: read[i],
            cache_write: write[i],
        })
        .collect()
}

/// Adds signed millionths.
fn add(into: &mut [u64; 4], micro: &[i64; 4]) {
    for (a, b) in into.iter_mut().zip(micro) {
        *a = a.saturating_add_signed(*b);
    }
}

fn day_total(micro: &[i64; 4]) -> i64 {
    micro.iter().fold(0i64, |t, m| t.saturating_add(*m))
}

fn total_of(rows: &[Tokens]) -> Tokens {
    rows.iter().fold(Tokens::default(), |t, r| Tokens {
        input: t.input + r.input,
        output: t.output + r.output,
        cache_read: t.cache_read + r.cache_read,
        cache_write: t.cache_write + r.cache_write,
    })
}

/// Per day and agent, each day rounded on its own.
fn daily(per_day: &[HashMap<AgentKey, u64>], agents: &[(&AgentKey, &Agent)]) -> Arc<[DayTotals]> {
    per_day
        .iter()
        .map(|day| {
            let parts: Vec<u64> = agents
                .iter()
                .map(|(k, _)| day.get(*k).copied().unwrap_or(0))
                .collect();
            let rounded = largest_remainder(&parts);
            agents
                .iter()
                .map(|(_, a)| (*a).clone())
                .zip(rounded)
                .collect::<Vec<_>>()
                .into()
        })
        .collect::<Vec<_>>()
        .into()
}

/// `ProjectInfo` of a project row.
pub(crate) fn info(row: &ProjectRow, open: &[ProjectKey]) -> ProjectInfo {
    let key = row.key();
    let name = row.name.clone().unwrap_or_else(|| {
        row.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    });
    ProjectInfo {
        id: row.pid,
        name: name.into(),
        folder: row.folder,
        key: open.contains(&key).then_some(key),
        status: ProjectStatus::Active,
    }
}

/// An agent present in the window: its fact rows and millionths.
#[derive(Clone, Debug)]
struct AgentAcc {
    agent: Agent,
    rows: i64,
    micro: [u64; 4],
}

/// A model present in the window.
#[derive(Clone, Debug)]
struct ModelAcc {
    agent: Agent,
    model: Arc<str>,
    rows: i64,
    micro: [u64; 4],
}

/// Adds rows to an agent's entry; drops it when it has none left (a full
/// compute would not see it).
fn add_agent(
    agents: &mut BTreeMap<AgentKey, AgentAcc>,
    agent: &Agent,
    micro: &[i64; 4],
    rows: i64,
) {
    let key = agent_key(agent);
    let entry = agents.entry(key.clone()).or_insert_with(|| AgentAcc {
        agent: agent.clone(),
        rows: 0,
        micro: [0; 4],
    });
    entry.rows += rows;
    add(&mut entry.micro, micro);
    if entry.rows <= 0 {
        agents.remove(&key);
    }
}

fn add_day(per_day: &mut [HashMap<AgentKey, u64>], d: usize, agent: &Agent, micro: &[i64; 4]) {
    let cell = per_day[d].entry(agent_key(agent)).or_insert(0);
    *cell = cell.saturating_add_signed(day_total(micro));
}

/// A project's window: totals, by model, by agent and per day. `rows` stays
/// empty in step 1 (every turn is attributed by its cwd, no branches yet).
#[derive(Clone, Debug)]
pub(crate) struct ProjectAcc {
    project: ProjectInfo,
    window: Window,
    days: Vec<(LocalDate, Range<i64>)>,
    /// The locations whose facts count here.
    locations: Vec<i64>,
    agents: BTreeMap<AgentKey, AgentAcc>,
    models: BTreeMap<(AgentKey, String), ModelAcc>,
    per_day: Vec<HashMap<AgentKey, u64>>,
}

impl ProjectAcc {
    fn new(
        project: ProjectInfo,
        window: Window,
        days: &[(LocalDate, Range<i64>)],
        locations: Vec<i64>,
    ) -> ProjectAcc {
        ProjectAcc {
            project,
            window,
            days: days.to_vec(),
            locations,
            agents: BTreeMap::new(),
            models: BTreeMap::new(),
            per_day: vec![HashMap::new(); days.len()],
        }
    }

    fn add(&mut self, bucket: i64, agent: &Agent, model: &Arc<str>, micro: &[i64; 4], rows: i64) {
        let Some(d) = day_of(&self.days, bucket) else {
            return;
        };
        add_agent(&mut self.agents, agent, micro, rows);
        let key = (agent_key(agent), model.to_string());
        let entry = self.models.entry(key.clone()).or_insert_with(|| ModelAcc {
            agent: agent.clone(),
            model: model.clone(),
            rows: 0,
            micro: [0; 4],
        });
        entry.rows += rows;
        add(&mut entry.micro, micro);
        if entry.rows <= 0 {
            self.models.remove(&key);
        }
        add_day(&mut self.per_day, d, agent, micro);
    }

    fn snapshot(&self, header: SnapshotHeader) -> ProjectUsage {
        let agents: Vec<(&AgentKey, &Agent)> =
            self.agents.iter().map(|(k, a)| (k, &a.agent)).collect();
        let model_rows: Vec<&ModelAcc> = self.models.values().collect();
        let model_tokens = round_rows(&model_rows.iter().map(|r| r.micro).collect::<Vec<_>>());
        let agent_tokens = round_rows(&self.agents.values().map(|a| a.micro).collect::<Vec<_>>());
        let total = total_of(&agent_tokens);
        ProjectUsage {
            header,
            project: self.project.clone(),
            window: self.window,
            days: self.days.iter().map(|(d, _)| *d).collect::<Vec<_>>().into(),
            total,
            rows: Arc::from(Vec::new()),
            by_model: model_rows
                .into_iter()
                .zip(model_tokens)
                .map(|(m, t)| (m.agent.clone(), m.model.clone(), t))
                .collect::<Vec<_>>()
                .into(),
            by_agent: self
                .agents
                .values()
                .map(|a| a.agent.clone())
                .zip(agent_tokens)
                .collect::<Vec<_>>()
                .into(),
            daily: daily(&self.per_day, &agents),
        }
    }
}

/// A project's window from the sums of its facts.
pub(crate) fn project(
    header: SnapshotHeader,
    project: ProjectInfo,
    window: Window,
    days: &[(LocalDate, Range<i64>)],
    totals: &Totals,
) -> ProjectUsage {
    let mut acc = ProjectAcc::new(project, window, days, Vec::new());
    fill_project(&mut acc, totals);
    acc.snapshot(header)
}

fn fill_project(acc: &mut ProjectAcc, totals: &Totals) {
    for s in &totals.sums {
        let agent = agent_text(&totals.texts, s.agent);
        let model = model_text(&totals.texts, s.model);
        acc.add(s.bucket, &agent, &model, &signed(&s.micro), s.rows as i64);
    }
}

fn signed(m: &[u64; 4]) -> [i64; 4] {
    m.map(|v| i64::try_from(v).unwrap_or(i64::MAX))
}

/// The history of a project (`UsageQuery::History`): one row per local
/// month of the range, each month and each day rounded as a group of rows
/// adding up to the range's total.
pub(crate) fn history(
    header: SnapshotHeader,
    project: ProjectInfo,
    from: LocalDate,
    totals: &HistoryTotals,
) -> HistoryUsage {
    let month_rows: Vec<[u64; 4]> = totals.months.values().copied().collect();
    let months = totals
        .months
        .keys()
        .map(|m| cold::year_month(*m))
        .zip(round_rows(&month_rows))
        .collect::<Vec<_>>()
        .into();
    let days = totals.days.as_ref().map(|days| {
        let first = crate::window::day_number(from);
        days.iter()
            .enumerate()
            .map(|(i, _)| crate::window::date_of(first + i as i64))
            .zip(round_rows(days))
            .collect::<Vec<_>>()
            .into()
    });
    HistoryUsage {
        header,
        project,
        months,
        days,
    }
}

/// Who a location's usage belongs to in the 「所有项目」 list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Owner {
    /// Index into the listed projects.
    Project(usize),
    /// 「其他」: never-opened repositories and places in no project.
    Other,
}

/// The owner of each location: a repository's own project when it was
/// opened; for a directory, the deepest opened folder project whose root
/// contains it (docs/design/usage.md「项目标识」).
#[derive(Clone, Debug)]
pub(crate) struct Owners {
    pub(crate) listed: Vec<ProjectRow>,
    by_location: HashMap<i64, Owner>,
    /// The largest location ID these owners know.
    pub(crate) max_location: i64,
}

impl Owners {
    pub(crate) fn new(projects: &[ProjectRow], dirs: &[(i64, Vec<u8>)]) -> Owners {
        let listed: Vec<ProjectRow> = projects.iter().filter(|p| p.opened).cloned().collect();
        let mut by_location = HashMap::new();
        let roots: Vec<FolderRoot> = listed
            .iter()
            .filter(|p| p.folder)
            .map(|p| FolderRoot {
                project: p.id,
                path: crate::store::path_bytes(&p.path).to_vec(),
            })
            .collect();
        for (i, p) in listed.iter().enumerate() {
            if !p.folder {
                by_location.insert(p.location, Owner::Project(i));
            }
        }
        for (id, path) in dirs {
            let claim = roots
                .iter()
                .filter(|r| under(path, &r.path))
                .max_by_key(|r| r.path.len());
            if let Some(root) = claim
                && let Some(i) = listed.iter().position(|p| p.id == root.project)
            {
                by_location.insert(*id, Owner::Project(i));
            }
        }
        let max_location = projects
            .iter()
            .map(|p| p.location)
            .chain(dirs.iter().map(|(id, _)| *id))
            .max()
            .unwrap_or(0);
        Owners {
            listed,
            by_location,
            max_location,
        }
    }

    pub(crate) fn owner(&self, location: i64) -> Owner {
        self.by_location
            .get(&location)
            .copied()
            .unwrap_or(Owner::Other)
    }

    /// The locations a listed project owns.
    pub(crate) fn locations_of(&self, project: i64) -> Vec<i64> {
        let Some(i) = self.listed.iter().position(|p| p.id == project) else {
            return Vec::new();
        };
        let mut out: Vec<i64> = self
            .by_location
            .iter()
            .filter(|(_, o)| **o == Owner::Project(i))
            .map(|(l, _)| *l)
            .collect();
        out.sort_unstable();
        out
    }
}

/// The 「所有项目」 window: one row per opened project and 「其他」, adding
/// up to the total kind by kind.
#[derive(Clone, Debug)]
pub(crate) struct AllAcc {
    window: Window,
    days: Vec<(LocalDate, Range<i64>)>,
    owners: Owners,
    open: Vec<ProjectKey>,
    /// One per listed project, then 「其他」.
    rows: Vec<[u64; 4]>,
    agents: BTreeMap<AgentKey, AgentAcc>,
    per_day: Vec<HashMap<AgentKey, u64>>,
}

impl AllAcc {
    fn new(
        window: Window,
        days: &[(LocalDate, Range<i64>)],
        owners: Owners,
        open: &[ProjectKey],
    ) -> AllAcc {
        let n = owners.listed.len();
        AllAcc {
            window,
            days: days.to_vec(),
            owners,
            open: open.to_vec(),
            rows: vec![[0; 4]; n + 1],
            agents: BTreeMap::new(),
            per_day: vec![HashMap::new(); days.len()],
        }
    }

    fn add(&mut self, location: i64, bucket: i64, agent: &Agent, micro: &[i64; 4], rows: i64) {
        let Some(d) = day_of(&self.days, bucket) else {
            return;
        };
        let n = self.owners.listed.len();
        let row = match self.owners.owner(location) {
            Owner::Project(i) => i,
            Owner::Other => n,
        };
        add(&mut self.rows[row], micro);
        add_agent(&mut self.agents, agent, micro, rows);
        add_day(&mut self.per_day, d, agent, micro);
    }

    fn snapshot(&self, header: SnapshotHeader) -> AllUsage {
        let agents: Vec<(&AgentKey, &Agent)> =
            self.agents.iter().map(|(k, a)| (k, &a.agent)).collect();
        let n = self.owners.listed.len();
        let rounded = round_rows(&self.rows);
        let total = total_of(&rounded);
        let mut projects: Vec<(ProjectInfo, Tokens)> = self
            .owners
            .listed
            .iter()
            .zip(&rounded)
            .map(|(p, t)| (info(p, &self.open), *t))
            .collect();
        projects.sort_by(|a, b| {
            b.1.total()
                .cmp(&a.1.total())
                .then_with(|| a.0.name.cmp(&b.0.name))
                .then_with(|| a.0.id.cmp(&b.0.id))
        });
        AllUsage {
            header,
            window: self.window,
            days: self.days.iter().map(|(d, _)| *d).collect::<Vec<_>>().into(),
            total,
            projects: projects.into(),
            other: rounded[n],
            daily: daily(&self.per_day, &agents),
        }
    }
}

/// The 「所有项目」 window from the sums of every fact.
pub(crate) fn all(
    header: SnapshotHeader,
    window: Window,
    days: &[(LocalDate, Range<i64>)],
    totals: &Totals,
    owners: &Owners,
    open: &[ProjectKey],
) -> AllUsage {
    let mut acc = AllAcc::new(window, days, owners.clone(), open);
    fill_all(&mut acc, totals);
    acc.snapshot(header)
}

fn fill_all(acc: &mut AllAcc, totals: &Totals) {
    for s in &totals.sums {
        let agent = agent_text(&totals.texts, s.agent);
        acc.add(
            s.location,
            s.bucket,
            &agent,
            &signed(&s.micro),
            s.rows as i64,
        );
    }
}

#[derive(Clone, Debug)]
enum Acc {
    Project(ProjectAcc),
    All(AllAcc),
}

/// A watched window: its accumulators and what applying later commits'
/// fact deltas needs.
#[derive(Clone, Debug)]
pub(crate) struct Watched {
    acc: Acc,
    texts: HashMap<i64, Vec<u8>>,
    /// The ingest commits the accumulators include.
    commits: i64,
    /// A delta at a location above this needs a full compute (it may be
    /// a new directory under an open folder project).
    max_location: i64,
    may_include_inherited: bool,
}

/// What applying deltas did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Applied {
    /// The commit is already included, or touches nothing of this window.
    Unchanged,
    Changed,
    /// The deltas cannot be applied: compute the snapshot again.
    Recompute,
}

impl Watched {
    /// A project's window from its full sums. `locations` are the ones its
    /// facts count from; `max_location` the largest ID the computation knew
    /// (`i64::MAX` when new locations cannot matter).
    pub(crate) fn project(
        project: ProjectInfo,
        window: Window,
        days: &[(LocalDate, Range<i64>)],
        locations: Vec<i64>,
        max_location: i64,
        totals: &Totals,
    ) -> Watched {
        let mut acc = ProjectAcc::new(project, window, days, locations);
        fill_project(&mut acc, totals);
        Watched {
            acc: Acc::Project(acc),
            texts: totals.texts.clone(),
            commits: totals.commits,
            max_location,
            may_include_inherited: totals.may_include_inherited,
        }
    }

    /// The 「所有项目」 window from its full sums.
    pub(crate) fn all(
        window: Window,
        days: &[(LocalDate, Range<i64>)],
        owners: Owners,
        open: &[ProjectKey],
        totals: &Totals,
    ) -> Watched {
        let max_location = owners.max_location;
        let mut acc = AllAcc::new(window, days, owners, open);
        fill_all(&mut acc, totals);
        Watched {
            acc: Acc::All(acc),
            texts: totals.texts.clone(),
            commits: totals.commits,
            max_location,
            may_include_inherited: totals.may_include_inherited,
        }
    }

    /// The agent and model IDs of `deltas` whose text is not known yet.
    pub(crate) fn missing(&self, deltas: &[FactDelta]) -> Vec<i64> {
        let mut out: Vec<i64> = deltas
            .iter()
            .flat_map(|d| [d.agent, d.model])
            .filter(|id| *id != 0 && !self.texts.contains_key(id))
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    pub(crate) fn add_texts(&mut self, texts: &HashMap<i64, Vec<u8>>) {
        for (id, t) in texts {
            self.texts.entry(*id).or_insert_with(|| t.clone());
        }
    }

    /// Applies the fact deltas of ingest commit `commit` (their texts known,
    /// see [`Watched::missing`]). Only the next commit applies: one already
    /// in the sums changes nothing, and one after a gap (an earlier
    /// commit's deltas not applied yet) needs a full compute.
    pub(crate) fn apply(
        &mut self,
        commit: i64,
        deltas: &[FactDelta],
        inheritance_unknown: bool,
    ) -> Applied {
        if commit <= self.commits {
            return Applied::Unchanged;
        }
        if commit != self.commits + 1 || deltas.iter().any(|d| d.location > self.max_location) {
            return Applied::Recompute;
        }
        self.commits = commit;
        let mut changed = false;
        match &mut self.acc {
            Acc::Project(acc) => {
                let locations = std::mem::take(&mut acc.locations);
                for d in deltas.iter().filter(|d| locations.contains(&d.location)) {
                    let agent = agent_text(&self.texts, d.agent);
                    let model = model_text(&self.texts, d.model);
                    acc.add(d.bucket, &agent, &model, &d.micro, d.rows);
                    changed = true;
                }
                acc.locations = locations;
            }
            Acc::All(acc) => {
                for d in deltas {
                    let agent = agent_text(&self.texts, d.agent);
                    acc.add(d.location, d.bucket, &agent, &d.micro, d.rows);
                    changed = true;
                }
            }
        }
        if changed && inheritance_unknown {
            // Kept until the next full compute, which knows exactly.
            self.may_include_inherited = true;
        }
        if changed {
            Applied::Changed
        } else {
            Applied::Unchanged
        }
    }

    /// The snapshot, with `state` in its header.
    pub(crate) fn snapshot(&self, state: UsageState) -> UsageSnapshot {
        let header = SnapshotHeader {
            generation: 0,
            state,
            may_include_inherited: self.may_include_inherited,
        };
        match &self.acc {
            Acc::Project(acc) => UsageSnapshot::Project(acc.snapshot(header)),
            Acc::All(acc) => UsageSnapshot::All(acc.snapshot(header)),
        }
    }
}
