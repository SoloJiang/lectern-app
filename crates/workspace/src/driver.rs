//! The main-thread loop (docs/design/workspace.md「数据流与线程」) without
//! the window: takes the queue's items, applies registrations, runs the
//! state machine, asks the leave guards and calls the command handlers;
//! what needs a window comes out as [`Effect`]s. The root view owns one and
//! feeds it from the queue's receiving end; the tests feed it directly.

use std::path::Path;
use std::sync::Arc;

use lectern_kernel::{ActivationToken, KernelControl, Name, PluginError, ServiceRef, contain};
use lectern_logging::{debug, error, warn};
use lectern_services::project::{OpenRequest, ProjectKey, Projects, WorktreeKey};
use lectern_services::workspace::{
    CommandCall, LeaveScope, ScreenLayout, WindowId, WorkspaceCommand, WorkspaceEvent,
};

use crate::diag::{id_text, plugin_error_kind};
use crate::effect::{Effect, Notice};
use crate::hub::{Item, Queued, Sender};
use crate::nav::{AskId, Env, GuardId, Navigator};
use crate::registry::{Applied, Kind, Registry, Removed, Serial};

/// The main-thread state of one window.
pub struct Driver {
    nav: Navigator,
    registry: Arc<Registry>,
    sender: Sender,
    projects: Option<ServiceRef<dyn Projects>>,
    /// From `Started`: activates the plugins of proxied commands.
    control: Option<KernelControl>,
    /// The workspace's own token: guards the replies of
    /// `KernelControl::command` in the queue.
    own: Option<ActivationToken>,
    /// Items to try again on the next `Tick`: applies whose token is still
    /// pending, commands waiting for `dyn Projects`, replays waiting for
    /// their registrations.
    deferred: Vec<Queued>,
}

/// The [`Env`] of the state machine over the registry and `dyn Projects`.
struct Surroundings<'a> {
    registry: &'a Registry,
    projects: Option<&'a ServiceRef<dyn Projects>>,
}

impl Env for Surroundings<'_> {
    fn screen_layout(&self, id: &str) -> Option<ScreenLayout> {
        self.registry.screen_layout(id)
    }

    fn has_sidebar_panel(&self, id: &str) -> bool {
        self.registry.has(Kind::SidebarPanel, id)
    }

    fn has_panel(&self, id: &str) -> bool {
        self.registry.has(Kind::Panel, id)
    }

    fn has_overlay(&self, id: &str) -> bool {
        self.registry.has(Kind::Overlay, id)
    }

    fn guards(&self) -> Vec<GuardId> {
        self.registry
            .guards()
            .into_iter()
            .map(|serial| GuardId(serial.0))
            .collect()
    }

    fn open_project(&self, path: &Path) -> Option<OpenRequest> {
        let projects = self.projects?.get()?;
        Some(projects.open(path))
    }

    fn close_project(&self, project: &ProjectKey) {
        if let Some(projects) = self.projects.and_then(ServiceRef::get) {
            projects.close(project);
        }
    }

    fn set_active_worktree(&self, project: &ProjectKey, worktree: &WorktreeKey) {
        if let Some(projects) = self.projects.and_then(ServiceRef::get) {
            projects.set_active_worktree(project, worktree);
        }
    }
}

/// The state machine's environment over the driver's fields (borrowed apart
/// from `nav`).
fn env<'a>(
    registry: &'a Registry,
    projects: Option<&'a ServiceRef<dyn Projects>>,
) -> Surroundings<'a> {
    Surroundings { registry, projects }
}

/// Whether the command calls `dyn Projects`, so it waits for the service.
fn needs_projects(command: &WorkspaceCommand) -> bool {
    matches!(
        command,
        WorkspaceCommand::OpenProject(_)
            | WorkspaceCommand::CloseProject
            | WorkspaceCommand::SetActiveWorktree(_)
    )
}

/// What `call_handler` found.
enum Handled {
    /// The handler ran (or panicked and failed its activation).
    Done,
    /// No live handler for the command.
    Missing,
}

impl Driver {
    pub fn new(window: WindowId, registry: Arc<Registry>, sender: Sender) -> Driver {
        Driver {
            nav: Navigator::new(window),
            registry,
            sender,
            projects: None,
            control: None,
            own: None,
            deferred: Vec::new(),
        }
    }

    pub fn nav(&self) -> &Navigator {
        &self.nav
    }

    pub fn nav_mut(&mut self) -> &mut Navigator {
        &mut self.nav
    }

    pub fn registry(&self) -> &Arc<Registry> {
        &self.registry
    }

