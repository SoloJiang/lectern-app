//! Finishing a session's turns: the changed files of each turn are read from
//! the session log (`turns_unretained`, `load`), counted, and handed to the
//! store with the turns (docs/design/usage.md「轮次的观察与改动项」). For a turn
//! in a repository that was watched around it, the calls that say where the
//! session was (commands that mention git or go into a directory, files read
//! or searched, files written without a change) go with it; the commands are
//! read here, in memory, and only what they did leaves this module.
//!
//! The engine runs it on the executor, a batch at a time; `testing::Scenario`
//! runs the same code. Blocking: file reads.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use lectern_logging::{debug, warn};
use lectern_services::session::{
    Content, EditOp, SessionError, SessionKey, SessionSource, ToolCall, ToolKind, ToolStatus, Turn,
    TurnItem,
};

use crate::command::{self, bits};
use crate::gitread::GitRead;
use crate::paths;
use crate::store::calls;
use crate::store::{
    CallKind, CallWork, Command, Ingested, ItemWork, PathPlace, Place, ReadyTurn, Store,
    StoreError, TurnWork, WtRef,
};

/// The most bytes of each side the line diff of a `Replace` reads
/// (docs/design/usage.md「轮次的观察与改动项」).
const DIFF_BYTES: u64 = 4 << 20;

/// Most turns one batch settles.
pub(crate) const BATCH_TURNS: usize = 50;

/// Longest a batch reads and counts before it stops (the rest is the next
/// batch's).
pub(crate) const BATCH_TIME: Duration = Duration::from_millis(100);

/// What to settle and how.
pub(crate) struct Settling<'a> {
    pub(crate) store: &'a Store,
    pub(crate) source: &'a dyn SessionSource,
    pub(crate) key: &'a SessionKey,
    pub(crate) now_ms: i64,
    /// The session has no open turn: its last turn is over.
    pub(crate) end_open: bool,
    /// Where the files of a directory lie.
    pub(crate) resolve: &'a dyn Fn(&Path) -> PathPlace,
    /// What the repositories say (the default branch, a branch's reflog, the
    /// paths of a commit).
    pub(crate) reads: &'a dyn GitRead,
    pub(crate) turns: usize,
    pub(crate) time: Duration,
}

/// What a batch did.
#[derive(Debug, Default)]
pub(crate) struct Settled {
    /// What the store wrote (None when there was nothing to write).
    pub(crate) written: Option<Ingested>,
    /// Turns settled.
    pub(crate) turns: usize,
    /// More turns are ready: run another batch.
    pub(crate) more: bool,
    /// The earliest moment a turn that is not ready yet will be.
    pub(crate) wait_until: Option<i64>,
    /// The log could not be read: try again later.
    pub(crate) retry: bool,
}

/// The worktree and repository of a place.
fn worktree_of(place: &PathPlace) -> Option<WtRef> {
    match (&place.place, &place.worktree) {
        (Place::Git(common), Some(key)) => Some(WtRef {
            key: key.clone(),
            common: common.clone(),
        }),
        _ => None,
    }
}

/// One batch.
pub(crate) fn settle(s: &Settling<'_>) -> Result<Settled, StoreError> {
    let Some(pending) = s.store.pending(s.key, s.now_ms, s.end_open, s.turns)? else {
        return Ok(Settled::default());
    };
    let mut out = Settled {
        more: pending.more,
        wait_until: pending.wait_until,
        ..Settled::default()
    };
    if pending.ready.is_empty() {
        return Ok(out);
    }
    let started = Instant::now();
    // A turn in a repository that was watched around it is read for what its
    // calls say about the branch; the others are read only for their edits.
    let watched: Vec<bool> = pending.ready.iter().map(|t| watched(s.store, t)).collect();
    let index = if pending
        .ready
        .iter()
        .zip(&watched)
        .any(|(t, w)| t.edits || *w)
    {
        match s.source.turns_unretained(s.key) {
            Ok(index) => Some(index),
            Err(SessionError::NotFound | SessionError::Unrecognized) => None,
            Err(_) => {
                warn!("turn index unreadable");
                out.retry = true;
                return Ok(out);
            }
        }
    } else {
        None
    };
    let mut works = Vec::new();
    let mut dirs = DirCache::default();
    for (n, turn) in pending.ready.iter().enumerate() {
        if n > 0 && started.elapsed() >= s.time {
            out.more = true;
            break;
        }
        let found = index
            .as_ref()
            .and_then(|i| i.turns.iter().find(|t| t.id.0.as_ref() == turn.turn));
        let work = match found {
            Some(found) => turn_work(s, turn.id, found, watched[n], &mut dirs),
            None => TurnWork {
                id: turn.id,
                items: Vec::new(),
                calls: Vec::new(),
            },
        };
        works.push(work);
    }
    out.turns = works.len();
    let written = s.store.finalize(s.key, &works, s.reads)?;
    debug!(
        "turns settled";
        turns = works.len(),
        items = works.iter().map(|w| w.items.len()).sum::<usize>(),
        calls = works.iter().map(|w| w.calls.len()).sum::<usize>(),
        facts = written.facts.len()
    );
    out.written = Some(written);
    Ok(out)
}

