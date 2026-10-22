//! Change chains, line provenance and attribution (docs/design/review.md
//! 「改动链」「归因的缓存」).
//!
//! A path's change chain starts from the base side's content and applies
//! each change step (one edit tool call's edits to the path) in time order:
//! `Write` / `Add` replace the version, `Replace` and `Patch` must match
//! exactly, `Delete` removes the file. A step that does not apply leaves the
//! version as it was and is recorded as failed; nothing is fuzzily matched
//! or guessed (docs/design/interaction.md「状态与异常」). Every applied step
//! updates the provenance of each line with `git::line_diff`; the last
//! version is then compared with the worktree side, whose new lines have no
//! source.
//!
//! - [`build`] computes a [`Chain`]: pure, any thread, no IO; the edits it
//!   applies were loaded by the caller.
//! - The chain is independent of the review target, so one cached value
//!   serves every session's review of the path: steps are referred to by
//!   their index in the path's history. [`attribute`] maps the steps to
//!   turns of the review target or to other sessions.
//! - [`history::digest`] and [`codec`] give the `review.attribution` cache
//!   key and value. The digest is computed from the turn indexes only, so a
//!   hit loads nothing.

mod apply;
pub(crate) mod attribute;
pub(crate) mod codec;
pub(crate) mod history;

#[cfg(test)]
mod tests;

use std::path::Path;
use std::sync::Arc;

use lectern_git::{HashKind, Oid, line_diff};
use lectern_services::changes::{ChangePath, DiffHunk};
use lectern_services::project::BlobId;
use lectern_services::review::Survival;
use lectern_services::session::{EditOp, FileEdit};

/// A version above this makes the file file-level only (「改动链」上限).
pub(crate) const MAX_VERSION_BYTES: u64 = 4 << 20;
/// More change steps than this make the file file-level only.
pub(crate) const MAX_STEPS: usize = 200;

/// One edit of a change step, its paths relative to the worktree root.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Edit {
    pub path: ChangePath,
    pub op: Op,
}

/// `EditOp`, with `Write` and `Add` as one (both replace the version) and
/// the target of a move relative to the worktree root.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum Op {
    Write {
        content: Arc<str>,
    },
    Replace {
        old: Arc<str>,
        new: Arc<str>,
        all: bool,
    },
    Patch {
        hunks: Arc<str>,
        move_to: Option<ChangePath>,
    },
    Delete,
}

impl Edit {
    /// The edit `edit` of a loaded `Content::Edits`, with its absolute
    /// paths made relative by `relative` (the worktree prefix, or the
    /// session's cwd mapped to the placed one; 「改动步与声明」). None when
    /// the path, or the target of a move, is outside the worktree.
    pub(crate) fn from_file_edit(
        edit: &FileEdit,
        relative: &dyn Fn(&Path) -> Option<ChangePath>,
    ) -> Option<Edit> {
        let op = match &edit.op {
            EditOp::Write { content, .. } | EditOp::Add { content } => Op::Write {
                content: content.clone(),
            },
            EditOp::Replace { old, new, all } => Op::Replace {
                old: old.clone(),
                new: new.clone(),
                all: *all,
            },
            EditOp::Patch { hunks, move_to } => Op::Patch {
                hunks: hunks.clone(),
                move_to: match move_to {
                    Some(to) => Some(relative(to)?),
                    None => None,
                },
            },
            EditOp::Delete => Op::Delete,
        };
        Some(Edit {
            path: relative(&edit.path)?,
            op,
        })
    }
}

/// One change step of the path's history, in time order.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct StepInput {
    /// The narrative step it belongs to: steps of one turn of one session
    /// share it (any value; equal values are one group).
    pub group: u32,
    /// The call's status is `Completed`. Other calls are not applied (the
    /// brief's `EditFailed`), and are not Drifted.
    pub completed: bool,
    /// The call's loaded edits, all paths: the chain applies, in order,
    /// those on the path it is on, following `move_to`.
    pub edits: Arc<[Edit]>,
}

