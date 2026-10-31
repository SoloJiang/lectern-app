//! Sending review notes in the engine (docs/design/review.md「审阅意见」的
//! 「发送」「再次复制」「与回应的那一轮对应」, 「审阅意见的发送途径」):
//! `SendNotes` and `CopyBatch`, the registered session routes, the
//! built-in clipboard route, the 10-second timeout of an answer, and the
//! link from a batch to the turn that answers it.
//!
//! A send marks its notes `Sending` on the calling thread and returns; a
//! `Background` task builds the prompt and calls the route with no lock
//! held. The route answers once, on any thread: the answer only submits a
//! task, which applies it (`resolve`) and holds the `BusyGuard` until the
//! new snapshot is published. An answer is told from a late one by the
//! number of its attempt, not by the batch (`CopyBatch` sends the same
//! batch again).

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lectern_executor::Priority;
use lectern_kernel::{ActivationToken, TokenState};
use lectern_logging::{Ctx, Value, debug, info, warn};
use lectern_services::changes::ChangePath;
use lectern_services::project::RelPath;
use lectern_services::review::{
    BatchId, DeliveryOutcome, DeliveryReply, DiffSide, Note, NoteDelivery, NoteId, NotePlace,
    NoteRoute, NoteRouteRegistration, NoteRows, NoteState, NotesState, ReviewEvent, ReviewId,
    RouteBlock, RouteKind, RouteOffer, SendFailure, SendOutcome, SentBatch, SentVia, limits,
};
use lectern_services::session::Turn;

use super::notes::{self, Last, Op, Reason};
use super::{Engine, State, lock};
use crate::log::{Record, SentRecord};
use crate::notes::prompt::{self, PromptNote, Where};

/// An answer later than this counts as `SendFailure::TimedOut`.
pub(crate) const DELIVERY_TIMEOUT: Duration = Duration::from_secs(10);
/// The most a prompt may be (the same as an ACP prompt's, 「发送」).
pub(crate) const MAX_PROMPT_BYTES: usize = 256 * 1024;
/// The notes whose position `NoteDelivery::locations` lists.
const LOCATIONS: usize = 8;
/// A turn counts as an answer when it started this long before the batch
/// was sent at the earliest (the clocks of two processes differ).
const CLOCK_SKEW_MS: i64 = 1000;

/// Which of the two deliveries of a review an attempt belongs to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Flight {
    /// `SendNotes`: the notes are `Sending` until the answer.
    Send,
    /// `CopyBatch`: only `NotesState::last` changes.
    Copy,
}

/// A delivery under way, in memory only.
pub(super) struct InFlight {
    /// Told from another attempt of the same batch by this number.
    pub attempt: u64,
    pub batch: BatchId,
    /// The notes marked `Sending` (empty for a copy).
    pub ids: Vec<NoteId>,
    pub route: RouteKind,
    /// The registration of the session route in use.
    pub registration: Option<u64>,
    pub started: Instant,
}

/// The session routes registered, in registration order.
#[derive(Default)]
pub(super) struct Routes {
    next: u64,
    entries: Vec<Registered>,
    /// A retry for a `Pending` token is queued.
    retry: bool,
}

struct Registered {
    seq: u64,
    id: Arc<str>,
    route: Arc<dyn NoteRoute>,
    token: ActivationToken,
}

impl Routes {
    pub(super) fn clear(&mut self) {
        self.entries.clear();
    }
}

/// What a delivery came to.
#[derive(Clone, Copy, Debug)]
enum Answer {
    /// The route answered; the second is the clipboard's error kind.
    Reply(DeliveryOutcome, Option<&'static str>),
    TimedOut,
    /// The prompt is over the limit; the route was not called.
    TooLarge,
    /// The route was unregistered, or its plugin deactivated.
    Gone,
}

/// Which attempt an answer is for.
#[derive(Clone, Copy)]
struct Ticket {
    id: ReviewId,
    flight: Flight,
    attempt: u64,
    batch: BatchId,
    /// The size of the prompt and the notes in it (a copy's count).
    bytes: usize,
    count: u32,
}

/// What the task of a delivery took from the snapshot.
struct Gathered {
    batch: BatchId,
    registration: Option<u64>,
    target: lectern_services::review::ReviewTarget,
    root: Arc<Path>,
    ctx: Ctx,
    notes: Vec<Note>,
}

impl Engine {
    // ---- the routes -------------------------------------------------------

