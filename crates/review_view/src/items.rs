//! The items of the narrative list (docs/design/review_view.md「数据结构」
//! 「叙事列表的项」): a pure function of the snapshot and the prepared rows.
//!
//! The list is a GPUI `list`: every item is its own element and only the
//! visible ones are laid out, so a diff of 5000 lines is never laid out in
//! one frame. This module reads no side text and touches no window; its cost
//! is linear in `files + turns + items + prepared rows`.

use std::collections::HashMap;
use std::sync::Arc;

use lectern_services::changes::{ChangeDetail, SideText};
use lectern_services::review::{
    Alignment, CallRef, DiffRow, FileEntry, FileKey, FileRows, GroupKind, IncompleteKind,
    ReviewSnapshot, RowsKey, StepId, TextKey, TurnRef,
};
use lectern_services::session::ToolCallId;

/// The unchanged lines shown around a hunk until the user asks for more.
pub const DEFAULT_CONTEXT: u32 = 3;

/// The rows a placeholder never claims more than: a guess must not make the
/// list's scroll range absurd.
const MAX_ESTIMATE: u32 = 20_000;

/// Where a narrative entry sits in the snapshot: the index of its turn in
/// `ReviewSnapshot::turns` and the index of the entry in `TurnEntry::items`.
/// The items are built from one snapshot and only used with it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Pos {
    pub turn: u32,
    pub item: u32,
}

/// One element of the narrative list.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Item {
    /// The brief, as one block (12 entries at most).
    Brief,
    /// A notice line (`BaseHow::CurrentHead`, `Degraded`, `unclaimed`,
    /// `invalid_paths`).
    Notice(Notice),
    /// The earlier turns folded into one line.
    EarlierTurns {
        count: u32,
        expanded: bool,
    },
    /// A turn's title: `ix` indexes `ReviewSnapshot::turns`.
    Turn {
        at: TurnRef,
        ix: u32,
    },
    /// The prompt of a turn: the preview until the text arrives.
    Prompt {
        at: TurnRef,
        ix: u32,
    },
    /// What the agent said.
    Text {
        key: TextKey,
        at: Pos,
    },
    /// Content that is not human input.
    Injected {
        key: TextKey,
        at: Pos,
    },
    /// A step's title: file, counts, alignment, survival, the check.
    Step {
        id: StepId,
        at: Pos,
    },
    /// A file's title in the rule-grouped view; `ix` indexes `files`.
    File {
        key: FileKey,
        ix: u32,
    },
    /// A rule group's title (grouped view); `files` is its file count.
    Group {
        kind: GroupKind,
        files: u32,
    },
    /// The folded noise files, at the end.
    Noise {
        count: u32,
        expanded: bool,
    },
    /// A hunk's title line: attribution labels and the Drifted mark.
    Hunk {
        rows: RowsKey,
        hunk: u32,
        old: u32,
        new: u32,
    },
    /// One diff row; `ix` indexes `FileRows::rows`.
    Row {
        rows: RowsKey,
        ix: u32,
        old: Option<u32>,
        new: Option<u32>,
    },
    /// Unchanged lines folded: `ix` indexes `FileRows::rows`, `old` is the
    /// first folded line of the old side.
    Fold {
        rows: RowsKey,
        ix: u32,
        old: u32,
    },
    /// Rows that are not prepared yet, as tall as their estimated lines.
    Placeholder {
        rows: RowsKey,
        lines: u32,
    },
    /// A file shown without a full diff, with the reason.
    Incomplete {
        rows: RowsKey,
        kind: IncompleteKind,
    },
    Command {
        call: CallRef,
        at: Pos,
    },
    Subagent {
        session: u16,
        call: ToolCallId,
        at: Pos,
    },
    Tool {
        call: CallRef,
        at: Pos,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Notice {
    BaseCurrentHead,
    Unrecognized,
    Folder,
    Unclaimed(u32),
    InvalidPaths(u32),
}

/// A notice without its count: the stable part of an item's key.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum NoticeKind {
    BaseCurrentHead,
    Unrecognized,
    Folder,
    Unclaimed,
    InvalidPaths,
}

impl Notice {
    pub fn kind(self) -> NoticeKind {
        match self {
            Notice::BaseCurrentHead => NoticeKind::BaseCurrentHead,
            Notice::Unrecognized => NoticeKind::Unrecognized,
            Notice::Folder => NoticeKind::Folder,
            Notice::Unclaimed(_) => NoticeKind::Unclaimed,
            Notice::InvalidPaths(_) => NoticeKind::InvalidPaths,
        }
    }
}