fn watched(store: &Store, turn: &ReadyTurn) -> bool {
    turn.repo.as_deref().is_some_and(|repo| {
        store
            .watched(repo, turn.started_at - 1_000, turn.last_at + 2_000)
            .unwrap_or(false)
    })
}

/// What was resolved in a batch: a directory's place, and whether a path is a
/// directory.
#[derive(Default)]
struct DirCache {
    places: HashMap<PathBuf, PathPlace>,
    is_dir: HashMap<PathBuf, bool>,
}

impl DirCache {
    fn place(&mut self, s: &Settling<'_>, dir: &Path) -> PathPlace {
        self.places
            .entry(dir.to_path_buf())
            .or_insert_with(|| (s.resolve)(dir))
            .clone()
    }

    /// The directory a path stands for: itself when a search may name a
    /// directory and it is one, else its parent.
    fn dir_of<'p>(&mut self, path: &'p Path, search: bool) -> &'p Path {
        if search {
            let is_dir = *self
                .is_dir
                .entry(path.to_path_buf())
                .or_insert_with(|| std::fs::metadata(path).is_ok_and(|m| m.is_dir()));
            if is_dir {
                return path;
            }
        }
        paths::dir_of(path)
    }
}

/// The changed files of a turn and, for a turn that was watched, its calls.
fn turn_work(
    s: &Settling<'_>,
    id: i64,
    turn: &Turn,
    watched: bool,
    dirs: &mut DirCache,
) -> TurnWork {
    let mut items = Vec::new();
    let mut calls: Vec<CallWork> = Vec::new();
    for item in turn.items.iter() {
        let TurnItem::Tool(call) = item else {
            continue;
        };
        let Some((finished, _)) = call.finished else {
            continue;
        };
        let cid = calls::hash(&call.id.0);
        let (start, end) = (call.started_at.0, finished.0.max(call.started_at.0));
        match &call.kind {
            ToolKind::Edit { paths } => {
                let before = items.len();
                if matches!(call.finished, Some((_, ToolStatus::Completed))) {
                    edits_of(s, call, cid, (start, end), dirs, &mut items);
                }
                if watched && items.len() == before {
                    // Written, but nothing left: where the files are still
                    // says where the session was.
                    let targets = targets_of(s, paths.iter().map(|p| (p.as_path(), false)), dirs);
                    push_call(
                        &mut calls,
                        CallWork {
                            cid,
                            kind: CallKind::Edit,
                            start,
                            end,
                            workdir: None,
                            targets,
                            command: None,
                        },
                    );
                }
            }
            ToolKind::Read { paths } if watched => {
                let targets = targets_of(s, paths.iter().map(|p| (p.as_path(), false)), dirs);
                push_call(
                    &mut calls,
                    CallWork {
                        cid,
                        kind: CallKind::Read,
                        start,
                        end,
                        workdir: None,
                        targets,
                        command: None,
                    },
                );
            }
            ToolKind::Search { paths } if watched => {
                let targets = targets_of(s, paths.iter().map(|p| (p.as_path(), true)), dirs);
                push_call(
                    &mut calls,
                    CallWork {
                        cid,
                        kind: CallKind::Search,
                        start,
                        end,
                        workdir: None,
                        targets,
                        command: None,
                    },
                );
            }
            ToolKind::Execute {
                command, workdir, ..
            } if watched => {
                let parsed = command::parse(command, workdir);
                if !parsed.git && parsed.entered.is_empty() {
                    continue;
                }
                let mut targets: Vec<WtRef> = Vec::new();
                for dir in &parsed.entered {
                    if let Some(w) = worktree_of(&dirs.place(s, dir))
                        && !targets.contains(&w)
                    {
                        targets.push(w);
                    }
                }
                calls.push(CallWork {
                    cid,
                    kind: CallKind::Execute,
                    start,
                    end,
                    workdir: worktree_of(&dirs.place(s, workdir)),
                    targets,
                    command: Some(Command {
                        mask: bits::of(&parsed),
                        facts: parsed.facts,
                    }),
                });
            }
            _ => {}
        }
    }
    TurnWork { id, items, calls }
}

