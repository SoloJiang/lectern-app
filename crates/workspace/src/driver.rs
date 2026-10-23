//! The main-thread loop (docs/design/workspace.md「数据流与线程」) without
//! the window: takes the queue's items, applies registrations, runs the
//! state machine and asks the leave guards; what needs a window comes out
//! as [`Effect`]s. The root view (step 2) owns one and feeds it from the
//! queue's receiving end; the tests feed it directly.

use std::path::Path;
use std::sync::Arc;

use lectern_kernel::{ServiceRef, contain};
use lectern_logging::{debug, error};
use lectern_services::project::{OpenRequest, ProjectKey, Projects, WorktreeKey};
use lectern_services::workspace::{
    LeaveScope, ScreenLayout, WindowId, WorkspaceCommand, WorkspaceEvent,
};

use crate::effect::Effect;
use crate::hub::{Item, Queued, Sender};
use crate::nav::{AskId, Env, GuardId, Navigator};
use crate::registry::{Applied, Kind, Registry, Removed, Serial};

/// The main-thread state of one window.
pub struct Driver {
    nav: Navigator,
    registry: Arc<Registry>,
    sender: Sender,
    projects: Option<ServiceRef<dyn Projects>>,
    /// Items to try again on the next `Tick`: applies whose token is still
    /// pending, commands waiting for `dyn Projects`.
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

/// Whether the command calls `dyn Projects`, so it waits for the service.
fn needs_projects(command: &WorkspaceCommand) -> bool {
    matches!(
        command,
        WorkspaceCommand::OpenProject(_)
            | WorkspaceCommand::CloseProject
            | WorkspaceCommand::SetActiveWorktree(_)
    )
}

impl Driver {
    pub fn new(window: WindowId, registry: Arc<Registry>, sender: Sender) -> Driver {
        Driver {
            nav: Navigator::new(window),
            registry,
            sender,
            projects: None,
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
                        let env = Surroundings {
                            registry: &self.registry,
                            projects: self.projects.as_ref(),
                        };
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
                let env = Surroundings {
                    registry: &self.registry,
                    projects: self.projects.as_ref(),
                };
                self.nav.dispatch(command, &env, out);
            }
            Item::GuardAnswer { ask, allowed } => {
                let env = Surroundings {
                    registry: &self.registry,
                    projects: self.projects.as_ref(),
                };
                self.nav.guard_answered(ask, allowed, &env, out);
            }
            Item::Project(event) => {
                let env = Surroundings {
                    registry: &self.registry,
                    projects: self.projects.as_ref(),
                };
                self.nav.project_event(&event, &env, out);
            }
            Item::Milestone(milestone) => out.push(Effect::Milestone(milestone)),
            Item::Projects(projects) => {
                self.projects = Some(projects);
                self.retry(out);
            }
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
            let sender = self.sender.clone();
            let pending = entry.token.pending();
            let done: Box<dyn FnOnce(bool) + Send> = Box::new(move |allowed| {
                sender(Queued {
                    item: Item::GuardAnswer { ask, allowed },
                    guard: pending,
                });
            });
            let _scope = lectern_logging::scope(entry.ctx);
            let asked = contain(|| entry.guard.ask(scope, done));
            drop(admission);
            match asked {
                Ok(()) => Some(()),
                Err(_) => {
                    error!("leave guard panicked"; activation = entry.token.id());
                    None
                }
            }
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
