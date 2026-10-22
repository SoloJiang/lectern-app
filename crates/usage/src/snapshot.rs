//! Window snapshots from fact sums (docs/design/usage.md「取整与时间窗口」):
//! buckets go to local days, sums of millionths become whole tokens with the
//! largest remainder method in each kind separately, so every group of rows
//! adds up to its total.

use std::collections::{BTreeMap, HashMap};
use std::ops::Range;
use std::sync::Arc;

use lectern_services::project::ProjectKey;
use lectern_services::session::Agent;
use lectern_services::usage::{
    AllUsage, DayTotals, LocalDate, ProjectInfo, ProjectStatus, ProjectUsage, SnapshotHeader,
    Tokens, Window,
};

use crate::round::largest_remainder;
use crate::store::under;
use crate::store::{FolderRoot, ProjectRow, Totals};
use crate::window::BUCKET_MS;

/// Agents in a fixed order: Claude Code, Codex, then the others by name.
fn agent_rank(agent: &Agent) -> (u8, &str) {
    match agent {
        Agent::ClaudeCode => (0, ""),
        Agent::Codex => (1, ""),
        Agent::Other(name) => (2, name),
    }
}

fn agent_text(totals: &Totals, id: i64) -> Agent {
    crate::store::agent_of(totals.texts.get(&id).map_or(&[][..], Vec::as_slice))
}

fn model_text(totals: &Totals, id: i64) -> Arc<str> {
    String::from_utf8_lossy(totals.texts.get(&id).map_or(&[][..], Vec::as_slice))
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

fn add(into: &mut [u64; 4], micro: &[u64; 4]) {
    for (a, b) in into.iter_mut().zip(micro) {
        *a = a.saturating_add(*b);
    }
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
fn daily(
    days: &[(LocalDate, Range<i64>)],
    agents: &[Agent],
    per_day: &[HashMap<usize, u64>],
) -> Arc<[DayTotals]> {
    (0..days.len())
        .map(|d| {
            let parts: Vec<u64> = (0..agents.len())
                .map(|a| per_day[d].get(&a).copied().unwrap_or(0))
                .collect();
            let rounded = largest_remainder(&parts);
            agents
                .iter()
                .cloned()
                .zip(rounded)
                .collect::<Vec<_>>()
                .into()
        })
        .collect::<Vec<_>>()
        .into()
}

/// Agents present in `sums`, in their fixed order.
fn agents_of(totals: &Totals) -> Vec<Agent> {
    let mut agents: Vec<Agent> = Vec::new();
    for s in &totals.sums {
        let a = agent_text(totals, s.agent);
        if !agents.contains(&a) {
            agents.push(a);
        }
    }
    agents.sort_by(|a, b| agent_rank(a).cmp(&agent_rank(b)));
    agents
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

/// The order of the by-model rows: agent rank and name, then model.
type ModelKey = ((u8, String), String);
/// A by-model row: agent, model, millionths per kind.
type ModelRow = (Agent, Arc<str>, [u64; 4]);

/// A project's window: totals, by model, by agent and per day. `rows` stays
/// empty in step 1 (every turn is attributed by its cwd, no branches yet).
pub(crate) fn project(
    header: SnapshotHeader,
    project: ProjectInfo,
    window: Window,
    days: &[(LocalDate, Range<i64>)],
    totals: &Totals,
) -> ProjectUsage {
    let agents = agents_of(totals);
    let index = |a: &Agent| agents.iter().position(|x| x == a).unwrap_or(0);
    let mut by_model: BTreeMap<ModelKey, ModelRow> = BTreeMap::new();
    let mut by_agent: Vec<[u64; 4]> = vec![[0; 4]; agents.len()];
    let mut per_day: Vec<HashMap<usize, u64>> = vec![HashMap::new(); days.len()];
    for s in &totals.sums {
        let Some(d) = day_of(days, s.bucket) else {
            continue;
        };
        let agent = agent_text(totals, s.agent);
        let model = model_text(totals, s.model);
        let (rank, name) = agent_rank(&agent);
        let slot = by_model
            .entry(((rank, name.to_owned()), model.to_string()))
            .or_insert_with(|| (agent.clone(), model.clone(), [0; 4]));
        add(&mut slot.2, &s.micro);
        let a = index(&agent);
        add(&mut by_agent[a], &s.micro);
        let day_total = s.micro.iter().fold(0u64, |t, m| t.saturating_add(*m));
        let cell = per_day[d].entry(a).or_insert(0);
        *cell = cell.saturating_add(day_total);
    }
    let model_rows: Vec<_> = by_model.into_values().collect();
    let model_tokens = round_rows(&model_rows.iter().map(|r| r.2).collect::<Vec<_>>());
    let agent_tokens = round_rows(&by_agent);
    let total = total_of(&agent_tokens);
    ProjectUsage {
        header,
        project,
        window,
        days: days.iter().map(|(d, _)| *d).collect::<Vec<_>>().into(),
        total,
        rows: Arc::from(Vec::new()),
        by_model: model_rows
            .into_iter()
            .zip(model_tokens)
            .map(|((agent, model, _), t)| (agent, model, t))
            .collect::<Vec<_>>()
            .into(),
        by_agent: agents
            .iter()
            .cloned()
            .zip(agent_tokens)
            .collect::<Vec<_>>()
            .into(),
        daily: daily(days, &agents, &per_day),
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
pub(crate) struct Owners {
    pub(crate) listed: Vec<ProjectRow>,
    by_location: HashMap<i64, Owner>,
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
        Owners {
            listed,
            by_location,
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
pub(crate) fn all(
    header: SnapshotHeader,
    window: Window,
    days: &[(LocalDate, Range<i64>)],
    totals: &Totals,
    owners: &Owners,
    open: &[ProjectKey],
) -> AllUsage {
    let agents = agents_of(totals);
    let index = |a: &Agent| agents.iter().position(|x| x == a).unwrap_or(0);
    let n = owners.listed.len();
    let mut rows: Vec<[u64; 4]> = vec![[0; 4]; n + 1];
    let mut per_day: Vec<HashMap<usize, u64>> = vec![HashMap::new(); days.len()];
    for s in &totals.sums {
        let Some(d) = day_of(days, s.bucket) else {
            continue;
        };
        let row = match owners.owner(s.location) {
            Owner::Project(i) => i,
            Owner::Other => n,
        };
        add(&mut rows[row], &s.micro);
        let a = index(&agent_text(totals, s.agent));
        let day_total = s.micro.iter().fold(0u64, |t, m| t.saturating_add(*m));
        let cell = per_day[d].entry(a).or_insert(0);
        *cell = cell.saturating_add(day_total);
    }
    let rounded = round_rows(&rows);
    let total = total_of(&rounded);
    let mut projects: Vec<(ProjectInfo, Tokens)> = owners
        .listed
        .iter()
        .zip(&rounded)
        .map(|(p, t)| (info(p, open), *t))
        .collect();
    projects.sort_by(|a, b| {
        b.1.total()
            .cmp(&a.1.total())
            .then_with(|| a.0.name.cmp(&b.0.name))
            .then_with(|| a.0.id.cmp(&b.0.id))
    });
    AllUsage {
        header,
        window,
        days: days.iter().map(|(d, _)| *d).collect::<Vec<_>>().into(),
        total,
        projects: projects.into(),
        other: rounded[n],
        daily: daily(days, &agents, &per_day),
    }
}
