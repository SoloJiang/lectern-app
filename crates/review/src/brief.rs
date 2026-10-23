//! The rule brief (docs/design/review.md「简报规则」「简报规则的扩展点」):
//! the items of one snapshot from its files, their seen states and their
//! attribution, plus the providers' items after reconciliation. Pure: no
//! IO, no lock, nothing of the engine's; the engine hands it what the
//! snapshot has.

use std::collections::HashMap;

use lectern_services::changes::{ChangeDetail, ChangeStatus, FileChange, SideIdentity};
use lectern_services::review::{
    AnalysisState, Brief, BriefItem, BriefRule, DiffSide, FileKey, IncompleteKind, LineAnchor,
    NoiseKind, ProvidedItem, SeenState, SensitiveKind,
};

/// Items kept; the rest are counted in `omitted`.
pub(crate) const MAX_ITEMS: usize = 12;
/// Items kept of one provider.
pub(crate) const MAX_PROVIDED: usize = 8;

/// One file of the snapshot, in progress order.
pub(crate) struct FileInput<'a> {
    pub key: &'a FileKey,
    pub change: &'a FileChange,
    pub noise: Option<NoiseKind>,
    pub seen: SeenState,
    /// A hunk with lines of no provenance, or a step that did not align.
    pub drifted: bool,
    /// The first line of the first hunk with lines of no provenance, when
    /// the hunks are known.
    pub drifted_at: Option<LineAnchor>,
    /// An own session's edit of it failed, was denied or was interrupted.
    pub edit_failed: bool,
    /// Claimed through commands only: the calls (session, call) that ran.
    pub command_only: &'a [(u32, u32)],
}

/// What one provider gave for the current request.
pub(crate) struct Provided<'a> {
    pub provider: &'a str,
    pub items: &'a [ProvidedItem],
}

