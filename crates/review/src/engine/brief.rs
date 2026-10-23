//! The brief in the engine (docs/design/review.md「简报规则」「简报规则的
//! 扩展点」): the registered `BriefRules` providers, one request to them
//! per round of a review, their replies, the 10-second timeout, and the
//! brief assembled from the work, the attribution so far and the replies.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use lectern_executor::Priority;
use lectern_kernel::{ActivationToken, TokenState};
use lectern_logging::{debug, info};
use lectern_services::changes::ChangeDetail;
use lectern_services::project::RelPath;
use lectern_services::review::{
    AnalysisState, Brief, BriefFile, BriefReply, BriefRequest, BriefRules, BriefRulesRegistration,
    DiffSide, LineAnchor, ProvidedItem, ReviewId, SeenState,
};
use lectern_services::session::ToolStatus;

use super::attribute::{self, FileResult};
use super::round::{FileWork, Work, side_oid};
use super::{Engine, Open, lock};
use crate::brief::{self, FileInput, MAX_PROVIDED, Provided};
use crate::claims::ClaimKind;

/// A provider that has not sent `done` by then counts as done.
pub(crate) const PROVIDER_TIMEOUT: Duration = Duration::from_secs(10);
/// A provider registered from its `started` has a `Pending` token, which
/// admits nothing until `started` returns: asked again after this long,
/// until it is `Live` or the request times out.
pub(crate) const PENDING_RETRY: Duration = Duration::from_millis(50);

/// The registered providers, in registration order.
#[derive(Default)]
pub(crate) struct Providers {
    next: u64,
    entries: Vec<Registered>,
}

struct Registered {
    seq: u64,
    id: Arc<str>,
    provider: Arc<dyn BriefRules>,
    token: ActivationToken,
}

/// A review's current request to the providers.
#[derive(Default)]
pub(crate) struct BriefState {
    /// The work the providers were asked about (by identity: each round
    /// makes a new one).
    asked: Option<Weak<Work>>,
    /// The request number; a reply of another is stale.
    request: u64,
    /// Set when the request is dropped: `BriefReply::cancelled`.
    cancel: Arc<AtomicBool>,
    replies: Vec<Reply>,
}

/// One registered provider, as a request takes it: registration, ID,
/// provider, token.
type Asked = (u64, Arc<str>, Arc<dyn BriefRules>, ActivationToken);

struct Reply {
    registration: u64,
    id: Arc<str>,
    items: Vec<ProvidedItem>,
    /// `evaluate` was called (or the provider was given up on).
    asked: bool,
    done: bool,
}

impl Providers {
    pub(super) fn clear(&mut self) {
        self.entries.clear();
    }
}

impl BriefState {
    /// Drops the request: late replies are ignored and the provider sees
    /// `cancelled`.
    pub(super) fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
    }
}

impl Engine {
    /// `Review::register_brief_rules`: the provider is asked about every
    /// open review's current round and every round from now on, until
    /// the registration is dropped or its token is dead.
    pub(crate) fn register_brief_rules(
        &self,
        provider: Arc<dyn BriefRules>,
        token: ActivationToken,
    ) -> BriefRulesRegistration {
        let id = provider.id();
        let seq = {
            let mut providers = lock(&self.providers);
            providers.next += 1;
            let seq = providers.next;
            providers.entries.push(Registered {
                seq,
                id,
                provider,
                token,
            });
            seq
        };
        // The reviews already open are asked again, this provider among
        // the others; their snapshots then show the request pending.
        let open_reviews: Vec<(ReviewId, u64, lectern_logging::Ctx)> = {
            let mut state = lock(&self.state);
            let mut ids = Vec::new();
            for (id, open) in state.reviews.iter_mut() {
                if open.brief.asked.is_some() {
                    open.brief.asked = None;
                    ids.push((*id, open.round, open.ctx));
                }
            }
            ids
        };
        for (id, round, ctx) in open_reviews {
            self.ask_providers(id);
            self.submit(Priority::Background, ctx, move |engine| {
                engine.republish(id, round)
            });
        }
        let me = self.me.clone();
        BriefRulesRegistration::new(move || {
            if let Some(engine) = me.upgrade() {
                engine.unregister(seq);
            }
        })
    }

