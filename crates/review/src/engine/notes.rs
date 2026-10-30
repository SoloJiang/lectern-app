//! Review notes in the engine (docs/design/review.md「审阅意见」): the
//! commands `AddNote`, `EditNote`, `RemoveNote` and `DiscardNotes`, where a
//! note sits in the snapshot (`NotePlace`) and the `notes` and
//! `notes_state` a snapshot carries.
//!
//! A command changes the log's in-memory index, swaps the two `Arc`s in a
//! new snapshot and queues its records, all on the calling thread and in
//! memory only; the text of an excerpt is scanned by a `Background` task
//! (`take_excerpt`). A folder project has no log: its notes live in the
//! review (`Open::folder_notes`) and are gone with it.

use std::sync::{Arc, OnceLock};

use lectern_executor::Priority;
use lectern_logging::{Ctx, Value, debug};
use lectern_services::review::{
    BatchId, DiffSide, Excerpt, FileKey, FileRows, Note, NoteAnchor, NoteId, NotePlace, NoteRows,
    NoteState, NotesState, ReviewCommand, ReviewEvent, ReviewId, ReviewSnapshot, ReviewTarget,
    RouteKind, RouteOffer, SendOutcome, SentBatch, limits,
};
use lectern_services::session::Timestamp;

use super::attribute::FileResult;
use super::round::{Work, side_oid};
use super::rows::step_span;
use super::{Engine, Open, RepoLog, State, changed, lock};
use crate::log::{Index, NOTES_RETAIN_MS, NoteRecord, Record, TargetNotes};
use crate::notes;
use crate::seen::TargetKey;

/// The id of the built-in clipboard route (`RouteOffer::id`).
pub(crate) const CLIPBOARD: &str = "clipboard";

/// The routes a review offers before its session routes are known (and
/// the last of every list): the clipboard.
pub(crate) fn clipboard_only() -> Arc<[RouteOffer]> {
    static ROUTES: OnceLock<Arc<[RouteOffer]>> = OnceLock::new();
    Arc::clone(ROUTES.get_or_init(|| {
        Arc::from(vec![RouteOffer {
            id: Arc::from(CLIPBOARD),
            kind: RouteKind::Clipboard,
        }])
    }))
}

/// The `notes_state` of a review without notes.
pub(crate) fn empty_state() -> Arc<NotesState> {
    Arc::new(NotesState {
        routes: clipboard_only(),
        ..NotesState::default()
    })
}

/// The notes of a review in the index: its target there, the scope of its
/// tasks, and the repository's log (None for a folder project).
pub(super) struct Scope {
    pub target: TargetKey,
    pub ctx: Ctx,
    pub log: Option<Arc<RepoLog>>,
}

/// The command a log record or message names.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Op {
    Add,
    Edit,
    Remove,
    Discard,
    Send,
    Copy,
}

impl Op {
    pub(super) fn name(self) -> Value<'static> {
        Value::text(match self {
            Op::Add => "add",
            Op::Edit => "edit",
            Op::Remove => "remove",
            Op::Discard => "discard",
            Op::Send => "send",
            Op::Copy => "copy",
        })
    }
}

/// Why a command was ignored.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Reason {
    Empty,
    TooLong,
    TooMany,
    UnknownFile,
    /// The note is not there, or already sent.
    NotPending,
    /// The note, or the whole review, is being sent.
    Sending,
    Unchanged,
    NothingPending,
    /// The route is not among the review's.
    NoRoute,
    /// The batch is not one of the review's, or none of its notes is left.
    UnknownBatch,
}

impl Reason {
    pub(super) fn name(self) -> Value<'static> {
        Value::text(match self {
            Reason::Empty => "empty",
            Reason::TooLong => "too_long",
            Reason::TooMany => "too_many",
            Reason::UnknownFile => "unknown_file",
            Reason::NotPending => "not_pending",
            Reason::Sending => "sending",
            Reason::Unchanged => "unchanged",
            Reason::NothingPending => "nothing_pending",
            Reason::NoRoute => "no_route",
            Reason::UnknownBatch => "unknown_batch",
        })
    }
}