/// The brief of a snapshot. `files` is in progress order; the items of a
/// rule follow it. Noise files only count in their `Noise` item: they are
/// folded and do not trigger the other rules.
pub(crate) fn build(
    files: &[FileInput<'_>],
    provided: &[Provided<'_>],
    pending_providers: u32,
    analysis: AnalysisState,
) -> Brief {
    // (critical, rank, file position, item)
    let mut items: Vec<(bool, u32, usize, BriefItem)> = Vec::new();
    let mut push = |critical: bool, position: usize, item: BriefItem| {
        items.push((critical, rank(&item.rule), position, item));
    };
    // Per command call: the first file it claimed and how many.
    let mut commands: Vec<((u32, u32), usize, u32)> = Vec::new();
    let mut command_index: HashMap<(u32, u32), usize> = HashMap::new();
    let mut deleted = 0u32;
    let mut incomplete: [u32; 7] = [0; 7];
    let mut noise: [u32; 3] = [0; 3];
    for (position, f) in files.iter().enumerate() {
        if let Some(kind) = f.noise {
            noise[noise_index(kind)] += 1;
            continue;
        }
        if let SeenState::ChangedSinceSeen { .. } = f.seen {
            push(
                false,
                position,
                file_item(BriefRule::ChangedSinceSeen, f.key, None),
            );
        }
        if f.drifted {
            push(
                false,
                position,
                file_item(BriefRule::Drifted, f.key, f.drifted_at),
            );
        }
        if f.edit_failed {
            push(
                false,
                position,
                file_item(BriefRule::EditFailed, f.key, None),
            );
        }
        for &call in f.command_only {
            match command_index.get(&call) {
                Some(&n) => commands[n].2 += 1,
                None => {
                    command_index.insert(call, commands.len());
                    commands.push((call, position, 1));
                }
            }
        }
        if let Some(kind) = crate::noise::sensitive(f.change) {
            push(
                sensitive_is_critical(kind),
                position,
                file_item(BriefRule::Sensitive(kind), f.key, None),
            );
        }
        if matches!(f.change.status, ChangeStatus::Deleted) && !f.change.untracked {
            deleted += 1;
        }
        for kind in incomplete_kinds(f.change) {
            incomplete[incomplete_index(kind)] += 1;
        }
    }
    // One item per command, anchored on the first file it claimed.
    for (_, position, count) in commands {
        push(
            false,
            position,
            BriefItem {
                rule: BriefRule::CommandOnly,
                critical: false,
                file: Some(files[position].key.clone()),
                line: None,
                count,
            },
        );
    }
    if deleted > 0 {
        push(true, usize::MAX, counted(BriefRule::Deleted, true, deleted));
    }
    for (i, &count) in incomplete.iter().enumerate() {
        if count > 0 {
            let kind = INCOMPLETE[i];
            push(
                false,
                usize::MAX,
                counted(BriefRule::Incomplete(kind), false, count),
            );
        }
    }
    for (i, &count) in noise.iter().enumerate() {
        if count > 0 {
            let kind = NOISE[i];
            push(
                false,
                usize::MAX,
                counted(BriefRule::Noise(kind), false, count),
            );
        }
    }
    // The providers' items, reconciled against the snapshot.
    let positions: HashMap<&FileKey, usize> =
        files.iter().enumerate().map(|(n, f)| (f.key, n)).collect();
    for p in provided {
        for item in p.items.iter().take(MAX_PROVIDED) {
            let (file, position) = match &item.file {
                // A file not in the snapshot: dropped.
                Some(key) => match positions.get(key) {
                    Some(&n) => (Some(key.clone()), n),
                    None => continue,
                },
                None => (None, usize::MAX),
            };
            // A line not shown in the final change: a file note.
            let line = item
                .line
                .filter(|anchor| position != usize::MAX && shown(files[position].change, *anchor));
            push(
                item.critical,
                position,
                BriefItem {
                    rule: BriefRule::Provided {
                        provider: p.provider.into(),
                        rule: item.rule.clone(),
                        args: item.args.clone(),
                    },
                    critical: item.critical,
                    file,
                    line,
                    count: 1,
                },
            );
        }
    }
    // Critical first, then the table's order, then the files' progress
    // order; stable, so a rule's aggregated items keep their order.
    items.sort_by_key(|(critical, rank, position, _)| (!*critical, *rank, *position));
    let omitted = items.len().saturating_sub(MAX_ITEMS) as u32;
    items.truncate(MAX_ITEMS);
    Brief {
        items: items.into_iter().map(|(_, _, _, item)| item).collect(),
        omitted,
        pending_providers,
        analysis,
    }
}

fn file_item(rule: BriefRule, key: &FileKey, line: Option<LineAnchor>) -> BriefItem {
    let critical = matches!(rule, BriefRule::Sensitive(kind) if sensitive_is_critical(kind));
    BriefItem {
        rule,
        critical,
        file: Some(key.clone()),
        line,
        count: 1,
    }
}

fn counted(rule: BriefRule, critical: bool, count: u32) -> BriefItem {
    BriefItem {
        rule,
        critical,
        file: None,
        line: None,
        count,
    }
}

/// Whether `anchor` is a line of a hunk shown for `change`.
fn shown(change: &FileChange, anchor: LineAnchor) -> bool {
    let ChangeDetail::Text { hunks, .. } = &change.detail else {
        return false;
    };
    hunks.iter().any(|h| match anchor.side {
        DiffSide::Old => h.old.contains(&anchor.line),
        DiffSide::New => h.new.contains(&anchor.line),
    })
}

/// The table's order (「简报规则」); `Provided` after every built-in rule.
fn rank(rule: &BriefRule) -> u32 {
    match rule {
        BriefRule::ChangedSinceSeen => 0,
        BriefRule::Drifted => 1,
        BriefRule::EditFailed => 2,
        BriefRule::CommandOnly => 3,
        BriefRule::Sensitive(kind) => {
            4 + match kind {
                SensitiveKind::AgentInstructions => 0,
                SensitiveKind::Ci => 1,
                SensitiveKind::Hooks => 2,
                SensitiveKind::GitMetadata => 3,
                SensitiveKind::Symlink => 4,
                SensitiveKind::Executable => 5,
                SensitiveKind::Submodule => 6,
            }
        }
        BriefRule::Deleted => 11,
        BriefRule::Incomplete(kind) => 12 + incomplete_index(*kind) as u32,
        BriefRule::Noise(kind) => 19 + noise_index(*kind) as u32,
        BriefRule::Provided { .. } => 22,
    }
}

/// `Executable` and `Submodule` are shown, not critical (「简报规则」).
pub(crate) fn sensitive_is_critical(kind: SensitiveKind) -> bool {
    !matches!(kind, SensitiveKind::Executable | SensitiveKind::Submodule)
}

const INCOMPLETE: [IncompleteKind; 7] = [
    IncompleteKind::Truncated,
    IncompleteKind::TooLarge,
    IncompleteKind::Binary,
    IncompleteKind::NotRetained,
    IncompleteKind::Filtered,
    IncompleteKind::Unavailable,
    IncompleteKind::Listed,
];

fn incomplete_index(kind: IncompleteKind) -> usize {
    INCOMPLETE.iter().position(|k| *k == kind).unwrap_or(0)
}

const NOISE: [NoiseKind; 3] = [
    NoiseKind::Lockfile,
    NoiseKind::Generated,
    NoiseKind::Snapshot,
];

fn noise_index(kind: NoiseKind) -> usize {
    NOISE.iter().position(|k| *k == kind).unwrap_or(0)
}

/// The `Incomplete` kinds of a change: its detail's, and either side's
/// identity. A change can be in several (a listed file whose content was
/// not retained).
fn incomplete_kinds(change: &FileChange) -> Vec<IncompleteKind> {
    let mut kinds = Vec::new();
    match &change.detail {
        ChangeDetail::Truncated { .. } => kinds.push(IncompleteKind::Truncated),
        ChangeDetail::TooLarge => kinds.push(IncompleteKind::TooLarge),
        ChangeDetail::Binary { .. } => kinds.push(IncompleteKind::Binary),
        ChangeDetail::Listed => kinds.push(IncompleteKind::Listed),
        ChangeDetail::Text { .. }
        | ChangeDetail::ModeOnly
        | ChangeDetail::StagedOnly
        | ChangeDetail::NoDiff => {}
    }
    for side in [&change.old, &change.new].into_iter().flatten() {
        let kind = match side.content {
            SideIdentity::NotRetained(_) => IncompleteKind::NotRetained,
            SideIdentity::Filtered(_) => IncompleteKind::Filtered,
            SideIdentity::Unavailable(_) => IncompleteKind::Unavailable,
            SideIdentity::Blob(_) | SideIdentity::Unhashed { .. } => continue,
        };
        if !kinds.contains(&kind) {
            kinds.push(kind);
        }
    }
    kinds
}

#[cfg(test)]
mod tests;