    /// `Review::register_note_route`: from now on the open reviews offer
    /// the route where it `applies`, until the registration is dropped.
    /// The same ID registered again replaces the earlier registration.
    pub(crate) fn register_note_route(
        &self,
        route: Arc<dyn NoteRoute>,
        token: ActivationToken,
    ) -> NoteRouteRegistration {
        let id = route.id();
        let (seq, replaced) = {
            let mut routes = lock(&self.note_routes);
            routes.next += 1;
            let seq = routes.next;
            let replaced: Vec<u64> = routes
                .entries
                .iter()
                .filter(|r| r.id == id)
                .map(|r| r.seq)
                .collect();
            routes.entries.retain(|r| r.id != id);
            routes.entries.push(Registered {
                seq,
                id,
                route,
                token,
            });
            (seq, replaced)
        };
        debug!("note route registered"; registration = seq);
        for old in replaced {
            self.route_gone(old);
        }
        self.routes_changed();
        let me = self.me.clone();
        NoteRouteRegistration::new(move || {
            if let Some(engine) = me.upgrade() {
                engine.unregister_note_route(seq);
            }
        })
    }

    /// The registration `seq` is dropped: the route is not called again,
    /// and a delivery under way through it fails.
    fn unregister_note_route(&self, seq: u64) {
        lock(&self.note_routes).entries.retain(|r| r.seq != seq);
        debug!("note route released"; registration = seq);
        self.route_gone(seq);
        self.routes_changed();
    }

    /// Fails the deliveries under way through registration `seq`.
    fn route_gone(&self, seq: u64) {
        let gone: Vec<(Ticket, Ctx)> = {
            let state = lock(&self.state);
            let mut gone = Vec::new();
            for (id, open) in &state.reviews {
                for (flight, slot) in [(Flight::Send, &open.sending), (Flight::Copy, &open.copying)]
                {
                    if let Some(f) = slot.as_ref().filter(|f| f.registration == Some(seq)) {
                        gone.push((
                            Ticket {
                                id: *id,
                                flight,
                                attempt: f.attempt,
                                batch: f.batch,
                                bytes: 0,
                                count: f.ids.len() as u32,
                            },
                            open.ctx,
                        ));
                    }
                }
            }
            gone
        };
        for (ticket, ctx) in gone {
            self.submit(Priority::Background, ctx, move |engine| {
                engine.resolve(ticket, Answer::Gone)
            });
        }
    }

    /// The routes of the open reviews are asked again (`applies`) and the
    /// snapshots re-issued where they changed.
    fn routes_changed(&self) {
        let open: Vec<(ReviewId, Ctx)> = lock(&self.state)
            .reviews
            .iter()
            .map(|(id, o)| (*id, o.ctx))
            .collect();
        for (id, ctx) in open {
            self.submit(Priority::Background, ctx, move |engine| {
                engine.republish_routes(id)
            });
        }
    }

