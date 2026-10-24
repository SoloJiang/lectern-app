//! The discovered folders (docs/design/welcome.md「发现的文件夹」): project
//! roots that agent sessions were run in and that are neither open nor in
//! the recent list. A pure function over the session lists the session
//! sources already published; nothing here reads a directory.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lectern_services::project::{ProjectKey, Resolved};
use lectern_services::session::{Agent, Place, SessionList, SessionSummary, Timestamp};

/// The most folders the list keeps.
pub(crate) const MAX_FOLDERS: usize = 50;
/// The most agents shown on one folder.
pub(crate) const MAX_AGENTS: usize = 4;
/// A session with an open turn counts as running when its last activity is
/// at most this old.
pub(crate) const RUNNING_WINDOW_MS: i64 = 10 * 60 * 1000;

/// A project root found through its sessions.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DiscoveredFolder {
    /// The project root: the newest session's cwd without the `Resolved::rel`
    /// suffix (string work, no IO); the folder itself for a
    /// `ProjectKey::Folder`.
    pub path: Arc<Path>,
    /// The last component of `path`.
    pub name: Arc<str>,
    pub key: ProjectKey,
    /// The agents seen in this project, in order of first appearance, at most
    /// [`MAX_AGENTS`].
    pub agents: Arc<[Agent]>,
    pub sessions: u32,
    /// `open_turn` and `last_activity` within [`RUNNING_WINDOW_MS`] of now.
    pub running: u32,
    /// The largest `last_activity` of the sessions.
    pub newest_activity: Timestamp,
}

/// What [`build`] gives.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Built {
    /// Newest activity first, at most [`MAX_FOLDERS`].
    pub folders: Vec<DiscoveredFolder>,
    /// `(sessions, running)` of every recent project, by its key. A recent
    /// project without sessions is not in the map.
    pub counts: HashMap<ProjectKey, (u32, u32)>,
    /// The sessions looked at, before grouping.
    pub total: usize,
    /// Sessions skipped because their place is not known yet (`Pending`,
    /// `Missing`, `Unresolvable`).
    pub skipped: usize,
}

struct Group {
    /// The root, from the newest session of the project.
    root: PathBuf,
    agents: Vec<Agent>,
    sessions: u32,
    running: u32,
    newest: i64,
}

/// Groups the sessions of every source by project.
///
/// - Only `Place::Resolved` sessions make a folder: the project is not open.
///   A project in `open_projects` or with a key in `recent_keys` is left out
///   (any worktree of the repository counts as the repository).
/// - `Resolved` and `Located` sessions of a project in `recent_keys` give
///   that recent project's `counts`.
/// - A sub-agent session counts as a session and never as running.
pub(crate) fn build(
    lists: &[Arc<SessionList>],
    open_projects: &[ProjectKey],
    recent_keys: &[ProjectKey],
    now: Timestamp,
) -> Built {
    let open: HashSet<&ProjectKey> = open_projects.iter().collect();
    let recent: HashSet<&ProjectKey> = recent_keys.iter().collect();
    let mut sessions: Vec<&SessionSummary> = lists.iter().flat_map(|l| l.sessions.iter()).collect();
    // Newest first across sources; stable, so the sources keep their order
    // among equal times.
    sessions.sort_by_key(|s| std::cmp::Reverse(s.last_activity));

    let mut built = Built {
        total: sessions.len(),
        ..Built::default()
    };
    let mut groups: HashMap<&ProjectKey, Group> = HashMap::new();
    for s in sessions {
        let running = s.parent.is_none() && is_running(s, now);
        let (project, resolved) = match &s.place {
            Place::Resolved(r) => (&r.project, Some(r)),
            Place::Located(l) => (&l.project, None),
            Place::Pending | Place::Missing | Place::Unresolvable(_) => {
                built.skipped += 1;
                continue;
            }
        };
        if recent.contains(project) {
            let counts = built.counts.entry(project.clone()).or_default();
            counts.0 += 1;
            counts.1 += u32::from(running);
            continue;
        }
        let Some(resolved) = resolved else { continue };
        if open.contains(project) {
            continue;
        }
        let group = groups.entry(project).or_insert_with(|| Group {
            root: root_of(&s.cwd, resolved),
            agents: Vec::new(),
            sessions: 0,
            running: 0,
            newest: s.last_activity.0,
        });
        if group.agents.len() < MAX_AGENTS && !group.agents.contains(&s.agent) {
            group.agents.push(s.agent.clone());
        }
        group.sessions += 1;
        group.running += u32::from(running);
    }

    let mut folders: Vec<DiscoveredFolder> = groups
        .into_iter()
        .map(|(key, g)| {
            let path: Arc<Path> = Arc::from(g.root);
            DiscoveredFolder {
                name: name_of(&path),
                path,
                key: key.clone(),
                agents: g.agents.into(),
                sessions: g.sessions,
                running: g.running,
                newest_activity: Timestamp(g.newest),
            }
        })
        .collect();
    folders.sort_by(|a, b| {
        b.newest_activity
            .cmp(&a.newest_activity)
            .then_with(|| a.key.cmp(&b.key))
    });
    folders.truncate(MAX_FOLDERS);
    built.folders = folders;
    built
}

fn is_running(session: &SessionSummary, now: Timestamp) -> bool {
    session.open_turn && now.0.saturating_sub(session.last_activity.0) <= RUNNING_WINDOW_MS
}

/// The project root a session ran under: a folder project is its own root;
/// for a repository, the cwd without the `rel` components at its end (the cwd
/// itself when it does not end with them).
pub(crate) fn root_of(cwd: &Path, resolved: &Resolved) -> PathBuf {
    if let ProjectKey::Folder { root } = &resolved.project {
        return root.to_path_buf();
    }
    if resolved.rel.is_empty() {
        return cwd.to_path_buf();
    }
    let rel = Path::new(&*resolved.rel);
    if !cwd.ends_with(rel) {
        return cwd.to_path_buf();
    }
    let depth = rel.components().count();
    cwd.ancestors()
        .nth(depth)
        .map_or_else(|| cwd.to_path_buf(), Path::to_path_buf)
}

fn name_of(path: &Path) -> Arc<str> {
    match path.file_name() {
        Some(name) => Arc::from(name.to_string_lossy().as_ref()),
        None => Arc::from(path.to_string_lossy().as_ref()),
    }
}
