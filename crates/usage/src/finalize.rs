//! Finishing a session's turns: the changed files of each turn are read from
//! the session log (`turns_unretained`, `load`), counted, and handed to the
//! store with the turns (docs/design/usage.md「轮次的观察与改动项」).
//!
//! The engine runs it on the executor, a batch at a time; `testing::Scenario`
//! runs the same code. Blocking: file reads.

use std::path::Path;
use std::time::{Duration, Instant};

use lectern_logging::{debug, warn};
use lectern_services::session::{
    Content, EditOp, SessionError, SessionKey, SessionSource, ToolKind, TurnItem,
};

use crate::store::{Ingested, ItemWork, PathPlace, Store, StoreError, TurnWork};

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
    /// Where a changed file lies, when no record of the call says.
    pub(crate) resolve: &'a dyn Fn(&Path) -> PathPlace,
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
    let index = if pending.ready.iter().any(|t| !t.edits.is_empty()) {
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
    for (n, turn) in pending.ready.iter().enumerate() {
        if n > 0 && started.elapsed() >= s.time {
            out.more = true;
            break;
        }
        let mut items = Vec::new();
        if let Some(index) = &index
            && let Some(found) = index.turns.iter().find(|t| t.id.0.as_ref() == turn.turn)
        {
            for cid in &turn.edits {
                let Some(call) = found.items.iter().find_map(|item| match item {
                    TurnItem::Tool(c)
                        if c.id.0.as_ref() == cid && matches!(c.kind, ToolKind::Edit { .. }) =>
                    {
                        Some(c)
                    }
                    _ => None,
                }) else {
                    continue;
                };
                let at = call.result.as_ref().unwrap_or(&call.input);
                match s.source.load(s.key, at) {
                    Ok(Content::Edits(edits)) => {
                        for edit in edits.iter() {
                            let lines = edit_lines(&edit.op);
                            if lines == 0 && !matches!(edit.op, EditOp::Delete) {
                                continue;
                            }
                            items.push(ItemWork {
                                call: cid.clone(),
                                path: edit.path.clone(),
                                lines,
                                place: (s.resolve)(&edit.path),
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
        }
        works.push(TurnWork { id: turn.id, items });
    }
    out.turns = works.len();
    let written = s.store.finalize(s.key, &works)?;
    debug!(
        "turns settled";
        turns = works.len(),
        items = works.iter().map(|w| w.items.len()).sum::<usize>(),
        facts = written.facts.len()
    );
    out.written = Some(written);
    Ok(out)
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
