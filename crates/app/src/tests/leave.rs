//! `Quit` asks the leave guards before the quit order starts
//! (docs/design/app.md「退出顺序的变化」). The guards are a fake that answers
//! in the way each test wants.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use gpui::TestAppContext;

use crate::quit::{LeaveAsk, ask_then, is_quitting};

type Done = Box<dyn FnOnce(bool) + Send>;

struct Answer(Box<dyn Fn(Done) + Send + Sync>);

impl LeaveAsk for Answer {
    fn ask(&self, done: Box<dyn FnOnce(bool) + Send>) {
        (self.0)(done);
    }
}

/// Runs the question and returns whether the quit went on.
fn asked(cx: &mut TestAppContext, answer: Answer) -> bool {
    let proceeded = Rc::new(Cell::new(false));
    let seen = proceeded.clone();
    cx.update(|cx| ask_then(cx, Arc::new(answer), move |_| seen.set(true)));
    cx.run_until_parked();
    proceeded.get()
}

#[gpui::test]
fn every_guard_allowing_goes_on(cx: &mut TestAppContext) {
    assert!(asked(cx, Answer(Box::new(|done| done(true)))));
    // The marker stays: the quit order runs and a second ⌘Q is ignored.
    assert!(cx.update(|cx| is_quitting(cx)));
}

#[gpui::test]
fn a_refusal_ends_the_quit_and_the_next_one_asks_again(cx: &mut TestAppContext) {
    assert!(!asked(cx, Answer(Box::new(|done| done(false)))));
    assert!(!cx.update(|cx| is_quitting(cx)), "⌘Q works again");
    assert!(asked(cx, Answer(Box::new(|done| done(true)))));
}

#[gpui::test]
fn an_answer_from_another_thread_is_waited_for(cx: &mut TestAppContext) {
    let answer = Answer(Box::new(|done| {
        std::thread::spawn(move || done(true)).join().unwrap();
    }));
    assert!(asked(cx, answer));
}

#[gpui::test]
fn a_dropped_answer_allows(cx: &mut TestAppContext) {
    assert!(asked(cx, Answer(Box::new(drop))));
}

#[gpui::test]
fn the_quit_waits_while_a_guard_has_not_answered(cx: &mut TestAppContext) {
    // The guard keeps `done` (its confirmation is on screen).
    let held: Arc<std::sync::Mutex<Option<Done>>> = Arc::default();
    let keep = held.clone();
    let answer = Answer(Box::new(move |done| *keep.lock().unwrap() = Some(done)));
    assert!(!asked(cx, answer), "no answer yet");
    assert!(
        cx.update(|cx| is_quitting(cx)),
        "a second ⌘Q is ignored meanwhile"
    );
    // The user confirms.
    let done = held.lock().unwrap().take().unwrap();
    done(false);
    cx.run_until_parked();
    assert!(!cx.update(|cx| is_quitting(cx)));
}