impl NoticeKind {
    /// The node ID's key.
    pub fn name(self) -> &'static str {
        match self {
            NoticeKind::BaseCurrentHead => "base-current-head",
            NoticeKind::Unrecognized => "unrecognized",
            NoticeKind::Folder => "folder",
            NoticeKind::Unclaimed => "unclaimed",
            NoticeKind::InvalidPaths => "invalid-paths",
        }
    }
}

/// An item's stable key: after a refresh the list finds the scroll position
/// and the selection again by it. A row's key is its two line numbers, not
/// its index.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum ItemKey {
    Brief,
    Notice(NoticeKind),
    EarlierTurns,
    Turn(TurnRef),
    Prompt(TurnRef),
    Text(TextKey),
    Step(StepId),
    File(FileKey),
    Group(GroupKind),
    Noise,
    Hunk {
        rows: RowsKey,
        old: u32,
        new: u32,
    },
    Row {
        rows: RowsKey,
        old: Option<u32>,
        new: Option<u32>,
    },
    Fold {
        rows: RowsKey,
        old: u32,
    },
    Placeholder(RowsKey),
    Incomplete(RowsKey),
    Call(ToolCallId),
}

impl Item {
    pub fn key(&self) -> ItemKey {
        match self {
            Item::Brief => ItemKey::Brief,
            Item::Notice(notice) => ItemKey::Notice(notice.kind()),
            Item::EarlierTurns { .. } => ItemKey::EarlierTurns,
            Item::Turn { at, .. } => ItemKey::Turn(*at),
            Item::Prompt { at, .. } => ItemKey::Prompt(*at),
            Item::Text { key, .. } | Item::Injected { key, .. } => ItemKey::Text(*key),
            Item::Step { id, .. } => ItemKey::Step(*id),
            Item::File { key, .. } => ItemKey::File(key.clone()),
            Item::Group { kind, .. } => ItemKey::Group(*kind),
            Item::Noise { .. } => ItemKey::Noise,
            Item::Hunk { rows, old, new, .. } => ItemKey::Hunk {
                rows: rows.clone(),
                old: *old,
                new: *new,
            },
            Item::Row { rows, old, new, .. } => ItemKey::Row {
                rows: rows.clone(),
                old: *old,
                new: *new,
            },
            Item::Fold { rows, old, .. } => ItemKey::Fold {
                rows: rows.clone(),
                old: *old,
            },
            Item::Placeholder { rows, .. } => ItemKey::Placeholder(rows.clone()),
            Item::Incomplete { rows, .. } => ItemKey::Incomplete(rows.clone()),
            Item::Command { call, .. } | Item::Tool { call, .. } => {
                ItemKey::Call(call.call.clone())
            }
            Item::Subagent { call, .. } => ItemKey::Call(call.clone()),
        }
    }

    /// The rows an item belongs to: the diff of one step or file.
    pub fn rows_key(&self) -> Option<&RowsKey> {
        match self {
            Item::Hunk { rows, .. }
            | Item::Row { rows, .. }
            | Item::Fold { rows, .. }
            | Item::Placeholder { rows, .. }
            | Item::Incomplete { rows, .. } => Some(rows),
            _ => None,
        }
    }

    /// Whether the item is a place the hunk keys (J / K) stop at.
    pub fn is_hunk(&self) -> bool {
        matches!(self, Item::Hunk { .. })
    }
}

/// What `build` needs besides the snapshot and the rows.
pub struct BuildOptions<'a> {
    /// The turns before the latest seen one fold into one line (default
    /// (a) of 「未决问题」3); false after the user expanded them.
    pub earlier_collapsed: bool,
    /// The noise files appear in the narrative.
    pub noise_expanded: bool,
    /// The unchanged lines around a hunk, per file; absent: `DEFAULT_CONTEXT`.
    pub context: &'a HashMap<FileKey, u32>,
    /// The height of rows that were prepared once and were dropped again,
    /// in lines, so that a placeholder is as tall as what it replaced.
    pub known_lines: &'a HashMap<RowsKey, u32>,
}

/// The `RowsKey` of a step's diff.
pub fn step_rows_key(id: StepId, file: &FileKey, context: &HashMap<FileKey, u32>) -> RowsKey {
    RowsKey::Step {
        step: id,
        context: context.get(file).copied().unwrap_or(DEFAULT_CONTEXT),
    }
}

