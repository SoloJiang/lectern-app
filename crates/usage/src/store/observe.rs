//! The observations a turn is settled with (docs/design/usage.md「分支观察」
//! 「轮次的观察与改动项」): where HEAD stayed during the calls of the turn,
//! the segment each changed file belongs to, and what the turn says about the
//! worktree it ran in. They are computed once, inside the settle transaction
//! (the design allows "或定稿时补算"; settling waits two seconds after the
//! last result, so the reflog records of the calls have been read), and
//! never changed afterwards: a replay of the session log, a rebuild of the
//! derived tables and a branch deleted later all find them as they were.
//!
//! A turn is tracked, in the design's words, when the repository was watched
//! around a call: the watch spans cover the second before its start to the
//! two seconds after its result. Other calls are imported: the file's
//! branch is the one the session log named, when the file is in the
//! worktree the session ran in.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, params};

use super::calls::KIND_EXECUTE;
use super::ingest::{Place, intern, location, path_bytes};
use super::repo::{ACT_COMMIT, ACT_DETACH, ACT_SWITCH, FILE_HEAD};
use super::segments;
use crate::command::{GitFact, bits};
use crate::gitread::{BranchLog, GitRead};
use crate::observe::{Head, Observed, Stay, Switch, replay};

/// `call_fact.state`: what the observation of a changed file decided.
pub(crate) const STATE_SET: i64 = 1;
/// The file's branch is the default branch.
pub(crate) const STATE_DEFAULT: i64 = 2;
/// The branch could not be told (no observation, or an unsure one).
pub(crate) const STATE_UNATTRIBUTABLE: i64 = 4;
/// The file is in no worktree: it belongs to no branch.
pub(crate) const STATE_NO_BRANCH: i64 = 8;

/// `turn_obs.flags`.
pub(crate) const OBS_TRACKED: i64 = 1;
pub(crate) const OBS_UNSURE: i64 = 2;
pub(crate) const OBS_SWITCHED: i64 = 4;

/// `call_obs.flags`.
const CALL_UNSURE: i64 = 1;
const CALL_SWITCHED: i64 = 2;

/// A worktree and the repository it belongs to.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct WtRef {
    /// The worktree's key: its administrative directory.
    pub(crate) key: PathBuf,
    /// The repository's common directory.
    pub(crate) common: PathBuf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CallKind {
    Execute,
    Read,
    Search,
    /// A completed `Edit` call that left no change.
    Edit,
}

/// What a command that mentions git or goes into a directory did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Command {
    pub(crate) mask: i64,
    pub(crate) facts: Vec<GitFact>,
}

/// A call of the turn that says where the session was (docs/design/usage.md
/// 「分摊」 rule 2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CallWork {
    /// The key of the call (`calls::hash`).
    pub(crate) cid: i64,
    pub(crate) kind: CallKind,
    /// Start and result times, milliseconds.
    pub(crate) start: i64,
    pub(crate) end: i64,
    /// Where an `Execute` ran.
    pub(crate) workdir: Option<WtRef>,
    /// The worktrees it went into, or the files it read, searched or wrote
    /// are in; each once.
    pub(crate) targets: Vec<WtRef>,
    /// An `Execute` that mentions git or goes into a directory.
    pub(crate) command: Option<Command>,
}

/// The turn as stored.
pub(crate) struct TurnInfo {
    pub(crate) id: i64,
    pub(crate) session: i64,
    pub(crate) generation: i64,
    pub(crate) tid: i64,
    pub(crate) started_at: i64,
    /// The turn's last moment (L).
    pub(crate) last_at: i64,
    /// Where the session ran: the worktree (an `intern` ID) and the
    /// repository's location.
    pub(crate) worktree: Option<i64>,
    pub(crate) location: Option<i64>,
    /// The branch the session log named, as a full reference name.
    pub(crate) branch: Option<String>,
}