/// The edits of a completed `Edit` call, as items.
fn edits_of(
    s: &Settling<'_>,
    call: &ToolCall,
    cid: i64,
    (start, end): (i64, i64),
    dirs: &mut DirCache,
    items: &mut Vec<ItemWork>,
) {
    let at = call.result.as_ref().unwrap_or(&call.input);
    match s.source.load(s.key, at) {
        Ok(Content::Edits(edits)) => {
            for edit in edits.iter() {
                let lines = edit_lines(&edit.op);
                if lines == 0 && !matches!(edit.op, EditOp::Delete) {
                    continue;
                }
                items.push(ItemWork {
                    call: cid,
                    path: edit.path.clone(),
                    lines,
                    place: dirs.place(s, paths::dir_of(&edit.path)),
                    start,
                    end,
                });
            }
        }
        // No edits to read: the call counts for nothing.
        Ok(Content::Text(_)) | Err(SessionError::NotFound) => {}
        Err(_) => {
            warn!("edit content unreadable");
        }
    }
}

/// The worktrees of some paths (a path that is a directory when `search`
/// says it may be), each once.
fn targets_of<'p>(
    s: &Settling<'_>,
    paths: impl Iterator<Item = (&'p Path, bool)>,
    dirs: &mut DirCache,
) -> Vec<WtRef> {
    let mut out: Vec<WtRef> = Vec::new();
    for (path, search) in paths {
        let dir = dirs.dir_of(path, search);
        if let Some(w) = worktree_of(&dirs.place(s, dir))
            && !out.contains(&w)
        {
            out.push(w);
        }
    }
    out
}

/// A call that read, searched or wrote where the previous one did is the
/// same observation: the two merge (docs/design/usage.md「分支观察」: the
/// observations of `Read` and `Search` in a row merge).
fn push_call(calls: &mut Vec<CallWork>, call: CallWork) {
    if call.targets.is_empty() {
        return;
    }
    if let Some(last) = calls.last_mut()
        && last.kind == call.kind
        && last.targets == call.targets
        && last.command.is_none()
    {
        last.end = last.end.max(call.end);
        return;
    }
    calls.push(call);
}

/// The changed lines of one edit (ADR 0047): `Replace` the lines added and
/// removed by a line diff of the two texts (their line counts together when
/// they are too big to diff); a patch the `+` and `-` lines of its hunks; a
/// new file, or one written over, the lines of its content; a deletion
/// none.
pub(crate) fn edit_lines(op: &EditOp) -> u64 {
    match op {
        EditOp::Replace { old, new, .. } => {
            match lectern_git::line_diff(old.as_bytes(), new.as_bytes(), DIFF_BYTES) {
                Some(diff) => u64::from(diff.added) + u64::from(diff.removed),
                None => count_lines(old) + count_lines(new),
            }
        }
        EditOp::Patch { hunks, .. } => patch_lines(hunks),
        EditOp::Write { content, .. } | EditOp::Add { content } => count_lines(content),
        EditOp::Delete => 0,
    }
}

fn count_lines(text: &str) -> u64 {
    text.lines().count() as u64
}

/// The `+` and `-` lines of a unified diff. A line of the file headers (the
/// ones before the first `@@`) is not a changed line.
fn patch_lines(hunks: &str) -> u64 {
    let has_hunk = hunks.lines().any(|l| l.starts_with("@@"));
    let mut in_hunk = !has_hunk;
    let mut n = 0;
    for line in hunks.lines() {
        if line.starts_with("@@") {
            in_hunk = true;
            continue;
        }
        if !in_hunk {
            continue;
        }
        if (line.starts_with('+') || line.starts_with('-'))
            && !(!has_hunk && (line.starts_with("+++ ") || line.starts_with("--- ")))
        {
            n += 1;
        }
    }
    n
}
