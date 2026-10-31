//! Sending review notes (`notes_send_*`, `notes_copy_again_*`,
//! `notes_link_*`, docs/design/review.md「审阅意见」的「发送」「再次复制」
//! 「与回应的那一轮对应」, 「审阅意见的发送途径」): the clipboard route and
//! session routes with their answers, the 10-second timeout, the batch in
//! the annotation log, `CopyBatch`, and the link from a batch to the turn
//! that answers it. The clipboard is `FakeClipboard`: no test writes the
//! real one.

use std::sync::Mutex;
use std::time::Duration;

use lectern_kernel::{
    Activation, Active, Context, Kernel, Manifest, Plugin, PluginError, PluginHandle, PluginSet,
};
use lectern_services::review::{
    BatchId, DeliveryOutcome, DiffSide, NoteRoute, NoteRouteRegistration, NoteState, ReviewEvent,
    RouteBlock, RouteKind, SendFailure, SendOutcome, SentVia, TurnRef, limits,
};

use super::notes::{Rig, add, anchor, excerpt, lines_of, new_oid, record, seed, session_target};
use super::*;
use crate::engine::Engine;
use crate::engine::brief::PENDING_RETRY;
use crate::engine::send::{DELIVERY_TIMEOUT, MAX_PROMPT_BYTES};
use crate::testing::{ClipMode, FakeRoute, RouteMode};

/// The review of session "a" of `unrelated_commit` (as `rig()`), with the
/// session source kept for turns that come later, and the manual host's
/// clock when `timers`.
fn rig_with(timers: bool) -> (Rig, Arc<FakeSessions>) {
    let (fx, sessions) = unrelated_commit();
    let d = if timers {
        Driven::with_timers(
            Arc::clone(&fx.projects),
            vec![Arc::clone(&sessions)],
            fx.dir.path(),
            NOW,
        )
    } else {
        fx.driven(vec![Arc::clone(&sessions)])
    };
    let (id, _) = open(&d, target("a"));
    (Rig { fx, d, id }, sessions)
}

const CLIPBOARD: &str = "clipboard";

fn send_to(r: &Rig, route: &str) -> Arc<ReviewSnapshot> {
    r.command(ReviewCommand::SendNotes {
        route: route.into(),
    })
}

fn sending_of(s: &ReviewSnapshot) -> BatchId {
    s.notes_state.sending.expect("a batch is being sent")
}

/// `n` pending notes on the final change of f.txt, with their excerpts.
fn add_notes(r: &Rig, n: u32) {
    let shown = r.shown();
    let a = anchor(modified("f.txt"), DiffSide::New, 2, new_oid(&shown));
    for i in 0..n {
        r.command(add(
            None,
            a.clone(),
            &format!("note {i}"),
            Some(excerpt(0, &["A", "b", "C"])),
        ));
    }
}

fn route_ids(s: &ReviewSnapshot) -> Vec<(String, RouteKind)> {
    s.notes_state
        .routes
        .iter()
        .map(|r| (r.id.to_string(), r.kind))
        .collect()
}

fn terminal(mode: RouteMode) -> FakeRoute {
    FakeRoute::new("terminal", mode)
}

fn states(s: &ReviewSnapshot) -> Vec<NoteState> {
    s.notes.iter().map(|n| n.state).collect()
}

// ---- notes_send_* -----------------------------------------------------------

#[test]
fn notes_send_the_clipboard_marks_the_notes_then_records_the_batch() {
    let (r, sessions) = rig_with(false);
    let shown = r.shown();
    let a = anchor(modified("f.txt"), DiffSide::New, 2, new_oid(&shown));
    let first = anchor(modified("f.txt"), DiffSide::New, 0, new_oid(&shown));
    // Written out of order; the first has no excerpt yet.
    r.command(add(Some(shown.fin_key.clone()), a, "Why C?", None));
    r.command(add(None, first, "Why A?", Some(excerpt(0, &["A", "b"]))));
    let before = r.snapshot();
    let s = send_to(&r, CLIPBOARD);

    // Marked before the command returns; nothing is on the clipboard yet.
    assert!(s.generation > before.generation);
    let batch = sending_of(&s);
    assert!(s.notes.iter().all(|n| n.state == NoteState::Sending(batch)));
    assert_eq!((s.notes_state.pending, s.notes_state.unsent), (0, 2));
    assert_eq!(s.notes_state.last, None);
    assert!(r.d.clipboard().writes().is_empty());
    assert!(lines_of(&r.fx, "notes_sent").is_empty());

    r.d.run();
    let s = r.snapshot();
    assert_eq!(s.notes_state.sending, None);
    assert!(s.notes.iter().all(|n| n.state == NoteState::Sent(batch)));
    assert_eq!((s.notes_state.pending, s.notes_state.unsent), (0, 0));
    assert_eq!(
        s.notes_state.last,
        Some(SendOutcome::Sent {
            batch,
            via: SentVia::Clipboard,
            count: 2
        })
    );
    assert_eq!(
        &*s.notes_state.batches,
        &[lectern_services::review::SentBatch {
            id: batch,
            at: NOW,
            via: SentVia::Clipboard,
            count: 2,
            answered_by: None,
        }]
    );

    // One text, with the marker of the batch, in the snapshot's order, and
    // the excerpt that was still to be taken when the batch was sent.
    let writes = r.d.clipboard().writes();
    assert_eq!(writes.len(), 1);
    let marker = format!("[lectern-notes {:016x}]", batch.0);
    assert!(writes[0].starts_with(&format!("Review notes from Lectern {marker}: 2 notes")));
    insta::assert_snapshot!("notes_send_prompt", writes[0]);

    // The record went to the log; a new engine reads the batch back.
    r.d.run();
    let written = lines_of(&r.fx, "notes_sent");
    assert_eq!(written.len(), 1);
    assert!(written[0].contains(&format!("\"batch\":\"{:016x}\"", batch.0)));
    assert!(written[0].contains("\"via\":\"clipboard\""));
    let Rig { fx, d, .. } = r;
    drop(d);
    let d = fx.driven(vec![sessions]);
    let (_, s) = open(&d, target("a"));
    assert!(s.notes.iter().all(|n| n.state == NoteState::Sent(batch)));
    assert_eq!(s.notes_state.batches.len(), 1);
    assert_eq!(s.notes_state.last, None, "the outcome is not kept");
}