/// What a command did to the index.
enum Applied {
    Ignored(Op, Reason),
    Changed {
        op: Op,
        /// `AddNote`: the new note.
        added: Option<NoteId>,
    },
}

/// What `apply` needs besides the index.
struct Env<'a> {
    now: Timestamp,
    /// The notes of the batch being sent: neither edited, removed nor
    /// discarded until it is answered.
    sending: &'a [NoteId],
    /// `AddNote`: the anchor's file is one of the snapshot's.
    in_files: bool,
    /// `AddNote`: the new note's ID.
    id: NoteId,
}

/// The command on the index. Pure: no snapshot, no IO, no text scan.
fn apply(index: &mut Index, target: &TargetKey, command: &ReviewCommand, env: &Env<'_>) -> Applied {
    let notes = || index.target_notes(target);
    let text_ok = |text: &str| {
        if text.trim().is_empty() {
            Err(Reason::Empty)
        } else if text.len() > limits::NOTE_BYTES {
            Err(Reason::TooLong)
        } else {
            Ok(())
        }
    };
    match command {
        ReviewCommand::AddNote {
            anchor,
            text,
            excerpt,
            ..
        } => {
            let refused = text_ok(text).err().or_else(|| {
                if notes().map_or(0, TargetNotes::unsent) >= limits::PENDING_NOTES as usize {
                    Some(Reason::TooMany)
                } else if !env.in_files && excerpt.is_none() {
                    Some(Reason::UnknownFile)
                } else {
                    None
                }
            });
            if let Some(reason) = refused {
                return Applied::Ignored(Op::Add, reason);
            }
            // An ID the log has already (written by a clock that was
            // ahead) would replace that note: take the next one.
            let mut id = env.id;
            while notes().and_then(|tn| tn.note(id)).is_some() {
                id = NoteId(id.0 + 1);
            }
            index.record(Record::Note(NoteRecord {
                at: env.now,
                target: target.clone(),
                id,
                anchor: anchor.clone(),
                excerpt: excerpt.clone(),
                text: Arc::clone(text),
                created: env.now,
            }));
            Applied::Changed {
                op: Op::Add,
                added: Some(id),
            }
        }
        ReviewCommand::EditNote { note, text } => {
            let Some(tn) = notes() else {
                return Applied::Ignored(Op::Edit, Reason::NotPending);
            };
            let Some(n) = tn
                .note(*note)
                .filter(|n| tn.sent_in(n.id).is_none() && !env.sending.contains(&n.id))
            else {
                return Applied::Ignored(Op::Edit, Reason::NotPending);
            };
            if let Err(reason) = text_ok(text) {
                return Applied::Ignored(Op::Edit, reason);
            }
            if *text == n.text {
                return Applied::Ignored(Op::Edit, Reason::Unchanged);
            }
            // After `created`, so that the record says it is an edit.
            let at = Timestamp(env.now.0.max(n.created.0 + 1));
            let record = NoteRecord {
                at,
                target: target.clone(),
                id: n.id,
                anchor: n.anchor.clone(),
                excerpt: n.excerpt.clone(),
                text: Arc::clone(text),
                created: n.created,
            };
            index.record(Record::Note(record));
            Applied::Changed {
                op: Op::Edit,
                added: None,
            }
        }
        ReviewCommand::RemoveNote(note) => {
            if notes().and_then(|tn| tn.note(*note)).is_none() {
                return Applied::Ignored(Op::Remove, Reason::NotPending);
            }
            if env.sending.contains(note) {
                return Applied::Ignored(Op::Remove, Reason::Sending);
            }
            index.record(Record::NoteRemoved {
                at: env.now,
                target: target.clone(),
                ids: vec![*note],
            });
            Applied::Changed {
                op: Op::Remove,
                added: None,
            }
        }
        ReviewCommand::DiscardNotes => {
            let ids: Vec<NoteId> = notes()
                .map(|tn| {
                    tn.notes
                        .iter()
                        .filter(|n| tn.sent_in(n.id).is_none() && !env.sending.contains(&n.id))
                        .map(|n| n.id)
                        .collect()
                })
                .unwrap_or_default();
            if ids.is_empty() {
                return Applied::Ignored(Op::Discard, Reason::NothingPending);
            }
            index.record(Record::NoteRemoved {
                at: env.now,
                target: target.clone(),
                ids,
            });
            Applied::Changed {
                op: Op::Discard,
                added: None,
            }
        }
        // The other commands are not notes'.
        _ => Applied::Ignored(Op::Add, Reason::UnknownFile),
    }
}