/// One side of the file.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Side<'a> {
    /// No file on this side (added, deleted).
    Absent,
    /// The canonical content (`SideText::Text`).
    Text(&'a [u8]),
    /// Content the chain cannot use: binary, `TooLarge`, `NotRetained`, a
    /// filter. The OID when the side has one, for file-level attribution.
    Opaque(Option<BlobId>),
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Chain {
    /// One per input step.
    pub steps: Arc<[StepResult]>,
    /// The narrative steps, in order of their first change step.
    pub groups: Arc<[GroupResult]>,
    pub detail: Detail,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct StepResult {
    pub outcome: Outcome,
    /// The version after this step, as a blob OID (`Oid::for_blob`; not in
    /// the object database). None when the file does not exist then, and
    /// for a file-level chain.
    pub version: Option<BlobId>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Outcome {
    Applied,
    /// Did not apply (Drifted): the version stays as it was.
    Failed,
    /// The call did not complete; not applied.
    NotCompleted,
    /// A file-level chain applies nothing.
    Unaligned,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct GroupResult {
    /// Indices of its change steps, ascending.
    pub steps: Arc<[u32]>,
    /// None for a file-level chain.
    pub diff: Option<GroupDiff>,
}

/// A narrative step's change: the `line_diff` of the version before its
/// first change step and the version after its last one.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct GroupDiff {
    pub hunks: Arc<[DiffHunk]>,
    pub added: u32,
    pub removed: u32,
    /// How many of the lines it added (those with its source in the
    /// version after it) are still in the worktree. A step that added no
    /// line survives `All`.
    pub survival: Survival,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum Detail {
    Lines(Lines),
    FileOnly {
        reason: Limit,
        /// The last `Write` / `Add` when its content's OID is the worktree
        /// side's: the file is attributed to that step.
        matched: Option<u32>,
    },
}

/// Line provenance of a chain within the limits.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Lines {
    /// The source of each worktree line.
    pub worktree: Arc<[Source]>,
    /// What happened to each base line.
    pub base: Arc<[Fate]>,
    /// The last version is the worktree side.
    pub aligned: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Source {
    /// Base line `n` (0-based).
    Base(u32),
    /// Added by step `n`.
    Step(u32),
    /// Not in the chain: only in the worktree (Drifted).
    None,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Fate {
    Kept,
    /// Removed by step `n`.
    Deleted(u32),
    /// Kept by the chain, not in the worktree: removed by a change outside
    /// it (Drifted).
    Outside,
}

/// Why a chain is file-level only.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Limit {
    /// More than `MAX_STEPS` change steps.
    Steps,
    /// A side or a version above `MAX_VERSION_BYTES`.
    Size,
    /// A side is not text. Not cached: whether the worktree side's content
    /// was retained depends on the comparison's budget, and the key is the
    /// same either way.
    Sides,
}

impl Chain {
    /// Whether the value may go into `review.attribution`: everything but a
    /// chain made file-level by a side that was not text.
    pub(crate) fn cacheable(&self) -> bool {
        !matches!(
            self.detail,
            Detail::FileOnly {
                reason: Limit::Sides,
                ..
            }
        )
    }
}

/// The input of [`build`].
pub(crate) struct ChainInput<'a> {
    pub hash: HashKind,
    /// The path the chain starts on: the base side's path (a rename's
    /// source), or the new path when the base has none.
    pub start: &'a ChangePath,
    /// The worktree side's path (a rename's target); `start` for a
    /// deletion and when the path did not change.
    pub end: &'a ChangePath,
    pub base: Side<'a>,
    pub worktree: Side<'a>,
    pub steps: &'a [StepInput],
}

impl<'a> ChainInput<'a> {
    fn rename(&self) -> Rename<'a> {
        Rename {
            start: self.start,
            end: self.end,
        }
    }
}

/// The file's rename in the change set, which the chain follows even when
/// no `move_to` says so: a rename by command (`mv`, `git mv`) followed by
/// edits on the new path, Codex's `Delete File` of the source with `Add
/// File` of the target, or a `Write` of the target before the source is
/// removed. While the chain is still on the source, the first edit on the
/// target moves it there (within a call, so a deletion of the source and an
/// addition of the target in one patch carry over); the content moves with
/// it, and a `Write` / `Add` realigns it as anywhere else.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Rename<'a> {
    pub start: &'a ChangePath,
    pub end: &'a ChangePath,
}

impl Rename<'_> {
    /// An edit on `edit_path` moves the chain from `path` to it.
    pub(crate) fn follows(&self, path: &ChangePath, edit_path: &ChangePath) -> bool {
        self.start != self.end && path == self.start && edit_path == self.end
    }
}

/// The lines of `text`: each ends after `\n`, a last line without one is a
/// line (as `line_diff` counts them).
pub(crate) fn lines(text: &[u8]) -> std::slice::SplitInclusive<'_, u8, impl FnMut(&u8) -> bool> {
    text.split_inclusive(|&b| b == b'\n')
}

fn line_count(text: &[u8]) -> usize {
    let newlines = text.iter().filter(|&&b| b == b'\n').count();
    newlines + usize::from(text.last().is_some_and(|&b| b != b'\n'))
}

fn blob_id(hash: HashKind, content: &[u8]) -> BlobId {
    let oid = Oid::for_blob(hash, content);
    // An `Oid` is 20 or 32 bytes, which `BlobId` takes.
    BlobId::from_bytes(oid.as_bytes()).unwrap_or(BlobId::Sha1([0; 20]))
}