#[test]
fn notes_send_a_clipboard_that_refuses_gives_the_notes_back() {
    let (r, _) = rig_with(false);
    add_notes(&r, 2);
    r.d.clipboard().set_mode(ClipMode::Reject);
    let s = send_to(&r, CLIPBOARD);
    let batch = sending_of(&s);
    r.d.run();
    let s = r.snapshot();
    assert_eq!(
        s.notes_state.last,
        Some(SendOutcome::Failed {
            route: RouteKind::Clipboard,
            reason: SendFailure::Failed
        })
    );
    assert_eq!(states(&s), [NoteState::Pending; 2]);
    assert_eq!(
        (
            s.notes_state.pending,
            s.notes_state.unsent,
            s.notes_state.sending
        ),
        (2, 2, None)
    );
    assert!(s.notes_state.batches.is_empty());
    r.d.run();
    assert!(lines_of(&r.fx, "notes_sent").is_empty());

    // Again, with a clipboard that works: a new batch.
    r.d.clipboard().set_mode(ClipMode::Accept);
    let again = sending_of(&send_to(&r, CLIPBOARD));
    assert_ne!(again, batch);
    r.d.run();
    let s = r.snapshot();
    assert_eq!(states(&s), [NoteState::Sent(again); 2]);
    assert_eq!(r.d.clipboard().writes().len(), 2);

    // No clipboard at all is a failure too.
    add_notes(&r, 1);
    r.d.clipboard().set_mode(ClipMode::Unsupported);
    send_to(&r, CLIPBOARD);
    r.d.run();
    assert!(matches!(
        r.snapshot().notes_state.last,
        Some(SendOutcome::Failed {
            route: RouteKind::Clipboard,
            reason: SendFailure::Failed
        })
    ));
}

#[test]
fn notes_send_a_session_route_gets_the_batch_and_answers() {
    let (r, _) = rig_with(false);
    let host = crate::testing::host();
    let route = terminal(RouteMode::NotNow(RouteBlock::Starting));
    let _reg =
        r.d.register_note_route(Arc::new(route.clone()), host.handle.token());
    r.d.run();
    assert_eq!(
        route_ids(&r.snapshot()),
        [
            ("terminal".to_owned(), RouteKind::Session),
            ("clipboard".to_owned(), RouteKind::Clipboard)
        ]
    );
    add_notes(&r, 1);

    let s = send_to(&r, "terminal");
    let batch = sending_of(&s);
    r.d.run();
    let deliveries = route.deliveries();
    assert_eq!(deliveries.len(), 1);
    let d = &deliveries[0];
    assert_eq!((d.batch, d.count), (batch, 1));
    assert_eq!(d.target, target("a"));
    assert_eq!(&*d.root, r.d.engine.work(r.id).unwrap().git.root.as_path());
    assert!(
        d.text
            .starts_with("Review notes from Lectern [lectern-notes ")
    );
    assert!(d.text.contains("1. f.txt:3\n"), "{}", d.text);
    // The first eight: the path relative to the root, the line (1-based)
    // when the anchor is on the content the worktree has now.
    assert_eq!(d.locations.len(), 1);
    assert_eq!((&*d.locations[0].0, d.locations[0].1), ("f.txt", Some(3)));
    assert!(r.d.clipboard().writes().is_empty(), "not the clipboard");

    for (mode, block) in [
        (
            RouteMode::NotNow(RouteBlock::Starting),
            RouteBlock::Starting,
        ),
        (RouteMode::NotNow(RouteBlock::Busy), RouteBlock::Busy),
        (
            RouteMode::NotNow(RouteBlock::NoSession),
            RouteBlock::NoSession,
        ),
    ] {
        route.set_mode(mode);
        send_to(&r, "terminal");
        r.d.run();
        let s = r.snapshot();
        assert_eq!(
            s.notes_state.last,
            Some(SendOutcome::Failed {
                route: RouteKind::Session,
                reason: SendFailure::NotNow(block)
            })
        );
        assert_eq!(states(&s), [NoteState::Pending]);
    }
    route.set_mode(RouteMode::Fail);
    send_to(&r, "terminal");
    r.d.run();
    assert!(matches!(
        r.snapshot().notes_state.last,
        Some(SendOutcome::Failed {
            reason: SendFailure::Failed,
            ..
        })
    ));

    route.set_mode(RouteMode::Deliver(SentVia::TerminalPaste));
    let batch = sending_of(&send_to(&r, "terminal"));
    r.d.run();
    let s = r.snapshot();
    assert_eq!(
        s.notes_state.last,
        Some(SendOutcome::Sent {
            batch,
            via: SentVia::TerminalPaste,
            count: 1
        })
    );
    assert_eq!(s.notes_state.batches[0].via, SentVia::TerminalPaste);
    assert_eq!(states(&s), [NoteState::Sent(batch)]);
    r.d.run();
    assert!(lines_of(&r.fx, "notes_sent")[0].contains("\"via\":\"terminal\""));
}

