//! A file's attribution in a review (docs/design/review.md「改动链」「归因的
//! 缓存」): its change chain from `review.attribution`, or built after
//! loading its steps' edits, then attributed to the target's turns and the
//! other sessions; files claimed through commands or by time are
//! file-level.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use lectern_logging::{Value, debug, warn};
use lectern_services::changes::{ChangeDetail, ChangeSource, SideText};
use lectern_services::review::{
    Alignment, AttributionState, CallRef, FileAttribution, HunkAttribution, StepEntry, Survival,
    TurnRef,
};
use lectern_services::session::{Content, ContentRef, SessionKey, ToolCallId, ToolStatus};

use super::Engine;
use super::round::{FileWork, Hist, MAX_TEXT, Work, ends, side_oid, source_of};
use crate::chain::attribute::{self as attr, Owner};
use crate::chain::history::{self, HistoryStep};
use crate::chain::{self, Chain, ChainInput, Edit, Side, StepInput, codec};
use crate::claims::ClaimKind;
use crate::diag;

/// A file's attribution.
#[derive(Clone, Debug)]
pub(crate) struct FileResult {
    pub attribution: FileAttribution,
    /// One per hunk of the final change.
    pub hunks: Arc<[HunkAttribution]>,
    pub chain: Option<Arc<Chain>>,
}

impl FileResult {
    /// The attribution of a file not computed yet.
    pub(crate) fn pending(work: &Work, file: &FileWork) -> FileAttribution {
        let _ = work;
        FileAttribution {
            state: if file.history.is_empty() && file.command.is_none() && !file.by_time {
                AttributionState::None
            } else {
                AttributionState::Pending
            },
            turns: Arc::from(Vec::new()),
            commands: Arc::from(Vec::new()),
            other_sessions: Arc::from(Vec::new()),
            drifted_hunks: 0,
        }
    }
}

/// The history steps of a file as the digest and the chain want them.
fn digest(work: &Work, file: &FileWork) -> [u8; 32] {
    let f = &work.list.files[file.list as usize];
    let (start, end) = ends(f);
    let steps: Vec<HistoryStep<'_>> = file
        .history
        .iter()
        .filter_map(|h| {
            let who = &work.whos[h.who as usize];
            let call = &who.claims.calls[h.call as usize];
            let ClaimKind::Edit {
                status,
                input,
                result,
                ..
            } = &call.kind
            else {
                return None;
            };
            Some(HistoryStep {
                session: &who.key,
                adapter_version: who.claims.adapter_version,
                turn: &call.turn,
                call: &call.call,
                status: *status,
                input: *input,
                result: *result,
            })
        })
        .collect();
    // The base side's path first, the worktree side's (`ChainInput::end`)
    // last.
    let paths = if start == end {
        vec![start]
    } else {
        vec![start, end]
    };
    history::digest(&steps, &paths)
}

/// Loads a change step's edits: the result's when it has them, the
/// input's otherwise. None when neither loads as edits.
fn load(engine: &Engine, work: &Work, h: &Hist) -> Option<Arc<[Edit]>> {
    let who = &work.whos[h.who as usize];
    let call = &who.claims.calls[h.call as usize];
    let ClaimKind::Edit { input, result, .. } = &call.kind else {
        return None;
    };
    let source = source_of(engine, &who.key)?;
    let edits = |content| match content {
        Ok(Content::Edits(edits)) => Ok(edits),
        Ok(_) => Err(Value::text("not_edits")),
        Err(e) => Err(diag::session_error(&e)),
    };
    let loaded = match result
        .ok_or(Value::text("no_result"))
        .and_then(|r| edits(source.load(&who.key, &r)))
    {
        Ok(edits) => edits,
        Err(_) => match edits(source.load(&who.key, input)) {
            Ok(edits) => edits,
            Err(error) => {
                not_loaded(&who.key, input, error);
                return None;
            }
        },
    };
    let rel = &who.rel;
    Some(
        loaded
            .iter()
            .filter_map(|e| Edit::from_file_edit(e, &|p| rel.rel(p)))
            .collect(),
    )
}

/// The step does not apply; its lines show as Drifted. Out of line: the
/// load is on the attribution's path, the failure is rare.
#[cold]
#[inline(never)]
fn not_loaded(key: &SessionKey, input: &ContentRef, error: Value<'static>) {
    warn!("change step did not load; it does not apply";
        from = diag::session(key),
        pos = input.pos(),
        line_len = input.line_len(),
        part = input.part(),
        error = error);
}

