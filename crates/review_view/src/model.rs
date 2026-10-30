//! The state a review screen and the Progress page share
//! (docs/design/review_view.md「视图」「数据结构」「数据流与线程」). Main
//! thread only; no IO: it reads `Review` snapshots (copying an `Arc`) and
//! sends commands.

use std::collections::{HashMap, HashSet, VecDeque};
use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use futures::StreamExt;
use gpui::{
    App, Context, Entity, FocusHandle, Global, ListAlignment, ListOffset, ListState, Pixels,
    SharedString, Task, WeakEntity, px,
};
use lectern_logging::{debug, info, warn};
use lectern_services::changes::ChangeTotals;
use lectern_services::code_intel::TextRef;
use lectern_services::project::{BlobId, RelPath};
use lectern_services::reader::ObjectShare;
use lectern_services::review::{
    AnalysisState, BatchId, Brief, FileKey, FileRows, LoadState, Need, NoteAnchor, NoteId,
    Progress, ReviewCommand, ReviewId, ReviewSnapshot, ReviewTarget, RowsKey, SeenState, TextKey,
    TurnRef,
};
use lectern_services::usage::{UsageInterest, UsageQuery, UsageSnapshot};
use lectern_services::workspace::WorkspaceCommand;
use lectern_ui::motion::{self, Transition};

use crate::anchor;
use crate::diag;
use crate::highlight::{Half, SideSpans, SpansCache};
use crate::items::{self, BuildOptions, DiffMode, DisplayRow, Item, ItemKey, NoteIndex};
use crate::needs;
use crate::notes::NoteEditor;
use crate::plugin::{Job, JobReceiver, JobSender, Shared};

/// The items assumed visible before the list has been laid out once.
const INITIAL_VISIBLE: usize = 40;

/// How far past the viewport the list lays items out (in pixels), so that
/// a scroll does not show items popping in.
const OVERDRAW: Pixels = px(400.0);

/// The brief's place in the list, after the title block: the one item whose
/// height changes without the structure doing so.
pub(crate) const BRIEF_ITEM: usize = 1;

/// The state of the screen on top, for the toolbar and the Progress page:
/// both read it from here (docs/design/review_view.md「视图」).
pub(crate) struct Current(pub WeakEntity<ReviewModel>);

impl Global for Current {}

/// Which way J / K walks.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Direction {
    Forward,
    Backward,
}

/// A row to scroll to and select as soon as its rows are prepared (a brief
/// entry with a line).
#[derive(Clone, Debug)]
struct PendingRow {
    file: FileKey,
    side_new: bool,
    line: u32,
}

/// A scroll in progress after V (`SCROLL_NEXT`): the list scrolls by
/// `distance` over the transition, then lands exactly on the item.
struct Glide {
    transition: Transition,
    last: f32,
    distance: Pixels,
    target: ItemKey,
}

/// A highlight task in the air: who wants its spans, and the flag that
/// cancels it once nobody does.
pub(crate) struct Inflight {
    pub(crate) cancel: Arc<AtomicBool>,
    pub(crate) owners: Vec<(RowsKey, Half)>,
}

/// What was handed to `code_intel` and `usage` already, so that only what
/// changed is sent again.
#[derive(Default)]
pub(crate) struct HandedOff {
    pub(crate) review: Option<Arc<[RelPath]>>,
    pub(crate) touched: Option<Arc<[RelPath]>>,
    pub(crate) current: Option<TextRef>,
    /// Whether `Current(Some(..))` has gone out at all: `Current(None)` is
    /// sent when the screen closes only if it did.
    pub(crate) current_sent: bool,
    /// The `what`s of `handoff unavailable` already logged for this screen.
    pub(crate) missing_logged: u8,
}

/// The `what` of `handoff unavailable`.
#[derive(Clone, Copy)]
pub(crate) enum Missing {
    OpenFile = 1,
    Focus = 2,
    Share = 4,
    Usage = 8,
}

impl Missing {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Missing::OpenFile => "open_file",
            Missing::Focus => "focus",
            Missing::Share => "share",
            Missing::Usage => "usage",
        }
    }
}

pub struct ReviewModel {
    pub(crate) shared: Arc<Shared>,
    /// The sender of this screen's channel: the highlight tasks deliver
    /// through it.
    pub(crate) tx: Option<JobSender>,
    pub(crate) mode: DiffMode,
    /// The display rows of every diff held in `rows`, in `mode`.
    pub(crate) displays: HashMap<RowsKey, Arc<[DisplayRow]>>,
    pub(crate) spans: SpansCache,
    /// The spans that arrived for each diff held in `rows`.
    pub(crate) highlighted: HashMap<RowsKey, SideSpans>,
    pub(crate) inflight: HashMap<(BlobId, Range<u64>), Inflight>,
    /// Windows that gave no spans this time: plain text, not tried again.
    pub(crate) plain: HashSet<(BlobId, Range<u64>)>,
    /// `Reader::share_object` registrations, with the rows they belong to.
    pub(crate) shares: HashMap<RowsKey, [Option<ObjectShare>; 2]>,
    pub(crate) usage_query: Option<UsageQuery>,
    pub(crate) usage_interest: Option<UsageInterest>,
    pub(crate) usage: Option<Arc<UsageSnapshot>>,
    pub(crate) handed_off: HandedOff,
    /// The shared horizontal offset of each diff's lines (side by side).
    pub(crate) hscroll: HashMap<RowsKey, Pixels>,
    /// The widest line drawn so far in each diff (cells): where a sideways
    /// scroll ends.
    pub(crate) widest: HashMap<RowsKey, u32>,
    /// Whether `Review::open` was called: the service was there.
    pub(crate) opened: bool,
    /// The registration of this screen's event channel.
    token: u64,
    pub(crate) id: ReviewId,
    pub(crate) target: ReviewTarget,
    /// When `Push` reached the screen; the loading phases count from here.
    pub(crate) opened_at: Instant,
    pub(crate) title: Option<Arc<str>>,
    pub(crate) snapshot: Arc<ReviewSnapshot>,
    applied: u64,
    pub(crate) items: Vec<Item>,
    pub(crate) list: ListState,
    pub(crate) rows: HashMap<RowsKey, Arc<FileRows>>,
    pub(crate) known_lines: HashMap<RowsKey, u32>,
    pub(crate) texts: HashMap<TextKey, Arc<str>>,
    pub(crate) context: HashMap<FileKey, u32>,
    earlier_collapsed: bool,
    /// The turns the user folded into one line each.
    pub(crate) collapsed_turns: HashSet<TurnRef>,
    pub(crate) noise_expanded: bool,
    pub(crate) selection: Option<ItemKey>,
    needs: Arc<[Need]>,
    pub(crate) visible: Range<usize>,
    pub(crate) line_height: Pixels,
    prepare_queued: bool,
    pending_row: Option<PendingRow>,
    pending_hunk: Option<Direction>,
    glide: Option<Glide>,
    after_mark: Option<Task<()>>,
    pub(crate) marked: u32,
    pub(crate) looks_good: bool,
    pub(crate) layer_open: bool,
    /// #22: the open note editor, at most one.
    pub(crate) editor: Option<NoteEditor>,
    /// #22: what Esc closed of new notes' texts, by anchor, at most
    /// `MAX_DRAFTS`; dropped with the screen.
    pub(crate) drafts: VecDeque<(NoteAnchor, SharedString)>,
    /// #22: the last batch the screen went to the Conversation page for.
    pub(crate) switched_for: Option<BatchId>,
    /// #22: C was pressed while the unsent notes are at their limit; the bar
    /// says so until the next command about notes.
    pub(crate) notes_full_hint: bool,
    /// #22: `AddNote` commands sent since the screen opened (the log).
    pub(crate) notes_added: u32,
    /// #22: the notes of the snapshot arranged for the item builder.
    pub(crate) note_index: NoteIndex,
    /// #22: where each note is in `snapshot.notes`.
    pub(crate) note_pos: HashMap<NoteId, u32>,
    /// #22: the screen's focus, which the editor returns it to.
    pub(crate) screen_focus: Option<FocusHandle>,
    /// Turns that came with a later snapshot than the first: they fade in.
    pub(crate) arriving: HashSet<TurnRef>,
    known_turns: HashSet<TurnRef>,
    built: bool,
    jobs: Option<Task<()>>,
    timers: Option<Task<()>>,
}

