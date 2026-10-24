//! Test and bench support (`test-support` feature, docs/design/review_view.md
//! 「接口」): a fake review service that gives scripted snapshots, rows and
//! texts and records the commands, and a synthetic snapshot.
//!
//! This crate cannot depend on `lectern-review` (plugins do not depend on
//! plugins, architecture rule 7), so the fake stands in for it. The GPUI
//! harness (a window, the fake contributions) is in this crate's own tests.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};

use lectern_kernel::ActivationToken;
use lectern_kernel::Emitter;
use lectern_services::changes::{
    BaseSpec, ChangeDetail, ChangePath, ChangeStatus, ChangeTotals, DiffHunk, FileChange, SideText,
};
use lectern_services::project::{ProjectKey, WorktreeKey};
use lectern_services::review::{
    Alignment, AnalysisState, AttributionState, BaseHow, BaseInfo, Brief, BriefItem, BriefRule,
    BriefRules, BriefRulesRegistration, DiffRow, FileAttribution, FileEntry, FileKey, FileRows,
    Group, GroupKind, HunkAttribution, HunkInfo, LoadState, NarrativeItem, Need, Overview,
    OverviewInterest, Progress, Review, ReviewCommand, ReviewEvent, ReviewId, ReviewSnapshot,
    ReviewTarget, RowsKey, RowsSide, SeenState, SensitiveKind, SessionRef, StepEntry, StepId,
    Survival, TextKey, TurnEntry, TurnRef,
};
use lectern_services::session::{
    Agent, ContentRef, SessionKey, SourceId, Timestamp, ToolCallId, TurnId,
};

/// The ID of the first review a [`FakeReview`] opens.
pub const FIRST_ID: ReviewId = ReviewId(1);

struct Entry {
    target: ReviewTarget,
    snapshot: Option<Arc<ReviewSnapshot>>,
    rows: HashMap<RowsKey, Arc<FileRows>>,
    texts: HashMap<TextKey, Arc<str>>,
    /// The keys of the latest `Prepare`: only these answer.
    prepared: HashSet<Need>,
    last_needs: Vec<Need>,
    refs: u32,
}

#[derive(Default)]
struct Inner {
    next: u64,
    reviews: HashMap<ReviewId, Entry>,
    commands: Vec<(ReviewId, ReviewCommand)>,
    closed: Vec<ReviewId>,
    rows_calls: usize,
    text_calls: usize,
    snapshot_calls: usize,
    emitter: Option<Emitter>,
    auto_prepare: bool,
}

/// A scripted `dyn Review`. Snapshots, rows and texts are set per
/// `ReviewId` (the first review opened is `FIRST_ID`); `rows` and `text`
/// answer only for the keys of the latest `Prepare`, as the real service
/// does, and `MarkSeen`, `MarkUnseen` and `LooksGood` swap `seen` before they
/// return.
#[derive(Default)]
pub struct FakeReview {
    inner: Mutex<Inner>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

impl FakeReview {
    pub fn new() -> Arc<FakeReview> {
        Arc::new(FakeReview::default())
    }

    /// The emitter events go out through (`kernel.emitter()`).
    pub fn set_emitter(&self, emitter: Emitter) {
        lock(&self.inner).emitter = Some(emitter);
    }

    /// Whether `Prepare` answers with a `Prepared` event at once.
    pub fn set_auto_prepare(&self, on: bool) {
        lock(&self.inner).auto_prepare = on;
    }

    fn entry<R>(&self, id: ReviewId, f: impl FnOnce(&mut Entry) -> R) -> R {
        let mut inner = lock(&self.inner);
        let entry = inner.reviews.entry(id).or_insert_with(|| Entry {
            target: placeholder_target(),
            snapshot: None,
            rows: HashMap::new(),
            texts: HashMap::new(),
            prepared: HashSet::new(),
            last_needs: Vec::new(),
            refs: 0,
        });
        f(entry)
    }

    /// The snapshot `snapshot()` returns from now on. No event.
    pub fn set_snapshot(&self, id: ReviewId, snapshot: ReviewSnapshot) {
        self.entry(id, |entry| entry.snapshot = Some(Arc::new(snapshot)));
    }

