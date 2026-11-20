//! The leave guard (docs/design/file_reader.md「编辑」的「离开守卫」): every
//! way out of a buffer with unsaved changes asks the user first. The
//! guard that `workspace` and `app` ask ([`LeaveGuard`]) is only a sender: it
//! runs on the main thread but has no window, so the question goes to a
//! task the view starts when it enters edit mode, and the answer comes back
//! through `done`. The view's own ways out (Done, Esc, ⌘E, a request to open
//! another file, back and forward by name) take the same road, one question
//! at a time.
//!
//! Nothing here exists before the user enters edit mode: the guard holds an
//! `Arc<Shared>`, the sender is in `Shared` only while a view edits.

use std::sync::Arc;

use futures::StreamExt as _;
use gpui::{Context, PromptButton, PromptLevel, Task, Window};
use lectern_i18n::{Args, keys};
use lectern_logging::info;
use lectern_services::workspace::{LeaveGuard as LeaveGuardService, LeaveScope};
use lectern_syntax::SaveResult;
use lectern_ui::strings;

use super::Editing;
use crate::session::Request;
use crate::shared::Shared;
use crate::text::file_name;
use crate::view::ReadingView;

/// The answer of a question, given exactly once. A question that is dropped
/// without one (the editing view goes, the task ends) is answered "pass":
/// the buffer it was about is gone, and `workspace` waits for the answer
/// before it does anything else.
pub(crate) struct Answer(Option<Box<dyn FnOnce(bool) + Send>>);

impl Answer {
    fn new(done: Box<dyn FnOnce(bool) + Send>) -> Answer {
        Answer(Some(done))
    }

    fn send(mut self, pass: bool) {
        if let Some(done) = self.0.take() {
            done(pass);
        }
    }
}

impl Drop for Answer {
    fn drop(&mut self) {
        if let Some(done) = self.0.take() {
            done(true);
        }
    }
}

/// A question of the guard: leaving `scope`, answered through `answer`.
pub(crate) struct LeaveAsk {
    scope: LeaveScope,
    answer: Answer,
}

/// The guard "file-reader" registers, one per window (activation).
pub(crate) struct LeaveGuard {
    shared: Arc<Shared>,
}

impl LeaveGuard {
    pub(crate) fn new(shared: Arc<Shared>) -> LeaveGuard {
        LeaveGuard { shared }
    }
}

impl LeaveGuardService for LeaveGuard {
    /// Main thread; does not block. Nobody is editing: passes at once.
    /// Otherwise the question goes to the editing view's task.
    fn ask(&self, scope: LeaveScope, done: Box<dyn FnOnce(bool) + Send>) {
        let ask = LeaveAsk {
            scope,
            answer: Answer::new(done),
        };
        if let Err(ask) = self.shared.ask_leave(ask) {
            ask.answer.send(true);
        }
    }
}

/// What the view is asked to leave, and what happens after the answer.
pub(crate) enum Leave {
    /// Done, Esc, ⌘E: leave edit mode.
    Done,
    /// A request to show another file, taken from the session's queue.
    Open(Request),
    /// ⌘[ by name: the previous position.
    Back,
    /// ⌘] by name: the next position.
    Forward,
    /// `workspace` or `app` asks: its `done` waits.
    Guard { scope: LeaveScope, answer: Answer },
}

impl Leave {
    /// The scope's name, for the log.
    fn name(&self) -> &'static str {
        match self {
            Leave::Done => "done",
            Leave::Open(_) => "open",
            Leave::Back => "back",
            Leave::Forward => "forward",
            Leave::Guard { scope, .. } => match scope {
                LeaveScope::View => "view",
                LeaveScope::Project(_) => "project",
                LeaveScope::Worktree(_) => "worktree",
                LeaveScope::Window => "window",
                LeaveScope::App => "app",
            },
        }
    }
}

/// A question the user has been asked and has not answered, or whose Save is
/// on its way.
pub(crate) struct Leaving {
    leave: Leave,
    /// After Save: the number of saves that must have been answered before
    /// the verdict (`seen_saves` plus the ones asked).
    saving: Option<u64>,
}

impl Leaving {
    /// What waits for the result of a save.
    pub(crate) fn saving(&self) -> Option<u64> {
        self.saving
    }
}

/// How a question ended, for the log.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// Saved: the buffer is the file, the leave goes on.
    Saved,
    /// Don't Save: the buffer goes, the leave goes on.
    DontSave,
    /// Cancel: nothing leaves.
    Cancel,
    /// Save found the file changed on disk: nothing leaves, the row says so.
    Conflict,
    /// Save failed (or could not start): nothing leaves.
    Failed,
}

impl Outcome {
    fn name(self) -> &'static str {
        match self {
            Outcome::Saved => "saved",
            Outcome::DontSave => "dont_save",
            Outcome::Cancel => "cancel",
            Outcome::Conflict => "conflict",
            Outcome::Failed => "failed",
        }
    }

    /// The leave goes on.
    fn passes(self) -> bool {
        matches!(self, Outcome::Saved | Outcome::DontSave)
    }
}

