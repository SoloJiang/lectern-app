//! The one cell the two plugins share (docs/design/worktrees.md「本 crate 的公开
//! 项」): `dyn Usage` may be absent, so the interests in the usage of the
//! worktrees are reconciled here, by whichever plugin arrives last.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use lectern_kernel::{ActivationToken, Admission, ServiceRef, TokenState};
use lectern_logging::warn;
use lectern_services::usage::{Usage, UsageInterest, UsageQuery};

/// The callback the main plugin registers: marks the snapshot dirty and
/// schedules a composition. Any thread, only flips flags.
type OnChange = Arc<dyn Fn() + Send + Sync>;

/// Shared by "worktrees" and "worktrees-usage". `app` builds one and hands a
/// clone to each.
///
/// The two plugins activate in any order and the screen may be shown before
/// "worktrees-usage" has started, so nothing depends on the order: the
/// interests are taken for the queries the main plugin wants while a
/// `dyn Usage` is there, and dropped when either goes, by
/// [`reconcile`](UsageLink::reconcile).
#[derive(Clone, Default)]
pub struct UsageLink {
    inner: Arc<Mutex<Inner>>,
}

#[derive(Default)]
struct Inner {
    /// The service and the token of "worktrees-usage"'s activation.
    usage: Option<(ServiceRef<dyn Usage>, ActivationToken)>,
    /// The queries of the latest snapshot; empty while the screen is not the
    /// current one.
    wanted: Arc<[UsageQuery]>,
    /// The interests taken, each for one query of `wanted`.
    interests: Vec<(UsageQuery, UsageInterest)>,
    /// Whether the main plugin was told that a `dyn Usage` is there.
    reported_present: bool,
    /// How many times `on_change` was called: the snapshot's source
    /// generation.
    generation: u64,
    on_change: Option<OnChange>,
    /// `warn` "usage call refused" is written once per `set_usage`.
    refused_logged: bool,
}

impl UsageLink {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// "worktrees-usage" `started`: only records. The token is still
    /// `Pending` there, so `enter()` would not admit; the interests are taken
    /// by a task that runs after the activation is `Live` (see
    /// [`reconcile`](UsageLink::reconcile)).
    pub fn set_usage(&self, usage: ServiceRef<dyn Usage>, token: ActivationToken) {
        let mut inner = self.lock();
        inner.usage = Some((usage, token));
        inner.refused_logged = false;
    }

    /// "worktrees-usage" is deactivated: drops the interests and has the main
    /// plugin compose once more, so that the tokens go away.
    pub fn clear_usage(&self) {
        self.lock().usage = None;
        self.reconcile();
    }

    /// The main plugin, after each composition: the queries this snapshot
    /// needs; the empty set while the screen is not the current one.
    pub fn set_wanted(&self, queries: Arc<[UsageQuery]>) {
        self.lock().wanted = queries;
        self.reconcile();
    }

    /// The main plugin's `started`.
    pub fn set_on_change(&self, on_change: OnChange) {
        self.lock().on_change = Some(on_change);
    }

    /// The listener of `UsageEvent::Changed`: calls `on_change` when `query`
    /// is one of the wanted.
    pub fn changed(&self, query: &UsageQuery) {
        let on_change = {
            let mut inner = self.lock();
            if !inner.wanted.contains(query) {
                return;
            }
            inner.generation += 1;
            inner.on_change.clone()
        };
        if let Some(on_change) = on_change {
            on_change();
        }
    }

    /// How many times the snapshot may have changed through this link
    /// (`SourceGenerations::usage`).
    pub fn generation(&self) -> u64 {
        self.lock().generation
    }

    /// The number of interests held (the log).
    pub fn watching(&self) -> usize {
        self.lock().interests.len()
    }

    /// Brings the interests in line with what is wanted: a `dyn Usage` is
    /// there → each wanted query without an interest is taken with
    /// `Usage::watch`, inside the token's admission; an interest that is not
    /// wanted any more, or all of them when the `dyn Usage` is gone (or the
    /// token no longer admits), are dropped. `on_change` is called only when
    /// the set really changed: an interest was taken or dropped, or the
    /// `dyn Usage` appeared or vanished. So "compose → `set_wanted` →
    /// compose" does not loop.
    ///
    /// Any thread, never blocks: `watch` and the drop of an old interest both
    /// return at once, and both happen outside the lock.
    ///
    /// Called by [`set_wanted`](UsageLink::set_wanted) and
    /// [`clear_usage`](UsageLink::clear_usage), and by the task
    /// "worktrees-usage" submits in `started`, which runs after the token is
    /// `Live`.
    pub fn reconcile(&self) {
        let (stale, missing, usage, present) = {
            let mut inner = self.lock();
            let present = inner.usage.is_some();
            let wanted = inner.wanted.clone();
            let (keep, stale): (Vec<_>, Vec<_>) = std::mem::take(&mut inner.interests)
                .into_iter()
                .partition(|(query, _)| present && wanted.contains(query));
            inner.interests = keep;
            let missing: Vec<UsageQuery> = if present {
                wanted
                    .iter()
                    .filter(|query| !inner.interests.iter().any(|(held, _)| held == *query))
                    .cloned()
                    .collect()
            } else {
                Vec::new()
            };
            (stale, missing, inner.usage.clone(), present)
        };
        let mut changed = !stale.is_empty();
        drop(stale);
        if let Some((service, token)) = usage.filter(|_| !missing.is_empty()) {
            changed |= self.watch(&service, &token, missing);
        }
        let on_change = {
            let mut inner = self.lock();
            changed |= std::mem::replace(&mut inner.reported_present, present) != present;
            if changed {
                inner.generation += 1;
            }
            changed.then(|| inner.on_change.clone()).flatten()
        };
        if let Some(on_change) = on_change {
            on_change();
        }
    }

    /// Takes the interests; whether any was kept.
    fn watch(
        &self,
        service: &ServiceRef<dyn Usage>,
        token: &ActivationToken,
        queries: Vec<UsageQuery>,
    ) -> bool {
        let Some(_admission) = token.enter() else {
            self.log_refused(token);
            return false;
        };
        let Some(usage) = service.get() else {
            return false;
        };
        let taken: Vec<(UsageQuery, UsageInterest)> = queries
            .into_iter()
            .map(|query| {
                let interest = usage.watch(query.clone());
                (query, interest)
            })
            .collect();
        drop(usage);
        // Another thread may have reconciled meanwhile: keep the first
        // interest of a query, and none for a query that is not wanted any
        // more or a service that is gone.
        let mut kept = false;
        let mut surplus = Vec::new();
        {
            let mut inner = self.lock();
            let current = inner
                .usage
                .as_ref()
                .is_some_and(|(_, current)| current.id() == token.id());
            for (query, interest) in taken {
                let still_wanted = current
                    && inner.wanted.contains(&query)
                    && !inner.interests.iter().any(|(held, _)| *held == query);
                if still_wanted {
                    inner.interests.push((query, interest));
                    kept = true;
                } else {
                    surplus.push(interest);
                }
            }
        }
        drop(surplus);
        kept
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
