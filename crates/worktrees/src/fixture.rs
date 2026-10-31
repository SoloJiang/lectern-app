//! Fixtures of the tests and of the `worktrees/*` benchmarks
//! (docs/design/worktrees.md「测试」「性能」): constructors of the services'
//! snapshots with synthetic names, paths and titles, and the inputs of
//! `worktrees/build_1000`. Compiled for this crate's tests and with the
//! `test-support` feature, which `lectern-bench` turns on; the fakes of the
//! services are in `testing`, which only the tests use.

use std::path::Path;
use std::sync::Arc;

use lectern_services::project::{
    ActiveTree, FileList, Head, Location, PackageMatch, ProjectKey, ProjectSnapshot, RelPath,
    Resolved, Scan, Worktree, WorktreeKey,
};
use lectern_services::review::{ChangeCount, Overlap, WorktreeChanges, WorktreesOverview};
use lectern_services::session::{
    Agent, Format, Parent, Place, SessionKey, SessionList, SessionSummary, SourceId, Timestamp,
};
use lectern_services::usage::{
    ForgeRepo, MrLabel, MrRef, MrState, MrUsage, SnapshotHeader, Tokens, UsageQuery, UsageSnapshot,
    UsageState, WorktreeUsage,
};

use crate::snapshot::{Inputs, UsageInputs};

/// The composing time of the tests: 2027-01-15 08:00 UTC, in milliseconds.
pub const NOW: i64 = 1_800_000_000_000;
pub const MINUTE: i64 = 60_000;
pub const DAY: i64 = 24 * 60 * MINUTE;

// ---------------------------------------------------------------- projects

pub fn git_key(name: &str) -> ProjectKey {
    ProjectKey::Git {
        common_dir: Arc::from(Path::new(&format!("/fixture/{name}/.git"))),
    }
}

pub fn folder_key(name: &str) -> ProjectKey {
    ProjectKey::Folder {
        root: Arc::from(Path::new(&format!("/fixture/{name}"))),
    }
}

/// A worktree key of the project `name`; "main" is the main worktree.
pub fn wt_key(project: &str, worktree: &str) -> WorktreeKey {
    let path = if worktree == "main" {
        format!("/fixture/{project}/.git")
    } else {
        format!("/fixture/{project}/.git/worktrees/{worktree}")
    };
    WorktreeKey(Arc::from(Path::new(&path)))
}

pub fn branch(name: &str) -> Head {
    Head::Branch(Arc::from(format!("refs/heads/{name}")))
}

/// Builds a `ProjectSnapshot`.
pub struct ProjectFx {
    name: String,
    key: ProjectKey,
    worktrees: Vec<Worktree>,
    active: Option<ActiveTree>,
    generation: u64,
}

fn active_tree(root: Arc<Path>, worktree: Option<WorktreeKey>) -> ActiveTree {
    ActiveTree {
        root,
        worktree,
        opened_at: Arc::from(""),
        files: Scan::Ready(Arc::new(FileList::builder().build())),
        packages: Scan::Ready(Arc::from(Vec::new())),
        dirs: Scan::Pending,
    }
}

impl ProjectFx {
    /// A git project with its main worktree on `main`, active, with an empty
    /// ready file list.
    pub fn git(name: &str) -> ProjectFx {
        let main = worktree(name, "main", branch("main"));
        ProjectFx {
            name: name.to_owned(),
            key: git_key(name),
            active: Some(active_tree(main.root.clone(), Some(main.key.clone()))),
            worktrees: vec![main],
            generation: 1,
        }
    }

    /// A plain folder: no worktrees, an empty ready file list.
    pub fn folder(name: &str) -> ProjectFx {
        ProjectFx {
            name: name.to_owned(),
            key: folder_key(name),
            worktrees: Vec::new(),
            active: Some(active_tree(
                Arc::from(Path::new(&format!("/fixture/{name}"))),
                None,
            )),
            generation: 1,
        }
    }

    pub fn key(&self) -> ProjectKey {
        self.key.clone()
    }