/// The task of an editing view that takes the guard's questions, and the
/// slot in `Shared` that sends them there. Dropping it stops both.
pub(crate) struct Port {
    shared: Arc<Shared>,
    id: u64,
    _task: Task<()>,
}

impl Port {
    pub(crate) fn open(
        view: &ReadingView,
        window: &mut Window,
        cx: &mut Context<ReadingView>,
    ) -> Port {
        let mut asks = view.shared.serve_asks(view.id);
        let task = cx.spawn_in(window, async move |this, cx| {
            while let Some(ask) = asks.next().await {
                let LeaveAsk { scope, answer } = ask;
                let alive = this.update_in(cx, |view, window, cx| {
                    view.begin_leave(Leave::Guard { scope, answer }, window, cx);
                });
                if alive.is_err() {
                    break;
                }
            }
        });
        Port {
            shared: view.shared.clone(),
            id: view.id,
            _task: task,
        }
    }
}

impl Drop for Port {
    fn drop(&mut self) {
        self.shared.stop_asks(self.id);
    }
}

/// The user's choice in the confirmation.
#[derive(Clone, Copy)]
enum Choice {
    Save,
    DontSave,
    Cancel,
}

impl ReadingView {
    /// Whether a question is open (the requests to show a file wait for it).
    pub(crate) fn leaving(&self) -> bool {
        self.edit
            .as_ref()
            .is_some_and(|edit| edit.leaving.is_some())
    }

    /// Whether the leave touches the document being edited: a project or a
    /// worktree switch of another project does not.
    fn touches(&self, leave: &Leave) -> bool {
        match leave {
            Leave::Guard {
                scope: LeaveScope::Project(key) | LeaveScope::Worktree(key),
                ..
            } => self
                .snapshot
                .as_ref()
                .is_none_or(|snapshot| snapshot.target.project == *key),
            _ => true,
        }
    }

    /// Asks whether `leave` may go on. Nothing unsaved (or nothing of the
    /// buffer touched): it goes on at once. Otherwise the system confirmation
    /// asks; one question is open at a time, the questions of `workspace`
    /// that come meanwhile wait behind it.
    pub(crate) fn begin_leave(
        &mut self,
        leave: Leave,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(edit) = self.edit.as_ref() else {
            self.resolve_leave(leave, true, window, cx);
            return;
        };
        if edit.leaving.is_some() {
            match leave {
                // The answer comes after the open one's.
                Leave::Guard { .. } => {
                    if let Some(edit) = self.edit.as_mut() {
                        edit.backlog.push_back(leave);
                    }
                }
                // The user's own keys while the confirmation is up: nothing
                // (it is a sheet; only a probe gets here). A request is
                // never taken while one is open.
                Leave::Open(request) => self.shared.session.cancel(request),
                Leave::Done | Leave::Back | Leave::Forward => {}
            }
            return;
        }
        if !edit.dirty() || !self.touches(&leave) {
            self.resolve_leave(leave, true, window, cx);
            return;
        }
        if let Some(edit) = self.edit.as_mut() {
            edit.leaving = Some(Leaving {
                leave,
                saving: None,
            });
        }
        self.confirm(window, cx);
    }