#[test]
fn notes_send_an_answer_after_ten_seconds_is_a_timeout() {
    let (r, _) = rig_with(true);
    let host = crate::testing::host();
    let route = terminal(RouteMode::Hold);
    let _reg =
        r.d.register_note_route(Arc::new(route.clone()), host.handle.token());
    r.d.run();
    add_notes(&r, 2);
    let batch = sending_of(&send_to(&r, "terminal"));
    r.d.run();
    assert_eq!(route.deliveries().len(), 1);

    r.d.advance(DELIVERY_TIMEOUT - Duration::from_secs(1));
    r.d.run();
    assert_eq!(r.snapshot().notes_state.sending, Some(batch), "not yet");
    r.d.advance(Duration::from_secs(1));
    r.d.run();
    let s = r.snapshot();
    assert_eq!(
        s.notes_state.last,
        Some(SendOutcome::Failed {
            route: RouteKind::Session,
            reason: SendFailure::TimedOut
        })
    );
    assert_eq!(states(&s), [NoteState::Pending; 2]);
    assert_eq!(s.notes_state.sending, None);

    // The route answers after all: dropped, nothing changes.
    route.release(DeliveryOutcome::Delivered(SentVia::TerminalPaste));
    r.d.run();
    assert!(Arc::ptr_eq(&s, &r.snapshot()));
    r.d.run();
    assert!(lines_of(&r.fx, "notes_sent").is_empty());

    // An answer in time is not undone by the timer.
    route.set_mode(RouteMode::Deliver(SentVia::TerminalPaste));
    let batch = sending_of(&send_to(&r, "terminal"));
    r.d.run();
    r.d.advance(DELIVERY_TIMEOUT * 2);
    r.d.run();
    assert_eq!(states(&r.snapshot()), [NoteState::Sent(batch); 2]);
}

#[test]
fn notes_send_the_clipboard_has_the_timeout_too() {
    let (r, _) = rig_with(true);
    add_notes(&r, 1);
    r.d.clipboard().set_mode(ClipMode::Hold);
    send_to(&r, CLIPBOARD);
    r.d.run();
    r.d.advance(DELIVERY_TIMEOUT);
    r.d.run();
    let s = r.snapshot();
    assert_eq!(
        s.notes_state.last,
        Some(SendOutcome::Failed {
            route: RouteKind::Clipboard,
            reason: SendFailure::TimedOut
        })
    );
    assert_eq!(states(&s), [NoteState::Pending]);
    r.d.clipboard().release(Ok(()));
    r.d.run();
    assert!(Arc::ptr_eq(&s, &r.snapshot()));
}

#[test]
fn notes_send_a_route_unregistered_meanwhile_fails_the_batch() {
    let (r, _) = rig_with(false);
    let host = crate::testing::host();
    let route = terminal(RouteMode::Hold);
    let reg =
        r.d.register_note_route(Arc::new(route.clone()), host.handle.token());
    r.d.run();
    add_notes(&r, 1);
    let batch = sending_of(&send_to(&r, "terminal"));
    r.d.run();
    assert_eq!(r.snapshot().notes_state.sending, Some(batch));
    drop(reg);
    r.d.run();
    let s = r.snapshot();
    assert_eq!(
        s.notes_state.last,
        Some(SendOutcome::Failed {
            route: RouteKind::Session,
            reason: SendFailure::Failed
        })
    );
    assert_eq!(states(&s), [NoteState::Pending]);
    // The route no longer applies, and the reply it dropped is late.
    assert_eq!(
        route_ids(&s),
        [(CLIPBOARD.to_owned(), RouteKind::Clipboard)]
    );
    route.drop_held();
    r.d.run();
    assert!(Arc::ptr_eq(&s, &r.snapshot()));
    // A send through the route that is gone is refused.
    r.ignored(ReviewCommand::SendNotes {
        route: "terminal".into(),
    });
}