    /// The key of this project's worktree `name` ("main" for the main one).
    pub fn wt(&self, name: &str) -> WorktreeKey {
        wt_key(&self.name, name)
    }

    /// Adds a linked worktree.
    pub fn linked(mut self, name: &str, head: Head) -> ProjectFx {
        self.worktrees.push(worktree(&self.name, name, head));
        self
    }

    /// Adds a linked worktree whose root no longer exists.
    pub fn prunable(mut self, name: &str, head: Head) -> ProjectFx {
        let mut w = worktree(&self.name, name, head);
        w.prunable = true;
        self.worktrees.push(w);
        self
    }

    /// Marks the worktree `name` as locked.
    pub fn locked(mut self, name: &str) -> ProjectFx {
        let key = wt_key(&self.name, name);
        if let Some(w) = self.worktrees.iter_mut().find(|w| w.key == key) {
            w.locked = true;
        }
        self
    }

    /// Makes the worktree `name` the active one.
    pub fn active_in(mut self, name: &str) -> ProjectFx {
        let key = wt_key(&self.name, name);
        let w = self.worktrees.iter().find(|w| w.key == key);
        if let (Some(w), Some(tree)) = (w, self.active.as_mut()) {
            tree.root = w.root.clone();
            tree.worktree = Some(w.key.clone());
        }
        self
    }

    /// The active tree's file list is still being scanned.
    pub fn files_pending(mut self) -> ProjectFx {
        if let Some(tree) = self.active.as_mut() {
            tree.files = Scan::Pending;
        }
        self
    }

    pub fn generation(mut self, generation: u64) -> ProjectFx {
        self.generation = generation;
        self
    }

    pub fn snapshot(&self) -> ProjectSnapshot {
        ProjectSnapshot {
            key: self.key.clone(),
            generation: self.generation,
            name: Arc::from(self.name.as_str()),
            worktrees: Arc::from(self.worktrees.clone()),
            active: self.active.clone(),
        }
    }
}

fn worktree(project: &str, name: &str, head: Head) -> Worktree {
    Worktree {
        key: wt_key(project, name),
        root: Arc::from(Path::new(&format!("/fixture/{project}-{name}"))),
        is_main: name == "main",
        head,
        locked: false,
        prunable: false,
    }
}

// ---------------------------------------------------------------- sessions

/// Builds a `SessionSummary` that is placed in a worktree of a project.
pub struct Sess(SessionSummary);

/// A session `id` of source `source`, located in `worktree` of `project`.
pub fn sess(source: &str, id: &str, project: &ProjectKey, worktree: Option<&WorktreeKey>) -> Sess {
    Sess(SessionSummary {
        key: SessionKey {
            source: SourceId(Arc::from(source)),
            id: Arc::from(id),
        },
        agent: Agent::ClaudeCode,
        parent: None,
        file: Arc::from(Path::new(&format!("/fixture/log/{id}.jsonl"))),
        cwd: Arc::from(Path::new("/fixture/cwd")),
        place: Place::Located(Location {
            project: project.clone(),
            worktree: worktree.cloned(),
            rel: Arc::from(""),
            package: PackageMatch::None,
        }),
        started_at: Timestamp(NOW - DAY),
        last_activity: Timestamp(NOW - MINUTE),
        title: Some(Arc::from(format!("title of {id}").as_str())),
        agent_version: None,
        logged_branch: None,
        open_turn: false,
        tail_pending: false,
        archived: false,
        inheritance_unknown: false,
        format: Format::Known,
    })
}