/// The chain's inputs: the steps with their edits loaded.
pub(crate) fn step_inputs(engine: &Engine, work: &Work, file: &FileWork) -> Vec<StepInput> {
    file.history
        .iter()
        .map(|h| {
            let call = &work.whos[h.who as usize].claims.calls[h.call as usize];
            let completed = matches!(
                call.kind,
                ClaimKind::Edit {
                    status: ToolStatus::Completed,
                    ..
                }
            );
            StepInput {
                group: h.group,
                completed,
                // A step that does not load (the position is stale) does
                // not apply: its lines show as Drifted.
                edits: load(engine, work, h).unwrap_or_else(|| Arc::from(Vec::new())),
            }
        })
        .collect()
}

/// The two sides' texts of a file, as the chain takes them.
pub(crate) fn texts(work: &Work, file: &FileWork) -> Option<lectern_services::changes::SideTexts> {
    work.source.texts(&work.list, file.list, MAX_TEXT).ok()
}

pub(crate) fn side<'a>(
    text: &'a SideText,
    identity: Option<&lectern_services::changes::FileSide>,
) -> Side<'a> {
    match text {
        SideText::Absent => Side::Absent,
        SideText::Text(bytes) => Side::Text(bytes),
        SideText::TooLarge { .. } | SideText::NotRetained | SideText::Unavailable => {
            Side::Opaque(side_oid(identity))
        }
    }
}

/// Computes the attribution of `work.files[n]`. Blocking.
pub(crate) fn file(engine: &Engine, work: &Work, n: usize, cancel: &AtomicBool) -> FileResult {
    let _ = cancel;
    let file = &work.files[n];
    if file.history.is_empty() {
        return claimed_without_steps(work, file);
    }
    if matches!(
        work.target,
        lectern_services::review::ReviewTarget::Range { .. }
    ) {
        return range_file(work, file);
    }
    let f = &work.list.files[file.list as usize];
    let digest = digest(work, file);
    let key = codec::key(
        work.git.hash,
        side_oid(f.old.as_ref()),
        f.new.as_ref().map(|s| &s.content),
        &digest,
    );
    let partition = engine.cfg.cache.repo(work.git.common_dir());
    let built = std::cell::Cell::new(false);
    let chain = codec::cached(&partition, key.as_ref(), || {
        built.set(true);
        build(engine, work, file)
    });
    let owners: Vec<Owner> = file.history.iter().map(|h| work.owner(h)).collect();
    let hunks: &[lectern_services::changes::DiffHunk] = match &f.detail {
        ChangeDetail::Text { hunks, .. } => hunks,
        _ => &[],
    };
    let order = narrative_rank(work);
    let mut hunk_attrs = attr::hunks(&chain, &owners, hunks);
    for h in &mut hunk_attrs {
        h.turns = sorted(&h.turns, &order);
    }
    let mut attribution = attr::file(&chain, &owners, &hunk_attrs);
    attribution.turns = sorted(&attribution.turns, &order);
    debug!("file attributed";
        file = n,
        path = diag::path(&f.path),
        cached = !built.get(),
        steps = file.history.len(),
        hunks = hunk_attrs.len(),
        drifted_hunks = attribution.drifted_hunks);
    FileResult {
        attribution,
        hunks: hunk_attrs.into(),
        chain: Some(Arc::new(chain)),
    }
}

/// Builds the chain of a file: loads every step. Blocking.
pub(crate) fn build(engine: &Engine, work: &Work, file: &FileWork) -> Chain {
    let f = &work.list.files[file.list as usize];
    let (start, end) = ends(f);
    let steps = step_inputs(engine, work, file);
    let texts = texts(work, file);
    let (base, worktree) = match &texts {
        Some(t) => (side(&t.old, f.old.as_ref()), side(&t.new, f.new.as_ref())),
        None => (
            Side::Opaque(side_oid(f.old.as_ref())),
            Side::Opaque(side_oid(f.new.as_ref())),
        ),
    };
    chain::build(&ChainInput {
        hash: work.git.hash,
        start: &start,
        end: &end,
        base,
        worktree,
        steps: &steps,
    })
}

