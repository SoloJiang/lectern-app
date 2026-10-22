//! Attribution of a chain for one review target (docs/design/review.md
//! 「改动链」第 5、6 步): the final hunks (base to worktree) to turns of the
//! target and to other sessions, the file's attribution, and each narrative
//! step's alignment. Cheap, and not cached: the cached chain is the same
//! for every review target, the owners of its steps are not.

use std::collections::BTreeSet;
use std::sync::Arc;

use lectern_services::changes::DiffHunk;
use lectern_services::review::{
    Alignment, AttributionState, FileAttribution, HunkAttribution, TurnRef,
};
use lectern_services::session::{SessionKey, ToolCallId};

use super::{Chain, Detail, Fate, Outcome, Source};

/// Who made a change step, seen from the review target.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Owner {
    /// A turn of the target (its session or a subagent in the worktree).
    Own(TurnRef),
    /// Another session.
    Other(SessionKey),
}

/// The attribution of each of `hunks` (the `ChangeList`'s base-to-worktree
/// hunks of the file). `owners` has one entry per step of the chain.
///
/// New-side lines take their worktree source, old-side lines their base
/// line's fate: steps of the target go to their turns, other sessions'
/// steps to `others`, and lines with no source (added outside the chain,
/// or base lines removed outside it) count as `drifted`. Where this diff
/// and the chain's disagree on a line (a base line the hunk shows as
/// changed that the chain kept), the line is neither.
pub(crate) fn hunks(chain: &Chain, owners: &[Owner], hunks: &[DiffHunk]) -> Vec<HunkAttribution> {
    debug_assert_eq!(owners.len(), chain.steps.len());
    let Detail::Lines(lines) = &chain.detail else {
        return hunks.iter().map(|_| file_level()).collect();
    };
    hunks
        .iter()
        .map(|h| {
            let mut steps = BTreeSet::new();
            let mut drifted = 0u32;
            for s in lines.worktree.get(range(&h.new)).unwrap_or_default() {
                match *s {
                    Source::Step(k) => {
                        steps.insert(k);
                    }
                    Source::None => drifted += 1,
                    Source::Base(_) => {}
                }
            }
            for f in lines.base.get(range(&h.old)).unwrap_or_default() {
                match *f {
                    Fate::Deleted(k) => {
                        steps.insert(k);
                    }
                    Fate::Outside => drifted += 1,
                    Fate::Kept => {}
                }
            }
            let (turns, others) = split(steps.into_iter().filter_map(|k| owners.get(k as usize)));
            HunkAttribution {
                lines: true,
                turns,
                others,
                drifted,
            }
        })
        .collect()
}

/// The file's attribution from its hunks' (`hunks` of the same chain).
/// `commands` stays empty: command claims are not part of the chain.
///
/// Line-level: the turns are those of every step of the target on the
/// file, in step order; the other sessions those the hunks point to.
/// File-level: the turns or session of the matched `Write`, or else every
/// step's; `drifted_hunks` is 0 (not known).
pub(crate) fn file(chain: &Chain, owners: &[Owner], hunks: &[HunkAttribution]) -> FileAttribution {
    match &chain.detail {
        Detail::Lines(_) => {
            let (turns, _) = split(owners.iter());
            let mut others: Vec<SessionKey> = Vec::new();
            for s in hunks.iter().flat_map(|h| h.others.iter()) {
                if !others.contains(s) {
                    others.push(s.clone());
                }
            }
            FileAttribution {
                state: AttributionState::Lines,
                turns,
                commands: Arc::from(Vec::new()),
                other_sessions: others.into(),
                drifted_hunks: hunks.iter().filter(|h| h.drifted > 0).count() as u32,
            }
        }
        Detail::FileOnly { matched, .. } => {
            let (turns, others) = match matched.and_then(|k| owners.get(k as usize)) {
                Some(owner) => split(std::iter::once(owner)),
                None => split(owners.iter()),
            };
            FileAttribution {
                state: AttributionState::FileOnly,
                turns,
                commands: Arc::from(Vec::new()),
                other_sessions: others,
                drifted_hunks: 0,
            }
        }
    }
}

/// The alignment of narrative step `group`: the calls among its steps that
/// did not apply (`calls` has one per step of the chain). `Unaligned` when
/// none applied, or for a file-level chain.
pub(crate) fn alignment(chain: &Chain, group: usize, calls: &[ToolCallId]) -> Alignment {
    let Some(g) = chain.groups.get(group) else {
        return Alignment::Unaligned;
    };
    let outcome = |k: &u32| chain.steps.get(*k as usize).map(|s| s.outcome);
    let failed: Vec<ToolCallId> = g
        .steps
        .iter()
        .filter(|k| outcome(k) == Some(Outcome::Failed))
        .filter_map(|k| calls.get(*k as usize).cloned())
        .collect();
    let applied = g.steps.iter().any(|k| outcome(k) == Some(Outcome::Applied));
    let unaligned = g
        .steps
        .iter()
        .any(|k| outcome(k) == Some(Outcome::Unaligned));
    if unaligned || (!applied && !failed.is_empty()) {
        Alignment::Unaligned
    } else if failed.is_empty() {
        Alignment::Aligned
    } else {
        Alignment::Partial {
            failed: failed.into(),
        }
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

fn range(r: &std::ops::Range<u32>) -> std::ops::Range<usize> {
    r.start as usize..r.end as usize
}

/// Turns and other sessions, each once, in the order given.
fn split<'a>(owners: impl Iterator<Item = &'a Owner>) -> (Arc<[TurnRef]>, Arc<[SessionKey]>) {
    let mut turns: Vec<TurnRef> = Vec::new();
    let mut others: Vec<SessionKey> = Vec::new();
    for owner in owners {
        match owner {
            Owner::Own(t) if !turns.contains(t) => turns.push(*t),
            Owner::Other(s) if !others.contains(s) => others.push(s.clone()),
            Owner::Own(_) | Owner::Other(_) => {}
        }
    }
    (turns.into(), others.into())
}