#[test]
fn notes_send_a_route_whose_plugin_is_deactivated_fails() {
    let (r, _) = rig_with(false);
    let host = crate::testing::host();
    let route = terminal(RouteMode::Deliver(SentVia::TerminalPaste));
    let _reg =
        r.d.register_note_route(Arc::new(route.clone()), host.handle.token());
    r.d.run();
    add_notes(&r, 1);
    host.deactivate();
    send_to(&r, "terminal");
    r.d.run();
    let s = r.snapshot();
    assert_eq!(
        s.notes_state.last,
        Some(SendOutcome::Failed {
            route: RouteKind::Session,
            reason: SendFailure::Failed
        })
    );
    assert!(route.deliveries().is_empty(), "never called");
    assert_eq!(states(&s), [NoteState::Pending]);
}

#[test]
fn notes_send_routes_follow_the_registrations() {
    let (r, _) = rig_with(false);
    let host = crate::testing::host();
    let clipboard_only = [(CLIPBOARD.to_owned(), RouteKind::Clipboard)];
    assert_eq!(route_ids(&r.snapshot()), clipboard_only);
    let ids = |r: &Rig| {
        route_ids(&r.snapshot())
            .into_iter()
            .map(|(id, _)| id)
            .collect::<Vec<_>>()
    };

    let a = FakeRoute::new("a", RouteMode::Fail);
    let b = FakeRoute::new("b", RouteMode::Fail);
    let reg_a =
        r.d.register_note_route(Arc::new(a.clone()), host.handle.token());
    let _reg_b =
        r.d.register_note_route(Arc::new(b.clone()), host.handle.token());
    r.d.run();
    assert_eq!(ids(&r), ["a", "b", "clipboard"]);

    // The same ID again replaces the earlier registration, at the end; the
    // earlier one's drop does nothing.
    let a2 = FakeRoute::new("a", RouteMode::Fail);
    let _reg_a2 =
        r.d.register_note_route(Arc::new(a2.clone()), host.handle.token());
    r.d.run();
    assert_eq!(ids(&r), ["b", "a", "clipboard"]);
    drop(reg_a);
    r.d.run();
    assert_eq!(ids(&r), ["b", "a", "clipboard"]);
    add_notes(&r, 1);
    send_to(&r, "a");
    r.d.run();
    assert_eq!((a.deliveries().len(), a2.deliveries().len()), (0, 1));

    // `applies` decides per target; the others only offer the clipboard.
    b.set_applies(false);
    let c = FakeRoute::new("c", RouteMode::Fail);
    let reg_c = r.d.register_note_route(Arc::new(c), host.handle.token());
    r.d.run();
    assert_eq!(ids(&r), ["a", "c", "clipboard"]);
    drop(reg_c);
    r.d.run();
    assert_eq!(ids(&r), ["a", "clipboard"]);
    let outside = r.d.open(r.d.outside());
    r.d.run();
    assert_eq!(
        route_ids(&r.d.snapshot(outside).unwrap()),
        clipboard_only,
        "a session route does not apply to the changes outside sessions"
    );
}

/// A plugin that registers `route` from its `started`, as a route's plugin
/// does: its token is `Pending` then.
struct Registrar {
    engine: Arc<Engine>,
    route: FakeRoute,
    registration: Arc<Mutex<Option<NoteRouteRegistration>>>,
    /// The routes offered for the target while `started` runs.
    during: Arc<Mutex<Option<usize>>>,
}

impl Plugin for Registrar {
    fn manifest(&self) -> Manifest {
        Manifest {
            provides: Vec::new(),
            requires: Vec::new(),
            activation: vec![Activation::AfterFirstFrame],
        }
    }

    fn activate(&self, _: &mut Context<'_>) -> Result<Box<dyn Active>, PluginError> {
        Ok(Box::new(Registrar {
            engine: self.engine.clone(),
            route: self.route.clone(),
            registration: self.registration.clone(),
            during: self.during.clone(),
        }))
    }
}

impl Active for Registrar {
    fn started(&mut self, handle: PluginHandle) {
        let registration = self.engine.register_note_route(
            Arc::new(self.route.clone()) as Arc<dyn NoteRoute>,
            handle.token(),
        );
        // The token admits nothing yet: the route is not offered, and the
        // engine asks again later.
        let offers = self.engine.offer_routes(&target("a"));
        *self.during.lock().unwrap() = Some(offers.len());
        *self.registration.lock().unwrap() = Some(registration);
    }
}