/// Whether `file` is one of the snapshot's files.
fn in_files(work: &Work, file: &FileKey) -> bool {
    file.new
        .as_ref()
        .or(file.old.as_ref())
        .and_then(|p| work.by_path.get(p))
        .is_some_and(|list| list.iter().any(|&n| work.files[n as usize].key == *file))
}

/// Where a note sits (「锚点与位置」): by the OID of the content it was
/// written on, never by line or text. The second value is the position of
/// the file in the snapshot, for the order of the notes.
fn place(
    work: &Work,
    results: &[Option<Arc<FileResult>>],
    anchor: &NoteAnchor,
) -> (NotePlace, usize) {
    // The files that are the anchor's file now: the same key, the same new
    // path, or the file that was renamed away from the anchor's new path.
    let mut found: Vec<u32> = Vec::new();
    let add = |found: &mut Vec<u32>, n: u32| {
        if !found.contains(&n) {
            found.push(n);
        }
    };
    if let Some(p) = &anchor.file.new {
        for &n in work.by_path.get(p).into_iter().flatten() {
            add(&mut found, n);
        }
    }
    if let Some(p) = &anchor.file.old {
        for &n in work.by_path.get(p).into_iter().flatten() {
            if work.files[n as usize].key == anchor.file {
                add(&mut found, n);
            }
        }
    }
    let mut rows: Vec<NoteRows> = Vec::new();
    let mut at = usize::MAX;
    for &n in &found {
        let file = &work.files[n as usize];
        let change = &work.list.files[file.list as usize];
        let side = match anchor.side {
            DiffSide::Old => change.old.as_ref(),
            DiffSide::New => change.new.as_ref(),
        };
        let mut hit = false;
        if side_oid(side) == Some(anchor.oid) && !rows.contains(&NoteRows::Final) {
            rows.push(NoteRows::Final);
            hit = true;
        }
        // The narrative's steps, once the file is attributed.
        let chain = results
            .get(n as usize)
            .and_then(Option::as_ref)
            .and_then(|r| r.chain.as_ref());
        if let Some(chain) = chain {
            for id in work.steps_of.get(&n).into_iter().flatten() {
                let Some(span) = work
                    .steps
                    .get(id)
                    .and_then(|step| step_span(work, chain, step))
                else {
                    continue;
                };
                if chain.groups[span.group].diff.is_none() {
                    continue;
                }
                let version = match anchor.side {
                    DiffSide::Old => span.before,
                    DiffSide::New => span.after,
                };
                if version == Some(anchor.oid) {
                    rows.push(NoteRows::Step(*id));
                    hit = true;
                }
            }
        }
        if hit {
            at = at.min(n as usize);
        }
    }
    if !rows.is_empty() {
        rows.sort_by_key(|r| match r {
            NoteRows::Final => (0, 0),
            NoteRows::Step(id) => (1, id.0),
        });
        rows.dedup();
        return (NotePlace::Rows(rows.into()), at);
    }
    // The file is still there but none of its versions is the note's.
    let nearest = found
        .iter()
        .copied()
        .find(|&n| work.files[n as usize].key == anchor.file)
        .or_else(|| found.first().copied());
    match nearest {
        Some(n) => (
            NotePlace::Drifted {
                file: work.files[n as usize].key.clone(),
            },
            n as usize,
        ),
        None => (NotePlace::Elsewhere, usize::MAX),
    }
}