    /// How many items wait for the next `Tick`.
    pub fn deferred(&self) -> usize {
        self.deferred.len()
    }

    /// Handles one item; the effects go to `out` in order. The item's
    /// pending guard drops when the item is handled, or stays with it while
    /// it is deferred.
    pub fn handle(&mut self, queued: Queued, out: &mut Vec<Effect>) {
        let Queued { item, guard } = queued;
        match item {
            Item::Apply(serial) => match self.registry.apply(serial) {
                Applied::Applied { kind, .. } => {
                    if kind == Kind::KeyBindings {
                        out.push(Effect::BindKeys(serial));
                    }
                    out.push(Effect::Event(WorkspaceEvent::ContributionsChanged));
                }
                Applied::Deferred => self.deferred.push(Queued {
                    item: Item::Apply(serial),
                    guard,
                }),
                Applied::Dropped => {}
            },
            Item::Remove(serial) => match self.registry.remove(serial) {
                Removed::Removed { kind, id } => {
                    if kind == Kind::Screen {
                        let env = env(&self.registry, self.projects.as_ref());
                        self.nav.screen_removed(&id, &env, out);
                    }
                    out.push(Effect::Event(WorkspaceEvent::ContributionsChanged));
                }
                Removed::Nothing => {}
            },
            Item::Command(command) => {
                if needs_projects(&command) && self.projects.is_none() {
                    self.deferred.push(Queued {
                        item: Item::Command(command),
                        guard,
                    });
                    return;
                }
                let env = env(&self.registry, self.projects.as_ref());
                self.nav.dispatch(command, &env, out);
            }
            Item::GuardAnswer { ask, allowed } => {
                let env = env(&self.registry, self.projects.as_ref());
                self.nav.guard_answered(ask, allowed, &env, out);
            }
            Item::Project(event) => {
                let env = env(&self.registry, self.projects.as_ref());
                self.nav.project_event(&event, &env, out);
            }
            Item::Milestone(milestone) => out.push(Effect::Milestone(milestone)),
            Item::Projects(projects) => {
                self.projects = Some(projects);
                self.retry(out);
            }
            Item::Started { control, token } => {
                self.control = Some(control);
                self.own = Some(token);
            }
            Item::Proxy(name) => self.proxy(name, out),
            Item::Replay { name, token } => {
                if !self.replay(&name, &token, out) {
                    self.deferred.push(Queued {
                        item: Item::Replay { name, token },
                        guard,
                    });
                }
            }
            Item::CommandFailed { name, error } => {
                error!("command activation failed"; command = id_text(&name), error = error);
                out.push(Effect::Notice(Notice::CommandFailed));
            }
            Item::AskLeave { scope, done } => {
                let env = env(&self.registry, self.projects.as_ref());
                self.nav.ask_leave(scope, done, &env, out);
            }
            Item::Restore(state) => {
                self.nav.restore_inspector(state.inspector);
                if let Some(path) = state.project {
                    self.handle(
                        Queued {
                            item: Item::Command(WorkspaceCommand::OpenProject(path)),
                            guard,
                        },
                        out,
                    );
                    return;
                }
            }
            // The root view takes these before the driver sees them.
            Item::RecordFrames { .. } | Item::StopRecording => {}
            Item::Tick => self.retry(out),
        }
        self.resolve_asks(out);
    }

    /// Tries the deferred items once more.
    fn retry(&mut self, out: &mut Vec<Effect>) {
        let deferred = std::mem::take(&mut self.deferred);
        for queued in deferred {
            self.handle(queued, out);
        }
    }

    /// A proxied command's action fired (kernel design「按命令激活」step 2):
    /// a live handler runs now; otherwise the kernel activates the plugin
    /// declaring the command and the reply comes back through the queue.
    fn proxy(&mut self, name: Name, out: &mut Vec<Effect>) {
        if let Handled::Done = self.call_handler(&name) {
            return;
        }
        let Some(control) = self.control.clone() else {
            warn!("command without handler"; command = id_text(&name));
            out.push(Effect::Notice(Notice::CommandFailed));
            return;
        };
        let sender = self.sender.clone();
        let own = self.own.clone();
        let command = name.clone();
        control.command(&name, move |result| {
            // Kernel thread, inside the workspace's admission. The reply
            // carries the workspace's pending guard; after `shutdown` there
            // is none and the reply is dropped.
            let Some(guard) = own.as_ref().and_then(ActivationToken::pending) else {
                debug!("job dropped after shutdown"; jobs = 1u32);
                return;
            };
            let item = match result {
                Ok(token) => Item::Replay {
                    name: command,
                    token,
                },
                Err(error) => Item::CommandFailed {
                    name: command,
                    error: plugin_error_kind(&error),
                },
            };
            sender(Queued {
                item,
                guard: Some(guard),
            });
        });
    }

