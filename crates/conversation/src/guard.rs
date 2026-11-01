//! The leave guard of the terminals (docs/design/conversation.md「数据结构」的
//! 「离开守卫」): closing a project or quitting Lectern with a terminal still
//! running asks first. `LeaveGuard::ask` runs on the main thread but gets
//! neither a window nor the app, so the question is sent to a foreground
//! task that the page started in its window ([`serve`]); that task shows the
//! system's confirmation and answers `done`. The count and the scope are
//! pure functions of the terminal list.

use std::sync::Arc;

use futures::StreamExt;
use futures::channel::mpsc;
use gpui::{AsyncWindowContext, PromptLevel};
use lectern_i18n::{Args, keys};
use lectern_kernel::ServiceRef;
use lectern_logging::{info, warn};
use lectern_services::terminal::{RunState, TerminalList, Terminals};
use lectern_services::workspace::{LeaveGuard, LeaveScope};
use lectern_ui::strings;

use crate::diag;
use crate::plugin::Shared;

/// A question for the foreground task: how many terminals run, in which
/// scope, and where the answer goes.
pub(crate) struct LeaveAsk {
    pub scope: &'static str,
    pub running: usize,
    pub done: Box<dyn FnOnce(bool) + Send>,
}

pub(crate) type AskSender = mpsc::UnboundedSender<LeaveAsk>;
pub(crate) type AskReceiver = mpsc::UnboundedReceiver<LeaveAsk>;

/// The name of a scope that terminals care about, for the log; None for the
/// scopes that do not end a terminal (a view, a window, a worktree).
fn scope_name(scope: &LeaveScope) -> Option<&'static str> {
    match scope {
        LeaveScope::Project(_) => Some("project"),
        LeaveScope::App => Some("app"),
        LeaveScope::View | LeaveScope::Window | LeaveScope::Worktree(_) => None,
    }
}

/// How many terminals that start or run would end with `scope`: those of the
/// project for `Project`, all for `App`, none for the other scopes (a
/// terminal outlives a view, a window and a worktree switch).
pub(crate) fn running_in(list: &TerminalList, scope: &LeaveScope) -> usize {
    let running = |state: &RunState| matches!(state, RunState::Starting | RunState::Running { .. });
    match scope {
        LeaveScope::Project(key) => list
            .terminals
            .iter()
            .filter(|info| info.project == *key && running(&info.state))
            .count(),
        LeaveScope::App => list
            .terminals
            .iter()
            .filter(|info| running(&info.state))
            .count(),
        LeaveScope::View | LeaveScope::Window | LeaveScope::Worktree(_) => 0,
    }
}

/// The guard "conversation-terminal" registers.
pub(crate) struct TerminalGuard {
    terminals: ServiceRef<dyn Terminals>,
    shared: Arc<Shared>,
}

impl TerminalGuard {
    pub(crate) fn new(terminals: ServiceRef<dyn Terminals>, shared: Arc<Shared>) -> TerminalGuard {
        TerminalGuard { terminals, shared }
    }
}

impl LeaveGuard for TerminalGuard {
    /// Main thread; does not block. Nothing runs (or the provider is gone, or
    /// the scope does not end terminals): allows at once. Otherwise the
    /// question goes to the page's task; without one (no page was ever built
    /// in a window) it is allowed and a warning says so.
    fn ask(&self, scope: LeaveScope, done: Box<dyn FnOnce(bool) + Send>) {
        let running = match self.terminals.get() {
            Some(terminals) => running_in(&terminals.list(), &scope),
            None => 0,
        };
        let Some(name) = scope_name(&scope).filter(|_| running > 0) else {
            done(true);
            return;
        };
        let ask = LeaveAsk {
            scope: name,
            running,
            done,
        };
        if let Err(ask) = self.shared.ask_leave(ask) {
            warn!(
                "terminal leave not asked";
                scope = ask.scope,
                running = diag::count(ask.running),
                reason = "no_window"
            );
            (ask.done)(true);
        }
    }
}

/// The foreground task of one window: shows the confirmation for each
/// question and answers it. Ends when the window is gone or the sender was
/// replaced (the page was built again) or dropped (the plugin stopped).
pub(crate) async fn serve(mut asks: AskReceiver, cx: &mut AsyncWindowContext) {
    while let Some(LeaveAsk {
        scope,
        running,
        done,
    }) = asks.next().await
    {
        let prompt = cx.update(|window, cx| {
            let strings = strings(cx);
            let title = strings.format(
                keys::conversation::LEAVE_TITLE,
                &Args::new().num("count", running as f64),
            );
            let detail = strings.get(keys::conversation::LEAVE_DETAIL).to_owned();
            let end = strings.get(keys::conversation::LEAVE_END).to_owned();
            let cancel = strings.get(keys::conversation::LEAVE_CANCEL).to_owned();
            window.prompt(
                PromptLevel::Warning,
                &title,
                Some(&detail),
                &[end.as_str(), cancel.as_str()],
                cx,
            )
        });
        let Ok(prompt) = prompt else {
            // The window is gone: nothing can be asked any more.
            warn!(
                "terminal leave not asked";
                scope = scope,
                running = diag::count(running),
                reason = "window_closed"
            );
            done(true);
            break;
        };
        let answer = prompt.await;
        // A prompt that was cancelled without an answer (the window closed
        // under it) lets the leave go on; only the second button refuses.
        let end = !matches!(answer, Ok(1));
        info!(
            "terminal leave answered";
            scope = scope,
            running = diag::count(running),
            answer = if end { "end" } else { "cancel" }
        );
        done(end);
    }
}