    /// Shows the system's Save / Don't Save / Cancel.
    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self
            .snapshot
            .as_ref()
            .map(|snapshot| file_name(&snapshot.target.path).to_owned())
            .unwrap_or_default();
        let strings = strings(cx);
        let title = strings.format(
            keys::file_reader::UNSAVED_TITLE,
            &Args::new().str("name", &name),
        );
        let message = strings.get(keys::file_reader::UNSAVED_MESSAGE).to_owned();
        let buttons = [
            PromptButton::ok(strings.get(keys::file_reader::UNSAVED_SAVE).to_owned()),
            PromptButton::new(strings.get(keys::file_reader::UNSAVED_DONT_SAVE).to_owned()),
            PromptButton::cancel(strings.get(keys::file_reader::UNSAVED_CANCEL).to_owned()),
        ];
        let answer = window.prompt(PromptLevel::Warning, &title, Some(&message), &buttons, cx);
        cx.spawn_in(window, async move |this, cx| {
            // A prompt that ends without an answer (the window is closing)
            // is a Cancel: nothing is lost by it.
            let choice = match answer.await {
                Ok(0) => Choice::Save,
                Ok(1) => Choice::DontSave,
                _ => Choice::Cancel,
            };
            let _ = this.update_in(cx, |view, window, cx| view.chosen(choice, window, cx));
        })
        .detach();
    }

    fn chosen(&mut self, choice: Choice, window: &mut Window, cx: &mut Context<Self>) {
        let Some(edit) = self.edit.as_mut() else {
            return;
        };
        if edit
            .leaving
            .as_ref()
            .is_none_or(|leaving| leaving.saving.is_some())
        {
            return;
        }
        match choice {
            Choice::Save => {
                // The Save button's own save: Save Anyway after Keep Editing.
                let anyway = self.save_anyway();
                if self.save(anyway, cx) {
                    if let Some(edit) = self.edit.as_mut()
                        && let Some(leaving) = edit.leaving.as_mut()
                    {
                        leaving.saving = Some(edit.seen_saves + edit.asked.len() as u64);
                    }
                } else if self.edit.as_ref().is_some_and(|edit| edit.merging) {
                    // Merging: the buffer is being replaced and cannot be
                    // saved now.
                    self.verdict(Outcome::Failed, window, cx);
                } else {
                    // Nothing to save any more: the result of an earlier
                    // save came in while the confirmation was up.
                    self.verdict(Outcome::Saved, window, cx);
                }
            }
            Choice::DontSave => self.verdict(Outcome::DontSave, window, cx),
            Choice::Cancel => self.verdict(Outcome::Cancel, window, cx),
        }
    }

    /// The result of the save the user chose: called after every wake-up of
    /// the session. Passes only on `Saved` with nothing typed since.
    pub(crate) fn save_result(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(edit) = self.edit.as_ref() else {
            return;
        };
        let Some(wait) = edit.leaving.as_ref().and_then(Leaving::saving) else {
            return;
        };
        if edit.seen_saves < wait {
            return;
        }
        let outcome = match edit.session.state().save {
            Some(SaveResult::Saved) if !edit.dirty() => Outcome::Saved,
            Some(SaveResult::Conflict) => Outcome::Conflict,
            _ => Outcome::Failed,
        };
        self.verdict(outcome, window, cx);
    }

    /// The question is answered: logs it, and lets the leave go on or not.
    fn verdict(&mut self, outcome: Outcome, window: &mut Window, cx: &mut Context<Self>) {
        let Some(edit) = self.edit.as_mut() else {
            return;
        };
        let Some(leaving) = edit.leaving.take() else {
            return;
        };
        info!(
            "a leave was asked about unsaved changes";
            scope = leaving.leave.name(),
            outcome = outcome.name(),
        );
        if outcome == Outcome::Conflict {
            // The row that says the file changed on disk shows again.
            edit.keep_editing = false;
        }
        if outcome == Outcome::DontSave {
            // The buffer goes with its session; the questions behind this
            // one are answered "pass" as the edit ends.
            self.drop_edit(window, cx);
        }
        self.resolve_leave(leaving.leave, outcome.passes(), window, cx);
        // What waited for the answer: the requests, the other questions.
        self.pump(window, cx);
        self.next_question(outcome.passes(), window, cx);
    }

    /// The questions that came while one was open: after a pass each is
    /// asked again (the buffer is saved or gone, usually they pass at once);
    /// after a refusal they are refused too, so that Cancel is asked once.
    fn next_question(&mut self, pass: bool, window: &mut Window, cx: &mut Context<Self>) {
        loop {
            let Some(edit) = self.edit.as_mut() else {
                return;
            };
            if edit.leaving.is_some() {
                return;
            }
            let Some(next) = edit.backlog.pop_front() else {
                return;
            };
            if pass {
                self.begin_leave(next, window, cx);
            } else if let Leave::Guard { answer, .. } = next {
                answer.send(false);
            }
        }
    }

    /// What follows the answer to `leave`.
    pub(crate) fn resolve_leave(
        &mut self,
        leave: Leave,
        pass: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match leave {
            Leave::Done => {
                if pass {
                    self.finish_edit(window, cx);
                }
            }
            Leave::Open(request) => {
                if pass {
                    self.drop_edit(window, cx);
                    self.show(request, cx);
                } else {
                    // The user stays: the request, and the ones behind it,
                    // are over.
                    self.shared.session.cancel(request);
                    while let Some(waiting) = self.shared.session.next_request() {
                        self.shared.session.cancel(waiting);
                    }
                }
            }
            Leave::Back => {
                if pass {
                    self.drop_edit(window, cx);
                    self.step_back(window, cx);
                }
            }
            Leave::Forward => {
                if pass {
                    self.drop_edit(window, cx);
                    self.step_forward(window, cx);
                }
            }
            Leave::Guard { answer, .. } => answer.send(pass),
        }
        cx.notify();
    }
}

impl Editing {
    /// The view goes with a question open: a request that waited for the
    /// answer is cancelled (`ReaderEvent::Open`), the guard's `done` is
    /// answered as the question drops.
    pub(crate) fn abandon(&mut self, view: &ReadingView) {
        if let Some(Leaving {
            leave: Leave::Open(request),
            ..
        }) = self.leaving.take()
        {
            view.shared.session.cancel(request);
        }
    }
}
