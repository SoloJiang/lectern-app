//! `build`: the classification rules of docs/design/project_home.md
//! 「视图读取的快照」, as a pure function of the services' snapshots and the
//! time of day.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use lectern_services::project::{
    Head, Package, ProjectKey, ProjectSnapshot, RelPath, Scan, Worktree, WorktreeKey,
};
use lectern_services::review::{OutsideCount, Overview, Progress, SessionOverview};
use lectern_services::session::{
    Place, SessionKey, SessionList, SessionSummary, SourceId, Timestamp,
};
use lectern_services::usage::{Limits, UsageSnapshot, UsageState};

use crate::diag::count;

use super::{
    Card, GroupKind, HomeSnapshot, MainModel, OutsideRow, PackageRow, SessionRow, SidebarModel,
    SidebarRow, Signals, SourceGenerations, TightestLimit, UsageModel, WorktreeGroup,
};

/// The sidebar lists the sessions of the last 14 days, the same window the
/// overview declares in.
const WINDOW_MS: i64 = 14 * 24 * 60 * 60 * 1000;

/// A session is running when its last turn is open and it moved in the last
/// 10 minutes.
const RUNNING_MS: i64 = 10 * 60 * 1000;

/// What `build` reads. All of it is a copy of an `Arc` the services hand out.
pub struct Inputs<'a> {
    pub project: &'a ProjectSnapshot,
    /// Each source's list, in any order.
    pub sessions: &'a [(SourceId, Arc<SessionList>)],
    /// None before `review` has a first result.
    pub overview: Option<&'a Overview>,
    /// Files changed today per sub-project directory, as `Overview.packages`
    /// will give them (project_home.md 前置条件 A). None when they are not
    /// available; the plugin passes None until `review` provides them.
    pub changed_today: Option<&'a [(RelPath, u32)]>,
    /// None when the `usage` plugin is not there.
    pub usage: Option<&'a UsageInputs>,
    /// The time of day the 14 days and the 10 minutes count back from.
    pub now: Timestamp,
}

/// What `Usage::snapshot` and `Usage::limits` returned.
pub struct UsageInputs {
    /// The project's 7-day snapshot; None while it is not computed.
    pub snapshot: Option<Arc<UsageSnapshot>>,
    pub limits: Arc<Limits>,
}

/// The result of one composition.
pub struct Built {
    pub snapshot: HomeSnapshot,
    /// Overview sessions that are not in any session list (the file was just
    /// deleted); they are left out of the cards.
    pub missing: Vec<SessionKey>,
    /// Displayed sessions whose tail fields are not computed yet
    /// (`SessionSummary::tail_pending`): the plugin asks `complete` for them.
    pub pending: Vec<SessionKey>,
}

/// Composes the snapshot. Never fails: what is missing is left out or marked.
pub fn build(inputs: &Inputs<'_>) -> Built {
    let project = inputs.project;
    let folder = matches!(project.key, ProjectKey::Folder { .. });
    let active = project.active.as_ref();
    let active_worktree = active.and_then(|tree| tree.worktree.clone());
    let branch = if folder {
        None
    } else {
        active_worktree
            .as_ref()
            .and_then(|key| project.worktrees.iter().find(|w| w.key == *key))
            .map(|w| short_head(&w.head))
    };

    let groups = groups(inputs, folder, active_worktree.as_ref());
    let session_rows = count(groups.iter().map(|g| g.sessions.len()).sum());

    let capped = matches!(
        inputs.overview.map(|o| o.outside),
        Some(OutsideCount::Capped(_))
    );
    let counts = if capped { None } else { inputs.changed_today };
    let package_state = active.map(|tree| &tree.packages);
    let packages = match package_state {
        Some(Scan::Ready(list)) => package_rows(list, counts),
        _ => Vec::new(),
    };

    let mut rows = vec![SidebarRow::SessionsHeader];
    if groups.is_empty() {
        rows.push(SidebarRow::SessionsEmpty);
    }
    for (g, group) in groups.iter().enumerate() {
        if !matches!(group.kind, GroupKind::Folder) {
            rows.push(SidebarRow::WorktreeHeader(count(g)));
        }
        for i in 0..group.sessions.len() {
            rows.push(SidebarRow::Session {
                group: count(g),
                index: count(i),
            });
        }
    }
    match package_state {
        Some(Scan::Pending) => {
            rows.push(SidebarRow::PackagesHeader);
            rows.push(SidebarRow::PackagesSkeleton);
        }
        Some(Scan::Ready(_)) if !packages.is_empty() => {
            rows.push(SidebarRow::PackagesHeader);
            rows.extend((0..packages.len()).map(|i| SidebarRow::Package(count(i))));
        }
        _ => {}
    }

    let (main, missing) = main_model(inputs, folder);
    let mut seen = HashSet::new();
    let pending: Vec<SessionKey> = groups
        .iter()
        .flat_map(|g| g.sessions.iter())
        .chain(main.needs_review.iter().map(|c| &c.session))
        .chain(main.done.iter().map(|c| &c.session))
        .filter(|row| row.tail_pending && seen.insert(&row.key))
        .map(|row| row.key.clone())
        .collect();

    let snapshot = HomeSnapshot {
        project: project.key.clone(),
        sources: SourceGenerations {
            project: project.generation,
            sessions: inputs
                .sessions
                .iter()
                .map(|(id, list)| (id.clone(), list.generation))
                .collect(),
            overview: inputs.overview.map(|o| o.generation),
            usage: inputs.usage.map(|u| {
                u.snapshot
                    .as_deref()
                    .and_then(snapshot_generation)
                    .unwrap_or(u.limits.generation)
            }),
        },
        name: project.name.clone(),
        branch,
        active_worktree,
        files_settled: active.is_some_and(|tree| !matches!(tree.files, Scan::Pending)),
        sidebar: SidebarModel {
            groups: groups.into(),
            session_rows,
            packages: packages.into(),
            package_counts: counts.is_some(),
            rows: rows.into(),
        },
        main,
        usage: inputs.usage.map(usage_model),
    };
    Built {
        snapshot,
        missing,
        pending,
    }
}

