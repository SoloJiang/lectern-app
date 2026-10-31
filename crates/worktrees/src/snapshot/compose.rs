//! `build`: the composition rules of docs/design/worktrees.md「合成规则」, as a
//! pure function of the services' snapshots and the time of day.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use lectern_services::project::{Head, ProjectKey, ProjectSnapshot, Scan, Worktree, WorktreeKey};
use lectern_services::review::{ChangeCount, WorktreeChanges, WorktreesOverview};
use lectern_services::session::{
    Place, SessionKey, SessionList, SessionSummary, SourceId, Timestamp,
};
use lectern_services::usage::{MrRef, UsageQuery, UsageSnapshot, UsageState, WorktreeUsage};

use crate::diag::count;
use crate::nodes;

use super::{
    BaseRef, BranchUsage, HeadLabel, MrItem, OverlapRow, Overlaps, RowChanges, RowUsage,
    SessionRow, SourceGenerations, WorktreeRow, WorktreesSnapshot,
};

/// Sessions count for a row when they moved in the last 14 days.
const WINDOW_MS: i64 = 14 * 24 * 60 * 60 * 1000;

/// A session is running when its last turn is open and it moved in the last
/// 10 minutes.
const RUNNING_MS: i64 = 10 * 60 * 1000;

/// The sessions a row lists, newest first.
const RECENT: usize = 5;

/// What `build` reads. All of it is a copy of an `Arc` the services hand out.
pub struct Inputs<'a> {
    pub project: &'a ProjectSnapshot,
    /// Each source's list, in any order.
    pub sessions: &'a [(SourceId, Arc<SessionList>)],
    /// None before `review` has a first result.
    pub changes: Option<&'a WorktreesOverview>,
    /// None when the `usage` plugin is not there.
    pub usage: Option<&'a UsageInputs>,
    /// The plugin's count of changes of the current worktree.
    pub switches: u64,
    /// The `UsageLink`'s change count.
    pub usage_generation: u64,
    /// The time of day the 14 days and the 10 minutes count back from.
    pub now: Timestamp,
}

/// What `Usage::snapshot` returned for the queries of the previous
/// composition's step: the snapshot of each worktree that is not prunable, and
/// of each MR of a worktree whose branch is in more than one.
#[derive(Default)]
pub struct UsageInputs {
    pub worktrees: HashMap<WorktreeKey, Arc<UsageSnapshot>>,
    pub mrs: HashMap<MrRef, Arc<UsageSnapshot>>,
}

/// The result of one composition.
pub struct Built {
    pub snapshot: WorktreesSnapshot,
    /// Sessions of the `recent` lists whose tail fields are not computed yet
    /// (`SessionSummary::tail_pending`): the plugin asks `complete` for them.
    pub pending: Vec<SessionKey>,
    /// The usage queries this snapshot needs: one `Worktree` query for each
    /// worktree that is not prunable, and one `Mr` query for each MR of a
    /// worktree whose branch is in more than one MR.
    pub queries: Arc<[UsageQuery]>,
}