/// The `RowsKey` of a file's final diff.
pub fn final_rows_key(file: &FileKey, context: &HashMap<FileKey, u32>) -> RowsKey {
    RowsKey::Final {
        file: file.clone(),
        context: context.get(file).copied().unwrap_or(DEFAULT_CONTEXT),
    }
}

/// How many turns the fold at the front takes: the run of turns before the
/// first one that began after the latest seen mark, when at least one turn
/// is newer. Nothing seen yet, or nothing new: no fold (「未决问题」3).
pub fn earlier_count(snapshot: &ReviewSnapshot) -> u32 {
    let Some(turns) = snapshot.turns.as_deref() else {
        return 0;
    };
    let run = turns.iter().take_while(|t| !t.after_last_seen).count();
    if run == 0 || run == turns.len() {
        return 0;
    }
    run as u32
}

/// The list's items. The order is the design's: the brief, the notices, the
/// earlier turns, every turn with its prompt and entries (a step followed by
/// its rows), the noise last; or, without turns, every group with its files.
pub fn build(
    snapshot: &ReviewSnapshot,
    rows: &HashMap<RowsKey, Arc<FileRows>>,
    opts: &BuildOptions<'_>,
) -> Vec<Item> {
    let mut out = Vec::with_capacity(64 + rows.values().map(|r| r.rows.len() + 2).sum::<usize>());
    out.push(Item::Brief);
    push_notices(snapshot, &mut out);
    let files: HashMap<&FileKey, u32> = snapshot
        .files
        .iter()
        .enumerate()
        .map(|(ix, entry)| (&entry.key, ix as u32))
        .collect();
    let noise_count = snapshot
        .files
        .iter()
        .filter(|entry| entry.noise.is_some())
        .count() as u32;
    match snapshot.turns.as_deref() {
        Some(turns) => build_turns(snapshot, turns, &files, rows, opts, &mut out),
        None => build_groups(snapshot, rows, opts, &mut out),
    }
    if noise_count > 0 && snapshot.turns.is_some() {
        out.push(Item::Noise {
            count: noise_count,
            expanded: opts.noise_expanded,
        });
    }
    out
}

fn push_notices(snapshot: &ReviewSnapshot, out: &mut Vec<Item>) {
    use lectern_services::review::{BaseHow, Degraded};
    if snapshot
        .base
        .is_some_and(|base| base.how == BaseHow::CurrentHead)
    {
        out.push(Item::Notice(Notice::BaseCurrentHead));
    }
    match snapshot.degraded {
        Some(Degraded::UnrecognizedSession) => out.push(Item::Notice(Notice::Unrecognized)),
        Some(Degraded::FolderProject) => out.push(Item::Notice(Notice::Folder)),
        None => {}
    }
    if snapshot.unclaimed > 0 {
        out.push(Item::Notice(Notice::Unclaimed(snapshot.unclaimed)));
    }
    let invalid = snapshot.totals.invalid_paths.len() as u32;
    if invalid > 0 {
        out.push(Item::Notice(Notice::InvalidPaths(invalid)));
    }
}

fn build_turns(
    snapshot: &ReviewSnapshot,
    turns: &[lectern_services::review::TurnEntry],
    files: &HashMap<&FileKey, u32>,
    rows: &HashMap<RowsKey, Arc<FileRows>>,
    opts: &BuildOptions<'_>,
    out: &mut Vec<Item>,
) {
    use lectern_services::review::NarrativeItem;
    let fold = earlier_count(snapshot);
    if fold > 0 {
        out.push(Item::EarlierTurns {
            count: fold,
            expanded: !opts.earlier_collapsed,
        });
    }
    let earlier = if opts.earlier_collapsed { fold } else { 0 };
    for (turn_ix, turn) in turns.iter().enumerate().skip(earlier as usize) {
        let turn_ix = turn_ix as u32;
        out.push(Item::Turn {
            at: turn.at,
            ix: turn_ix,
        });
        if turn.prompt.is_some() || !turn.prompt_preview.is_empty() {
            out.push(Item::Prompt {
                at: turn.at,
                ix: turn_ix,
            });
        }
        for (item_ix, item) in turn.items.iter().enumerate() {
            let at = Pos {
                turn: turn_ix,
                item: item_ix as u32,
            };
            match item {
                NarrativeItem::AgentText(key) => out.push(Item::Text { key: *key, at }),
                NarrativeItem::Injected(key) => out.push(Item::Injected { key: *key, at }),
                NarrativeItem::Step(step) => {
                    let entry = files
                        .get(&step.file)
                        .map(|ix| &snapshot.files[*ix as usize]);
                    if !opts.noise_expanded && entry.is_some_and(|entry| entry.noise.is_some()) {
                        continue;
                    }
                    out.push(Item::Step { id: step.step, at });
                    let key = step_rows_key(step.step, &step.file, opts.context);
                    let estimate = step.added.saturating_add(step.removed).saturating_add(7);
                    push_rows(out, &key, rows, estimate, None, opts);
                }
                NarrativeItem::Command { call, .. } => out.push(Item::Command {
                    call: CallRef {
                        turn: turn.at,
                        call: call.clone(),
                    },
                    at,
                }),
                NarrativeItem::Subagent { session, call } => out.push(Item::Subagent {
                    session: *session,
                    call: call.clone(),
                    at,
                }),
                NarrativeItem::Tool { call, .. } => out.push(Item::Tool {
                    call: CallRef {
                        turn: turn.at,
                        call: call.clone(),
                    },
                    at,
                }),
            }
        }
    }
}