/// What a snapshot's `NotesState::last` becomes.
#[derive(Clone, Copy, Debug)]
pub(super) enum Last {
    /// The previous snapshot's.
    Keep,
    /// A new `AddNote`, `DiscardNotes` or `SendNotes`.
    Clear,
    /// The result of a send or a `CopyBatch`.
    Set(SendOutcome),
}

/// What `build` takes besides the index.
pub(super) struct View<'a> {
    /// The review's work and the attribution computed so far; without it (a
    /// folder project) every note is `Elsewhere`.
    pub placing: Option<(&'a Work, &'a [Option<Arc<FileResult>>])>,
    pub prev: (&'a Arc<[Note]>, &'a Arc<NotesState>),
    /// The batch being sent and its notes: in memory only (「发送」).
    pub sending: Option<(BatchId, &'a [NoteId])>,
    /// The session routes that apply to the target, then the clipboard.
    pub routes: &'a Arc<[RouteOffer]>,
    pub last: Last,
    pub now: Timestamp,
}

/// The `notes` and `notes_state` of `target` from the index. The sent
/// notes of batches over 14 days old are left out whether or not the index
/// was pruned yet. A result equal to the previous snapshot's is that
/// snapshot's `Arc`. A batch keeps the turn that answered it (`answered_by`
/// of the previous snapshot): finding it is the engine's, per snapshot.
pub(super) fn build(
    index: &Index,
    target: &TargetKey,
    view: View<'_>,
) -> (Arc<[Note]>, Arc<NotesState>) {
    let View {
        placing,
        prev,
        sending,
        routes,
        last,
        now,
    } = view;
    let in_flight: &[NoteId] = sending.map_or(&[], |(_, ids)| ids);
    let cutoff = now.0.saturating_sub(NOTES_RETAIN_MS);
    let mut keyed: Vec<((usize, u32, i64, u64), Note)> = Vec::new();
    let mut batches: Vec<SentBatch> = Vec::new();
    let mut pending = 0u32;
    let mut unsent = 0u32;
    if let Some(tn) = index.target_notes(target) {
        for e in &tn.notes {
            let state = match tn.sent_in(e.id) {
                None => {
                    unsent += 1;
                    match sending {
                        Some((batch, _)) if in_flight.contains(&e.id) => NoteState::Sending(batch),
                        _ => {
                            pending += 1;
                            NoteState::Pending
                        }
                    }
                }
                Some(batch) => {
                    if tn.batch(batch).is_some_and(|b| b.at.0 < cutoff) {
                        continue;
                    }
                    NoteState::Sent(batch)
                }
            };
            let (place, at) = match placing {
                Some((work, results)) => place(work, results, &e.anchor),
                None => (NotePlace::Elsewhere, usize::MAX),
            };
            // In the snapshot's order: the files', then by line; the notes
            // of no file by the time they were written.
            let line = if at == usize::MAX { 0 } else { e.anchor.line };
            keyed.push((
                (at, line, e.created.0, e.id.0),
                Note {
                    id: e.id,
                    anchor: e.anchor.clone(),
                    text: Arc::clone(&e.text),
                    created_at: e.created,
                    edited_at: (e.at > e.created).then_some(e.at),
                    state,
                    place,
                    excerpt: e.excerpt.clone(),
                },
            ));
        }
        batches = tn
            .batches
            .iter()
            .filter(|b| b.at.0 >= cutoff)
            .map(|b| SentBatch {
                id: b.id,
                at: b.at,
                via: b.via,
                count: b.notes.len() as u32,
                answered_by: prev
                    .1
                    .batches
                    .iter()
                    .find(|p| p.id == b.id)
                    .and_then(|p| p.answered_by),
            })
            .collect();
        batches.sort_by_key(|b| (b.at, b.id));
    }
    keyed.sort_by_key(|(key, _)| *key);
    let list: Vec<Note> = keyed.into_iter().map(|(_, n)| n).collect();
    let (prev_notes, prev_state) = prev;
    let state = NotesState {
        routes: Arc::clone(routes),
        pending,
        unsent,
        sending: sending.map(|(batch, _)| batch),
        last: match last {
            Last::Keep => prev_state.last,
            Last::Clear => None,
            Last::Set(outcome) => Some(outcome),
        },
        batches: batches.into(),
    };
    let notes = if **prev_notes == *list {
        Arc::clone(prev_notes)
    } else {
        Arc::from(list)
    };
    let state = if **prev_state == state {
        Arc::clone(prev_state)
    } else {
        Arc::new(state)
    };
    (notes, state)
}