    /// The registration `seq` is dropped: the provider is not called again
    /// and its items, delivered or in flight, go.
    fn unregister(&self, seq: u64) {
        lock(&self.providers).entries.retain(|r| r.seq != seq);
        let affected: Vec<(ReviewId, u64, lectern_logging::Ctx)> = {
            let mut state = lock(&self.state);
            let mut affected = Vec::new();
            for (id, open) in state.reviews.iter_mut() {
                let before = open.brief.replies.len();
                open.brief.replies.retain(|r| r.registration != seq);
                if open.brief.replies.len() != before {
                    affected.push((*id, open.round, open.ctx));
                }
            }
            affected
        };
        for (id, round, ctx) in affected {
            self.submit(Priority::Background, ctx, move |engine| {
                engine.republish(id, round)
            });
        }
    }

    /// Asks the providers about the review's current work, once per work.
    /// Called after a `Changed` event, with no lock held; the providers'
    /// `evaluate` runs here, outside the state lock.
    pub(super) fn ask_providers(&self, id: ReviewId) {
        let (request, cancel, ctx, work) = {
            let mut state = lock(&self.state);
            let Some(open) = state.reviews.get_mut(&id) else {
                return;
            };
            if open.snapshot.state != lectern_services::review::LoadState::Ready
                || open.snapshot.degraded.is_some()
            {
                return;
            }
            let Some(work) = open.work.clone() else {
                return;
            };
            if open
                .brief
                .asked
                .as_ref()
                .is_some_and(|asked| Weak::ptr_eq(asked, &Arc::downgrade(&work)))
            {
                return;
            }
            open.brief.cancel();
            open.brief.asked = Some(Arc::downgrade(&work));
            open.brief.request += 1;
            open.brief.cancel = Arc::new(AtomicBool::new(false));
            open.brief.replies.clear();
            (
                open.brief.request,
                open.brief.cancel.clone(),
                open.ctx,
                work,
            )
        };
        let providers: Vec<Asked> = lock(&self.providers)
            .entries
            .iter()
            .map(|r| (r.seq, r.id.clone(), r.provider.clone(), r.token.clone()))
            .collect();
        if providers.is_empty() {
            return;
        }
        let files = brief_files(&work);
        let request_data = BriefRequest {
            project: work.git.project.clone(),
            worktree: Some(work.git.key.clone()),
            files,
        };
        debug!("brief requested";
            id = id.0,
            request = request,
            providers = providers.len(),
            files = request_data.files.len());
        // Registered before any reply can arrive: a provider answering
        // inside `evaluate` finds its entry.
        {
            let mut state = lock(&self.state);
            if let Some(open) = state.reviews.get_mut(&id)
                && open.brief.request == request
            {
                open.brief.replies = providers
                    .iter()
                    .map(|(seq, pid, _, _)| Reply {
                        registration: *seq,
                        id: pid.clone(),
                        items: Vec::new(),
                        asked: false,
                        done: false,
                    })
                    .collect();
            }
        }
        self.evaluate_all(id, request, ctx, &cancel, &request_data, &providers);
        if let Some(host) = self.cfg.host.clone() {
            let me = self.me.clone();
            host.later(
                PROVIDER_TIMEOUT,
                Box::new(move || {
                    let Some(engine) = me.upgrade() else { return };
                    engine.submit(Priority::Background, ctx, move |engine| {
                        engine.providers_timed_out(id, request);
                    });
                }),
            );
        }
    }

    /// Calls `evaluate` on each provider whose token admits it. A dead
    /// token counts as done; a `Pending` one (the provider registered from
    /// its `started`) is asked again after `PENDING_RETRY`.
    fn evaluate_all(
        &self,
        id: ReviewId,
        request: u64,
        ctx: lectern_logging::Ctx,
        cancel: &Arc<AtomicBool>,
        request_data: &BriefRequest,
        providers: &[Asked],
    ) {
        let mut asked: Vec<u64> = Vec::new();
        let mut dead: Vec<u64> = Vec::new();
        let mut pending = false;
        for (seq, _, provider, token) in providers {
            // Held while the provider runs: it is not deactivated under us.
            let Some(_admission) = token.enter() else {
                match token.state() {
                    TokenState::Pending => pending = true,
                    TokenState::Live | TokenState::Dead => dead.push(*seq),
                }
                continue;
            };
            asked.push(*seq);
            let me = self.me.clone();
            let flag = cancel.clone();
            let registration = *seq;
            let reply = BriefReply::new(
                move |items, done| {
                    if let Some(engine) = me.upgrade() {
                        engine.provider_reply(id, request, registration, items, done);
                    }
                },
                flag,
            );
            provider.evaluate(request_data.clone(), reply);
        }
        {
            let mut state = lock(&self.state);
            if let Some(open) = state.reviews.get_mut(&id)
                && open.brief.request == request
            {
                for reply in &mut open.brief.replies {
                    if asked.contains(&reply.registration) {
                        reply.asked = true;
                    }
                }
            }
        }
        if !dead.is_empty() {
            self.provider_finished(id, request, |r| dead.contains(&r.registration));
        }
        if !pending {
            return;
        }
        match self.cfg.host.clone() {
            Some(host) => {
                let me = self.me.clone();
                host.later(
                    PENDING_RETRY,
                    Box::new(move || {
                        let Some(engine) = me.upgrade() else { return };
                        engine.submit(Priority::Background, ctx, move |engine| {
                            engine.ask_pending(id, request);
                        });
                    }),
                );
            }
            // No timer to come back on: given up, as a dead token.
            None => {
                self.provider_finished(id, request, |r| !r.asked);
            }
        }
    }

