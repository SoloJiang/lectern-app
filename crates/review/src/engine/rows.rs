//! Display rows and texts for `Prepare` (docs/design/review.md「接口」
//! `FileRows`, 「改动链」 intermediate versions): the final change of a file
//! from the change list's hunks and texts; a narrative step from two
//! replayed versions of its chain, kept in the version LRU by OID.

use std::sync::Arc;

use lectern_git::{FileDiff, FileTexts, Hunk, Row, Text, inline_changes, line_diff, rows};
use lectern_services::changes::{ChangeDetail, DiffHunk, SideText};
use lectern_services::project::BlobId;
use lectern_services::review::{
    DiffRow, FileRows, HunkAttribution, HunkInfo, InlineSpans, RowsKey, RowsSide, StepId, TurnRef,
};

use super::Engine;
use super::attribute::{self, FileResult, side};
use super::round::{Work, ends, side_oid};
use crate::chain::{self, ChainInput, MAX_VERSION_BYTES};

/// The rows of `key`, None when the file or step is not in the review.
/// Blocking.
pub(crate) fn rows_for(
    engine: &Engine,
    work: &Work,
    result: &dyn Fn(usize) -> Arc<FileResult>,
    key: &RowsKey,
) -> Option<FileRows> {
    match key {
        RowsKey::Final { file, context } => {
            let n = work.files.iter().position(|f| f.key == *file)?;
            let f = &work.list.files[work.files[n].list as usize];
            let texts = attribute::texts(work, &work.files[n])?;
            let diff_hunks: &[DiffHunk] = match &f.detail {
                ChangeDetail::Text { hunks, .. } => hunks,
                _ => &[],
            };
            let result = result(n);
            let infos = diff_hunks
                .iter()
                .enumerate()
                .map(|(i, h)| HunkInfo {
                    old: h.old.clone(),
                    new: h.new.clone(),
                    attribution: result.hunks.get(i).cloned().unwrap_or_else(file_level),
                })
                .collect();
            Some(assemble(
                key.clone(),
                side_of(texts.old, side_oid(f.old.as_ref())),
                side_of(texts.new, side_oid(f.new.as_ref())),
                diff_hunks,
                infos,
                *context,
            ))
        }
        RowsKey::Step { step, context } => step_rows(engine, work, result, *step, *context, key),
    }
}

fn file_level() -> HunkAttribution {
    HunkAttribution {
        lines: false,
        turns: Arc::from(Vec::new()),
        others: Arc::from(Vec::new()),
        drifted: 0,
    }
}

fn side_of(text: SideText, oid: Option<BlobId>) -> RowsSide {
    RowsSide { oid, text }
}

fn step_rows(
    engine: &Engine,
    work: &Work,
    result: &dyn Fn(usize) -> Arc<FileResult>,
    id: StepId,
    context: u32,
    key: &RowsKey,
) -> Option<FileRows> {
    let step = work.steps.get(&id)?;
    let n = step.file as usize;
    let file = &work.files[n];
    let chain = result(n).chain.clone()?;
    let g = chain.groups.iter().position(|g| {
        g.steps
            .first()
            .is_some_and(|&k| file.history[k as usize].group == step.group)
    })?;
    let group = &chain.groups[g];
    let first = *group.steps.first()? as usize;
    let last = *group.steps.last()? as usize;
    let f = &work.list.files[file.list as usize];
    let turn = TurnRef {
        session: step.session,
        ordinal: step.ordinal,
    };
    let Some(diff) = &group.diff else {
        // A file-level chain: no versions, the step's change only as the
        // log has it.
        return Some(FileRows {
            key: key.clone(),
            old: side_of(SideText::Unavailable, None),
            new: side_of(SideText::Unavailable, None),
            rows: Arc::from(Vec::new()),
            hunks: Arc::from(Vec::new()),
            inline: Arc::from(Vec::new()),
        });
    };
    // The versions before and after the step: the base side or the version
    // after the previous step, and the version after its last step.
    let before_oid = if first == 0 {
        side_oid(f.old.as_ref())
    } else {
        chain.steps[first - 1].version
    };
    let after_oid = chain.steps[last].version;
    let cached = |oid: Option<BlobId>| -> Option<Option<Arc<[u8]>>> {
        match oid {
            None => Some(None),
            Some(oid) => engine.version(&oid).map(Some),
        }
    };
    let (before, after) = match (cached(before_oid), cached(after_oid)) {
        (Some(b), Some(a)) => (b, a),
        _ => {
            let texts = attribute::texts(work, file)?;
            let (start, end) = ends(f);
            let inputs = attribute::step_inputs(engine, work, file);
            let input = ChainInput {
                hash: work.git.hash,
                start: &start,
                end: &end,
                base: side(&texts.old, f.old.as_ref()),
                worktree: side(&texts.new, f.new.as_ref()),
                steps: &inputs,
            };
            let before: Option<Arc<[u8]>> = chain::replay(&input, first).map(Arc::from);
            let after: Option<Arc<[u8]>> = chain::replay(&input, last + 1).map(Arc::from);
            for v in [&before, &after].into_iter().flatten() {
                engine.keep_version(chain::version_id(work.git.hash, v), Arc::clone(v));
            }
            (before, after)
        }
    };
    let text = |v: &Option<Arc<[u8]>>| match v {
        Some(bytes) => SideText::Text(Arc::clone(bytes)),
        None => SideText::Absent,
    };
    let hunks: Vec<DiffHunk> = match line_diff(
        before.as_deref().unwrap_or_default(),
        after.as_deref().unwrap_or_default(),
        MAX_VERSION_BYTES,
    ) {
        Some(d) => d
            .hunks
            .iter()
            .map(|h| DiffHunk {
                old: h.old.clone(),
                new: h.new.clone(),
                old_byte: h.old_byte,
                new_byte: h.new_byte,
            })
            .collect(),
        None => diff.hunks.to_vec(),
    };
    let infos = hunks
        .iter()
        .map(|h| HunkInfo {
            old: h.old.clone(),
            new: h.new.clone(),
            attribution: HunkAttribution {
                lines: true,
                turns: vec![turn].into(),
                others: Arc::from(Vec::new()),
                drifted: 0,
            },
        })
        .collect();
    Some(assemble(
        key.clone(),
        side_of(text(&before), before.as_ref().and(before_oid)),
        side_of(text(&after), after.as_ref().and(after_oid)),
        &hunks,
        infos,
        context,
    ))
}