/// The text of the excerpt of a new note, to be taken on a `Background`
/// task.
pub(super) struct ExcerptJob {
    rows: Arc<FileRows>,
    anchor: NoteAnchor,
}

/// The log's target of a review without a work: a folder project's
/// session.
pub(super) fn folder_target(target: &ReviewTarget) -> Option<TargetKey> {
    match target {
        ReviewTarget::Session(key) => Some(TargetKey::Session {
            source: key.source.0.clone(),
            id: key.id.clone(),
        }),
        _ => None,
    }
}

impl Engine {
    /// A new note ID: the time in Unix milliseconds times 1024 plus a
    /// counter, never one that was given before in this process.
    pub(super) fn next_note_id(&self, now: Timestamp) -> NoteId {
        let mut last = lock(&self.note_ids);
        *last = (*last + 1).max((now.0.max(0) as u64) << 10);
        NoteId(*last)
    }

    /// Where the notes of review `id` live. None while it is loading or
    /// failed: no files, no log.
    pub(super) fn scope(&self, state: &mut State, id: ReviewId) -> Option<Scope> {
        let open = state.reviews.get(&id)?;
        let work = open.work.clone();
        let folder = work.is_none()
            && open.snapshot.degraded == Some(lectern_services::review::Degraded::FolderProject);
        match (&work, folder.then(|| folder_target(&open.target)).flatten()) {
            (Some(w), _) => {
                let log = self.log_for(state, w.git.common_dir(), w.ctx);
                Some(Scope {
                    target: w.target_key.clone(),
                    ctx: w.ctx,
                    log: Some(log),
                })
            }
            (None, Some(target)) => Some(Scope {
                target,
                ctx: open.ctx,
                log: None,
            }),
            (None, None) => None,
        }
    }

    /// Runs `f` on review `id` and the index of its notes: the repository's
    /// (its lock held, taken after the state's) or, for a folder project,
    /// the review's own, whose queued records are dropped.
    pub(super) fn with_index<R>(
        &self,
        state: &mut State,
        id: ReviewId,
        sc: &Scope,
        f: impl FnOnce(&mut Open, &mut Index) -> R,
    ) -> Option<R> {
        match &sc.log {
            Some(log) => {
                let mut mem = lock(&log.mem);
                let open = state.reviews.get_mut(&id)?;
                Some(f(open, &mut mem.index))
            }
            None => {
                let open = state.reviews.get_mut(&id)?;
                let mut index = open.folder_notes.take().unwrap_or_default();
                let result = f(open, &mut index);
                index.take_pending();
                open.folder_notes = Some(index);
                Some(result)
            }
        }
    }

