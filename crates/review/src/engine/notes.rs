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
use lectern_logging::{Value, debug};
use lectern_services::review::{
    DiffSide, Excerpt, FileKey, FileRows, Note, NoteAnchor, NoteId, NotePlace, NoteRows, NoteState,
    NotesState, ReviewCommand, ReviewEvent, ReviewId, ReviewSnapshot, ReviewTarget, RouteKind,
    RouteOffer, SentBatch, limits,
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

/// The routes every review offers until a session route is registered
/// (step 7): the clipboard.
fn routes() -> Arc<[RouteOffer]> {
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
        routes: routes(),
        ..NotesState::default()
    })
}

/// The command a log record or message names.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Op {
    Add,
    Edit,
    Remove,
    Discard,
}

impl Op {
    fn name(self) -> Value<'static> {
        Value::text(match self {
            Op::Add => "add",
            Op::Edit => "edit",
            Op::Remove => "remove",
            Op::Discard => "discard",
        })
    }
}

/// Why a command was ignored.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Reason {
    Empty,
    TooLong,
    TooMany,
    UnknownFile,
    /// The note is not there, or already sent.
    NotPending,
    Unchanged,
    NothingPending,
}

impl Reason {
    fn name(self) -> Value<'static> {
        Value::text(match self {
            Reason::Empty => "empty",
            Reason::TooLong => "too_long",
            Reason::TooMany => "too_many",
            Reason::UnknownFile => "unknown_file",
            Reason::NotPending => "not_pending",
            Reason::Unchanged => "unchanged",
            Reason::NothingPending => "nothing_pending",
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
struct Env {
    now: Timestamp,
    /// `AddNote`: the anchor's file is one of the snapshot's.
    in_files: bool,
    /// `AddNote`: the new note's ID.
    id: NoteId,
}

/// The command on the index. Pure: no snapshot, no IO, no text scan.
fn apply(index: &mut Index, target: &TargetKey, command: &ReviewCommand, env: &Env) -> Applied {
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
            let Some(n) = tn.note(*note).filter(|n| tn.sent_in(n.id).is_none()) else {
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
                        .filter(|n| tn.sent_in(n.id).is_none())
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

/// The `notes` and `notes_state` of `target` from the index. `placing` is
/// the review's work and the attribution computed so far; without it (a
/// folder project) every note is `Elsewhere`. The sent notes of batches
/// over 14 days old are left out whether or not the index was pruned yet.
/// A result equal to the previous snapshot's is that snapshot's `Arc`.
pub(super) fn build(
    index: &Index,
    target: &TargetKey,
    placing: Option<(&Work, &[Option<Arc<FileResult>>])>,
    prev: (&Arc<[Note]>, &Arc<NotesState>),
    clear_last: bool,
    now: Timestamp,
) -> (Arc<[Note]>, Arc<NotesState>) {
    let cutoff = now.0.saturating_sub(NOTES_RETAIN_MS);
    let mut keyed: Vec<((usize, u32, i64, u64), Note)> = Vec::new();
    let mut batches: Vec<SentBatch> = Vec::new();
    let mut pending = 0u32;
    if let Some(tn) = index.target_notes(target) {
        for e in &tn.notes {
            let state = match tn.sent_in(e.id) {
                None => {
                    pending += 1;
                    NoteState::Pending
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
                answered_by: None,
            })
            .collect();
        batches.sort_by_key(|b| (b.at, b.id));
    }
    keyed.sort_by_key(|(key, _)| *key);
    let list: Vec<Note> = keyed.into_iter().map(|(_, n)| n).collect();
    let (prev_notes, prev_state) = prev;
    let state = NotesState {
        routes: routes(),
        pending,
        unsent: pending,
        sending: prev_state.sending,
        last: if clear_last { None } else { prev_state.last },
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
struct ExcerptJob {
    rows: Arc<FileRows>,
    anchor: NoteAnchor,
    note: NoteId,
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
    fn next_note_id(&self, now: Timestamp) -> NoteId {
        let mut last = lock(&self.note_ids);
        *last = (*last + 1).max((now.0.max(0) as u64) << 10);
        NoteId(*last)
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
        let env = Env {
            now,
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
                .map(|rows| ExcerptJob {
                    rows: Arc::clone(rows),
                    anchor: anchor.clone(),
                    note,
                }),
            _ => None,
        };
        let clear_last = matches!(op, Op::Add | Op::Discard);
        let event = match guard.as_mut() {
            Some(g) => self.swap_notes(open, &g.index, &target, clear_last, now),
            None => {
                let Some(index) = local.as_mut() else { return };
                // Nothing is written for a folder project.
                index.take_pending();
                let event = self.swap_notes(open, index, &target, clear_last, now);
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
        if let Some(job) = job {
            self.submit(Priority::Background, ctx, move |engine| {
                engine.take_excerpt(id, &log, target, job)
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
        clear_last: bool,
        now: Timestamp,
    ) -> ReviewEvent {
        let work = open.work.clone();
        let results = work.as_ref().map(|w| lock(&w.results));
        let placing = work
            .as_ref()
            .zip(results.as_ref())
            .map(|(w, r)| (&**w, r.as_slice()));
        let (notes, notes_state) = build(
            index,
            target,
            placing,
            (&open.snapshot.notes, &open.snapshot.notes_state),
            clear_last,
            now,
        );
        drop(results);
        let snapshot = ReviewSnapshot {
            notes,
            notes_state,
            ..(*open.snapshot).clone()
        };
        changed(open, snapshot)
    }

    /// Takes the excerpt of a new note from the text it was written on
    /// (`Background`), puts it in the note and in its queued record, then
    /// appends the queue.
    fn take_excerpt(&self, id: ReviewId, log: &Arc<RepoLog>, target: TargetKey, job: ExcerptJob) {
        let found: Option<Excerpt> =
            notes::anchored_text(&job.rows, &job.anchor).and_then(|text| {
                let hint = notes::hint(&job.rows, job.anchor.side, job.anchor.line);
                notes::excerpt(text, job.anchor.line, hint)
            });
        let mut events = Vec::new();
        let flush = {
            let mut state = lock(&self.state);
            let mut mem = lock(&log.mem);
            let attached = found.is_some_and(|e| mem.index.attach_excerpt(&target, job.note, e));
            if attached && let Some(open) = state.reviews.get_mut(&id) {
                let now = (self.cfg.clock)();
                events.push(self.swap_notes(open, &mem.index, &target, false, now));
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

fn ignored(id: ReviewId, op: Op, reason: Reason) {
    debug!("note command ignored"; id = id.0, op = op.name(), reason = reason.name());
}