fn build_groups(
    snapshot: &ReviewSnapshot,
    rows: &HashMap<RowsKey, Arc<FileRows>>,
    opts: &BuildOptions<'_>,
    out: &mut Vec<Item>,
) {
    let mut noise_files = 0u32;
    for group in snapshot.groups.iter() {
        if matches!(group.kind, GroupKind::Noise(_)) {
            noise_files = noise_files.saturating_add(group.files.len() as u32);
            if !opts.noise_expanded {
                continue;
            }
        }
        out.push(Item::Group {
            kind: group.kind,
            files: group.files.len() as u32,
        });
        for &ix in group.files.iter() {
            let Some(entry) = snapshot.files.get(ix as usize) else {
                continue;
            };
            out.push(Item::File {
                key: entry.key.clone(),
                ix,
            });
            let key = final_rows_key(&entry.key, opts.context);
            let estimate = final_estimate(entry, opts.context);
            push_rows(out, &key, rows, estimate, Some(entry), opts);
        }
    }
    if noise_files > 0 {
        out.push(Item::Noise {
            count: noise_files,
            expanded: opts.noise_expanded,
        });
    }
}

/// The rows of a `Final` diff, estimated from the change list's hunks.
fn final_estimate(entry: &FileEntry, context: &HashMap<FileKey, u32>) -> u32 {
    match &entry.change.detail {
        ChangeDetail::Text {
            hunks,
            added,
            removed,
        } => {
            let ctx = context
                .get(&entry.key)
                .copied()
                .unwrap_or(DEFAULT_CONTEXT)
                .min(MAX_ESTIMATE);
            let per_hunk = ctx.saturating_mul(2).saturating_add(1);
            added
                .saturating_add(*removed)
                .saturating_add(per_hunk.saturating_mul(hunks.len() as u32))
        }
        _ => 1,
    }
    .min(MAX_ESTIMATE)
}

/// The reason a file has no full diff, from the change list alone.
pub fn incomplete_of_detail(detail: &ChangeDetail) -> Option<IncompleteKind> {
    match detail {
        ChangeDetail::Truncated { .. } => Some(IncompleteKind::Truncated),
        ChangeDetail::TooLarge => Some(IncompleteKind::TooLarge),
        ChangeDetail::Binary { .. } => Some(IncompleteKind::Binary),
        ChangeDetail::Listed => Some(IncompleteKind::Listed),
        ChangeDetail::Text { .. }
        | ChangeDetail::ModeOnly
        | ChangeDetail::StagedOnly
        | ChangeDetail::NoDiff => None,
    }
}

/// The reason prepared rows are empty although the file changed: a side
/// whose content is not text.
pub fn incomplete_of_rows(rows: &FileRows) -> Option<IncompleteKind> {
    let kind = |text: &SideText| match text {
        SideText::TooLarge { .. } => Some(IncompleteKind::TooLarge),
        SideText::NotRetained => Some(IncompleteKind::NotRetained),
        SideText::Unavailable => Some(IncompleteKind::Unavailable),
        SideText::Absent | SideText::Text(_) => None,
    };
    kind(&rows.new.text).or_else(|| kind(&rows.old.text))
}

/// Pushes the items of one diff: the prepared rows, a note for a file
/// without a full diff, or a placeholder.
fn push_rows(
    out: &mut Vec<Item>,
    key: &RowsKey,
    rows: &HashMap<RowsKey, Arc<FileRows>>,
    estimate: u32,
    entry: Option<&FileEntry>,
    opts: &BuildOptions<'_>,
) {
    if let Some(kind) = entry.and_then(|entry| incomplete_of_detail(&entry.change.detail)) {
        out.push(Item::Incomplete {
            rows: key.clone(),
            kind,
        });
        return;
    }
    match rows.get(key) {
        Some(rows) => rows_items(out, key, rows),
        None => out.push(Item::Placeholder {
            rows: key.clone(),
            lines: opts
                .known_lines
                .get(key)
                .copied()
                .unwrap_or(estimate)
                .max(1),
        }),
    }
}