#[test]
fn notes_send_a_route_registered_from_started_is_offered_once_its_token_is_live() {
    let (r, _) = rig_with(true);
    let registration = Arc::new(Mutex::new(None));
    let during = Arc::new(Mutex::new(None));
    let kernel = Kernel::start(PluginSet::new().with(
        "registrar",
        Registrar {
            engine: r.d.engine.clone(),
            route: terminal(RouteMode::Fail),
            registration: registration.clone(),
            during: during.clone(),
        },
    ));
    kernel.barrier();
    // Inside `started` the token admitted nothing: only the clipboard.
    assert_eq!(*during.lock().unwrap(), Some(1));
    // Afterwards it is `Live`: the registration's refresh offers the route,
    // and the retry that the pending token armed finds nothing to change.
    r.d.run();
    assert_eq!(route_ids(&r.snapshot()).len(), 2);
    r.d.advance(PENDING_RETRY);
    r.d.run();
    assert_eq!(
        route_ids(&r.snapshot()),
        [
            ("terminal".to_owned(), RouteKind::Session),
            (CLIPBOARD.to_owned(), RouteKind::Clipboard)
        ]
    );
    let (done, wait) = std::sync::mpsc::channel();
    kernel.shutdown(move || {
        let _ = done.send(());
    });
    wait.recv_timeout(Duration::from_secs(10)).unwrap();
}

#[test]
fn notes_send_a_batch_is_the_earliest_forty_and_the_rest_wait() {
    let (fx, sessions) = unrelated_commit();
    let t = session_target("a");
    let a = anchor(
        modified("f.txt"),
        DiffSide::New,
        0,
        BlobId::from_bytes(&[9; 20]).unwrap(),
    );
    // Written backwards, so that the order of the log is not the order of
    // the notes.
    let records = (0..45u64)
        .map(|n| {
            record(
                &t,
                0x200 + n,
                a.clone(),
                &format!("n{n}"),
                Timestamp(NOW.0 - 1000 * n as i64),
            )
        })
        .collect();
    seed(&fx, records);
    let d = fx.driven(vec![sessions]);
    let (id, s) = open(&d, target("a"));
    assert_eq!((s.notes_state.pending, s.notes_state.unsent), (45, 45));
    let r = Rig { fx, d, id };

    let s = send_to(&r, CLIPBOARD);
    let batch = sending_of(&s);
    let sending: Vec<&str> = s
        .notes
        .iter()
        .filter(|n| n.state == NoteState::Sending(batch))
        .map(|n| &*n.text)
        .collect();
    assert_eq!(sending.len(), limits::PENDING_NOTES as usize);
    // The earliest written: n44 down to n5; n0 to n4 wait.
    assert!(sending.contains(&"n44") && sending.contains(&"n5"));
    assert!(!sending.contains(&"n4") && !sending.contains(&"n0"));
    assert_eq!((s.notes_state.pending, s.notes_state.unsent), (5, 45));
    r.d.run();
    let s = r.snapshot();
    assert_eq!(s.notes_state.batches[0].count, 40);
    assert_eq!((s.notes_state.pending, s.notes_state.unsent), (5, 5));
    let text = r.d.clipboard().writes()[0].clone();
    assert!(text.contains(": 40 notes on your changes."));
    assert!(!text.contains("n4\n") && text.contains("n44"), "{text}");

    // The rest go in the next one.
    let second = sending_of(&send_to(&r, CLIPBOARD));
    assert_ne!(second, batch);
    r.d.run();
    let s = r.snapshot();
    assert_eq!(s.notes_state.batches.len(), 2);
    assert_eq!(s.notes_state.batches[1].count, 5);
    assert_eq!((s.notes_state.pending, s.notes_state.unsent), (0, 0));
}

#[test]
fn notes_send_a_command_that_cannot_be_obeyed_is_ignored() {
    let (r, _) = rig_with(false);
    // Nothing to send.
    r.ignored(ReviewCommand::SendNotes {
        route: CLIPBOARD.into(),
    });
    add_notes(&r, 1);
    // A route the review does not offer.
    r.ignored(ReviewCommand::SendNotes {
        route: "terminal".into(),
    });
    // A batch that does not exist cannot be copied.
    r.ignored(ReviewCommand::CopyBatch(BatchId(7)));

    // While a batch is out: no second one, its notes stay as they are.
    r.d.clipboard().set_mode(ClipMode::Hold);
    let s = send_to(&r, CLIPBOARD);
    let batch = sending_of(&s);
    let sending = s.notes[0].id;
    r.d.run();
    r.ignored(ReviewCommand::SendNotes {
        route: CLIPBOARD.into(),
    });
    r.ignored(ReviewCommand::RemoveNote(sending));
    r.ignored(ReviewCommand::EditNote {
        note: sending,
        text: "changed".into(),
    });
    r.ignored(ReviewCommand::DiscardNotes);
    r.ignored(ReviewCommand::CopyBatch(batch));
    // A note written meanwhile is pending, and discarded alone.
    add_notes(&r, 1);
    let s = r.snapshot();
    assert_eq!((s.notes_state.pending, s.notes_state.unsent), (1, 2));
    assert_eq!(s.notes_state.sending, Some(batch));
    let s = r.command(ReviewCommand::DiscardNotes);
    assert_eq!(states(&s), [NoteState::Sending(batch)]);
    r.d.clipboard().release(Ok(()));
    r.d.run();
    assert_eq!(states(&r.snapshot()), [NoteState::Sent(batch)]);
}