impl ReviewModel {
    /// Called by the screen factory (main thread, < 1 ms): opens the review
    /// (`Review::open` returns at once), registers the event channel before
    /// reading the first snapshot, and starts the foreground task that
    /// applies what the channel brings.
    pub(crate) fn new(
        shared: Arc<Shared>,
        target: ReviewTarget,
        cx: &mut Context<ReviewModel>,
    ) -> ReviewModel {
        let started = Instant::now();
        let id = shared.with_review(|review| review.open(target.clone()));
        let registration = id.map(|id| shared.register_screen(id));
        let (token, tx, receiver) = match registration {
            Some((token, tx, receiver)) => (token, Some(tx), Some(receiver)),
            None => (0, None, None),
        };
        let first = id.and_then(|id| shared.with_review(|review| review.snapshot(id)).flatten());
        let state = if id.is_some() {
            LoadState::Loading
        } else {
            LoadState::Failed(lectern_services::review::ReviewError::Missing)
        };
        let id = id.unwrap_or(ReviewId(0));
        let snapshot = first.unwrap_or_else(|| Arc::new(blank(id, target.clone(), state)));
        let title = match &target {
            ReviewTarget::Session(key) => shared.session_title(key),
            _ => None,
        };
        info!("review screen opened"; id = id.0, target = diag::target_kind(&target));
        if shared.mode() == DiffMode::SideBySide {
            // The screen needs the whole width from its first frame; the
            // command queues behind the `Push` that built it.
            shared.dispatch(WorkspaceCommand::SetReadingColumn(false));
        }
        // The mode the user chose on an earlier screen of this run.
        let mode = shared.mode();
        let mut model = ReviewModel {
            shared,
            tx,
            mode,
            displays: HashMap::new(),
            spans: SpansCache::new(),
            highlighted: HashMap::new(),
            inflight: HashMap::new(),
            plain: HashSet::new(),
            shares: HashMap::new(),
            usage_query: None,
            usage_interest: None,
            usage: None,
            handed_off: HandedOff::default(),
            hscroll: HashMap::new(),
            widest: HashMap::new(),
            opened: receiver.is_some(),
            token,
            id,
            target,
            opened_at: started,
            title,
            applied: snapshot.generation,
            snapshot: snapshot.clone(),
            items: Vec::new(),
            list: ListState::new(0, ListAlignment::Top, OVERDRAW),
            rows: HashMap::new(),
            known_lines: HashMap::new(),
            texts: HashMap::new(),
            context: HashMap::new(),
            earlier_collapsed: true,
            collapsed_turns: HashSet::new(),
            noise_expanded: false,
            selection: None,
            needs: Arc::from(Vec::new()),
            visible: 0..0,
            line_height: px(16.0),
            prepare_queued: false,
            pending_row: None,
            pending_hunk: None,
            glide: None,
            after_mark: None,
            marked: 0,
            looks_good: false,
            layer_open: false,
            editor: None,
            drafts: VecDeque::new(),
            switched_for: None,
            notes_full_hint: false,
            notes_added: 0,
            note_index: NoteIndex::default(),
            note_pos: HashMap::new(),
            screen_focus: None,
            arriving: HashSet::new(),
            known_turns: HashSet::new(),
            built: false,
            jobs: None,
            timers: None,
        };
        if let Some(receiver) = receiver {
            model.jobs = Some(Self::listen(receiver, cx));
        }
        if matches!(snapshot.state, LoadState::Loading) {
            model.timers = Some(Self::loading_timers(cx));
        }
        model.attach_usage();
        if matches!(snapshot.state, LoadState::Ready) {
            model.rebuild(&[], ListOffset::default(), cx);
            model.sync_focus();
        } else if let LoadState::Failed(error) = &snapshot.state {
            warn!("review failed shown"; id = id.0, error = diag::error_kind(error));
        }
        model
    }

    /// A screen for a target that is not a review: it shows that there is
    /// none. Opens nothing, so it closes nothing.
    pub(crate) fn missing(shared: Arc<Shared>) -> ReviewModel {
        let target = ReviewTarget::Outside {
            project: lectern_services::project::ProjectKey::Folder {
                root: Arc::from(std::path::Path::new("")),
            },
            worktree: lectern_services::project::WorktreeKey(Arc::from(std::path::Path::new(""))),
        };
        let snapshot = Arc::new(blank(
            ReviewId(0),
            target.clone(),
            LoadState::Failed(lectern_services::review::ReviewError::Missing),
        ));
        ReviewModel {
            shared,
            tx: None,
            mode: DiffMode::Unified,
            displays: HashMap::new(),
            spans: SpansCache::new(),
            highlighted: HashMap::new(),
            inflight: HashMap::new(),
            plain: HashSet::new(),
            shares: HashMap::new(),
            usage_query: None,
            usage_interest: None,
            usage: None,
            handed_off: HandedOff::default(),
            hscroll: HashMap::new(),
            widest: HashMap::new(),
            opened: false,
            token: 0,
            id: ReviewId(0),
            target,
            opened_at: Instant::now(),
            title: None,
            applied: 0,
            snapshot,
            items: Vec::new(),
            list: ListState::new(0, ListAlignment::Top, OVERDRAW),
            rows: HashMap::new(),
            known_lines: HashMap::new(),
            texts: HashMap::new(),
            context: HashMap::new(),
            earlier_collapsed: true,
            collapsed_turns: HashSet::new(),
            noise_expanded: false,
            selection: None,
            needs: Arc::from(Vec::new()),
            visible: 0..0,
            line_height: px(16.0),
            prepare_queued: false,
            pending_row: None,
            pending_hunk: None,
            glide: None,
            after_mark: None,
            marked: 0,
            looks_good: false,
            layer_open: false,
            editor: None,
            drafts: VecDeque::new(),
            switched_for: None,
            notes_full_hint: false,
            notes_added: 0,
            note_index: NoteIndex::default(),
            note_pos: HashMap::new(),
            screen_focus: None,
            arriving: HashSet::new(),
            known_turns: HashSet::new(),
            built: false,
            jobs: None,
            timers: None,
        }
    }