    /// `AddNote`, `EditNote`, `RemoveNote` and `DiscardNotes` (the calling
    /// thread, in memory only).
    pub(super) fn note_command(
        &self,
        state: &mut State,
        id: ReviewId,
        command: ReviewCommand,
        events: &mut Vec<ReviewEvent>,
    ) {
        let Some(open) = state.reviews.get(&id) else {
            return;
        };
        let work = open.work.clone();
        let folder = work.is_none()
            && open.snapshot.degraded == Some(lectern_services::review::Degraded::FolderProject);
        let (target, ctx) = match (&work, folder.then(|| folder_target(&open.target)).flatten()) {
            (Some(w), _) => (w.target_key.clone(), w.ctx),
            (None, Some(key)) => (key, open.ctx),
            // Still loading, or failed: no files, no log.
            (None, None) => {
                let op = op_of(&command);
                ignored(id, op, Reason::UnknownFile);
                return;
            }
        };
        let log = work
            .as_ref()
            .map(|w| self.log_for(state, w.git.common_dir(), w.ctx));
        let now = (self.cfg.clock)();
        let sending: Vec<NoteId> = state
            .reviews
            .get(&id)
            .and_then(|o| o.sending.as_ref())
            .map(|f| f.ids.clone())
            .unwrap_or_default();
        let env = Env {
            now,
            sending: &sending,
            in_files: match &command {
                ReviewCommand::AddNote { anchor, .. } => {
                    work.as_ref().is_some_and(|w| in_files(w, &anchor.file))
                }
                _ => false,
            },
            id: self.next_note_id(now),
        };
        let mut guard = log.as_ref().map(|l| lock(&l.mem));
        let mut local: Option<Index> = None;
        let index: &mut Index = match guard.as_mut() {
            Some(g) => &mut g.index,
            None => local.insert(
                state
                    .reviews
                    .get_mut(&id)
                    .and_then(|o| o.folder_notes.take())
                    .unwrap_or_default(),
            ),
        };
        let applied = apply(index, &target, &command, &env);
        let (op, added) = match applied {
            Applied::Ignored(op, reason) => {
                if let Some(l) = local.take()
                    && let Some(open) = state.reviews.get_mut(&id)
                {
                    open.folder_notes = Some(l);
                }
                ignored(id, op, reason);
                return;
            }
            Applied::Changed { op, added } => (op, added),
        };
        let Some(open) = state.reviews.get_mut(&id) else {
            return;
        };
        // The excerpt of a note added on a row the view has prepared: the
        // text is scanned later, on a task.
        let job = match (&command, added) {
            (
                ReviewCommand::AddNote {
                    rows: Some(key),
                    anchor,
                    excerpt: None,
                    ..
                },
                Some(note),
            ) => open
                .rows
                .get(key)
                .filter(|r| notes::anchored_text(r, anchor).is_some())
                .map(|rows| {
                    (
                        note,
                        ExcerptJob {
                            rows: Arc::clone(rows),
                            anchor: anchor.clone(),
                        },
                    )
                }),
            _ => None,
        };
        // The job is kept with the review: the task below takes it, or the
        // send of a batch that has the note (`complete_excerpts`) does,
        // whichever comes first.
        let job = job.map(|(note, job)| {
            open.excerpts.insert(note, job);
            note
        });
        let last = if matches!(op, Op::Add | Op::Discard) {
            Last::Clear
        } else {
            Last::Keep
        };
        let event = match guard.as_mut() {
            Some(g) => self.swap_notes(open, &g.index, &target, last, now),
            None => {
                let Some(index) = local.as_mut() else { return };
                // Nothing is written for a folder project.
                index.take_pending();
                let event = self.swap_notes(open, index, &target, last, now);
                open.folder_notes = local.take();
                event
            }
        };
        events.push(event);
        let notes = &open.snapshot.notes;
        let sent = notes
            .iter()
            .filter(|n| matches!(n.state, NoteState::Sent(_)))
            .count() as u32;
        let pending = open.snapshot.notes_state.pending;
        match added.and_then(|a| notes.iter().find(|n| n.id == a)) {
            Some(note) => debug!("note changed";
            id = id.0,
            op = op.name(),
            pending = pending,
            sent = sent,
            place = Value::text(match note.place {
                NotePlace::Rows(_) => "rows",
                NotePlace::Drifted { .. } => "drifted",
                NotePlace::Elsewhere => "elsewhere",
            })),
            None => debug!("note changed";
                id = id.0, op = op.name(), pending = pending, sent = sent),
        }
        let Some(g) = guard.as_mut() else {
            return;
        };
        // The record of a note with an excerpt to come waits for it.
        let flush = job.is_none() && !std::mem::replace(&mut g.flushing, true);
        drop(guard);
        let Some(log) = log else {
            return;
        };
        if let Some(note) = job {
            self.submit(Priority::Background, ctx, move |engine| {
                engine.complete_excerpts(id, &log, &target, &[note])
            });
        } else if flush {
            self.submit(Priority::Background, ctx, move |engine| engine.flush(&log));
        }
    }