#[test]
fn notes_cmd_forty_unsent_with_thirty_sending_refuse_the_41st() {
    let (r, _) = rig_with(false);
    add_notes(&r, 30);
    r.d.clipboard().set_mode(ClipMode::Hold);
    let batch = sending_of(&send_to(&r, CLIPBOARD));
    r.d.run();
    // 30 are out; ten more fit, the 41st does not.
    add_notes(&r, 10);
    let s = r.snapshot();
    assert_eq!((s.notes_state.pending, s.notes_state.unsent), (10, 40));
    let shown = r.shown();
    let a = anchor(modified("f.txt"), DiffSide::New, 2, new_oid(&shown));
    let one_more = add(None, a, "41st", Some(excerpt(0, &["C"])));
    r.ignored(one_more.clone());

    // The batch fails and comes back: still 40, still refused.
    r.d.clipboard()
        .release(Err(crate::testing::WriteError::Rejected));
    r.d.run();
    let s = r.snapshot();
    assert_eq!((s.notes_state.pending, s.notes_state.unsent), (40, 40));
    assert!(s.notes.iter().all(|n| n.state == NoteState::Pending));
    r.ignored(one_more);
    // A send takes all 40 at once.
    let s = send_to(&r, CLIPBOARD);
    assert_ne!(sending_of(&s), batch);
    assert_eq!(s.notes_state.pending, 0);
}

#[test]
fn notes_send_a_prompt_over_256_kib_is_not_sent() {
    let (r, _) = rig_with(false);
    // Paths of 3 KiB: 40 notes with 4 KiB of text each pass the limit.
    let oid = BlobId::from_bytes(&[9; 20]).unwrap();
    for i in 0..limits::PENDING_NOTES {
        let long = format!("{}{i}", "d/".repeat(1500));
        let file = FileKey {
            old: Some(path(&long)),
            new: Some(path(&long)),
        };
        r.command(add(
            None,
            anchor(file, DiffSide::New, 0, oid),
            &"x".repeat(limits::NOTE_BYTES),
            Some(excerpt(0, &["y"])),
        ));
    }
    send_to(&r, CLIPBOARD);
    r.d.run();
    let s = r.snapshot();
    assert_eq!(
        s.notes_state.last,
        Some(SendOutcome::Failed {
            route: RouteKind::Clipboard,
            reason: SendFailure::Failed
        })
    );
    assert_eq!(s.notes_state.pending, limits::PENDING_NOTES);
    assert!(
        r.d.clipboard().writes().is_empty(),
        "not truncated, not sent"
    );
    const { assert!(MAX_PROMPT_BYTES == 256 * 1024) };
}

// ---- notes_copy_again_* -----------------------------------------------------

/// Two notes sent through the clipboard.
fn sent_batch(r: &Rig) -> BatchId {
    add_notes(r, 2);
    let batch = sending_of(&send_to(r, CLIPBOARD));
    r.d.run();
    assert_eq!(states(&r.snapshot()), [NoteState::Sent(batch); 2]);
    batch
}

#[test]
fn notes_copy_again_writes_the_same_batch_again_and_changes_no_note() {
    let (r, _) = rig_with(false);
    let batch = sent_batch(&r);
    r.d.run();
    let before = r.snapshot();
    let first = r.d.clipboard().writes()[0].clone();
    r.d.take_events();

    // The snapshot does not change on the calling thread.
    r.d.command(r.id, ReviewCommand::CopyBatch(batch));
    assert!(Arc::ptr_eq(&before, &r.snapshot()));
    r.d.run();
    let writes = r.d.clipboard().writes();
    assert_eq!(writes.len(), 2);
    assert_eq!(writes[1], first, "the same batch, the same marker");
    let s = r.snapshot();
    assert_eq!(
        s.notes_state.last,
        Some(SendOutcome::CopiedAgain {
            batch,
            count: 2,
            failed: None
        })
    );
    // Only `last` changed.
    assert!(Arc::ptr_eq(&before.notes, &s.notes));
    assert_eq!(s.notes_state.batches, before.notes_state.batches);
    assert_eq!(states(&s), [NoteState::Sent(batch); 2]);
    assert!(
        r.d.take_events()
            .iter()
            .any(|(e, _)| matches!(e, ReviewEvent::Changed { .. })),
        "the result is an event"
    );
    r.d.run();
    assert_eq!(lines_of(&r.fx, "notes_sent").len(), 1, "no new record");
}