    /// The routes that apply to `target` in registration order, then the
    /// clipboard. Calls `applies` with no engine lock held, each under its
    /// token; a route registered from `started` has a `Pending` token that
    /// admits nothing yet: asked again after `PENDING_RETRY`.
    pub(super) fn offer_routes(
        &self,
        target: &lectern_services::review::ReviewTarget,
    ) -> Arc<[RouteOffer]> {
        let entries: Vec<(Arc<str>, Arc<dyn NoteRoute>, ActivationToken)> = lock(&self.note_routes)
            .entries
            .iter()
            .map(|r| (r.id.clone(), r.route.clone(), r.token.clone()))
            .collect();
        if entries.is_empty() {
            return notes::clipboard_only();
        }
        let mut offers = Vec::with_capacity(entries.len() + 1);
        let mut pending = false;
        for (id, route, token) in entries {
            match token.enter() {
                Some(_admission) => {
                    if route.applies(target) {
                        offers.push(RouteOffer {
                            id,
                            kind: RouteKind::Session,
                        });
                    }
                }
                None => pending |= token.state() == TokenState::Pending,
            }
        }
        offers.push(RouteOffer {
            id: Arc::from(notes::CLIPBOARD),
            kind: RouteKind::Clipboard,
        });
        if pending {
            self.retry_routes();
        }
        offers.into()
    }

    fn retry_routes(&self) {
        let Some(host) = self.cfg.host.clone() else {
            return;
        };
        {
            let mut routes = lock(&self.note_routes);
            if routes.retry {
                return;
            }
            routes.retry = true;
        }
        let me = self.me.clone();
        host.later(
            super::brief::PENDING_RETRY,
            Box::new(move || {
                let Some(engine) = me.upgrade() else { return };
                engine.submit(Priority::Background, Ctx::default(), move |engine| {
                    lock(&engine.note_routes).retry = false;
                    engine.routes_changed();
                });
            }),
        );
    }

    /// The routes of review `id` as they are now (`Background`).
    fn republish_routes(&self, id: ReviewId) {
        let Some(target) = lock(&self.state).reviews.get(&id).map(|o| o.target.clone()) else {
            return;
        };
        let routes = self.offer_routes(&target);
        let mut events = Vec::new();
        {
            let mut state = lock(&self.state);
            let Some(open) = state.reviews.get_mut(&id) else {
                return;
            };
            if *open.routes == *routes {
                return;
            }
            open.routes = routes;
            if let Some(sc) = self.scope(&mut state, id) {
                let now = (self.cfg.clock)();
                events.extend(self.with_index(&mut state, id, &sc, |open, index| {
                    self.swap_notes(open, index, &sc.target, Last::Keep, now)
                }));
            }
        }
        self.emit(events);
    }

    // ---- the commands -----------------------------------------------------

    /// `SendNotes` (the calling thread, in memory only).
    pub(super) fn send_command(
        &self,
        state: &mut State,
        id: ReviewId,
        route: Arc<str>,
        events: &mut Vec<ReviewEvent>,
    ) {
        let refuse = |reason: Reason| notes::ignored(id, Op::Send, reason);
        let Some(open) = state.reviews.get(&id) else {
            return;
        };
        let offers = &open.snapshot.notes_state;
        if offers.pending == 0 {
            return refuse(Reason::NothingPending);
        }
        if open.sending.is_some() {
            return refuse(Reason::Sending);
        }
        let Some(offer) = offers.routes.iter().find(|r| *r.id == *route) else {
            return refuse(Reason::NoRoute);
        };
        let kind = offer.kind;
        let registration = match kind {
            RouteKind::Clipboard => None,
            RouteKind::Session => {
                let routes = lock(&self.note_routes);
                match routes.entries.iter().find(|r| r.id == route) {
                    Some(r) => Some(r.seq),
                    None => return refuse(Reason::NoRoute),
                }
            }
        };
        // The earliest first, as many as a batch takes.
        let mut pending: Vec<&Note> = open
            .snapshot
            .notes
            .iter()
            .filter(|n| n.state == NoteState::Pending)
            .collect();
        pending.sort_by_key(|n| (n.created_at, n.id));
        let ids: Vec<NoteId> = pending
            .iter()
            .take(limits::PENDING_NOTES as usize)
            .map(|n| n.id)
            .collect();
        let Some(sc) = self.scope(state, id) else {
            return refuse(Reason::UnknownFile);
        };
        let now = (self.cfg.clock)();
        let batch = BatchId(self.next_note_id(now).0);
        let attempt = self.next_attempt();
        let event = self.with_index(state, id, &sc, |open, index| {
            open.sending = Some(InFlight {
                attempt,
                batch,
                ids,
                route: kind,
                registration,
                started: Instant::now(),
            });
            self.swap_notes(open, index, &sc.target, Last::Clear, now)
        });
        events.extend(event);
        self.submit(Priority::Background, sc.ctx, move |engine| {
            engine.deliver(id, Flight::Send, attempt)
        });
    }