/// The rows of a diff between two sides, each row with its hunk.
fn assemble(
    key: RowsKey,
    old: RowsSide,
    new: RowsSide,
    hunks: &[DiffHunk],
    infos: Vec<HunkInfo>,
    context: u32,
) -> FileRows {
    let git_side = |s: &SideText| match s {
        SideText::Absent => lectern_git::SideText::Absent,
        SideText::Text(bytes) => lectern_git::SideText::Text(Text::new(Arc::clone(bytes))),
        SideText::TooLarge { size } => lectern_git::SideText::TooLarge { size: *size },
        SideText::NotRetained => lectern_git::SideText::NotRetained,
        SideText::Unavailable => lectern_git::SideText::Unavailable,
    };
    let texts = FileTexts {
        old: git_side(&old.text),
        new: git_side(&new.text),
    };
    let diff = FileDiff {
        hunks: hunks
            .iter()
            .map(|h| Hunk {
                old: h.old.clone(),
                new: h.new.clone(),
                old_byte: h.old_byte,
                new_byte: h.new_byte,
            })
            .collect(),
        added: hunks.iter().map(|h| h.new.len() as u32).sum(),
        removed: hunks.iter().map(|h| h.old.len() as u32).sum(),
    };
    let raw = rows(&diff, &texts, context);
    let hunk_of_old = |line: u32| {
        hunks
            .iter()
            .position(|h| h.old.contains(&line))
            .map(|i| i as u32)
    };
    let hunk_of_new = |line: u32| {
        hunks
            .iter()
            .position(|h| h.new.contains(&line))
            .map(|i| i as u32)
    };
    // The hunk a context row belongs to: the one it leads to, before the
    // first fold after the previous hunk; the previous one otherwise.
    let next_hunk = |old: u32| {
        hunks
            .iter()
            .position(|h| h.old.start >= old)
            .map(|i| i as u32)
    };
    let mut out: Vec<DiffRow> = Vec::with_capacity(raw.len());
    let mut inline: Vec<(u32, InlineSpans)> = Vec::new();
    let mut current: Option<u32> = None;
    let mut folded_since = true;
    for row in raw {
        let index = out.len() as u32;
        match row {
            Row::Context {
                old,
                new,
                old_byte,
                new_byte,
            } => {
                let hunk = if folded_since || current.is_none() {
                    next_hunk(old).or(current)
                } else {
                    current
                };
                out.push(DiffRow::Context {
                    old,
                    new,
                    old_byte,
                    new_byte,
                    hunk,
                });
            }
            Row::Removed {
                old,
                pair,
                old_byte,
            } => {
                let hunk = hunk_of_old(old).unwrap_or(0);
                current = Some(hunk);
                folded_since = false;
                if let (Some(p), lectern_git::SideText::Text(o), lectern_git::SideText::Text(n)) =
                    (pair, &texts.old, &texts.new)
                    && let Some(spans) = inline_changes(o.line(old), n.line(p))
                {
                    inline.push((
                        index,
                        InlineSpans {
                            old: spans.old.into(),
                            new: spans.new.into(),
                        },
                    ));
                }
                out.push(DiffRow::Removed {
                    old,
                    pair,
                    old_byte,
                    hunk,
                });
            }
            Row::Added {
                new,
                pair,
                new_byte,
            } => {
                let hunk = hunk_of_new(new).unwrap_or(0);
                current = Some(hunk);
                folded_since = false;
                out.push(DiffRow::Added {
                    new,
                    pair,
                    new_byte,
                    hunk,
                });
            }
            Row::Folded {
                old,
                new,
                old_byte,
                new_byte,
            } => {
                folded_since = true;
                out.push(DiffRow::Folded {
                    old,
                    new,
                    old_byte,
                    new_byte,
                });
            }
        }
    }
    FileRows {
        key,
        old,
        new,
        rows: out.into(),
        hunks: infos.into(),
        inline: inline.into(),
    }
}

/// The bytes of a `FileRows`: its two texts, roughly, for the 8 MiB bound.
pub(crate) fn bytes(rows: &FileRows) -> usize {
    let side = |s: &RowsSide| match &s.text {
        SideText::Text(b) => b.len(),
        _ => 0,
    };
    side(&rows.old) + side(&rows.new) + rows.rows.len() * 48
}
