//! The one cell the two plugins share (docs/design/project_home.md「本 crate 的
//! 公开项」): `dyn Usage` may be absent, so the interest in the project's usage
//! is reconciled here, by whichever plugin arrives last.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use lectern_kernel::{ActivationToken, Admission, ServiceRef, TokenState};
use lectern_logging::warn;
use lectern_services::project::ProjectKey;
use lectern_services::usage::{ProjectRef, Usage, UsageInterest, UsageQuery, Window};

/// The callback the main plugin registers: marks the snapshot dirty and
/// schedules a composition. Any thread, only flips flags.
type OnChange = Arc<dyn Fn() + Send + Sync>;

/// Shared by "project-home" and "project-home-usage". `app` builds one and
/// hands a clone to each.
///
/// The two plugins activate in any order and `Opened` may come before
/// "project-home-usage" has started, so nothing depends on the order: the
/// interest is taken when both a `dyn Usage` and a project are there and
/// dropped when either goes, by [`reconcile`](UsageSlot::reconcile).
#[derive(Clone, Default)]
pub struct UsageSlot {
    inner: Arc<Mutex<Inner>>,
}

#[derive(Default)]
struct Inner {
    /// The service and the token of "project-home-usage"'s activation.
    usage: Option<(ServiceRef<dyn Usage>, ActivationToken)>,
    project: Option<ProjectKey>,
    /// The interest and the project it was taken for.
    interest: Option<(ProjectKey, UsageInterest)>,
    on_change: Option<OnChange>,
    /// `warn` "usage call refused" is written once per `set_usage`.
    refused_logged: bool,
}

impl UsageSlot {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// "project-home-usage" `started`: only records. The token is still
    /// `Pending` there, so `enter()` would not admit; the interest is taken by
    /// a task that runs after the activation is `Live` (see
    /// [`reconcile`](UsageSlot::reconcile)).
    pub fn set_usage(&self, usage: ServiceRef<dyn Usage>, token: ActivationToken) {
        let mut inner = self.lock();
        inner.usage = Some((usage, token));
        inner.refused_logged = false;
    }

    /// "project-home-usage" is deactivated: drops the interest and has the
    /// main plugin recompose once, so that the Usage row goes away.
    pub fn clear_usage(&self) {
        self.lock().usage = None;
        self.reconcile();
    }

    /// The main plugin: a project became the base (`Some`) or closed (`None`).
    pub fn set_project(&self, project: Option<ProjectKey>) {
        self.lock().project = project;
        self.reconcile();
    }

    /// The main plugin's `started`.
    pub fn set_on_change(&self, on_change: OnChange) {
        self.lock().on_change = Some(on_change);
    }

    /// The listener of `UsageEvent`: only calls `on_change`.
    pub fn changed(&self) {
        let on_change = self.lock().on_change.clone();
        if let Some(on_change) = on_change {
            on_change();
        }
    }

    /// Brings the interest in line with what is there: both a `dyn Usage` and
    /// a project, and no interest yet (or one for another project) → takes
    /// `Usage::watch(Project { Open(project), D7 })` inside the token's
    /// admission; either missing (or the token no longer admits) → drops it.
    /// Then calls `on_change`. Any thread, never blocks: `watch` and the drop
    /// of an old interest both return at once, and both happen outside the
    /// lock.
    ///
    /// Called by [`set_project`](UsageSlot::set_project) and
    /// [`clear_usage`](UsageSlot::clear_usage), and by the task
    /// "project-home-usage" submits in `started`, which runs after the token is
    /// `Live`.
    pub fn reconcile(&self) {
        let (stale, wanted, on_change) = {
            let mut inner = self.lock();
            let current = matches!(
                (&inner.interest, &inner.project),
                (Some((taken_for, _)), Some(project)) if taken_for == project
            );
            let wanted = inner.usage.as_ref().zip(inner.project.as_ref());
            let keep = current && wanted.is_some();
            let wanted = wanted.map(|(usage, project)| (usage.clone(), project.clone()));
            let stale = if keep { None } else { inner.interest.take() };
            let wanted = if keep { None } else { wanted };
            (stale, wanted, inner.on_change.clone())
        };
        drop(stale);
        if let Some(((service, token), project)) = wanted {
            self.watch(&service, &token, project);
        }
        if let Some(on_change) = on_change {
            on_change();
        }
    }

    fn watch(&self, service: &ServiceRef<dyn Usage>, token: &ActivationToken, project: ProjectKey) {
        let Some(_admission) = token.enter() else {
            self.log_refused(token);
            return;
        };
        let Some(usage) = service.get() else {
            return;
        };
        let interest = usage.watch(UsageQuery::Project {
            project: ProjectRef::Open(project.clone()),
            window: Window::D7,
        });
        drop(usage);
        // Another thread may have reconciled meanwhile: keep the first
        // interest, and one for a project or a service that is gone.
        let surplus = {
            let mut inner = self.lock();
            let still_wanted = inner.interest.is_none()
                && inner.project.as_ref() == Some(&project)
                && inner
                    .usage
                    .as_ref()
                    .is_some_and(|(_, current)| current.id() == token.id());
            if still_wanted {
                inner.interest = Some((project, interest));
                None
            } else {
                Some(interest)
            }
        };
        drop(surplus);
    }

    /// The composing task: the service and the token's admission, or None
    /// when there is no `dyn Usage` or its activation is dead.
    pub fn usage(&self) -> Option<(ServiceRef<dyn Usage>, Admission)> {
        let (service, token) = self.lock().usage.clone()?;
        match token.enter() {
            Some(admission) => Some((service, admission)),
            None => {
                self.log_refused(&token);
                None
            }
        }
    }

    /// A token that is still `Pending` is not a refusal: the task that
    /// follows `started` reconciles once it is `Live`.
    fn log_refused(&self, token: &ActivationToken) {
        if token.state() != TokenState::Dead {
            return;
        }
        let first = !std::mem::replace(&mut self.lock().refused_logged, true);
        if first {
            warn!("usage call refused"; reason = "token-dead");
        }
    }
}