    /// `PENDING_RETRY` after a request met a `Pending` token: asks the
    /// providers of this request not asked yet.
    fn ask_pending(&self, id: ReviewId, request: u64) {
        let (waiting, cancel, ctx, work) = {
            let state = lock(&self.state);
            let Some(open) = state.reviews.get(&id) else {
                return;
            };
            if open.brief.request != request {
                return;
            }
            let waiting: Vec<u64> = open
                .brief
                .replies
                .iter()
                .filter(|r| !r.asked && !r.done)
                .map(|r| r.registration)
                .collect();
            let Some(work) = open.work.clone() else {
                return;
            };
            (waiting, open.brief.cancel.clone(), open.ctx, work)
        };
        if waiting.is_empty() {
            return;
        }
        let providers: Vec<Asked> = lock(&self.providers)
            .entries
            .iter()
            .filter(|r| waiting.contains(&r.seq))
            .map(|r| (r.seq, r.id.clone(), r.provider.clone(), r.token.clone()))
            .collect();
        // Registrations dropped meanwhile: `unregister` removed their
        // replies, nothing to do for them.
        let request_data = BriefRequest {
            project: work.git.project.clone(),
            worktree: Some(work.git.key.clone()),
            files: brief_files(&work),
        };
        self.evaluate_all(id, request, ctx, &cancel, &request_data, &providers);
    }

    /// A provider's batch for request `request` of review `id`. Stale
    /// requests are ignored; otherwise the items are kept (the first
    /// `MAX_PROVIDED`) and the snapshot is published again on a
    /// `Background` task.
    fn provider_reply(
        &self,
        id: ReviewId,
        request: u64,
        registration: u64,
        items: Vec<ProvidedItem>,
        done: bool,
    ) {
        let (round, ctx, items) = {
            let mut state = lock(&self.state);
            let Some(open) = state.reviews.get_mut(&id) else {
                return;
            };
            if open.brief.request != request {
                return;
            }
            let Some(reply) = open
                .brief
                .replies
                .iter_mut()
                .find(|r| r.registration == registration)
            else {
                return;
            };
            if reply.done {
                return;
            }
            let room = MAX_PROVIDED.saturating_sub(reply.items.len());
            reply.items.extend(items.into_iter().take(room));
            reply.done = done;
            (open.round, open.ctx, reply.items.len())
        };
        // The provider is named by its registration: a plugin ID is text,
        // and text is not a log value.
        debug!("brief reply";
            id = id.0,
            request = request,
            registration = registration,
            items = items,
            done = done);
        self.submit(Priority::Background, ctx, move |engine| {
            engine.republish(id, round)
        });
    }

    /// `PROVIDER_TIMEOUT` after the request: the providers still pending
    /// count as done.
    fn providers_timed_out(&self, id: ReviewId, request: u64) {
        let late = self.provider_finished(id, request, |_| true);
        for registration in late {
            info!("brief provider timed out";
                id = id.0,
                request = request,
                registration = registration);
        }
    }

    /// Marks the pending replies `which` selects as done, and publishes
    /// the snapshot again when any was. Returns the registrations marked.
    fn provider_finished(
        &self,
        id: ReviewId,
        request: u64,
        mut which: impl FnMut(&Reply) -> bool,
    ) -> Vec<u64> {
        let (round, ctx, finished) = {
            let mut state = lock(&self.state);
            let Some(open) = state.reviews.get_mut(&id) else {
                return Vec::new();
            };
            if open.brief.request != request {
                return Vec::new();
            }
            let mut finished = Vec::new();
            for reply in open.brief.replies.iter_mut().filter(|r| !r.done) {
                if which(reply) {
                    reply.done = true;
                    finished.push(reply.registration);
                }
            }
            if finished.is_empty() {
                return finished;
            }
            (open.round, open.ctx, finished)
        };
        self.submit(Priority::Background, ctx, move |engine| {
            engine.republish(id, round)
        });
        finished
    }