impl Sess {
    pub fn key(&self) -> SessionKey {
        self.0.key.clone()
    }
    pub fn title(mut self, title: &str) -> Sess {
        self.0.title = Some(Arc::from(title));
        self
    }
    pub fn untitled(mut self) -> Sess {
        self.0.title = None;
        self
    }
    pub fn agent(mut self, agent: Agent) -> Sess {
        self.0.agent = agent;
        self
    }
    /// The last activity `ms` before `NOW`.
    pub fn ago(mut self, ms: i64) -> Sess {
        self.0.last_activity = Timestamp(NOW - ms);
        self
    }
    pub fn open_turn(mut self) -> Sess {
        self.0.open_turn = true;
        self
    }
    pub fn pending(mut self) -> Sess {
        self.0.tail_pending = true;
        self
    }
    pub fn child_of(mut self, parent: &SessionKey) -> Sess {
        self.0.parent = Some(Parent {
            session: parent.clone(),
            tool_call: None,
        });
        self
    }
    /// Placed by `Projects::resolve` (the project was not open when it was
    /// placed) instead of by `locate`.
    pub fn resolved(mut self) -> Sess {
        if let Place::Located(location) = &self.0.place {
            self.0.place = Place::Resolved(Resolved {
                project: location.project.clone(),
                worktree: location.worktree.clone(),
                rel: location.rel.clone(),
            });
        }
        self
    }
    pub fn unplaced(mut self) -> Sess {
        self.0.place = Place::Pending;
        self
    }
    pub fn build(self) -> SessionSummary {
        self.0
    }
}

pub fn list(generation: u64, sessions: Vec<SessionSummary>) -> Arc<SessionList> {
    Arc::new(SessionList {
        generation,
        sessions: Arc::from(sessions),
        diagnostics: Default::default(),
    })
}

// ----------------------------------------------------------------- changes

/// The counts of one worktree, with the overlaps as they are given.
pub fn changes_of(
    worktree: &WorktreeKey,
    uncommitted: ChangeCount,
    committed: ChangeCount,
    vs_base: ChangeCount,
    overlaps: Option<Vec<Overlap>>,
) -> WorktreeChanges {
    WorktreeChanges {
        worktree: worktree.clone(),
        head: None,
        uncommitted,
        committed,
        vs_base,
        overlaps: overlaps.map(Arc::from),
        counted_at: Some(Timestamp(NOW - 2 * MINUTE)),
    }
}

/// A worktree whose counts are all `Files`.
pub fn counted(
    worktree: &WorktreeKey,
    uncommitted: u32,
    committed: u32,
    vs_base: u32,
    overlaps: Vec<Overlap>,
) -> WorktreeChanges {
    changes_of(
        worktree,
        ChangeCount::Files(uncommitted),
        ChangeCount::Files(committed),
        ChangeCount::Files(vs_base),
        Some(overlaps),
    )
}

pub fn overlap(other: &WorktreeKey, files: u32, paths: &[&str]) -> Overlap {
    let paths: Vec<RelPath> = paths.iter().map(|p| Arc::from(*p)).collect();
    Overlap {
        other: other.clone(),
        files,
        paths: Arc::from(paths),
    }
}

pub fn overview_of(
    project: &ProjectKey,
    base: Option<&str>,
    worktrees: Vec<WorktreeChanges>,
) -> WorktreesOverview {
    WorktreesOverview {
        project: project.clone(),
        generation: 5,
        base: base.map(Arc::from),
        worktrees: Arc::from(worktrees),
    }
}

// ------------------------------------------------------------------- usage

pub fn tokens(input: u64, output: u64) -> Tokens {
    Tokens {
        input,
        output,
        ..Tokens::default()
    }
}

pub fn header(state: UsageState) -> SnapshotHeader {
    SnapshotHeader {
        generation: 3,
        state,
        may_include_inherited: false,
    }
}

pub fn mr_ref(number: u64) -> MrRef {
    MrRef {
        repo: ForgeRepo {
            host: Arc::from("forge.example"),
            id: Arc::from("1"),
        },
        number,
    }
}

pub fn mr_label(number: u64, title: &str) -> MrLabel {
    MrLabel {
        mr: mr_ref(number),
        title: Arc::from(title),
        state: MrState::Open,
    }
}