    pub fn set_rows(&self, id: ReviewId, key: RowsKey, rows: FileRows) {
        self.entry(id, |entry| {
            entry.rows.insert(key, Arc::new(rows));
        });
    }

    pub fn set_text(&self, id: ReviewId, key: TextKey, text: &str) {
        self.entry(id, |entry| {
            entry.texts.insert(key, Arc::from(text));
        });
    }

    /// Sets every rows entry of a fixture.
    pub fn set_all_rows(&self, id: ReviewId, rows: Vec<(RowsKey, FileRows)>) {
        for (key, rows) in rows {
            self.set_rows(id, key, rows);
        }
    }

    /// Emits `ReviewEvent::Changed` for the snapshot now set, which gets the
    /// next generation; returns it.
    pub fn emit_changed(&self, id: ReviewId) -> u64 {
        let (generation, emitter) = {
            let mut inner = lock(&self.inner);
            let emitter = inner.emitter.clone();
            let generation = match inner
                .reviews
                .get_mut(&id)
                .and_then(|entry| entry.snapshot.as_mut())
            {
                Some(snapshot) => {
                    let mut next = (**snapshot).clone();
                    next.generation += 1;
                    let generation = next.generation;
                    *snapshot = Arc::new(next);
                    generation
                }
                None => 0,
            };
            (generation, emitter)
        };
        if let Some(emitter) = emitter {
            emitter.emit(ReviewEvent::Changed { id, generation });
        }
        generation
    }

    /// Emits `ReviewEvent::Changed` with the snapshot's own generation: what
    /// a snapshot swapped by `set_snapshot` announces.
    pub fn announce(&self, id: ReviewId) {
        let (generation, emitter) = {
            let inner = lock(&self.inner);
            let generation = inner
                .reviews
                .get(&id)
                .and_then(|entry| entry.snapshot.as_ref())
                .map_or(0, |snapshot| snapshot.generation);
            (generation, inner.emitter.clone())
        };
        if let Some(emitter) = emitter {
            emitter.emit(ReviewEvent::Changed { id, generation });
        }
    }

    /// Emits any event, for the cases the other methods do not cover.
    pub fn emit_event(&self, event: ReviewEvent) {
        let emitter = lock(&self.inner).emitter.clone();
        if let Some(emitter) = emitter {
            emitter.emit(event);
        }
    }

    /// Emits `ReviewEvent::Prepared`; returns the snapshot's generation.
    pub fn emit_prepared(&self, id: ReviewId) -> u64 {
        let (generation, emitter) = {
            let inner = lock(&self.inner);
            let generation = inner
                .reviews
                .get(&id)
                .and_then(|entry| entry.snapshot.as_ref())
                .map_or(0, |snapshot| snapshot.generation);
            (generation, inner.emitter.clone())
        };
        if let Some(emitter) = emitter {
            emitter.emit(ReviewEvent::Prepared { id, generation });
        }
        generation
    }

    /// The commands received, in order.
    pub fn commands(&self) -> Vec<(ReviewId, ReviewCommand)> {
        lock(&self.inner).commands.clone()
    }

    /// The needs of the latest `Prepare` for a review.
    pub fn last_needs(&self, id: ReviewId) -> Vec<Need> {
        lock(&self.inner)
            .reviews
            .get(&id)
            .map(|entry| entry.last_needs.clone())
            .unwrap_or_default()
    }

    /// How many `Prepare` commands a review has received.
    pub fn prepares(&self, id: ReviewId) -> usize {
        lock(&self.inner)
            .commands
            .iter()
            .filter(|(review, command)| {
                *review == id && matches!(command, ReviewCommand::Prepare(_))
            })
            .count()
    }

    pub fn closed(&self) -> Vec<ReviewId> {
        lock(&self.inner).closed.clone()
    }

    pub fn rows_calls(&self) -> usize {
        lock(&self.inner).rows_calls
    }

    pub fn text_calls(&self) -> usize {
        lock(&self.inner).text_calls
    }

    pub fn snapshot_calls(&self) -> usize {
        lock(&self.inner).snapshot_calls
    }