/// A changed file to place.
pub(crate) struct ItemIn<'a> {
    pub(crate) cid: i64,
    pub(crate) path: &'a Path,
    pub(crate) start: i64,
    pub(crate) end: i64,
    pub(crate) wt: Option<&'a WtRef>,
    /// The file's path below its worktree's root, slash separated.
    pub(crate) rel: &'a str,
}

/// What an item's call_fact row is after observation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Placed {
    /// The `call_fact` row.
    pub(crate) cf: i64,
}

/// One HEAD reflog record around a call.
#[derive(Clone, Debug)]
struct Event {
    lo: i64,
    hi: i64,
    act: i64,
    to: Option<String>,
    old: Option<Vec<u8>>,
    new: Option<Vec<u8>>,
}

/// The HEADs a worktree was read to have when a watch began: when, and the
/// branch (None when detached).
type Baselines = Vec<(i64, Option<String>)>;

/// The reads of one transaction, remembered: spans, baselines, records,
/// logs and IDs.
pub(crate) struct Cx<'a> {
    conn: &'a Connection,
    git: &'a dyn GitRead,
    repos: HashMap<PathBuf, i64>,
    commons: HashMap<i64, PathBuf>,
    wts: HashMap<PathBuf, i64>,
    spans: HashMap<i64, Vec<(i64, Option<i64>)>>,
    baselines: HashMap<(i64, i64), Baselines>,
    events: HashMap<(i64, i64, i64), Vec<Event>>,
    gaps: HashMap<(i64, i64), Vec<i64>>,
    logs: HashMap<(i64, String), Option<BranchLog>>,
    defaults: HashMap<i64, Option<String>>,
    /// The paths of the commits asked for, by repository and new commit.
    commits: HashMap<(i64, Vec<u8>), Option<Vec<String>>>,
}

const SECOND: i64 = 1_000_000_000;