/// The usage of a worktree's branch.
pub fn branch_usage(
    state: UsageState,
    cumulative: Option<Tokens>,
    mrs: Vec<MrLabel>,
) -> WorktreeUsage {
    WorktreeUsage {
        header: header(state),
        branch: None,
        mrs: Arc::from(mrs),
        cumulative,
        review: tokens(10, 5),
        since: Some(Timestamp(NOW - 3 * DAY)),
    }
}

pub fn worktree_snapshot(usage: WorktreeUsage) -> UsageSnapshot {
    UsageSnapshot::Worktree(usage)
}

pub fn mr_snapshot(
    number: u64,
    state: UsageState,
    write: Tokens,
    shared_with: Vec<u64>,
) -> UsageSnapshot {
    UsageSnapshot::Mr(MrUsage {
        header: header(state),
        mr: mr_label(number, "title"),
        start: Timestamp(NOW - 3 * DAY),
        end: None,
        write,
        review: Tokens::default(),
        shared_with: shared_with.into_iter().map(mr_ref).collect(),
        sessions: Arc::from(Vec::new()),
    })
}

pub fn worktree_query(project: &ProjectKey, worktree: &WorktreeKey) -> UsageQuery {
    UsageQuery::Worktree {
        project: project.clone(),
        worktree: worktree.clone(),
    }
}

/// The inputs of the `worktrees/build_1000` bench: 17 worktrees, 1000
/// sessions, 8 overlaps for each worktree and a branch usage for each.
pub struct Fixture1000 {
    pub project: ProjectSnapshot,
    pub lists: Vec<(SourceId, Arc<SessionList>)>,
    pub changes: WorktreesOverview,
    pub usage: UsageInputs,
}

impl Fixture1000 {
    /// The inputs of one `build`, at the fixture's clock.
    pub fn inputs(&self) -> Inputs<'_> {
        Inputs {
            project: &self.project,
            sessions: &self.lists,
            changes: Some(&self.changes),
            usage: Some(&self.usage),
            switches: 0,
            usage_generation: 0,
            now: Timestamp(NOW),
        }
    }
}

pub fn fixture_1000() -> Fixture1000 {
    let mut fx = ProjectFx::git("big");
    for i in 1..17 {
        fx = if i % 5 == 0 {
            fx.linked(
                &format!("w{i}"),
                Head::Detached(Arc::from("0123456789abcdef0123")),
            )
        } else {
            fx.linked(&format!("w{i}"), branch(&format!("feature/w{i}")))
        };
    }
    let key = fx.key();
    let project = fx.snapshot();
    let names: Vec<String> = std::iter::once("main".to_owned())
        .chain((1..17).map(|i| format!("w{i}")))
        .collect();
    let sessions: Vec<SessionSummary> = (0..1000)
        .map(|i| {
            let worktree = fx.wt(&names[i % 17]);
            let session = sess("a", &format!("session-{i}"), &key, Some(&worktree))
                .ago(((i % 5000) as i64 + 1) * MINUTE);
            if i % 40 == 0 {
                session.open_turn().ago(MINUTE).build()
            } else {
                session.build()
            }
        })
        .collect();
    let changes = overview_of(
        &key,
        Some("refs/remotes/origin/main"),
        names
            .iter()
            .enumerate()
            .map(|(i, name)| {
                let overlaps = (1..=8)
                    .map(|d| {
                        overlap(
                            &fx.wt(&names[(i + d) % 17]),
                            d as u32,
                            &["src/lib.rs", "src/main.rs", "Cargo.toml"],
                        )
                    })
                    .collect();
                counted(&fx.wt(name), 3, 5, 7, overlaps)
            })
            .collect(),
    );
    let usage = UsageInputs {
        worktrees: names
            .iter()
            .map(|name| {
                (
                    fx.wt(name),
                    Arc::new(worktree_snapshot(branch_usage(
                        UsageState::Ready,
                        Some(tokens(1_000, 400)),
                        vec![],
                    ))),
                )
            })
            .collect(),
        mrs: Default::default(),
    };
    Fixture1000 {
        project,
        lists: vec![(SourceId(Arc::from("a")), list(1, sessions))],
        changes,
        usage,
    }
}