    /// `CopyBatch` (the calling thread; the snapshot does not change).
    pub(super) fn copy_command(&self, state: &mut State, id: ReviewId, batch: BatchId) {
        let Some(open) = state.reviews.get_mut(&id) else {
            return;
        };
        let refuse = |reason: Reason| notes::ignored(id, Op::Copy, reason);
        if open.sending.is_some() {
            return refuse(Reason::Sending);
        }
        let known = open
            .snapshot
            .notes_state
            .batches
            .iter()
            .any(|b| b.id == batch);
        let left = open
            .snapshot
            .notes
            .iter()
            .any(|n| n.state == NoteState::Sent(batch));
        if !known || !left {
            return refuse(Reason::UnknownBatch);
        }
        let attempt = self.next_attempt();
        open.copying = Some(InFlight {
            attempt,
            batch,
            ids: Vec::new(),
            route: RouteKind::Clipboard,
            registration: None,
            started: Instant::now(),
        });
        let ctx = open.ctx;
        self.submit(Priority::Background, ctx, move |engine| {
            engine.deliver(id, Flight::Copy, attempt)
        });
    }

    fn next_attempt(&self) -> u64 {
        let mut last = lock(&self.attempts);
        *last += 1;
        *last
    }

    // ---- the delivery -----------------------------------------------------

    /// The task of a delivery (`Background`): the excerpts of a send that
    /// are still to be taken, the prompt, the route.
    fn deliver(&self, id: ReviewId, flight: Flight, attempt: u64) {
        if flight == Flight::Send {
            self.complete_batch_excerpts(id, attempt);
        }
        let Some(gathered) = self.gather(id, flight, attempt) else {
            return;
        };
        let Gathered {
            batch,
            registration,
            target,
            root,
            ctx,
            notes: batch_notes,
        } = gathered;
        let count = batch_notes.len() as u32;
        let mut ticket = Ticket {
            id,
            flight,
            attempt,
            batch,
            bytes: 0,
            count,
        };
        if batch_notes.is_empty() {
            return self.resolve(ticket, Answer::Gone);
        }
        let prompt_notes: Vec<PromptNote> = batch_notes.iter().map(prompt_note).collect();
        let text: Arc<str> = Arc::from(prompt::prompt(batch, &prompt_notes));
        ticket.bytes = text.len();
        if text.len() > MAX_PROMPT_BYTES {
            return self.resolve(ticket, Answer::TooLarge);
        }
        self.arm_timeout(ticket, ctx);
        let me = self.me.clone();
        let answer = move |answer: Answer| {
            if let Some(engine) = me.upgrade() {
                engine.submit(Priority::Background, ctx, move |engine| {
                    engine.resolve(ticket, answer)
                });
            }
        };
        let Some(seq) = registration else {
            // The built-in clipboard route.
            (self.cfg.clipboard)(
                text,
                Box::new(move |result| {
                    answer(match result {
                        Ok(()) => {
                            Answer::Reply(DeliveryOutcome::Delivered(SentVia::Clipboard), None)
                        }
                        Err(e) => Answer::Reply(DeliveryOutcome::Failed, Some(e.kind())),
                    })
                }),
            );
            return;
        };
        let entry = lock(&self.note_routes)
            .entries
            .iter()
            .find(|r| r.seq == seq)
            .map(|r| (r.route.clone(), r.token.clone()));
        let Some((route, token)) = entry else {
            return self.resolve(ticket, Answer::Gone);
        };
        // Held while the route runs: its plugin is not deactivated under us.
        let Some(_admission) = token.enter() else {
            return self.resolve(ticket, Answer::Gone);
        };
        let locations: Arc<[(RelPath, Option<u32>)]> = batch_notes
            .iter()
            .take(LOCATIONS)
            .zip(&prompt_notes)
            .map(|(_, p)| {
                let path: RelPath = Arc::from(String::from_utf8_lossy(&p.path.0).as_ref());
                (path, (p.place == Where::Current).then_some(p.line + 1))
            })
            .collect();
        route.deliver(
            NoteDelivery {
                batch,
                target,
                root,
                text,
                count,
                locations,
            },
            DeliveryReply::new(move |outcome| answer(Answer::Reply(outcome, None))),
        );
    }