    pub fn opened(&self) -> Vec<ReviewTarget> {
        let inner = lock(&self.inner);
        let mut ids: Vec<&ReviewId> = inner.reviews.keys().collect();
        ids.sort();
        ids.into_iter()
            .map(|id| inner.reviews[id].target.clone())
            .collect()
    }
}

fn placeholder_target() -> ReviewTarget {
    ReviewTarget::Outside {
        project: ProjectKey::Folder {
            root: Arc::from(std::path::Path::new("/fake")),
        },
        worktree: WorktreeKey(Arc::from(std::path::Path::new("/fake"))),
    }
}

impl Review for FakeReview {
    fn open(&self, target: ReviewTarget) -> ReviewId {
        let mut inner = lock(&self.inner);
        if let Some((id, entry)) = inner
            .reviews
            .iter_mut()
            .find(|(_, entry)| entry.target == target && entry.refs > 0)
        {
            entry.refs += 1;
            return *id;
        }
        inner.next += 1;
        let id = ReviewId(inner.next);
        let entry = inner.reviews.entry(id).or_insert_with(|| Entry {
            target: target.clone(),
            snapshot: None,
            rows: HashMap::new(),
            texts: HashMap::new(),
            prepared: HashSet::new(),
            last_needs: Vec::new(),
            refs: 0,
        });
        entry.target = target.clone();
        entry.refs += 1;
        if entry.snapshot.is_none() {
            entry.snapshot = Some(Arc::new(loading(id, target)));
        }
        id
    }

    fn snapshot(&self, id: ReviewId) -> Option<Arc<ReviewSnapshot>> {
        let mut inner = lock(&self.inner);
        inner.snapshot_calls += 1;
        inner.reviews.get(&id)?.snapshot.clone()
    }

    fn command(&self, id: ReviewId, command: ReviewCommand) {
        let (emitter, announce, prepared) = {
            let mut inner = lock(&self.inner);
            inner.commands.push((id, command.clone()));
            let auto = inner.auto_prepare;
            let emitter = inner.emitter.clone();
            let mut announce = None;
            let mut prepared = false;
            if let Some(entry) = inner.reviews.get_mut(&id) {
                match &command {
                    ReviewCommand::MarkSeen(file) => {
                        announce =
                            swap_seen(entry, |_, key| (key == file).then_some(SeenState::Seen));
                    }
                    ReviewCommand::MarkUnseen(file) => {
                        announce =
                            swap_seen(entry, |_, key| (key == file).then_some(SeenState::Unseen));
                    }
                    ReviewCommand::LooksGood => {
                        announce = swap_seen(entry, |_, _| Some(SeenState::Seen));
                    }
                    ReviewCommand::Prepare(needs) => {
                        entry.prepared = needs.iter().cloned().collect();
                        entry.last_needs = needs.to_vec();
                        prepared = auto;
                    }
                    _ => {}
                }
            }
            (emitter, announce, prepared)
        };
        if let Some(emitter) = emitter {
            if let Some(generation) = announce {
                emitter.emit(ReviewEvent::Changed { id, generation });
            }
            if prepared {
                let generation = lock(&self.inner)
                    .reviews
                    .get(&id)
                    .and_then(|entry| entry.snapshot.as_ref())
                    .map_or(0, |snapshot| snapshot.generation);
                emitter.emit(ReviewEvent::Prepared { id, generation });
            }
        }
    }

    fn rows(&self, id: ReviewId, key: &RowsKey) -> Option<Arc<FileRows>> {
        let mut inner = lock(&self.inner);
        inner.rows_calls += 1;
        let entry = inner.reviews.get(&id)?;
        if !entry.prepared.contains(&Need::Rows(key.clone())) {
            return None;
        }
        entry.rows.get(key).cloned()
    }

    fn text(&self, id: ReviewId, key: &TextKey) -> Option<Arc<str>> {
        let mut inner = lock(&self.inner);
        inner.text_calls += 1;
        let entry = inner.reviews.get(&id)?;
        if !entry.prepared.contains(&Need::Text(*key)) {
            return None;
        }
        entry.texts.get(key).cloned()
    }