    /// The foreground task: waits for the listeners' jobs, takes everything
    /// that has queued up, and applies it in one update.
    fn listen(mut receiver: JobReceiver, cx: &mut Context<ReviewModel>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            while let Some(first) = receiver.next().await {
                let mut jobs = vec![first];
                while let Ok(more) = receiver.try_recv() {
                    jobs.push(more);
                }
                if this
                    .update(cx, |model, cx| model.apply_jobs(jobs, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
    }

    /// Redraws when the loading phases change (`LOADING_SKELETON_AT`, then
    /// `LOADING_PROGRESS_AT`).
    fn loading_timers(cx: &mut Context<ReviewModel>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            let executor = cx.background_executor().clone();
            executor.timer(motion::LOADING_SKELETON_AT).await;
            if this.update(cx, |_, cx| cx.notify()).is_err() {
                return;
            }
            executor
                .timer(motion::LOADING_PROGRESS_AT - motion::LOADING_SKELETON_AT)
                .await;
            let _ = this.update(cx, |_, cx| cx.notify());
        })
    }

    // ----- commands -----------------------------------------------------

    /// Sends a command; `Review::command` returns at once.
    pub(crate) fn command(&mut self, command: ReviewCommand) {
        if !self.opened {
            return;
        }
        debug!("command"; id = self.id.0, kind = diag::command_kind(&command));
        if matches!(
            command,
            ReviewCommand::AddNote { .. }
                | ReviewCommand::EditNote { .. }
                | ReviewCommand::RemoveNote(_)
                | ReviewCommand::DiscardNotes
                | ReviewCommand::SendNotes { .. }
                | ReviewCommand::CopyBatch(_)
        ) {
            self.notes_full_hint = false;
        }
        self.shared
            .with_review(|review| review.command(self.id, command));
    }

    /// Reads the snapshot `Review` holds now and applies it if it is newer
    /// than the one shown: after a command that changed the seen state, the
    /// review swapped its snapshot before returning.
    pub(crate) fn adopt_latest(&mut self, cx: &mut Context<ReviewModel>) {
        if !self.opened {
            return;
        }
        let Some(latest) = self
            .shared
            .with_review(|review| review.snapshot(self.id))
            .flatten()
        else {
            return;
        };
        if latest.generation > self.applied {
            self.apply_snapshot(latest, cx);
        }
    }

    // ----- jobs ---------------------------------------------------------

    /// Applies what the listeners sent: `Changed` events keep only the
    /// highest generation, `Prepared` counts once.
    fn apply_jobs(&mut self, jobs: Vec<Job>, cx: &mut Context<ReviewModel>) {
        let mut changed: Option<u64> = None;
        let mut prepared = false;
        let mut layer = None;
        let mut usage = false;
        let mut handoff = false;
        for job in jobs {
            match job {
                Job::Review(lectern_services::review::ReviewEvent::Changed {
                    generation, ..
                }) => changed = Some(changed.map_or(generation, |g| g.max(generation))),
                Job::Review(lectern_services::review::ReviewEvent::Prepared { .. }) => {
                    prepared = true;
                }
                Job::Review(_) => {}
                Job::Layer { open } => layer = Some(open),
                Job::Usage { query } => usage |= self.usage_query.as_ref() == Some(&query),
                Job::Handoff => handoff = true,
                Job::Spans {
                    key,
                    spans,
                    took_us,
                } => self.on_spans(key, spans, took_us, cx),
                Job::SpansUnavailable {
                    key,
                    unavailable,
                    language,
                    bytes,
                    took_us,
                    task,
                } => self.on_spans_unavailable(key, unavailable, &language, bytes, took_us, &task),
            }
        }
        if handoff {
            self.handoff_changed(cx);
        }
        if usage {
            self.read_usage(cx);
        }
        if let Some(open) = layer {
            self.layer_open = open;
        }
        if let Some(generation) = changed {
            if generation > self.applied {
                self.adopt_latest(cx);
            } else {
                debug!("stale generation dropped"; id = self.id.0, generation = generation);
            }
        }
        if prepared {
            self.on_prepared(cx);
        }
    }

    // ----- snapshots ----------------------------------------------------

    /// Shows a new snapshot. When nothing that decides the items changed
    /// (a mark on the seen state, say) the items stay; otherwise they are
    /// rebuilt and the scroll position and the selection are found again.
    pub(crate) fn apply_snapshot(
        &mut self,
        new: Arc<ReviewSnapshot>,
        cx: &mut Context<ReviewModel>,
    ) {
        let started = Instant::now();
        let old = std::mem::replace(&mut self.snapshot, new.clone());
        self.index_notes();
        self.applied = new.generation;
        if let ReviewTarget::Session(key) = &self.target {
            self.title = self.shared.session_title(key).or(self.title.take());
        }
        match &new.state {
            LoadState::Loading => {
                cx.notify();
                return;
            }
            LoadState::Failed(error) => {
                if old.state != new.state {
                    warn!("review failed shown"; id = self.id.0, error = diag::error_kind(error));
                }
                self.items.clear();
                self.list.reset(0);
                self.selection = None;
                cx.notify();
                return;
            }
            LoadState::Ready => self.notes_snapshot_applied(),
        }
        if old.state == LoadState::Ready && same_structure(&old, &new) {
            if !Arc::ptr_eq(&old.brief, &new.brief) {
                self.list.remeasure_items(BRIEF_ITEM..BRIEF_ITEM + 1);
            }
            debug!(
                "snapshot applied";
                id = self.id.0,
                generation = new.generation,
                state = "ready",
                items = diag::count(self.items.len()),
                files = diag::count(new.files.len()),
                turns = diag::count(new.turns.as_deref().map_or(0, <[_]>::len)),
                anchor_found = true,
                took_us = started.elapsed()
            );
            self.sync_focus();
            cx.notify();
            return;
        }
        let top = if old.state == LoadState::Ready {
            self.list.logical_scroll_top()
        } else {
            ListOffset::default()
        };
        let old_items = std::mem::take(&mut self.items);
        self.check_consistent();
        let found = self.rebuild(&old_items, top, cx);
        self.sync_focus();
        debug!(
            "snapshot applied";
            id = self.id.0,
            generation = new.generation,
            state = "ready",
            items = diag::count(self.items.len()),
            files = diag::count(new.files.len()),
            turns = diag::count(new.turns.as_deref().map_or(0, <[_]>::len)),
            anchor_found = found,
            took_us = started.elapsed()
        );
    }

    /// Builds the items for `self.snapshot` and puts the list where the old
    /// items were: returns whether the anchor's own key was found.
    fn rebuild(&mut self, old: &[Item], top: ListOffset, cx: &mut Context<ReviewModel>) -> bool {
        let options = BuildOptions {
            mode: self.mode,
            earlier_collapsed: self.earlier_collapsed,
            noise_expanded: self.noise_expanded,
            context: &self.context,
            known_lines: &self.known_lines,
            // `build_noted` takes the editor from the note index.
            editor: None,
        };
        let note_index = NoteIndex::new(&self.snapshot, self.editor_at());
        let new_items = items::build_noted(
            &self.snapshot,
            &self.rows,
            &options,
            &self.collapsed_turns,
            &note_index,
        );
        self.note_index = note_index;
        self.index_notes();
        if self.editor.is_some()
            && !new_items
                .iter()
                .any(|item| matches!(item, Item::NoteEditor { .. }))
        {
            // The place of the editor is not in the review any more.
            self.editor = None;
            debug!("note editor"; id = self.id.0, target = "new", how = "closed_by_change");
        }
        let target = if old.is_empty() {
            ListOffset::default()
        } else {
            anchor::restore(old, top, &new_items)
        };
        let found = old
            .get(top.item_ix)
            .is_some_and(|item| anchor::find(&new_items, &item.key()) == Some(target.item_ix));
        self.items = new_items;
        if let Some(turns) = self.snapshot.turns.as_deref() {
            for turn in turns {
                if self.known_turns.insert(turn.at) && self.built {
                    self.arriving.insert(turn.at);
                }
            }
        }
        self.built = true;
        self.list.reset(self.items.len());
        if target.item_ix > 0 || target.offset_in_item > Pixels::ZERO {
            self.list.scroll_to(target);
        }
        // The selection follows its item, which may have changed shape (a
        // removed row beside its partner after the mode changed).
        if let Some(selected) = &self.selection {
            let before = self.selection.clone();
            self.selection = anchor::find(&self.items, selected).map(|ix| self.items[ix].key());
            if self.selection != before {
                self.sync_current();
            }
        }
        self.visible = target.item_ix
            ..target
                .item_ix
                .saturating_add(INITIAL_VISIBLE)
                .min(self.items.len());
        self.schedule_needs(cx);
        cx.notify();
        found
    }

    /// Warns about a snapshot that disagrees with itself (it should not
    /// happen): the entries are still shown.
    fn check_consistent(&self) {
        let sessions = self.snapshot.sessions.len();
        let Some(turns) = self.snapshot.turns.as_deref() else {
            return;
        };
        let bad = turns
            .iter()
            .filter(|turn| usize::from(turn.at.session) >= sessions.max(1))
            .count();
        if bad > 0 {
            warn!(
                "snapshot inconsistent";
                id = self.id.0,
                generation = self.snapshot.generation,
                kind = "turn_ref"
            );
        }
    }

    // ----- prepare ------------------------------------------------------

    /// Notes that the visible range may have changed: the layout the list
    /// last did. The `Prepare` itself is sent from the end of the update.
    pub(crate) fn sync_layout(&mut self, cx: &mut Context<ReviewModel>) {
        let Some(measured) = self.measure_visible() else {
            return;
        };
        if measured != self.visible {
            self.visible = measured;
            self.schedule_needs(cx);
        }
    }

    /// The items the list laid out inside its viewport, from what the list
    /// knows (its scroll handler fires only for the user's own scrolling).
    pub(crate) fn measure_visible(&self) -> Option<Range<usize>> {
        let viewport = self.list.viewport_bounds();
        if viewport.size.height <= Pixels::ZERO || self.items.is_empty() {
            return None;
        }
        let start = self.list.logical_scroll_top().item_ix.min(self.items.len());
        let mut end = start;
        let mut covered = Pixels::ZERO;
        while end < self.items.len() {
            match self.list.bounds_for_item(end) {
                Some(bounds) if bounds.top() < viewport.bottom() => {
                    covered = bounds.bottom();
                    end += 1;
                }
                Some(_) => break,
                // The viewport is full: what follows is not in view.
                None if covered >= viewport.bottom() => break,
                // Not laid out yet (items just spliced in): the range is
                // not known until the next frame.
                None => return None,
            }
        }
        Some(start..end.max(start + 1).min(self.items.len()))
    }

    /// Sends `Prepare` once the current update ends, at most once per update
    /// (docs/design/review_view.md「可见范围与 Prepare」).
    pub(crate) fn schedule_needs(&mut self, cx: &mut Context<ReviewModel>) {
        if self.prepare_queued {
            return;
        }
        self.prepare_queued = true;
        cx.spawn(async move |this, cx| {
            let _ = this.update(cx, |model, cx| model.flush_needs(cx));
        })
        .detach();
    }

    fn flush_needs(&mut self, cx: &mut Context<ReviewModel>) {
        self.prepare_queued = false;
        if self.items.is_empty() || !self.opened {
            return;
        }
        let wanted: Arc<[Need]> =
            needs::compute(&self.items, &self.snapshot, &self.visible, needs::CAP).into();
        if wanted == self.needs {
            return;
        }
        self.needs = wanted.clone();
        self.release_unwanted(&wanted);
        debug!(
            "prepare sent";
            id = self.id.0,
            rows = diag::count(wanted.iter().filter(|n| matches!(n, Need::Rows(_))).count()),
            texts = diag::count(wanted.iter().filter(|n| matches!(n, Need::Text(_))).count()),
            visible = diag::count(self.visible.len())
        );
        self.command(ReviewCommand::Prepare(wanted));
        cx.notify();
    }

    /// Rows that are not in the request any more go back to a placeholder as
    /// tall as they were (`review` drops them as well).
    fn release_unwanted(&mut self, wanted: &[Need]) {
        let keep: HashSet<&RowsKey> = wanted
            .iter()
            .filter_map(|need| match need {
                Need::Rows(key) => Some(key),
                Need::Text(_) => None,
            })
            .collect();
        let dropped: Vec<RowsKey> = self
            .rows
            .keys()
            .filter(|key| !keep.contains(key))
            .cloned()
            .collect();
        for key in dropped {
            self.rows.remove(&key);
            self.drop_rows(&key);
            let lines = self.known_lines.get(&key).copied().unwrap_or(1);
            let mut fresh = Vec::new();
            match self.note_file(&key) {
                Some(file) => {
                    let notes = self.note_index.context(&key, &file);
                    items::placeholder_items(&mut fresh, &key, lines, &notes);
                }
                None => fresh.push(Item::Placeholder {
                    rows: key.clone(),
                    lines,
                }),
            }
            self.replace_segment(&key, fresh);
        }
    }

    /// `Prepared`: take what was asked for from `Review`; a row or a text
    /// that is not there yet stays what it was.
    fn on_prepared(&mut self, cx: &mut Context<ReviewModel>) {
        let needs = self.needs.clone();
        let mut missing_rows = 0usize;
        let mut missing_texts = 0usize;
        let mut spliced = 0usize;
        let mut texts_changed: HashSet<TextKey> = HashSet::new();
        for need in needs.iter() {
            match need {
                Need::Rows(key) => {
                    let rows = self
                        .shared
                        .with_review(|review| review.rows(self.id, key))
                        .flatten();
                    match rows {
                        Some(rows) => {
                            if self.store_rows(key, rows) {
                                spliced += 1;
                            }
                        }
                        None => missing_rows += 1,
                    }
                }
                Need::Text(key) => {
                    let text = self
                        .shared
                        .with_review(|review| review.text(self.id, key))
                        .flatten();
                    match text {
                        Some(text) => {
                            if self.texts.get(key).is_none_or(|old| **old != *text) {
                                texts_changed.insert(*key);
                            }
                            self.texts.insert(*key, text);
                        }
                        None => missing_texts += 1,
                    }
                }
            }
        }
        if missing_rows + missing_texts > 0 {
            warn!(
                "prepared keys missing";
                id = self.id.0,
                rows = diag::count(missing_rows),
                texts = diag::count(missing_texts)
            );
        }
        if !texts_changed.is_empty() {
            for (ix, item) in self.items.iter().enumerate() {
                let key = match item {
                    Item::Text { key, .. } | Item::Injected { key, .. } => Some(*key),
                    Item::Prompt { ix: turn, .. } => needs::prompt_key(&self.snapshot, *turn),
                    _ => None,
                };
                if key.is_some_and(|key| texts_changed.contains(&key)) {
                    self.list.remeasure_items(ix..ix + 1);
                }
            }
        }
        debug!(
            "prepared applied";
            id = self.id.0,
            rows = diag::count(needs.len() - missing_rows - missing_texts),
            texts = diag::count(texts_changed.len()),
            spliced = diag::count(spliced)
        );
        self.resolve_pending(cx);
        cx.notify();
    }

    /// Stores prepared rows and replaces the items of that diff. False when
    /// they equal the rows already shown (nothing to lay out again).
    pub(crate) fn store_rows(&mut self, key: &RowsKey, rows: Arc<FileRows>) -> bool {
        if self
            .rows
            .get(key)
            .is_some_and(|old| Arc::ptr_eq(old, &rows) || **old == *rows)
        {
            return false;
        }
        let display: Arc<[DisplayRow]> = items::display_rows(&rows, self.mode).into();
        let mut fresh = Vec::new();
        match self.note_file(key) {
            Some(file) => {
                let notes = self.note_index.context(key, &file);
                items::rows_items_with(&mut fresh, key, &rows, &display, &notes);
            }
            None => items::rows_items_of(&mut fresh, key, &rows, &display),
        }
        // The height a placeholder takes after the rows are dropped again:
        // the lines of the diff, not its notes.
        let lines = fresh
            .iter()
            .filter(|item| !matches!(item, Item::Note { .. } | Item::NoteEditor { .. }))
            .count();
        self.known_lines
            .insert(key.clone(), diag::count(lines).max(1));
        self.displays.insert(key.clone(), display);
        self.rows.insert(key.clone(), rows.clone());
        self.replace_segment(key, fresh);
        self.after_rows(key, &rows);
        true
    }

    /// Replaces the items of one diff (a placeholder, or its rows) with
    /// `fresh`, and keeps the list where it was: a position inside the old
    /// segment is found again by the item's key, or by the row inside a
    /// placeholder.
    fn replace_segment(&mut self, key: &RowsKey, fresh: Vec<Item>) {
        let Some(start) = self
            .items
            .iter()
            .position(|item| item.rows_key() == Some(key))
        else {
            return;
        };
        let end = start
            + self.items[start..]
                .iter()
                .take_while(|item| item.rows_key() == Some(key))
                .count();
        let top = self.list.logical_scroll_top();
        let inside = (start..end).contains(&top.item_ix);
        let anchor_key = inside.then(|| self.items[top.item_ix].key());
        let was_placeholder =
            end - start == 1 && matches!(self.items[start], Item::Placeholder { .. });
        let count = fresh.len();
        self.items.splice(start..end, fresh);
        self.list.splice(start..end, count);
        self.visible = shift_visible(&self.visible, start..end, count);
        if !inside || count == 0 {
            return;
        }
        let target = match anchor_key
            .and_then(|key| anchor::find(&self.items[start..start + count], &key))
        {
            Some(offset) => ListOffset {
                item_ix: start + offset,
                offset_in_item: top.offset_in_item,
            },
            None if was_placeholder && self.line_height > Pixels::ZERO => {
                let row = (top.offset_in_item / self.line_height).floor().max(0.0) as usize;
                let row = row.min(count - 1);
                ListOffset {
                    item_ix: start + row,
                    offset_in_item: top.offset_in_item - self.line_height * row as f32,
                }
            }
            None => ListOffset {
                item_ix: start,
                offset_in_item: Pixels::ZERO,
            },
        };
        self.list.scroll_to(target);
    }

    // ----- selection and jumps -------------------------------------------

    pub(crate) fn select(&mut self, key: Option<ItemKey>, cx: &mut Context<ReviewModel>) {
        self.pending_row = None;
        if self.selection != key {
            self.selection = key;
            self.sync_current();
            cx.notify();
        }
    }

    /// Selects the item at `ix` and scrolls it into view.
    fn select_at(&mut self, ix: usize, cx: &mut Context<ReviewModel>) {
        let Some(item) = self.items.get(ix) else {
            return;
        };
        let key = item.key();
        // A hunk may have no height of its own (nothing is drawn for it in
        // a session's narrative): its first row is what to bring in view.
        let reveal = if item.is_hunk() && self.items.get(ix + 1).is_some_and(Item::is_card_body) {
            ix + 1
        } else {
            ix
        };
        self.selection = Some(key);
        self.sync_current();
        self.list.scroll_to_reveal_item(reveal);
        cx.notify();
    }

    /// J / K: the next (previous) hunk after the selection, or after the
    /// top of the list when nothing is selected. A diff that is not prepared
    /// yet is scrolled to (so that it gets prepared) and the walk goes on
    /// when its rows arrive.
    pub(crate) fn walk_hunks(&mut self, direction: Direction, cx: &mut Context<ReviewModel>) {
        self.pending_hunk = None;
        let from = self
            .selection
            .as_ref()
            .and_then(|key| anchor::find(&self.items, key))
            .unwrap_or(self.list.logical_scroll_top().item_ix);
        let has_selection = self.selection.is_some();
        let stop = |item: &Item| item.is_hunk() || matches!(item, Item::Placeholder { .. });
        let found = match direction {
            Direction::Forward => {
                let begin = if has_selection { from + 1 } else { from };
                self.items
                    .iter()
                    .enumerate()
                    .skip(begin)
                    .find(|(_, item)| stop(item))
                    .map(|(ix, _)| ix)
            }
            Direction::Backward => self.items[..from.min(self.items.len())]
                .iter()
                .enumerate()
                .rev()
                .find(|(_, item)| stop(item))
                .map(|(ix, _)| ix),
        };
        let Some(ix) = found else { return };
        if self.items[ix].is_hunk() {
            self.select_at(ix, cx);
        } else {
            // A placeholder: bring its diff in, then look again.
            self.pending_hunk = Some(direction);
            self.list.scroll_to_reveal_item(ix);
            self.visible = ix..(ix + 1);
            self.schedule_needs(cx);
            cx.notify();
        }
    }

    /// After `Prepared`: finish a walk or a jump that waited for rows.
    fn resolve_pending(&mut self, cx: &mut Context<ReviewModel>) {
        if let Some(pending) = self.pending_row.clone() {
            let found = self.items.iter().position(|item| {
                matches!(
                    item,
                    Item::Row { rows: RowsKey::Final { file, .. }, old, new, .. }
                        if *file == pending.file
                            && if pending.side_new { *new == Some(pending.line) } else { *old == Some(pending.line) }
                )
            });
            if let Some(ix) = found {
                self.pending_row = None;
                self.select_at(ix, cx);
            }
        }
        if let Some(direction) = self.pending_hunk.take() {
            let still_waiting = self
                .items
                .iter()
                .any(|item| matches!(item, Item::Placeholder { .. }));
            if still_waiting || self.items.iter().any(Item::is_hunk) {
                self.walk_hunks(direction, cx);
            }
        }
    }

    /// A click on a brief entry: the row of the line in the file's final
    /// diff when that is shown, otherwise the file's step (or title).
    pub(crate) fn jump_to_brief(
        &mut self,
        file: Option<FileKey>,
        line: Option<lectern_services::review::LineAnchor>,
        cx: &mut Context<ReviewModel>,
    ) {
        let Some(file) = file else { return };
        if let Some(anchor) = line {
            let side_new = anchor.side == lectern_services::review::DiffSide::New;
            let pending = PendingRow {
                file: file.clone(),
                side_new,
                line: anchor.line,
            };
            // Final rows are shown in the rule-grouped view only; a
            // session's narrative shows the steps' rows, so there the entry
            // goes to the file's step (docs/design/review_view.md「与设计的差异」).
            self.pending_row = self.snapshot.turns.is_none().then_some(pending);
            self.resolve_pending(cx);
            if self.pending_row.is_none() && self.snapshot.turns.is_none() {
                return;
            }
        } else {
            self.pending_row = None;
        }
        // The first step that touches the file, or the file's title; a
        // folded noise file is unfolded to be shown.
        let mut target = self.file_item(&file);
        if target.is_none() && self.unfold_earlier(cx) {
            target = self.file_item(&file);
        }
        if target.is_none() && self.unfold_turn_of(&file, cx) {
            target = self.file_item(&file);
        }
        if target.is_none()
            && !self.noise_expanded
            && self
                .snapshot
                .files
                .iter()
                .any(|entry| entry.key == file && entry.noise.is_some())
        {
            self.noise_expanded = true;
            self.rebuild_in_place(cx);
            target = self.file_item(&file);
        }
        if let Some(ix) = target {
            self.select_at(ix, cx);
            self.visible = ix..(ix + 1);
            self.schedule_needs(cx);
        }
    }

    /// Unfolds the earlier turns when they are folded, so that a jump can reach
    /// what is inside them; false when there was nothing to unfold.
    fn unfold_earlier(&mut self, cx: &mut Context<ReviewModel>) -> bool {
        if !self.earlier_collapsed || items::earlier_count(&self.snapshot) == 0 {
            return false;
        }
        self.earlier_collapsed = false;
        self.rebuild_in_place(cx);
        true
    }

    /// The first item that is a file's title or a step on it.
    fn file_item(&self, file: &FileKey) -> Option<usize> {
        self.items.iter().position(|item| match item {
            Item::File { key, .. } => key == file,
            Item::Step { at, .. } => {
                items::step_entry(&self.snapshot, *at).is_some_and(|step| step.file == *file)
            }
            _ => false,
        })
    }

    /// A click on a sub-agent's line: its first turn (known limitation of
    /// the design: only that turn is reached).
    pub(crate) fn jump_to_session(&mut self, session: u16, cx: &mut Context<ReviewModel>) {
        let find = |items: &[Item]| {
            items
                .iter()
                .position(|item| matches!(item, Item::Turn { at, .. } if at.session == session))
        };
        let mut target = find(&self.items);
        if target.is_none() && self.unfold_earlier(cx) {
            target = find(&self.items);
        }
        if let Some(ix) = target {
            self.select_at(ix, cx);
            self.visible = ix..(ix + 1);
            self.schedule_needs(cx);
        }
    }

    // ----- seen -----------------------------------------------------------

    /// The file V acts on: the selected item's file, otherwise the first
    /// visible item that belongs to one.
    pub(crate) fn current_file(&self) -> Option<FileKey> {
        if let Some(ix) = self
            .selection
            .as_ref()
            .and_then(|key| anchor::find(&self.items, key))
            && let Some(file) = items::item_file(&self.items[ix], &self.snapshot)
        {
            return Some(file);
        }
        let range = if self.visible.is_empty() {
            0..self.items.len().min(INITIAL_VISIBLE)
        } else {
            self.visible.clone()
        };
        self.items
            .get(range)?
            .iter()
            .find_map(|item| items::item_file(item, &self.snapshot))
    }

    pub(crate) fn seen_of(&self, file: &FileKey) -> Option<SeenState> {
        let ix = self
            .snapshot
            .files
            .iter()
            .position(|entry| entry.key == *file)?;
        self.snapshot.seen.get(ix).copied()
    }

    /// Marks a file seen or not seen; the review has swapped its snapshot
    /// when `command` returns, so the screen shows the new state at once.
    pub(crate) fn set_seen(&mut self, file: FileKey, seen: bool, cx: &mut Context<ReviewModel>) {
        self.command(if seen {
            ReviewCommand::MarkSeen(file)
        } else {
            ReviewCommand::MarkUnseen(file)
        });
        if seen {
            self.marked += 1;
        }
        self.adopt_latest(cx);
    }

    /// The check of a file: seen becomes not seen and the other way round.
    pub(crate) fn toggle_seen(&mut self, file: FileKey, cx: &mut Context<ReviewModel>) {
        let seen = matches!(self.seen_of(&file), Some(SeenState::Seen));
        self.set_seen(file, !seen, cx);
    }

    /// V: marks the current file seen and, once the check has played
    /// (`CHECK_FILL` and `CHECK_STROKE`), goes to the next unseen file.
    pub(crate) fn mark_seen_and_next(&mut self, cx: &mut Context<ReviewModel>) {
        let Some(file) = self.current_file() else {
            return;
        };
        if !matches!(self.seen_of(&file), Some(SeenState::Seen)) {
            self.set_seen(file.clone(), true, cx);
        }
        // Where the search for the next file starts, as a key: the items
        // may be rebuilt before the check has played.
        let from = self.selection.clone().or_else(|| {
            self.items
                .get(self.visible.start.min(self.items.len().saturating_sub(1)))
                .map(Item::key)
        });
        let wait = if motion::reduced(cx) {
            (motion::CHECK_STROKE.delay + motion::CHECK_STROKE.duration) / 2
        } else {
            motion::CHECK_STROKE.delay + motion::CHECK_STROKE.duration
        };
        self.after_mark = Some(cx.spawn(async move |this, cx| {
            let executor = cx.background_executor().clone();
            executor.timer(wait).await;
            let _ = this.update(cx, |model, cx| {
                let from = from
                    .and_then(|key| anchor::find(&model.items, &key))
                    .unwrap_or(0);
                model.go_to_next_unseen(&file, from, cx)
            });
        }));
    }

    /// The first step (or file title) after `from` whose file is not seen.
    fn go_to_next_unseen(&mut self, current: &FileKey, from: usize, cx: &mut Context<ReviewModel>) {
        let next = self
            .items
            .iter()
            .enumerate()
            .skip(from.saturating_add(1))
            .find(|(_, item)| match item {
                Item::Step { .. } | Item::File { .. } => items::item_file(item, &self.snapshot)
                    .is_some_and(|file| {
                        file != *current
                            && !matches!(self.seen_of(&file), Some(SeenState::Seen))
                            && self.in_progress(&file)
                    }),
                _ => false,
            })
            .map(|(ix, _)| ix);
        let Some(ix) = next else { return };
        self.selection = Some(self.items[ix].key());
        self.glide_to(ix, cx);
        cx.notify();
    }

    /// Whether the file counts in the progress (it is one of `files`).
    fn in_progress(&self, file: &FileKey) -> bool {
        self.snapshot.files.iter().any(|entry| entry.key == *file)
    }

    /// Scrolls to an item over `SCROLL_NEXT` when its place is known
    /// (it was laid out), otherwise at once.
    fn glide_to(&mut self, ix: usize, cx: &mut Context<ReviewModel>) {
        let viewport = self.list.viewport_bounds();
        let bounds = self.list.bounds_for_item(ix);
        match bounds {
            Some(bounds) if viewport.size.height > Pixels::ZERO && !motion::reduced(cx) => {
                let mut transition = Transition::new(0.0);
                transition.go(1.0, motion::SCROLL_NEXT, Instant::now(), cx);
                self.glide = Some(Glide {
                    transition,
                    last: 0.0,
                    distance: bounds.top() - viewport.top(),
                    target: self.items[ix].key(),
                });
            }
            _ => {
                self.glide = None;
                self.list.scroll_to(ListOffset {
                    item_ix: ix,
                    offset_in_item: Pixels::ZERO,
                });
            }
        }
        self.visible = ix..(ix + 1).min(self.items.len());
        self.schedule_needs(cx);
    }

    /// Advances a scroll in progress; called from the screen's `render`,
    /// where the next frame may be requested.
    pub(crate) fn tick_glide(&mut self, window: &mut gpui::Window, now: Instant) {
        let Some(glide) = &mut self.glide else {
            return;
        };
        let value = glide.transition.tick(now, window);
        self.list.scroll_by(glide.distance * (value - glide.last));
        glide.last = value;
        if !glide.transition.is_running(now) {
            let target = glide.target.clone();
            self.glide = None;
            if let Some(ix) = anchor::find(&self.items, &target) {
                self.list.scroll_to(ListOffset {
                    item_ix: ix,
                    offset_in_item: Pixels::ZERO,
                });
            }
        }
    }

    pub(crate) fn looks_good(&mut self, cx: &mut Context<ReviewModel>) {
        self.looks_good = true;
        self.command(ReviewCommand::LooksGood);
        self.adopt_latest(cx);
    }

    pub(crate) fn refresh(&mut self) {
        self.command(ReviewCommand::Refresh);
    }

    /// Analyze, Analyze again and Retry: starts an analysis unless one runs
    /// or none can run. The view sends the command; the analysis, its
    /// sandbox and what it may reach are the domain's (ADR 0019).
    pub(crate) fn analyze(&mut self) {
        if matches!(
            self.snapshot.brief.analysis,
            AnalysisState::Idle | AnalysisState::Done { .. } | AnalysisState::Failed(_)
        ) {
            self.command(ReviewCommand::Analyze);
        }
    }

    /// Cancel while an analysis runs.
    pub(crate) fn cancel_analyze(&mut self) {
        if matches!(self.snapshot.brief.analysis, AnalysisState::Running { .. }) {
            self.command(ReviewCommand::CancelAnalyze);
        }
    }

    // ----- folds ----------------------------------------------------------

    pub(crate) fn toggle_noise(&mut self, cx: &mut Context<ReviewModel>) {
        self.noise_expanded = !self.noise_expanded;
        self.rebuild_in_place(cx);
    }

    pub(crate) fn toggle_earlier(&mut self, cx: &mut Context<ReviewModel>) {
        self.earlier_collapsed = !self.earlier_collapsed;
        self.rebuild_in_place(cx);
    }

    /// A click on a turn's title: the turn folds into one line, or opens.
    pub(crate) fn toggle_turn(&mut self, at: TurnRef, cx: &mut Context<ReviewModel>) {
        if !self.collapsed_turns.remove(&at) {
            self.collapsed_turns.insert(at);
        }
        self.rebuild_in_place(cx);
    }

    /// Opens the folded turn that has a step on `file`, so that a jump can
    /// reach it; false when no folded turn has one.
    fn unfold_turn_of(&mut self, file: &FileKey, cx: &mut Context<ReviewModel>) -> bool {
        let Some(turns) = self.snapshot.turns.clone() else {
            return false;
        };
        let found = turns.iter().find(|turn| {
            self.collapsed_turns.contains(&turn.at)
                && turn.items.iter().any(|item| {
                    matches!(item, lectern_services::review::NarrativeItem::Step(step) if step.file == *file)
                })
        });
        let Some(turn) = found else { return false };
        self.collapsed_turns.remove(&turn.at);
        self.rebuild_in_place(cx);
        true
    }

    /// Rebuilds the items of the same snapshot after a fold changed.
    pub(crate) fn rebuild_in_place(&mut self, cx: &mut Context<ReviewModel>) {
        if !matches!(self.snapshot.state, LoadState::Ready) {
            return;
        }
        let top = self.list.logical_scroll_top();
        let old = std::mem::take(&mut self.items);
        self.rebuild(&old, top, cx);
    }

    /// Esc (docs/design/review_view.md「Action 与快捷键」): false when the
    /// action is not handled here, so that it goes on to `workspace`:
    /// while a panel or overlay is open, and when nothing is selected.
    pub(crate) fn escape(&mut self, cx: &mut Context<ReviewModel>) -> bool {
        if self.layer_open || self.selection.is_none() {
            return false;
        }
        self.selection = None;
        cx.notify();
        true
    }

    // ----- helpers for the screen and the Progress page ---------------------

    /// The progress counts for the bar and the subtitle.
    pub(crate) fn progress(&self) -> Progress {
        self.snapshot.progress
    }

    pub(crate) fn is_ready(&self) -> bool {
        matches!(self.snapshot.state, LoadState::Ready)
    }

    /// Whether any row of a diff is inside the viewport: the milestone.
    pub(crate) fn shows_rows(&self) -> bool {
        self.items
            .get(self.visible.clone())
            .is_some_and(|items| items.iter().any(|item| matches!(item, Item::Row { .. })))
    }

    /// The row count of the first screen for the `diff shown` record.
    pub(crate) fn visible_rows(&self) -> usize {
        self.items.get(self.visible.clone()).map_or(0, |items| {
            items
                .iter()
                .filter(|item| matches!(item, Item::Row { .. }))
                .count()
        })
    }

    /// The first moment that counts for `diff shown`: logged once.
    pub(crate) fn log_diff_shown(&self) {
        info!(
            "diff shown";
            id = self.id.0,
            took_us = self.opened_at.elapsed(),
            files = diag::count(self.snapshot.files.len()),
            turns = diag::count(self.snapshot.turns.as_deref().map_or(0, <[_]>::len)),
            rows = diag::count(self.visible_rows()),
            generation = self.snapshot.generation
        );
    }

    /// Test hook: pretend the screen opened `age` ago.
    #[cfg(test)]
    pub(crate) fn backdate(&mut self, age: std::time::Duration) {
        self.opened_at = Instant::now() - age;
    }

    /// Pops the screen (the failed state's button).
    pub(crate) fn back(&self) {
        self.shared.dispatch(WorkspaceCommand::Pop);
    }
}

impl Drop for ReviewModel {
    fn drop(&mut self) {
        for (_, inflight) in self.inflight.drain() {
            inflight.cancel.store(true, Ordering::Relaxed);
        }
        self.clear_current();
        if !self.opened {
            return;
        }
        if matches!(self.snapshot.brief.analysis, AnalysisState::Running { .. }) {
            self.command(ReviewCommand::CancelAnalyze);
        }
        self.shared.with_review(|review| review.close(self.id));
        self.shared.unregister_screen(self.id, self.token);
        info!(
            "review screen closed";
            id = self.id.0,
            marked = self.marked,
            looks_good = self.looks_good,
            notes_added = self.notes_added,
            took_us = self.opened_at.elapsed()
        );
    }
}

/// The visible range after the items of `old` were replaced by `count`
/// items: a range that touched the segment takes the whole new segment (the
/// next frame measures what is really in view), a range after it moves by the
/// difference. The list has not laid the new items out yet, so a range kept as
/// it was would point at other items.
pub(crate) fn shift_visible(
    visible: &Range<usize>,
    old: Range<usize>,
    count: usize,
) -> Range<usize> {
    let new_end = old.start + count;
    if visible.start >= old.end {
        let start = visible.start + count - (old.end - old.start);
        let end = visible.end + count - (old.end - old.start);
        return start..end;
    }
    if visible.end <= old.start {
        return visible.clone();
    }
    let start = visible.start.min(old.start);
    let end = if visible.end >= old.end {
        visible.end + count - (old.end - old.start)
    } else {
        new_end
    };
    start..end
}

/// Whether two ready snapshots lead to the same items: everything the items
/// are built from, the brief aside (it has its own item, which is measured
/// again). The seen marks are not among them: they are read when drawing.
fn same_structure(a: &ReviewSnapshot, b: &ReviewSnapshot) -> bool {
    fn same<T>(a: &Option<Arc<[T]>>, b: &Option<Arc<[T]>>) -> bool {
        match (a, b) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        }
    }
    a.target == b.target
        && Arc::ptr_eq(&a.notes, &b.notes)
        && a.base == b.base
        && a.degraded == b.degraded
        && a.unclaimed == b.unclaimed
        && Arc::ptr_eq(&a.files, &b.files)
        && Arc::ptr_eq(&a.groups, &b.groups)
        && same(&a.turns, &b.turns)
        && Arc::ptr_eq(&a.sessions, &b.sessions)
        && a.totals.invalid_paths.len() == b.totals.invalid_paths.len()
}

