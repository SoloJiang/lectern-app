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

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ops::Range;
use std::path::Path;
use std::sync::Arc;

use lectern_services::project::ProjectKey;
use lectern_services::session::Agent;
use lectern_services::usage::{
    AliasId, AllUsage, DayTotals, HistoryUsage, LocalDate, ProjectInfo, ProjectStatus,
    ProjectUsage, SnapshotHeader, Tokens, UsageSnapshot, UsageState, Window,
};

use crate::round::largest_remainder;
use crate::store::cold::{self, HistoryTotals};
use crate::store::under;
use crate::store::{Aliases, FactDelta, FolderRoot, ProjectRow, Totals, temporary_of};
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

/// The name a project row shows: the stored one, else the directory name.
fn display_name(row: &ProjectRow) -> String {
    match row.name.as_deref() {
        Some(name) if !name.is_empty() => name.to_owned(),
        _ => row
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
    }
}

/// `ProjectInfo` of a project row that is not listed (never opened, or
/// asked for by ID): nothing joins it to another project.
pub(crate) fn info(row: &ProjectRow, open: &[ProjectKey]) -> ProjectInfo {
    let key = row.key();
    ProjectInfo {
        id: row.pid,
        name: display_name(row).into(),
        folder: row.folder,
        key: open.contains(&key).then_some(key),
        status: ProjectStatus::Active,
        confirmed: None,
    }
}

/// Whose facts a project window counts: the facts of the listed project's
/// member projects, and of the directories under a folder project's root.
#[derive(Clone, Debug, Default)]
pub(crate) struct Scope {
    /// The `project.id`s that are this project.
    members: HashSet<i64>,
    /// The directory locations a folder project claims.
    claimed: HashSet<i64>,
    /// Every location a member has or claimed: where its facts are.
    pub(crate) locations: Vec<i64>,
    /// A delta at a location or project above these is new to the
    /// computation (a directory that may be claimed, a project made since):
    /// the snapshot is computed again.
    pub(crate) max_location: i64,
    pub(crate) max_project: i64,
}

impl Scope {
    /// A project that no other joins and no folder claims around: only its
    /// own facts, and nothing new can change that.
    pub(crate) fn single(row: &ProjectRow) -> Scope {
        Scope {
            members: HashSet::from([row.id]),
            claimed: HashSet::new(),
            locations: vec![row.location],
            max_location: i64::MAX,
            max_project: i64::MAX,
        }
    }