impl<'a> Cx<'a> {
    pub(crate) fn new(conn: &'a Connection, git: &'a dyn GitRead) -> Cx<'a> {
        Cx {
            conn,
            git,
            repos: HashMap::new(),
            commons: HashMap::new(),
            wts: HashMap::new(),
            spans: HashMap::new(),
            baselines: HashMap::new(),
            events: HashMap::new(),
            gaps: HashMap::new(),
            logs: HashMap::new(),
            defaults: HashMap::new(),
            commits: HashMap::new(),
        }
    }

    fn repo(&mut self, common: &Path) -> rusqlite::Result<i64> {
        if let Some(&id) = self.repos.get(common) {
            return Ok(id);
        }
        let id = location(self.conn, &Place::Git(common.to_path_buf()))?;
        self.repos.insert(common.to_path_buf(), id);
        self.commons.insert(id, common.to_path_buf());
        Ok(id)
    }

    fn wt(&mut self, key: &Path) -> rusqlite::Result<i64> {
        if let Some(&id) = self.wts.get(key) {
            return Ok(id);
        }
        let id = intern(self.conn, path_bytes(key))?;
        self.wts.insert(key.to_path_buf(), id);
        Ok(id)
    }

    fn common(&mut self, repo: i64) -> rusqlite::Result<Option<PathBuf>> {
        if let Some(c) = self.commons.get(&repo) {
            return Ok(Some(c.clone()));
        }
        let path: Option<Vec<u8>> = self
            .conn
            .prepare_cached("SELECT path FROM location WHERE id = ?1 AND kind = 0")?
            .query_row(params![repo], |r| r.get(0))
            .optional()?;
        Ok(path.map(|p| {
            use std::os::unix::ffi::OsStringExt;
            let path = PathBuf::from(std::ffi::OsString::from_vec(p));
            self.commons.insert(repo, path.clone());
            path
        }))
    }

    /// The span of the repository's watch that covers a call, if any: the
    /// second before its start to the two after its result.
    fn span(&mut self, repo: i64, start: i64, end: i64) -> rusqlite::Result<Option<i64>> {
        if !self.spans.contains_key(&repo) {
            let rows: Vec<(i64, Option<i64>)> = self
                .conn
                .prepare_cached(
                    "SELECT begin_ns, end_ns FROM watch_span WHERE repo = ?1 ORDER BY begin_ns",
                )?
                .query_map(params![repo], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?;
            self.spans.insert(repo, rows);
        }
        let (s, t) = (start * 1_000_000, end * 1_000_000);
        Ok(self.spans[&repo]
            .iter()
            .find(|(b, e)| *b <= s - SECOND && e.is_none_or(|e| e >= t + 2 * SECOND))
            .map(|(b, _)| *b))
    }

    /// Where HEAD of a worktree stayed from `start` to `end` (milliseconds);
    /// None when the repository was not watched around then.
    pub(crate) fn observe(
        &mut self,
        repo: i64,
        wt: i64,
        start: i64,
        end: i64,
    ) -> rusqlite::Result<Option<Observed>> {
        let Some(begin) = self.span(repo, start, end)? else {
            return Ok(None);
        };
        let (s, t) = (start * 1_000_000, end * 1_000_000);
        if !self.baselines.contains_key(&(repo, wt)) {
            let rows: Vec<(i64, Option<String>)> = self
                .conn
                .prepare_cached(
                    "SELECT at_ns, branch FROM head_baseline WHERE repo = ?1 AND wt = ?2
                     ORDER BY at_ns",
                )?
                .query_map(params![repo, wt], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?;
            self.baselines.insert((repo, wt), rows);
        }
        let Some((base_at, base)) = self.baselines[&(repo, wt)]
            .iter()
            .rfind(|(at, _)| *at >= begin && *at <= s)
            .cloned()
        else {
            return Ok(None);
        };
        let events = self.events(repo, wt, base_at)?;
        let switches = switches_of(events);
        let gap = self.gap(repo, wt, base_at, s, t)?;
        let head = match base {
            Some(name) => Head::Branch(name),
            None => Head::Detached,
        };
        Ok(Some(replay(&head, &switches, s, t, gap)))
    }

    fn events(&mut self, repo: i64, wt: i64, base_at: i64) -> rusqlite::Result<&Vec<Event>> {
        let key = (repo, wt, base_at);
        if !self.events.contains_key(&key) {
            let rows: Vec<Event> = self
                .conn
                .prepare_cached(
                    "SELECT act, b, at, obs_ns, aft_ns, old, new FROM reflog
                     WHERE repo = ?1 AND file = ?2 AND wt = ?3 AND obs_ns > ?4
                     ORDER BY run, seq",
                )?
                .query_map(params![repo, FILE_HEAD, wt, base_at], |r| {
                    let at: i64 = r.get::<_, i64>(2)?.saturating_mul(1_000_000);
                    let observed: i64 = r.get(3)?;
                    let after: Option<i64> = r.get(4)?;
                    Ok(Event {
                        act: r.get(0)?,
                        to: r.get(1)?,
                        lo: after.map_or(at, |a| a.max(at)),
                        hi: observed.min(at.saturating_add(SECOND)),
                        old: r.get(5)?,
                        new: r.get(6)?,
                    })
                })?
                .collect::<rusqlite::Result<_>>()?;
            self.events.insert(key, rows);
        }
        Ok(&self.events[&key])
    }

    /// Reading the HEAD reflog failed or was cut short around the call.
    fn gap(&mut self, repo: i64, wt: i64, base_at: i64, s: i64, t: i64) -> rusqlite::Result<bool> {
        if !self.gaps.contains_key(&(repo, wt)) {
            let rows: Vec<i64> = self
                .conn
                .prepare_cached(
                    "SELECT obs_ns FROM reflog_reset WHERE repo = ?1 AND file = ?2 AND wt = ?3
                       AND reason != 2 ORDER BY obs_ns",
                )?
                .query_map(params![repo, FILE_HEAD, wt], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            self.gaps.insert((repo, wt), rows);
        }
        Ok(self.gaps[&(repo, wt)]
            .iter()
            .any(|&g| g > base_at && g >= s - SECOND && g <= t + 2 * SECOND))
    }

    fn log(&mut self, repo: i64, name: &str) -> rusqlite::Result<Option<BranchLog>> {
        let key = (repo, name.to_owned());
        if let Some(found) = self.logs.get(&key) {
            return Ok(found.clone());
        }
        let found = match self.common(repo)? {
            Some(common) => self.git.branch_log(&common, name),
            None => None,
        };
        self.logs.insert(key, found.clone());
        Ok(found)
    }

    /// The segment a head was in at `at` (milliseconds).
    pub(crate) fn segment(
        &mut self,
        repo: i64,
        wt: i64,
        head: &Head,
        at: i64,
    ) -> rusqlite::Result<i64> {
        match head {
            Head::Detached => segments::detached_now(self.conn, repo, wt),
            Head::Branch(name) => {
                if let Some(f) = segments::lookup(self.conn, repo, name, at)? {
                    return Ok(f.id);
                }
                let log = self.log(repo, name)?;
                segments::resolve(self.conn, repo, name, at, log.as_ref())
            }
        }
    }

    /// The paths a commit changed, read once.
    fn commit_paths(
        &mut self,
        repo: i64,
        old: Option<&[u8]>,
        new: &[u8],
    ) -> rusqlite::Result<Option<Vec<String>>> {
        let key = (repo, new.to_vec());
        if let Some(found) = self.commits.get(&key) {
            return Ok(found.clone());
        }
        let found = match self.common(repo)? {
            Some(common) => self.git.commit_paths(&common, old, new),
            None => None,
        };
        self.commits.insert(key, found.clone());
        Ok(found)
    }

    /// Whether `head` is the repository's default branch.
    pub(crate) fn is_default(&mut self, repo: i64, head: &Head) -> rusqlite::Result<bool> {
        let Head::Branch(name) = head else {
            return Ok(false);
        };
        if !self.defaults.contains_key(&repo) {
            let found = match self.common(repo)? {
                Some(common) => self.git.default_branch(&common),
                None => None,
            };
            self.defaults.insert(repo, found);
        }
        Ok(self.defaults[&repo].as_deref() == Some(name.as_str()))
    }
}

fn switches_of(events: &[Event]) -> Vec<Switch> {
    events
        .iter()
        .filter_map(|e| match (e.act, &e.to) {
            (ACT_SWITCH, Some(to)) => Some(Switch {
                lo: e.lo,
                hi: e.hi,
                to: Head::Branch(to.clone()),
            }),
            (ACT_DETACH, _) => Some(Switch {
                lo: e.lo,
                hi: e.hi,
                to: Head::Detached,
            }),
            _ => None,
        })
        .collect()
}

/// The commits a worktree's HEAD made during a call, each with the head it
/// was on.
fn commits_during(events: &[Event], baseline: &Head, start: i64, end: i64) -> Vec<(Head, Event)> {
    let mut state = baseline.clone();
    let mut out = Vec::new();
    for e in events {
        match e.act {
            ACT_SWITCH => {
                if let Some(to) = &e.to {
                    state = Head::Branch(to.clone());
                }
            }
            ACT_DETACH => state = Head::Detached,
            ACT_COMMIT if e.lo > start && e.hi < end => out.push((state.clone(), e.clone())),
            _ => {}
        }
    }
    out
}

fn stay_segment(cx: &mut Cx<'_>, repo: i64, wt: i64, stay: &Stay) -> rusqlite::Result<(i64, bool)> {
    let at_ms = stay.at / 1_000_000;
    let id = cx.segment(repo, wt, &stay.head, at_ms)?;
    let default = cx.is_default(repo, &stay.head)?;
    if default {
        segments::mark_default(cx.conn, id)?;
    }
    Ok((id, default))
}

fn put_pairs(pairs: &[(i64, i64)]) -> Option<Vec<u8>> {
    if pairs.is_empty() {
        return None;
    }
    let mut out = Vec::with_capacity(pairs.len() * 16);
    for (a, b) in pairs {
        out.extend_from_slice(&a.to_le_bytes());
        out.extend_from_slice(&b.to_le_bytes());
    }
    Some(out)
}

pub(crate) fn get_pairs(bytes: &[u8]) -> Vec<(i64, i64)> {
    bytes
        .chunks_exact(16)
        .map(|c| {
            (
                i64::from_le_bytes(c[..8].try_into().unwrap_or([0; 8])),
                i64::from_le_bytes(c[8..].try_into().unwrap_or([0; 8])),
            )
        })
        .collect()
}

fn put_ids(ids: &[i64]) -> Option<Vec<u8>> {
    if ids.is_empty() {
        return None;
    }
    Some(ids.iter().flat_map(|i| i.to_le_bytes()).collect())
}

pub(crate) fn get_ids(bytes: &[u8]) -> Vec<i64> {
    bytes
        .chunks_exact(8)
        .map(|c| i64::from_le_bytes(c.try_into().unwrap_or([0; 8])))
        .collect()
}

/// A branch name as the log gave it, as a full reference name.
pub(crate) fn full_ref(name: &str) -> String {
    if name.starts_with("refs/") {
        name.to_owned()
    } else {
        format!("refs/heads/{name}")
    }
}

/// What a command's observation of one worktree was.
struct ExecObs {
    wt: i64,
    repo: i64,
    obs: Observed,
}

/// The state of the changed files of a turn and what the turn observed:
/// writes `call_fact.segment/state` of the items, the `call` rows of the
/// commands that mention git with their `call_obs`, and the turn's
/// `turn_obs`; returns the `call_fact` row of each item.
pub(crate) fn settle_turn(
    cx: &mut Cx<'_>,
    turn: &TurnInfo,
    items: &[ItemIn<'_>],
    calls: &[CallWork],
    places: &[(i64, Option<i64>)],
) -> rusqlite::Result<Vec<Placed>> {
    let conn = cx.conn;
    let fixed: bool = conn
        .prepare_cached("SELECT EXISTS (SELECT 1 FROM turn_obs WHERE session = ?1 AND tid = ?2)")?
        .query_row(params![turn.session, turn.tid], |r| r.get(0))?;

    // Commands that mention git: what they did to HEAD, per worktree.
    let mut execs: Vec<(usize, Vec<ExecObs>)> = Vec::new();
    for (n, call) in calls.iter().enumerate() {
        let Some(command) = &call.command else {
            continue;
        };
        if fixed {
            // Settled before: the observations are as they were.
            if keeps_row(conn, turn.session, call, command)? {
                write_call(conn, turn, call, command)?;
            }
            continue;
        }
        let mut touched: Vec<&WtRef> = Vec::new();
        for w in call.workdir.iter().chain(call.targets.iter()) {
            if !touched.contains(&w) {
                touched.push(w);
            }
        }
        let mut seen = Vec::new();
        for w in touched {
            let repo = cx.repo(&w.common)?;
            let wt = cx.wt(&w.key)?;
            if let Some(obs) = cx.observe(repo, wt, call.start, call.end)? {
                write_call_obs(cx, turn.session, call, (repo, wt), &obs)?;
                seen.push(ExecObs { wt, repo, obs });
            }
        }
        // A command that did nothing the rules look for (it only looked at
        // the repository) has no row: its text is gone, and a row of
        // nothing would be one of the most common of the table.
        let moved = seen
            .iter()
            .any(|e: &ExecObs| !e.obs.certain || e.obs.switched());
        if moved || has_facts(call, command) {
            write_call(conn, turn, call, command)?;
        }
        execs.push((n, seen));
    }

    // The changed files.
    let mut placed = Vec::with_capacity(items.len());
    for (n, item) in items.iter().enumerate() {
        let (location, worktree) = places[n];
        let path_id = intern(conn, path_bytes(item.path))?;
        let row: Option<(i64, i64)> = conn
            .prepare_cached(
                "SELECT id, state FROM call_fact WHERE session = ?1 AND cid = ?2 AND path = ?3",
            )?
            .query_row(params![turn.session, item.cid, path_id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()?;
        let cf = match row {
            Some((id, state)) if state != 0 => {
                placed.push(Placed { cf: id });
                continue;
            }
            Some((id, _)) => id,
            None => {
                conn.prepare_cached(
                    "INSERT INTO call_fact (session, cid, path, location, worktree, at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                )?
                .execute(params![
                    turn.session,
                    item.cid,
                    path_id,
                    location,
                    worktree,
                    turn.started_at
                ])?;
                conn.last_insert_rowid()
            }
        };
        let (segment, state) = place_item(cx, turn, item, &execs, calls)?;
        conn.prepare_cached("UPDATE call_fact SET segment = ?2, state = ?3 WHERE id = ?1")?
            .execute(params![cf, segment, state])?;
        placed.push(Placed { cf });
    }
    if fixed {
        return Ok(placed);
    }

    // What the turn says about the worktree it ran in, and the worktrees
    // its calls went into.
    let mut flags = 0;
    let mut start_seg = None;
    let mut end_seg = None;
    let mut stays: Vec<i64> = Vec::new();
    let mut enter: Vec<(i64, i64)> = Vec::new();
    if let (Some(wt), Some(repo)) = (turn.worktree, turn.location) {
        let begin = cx.observe(repo, wt, turn.started_at, turn.started_at)?;
        let last = cx.observe(repo, wt, turn.last_at, turn.last_at)?;
        if let Some(o) = &begin
            && let Some(stay) = o.end()
        {
            start_seg = Some(stay_segment(cx, repo, wt, stay)?.0);
        }
        if let Some(o) = &last {
            flags |= OBS_TRACKED;
            if !o.certain {
                flags |= OBS_UNSURE;
            }
            if let Some(stay) = o.end()
                && o.certain
            {
                end_seg = Some(stay_segment(cx, repo, wt, stay)?.0);
            }
        } else if let Some(name) = &turn.branch {
            // Imported: the branch the log named.
            let head = Head::Branch(name.clone());
            let seg = cx.segment(repo, wt, &head, turn.started_at)?;
            if cx.is_default(repo, &head)? {
                segments::mark_default(cx.conn, seg)?;
            }
            start_seg = Some(seg);
            end_seg = Some(seg);
        }
        // Stays of commands in this worktree that switched branches.
        for (_, seen) in &execs {
            for e in seen.iter().filter(|e| e.wt == wt && e.repo == repo) {
                if e.obs.switched() {
                    flags |= OBS_SWITCHED;
                }
                if e.obs.certain {
                    for stay in &e.obs.stays {
                        let seg = stay_segment(cx, repo, wt, stay)?.0;
                        if !stays.contains(&seg) {
                            stays.push(seg);
                        }
                    }
                }
            }
        }
        if let (Some(a), Some(b)) = (start_seg, end_seg)
            && a != b
            && flags & OBS_TRACKED != 0
        {
            flags |= OBS_SWITCHED;
        }
        stays.retain(|s| Some(*s) != end_seg);
    }
    for (n, call) in calls.iter().enumerate() {
        let seen = execs.iter().find(|(m, _)| *m == n).map(|(_, seen)| seen);
        for w in &call.targets {
            let (repo, wt) = (cx.repo(&w.common)?, cx.wt(&w.key)?);
            let known = seen
                .and_then(|s| s.iter().find(|e| e.wt == wt && e.repo == repo))
                .map(|e| e.obs.clone());
            let obs = match known {
                Some(o) => Some(o),
                None => cx.observe(repo, wt, call.start, call.end)?,
            };
            if let Some(o) = obs
                && o.certain
                && let Some(stay) = o.end()
            {
                let seg = stay_segment(cx, repo, wt, stay)?.0;
                if !enter.contains(&(repo, seg)) {
                    enter.push((repo, seg));
                }
            }
        }
    }
    // A turn that says nothing (nobody watched it, the log named no branch)
    // has no row: settling it again finds the same.
    if flags == 0
        && start_seg.is_none()
        && end_seg.is_none()
        && stays.is_empty()
        && enter.is_empty()
    {
        return Ok(placed);
    }
    conn.prepare_cached(
        "INSERT OR IGNORE INTO turn_obs (session, tid, at, flags, start_seg, end_seg, stays, enter)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
    )?
    .execute(params![
        turn.session,
        turn.tid,
        turn.last_at,
        flags,
        start_seg,
        end_seg,
        put_ids(&stays),
        put_pairs(&enter)
    ])?;
    Ok(placed)
}

/// What a command says beyond "git ran": a stash, a push, a cherry-pick, a
/// rewrite, a reset, a new branch, or a directory it went into.
fn has_facts(call: &CallWork, command: &Command) -> bool {
    !command.facts.is_empty() || !call.targets.is_empty() || command.mask & bits::UNSURE != 0
}

/// Whether the first settling kept a row for the command: it had facts, or
/// the observation of it was kept (`call_obs`).
fn keeps_row(
    conn: &Connection,
    session: i64,
    call: &CallWork,
    command: &Command,
) -> rusqlite::Result<bool> {
    if has_facts(call, command) {
        return Ok(true);
    }
    conn.prepare_cached("SELECT EXISTS (SELECT 1 FROM call_obs WHERE session = ?1 AND cid = ?2)")?
        .query_row(params![session, call.cid], |r| r.get(0))
}

fn write_call(
    conn: &Connection,
    turn: &TurnInfo,
    call: &CallWork,
    command: &Command,
) -> rusqlite::Result<()> {
    let facts = if command.facts.is_empty() {
        None
    } else {
        serde_json::to_vec(&command.facts).ok()
    };
    let wt = match &call.workdir {
        Some(w) => Some(intern(conn, path_bytes(&w.key))?),
        None => None,
    };
    conn.prepare_cached(
        "INSERT OR IGNORE INTO call (session, gen, cid, turn, started_at, finished_at, kind,
                                     status, wt, git, facts)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, ?8, ?9, ?10)",
    )?
    .execute(params![
        turn.session,
        turn.generation,
        call.cid,
        turn.id,
        call.start,
        call.end,
        KIND_EXECUTE,
        wt,
        command.mask,
        facts
    ])?;
    Ok(())
}

fn write_call_obs(
    cx: &mut Cx<'_>,
    session: i64,
    call: &CallWork,
    (repo, wt): (i64, i64),
    obs: &Observed,
) -> rusqlite::Result<()> {
    // A command that stayed on one branch for sure says what the turn's own
    // observation says: no row. One that moved HEAD, or could not be told,
    // is kept.
    if obs.certain && !obs.switched() {
        return Ok(());
    }
    let mut ids = Vec::new();
    for stay in &obs.stays {
        ids.push(stay_segment(cx, repo, wt, stay)?.0);
    }
    let mut flags = 0;
    if !obs.certain {
        flags |= CALL_UNSURE;
    }
    if obs.switched() {
        flags |= CALL_SWITCHED;
    }
    let end = if obs.certain {
        ids.last().copied()
    } else {
        None
    };
    cx.conn
        .prepare_cached(
            "INSERT OR IGNORE INTO call_obs (session, cid, wt, at, flags, end_seg, stays)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?
        .execute(params![
            session,
            call.cid,
            wt,
            call.start,
            flags,
            end,
            if ids.len() > 1 { put_ids(&ids) } else { None }
        ])?;
    Ok(())
}

/// The segment and state of a changed file (docs/design/usage.md
/// 「轮次的观察与改动项」: 分支、去向、是否为默认分支).
fn place_item(
    cx: &mut Cx<'_>,
    turn: &TurnInfo,
    item: &ItemIn<'_>,
    execs: &[(usize, Vec<ExecObs>)],
    calls: &[CallWork],
) -> rusqlite::Result<(Option<i64>, i64)> {
    let Some(w) = item.wt else {
        return Ok((None, STATE_SET | STATE_NO_BRANCH));
    };
    let repo = cx.repo(&w.common)?;
    let wt = cx.wt(&w.key)?;
    let unsure = (None, STATE_SET | STATE_UNATTRIBUTABLE);
    let Some(obs) = cx.observe(repo, wt, item.start, item.end)? else {
        // Imported: the log's branch, for a file in the worktree the
        // session ran in.
        if turn.worktree == Some(wt)
            && let Some(name) = &turn.branch
        {
            let head = Head::Branch(name.clone());
            let seg = cx.segment(repo, wt, &head, turn.started_at)?;
            let default = cx.is_default(repo, &head)?;
            if default {
                segments::mark_default(cx.conn, seg)?;
            }
            return Ok((
                Some(seg),
                STATE_SET | if default { STATE_DEFAULT } else { 0 },
            ));
        }
        return Ok(unsure);
    };
    let (Some(stay), true, false) = (obs.end(), obs.certain, obs.switched()) else {
        return Ok(unsure);
    };
    let (mut segment, mut default) = stay_segment(cx, repo, wt, stay)?;
    // The file goes where the content goes: a later command of the turn in
    // this worktree that switched branches takes it to the commit that holds
    // it, else to the branch HEAD ended on.
    let later = execs.iter().find(|(n, seen)| {
        calls[*n].start >= item.end
            && seen
                .iter()
                .any(|e| e.wt == wt && e.repo == repo && e.obs.certain && e.obs.switched())
    });
    if let Some((n, seen)) = later
        && let Some(e) = seen.iter().find(|e| e.wt == wt && e.repo == repo)
    {
        let call = &calls[*n];
        let mut moved: Option<(Head, i64)> = None;
        {
            // The commits made during the command that changed the file.
            let begin = cx
                .baselines
                .get(&(repo, wt))
                .and_then(|b| b.iter().rfind(|(at, _)| *at <= call.start * 1_000_000))
                .cloned();
            if let Some((base_at, base)) = begin {
                let baseline = base.map_or(Head::Detached, Head::Branch);
                let events = cx.events(repo, wt, base_at)?.clone();
                // The head HEAD was on when the baseline was read, advanced to the call.
                let commits = commits_during(
                    &events,
                    &baseline,
                    call.start * 1_000_000,
                    call.end * 1_000_000,
                );
                for (head, ev) in commits {
                    let Some(new) = ev.new.as_deref() else {
                        continue;
                    };
                    let paths = cx.commit_paths(repo, ev.old.as_deref(), new)?;
                    if paths.is_some_and(|p| p.iter().any(|p| p == item.rel)) {
                        moved = Some((head, ev.lo / 1_000_000));
                        break;
                    }
                }
            }
        }
        match moved {
            Some((head, at)) => {
                segment = cx.segment(repo, wt, &head, at)?;
                default = cx.is_default(repo, &head)?;
                if default {
                    segments::mark_default(cx.conn, segment)?;
                }
            }
            None => {
                if let Some(end) = e.obs.end() {
                    (segment, default) = stay_segment(cx, repo, wt, end)?;
                }
            }
        }
    }
    Ok((
        Some(segment),
        STATE_SET | if default { STATE_DEFAULT } else { 0 },
    ))
}