/// A file claimed through commands or by time: file-level, no lines.
fn claimed_without_steps(work: &Work, file: &FileWork) -> FileResult {
    let f = &work.list.files[file.list as usize];
    let count = match &f.detail {
        ChangeDetail::Text { hunks, .. } => hunks.len(),
        _ => 0,
    };
    let file_level = HunkAttribution {
        lines: false,
        turns: Arc::from(Vec::new()),
        others: Arc::from(Vec::new()),
        drifted: 0,
    };
    let mut attribution = FileAttribution {
        state: AttributionState::FileOnly,
        turns: Arc::from(Vec::new()),
        commands: Arc::from(Vec::new()),
        other_sessions: Arc::from(Vec::new()),
        drifted_hunks: 0,
    };
    match &file.command {
        Some((w, calls)) => {
            let who = &work.whos[*w as usize];
            match who.own {
                Some(s) => {
                    let mut turns: Vec<TurnRef> = Vec::new();
                    let mut commands: Vec<CallRef> = Vec::new();
                    for &c in calls {
                        let call = &who.claims.calls[c as usize];
                        let turn = TurnRef {
                            session: s,
                            ordinal: call.ordinal,
                        };
                        if !turns.contains(&turn) {
                            turns.push(turn);
                        }
                        commands.push(CallRef {
                            turn,
                            call: call.call.clone(),
                        });
                    }
                    attribution.turns = turns.into();
                    attribution.commands = commands.into();
                }
                None => attribution.other_sessions = vec![who.key.clone()].into(),
            }
        }
        None if file.by_time => {}
        None => attribution.state = AttributionState::None,
    }
    FileResult {
        attribution,
        hunks: vec![file_level; count].into(),
        chain: None,
    }
}

/// A file of a range (「审阅对象与基准」): no chain, whose base is not a
/// session's start; file-level, the sessions that claimed it.
fn range_file(work: &Work, file: &FileWork) -> FileResult {
    let mut sessions: Vec<lectern_services::session::SessionKey> = Vec::new();
    for h in file.history.iter() {
        let key = &work.whos[h.who as usize].key;
        if !sessions.contains(key) {
            sessions.push(key.clone());
        }
    }
    let mut result = claimed_without_steps(work, file);
    result.attribution.state = AttributionState::FileOnly;
    result.attribution.other_sessions = sessions.into();
    result
}

/// Each turn's position in the narrative.
fn narrative_rank(work: &Work) -> HashMap<TurnRef, usize> {
    work.order
        .iter()
        .enumerate()
        .filter_map(|(n, &(s, t))| {
            let ordinal = work.own[s as usize].index.as_ref()?.turns[t].ordinal;
            Some((
                TurnRef {
                    session: s,
                    ordinal,
                },
                n,
            ))
        })
        .collect()
}

fn sorted(turns: &[TurnRef], rank: &HashMap<TurnRef, usize>) -> Arc<[TurnRef]> {
    let mut out = turns.to_vec();
    out.sort_by_key(|t| rank.get(t).copied().unwrap_or(usize::MAX));
    out.into()
}

/// The narrative step of `file` in turn `ordinal` of session `s`.
pub(crate) fn step_entry(
    work: &Work,
    results: &[Option<Arc<FileResult>>],
    s: u16,
    ordinal: u32,
    file: u32,
) -> Option<StepEntry> {
    let id = *work.step_of.get(&(s, ordinal, file))?;
    let step = work.steps.get(&id)?;
    let f = &work.files[file as usize];
    // The calls of this step, in history order.
    let members: Vec<usize> = f
        .history
        .iter()
        .enumerate()
        .filter(|(_, h)| h.group == step.group)
        .map(|(k, _)| k)
        .collect();
    let calls: Arc<[ToolCallId]> = members
        .iter()
        .map(|&k| {
            let h = &f.history[k];
            work.whos[h.who as usize].claims.calls[h.call as usize]
                .call
                .clone()
        })
        .collect();
    let chain = results
        .get(file as usize)
        .and_then(Option::as_ref)
        .and_then(|r| r.chain.clone());
    let (added, removed, alignment, survival) = match chain {
        // Not computed yet: placeholders until the file is attributed.
        None => (0, 0, Alignment::Aligned, Survival::All),
        Some(chain) => {
            let all_calls: Vec<ToolCallId> = f
                .history
                .iter()
                .map(|h| {
                    work.whos[h.who as usize].claims.calls[h.call as usize]
                        .call
                        .clone()
                })
                .collect();
            let g = chain.groups.iter().position(|g| {
                g.steps
                    .first()
                    .is_some_and(|&k| f.history[k as usize].group == step.group)
            });
            match g {
                Some(g) => {
                    let alignment = attr::alignment(&chain, g, &all_calls);
                    match &chain.groups[g].diff {
                        Some(d) => (d.added, d.removed, alignment, d.survival),
                        None => (0, 0, Alignment::Unaligned, Survival::All),
                    }
                }
                None => (0, 0, Alignment::Unaligned, Survival::All),
            }
        }
    };
    Some(StepEntry {
        step: id,
        file: f.key.clone(),
        calls,
        added,
        removed,
        alignment,
        survival,
    })
}