fn too_large(text: &[u8]) -> bool {
    text.len() as u64 > MAX_VERSION_BYTES
}

/// The groups of `steps`, in order of their first step.
fn groups(steps: &[StepInput]) -> Vec<Vec<u32>> {
    let mut keys: Vec<u32> = Vec::new();
    let mut out: Vec<Vec<u32>> = Vec::new();
    for (k, step) in steps.iter().enumerate() {
        match keys.iter().position(|&g| g == step.group) {
            Some(i) => out[i].push(k as u32),
            None => {
                keys.push(step.group);
                out.push(vec![k as u32]);
            }
        }
    }
    out
}

/// The change chain of one path. Pure computation, any thread.
pub(crate) fn build(input: &ChainInput<'_>) -> Chain {
    let base = match input.base {
        Side::Absent => None,
        Side::Text(text) => Some(text),
        Side::Opaque(_) => return file_only(input, Limit::Sides),
    };
    let worktree = match input.worktree {
        Side::Absent => None,
        Side::Text(text) => Some(text),
        Side::Opaque(_) => return file_only(input, Limit::Sides),
    };
    if input.steps.len() > MAX_STEPS {
        return file_only(input, Limit::Steps);
    }
    if base.is_some_and(too_large) || worktree.is_some_and(too_large) {
        return file_only(input, Limit::Size);
    }
    let worktree_cr = worktree.is_some_and(|w| w.contains(&b'\r'));
    let members = groups(input.steps);
    // For each step, the group it ends (its last step) or starts.
    let mut first_of = vec![None; input.steps.len()];
    let mut last_of = vec![None; input.steps.len()];
    for (g, steps) in members.iter().enumerate() {
        first_of[steps[0] as usize] = Some(g);
        last_of[*steps.last().unwrap_or(&0) as usize] = Some(g);
    }
    let mut before: Vec<Option<Option<Arc<[u8]>>>> = vec![None; members.len()];
    let mut diffs: Vec<Option<(lectern_git::FileDiff, u32)>> = vec![None; members.len()];

    let mut path = input.start.clone();
    let mut version: Option<Arc<[u8]>> = base.map(Arc::from);
    let mut version_id = version.as_deref().map(|v| blob_id(input.hash, v));
    let base_lines = base.map_or(0, line_count);
    let mut sources: Vec<Source> = (0..base_lines as u32).map(Source::Base).collect();
    let mut fates = vec![Fate::Kept; base_lines];
    let mut results = Vec::with_capacity(input.steps.len());

    for (k, step) in input.steps.iter().enumerate() {
        if let Some(g) = first_of[k] {
            before[g] = Some(version.clone());
        }
        let outcome = if step.completed {
            match apply::step(
                version.as_deref(),
                &path,
                input.rename(),
                &step.edits,
                worktree_cr,
            ) {
                Some((next, next_path)) => {
                    if next.as_deref().is_some_and(too_large) {
                        return file_only(input, Limit::Size);
                    }
                    let old = version.as_deref().unwrap_or_default();
                    let new = next.as_deref().unwrap_or_default();
                    let Some(diff) = line_diff(old, new, MAX_VERSION_BYTES) else {
                        return file_only(input, Limit::Size);
                    };
                    sources = advance(&sources, &diff, k as u32, &mut fates);
                    version_id = next.as_deref().map(|v| blob_id(input.hash, v));
                    version = next.map(Arc::from);
                    path = next_path;
                    Outcome::Applied
                }
                None => Outcome::Failed,
            }
        } else {
            Outcome::NotCompleted
        };
        results.push(StepResult {
            outcome,
            version: version_id,
        });
        if let Some(g) = last_of[k] {
            let old = before[g].take().flatten();
            let old = old.as_deref().unwrap_or_default();
            let new = version.as_deref().unwrap_or_default();
            let Some(diff) = line_diff(old, new, MAX_VERSION_BYTES) else {
                return file_only(input, Limit::Size);
            };
            // The lines it added: its sources in the version after it.
            let added = count_sources(&sources, &members[g]);
            diffs[g] = Some((diff, added));
        }
    }

    // Align with the worktree side.
    let last = version.as_deref();
    let aligned = last == worktree;
    let worktree_sources = if aligned {
        sources
    } else {
        let old = last.unwrap_or_default();
        let new = worktree.unwrap_or_default();
        let Some(diff) = line_diff(old, new, MAX_VERSION_BYTES) else {
            return file_only(input, Limit::Size);
        };
        outside(&sources, &diff, &mut fates)
    };

    let groups = members
        .into_iter()
        .zip(diffs)
        .map(|(steps, diff)| {
            let diff = diff.map(|(d, added)| {
                let left = count_sources(&worktree_sources, &steps);
                GroupDiff {
                    hunks: d.hunks.iter().map(hunk).collect(),
                    added: d.added,
                    removed: d.removed,
                    survival: if left == added {
                        Survival::All
                    } else if left == 0 {
                        Survival::None
                    } else {
                        Survival::Partly
                    },
                }
            });
            GroupResult {
                steps: steps.into(),
                diff,
            }
        })
        .collect();
    Chain {
        steps: results.into(),
        groups,
        detail: Detail::Lines(Lines {
            worktree: worktree_sources.into(),
            base: fates.into(),
            aligned,
        }),
    }
}