    fn close(&self, id: ReviewId) {
        let mut inner = lock(&self.inner);
        inner.closed.push(id);
        if let Some(entry) = inner.reviews.get_mut(&id) {
            entry.refs = entry.refs.saturating_sub(1);
        }
    }

    fn overview(&self, _: &ProjectKey) -> Option<Arc<Overview>> {
        None
    }

    fn watch_overview(&self, _: &ProjectKey) -> OverviewInterest {
        OverviewInterest::new(|| {})
    }

    fn register_brief_rules(
        &self,
        _: Arc<dyn BriefRules>,
        _: ActivationToken,
    ) -> BriefRulesRegistration {
        BriefRulesRegistration::new(|| {})
    }
}

/// Swaps `seen` as the real service does before `command` returns; the new
/// generation, if anything changed.
fn swap_seen(
    entry: &mut Entry,
    change: impl Fn(usize, &FileKey) -> Option<SeenState>,
) -> Option<u64> {
    let snapshot = entry.snapshot.as_ref()?;
    let mut next = (**snapshot).clone();
    let mut seen: Vec<SeenState> = next.seen.to_vec();
    let mut changed = false;
    for (ix, file) in next.files.iter().enumerate() {
        if let Some(state) = change(ix, &file.key)
            && seen[ix] != state
        {
            seen[ix] = state;
            changed = true;
        }
    }
    if !changed {
        return None;
    }
    next.progress = Progress {
        total: seen.len() as u32,
        seen: seen.iter().filter(|s| matches!(s, SeenState::Seen)).count() as u32,
        changed_since_seen: seen
            .iter()
            .filter(|s| matches!(s, SeenState::ChangedSinceSeen { .. }))
            .count() as u32,
    };
    next.seen = Arc::from(seen);
    next.generation += 1;
    let generation = next.generation;
    entry.snapshot = Some(Arc::new(next));
    Some(generation)
}

fn loading(id: ReviewId, target: ReviewTarget) -> ReviewSnapshot {
    let mut snapshot = crate::model::blank(id, target, LoadState::Loading);
    snapshot.generation = 1;
    snapshot
}

// ----- the synthetic snapshot ---------------------------------------------

fn path(tag: &str, i: u32) -> ChangePath {
    ChangePath(Arc::from(format!("src/{tag}file_{i:03}.rs").into_bytes()))
}

/// The key of fixture file `i`.
pub fn file_key(i: u32) -> FileKey {
    file_key_tagged("", i)
}

/// The key of fixture file `i` of a fixture whose names carry `tag`.
pub fn file_key_tagged(tag: &str, i: u32) -> FileKey {
    FileKey {
        old: Some(path(tag, i)),
        new: Some(path(tag, i)),
    }
}

/// The session the fixture is of.
pub fn session_key() -> SessionKey {
    SessionKey {
        source: SourceId(Arc::from("fake")),
        id: Arc::from("11111111-2222-4333-8444-555555555555"),
    }
}

/// The `TextKey` of the prompt of fixture turn `turn` (0-based).
pub fn prompt_key(turn: u32) -> TextKey {
    TextKey {
        session: 0,
        at: ContentRef::new(u64::from(turn) * 1000, 100, 0),
    }
}

/// The `TextKey` of the agent text of fixture turn `turn`.
pub fn agent_key(turn: u32) -> TextKey {
    TextKey {
        session: 0,
        at: ContentRef::new(u64::from(turn) * 1000 + 500, 100, 1),
    }
}

/// The side texts of a fixture file: `lines` lines of Rust-looking source,
/// the old side with `old_lines` of them.
fn side_bytes(lines: u32, tag: &str) -> (Arc<[u8]>, Vec<u64>) {
    let mut text = Vec::new();
    let mut offsets = Vec::new();
    for n in 0..lines {
        offsets.push(text.len() as u64);
        text.extend_from_slice(format!("    let {tag}_{n} = compute({n});\n").as_bytes());
    }
    (Arc::from(text), offsets)
}

/// The rows of one fixture file: `n` rows in a single hunk, a context row,
/// a removed row, then two added rows, again and again.
pub fn fixture_rows(key: RowsKey, turn: TurnRef, n: u32, tag: &str) -> FileRows {
    let (old_text, old_offsets) = side_bytes(n, &format!("{tag}old"));
    let (new_text, new_offsets) = side_bytes(n, &format!("{tag}new"));
    let mut rows: Vec<DiffRow> = Vec::new();
    let (mut old, mut new) = (0u32, 0u32);
    for i in 0..n {
        rows.push(match i % 4 {
            0 => {
                let row = DiffRow::Context {
                    old,
                    new,
                    old_byte: old_offsets.get(old as usize).copied(),
                    new_byte: new_offsets.get(new as usize).copied(),
                    hunk: Some(0),
                };
                old += 1;
                new += 1;
                row
            }
            1 => {
                let row = DiffRow::Removed {
                    old,
                    pair: None,
                    old_byte: old_offsets.get(old as usize).copied(),
                    hunk: 0,
                };
                old += 1;
                row
            }
            _ => {
                let row = DiffRow::Added {
                    new,
                    pair: None,
                    new_byte: new_offsets.get(new as usize).copied(),
                    hunk: 0,
                };
                new += 1;
                row
            }
        });
    }
    FileRows {
        key,
        old: RowsSide {
            oid: None,
            text: SideText::Text(old_text),
        },
        new: RowsSide {
            oid: None,
            text: SideText::Text(new_text),
        },
        rows: Arc::from(rows),
        hunks: Arc::from(vec![HunkInfo {
            old: 0..old,
            new: 0..new,
            attribution: HunkAttribution {
                lines: true,
                turns: Arc::from(vec![turn]),
                others: Arc::from(Vec::new()),
                drifted: 0,
            },
        }]),
        inline: Arc::from(Vec::new()),
    }
}

/// A synthetic review of a session: `files` files, `turns` turns, every file
/// changed by one step in turn `file % turns`, `rows_per_file` rows in each
/// step's diff (and in each file's final diff); the rows of every step and
/// of every file are returned with the snapshot. The brief has two entries.
pub fn snapshot_fixture(
    files: u32,
    turns: u32,
    rows_per_file: u32,
) -> (ReviewSnapshot, Vec<(RowsKey, FileRows)>) {
    snapshot_fixture_tagged(files, turns, rows_per_file, "")
}

/// `snapshot_fixture` with `tag` in every name a log must not carry: paths,
/// the prompt previews, the tool name, the diff lines.
pub fn snapshot_fixture_tagged(
    files: u32,
    turns: u32,
    rows_per_file: u32,
    tag: &str,
) -> (ReviewSnapshot, Vec<(RowsKey, FileRows)>) {
    let turns = turns.max(1);
    let key = session_key();
    let mut entries = Vec::new();
    let mut rows_out = Vec::new();
    let mut by_turn: Vec<Vec<NarrativeItem>> = (0..turns).map(|_| Vec::new()).collect();
    for i in 0..files {
        let turn = i % turns;
        let at = TurnRef {
            session: 0,
            ordinal: turn + 1,
        };
        let added = rows_per_file / 4 * 2 + rows_per_file % 4 / 2;
        let removed = rows_per_file / 4;
        entries.push(FileEntry {
            key: file_key_tagged(tag, i),
            change: FileChange {
                status: ChangeStatus::Modified,
                path: path(tag, i),
                old: None,
                new: None,
                detail: ChangeDetail::Text {
                    hunks: Arc::from(vec![DiffHunk {
                        old: 0..rows_per_file,
                        new: 0..rows_per_file,
                        old_byte: 0,
                        new_byte: 0,
                    }]),
                    added,
                    removed,
                },
                generated: false,
                untracked: false,
                staged: None,
            },
            noise: None,
            group: GroupKind::Code,
            package: None,
            attribution: FileAttribution {
                state: AttributionState::Lines,
                turns: Arc::from(vec![at]),
                commands: Arc::from(Vec::new()),
                other_sessions: Arc::from(Vec::new()),
                drifted_hunks: 0,
            },
        });
        let step = StepId(i);
        by_turn[turn as usize].push(NarrativeItem::Step(StepEntry {
            step,
            file: file_key_tagged(tag, i),
            calls: Arc::from(vec![ToolCallId(Arc::from(format!("call-{i}")))]),
            added,
            removed,
            alignment: Alignment::Aligned,
            survival: Survival::All,
        }));
        rows_out.push((
            RowsKey::Step { step, context: 3 },
            fixture_rows(RowsKey::Step { step, context: 3 }, at, rows_per_file, tag),
        ));
        rows_out.push((
            RowsKey::Final {
                file: file_key_tagged(tag, i),
                context: 3,
            },
            fixture_rows(
                RowsKey::Final {
                    file: file_key_tagged(tag, i),
                    context: 3,
                },
                at,
                rows_per_file,
                tag,
            ),
        ));
    }
    let turn_entries: Vec<TurnEntry> = by_turn
        .into_iter()
        .enumerate()
        .map(|(t, mut items)| {
            let t = t as u32;
            items.insert(0, NarrativeItem::AgentText(agent_key(t)));
            items.push(NarrativeItem::Tool {
                call: ToolCallId(Arc::from(format!("tool-{t}"))),
                name: Arc::from(format!("{tag}Read")),
                status: Some(lectern_services::session::ToolStatus::Completed),
            });
            TurnEntry {
                at: TurnRef {
                    session: 0,
                    ordinal: t + 1,
                },
                id: TurnId(Arc::from(format!("turn-{t}"))),
                started_at: Timestamp(1_700_000_000_000 + i64::from(t) * 60_000),
                ended_at: Some(Timestamp(1_700_000_030_000 + i64::from(t) * 60_000)),
                prompt: Some(prompt_key(t)),
                prompt_preview: Arc::from(format!("{tag}prompt of turn {t}")),
                items: Arc::from(items),
                after_last_seen: true,
            }
        })
        .collect();
    let all: Vec<u32> = (0..files).collect();
    let brief_items: Vec<BriefItem> = entries
        .iter()
        .take(2)
        .enumerate()
        .map(|(n, file)| BriefItem {
            rule: if n == 0 {
                BriefRule::Sensitive(SensitiveKind::Ci)
            } else {
                BriefRule::EditFailed
            },
            critical: n == 0,
            file: Some(file.key.clone()),
            line: None,
            count: 1,
        })
        .collect();
    let snapshot = ReviewSnapshot {
        id: FIRST_ID,
        target: ReviewTarget::Session(key.clone()),
        generation: 2,
        state: LoadState::Ready,
        base: Some(BaseInfo {
            commit: None,
            how: BaseHow::Reflog,
        }),
        sessions: Arc::from(vec![SessionRef {
            key,
            agent: Agent::ClaudeCode,
            spawned_by: None,
        }]),
        files: Arc::from(entries),
        unclaimed: 0,
        seen: Arc::from(vec![SeenState::Unseen; files as usize]),
        progress: Progress {
            total: files,
            seen: 0,
            changed_since_seen: 0,
        },
        turns: Some(Arc::from(turn_entries)),
        groups: Arc::from(vec![Group {
            kind: GroupKind::Code,
            files: Arc::from(all),
        }]),
        brief: Arc::new(Brief {
            items: Arc::from(brief_items),
            omitted: 0,
            pending_providers: 0,
            analysis: AnalysisState::Unavailable(Arc::from("not yet")),
        }),
        focus_review: Arc::from(Vec::new()),
        focus_touched: Arc::from(Vec::new()),
        totals: ChangeTotals {
            files,
            ..ChangeTotals::default()
        },
        degraded: None,
        notes: Arc::from(Vec::new()),
    };
    (snapshot, rows_out)
}

/// A base for `ReviewTarget::Range` in tests and fixtures.
pub fn head_range() -> ReviewTarget {
    ReviewTarget::Range {
        project: ProjectKey::Folder {
            root: Arc::from(std::path::Path::new("/fake")),
        },
        worktree: WorktreeKey(Arc::from(std::path::Path::new("/fake"))),
        base: BaseSpec::Head,
    }
}