    /// Takes the excerpts of the batch's notes that are still to be taken.
    fn complete_batch_excerpts(&self, id: ReviewId, attempt: u64) {
        let (log, target, ids) = {
            let mut state = lock(&self.state);
            let Some(ids) = state
                .reviews
                .get(&id)
                .and_then(|o| o.sending.as_ref())
                .filter(|f| f.attempt == attempt)
                .map(|f| f.ids.clone())
            else {
                return;
            };
            let Some(sc) = self.scope(&mut state, id) else {
                return;
            };
            (sc.log, sc.target, ids)
        };
        if let Some(log) = log {
            self.complete_excerpts(id, &log, &target, &ids);
        }
    }

    /// The batch's notes in the snapshot's order, with what a delivery
    /// needs; None when the attempt is not the current one any more.
    fn gather(&self, id: ReviewId, flight: Flight, attempt: u64) -> Option<Gathered> {
        let state = lock(&self.state);
        let open = state.reviews.get(&id)?;
        let f = match flight {
            Flight::Send => open.sending.as_ref(),
            Flight::Copy => open.copying.as_ref(),
        }
        .filter(|f| f.attempt == attempt)?;
        let notes: Vec<Note> = open
            .snapshot
            .notes
            .iter()
            .filter(|n| match (flight, n.state) {
                (Flight::Send, NoteState::Sending(b)) | (Flight::Copy, NoteState::Sent(b)) => {
                    b == f.batch
                }
                _ => false,
            })
            .cloned()
            .collect();
        Some(Gathered {
            batch: f.batch,
            registration: f.registration,
            target: open.target.clone(),
            root: open
                .root
                .clone()
                .unwrap_or_else(|| Arc::from(Path::new(""))),
            ctx: open.ctx,
            notes,
        })
    }

    /// `DELIVERY_TIMEOUT` after the call: the attempt, if still current,
    /// fails. Without a timer (the engine without a host) there is no
    /// timeout.
    fn arm_timeout(&self, ticket: Ticket, ctx: Ctx) {
        let Some(host) = self.cfg.host.clone() else {
            return;
        };
        let me = self.me.clone();
        host.later(
            DELIVERY_TIMEOUT,
            Box::new(move || {
                let Some(engine) = me.upgrade() else { return };
                engine.submit(Priority::Background, ctx, move |engine| {
                    engine.resolve(ticket, Answer::TimedOut)
                });
            }),
        );
    }