/// Composes the snapshot. Never fails: what is missing is left out or marked.
pub fn build(inputs: &Inputs<'_>) -> Built {
    let project = inputs.project;
    let now = inputs.now;
    let active_key = project
        .active
        .as_ref()
        .and_then(|tree| tree.worktree.clone());
    let worktrees: &[Worktree] = &project.worktrees;

    let labels: HashMap<&WorktreeKey, HeadLabel> = worktrees
        .iter()
        .map(|worktree| (&worktree.key, head_label(&worktree.head)))
        .collect();
    let changes: HashMap<&WorktreeKey, &WorktreeChanges> = inputs
        .changes
        .into_iter()
        .flat_map(|overview| overview.worktrees.iter())
        .map(|entry| (&entry.worktree, entry))
        .collect();

    let places = Places::new(inputs, worktrees);

    let mut rows: Vec<WorktreeRow> = Vec::with_capacity(worktrees.len());
    let mut pending: Vec<SessionKey> = Vec::new();
    let mut seen: HashSet<&SessionKey> = HashSet::new();
    let mut queries: Vec<UsageQuery> = Vec::new();
    let mut queried: HashSet<&MrRef> = HashSet::new();
    for (i, worktree) in worktrees.iter().enumerate() {
        let counted = places.counted(i);
        let running = counted.iter().filter(|entry| entry.running).count();
        let recent: Vec<&Counted<'_>> = counted.iter().take(RECENT).collect();
        for entry in &recent {
            if entry.summary.tail_pending && seen.insert(&entry.summary.key) {
                pending.push(entry.summary.key.clone());
            }
        }
        if !worktree.prunable {
            queries.push(UsageQuery::Worktree {
                project: project.key.clone(),
                worktree: worktree.key.clone(),
            });
        }
        let usage = row_usage(inputs, worktree);
        if let Some(mrs) = multiple_mrs(inputs, worktree) {
            for label in mrs {
                if queried.insert(&label.mr) {
                    queries.push(UsageQuery::Mr {
                        mr: label.mr.clone(),
                    });
                }
            }
        }
        rows.push(WorktreeRow {
            key: worktree.key.clone(),
            root: worktree.root.clone(),
            head: labels[&worktree.key].clone(),
            is_active: active_key.as_ref() == Some(&worktree.key),
            is_main: worktree.is_main,
            locked: worktree.locked,
            prunable: worktree.prunable,
            sessions: count(counted.len()),
            running: count(running),
            recent: recent.into_iter().map(|entry| session_row(entry)).collect(),
            changes: row_changes(worktree, changes.get(&worktree.key).copied()),
            overlaps: row_overlaps(worktree, changes.get(&worktree.key).copied(), &labels),
            usage,
            node: nodes::row(&worktree.key.0),
        });
    }

    // Idle ends with the prunable rows; within a group the project's order.
    let mut active_rows = Vec::new();
    let mut idle_rows = Vec::new();
    let mut missing_rows = Vec::new();
    for row in rows {
        if row.prunable {
            missing_rows.push(row);
        } else if row.running > 0 {
            active_rows.push(row);
        } else {
            idle_rows.push(row);
        }
    }
    idle_rows.append(&mut missing_rows);

    let snapshot = WorktreesSnapshot {
        project: project.key.clone(),
        sources: SourceGenerations {
            project: project.generation,
            sessions: inputs
                .sessions
                .iter()
                .map(|(id, list)| (id.clone(), list.generation))
                .collect(),
            changes: inputs.changes.map(|overview| overview.generation),
            usage: inputs.usage_generation,
        },
        built_at: now,
        active: active_key,
        files_settled: project
            .active
            .as_ref()
            .is_some_and(|tree| !matches!(tree.files, Scan::Pending)),
        switches: inputs.switches,
        base: inputs
            .changes
            .and_then(|overview| overview.base.as_ref())
            .map(|full| BaseRef {
                full: full.clone(),
                short: short_ref(full),
            }),
        active_rows: active_rows.into(),
        idle_rows: idle_rows.into(),
        next_expiry: places.next_expiry,
        folder: matches!(project.key, ProjectKey::Folder { .. }),
    };
    Built {
        snapshot,
        pending,
        queries: queries.into(),
    }
}

/// `refs/heads/feat/x` → `feat/x`, `refs/remotes/origin/main` →
/// `origin/main`; other names stay as they are.
fn short_ref(full: &str) -> Arc<str> {
    let short = full
        .strip_prefix("refs/heads/")
        .or_else(|| full.strip_prefix("refs/remotes/"))
        .unwrap_or(full);
    Arc::from(short)
}

fn head_label(head: &Head) -> HeadLabel {
    match head {
        Head::Branch(name) => HeadLabel::Branch(short_ref(name)),
        Head::Detached(oid) => HeadLabel::Detached(Arc::from(oid.get(..8).unwrap_or(oid))),
        Head::Unborn(name) => HeadLabel::Unborn(short_ref(name)),
        Head::Unknown => HeadLabel::Unknown,
    }
}

/// `open_turn` and moved in the last 10 minutes.
fn is_running(summary: &SessionSummary, now: Timestamp) -> bool {
    summary.open_turn && now.0.saturating_sub(summary.last_activity.0) <= RUNNING_MS
}

