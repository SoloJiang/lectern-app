//! The fixture of `project_home/build_1000` (docs/design/project_home.md
//! 「性能」): a git project with 17 worktrees and 2000 sub-projects, 1000
//! sessions in it over the last 13 days, 50 of them in the overview. Only with
//! the `test-support` feature; `lectern-bench` builds it once and composes it
//! with [`snapshot::build`](crate::snapshot::build) in each sample.

use std::path::Path;
use std::sync::Arc;

use lectern_services::project::{
    ActiveTree, FileList, Head, Location, Package, PackageMatch, ProjectKey, ProjectSnapshot, Scan,
    Worktree, WorktreeKey,
};
use lectern_services::review::{OutsideCount, Overview, Progress, SessionOverview};
use lectern_services::session::{
    Agent, Format, Place, SessionKey, SessionList, SessionSummary, SourceId, Timestamp,
};

use crate::snapshot::Inputs;

/// The composing time: 2027-01-15 08:00 UTC, in milliseconds.
const NOW: i64 = 1_800_000_000_000;
const MINUTE: i64 = 60_000;
pub const WORKTREES: usize = 17;
pub const SESSIONS: usize = 1000;
pub const OVERVIEW: usize = 50;
/// Sub-projects besides the root.
pub const PACKAGES: usize = 2000;

/// The inputs of one build, owned.
pub struct BuildFixture {
    project: ProjectSnapshot,
    sessions: Vec<(SourceId, Arc<SessionList>)>,
    overview: Overview,
}

impl BuildFixture {
    pub fn new() -> BuildFixture {
        let common = "/fixture/app/.git";
        let key = ProjectKey::Git {
            common_dir: Arc::from(Path::new(common)),
        };
        let worktrees: Vec<Worktree> = (0..WORKTREES)
            .map(|i| {
                let main = i == 0;
                let dir = if main {
                    common.to_owned()
                } else {
                    format!("{common}/worktrees/w{i:02}")
                };
                Worktree {
                    key: WorktreeKey(Arc::from(Path::new(&dir))),
                    root: Arc::from(Path::new(&format!("/fixture/app-w{i:02}"))),
                    is_main: main,
                    head: Head::Branch(Arc::from(format!("refs/heads/b{i:02}"))),
                    locked: false,
                    prunable: false,
                }
            })
            .collect();
        let mut packages = vec![package("", "app", None)];
        for i in 0..PACKAGES / 2 {
            packages.push(package(&format!("pkg{i:04}"), "p", Some(0)));
            let parent = (packages.len() - 1) as u32;
            packages.push(package(&format!("pkg{i:04}/sub"), "sub", Some(parent)));
        }
        let main = worktrees[0].clone();
        let project = ProjectSnapshot {
            key: key.clone(),
            generation: 1,
            name: Arc::from("app"),
            active: Some(ActiveTree {
                root: main.root.clone(),
                worktree: Some(main.key.clone()),
                opened_at: Arc::from(""),
                files: Scan::Ready(Arc::new(FileList::builder().build())),
                packages: Scan::Ready(Arc::from(packages)),
                dirs: Scan::Pending,
            }),
            worktrees: Arc::from(worktrees.clone()),
        };
        // Newest first, every 18 minutes: the oldest is about 12.5 days old,
        // inside the 14 days the sidebar lists.
        let source = SourceId(Arc::from("session-claude-code"));
        let sessions: Vec<SessionSummary> = (0..SESSIONS)
            .map(|n| {
                let worktree = &worktrees[n % WORKTREES];
                let last = NOW - n as i64 * 18 * MINUTE;
                SessionSummary {
                    key: SessionKey {
                        source: source.clone(),
                        id: Arc::from(format!("s{n:04}")),
                    },
                    agent: Agent::ClaudeCode,
                    parent: None,
                    file: Arc::from(Path::new(&format!("/fixture/log/s{n:04}.jsonl"))),
                    cwd: worktree.root.clone(),
                    place: Place::Located(Location {
                        project: key.clone(),
                        worktree: Some(worktree.key.clone()),
                        rel: Arc::from(""),
                        package: PackageMatch::None,
                    }),
                    started_at: Timestamp(last - 30 * MINUTE),
                    last_activity: Timestamp(last),
                    title: Some(Arc::from(format!("Session {n} of the fixture"))),
                    agent_version: None,
                    logged_branch: None,
                    open_turn: n < 3,
                    tail_pending: false,
                    archived: false,
                    inheritance_unknown: false,
                    format: Format::Known,
                }
            })
            .collect();
        // The overview covers the newest sessions of the active worktree.
        let in_main = |s: &&SessionSummary| match &s.place {
            Place::Located(l) => l.worktree.as_ref() == Some(&main.key),
            _ => false,
        };
        let overview = Overview {
            project: key,
            generation: 7,
            sessions: sessions
                .iter()
                .filter(in_main)
                .take(OVERVIEW)
                .enumerate()
                .map(|(n, s)| SessionOverview {
                    session: s.key.clone(),
                    progress: Some(Progress {
                        total: 6,
                        seen: (n % 7) as u32,
                        changed_since_seen: u32::from(n % 5 == 0),
                    }),
                    drifted: Some(n % 9 == 0),
                    critical: (n % 4) as u32,
                })
                .collect(),
            outside: OutsideCount::Files(12),
            packages: None,
        };
        let list = Arc::new(SessionList {
            generation: 3,
            sessions: Arc::from(sessions),
            diagnostics: Default::default(),
        });
        BuildFixture {
            project,
            sessions: vec![(source, list)],
            overview,
        }
    }

    /// The inputs of `snapshot::build`, without usage.
    pub fn inputs(&self) -> Inputs<'_> {
        Inputs {
            project: &self.project,
            sessions: &self.sessions,
            overview: Some(&self.overview),
            changed_today: None,
            usage: None,
            now: Timestamp(NOW),
        }
    }
}

impl Default for BuildFixture {
    fn default() -> BuildFixture {
        BuildFixture::new()
    }
}

fn package(dir: &str, name: &str, parent: Option<u32>) -> Package {
    Package {
        dir: Arc::from(dir),
        name: Arc::from(name),
        manifests: Arc::from(Vec::new()),
        declared: true,
        workspace_root: false,
        parent,
        files: 1,
    }
}