/// A snapshot with nothing in it: what the screen shows while it has not
/// read one, or when the service is gone.
pub(crate) fn blank(id: ReviewId, target: ReviewTarget, state: LoadState) -> ReviewSnapshot {
    ReviewSnapshot {
        id,
        target,
        generation: 0,
        state,
        base: None,
        sessions: Arc::from(Vec::new()),
        files: Arc::from(Vec::new()),
        unclaimed: 0,
        seen: Arc::from(Vec::new()),
        progress: Progress::default(),
        turns: None,
        groups: Arc::from(Vec::new()),
        brief: Arc::new(Brief {
            items: Arc::from(Vec::new()),
            omitted: 0,
            pending_providers: 0,
            analysis: AnalysisState::Unavailable(Arc::from("")),
        }),
        focus_review: Arc::from(Vec::new()),
        focus_touched: Arc::from(Vec::new()),
        totals: ChangeTotals::default(),
        degraded: None,
        notes: Arc::from(Vec::new()),
        notes_state: Arc::default(),
    }
}

/// Makes `model` the one the toolbar and the Progress page read.
pub(crate) fn set_current(model: &Entity<ReviewModel>, cx: &mut App) {
    cx.set_global(Current(model.downgrade()));
}

/// The model on top, if there is one.
pub(crate) fn current(cx: &App) -> Option<Entity<ReviewModel>> {
    cx.try_global::<Current>()?.0.upgrade()
}