#[test]
fn notes_copy_again_takes_the_notes_that_are_left() {
    let (r, _) = rig_with(false);
    let batch = sent_batch(&r);
    let gone = r.snapshot().notes[0].id;
    r.command(ReviewCommand::RemoveNote(gone));
    r.command(ReviewCommand::CopyBatch(batch));
    r.d.run();
    let s = r.snapshot();
    assert_eq!(
        s.notes_state.last,
        Some(SendOutcome::CopiedAgain {
            batch,
            count: 1,
            failed: None
        })
    );
    let text = r.d.clipboard().writes()[1].clone();
    assert!(text.contains(": 1 note on your changes."), "{text}");
    assert!(text.contains(&format!("[lectern-notes {:016x}]", batch.0)));
    // The batch keeps the count it was sent with.
    assert_eq!(s.notes_state.batches[0].count, 2);

    // With no note of the batch left there is nothing to copy.
    let left = r.snapshot().notes[0].id;
    r.command(ReviewCommand::RemoveNote(left));
    r.ignored(ReviewCommand::CopyBatch(batch));
}

#[test]
fn notes_copy_again_failures_are_only_in_last() {
    let (r, _) = rig_with(true);
    let batch = sent_batch(&r);
    let before = r.snapshot();

    r.d.clipboard().set_mode(ClipMode::Reject);
    r.command(ReviewCommand::CopyBatch(batch));
    r.d.run();
    let s = r.snapshot();
    assert_eq!(
        s.notes_state.last,
        Some(SendOutcome::CopiedAgain {
            batch,
            count: 2,
            failed: Some(SendFailure::Failed)
        })
    );
    assert_eq!(states(&s), [NoteState::Sent(batch); 2]);
    assert_eq!(s.notes_state.batches, before.notes_state.batches);

    r.d.clipboard().set_mode(ClipMode::Hold);
    r.command(ReviewCommand::CopyBatch(batch));
    r.d.run();
    r.d.advance(DELIVERY_TIMEOUT);
    r.d.run();
    assert_eq!(
        r.snapshot().notes_state.last,
        Some(SendOutcome::CopiedAgain {
            batch,
            count: 2,
            failed: Some(SendFailure::TimedOut)
        })
    );
    // A late answer is dropped.
    let s = r.snapshot();
    r.d.clipboard().release(Ok(()));
    r.d.run();
    assert!(Arc::ptr_eq(&s, &r.snapshot()));
    assert_eq!(lines_of(&r.fx, "notes_sent").len(), 1);
}

#[test]
fn notes_copy_again_goes_to_the_clipboard_whatever_route_sent_the_batch() {
    let (r, _) = rig_with(false);
    let host = crate::testing::host();
    let route = terminal(RouteMode::Deliver(SentVia::TerminalPaste));
    let _reg =
        r.d.register_note_route(Arc::new(route.clone()), host.handle.token());
    r.d.run();
    add_notes(&r, 1);
    let batch = sending_of(&send_to(&r, "terminal"));
    r.d.run();
    assert_eq!(route.deliveries().len(), 1);
    assert!(r.d.clipboard().writes().is_empty());
    r.command(ReviewCommand::CopyBatch(batch));
    r.d.run();
    assert_eq!(route.deliveries().len(), 1, "the route is not asked again");
    assert_eq!(r.d.clipboard().writes().len(), 1);
    assert_eq!(
        r.snapshot().notes_state.batches[0].via,
        SentVia::TerminalPaste
    );
}

// ---- notes_link_* -----------------------------------------------------------

/// A turn of session "a" that starts `at` milliseconds from the epoch and
/// whose prompt starts as the prompt of `batch` does.
fn answer(id: &str, at: i64, batch: BatchId) -> serde_json::Value {
    json!({
        "id": id,
        "at": at,
        "prompt": format!(
            "Review notes from Lectern [lectern-notes {:016x}]: 2 notes on your changes.",
            batch.0
        ),
        "items": [],
    })
}

fn append(sessions: &FakeSessions, d: &Driven, turn: serde_json::Value) {
    sessions.append("a", &turn.to_string());
    for event in sessions.take_events() {
        d.sessions_changed(&event);
    }
    d.run();
}

fn answers(s: &ReviewSnapshot) -> Vec<(u32, Option<BatchId>)> {
    s.turns
        .as_ref()
        .unwrap()
        .iter()
        .filter(|t| t.at.session == 0)
        .map(|t| (t.at.ordinal, t.answers))
        .collect()
}

#[test]
fn notes_link_the_turn_that_answers_the_batch_is_marked() {
    let (r, sessions) = rig_with(false);
    let batch = sent_batch(&r);
    let s = r.snapshot();
    assert!(s.notes_state.batches[0].answered_by.is_none());
    assert_eq!(answers(&s), [(1, None)]);

    // The agent's next turn is the user's paste of the prompt.
    append(&sessions, &r.d, answer("reply", NOW.0 + 5000, batch));
    let s = r.snapshot();
    assert_eq!(
        s.notes_state.batches[0].answered_by,
        Some(TurnRef {
            session: 0,
            ordinal: 2
        })
    );
    assert_eq!(answers(&s), [(1, None), (2, Some(batch))]);
}