    /// Step 3 of「按命令激活」: once the activation's registrations have
    /// applied, the handler runs; a token dead by then, or still no handler,
    /// is a failure shown once. Returns false while the registrations are
    /// still on their way (the item is deferred).
    fn replay(&mut self, name: &Name, token: &ActivationToken, out: &mut Vec<Effect>) -> bool {
        if token.state() == lectern_kernel::TokenState::Dead {
            error!("command activation failed"; command = id_text(name), error = "dead");
            out.push(Effect::Notice(Notice::CommandFailed));
            return true;
        }
        if !self.registry.activation_applied(token.id()) {
            return false;
        }
        match self.call_handler(name) {
            Handled::Done => {}
            Handled::Missing => {
                warn!("command without handler"; command = id_text(name));
                out.push(Effect::Notice(Notice::CommandFailed));
            }
        }
        true
    }

    /// Calls the command's live handler in its activation's admission and
    /// log scope, under `contain`; a panic fails that activation.
    fn call_handler(&self, name: &Name) -> Handled {
        let Some(entry) = self.registry.handler(name) else {
            return Handled::Missing;
        };
        let Some(admission) = entry.token.enter() else {
            return Handled::Missing;
        };
        let call = CommandCall {
            window: self.nav.window(),
            name: name.clone(),
        };
        let _scope = lectern_logging::scope(entry.ctx);
        if let Err(message) = contain(|| (entry.handler)(&call)) {
            error!("command handler panicked"; command = id_text(name), activation = entry.token.id());
            entry.token.fail(PluginError::Panicked(message));
        }
        drop(admission);
        Handled::Done
    }

    /// Takes every `Ask` out of `out` and asks the guard, in its activation's
    /// admission and log scope, under `contain`. A guard that is gone, not
    /// live or panics counts as allowing.
    fn resolve_asks(&mut self, out: &mut Vec<Effect>) {
        let mut i = 0;
        while i < out.len() {
            if !matches!(out[i], Effect::Ask { .. }) {
                i += 1;
                continue;
            }
            let Effect::Ask { ask, guard, scope } = out.remove(i) else {
                unreachable!("checked above");
            };
            self.ask_guard(ask, guard, scope, out);
        }
    }

    fn ask_guard(&mut self, ask: AskId, guard: GuardId, scope: LeaveScope, out: &mut Vec<Effect>) {
        let asked = self.registry.guard(Serial(guard.0)).and_then(|entry| {
            let admission = entry.token.enter()?;
            let answer = Answer {
                sender: self.sender.clone(),
                ask,
                pending: entry.token.pending(),
                answered: false,
            };
            let done: Box<dyn FnOnce(bool) + Send> = Box::new(move |allowed| answer.send(allowed));
            let _scope = lectern_logging::scope(entry.ctx);
            let asked = contain(|| entry.guard.ask(scope, done));
            if let Err(message) = asked {
                // `done` answered already, or its drop during the unwind
                // answered for it. The activation fails while the admission
                // is still held (kernel design「撤销」).
                error!("leave guard panicked"; activation = entry.token.id());
                entry.token.fail(PluginError::Panicked(message));
            }
            drop(admission);
            Some(())
        });
        if asked.is_none() {
            debug!("leave guard skipped"; guard = guard.0);
            let env = Surroundings {
                registry: &self.registry,
                projects: self.projects.as_ref(),
            };
            self.nav.guard_answered(ask, true, &env, out);
        }
    }
}

/// A guard's `done`: sends the answer into the queue, once. A guard that
/// drops it without answering (its plugin deactivated while a dialog was
/// open, or a panic before `done`) counts as allowing, so the round ends
/// and the queued inputs run.
struct Answer {
    sender: Sender,
    ask: AskId,
    pending: Option<lectern_kernel::PendingGuard>,
    answered: bool,
}

impl Answer {
    fn send(mut self, allowed: bool) {
        self.answered = true;
        (self.sender)(Queued {
            item: Item::GuardAnswer {
                ask: self.ask,
                allowed,
            },
            guard: self.pending.take(),
        });
    }
}

impl Drop for Answer {
    fn drop(&mut self) {
        if !self.answered {
            debug!("leave guard dropped its answer"; ask = self.ask.0);
            (self.sender)(Queued {
                item: Item::GuardAnswer {
                    ask: self.ask,
                    allowed: true,
                },
                guard: self.pending.take(),
            });
        }
    }
}