/// The sources after `diff`, applied by step `step`: unchanged lines keep
/// theirs, new lines are the step's; base lines it removes are recorded in
/// `fates`.
fn advance(
    sources: &[Source],
    diff: &lectern_git::FileDiff,
    step: u32,
    fates: &mut [Fate],
) -> Vec<Source> {
    let mut out = Vec::with_capacity(sources.len() + diff.added as usize);
    let mut at = 0usize;
    for h in diff.hunks.iter() {
        let (start, end) = (h.old.start as usize, h.old.end as usize);
        out.extend_from_slice(&sources[at..start]);
        for s in &sources[start..end] {
            if let Source::Base(b) = *s {
                fates[b as usize] = Fate::Deleted(step);
            }
        }
        out.extend(std::iter::repeat_n(Source::Step(step), h.new.len()));
        at = end;
    }
    out.extend_from_slice(&sources[at..]);
    out
}

/// The worktree's sources from the last version's and their `diff`: new
/// lines have none, and base lines the worktree no longer has were removed
/// outside the chain.
fn outside(sources: &[Source], diff: &lectern_git::FileDiff, fates: &mut [Fate]) -> Vec<Source> {
    let mut out = Vec::with_capacity(sources.len() + diff.added as usize);
    let mut at = 0usize;
    for h in diff.hunks.iter() {
        let (start, end) = (h.old.start as usize, h.old.end as usize);
        out.extend_from_slice(&sources[at..start]);
        for s in &sources[start..end] {
            if let Source::Base(b) = *s {
                fates[b as usize] = Fate::Outside;
            }
        }
        out.extend(std::iter::repeat_n(Source::None, h.new.len()));
        at = end;
    }
    out.extend_from_slice(&sources[at..]);
    out
}

/// Lines whose source is one of `steps` (ascending).
fn count_sources(sources: &[Source], steps: &[u32]) -> u32 {
    sources
        .iter()
        .filter(|s| matches!(s, Source::Step(k) if steps.binary_search(k).is_ok()))
        .count() as u32
}

fn hunk(h: &lectern_git::Hunk) -> DiffHunk {
    DiffHunk {
        old: h.old.clone(),
        new: h.new.clone(),
        old_byte: h.old_byte,
        new_byte: h.new_byte,
    }
}

/// A file-level chain: nothing is applied. The file goes to the last
/// `Write` / `Add` on the path (followed as `build` does, through `move_to`
/// and the rename) when its content (or its content with CRLF made LF, as
/// for a canonical side) is the worktree side's blob.
fn file_only(input: &ChainInput<'_>, reason: Limit) -> Chain {
    let worktree = match input.worktree {
        Side::Absent => None,
        Side::Text(text) => Some(blob_id(input.hash, text)),
        Side::Opaque(oid) => oid,
    };
    let rename = input.rename();
    let mut path = input.start.clone();
    let mut last_write: Option<(u32, &str)> = None;
    for (k, step) in input.steps.iter().enumerate() {
        if !step.completed {
            continue;
        }
        for edit in step.edits.iter() {
            if edit.path != path {
                if !rename.follows(&path, &edit.path) {
                    continue;
                }
                path = edit.path.clone();
            }
            match &edit.op {
                Op::Write { content } => last_write = Some((k as u32, content)),
                Op::Delete => last_write = None,
                Op::Patch {
                    move_to: Some(to), ..
                } => {
                    path = to.clone();
                    last_write = None;
                }
                Op::Replace { .. } | Op::Patch { .. } => {}
            }
        }
    }
    let matched = last_write.and_then(|(k, content)| {
        let worktree = worktree?;
        let same = |bytes: &[u8]| blob_id(input.hash, bytes) == worktree;
        (same(content.as_bytes())
            || (content.contains("\r\n") && same(content.replace("\r\n", "\n").as_bytes())))
        .then_some(k)
    });
    Chain {
        steps: input
            .steps
            .iter()
            .map(|_| StepResult {
                outcome: Outcome::Unaligned,
                version: None,
            })
            .collect(),
        groups: groups(input.steps)
            .into_iter()
            .map(|steps| GroupResult {
                steps: steps.into(),
                diff: None,
            })
            .collect(),
        detail: Detail::FileOnly { reason, matched },
    }
}