    fn accepts(&self, location: i64, project: i64) -> bool {
        if project != 0 {
            self.members.contains(&project)
        } else {
            self.claimed.contains(&location)
        }
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
    /// Whose facts count here.
    scope: Scope,
    agents: BTreeMap<AgentKey, AgentAcc>,
    models: BTreeMap<(AgentKey, String), ModelAcc>,
    per_day: Vec<HashMap<AgentKey, u64>>,
}

impl ProjectAcc {
    fn new(
        project: ProjectInfo,
        window: Window,
        days: &[(LocalDate, Range<i64>)],
        scope: Scope,
    ) -> ProjectAcc {
        ProjectAcc {
            project,
            window,
            days: days.to_vec(),
            scope,
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
    let mut acc = ProjectAcc::new(project, window, days, Scope::default());
    fill_project(&mut acc, totals);
    acc.snapshot(header)
}

fn fill_project(acc: &mut ProjectAcc, totals: &Totals) {
    for s in &totals.sums {
        if !acc.scope.accepts(s.location, s.project) {
            continue;
        }
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

/// A project as the Usage page lists it: the final project of a group of
/// projects (those the alias records join to it). The group is listed when
/// any of its projects was opened.
#[derive(Clone, Debug)]
pub(crate) struct Listed {
    pub(crate) row: ProjectRow,
    name: String,
    /// The keys of the group's projects: the project is open when one is.
    keys: Vec<ProjectKey>,
    status: ProjectStatus,
    confirmed: Option<AliasId>,
}

/// Whose usage each fact is (docs/design/usage.md「项目标识」): a fact that
/// belongs to a project is the listed project that project finally is (an
/// unopened repository's, 「其他」); a fact in a directory under a folder
/// project's root is the deepest opened folder project's.
#[derive(Clone, Debug)]
pub(crate) struct Owners {
    pub(crate) listed: Vec<Listed>,
    /// `project.id` to the listed project it finally is.
    by_project: HashMap<i64, usize>,
    /// Directory location to the folder project that claims it.
    by_dir: HashMap<i64, usize>,
    /// The location each project has.
    place_of: HashMap<i64, i64>,
    /// The projects the user joined to another and has not undone, newest
    /// first (`AllUsage::merged`).
    pub(crate) merged: Vec<ProjectInfo>,
    /// The largest location and project IDs these owners know.
    pub(crate) max_location: i64,
    pub(crate) max_project: i64,
}

impl Owners {
    /// `exists` says whether a project's path still holds anything.
    pub(crate) fn new(
        projects: &[ProjectRow],
        dirs: &[(i64, Vec<u8>)],
        aliases: &Aliases,
        exists: &dyn Fn(&Path) -> bool,
    ) -> Owners {
        let final_of: HashMap<i64, i64> = projects
            .iter()
            .map(|p| (p.id, aliases.final_of(p.id)))
            .collect();
        let row_of: HashMap<i64, &ProjectRow> = projects.iter().map(|p| (p.id, p)).collect();
        let mut groups: BTreeMap<i64, Vec<&ProjectRow>> = BTreeMap::new();
        for p in projects {
            groups.entry(final_of[&p.id]).or_default().push(p);
        }
        // The project that has each location now: the latest.
        let mut latest_at: HashMap<i64, i64> = HashMap::new();
        let mut by_age: Vec<&ProjectRow> = projects.iter().collect();
        by_age.sort_by_key(|p| (p.since, p.id));
        for p in by_age {
            latest_at.insert(p.location, p.id);
        }

        let mut listed = Vec::new();
        let mut by_project = HashMap::new();
        for (final_id, members) in &groups {
            if !members.iter().any(|m| m.opened) {
                continue;
            }
            let Some(&row) = row_of.get(final_id) else {
                continue;
            };
            let index = listed.len();
            for m in members {
                by_project.insert(m.id, index);
            }
            // The newest opened member names the project: a repository that
            // moved shows the new directory's name.
            let newest = members
                .iter()
                .filter(|m| m.opened)
                .max_by_key(|m| (m.since, m.id))
                .copied()
                .unwrap_or(row);
            let present = members
                .iter()
                .any(|m| exists(&m.path) && latest_at.get(&m.location) == Some(&m.id));
            let status = match temporary_of(row, aliases) {
                Some(temp) => ProjectStatus::Temporary {
                    reason: temp.reason,
                    candidate: temp
                        .candidate
                        .map(|c| final_of.get(&c).copied().unwrap_or(c))
                        .and_then(|c| row_of.get(&c))
                        .map(|c| c.pid),
                },
                None if !present => ProjectStatus::PathMissing,
                None => ProjectStatus::Active,
            };
            listed.push(Listed {
                row: row.clone(),
                name: display_name(newest),
                keys: members.iter().map(|m| m.key()).collect(),
                status,
                confirmed: aliases.confirmed(row.id).map(|id| AliasId(id as u64)),
            });
        }

        let roots: Vec<FolderRoot> = projects
            .iter()
            .filter(|p| p.folder && p.opened)
            .map(|p| FolderRoot {
                project: p.id,
                path: crate::store::path_bytes(&p.path).to_vec(),
            })
            .collect();
        let mut by_dir = HashMap::new();
        for (id, path) in dirs {
            let claim = roots
                .iter()
                .filter(|r| under(path, &r.path))
                .max_by_key(|r| r.path.len());
            if let Some(root) = claim
                && let Some(&index) = by_project.get(&root.project)
            {
                by_dir.insert(*id, index);
            }
        }

        let merged = aliases
            .user_joins()
            .into_iter()
            .filter_map(|record| {
                let src = row_of.get(&record.src)?;
                let into = row_of.get(&final_of.get(&record.dst?).copied()?)?;
                Some(ProjectInfo {
                    id: src.pid,
                    name: display_name(src).into(),
                    folder: src.folder,
                    key: None,
                    status: ProjectStatus::Merged {
                        into: into.pid,
                        alias: AliasId(record.id as u64),
                    },
                    confirmed: None,
                })
            })
            .collect();

        let max_location = projects
            .iter()
            .map(|p| p.location)
            .chain(dirs.iter().map(|(id, _)| *id))
            .max()
            .unwrap_or(0);
        let max_project = projects.iter().map(|p| p.id).max().unwrap_or(0);
        Owners {
            listed,
            by_project,
            by_dir,
            place_of: projects.iter().map(|p| (p.id, p.location)).collect(),
            merged,
            max_location,
            max_project,
        }
    }

    /// Whose a fact is: its project's, else its directory's.
    pub(crate) fn owner(&self, location: i64, project: i64) -> Owner {
        let found = if project != 0 {
            self.by_project.get(&project)
        } else {
            self.by_dir.get(&location)
        };
        found.map_or(Owner::Other, |&i| Owner::Project(i))
    }

    /// The listed project a project row is, if its group is listed.
    pub(crate) fn listing_of(&self, project: i64) -> Option<usize> {
        self.by_project.get(&project).copied()
    }

    /// The `ProjectInfo` of listed project `index`; `open` are the projects
    /// open in this run.
    pub(crate) fn info(&self, index: usize, open: &[ProjectKey]) -> ProjectInfo {
        let l = &self.listed[index];
        ProjectInfo {
            id: l.row.pid,
            name: l.name.clone().into(),
            folder: l.row.folder,
            key: l.keys.iter().find(|k| open.contains(k)).cloned(),
            status: l.status,
            confirmed: l.confirmed,
        }
    }

    /// What listed project `index` counts.
    pub(crate) fn scope(&self, index: usize) -> Scope {
        let mut scope = Scope {
            max_location: self.max_location,
            max_project: self.max_project,
            ..Scope::default()
        };
        let mut locations = HashSet::new();
        for (project, &i) in &self.by_project {
            if i == index {
                scope.members.insert(*project);
                locations.extend(self.place_of.get(project));
            }
        }
        for (dir, &i) in &self.by_dir {
            if i == index {
                scope.claimed.insert(*dir);
                locations.insert(*dir);
            }
        }
        scope.locations = locations.into_iter().collect();
        scope.locations.sort_unstable();
        scope
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

    fn add(
        &mut self,
        (location, project): (i64, i64),
        bucket: i64,
        agent: &Agent,
        micro: &[i64; 4],
        rows: i64,
    ) {
        let Some(d) = day_of(&self.days, bucket) else {
            return;
        };
        let n = self.owners.listed.len();
        let row = match self.owners.owner(location, project) {
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
        let mut projects: Vec<(ProjectInfo, Tokens)> = (0..n)
            .zip(&rounded)
            .map(|(i, t)| (self.owners.info(i, &self.open), *t))
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
            merged: self.owners.merged.clone().into(),
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
            (s.location, s.project),
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
    /// A delta at a location or project above these needs a full compute
    /// (it may be a new directory under an open folder project, or a
    /// project made since).
    max_location: i64,
    max_project: i64,
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
    /// A project's window from its full sums; `scope` says whose facts
    /// count and how far the computation knew the IDs.
    pub(crate) fn project(
        project: ProjectInfo,
        window: Window,
        days: &[(LocalDate, Range<i64>)],
        scope: Scope,
        totals: &Totals,
    ) -> Watched {
        let (max_location, max_project) = (scope.max_location, scope.max_project);
        let mut acc = ProjectAcc::new(project, window, days, scope);
        fill_project(&mut acc, totals);
        Watched {
            acc: Acc::Project(acc),
            texts: totals.texts.clone(),
            commits: totals.commits,
            max_location,
            max_project,
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
        let (max_location, max_project) = (owners.max_location, owners.max_project);
        let mut acc = AllAcc::new(window, days, owners, open);
        fill_all(&mut acc, totals);
        Watched {
            acc: Acc::All(acc),
            texts: totals.texts.clone(),
            commits: totals.commits,
            max_location,
            max_project,
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
        if commit != self.commits + 1
            || deltas
                .iter()
                .any(|d| d.location > self.max_location || d.project > self.max_project)
        {
            return Applied::Recompute;
        }
        self.commits = commit;
        let mut changed = false;
        match &mut self.acc {
            Acc::Project(acc) => {
                let scope = std::mem::take(&mut acc.scope);
                for d in deltas
                    .iter()
                    .filter(|d| scope.accepts(d.location, d.project))
                {
                    let agent = agent_text(&self.texts, d.agent);
                    let model = model_text(&self.texts, d.model);
                    acc.add(d.bucket, &agent, &model, &d.micro, d.rows);
                    changed = true;
                }
                acc.scope = scope;
            }
            Acc::All(acc) => {
                for d in deltas {
                    let agent = agent_text(&self.texts, d.agent);
                    acc.add((d.location, d.project), d.bucket, &agent, &d.micro, d.rows);
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