#[test]
fn notes_link_only_a_turn_after_the_batch_with_its_marker_counts() {
    let (r, sessions) = rig_with(false);
    let batch = sent_batch(&r);
    let other = BatchId(batch.0 + 77);
    // Before the batch was sent (beyond the clock skew); another batch's
    // marker; a turn that does not mention one.
    append(&sessions, &r.d, answer("early", NOW.0 - 5000, batch));
    append(&sessions, &r.d, answer("other", NOW.0 + 1000, other));
    append(
        &sessions,
        &r.d,
        json!({ "id": "plain", "at": NOW.0 + 2000, "prompt": "thanks", "items": [] }),
    );
    let s = r.snapshot();
    assert_eq!(s.notes_state.batches[0].answered_by, None);
    assert!(answers(&s).iter().all(|(_, a)| a.is_none()));

    // A second earlier than the batch is still its answer (the clocks of
    // the agent and of Lectern differ); two are not.
    append(&sessions, &r.d, answer("skew", NOW.0 - 900, batch));
    let s = r.snapshot();
    assert_eq!(
        s.notes_state.batches[0].answered_by.map(|t| t.ordinal),
        Some(5)
    );

    // Once found it stays.
    append(&sessions, &r.d, answer("sooner", NOW.0 - 950, batch));
    let s = r.snapshot();
    assert_eq!(
        s.notes_state.batches[0].answered_by.map(|t| t.ordinal),
        Some(5)
    );
}

#[test]
fn notes_link_the_earliest_turn_that_names_the_batch_answers_it() {
    let (r, sessions) = rig_with(false);
    let batch = sent_batch(&r);
    // Two turns arrive together: the one that started first answers.
    let turns = json!([
        answer("late", NOW.0 + 9000, batch),
        answer("sooner", NOW.0 + 1000, batch),
    ]);
    sessions.append("a", &turns.to_string());
    for event in sessions.take_events() {
        r.d.sessions_changed(&event);
    }
    r.d.run();
    let s = r.snapshot();
    assert_eq!(
        s.notes_state.batches[0].answered_by.map(|t| t.ordinal),
        Some(3)
    );
    assert_eq!(
        answers(&s).into_iter().filter(|(_, a)| a.is_some()).count(),
        1
    );
}

#[test]
fn notes_link_a_marker_in_another_session_does_not_answer_the_batch() {
    let (r, sessions) = rig_with(false);
    let batch = sent_batch(&r);
    // Session "b" (another session of the worktree) is asked the same.
    sessions.append("b", &answer("pasted", NOW.0 + 5000, batch).to_string());
    for event in sessions.take_events() {
        r.d.sessions_changed(&event);
    }
    r.d.run();
    let s = r.snapshot();
    assert_eq!(s.notes_state.batches[0].answered_by, None);
    assert!(answers(&s).iter().all(|(_, a)| a.is_none()));
}

#[test]
fn notes_link_is_found_again_after_a_restart() {
    let (r, sessions) = rig_with(false);
    let batch = sent_batch(&r);
    append(&sessions, &r.d, answer("reply", NOW.0 + 5000, batch));
    r.d.run();
    let Rig { fx, d, .. } = r;
    drop(d);
    let d = fx.driven(vec![sessions]);
    let (_, s) = open(&d, target("a"));
    assert_eq!(
        s.notes_state.batches[0].answered_by.map(|t| t.ordinal),
        Some(2)
    );
    assert_eq!(answers(&s), [(1, None), (2, Some(batch))]);
}

#[test]
fn notes_link_a_batch_copied_again_is_answered_by_the_same_marker() {
    let (r, sessions) = rig_with(false);
    let batch = sent_batch(&r);
    r.command(ReviewCommand::CopyBatch(batch));
    r.d.run();
    let copied = r.d.clipboard().writes()[1].clone();
    // The first line of what was copied, as an agent keeps it.
    let preview: String = copied.lines().next().unwrap().chars().take(120).collect();
    let turn = json!({ "id": "pasted", "at": NOW.0 + 60_000, "prompt": preview, "items": [] });
    append(&sessions, &r.d, turn);
    assert_eq!(
        r.snapshot().notes_state.batches[0]
            .answered_by
            .map(|t| t.ordinal),
        Some(2)
    );
}

#[test]
fn notes_link_is_not_looked_for_outside_sessions() {
    let (r, _) = rig_with(false);
    let sent = sent_batch(&r);
    let outside = r.d.open(r.d.outside());
    r.d.run();
    let s = r.d.snapshot(outside).unwrap();
    assert!(s.turns.is_none());
    assert!(s.notes.is_empty(), "notes belong to their review");
    let _ = sent;
}