    /// The brief of a snapshot being assembled: the rules over the work,
    /// the attribution computed so far and the seen states, plus the
    /// providers' replies for the current request.
    pub(super) fn brief(
        &self,
        open: &Open,
        work: &Work,
        results: &[Option<Arc<FileResult>>],
        seen: &[SeenState],
    ) -> Arc<Brief> {
        // The files an own session's failed edit claimed.
        let mut failed: HashSet<u32> = HashSet::new();
        for who in work.whos.iter().filter(|w| w.own.is_some()) {
            for call in who.claims.calls.iter() {
                let ClaimKind::Edit { paths, status, .. } = &call.kind else {
                    continue;
                };
                if matches!(status, ToolStatus::Completed) {
                    continue;
                }
                for p in paths.iter() {
                    if let Some(files) = work.by_path.get(p) {
                        failed.extend(files.iter().copied());
                    }
                }
            }
        }
        let commands: Vec<Vec<(u32, u32)>> = work
            .files
            .iter()
            .map(|f| match &f.command {
                Some((who, calls)) if f.history.is_empty() => {
                    calls.iter().map(|c| (*who, *c)).collect()
                }
                _ => Vec::new(),
            })
            .collect();
        let inputs: Vec<FileInput<'_>> = work
            .files
            .iter()
            .enumerate()
            .map(|(n, f)| {
                let change = &work.list.files[f.list as usize];
                let (drifted, drifted_at) = match results.get(n).and_then(Option::as_ref) {
                    Some(r) => drift(work, f, change, r),
                    None => (false, None),
                };
                FileInput {
                    key: &f.key,
                    change,
                    noise: f.noise,
                    seen: seen.get(n).copied().unwrap_or(SeenState::Unseen),
                    drifted,
                    drifted_at,
                    edit_failed: failed.contains(&(n as u32)),
                    command_only: &commands[n],
                }
            })
            .collect();
        // The replies belong to the work they were asked about. A new
        // work's first snapshot is assembled before `emit` asks about it:
        // then nothing is shown and every registered provider is pending.
        let current = open
            .brief
            .asked
            .as_ref()
            .is_some_and(|asked| std::ptr::eq(Weak::as_ptr(asked), work));
        let (provided, pending): (Vec<Provided<'_>>, u32) = if current {
            (
                open.brief
                    .replies
                    .iter()
                    .map(|r| Provided {
                        provider: &r.id,
                        items: &r.items,
                    })
                    .collect(),
                open.brief.replies.iter().filter(|r| !r.done).count() as u32,
            )
        } else {
            (Vec::new(), lock(&self.providers).entries.len() as u32)
        };
        Arc::new(brief::build(
            &inputs,
            &provided,
            pending,
            // Analyze comes with #66.
            AnalysisState::Unavailable(Arc::from("unavailable")),
        ))
    }
}

/// Whether the file drifted (「简报规则」: lines of no provenance, or a
/// step that did not align), and the first line of the first drifted hunk.
fn drift(
    work: &Work,
    f: &FileWork,
    change: &lectern_services::changes::FileChange,
    result: &FileResult,
) -> (bool, Option<LineAnchor>) {
    let misaligned = result
        .chain
        .as_ref()
        .is_some_and(|chain| attribute::misaligned(work, f, chain));
    let drifted = result.attribution.drifted_hunks > 0 || misaligned;
    let at = result
        .hunks
        .iter()
        .position(|h| h.drifted > 0)
        .and_then(|i| {
            let ChangeDetail::Text { hunks, .. } = &change.detail else {
                return None;
            };
            let hunk = hunks.get(i)?;
            Some(if hunk.new.is_empty() {
                LineAnchor {
                    side: DiffSide::Old,
                    line: hunk.old.start,
                }
            } else {
                LineAnchor {
                    side: DiffSide::New,
                    line: hunk.new.start,
                }
            })
        });
    (drifted, at)
}

/// The request's files: the snapshot's, minus noise files and paths that
/// are not UTF-8 (`BriefRequest::files`).
fn brief_files(work: &Work) -> Arc<[BriefFile]> {
    work.files
        .iter()
        .filter(|f| f.noise.is_none())
        .filter_map(|f| {
            let change = &work.list.files[f.list as usize];
            let path = RelPath::from(change.path.as_str()?);
            let hunks = match &change.detail {
                ChangeDetail::Text { hunks, .. } => hunks.clone(),
                _ => Arc::from(Vec::new()),
            };
            Some(BriefFile {
                key: f.key.clone(),
                path,
                old_oid: side_oid(change.old.as_ref()),
                new_oid: side_oid(change.new.as_ref()),
                hunks,
            })
        })
        .collect()
}