/// The index in `worktrees` of the worktree the session is placed in.
fn worktree_of(summary: &SessionSummary, worktrees: &[Worktree]) -> Option<usize> {
    let key = match &summary.place {
        Place::Located(location) => location.worktree.as_ref(),
        Place::Resolved(resolved) => resolved.worktree.as_ref(),
        _ => None,
    }?;
    worktrees.iter().position(|worktree| worktree.key == *key)
}

/// A session that counts for a row: a session that is not merged into a
/// parent, and whether it or a sub-agent session merged into it is running.
struct Counted<'a> {
    summary: &'a SessionSummary,
    running: bool,
}

/// Where the sessions of the project are, and which of them count.
struct Places<'a> {
    /// For each worktree, the sessions that count, newest first.
    by_worktree: Vec<Vec<Counted<'a>>>,
    next_expiry: Option<Timestamp>,
}

impl<'a> Places<'a> {
    fn new(inputs: &'a Inputs<'a>, worktrees: &[Worktree]) -> Places<'a> {
        let now = inputs.now;
        // The sessions of the project that moved in the last 14 days and are
        // placed in a listed worktree.
        let mut candidates: HashMap<&'a SessionKey, (usize, &'a SessionSummary)> = HashMap::new();
        for (_, list) in inputs.sessions {
            for summary in list.in_project(&inputs.project.key) {
                if now.0.saturating_sub(summary.last_activity.0) > WINDOW_MS {
                    continue;
                }
                if let Some(worktree) = worktree_of(summary, worktrees) {
                    candidates.insert(&summary.key, (worktree, summary));
                }
            }
        }

        // A sub-agent session merges into its parent when the parent is a
        // candidate in the same worktree; a chain of them into the first
        // session of the chain. The walk is bounded, so a damaged `parent`
        // chain cannot loop.
        let root_of = |start: &'a SessionKey| -> &'a SessionKey {
            let mut current = start;
            for _ in 0..candidates.len() {
                let (worktree, summary) = candidates[current];
                let Some(parent) = summary.parent.as_ref() else {
                    break;
                };
                match candidates.get_key_value(&parent.session) {
                    Some((key, (parent_worktree, _))) if *parent_worktree == worktree => {
                        current = key;
                    }
                    _ => break,
                }
            }
            current
        };

        let mut groups: HashMap<&'a SessionKey, Counted<'a>> = HashMap::new();
        let mut next_expiry: Option<i64> = None;
        for (key, (_, summary)) in &candidates {
            let running = is_running(summary, now);
            if running {
                // The first instant at which it is no longer running.
                let expiry = summary.last_activity.0.saturating_add(RUNNING_MS + 1);
                next_expiry = Some(next_expiry.map_or(expiry, |least| least.min(expiry)));
            }
            let root = root_of(key);
            groups
                .entry(root)
                .or_insert_with(|| Counted {
                    summary: candidates[root].1,
                    running: false,
                })
                .running |= running;
        }

        let mut by_worktree: Vec<Vec<Counted<'a>>> =
            (0..worktrees.len()).map(|_| Vec::new()).collect();
        for (root, counted) in groups {
            by_worktree[candidates[root].0].push(counted);
        }
        for list in &mut by_worktree {
            list.sort_by(|a, b| {
                b.summary
                    .last_activity
                    .cmp(&a.summary.last_activity)
                    .then_with(|| a.summary.key.cmp(&b.summary.key))
            });
        }
        Places {
            by_worktree,
            next_expiry: next_expiry.map(Timestamp),
        }
    }

    fn counted(&self, worktree: usize) -> &[Counted<'a>] {
        &self.by_worktree[worktree]
    }
}

fn session_row(entry: &Counted<'_>) -> SessionRow {
    let summary = entry.summary;
    SessionRow {
        key: summary.key.clone(),
        agent: summary.agent.clone(),
        title: summary.title.clone(),
        last_activity: summary.last_activity,
        running: entry.running,
        node: nodes::session(&summary.key.id),
    }
}