/// `refs/heads/main` → `main`; a detached HEAD is the first 8 characters of
/// the OID. A HEAD that could not be read has no name of its own, so it shows
/// as git calls it.
fn short_head(head: &Head) -> Arc<str> {
    match head {
        Head::Branch(name) | Head::Unborn(name) => {
            Arc::from(name.strip_prefix("refs/heads/").unwrap_or(name))
        }
        Head::Detached(oid) => Arc::from(oid.get(..8).unwrap_or(oid)),
        Head::Unknown => Arc::from("HEAD"),
    }
}

fn row(summary: &SessionSummary, now: Timestamp) -> SessionRow {
    SessionRow {
        key: summary.key.clone(),
        agent: summary.agent.clone(),
        title: summary.title.clone(),
        last_activity: summary.last_activity,
        running: summary.open_turn && now.0.saturating_sub(summary.last_activity.0) <= RUNNING_MS,
        tail_pending: summary.tail_pending,
    }
}

/// The sidebar's session groups: the sessions of the project that are not
/// sub-agents' and moved in the last 14 days, by worktree.
fn groups(inputs: &Inputs<'_>, folder: bool, active: Option<&WorktreeKey>) -> Vec<WorktreeGroup> {
    let project = inputs.project;
    // A prunable worktree is listed by `project` but is not a place to show:
    // its sessions go to `Other`.
    let listed: Vec<&Worktree> = if folder {
        Vec::new()
    } else {
        project.worktrees.iter().filter(|w| !w.prunable).collect()
    };
    let mut buckets: Vec<Vec<&SessionSummary>> = vec![Vec::new(); listed.len()];
    let mut loose: Vec<&SessionSummary> = Vec::new();
    for (_, list) in inputs.sessions {
        for summary in list.in_project(&project.key) {
            if summary.parent.is_some()
                || inputs.now.0.saturating_sub(summary.last_activity.0) > WINDOW_MS
            {
                continue;
            }
            let worktree = match &summary.place {
                Place::Located(location) => location.worktree.as_ref(),
                Place::Resolved(resolved) => resolved.worktree.as_ref(),
                _ => None,
            };
            match worktree.and_then(|key| listed.iter().position(|w| w.key == *key)) {
                Some(i) => buckets[i].push(summary),
                None => loose.push(summary),
            }
        }
    }

    let to_rows = |mut bucket: Vec<&SessionSummary>| -> Arc<[SessionRow]> {
        bucket.sort_by(|a, b| {
            b.last_activity
                .cmp(&a.last_activity)
                .then_with(|| a.key.cmp(&b.key))
        });
        bucket.into_iter().map(|s| row(s, inputs.now)).collect()
    };
    let mut groups = Vec::new();
    for (worktree, bucket) in listed.iter().zip(buckets) {
        if bucket.is_empty() {
            continue;
        }
        groups.push(WorktreeGroup {
            worktree: Some(worktree.key.clone()),
            kind: GroupKind::Worktree {
                branch: short_head(&worktree.head),
                is_active: active == Some(&worktree.key),
            },
            sessions: to_rows(bucket),
        });
    }
    if !loose.is_empty() {
        groups.push(WorktreeGroup {
            worktree: None,
            kind: if folder {
                GroupKind::Folder
            } else {
                GroupKind::Other
            },
            sessions: to_rows(loose),
        });
    }
    groups
}