    /// Applies the answer of an attempt (`Background`): a sent batch is
    /// recorded, a failed one goes back to pending; the snapshot is swapped
    /// and `Changed` emitted before the task ends.
    fn resolve(&self, ticket: Ticket, answer: Answer) {
        let Ticket {
            id,
            flight,
            attempt,
            batch,
            bytes,
            count,
        } = ticket;
        let mut events = Vec::new();
        let flush;
        {
            let mut state = lock(&self.state);
            let Some(open) = state.reviews.get(&id) else {
                return;
            };
            let current = match flight {
                Flight::Send => open.sending.as_ref(),
                Flight::Copy => open.copying.as_ref(),
            };
            let Some(f) = current.filter(|f| f.attempt == attempt) else {
                if let Answer::Reply(outcome, _) = answer {
                    debug!("late delivery reply dropped";
                    id = id.0,
                    batch = batch.0,
                    outcome = Value::text(match outcome {
                        DeliveryOutcome::Delivered(_) => "delivered",
                        DeliveryOutcome::NotNow(_) => "not_now",
                        DeliveryOutcome::Failed => "failed",
                    }));
                }
                return;
            };
            let (route, started, ids) = (f.route, f.started, f.ids.clone());
            let took = started.elapsed();
            let now = (self.cfg.clock)();
            let failure: Option<(SendFailure, &'static str, Option<&'static str>)> = match answer {
                Answer::Reply(DeliveryOutcome::Delivered(_), _) => None,
                Answer::Reply(DeliveryOutcome::NotNow(block), _) => Some((
                    SendFailure::NotNow(block),
                    match block {
                        RouteBlock::Starting => "starting",
                        RouteBlock::Busy => "busy",
                        RouteBlock::NoSession => "no_session",
                    },
                    None,
                )),
                Answer::Reply(DeliveryOutcome::Failed, error) => {
                    Some((SendFailure::Failed, "failed", error))
                }
                Answer::TimedOut => Some((SendFailure::TimedOut, "timed_out", None)),
                Answer::TooLarge => Some((SendFailure::Failed, "too_large", None)),
                Answer::Gone => Some((SendFailure::Failed, "failed", None)),
            };
            let via = match answer {
                Answer::Reply(DeliveryOutcome::Delivered(via), _) => Some(via),
                _ => None,
            };
            let route_name = match route {
                RouteKind::Clipboard => "clipboard",
                RouteKind::Session => "session",
            };
            let Some(sc) = self.scope(&mut state, id) else {
                if let Some(open) = state.reviews.get_mut(&id) {
                    match flight {
                        Flight::Send => open.sending = None,
                        Flight::Copy => open.copying = None,
                    }
                }
                return;
            };
            let count = if flight == Flight::Send {
                ids.len() as u32
            } else {
                count
            };
            let outcome = match (flight, failure) {
                (Flight::Send, None) => SendOutcome::Sent {
                    batch,
                    via: via.unwrap_or(SentVia::Clipboard),
                    count,
                },
                (Flight::Send, Some((reason, _, _))) => SendOutcome::Failed { route, reason },
                (Flight::Copy, failed) => SendOutcome::CopiedAgain {
                    batch,
                    count,
                    failed: failed.map(|(reason, _, _)| reason),
                },
            };
            let record = (flight == Flight::Send && failure.is_none()).then(|| {
                Record::NotesSent(SentRecord {
                    at: now,
                    target: sc.target.clone(),
                    batch,
                    notes: ids.clone(),
                    via: via.unwrap_or(SentVia::Clipboard),
                })
            });
            let event = self.with_index(&mut state, id, &sc, |open, index| {
                if let Some(record) = record {
                    index.record(record);
                }
                match flight {
                    Flight::Send => open.sending = None,
                    Flight::Copy => open.copying = None,
                }
                self.swap_notes(open, index, &sc.target, Last::Set(outcome), now)
            });
            events.extend(event);
            match (flight, failure) {
                (Flight::Send, None) => info!("review notes sent";
                    id = id.0,
                    batch = batch.0,
                    via = Value::text(via_name(via.unwrap_or(SentVia::Clipboard))),
                    notes = count,
                    prompt_bytes = bytes,
                    took_us = took),
                (Flight::Send, Some((_, reason, error))) => warn!("review notes not sent";
                    id = id.0,
                    batch = batch.0,
                    route = Value::text(route_name),
                    reason = Value::text(reason),
                    error = error.map(Value::text),
                    notes = count),
                (Flight::Copy, None) => info!("review notes copied again";
                    id = id.0,
                    batch = batch.0,
                    notes = count,
                    prompt_bytes = bytes,
                    took_us = took),
                (Flight::Copy, Some((_, reason, error))) => warn!("review notes not copied again";
                    id = id.0,
                    batch = batch.0,
                    reason = Value::text(reason),
                    error = error.map(Value::text),
                    notes = count),
            }
            flush = record_queued(&sc, flight, failure.is_none());
        }
        self.emit(events);
        if let Some((log, ctx)) = flush {
            self.submit(Priority::Background, ctx, move |engine| engine.flush(&log));
        }
    }
}

/// Whether a record was queued for the log (a delivered send): the append
/// task to submit, and its scope.
fn record_queued(
    sc: &notes::Scope,
    flight: Flight,
    delivered: bool,
) -> Option<(Arc<super::RepoLog>, Ctx)> {
    if flight != Flight::Send || !delivered {
        return None;
    }
    let log = sc.log.clone()?;
    let start = !std::mem::replace(&mut lock(&log.mem).flushing, true);
    start.then_some((log, sc.ctx))
}

fn via_name(via: SentVia) -> &'static str {
    match via {
        SentVia::Clipboard => "clipboard",
        SentVia::TerminalPaste => "terminal",
        SentVia::Acp => "acp",
    }
}