fn row_changes(worktree: &Worktree, entry: Option<&WorktreeChanges>) -> RowChanges {
    if worktree.prunable {
        return RowChanges {
            uncommitted: ChangeCount::Unavailable,
            committed: ChangeCount::Unavailable,
            vs_base: ChangeCount::Unavailable,
            counted_at: None,
        };
    }
    match entry {
        Some(entry) => RowChanges {
            uncommitted: entry.uncommitted,
            committed: entry.committed,
            vs_base: entry.vs_base,
            counted_at: entry.counted_at,
        },
        None => RowChanges {
            uncommitted: ChangeCount::Pending,
            committed: ChangeCount::Pending,
            vs_base: ChangeCount::Pending,
            counted_at: None,
        },
    }
}

fn row_overlaps(
    worktree: &Worktree,
    entry: Option<&WorktreeChanges>,
    labels: &HashMap<&WorktreeKey, HeadLabel>,
) -> Overlaps {
    // `review` does not compare a prunable worktree: waiting for it would
    // never end.
    if worktree.prunable {
        return Overlaps::Unknown;
    }
    let Some(entry) = entry else {
        return Overlaps::Pending;
    };
    match &entry.overlaps {
        Some(list) => Overlaps::List(
            list.iter()
                .filter_map(|overlap| {
                    Some(OverlapRow {
                        other: overlap.other.clone(),
                        other_head: labels.get(&overlap.other)?.clone(),
                        files: overlap.files,
                        paths: overlap.paths.clone(),
                        node: nodes::overlap(&overlap.other.0),
                    })
                })
                .collect(),
        ),
        // The set of paths is unknown: not yet, or for good.
        None if entry.uncommitted == ChangeCount::Pending
            || entry.vs_base == ChangeCount::Pending =>
        {
            Overlaps::Pending
        }
        None => Overlaps::Unknown,
    }
}

/// The snapshot of a worktree's usage, when `usage` is there and the
/// worktree is asked for.
fn worktree_usage<'a>(inputs: &'a Inputs<'_>, worktree: &Worktree) -> Option<&'a WorktreeUsage> {
    match inputs.usage?.worktrees.get(&worktree.key)?.as_ref() {
        UsageSnapshot::Worktree(usage) => Some(usage),
        _ => None,
    }
}

/// The MRs of a worktree whose branch is in more than one MR: each gets a
/// snapshot of its own.
fn multiple_mrs<'a>(
    inputs: &'a Inputs<'_>,
    worktree: &Worktree,
) -> Option<&'a [lectern_services::usage::MrLabel]> {
    if worktree.prunable {
        return None;
    }
    let usage = worktree_usage(inputs, worktree)?;
    (usage.mrs.len() > 1).then_some(&usage.mrs[..])
}

fn row_usage(inputs: &Inputs<'_>, worktree: &Worktree) -> RowUsage {
    let Some(usage_inputs) = inputs.usage else {
        return RowUsage::Absent;
    };
    if worktree.prunable {
        return RowUsage::Absent;
    }
    let Some(usage) = worktree_usage(inputs, worktree) else {
        return RowUsage::Pending;
    };
    if matches!(usage.header.state, UsageState::Loading) {
        return RowUsage::Pending;
    }
    match usage.mrs.len() {
        0 => match usage.cumulative {
            None => RowUsage::NotCounted,
            Some(cumulative) => RowUsage::Branch(BranchUsage {
                cumulative,
                review: usage.review,
                since: usage.since,
                may_include_inherited: usage.header.may_include_inherited,
                backfilling: matches!(usage.header.state, UsageState::Backfilling { .. }),
            }),
        },
        1 => RowUsage::Mrs(Arc::from(vec![MrItem {
            label: usage.mrs[0].clone(),
            tokens: usage.cumulative,
            shared_with: 0,
        }])),
        _ => RowUsage::Mrs(
            usage
                .mrs
                .iter()
                .map(|label| {
                    let snapshot = usage_inputs.mrs.get(&label.mr).map(|s| s.as_ref());
                    match snapshot {
                        Some(UsageSnapshot::Mr(mr))
                            if !matches!(mr.header.state, UsageState::Loading) =>
                        {
                            MrItem {
                                label: label.clone(),
                                tokens: Some(mr.write),
                                shared_with: count(mr.shared_with.len()),
                            }
                        }
                        _ => MrItem {
                            label: label.clone(),
                            tokens: None,
                            shared_with: 0,
                        },
                    }
                })
                .collect(),
        ),
    }
}