    /// The snapshot of `open` with the notes of `index` (the caller holds
    /// the lock of the index, or owns it).
    pub(super) fn swap_notes(
        &self,
        open: &mut Open,
        index: &Index,
        target: &TargetKey,
        last: Last,
        now: Timestamp,
    ) -> ReviewEvent {
        let work = open.work.clone();
        let results = work.as_ref().map(|w| lock(&w.results));
        let placing = work
            .as_ref()
            .zip(results.as_ref())
            .map(|(w, r)| (&**w, r.as_slice()));
        let sending = open.sending.as_ref().map(|f| (f.batch, &f.ids[..]));
        let (notes, notes_state) = build(
            index,
            target,
            View {
                placing,
                prev: (&open.snapshot.notes, &open.snapshot.notes_state),
                sending,
                routes: &open.routes,
                last,
                now,
            },
        );
        drop(results);
        let snapshot = ReviewSnapshot {
            notes,
            notes_state,
            ..(*open.snapshot).clone()
        };
        changed(open, snapshot)
    }

    /// Takes the excerpts of the notes `notes` that are still to be taken
    /// from the text they were written on (`Background`), puts them in the
    /// notes and in their queued records, then appends the queue. A note
    /// whose job is gone (taken by the other caller) is skipped.
    pub(super) fn complete_excerpts(
        &self,
        id: ReviewId,
        log: &Arc<RepoLog>,
        target: &TargetKey,
        notes: &[NoteId],
    ) {
        let jobs: Vec<(NoteId, ExcerptJob)> = {
            let mut state = lock(&self.state);
            let Some(open) = state.reviews.get_mut(&id) else {
                return;
            };
            notes
                .iter()
                .filter_map(|n| open.excerpts.remove(n).map(|job| (*n, job)))
                .collect()
        };
        // The scan of the text is outside every lock.
        let found: Vec<(NoteId, Option<Excerpt>)> = jobs
            .into_iter()
            .map(|(note, job)| {
                let excerpt = notes::anchored_text(&job.rows, &job.anchor).and_then(|text| {
                    let hint = notes::hint(&job.rows, job.anchor.side, job.anchor.line);
                    notes::excerpt(text, job.anchor.line, hint)
                });
                (note, excerpt)
            })
            .collect();
        let mut events = Vec::new();
        let flush = {
            let mut state = lock(&self.state);
            let mut mem = lock(&log.mem);
            let mut attached = false;
            for (note, excerpt) in found {
                attached |= excerpt.is_some_and(|e| mem.index.attach_excerpt(target, note, e));
            }
            if attached && let Some(open) = state.reviews.get_mut(&id) {
                let now = (self.cfg.clock)();
                events.push(self.swap_notes(open, &mem.index, target, Last::Keep, now));
            }
            !std::mem::replace(&mut mem.flushing, true)
        };
        self.emit(events);
        if flush {
            self.flush(log);
        }
    }
}

fn op_of(command: &ReviewCommand) -> Op {
    match command {
        ReviewCommand::EditNote { .. } => Op::Edit,
        ReviewCommand::RemoveNote(_) => Op::Remove,
        ReviewCommand::DiscardNotes => Op::Discard,
        _ => Op::Add,
    }
}

pub(super) fn ignored(id: ReviewId, op: Op, reason: Reason) {
    debug!("note command ignored"; id = id.0, op = op.name(), reason = reason.name());
}