/// All sub-projects, in directory order (a parent before its members), with
/// the nesting depth and today's change count.
fn package_rows(list: &[Package], counts: Option<&[(RelPath, u32)]>) -> Vec<PackageRow> {
    let today: HashMap<&str, u32> = counts
        .into_iter()
        .flatten()
        .map(|(dir, n)| (&**dir, *n))
        .collect();
    let mut order: Vec<usize> = (0..list.len()).collect();
    order.sort_by(|&a, &b| list[a].dir.split('/').cmp(list[b].dir.split('/')));
    order
        .into_iter()
        .map(|i| PackageRow {
            dir: list[i].dir.clone(),
            name: list[i].name.clone(),
            depth: depth(list, i),
            changed_today: today.get(&*list[i].dir).copied().unwrap_or(0),
        })
        .collect()
}

/// How many enclosing sub-projects `list[i]` has. The walk is bounded by the
/// list's length, so a damaged `parent` chain cannot loop.
fn depth(list: &[Package], i: usize) -> u8 {
    let mut depth = 0u8;
    let mut parent = list[i].parent;
    for _ in 0..list.len() {
        let Some(p) = parent else { break };
        depth = depth.saturating_add(1);
        parent = list.get(p as usize).and_then(|package| package.parent);
    }
    depth
}

/// The main area, and the overview sessions the session lists do not know.
fn main_model(inputs: &Inputs<'_>, folder: bool) -> (MainModel, Vec<SessionKey>) {
    let index: HashMap<&SessionKey, &SessionSummary> = inputs
        .sessions
        .iter()
        .flat_map(|(_, list)| list.sessions.iter())
        .map(|summary| (&summary.key, summary))
        .collect();
    let mut needs_review = Vec::new();
    let mut done = Vec::new();
    let mut missing = Vec::new();
    for entry in inputs.overview.iter().flat_map(|o| o.sessions.iter()) {
        let Some(summary) = index.get(&entry.session) else {
            missing.push(entry.session.clone());
            continue;
        };
        let card = card(summary, entry, inputs.now);
        match entry.progress {
            None => needs_review.push(card),
            Some(p) if p.seen < p.total || p.changed_since_seen > 0 => needs_review.push(card),
            Some(p) if p.total > 0 => done.push(card),
            // No files at all: nothing to review and nothing done.
            Some(_) => {}
        }
    }
    let outside = if folder {
        OutsideRow::Hidden
    } else {
        match inputs.overview.map(|o| o.outside) {
            None | Some(OutsideCount::Pending) => OutsideRow::Pending,
            Some(OutsideCount::Files(n)) => OutsideRow::Files(n),
            Some(OutsideCount::Capped(n)) => OutsideRow::Capped(n),
        }
    };
    let main = MainModel {
        overview_ready: inputs.overview.is_some(),
        needs_review: needs_review.into(),
        outside,
        done: done.into(),
    };
    (main, missing)
}

fn card(summary: &SessionSummary, entry: &SessionOverview, now: Timestamp) -> Card {
    Card {
        session: row(summary, now),
        progress: entry.progress,
        signals: Signals {
            drifted: entry.drifted == Some(true),
            critical: entry.critical,
            changed_since_seen: entry.progress.map_or(0, |p: Progress| p.changed_since_seen),
        },
    }
}

fn snapshot_generation(snapshot: &UsageSnapshot) -> Option<u64> {
    match snapshot {
        UsageSnapshot::Project(usage) => Some(usage.header.generation),
        _ => None,
    }
}

fn usage_model(usage: &UsageInputs) -> UsageModel {
    let tokens_7d = match usage.snapshot.as_deref() {
        Some(UsageSnapshot::Project(project))
            if !matches!(project.header.state, UsageState::Loading) =>
        {
            Some(project.total.total())
        }
        _ => None,
    };
    let tightest = usage
        .limits
        .tightest
        .and_then(|i| usage.limits.windows.get(usize::from(i)))
        .map(|window| TightestLimit {
            agent: window.agent.clone(),
            used_percent: window.used_percent.round().clamp(0.0, 100.0) as u8,
            resets_at: window.resets_at,
        });
    UsageModel {
        tokens_7d,
        tightest,
    }
}