/// The items of prepared rows: a hunk's title before its first row, one item
/// per diff row, a fold for folded lines, or a note when there is nothing to
/// show because a side is not text.
pub fn rows_items(out: &mut Vec<Item>, key: &RowsKey, rows: &FileRows) {
    if rows.rows.is_empty() {
        if let Some(kind) = incomplete_of_rows(rows) {
            out.push(Item::Incomplete {
                rows: key.clone(),
                kind,
            });
        }
        return;
    }
    let mut hunk: Option<u32> = None;
    for (ix, row) in rows.rows.iter().enumerate() {
        let ix = ix as u32;
        let (old, new, row_hunk) = match row {
            DiffRow::Context { old, new, hunk, .. } => (Some(*old), Some(*new), *hunk),
            DiffRow::Removed { old, hunk, .. } => (Some(*old), None, Some(*hunk)),
            DiffRow::Added { new, hunk, .. } => (None, Some(*new), Some(*hunk)),
            DiffRow::Folded { old, .. } => {
                out.push(Item::Fold {
                    rows: key.clone(),
                    ix,
                    old: old.start,
                });
                continue;
            }
        };
        if let Some(h) = row_hunk
            && hunk != Some(h)
        {
            hunk = Some(h);
            if let Some(info) = rows.hunks.get(h as usize) {
                out.push(Item::Hunk {
                    rows: key.clone(),
                    hunk: h,
                    old: info.old.start,
                    new: info.new.start,
                });
            }
        }
        out.push(Item::Row {
            rows: key.clone(),
            ix,
            old,
            new,
        });
    }
}

/// How many items `rows_items` yields for these rows: what a placeholder is
/// as tall as after the rows were dropped again.
pub fn rows_lines(key: &RowsKey, rows: &FileRows) -> u32 {
    let mut scratch = Vec::new();
    rows_items(&mut scratch, key, rows);
    scratch.len() as u32
}

/// The file an item shows, as an index into `files`, for the keys that name
/// a file: used to find "the current file" for V.
pub fn item_file(item: &Item, snapshot: &ReviewSnapshot) -> Option<FileKey> {
    match item {
        Item::Step { at, .. } => step_entry(snapshot, *at).map(|step| step.file.clone()),
        Item::File { key, .. } => Some(key.clone()),
        Item::Hunk { rows, .. }
        | Item::Row { rows, .. }
        | Item::Fold { rows, .. }
        | Item::Placeholder { rows, .. }
        | Item::Incomplete { rows, .. } => rows_file(snapshot, rows),
        _ => None,
    }
}

/// The step at a position, None if the position is out of range or does not
/// hold a step.
pub fn step_entry(
    snapshot: &ReviewSnapshot,
    at: Pos,
) -> Option<&lectern_services::review::StepEntry> {
    match snapshot
        .turns
        .as_deref()?
        .get(at.turn as usize)?
        .items
        .get(at.item as usize)?
    {
        lectern_services::review::NarrativeItem::Step(step) => Some(step),
        _ => None,
    }
}

/// The file whose diff `rows` is.
pub fn rows_file(snapshot: &ReviewSnapshot, rows: &RowsKey) -> Option<FileKey> {
    match rows {
        RowsKey::Final { file, .. } => Some(file.clone()),
        RowsKey::Step { step, .. } => snapshot
            .turns
            .as_deref()?
            .iter()
            .flat_map(|turn| turn.items.iter())
            .find_map(|item| match item {
                lectern_services::review::NarrativeItem::Step(entry) if entry.step == *step => {
                    Some(entry.file.clone())
                }
                _ => None,
            }),
    }
}

/// Whether a step's rows are "as recorded" (every hunk drifts): the
/// alignment is `Unaligned`.
pub fn step_unaligned(snapshot: &ReviewSnapshot, step: StepId) -> bool {
    snapshot
        .turns
        .as_deref()
        .into_iter()
        .flat_map(|turns| turns.iter())
        .flat_map(|turn| turn.items.iter())
        .any(|item| {
            matches!(
                item,
                lectern_services::review::NarrativeItem::Step(entry)
                    if entry.step == step && entry.alignment == Alignment::Unaligned
            )
        })
}