/// How a note is located in the prompt (「prompt 的模板」).
fn location(note: &Note) -> Where {
    match (&note.place, note.anchor.side) {
        (NotePlace::Rows(rows), DiffSide::New) if rows.contains(&NoteRows::Final) => Where::Current,
        (NotePlace::Rows(rows), DiffSide::Old) if rows.contains(&NoteRows::Final) => Where::Base,
        _ => Where::Earlier,
    }
}

fn prompt_note(note: &Note) -> PromptNote {
    let file = &note.anchor.file;
    PromptNote {
        path: file
            .new
            .clone()
            .or_else(|| file.old.clone())
            .unwrap_or_else(|| ChangePath(Arc::from(&b""[..]))),
        line: note.anchor.line,
        place: location(note),
        excerpt: note.excerpt.clone(),
        text: Arc::clone(&note.text),
    }
}

/// Finds, for each batch no turn is linked to yet, the earliest turn of
/// the review's own session that started no earlier than the batch was
/// sent (less the clock skew) and whose prompt preview holds the batch's
/// marker. Only the previews are compared, the prompts are not read.
/// None when nothing new was found.
pub(super) fn link(id: ReviewId, turns: &[Turn], batches: &[SentBatch]) -> Option<Vec<SentBatch>> {
    let mut out: Option<Vec<SentBatch>> = None;
    for (i, batch) in batches.iter().enumerate() {
        if batch.answered_by.is_some() {
            continue;
        }
        let marker = prompt::marker(batch.id);
        let found = turns
            .iter()
            .filter(|t| t.started_at.0 >= batch.at.0 - CLOCK_SKEW_MS)
            .filter(|t| t.prompt_preview.contains(&marker))
            .min_by_key(|t| (t.started_at, t.ordinal));
        if let Some(turn) = found {
            let list = out.get_or_insert_with(|| batches.to_vec());
            list[i].answered_by = Some(lectern_services::review::TurnRef {
                session: 0,
                ordinal: turn.ordinal,
            });
            debug!("notes answered"; id = id.0, batch = batch.id.0, ordinal = turn.ordinal);
        }
    }
    out
}

/// The batch each turn answers, from the batches' links.
pub(super) fn answers_of(
    state: &NotesState,
) -> std::collections::HashMap<lectern_services::review::TurnRef, BatchId> {
    state
        .batches
        .iter()
        .filter_map(|b| b.answered_by.map(|turn| (turn, b.id)))
        .collect()
}

/// A `NotesState` with other batches.
pub(super) fn with_batches(state: &Arc<NotesState>, batches: Vec<SentBatch>) -> Arc<NotesState> {
    Arc::new(NotesState {
        batches: batches.into(),
        ..(**state).clone()
    })
}
