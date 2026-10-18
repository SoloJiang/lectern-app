//! Semantic navigation (#26's hook, docs/design/code_intel.md「语义导航的
//! 注册」): registered providers, when a query escalates to them, and the
//! fixed arbitration between several providers of one language.
//!
//! - A Java `Definition` with several candidates after step 3, or only one
//!   `NameOnly` candidate, and a Java `References`, ask the providers that
//!   handle Java; only for worktree text.
//! - `head_only` providers (SCIP) answer for files equal to HEAD and are
//!   asked only when the text is; the others (jdtls) answer for changed and
//!   untracked files. Within a class, by provider ID.
//! - A reply's locations go to the class responsible for their file; one
//!   file with locations from several providers takes the first one's. The
//!   merge depends only on the replies arrived, so neither activation order
//!   nor reply order changes it.
//! - Only replies whose request ID is still current are taken. Closing a
//!   query (or the project, or this activation) cancels its unanswered
//!   requests; another provider's reply never does.

use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use lectern_executor::Priority;
use lectern_kernel::ActivationToken;
use lectern_services::code_intel::{
    Candidate, DefinitionRef, Evidence, LangName, Location, NavigationProvider, Position,
    ProviderRegistration, QueryId, QueryResults, SemanticError, SemanticLocation, SemanticReply,
    SemanticRequest, SemanticRequestId, SemanticState, TextOrigin, TextRef,
};
use lectern_services::project::{ProjectKey, RelPath, WorktreeKey};

use crate::index::{Inner, lock};
use crate::map::LiveMap;
use crate::query::{Prepared, Snap};

/// The language whose queries escalate.
const JAVA: &str = "java";

/// One registered provider.
#[derive(Clone)]
pub(crate) struct Registered {
    pub seq: u64,
    pub provider: Arc<dyn NavigationProvider>,
    pub token: ActivationToken,
}

#[derive(Default)]
pub(crate) struct Providers {
    next: u64,
    list: Vec<Registered>,
}

/// Which provider call a query makes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SemKind {
    Definition,
    References,
}

/// One request of a query.
pub(crate) struct SemRequest {
    pub id: SemanticRequestId,
    pub seq: u64,
    pub provider: Arc<dyn NavigationProvider>,
    pub token: ActivationToken,
    pub provider_id: Box<str>,
    pub head_only: bool,
    pub reply: Option<Result<Arc<[SemanticLocation]>, SemanticError>>,
    /// False once its provider unregistered: its reply is never taken.
    pub live: bool,
}

/// A query's semantic requests, their replies and the merge.
pub(crate) struct SemQuery {
    pub kind: SemKind,
    pub project: ProjectKey,
    pub worktree: Option<WorktreeKey>,
    /// The map the query started with: it gives the replies' OIDs.
    pub map: LiveMap,
    pub requests: Vec<SemRequest>,
    /// The merge of the replies so far; None until one is `Ok`.
    pub merged: Option<Arc<[Location]>>,
    /// Grows with every change of `requests`: a merge computed from an
    /// older one is not installed (the change merges again itself).
    pub version: u64,
}

fn position_set(locations: impl Iterator<Item = (RelPath, u32, u32)>) -> HashSet<(RelPath, u32, u32)> {
    locations.collect()
}

impl SemQuery {
    /// `QueryResults::semantic`: `Same` when the merge lists the candidates'
    /// (or callers') positions, `Ready` when not; `Pending` while requests
    /// are open; `Failed` when none can answer any more.
    pub fn state(&self, results: &QueryResults) -> SemanticState {
        if self.requests.is_empty() {
            return SemanticState::NotRequested;
        }
        if let Some(merged) = &self.merged {
            let listed = match self.kind {
                SemKind::Definition => position_set(results.candidates.iter().map(|c: &Candidate| {
                    let l = &c.definition.location;
                    (l.path.clone(), l.line, l.column)
                })),
                SemKind::References => position_set(
                    results
                        .callers
                        .iter()
                        .map(|r| (r.location.path.clone(), r.location.line, r.location.column)),
                ),
            };
            let semantic = position_set(merged.iter().map(|l| (l.path.clone(), l.line, l.column)));
            return if semantic == listed {
                SemanticState::Same
            } else {
                SemanticState::Ready(merged.clone())
            };
        }
        if self.requests.iter().any(|r| r.live && r.reply.is_none()) {
            SemanticState::Pending
        } else {
            SemanticState::Failed
        }
    }

    /// Cancels every request not answered yet, within its provider's
    /// admission.
    pub fn cancel_unanswered(&self) {
        for r in &self.requests {
            if r.live
                && r.reply.is_none()
                && let Some(_admission) = r.token.enter()
            {
                r.provider.cancel(r.id);
            }
        }
    }

    /// The merge of the replies arrived, by the fixed arbitration.
    /// `head_clean` says whether a path's content equals HEAD.
    fn merge(&self, head_clean: &dyn Fn(&str) -> bool) -> Option<Arc<[Location]>> {
        let mut order: Vec<&SemRequest> = self.requests.iter().collect();
        order.sort_by(|a, b| (!a.head_only, &a.provider_id).cmp(&(!b.head_only, &b.provider_id)));
        // HEAD-equal files belong to the head-only class while one of its
        // providers can still answer or has.
        let head_class = self
            .requests
            .iter()
            .any(|r| r.head_only && (r.live || r.reply.is_some()));
        let mut owner: HashMap<RelPath, &str> = HashMap::new();
        let mut any = false;
        let mut out = Vec::new();
        for r in order {
            let Some(Ok(locations)) = &r.reply else {
                continue;
            };
            any = true;
            for loc in locations.iter() {
                let file_head = head_class && head_clean(&loc.path);
                if r.head_only != file_head {
                    continue;
                }
                match owner.get(&loc.path) {
                    Some(o) if *o != &*r.provider_id => continue,
                    Some(_) => {}
                    None => {
                        owner.insert(loc.path.clone(), &r.provider_id);
                    }
                }
                // Paths the query's map does not have (deleted, ignored,
                // another worktree) are dropped.
                let Some(eff) = self.map.effective(&loc.path) else {
                    continue;
                };
                out.push(Location {
                    worktree: self.worktree.clone(),
                    path: eff.path,
                    origin: TextOrigin::Worktree,
                    oid: eff.oid,
                    line: loc.line,
                    column: loc.column,
                    end_line: loc.end_line,
                });
            }
        }
        if !any {
            return None;
        }
        out.sort_by(|a, b| (&a.path, a.line, a.column).cmp(&(&b.path, b.line, b.column)));
        out.dedup();
        Some(out.into())
    }
}

type Reply = Result<Arc<[SemanticLocation]>, SemanticError>;

/// Replies waiting to be taken, and whether a task takes them.
#[derive(Default)]
pub(crate) struct Replies {
    queue: Vec<(QueryId, SemanticRequestId, Reply)>,
    running: bool,
}

impl Inner {
    /// `CodeIntel::register_provider`.
    pub(crate) fn register_provider(
        &self,
        provider: Arc<dyn NavigationProvider>,
        token: ActivationToken,
    ) -> ProviderRegistration {
        let seq = {
            let mut p = lock(&self.providers);
            p.next += 1;
            let seq = p.next;
            p.list.push(Registered {
                seq,
                provider,
                token,
            });
            seq
        };
        let me = self.me.clone();
        ProviderRegistration::new(move || {
            if let Some(inner) = me.upgrade() {
                inner.unregister(seq);
            }
        })
    }

    /// The providers registered now (tests).
    #[cfg(test)]
    pub(crate) fn providers(&self) -> usize {
        lock(&self.providers).list.len()
    }

    /// A provider is gone (its registration dropped, or its activation no
    /// longer admits calls): its unanswered requests are void, and the
    /// queries that had them merge again.
    fn unregister(&self, seq: u64) {
        lock(&self.providers).list.retain(|r| r.seq != seq);
        let affected: Vec<QueryId> = {
            let mut q = lock(&self.queries);
            q.entries
                .iter_mut()
                .filter_map(|e| {
                    let s = e.semantic.as_mut()?;
                    let mut hit = false;
                    for r in &mut s.requests {
                        if r.seq == seq && r.reply.is_none() && r.live {
                            r.live = false;
                            hit = true;
                        }
                    }
                    if hit {
                        s.version += 1;
                    }
                    hit.then_some(e.id)
                })
                .collect()
        };
        if affected.is_empty() {
            return;
        }
        let Some(me) = self.arc() else {
            return;
        };
        self.host.spawn_busy("semantic", Priority::Background, move || {
            for id in affected {
                me.remerge(id);
            }
        });
    }

    pub(crate) fn escalate_definition(
        &self,
        id: QueryId,
        snap: &Snap,
        at: &TextRef,
        position: Position,
        prepared: &Prepared,
    ) {
        let c = &prepared.candidates;
        let wanted = c.len() > 1 || (c.len() == 1 && c[0].evidence == Evidence::NameOnly);
        if wanted {
            self.escalate(id, snap, SemKind::Definition, at, position, prepared.lang.name());
        }
    }

    pub(crate) fn escalate_references(&self, id: QueryId, snap: &Snap, of: &DefinitionRef) {
        let l = &of.location;
        let at = TextRef {
            worktree: l.worktree.clone(),
            path: l.path.clone(),
            oid: l.oid,
            origin: l.origin,
        };
        let position = Position {
            line: l.line,
            column: l.column,
        };
        self.escalate(id, snap, SemKind::References, &at, position, &of.lang);
    }

    /// Asks the providers of `lang` for the text `at`, by the arbitration's
    /// order. Only Java, only worktree text (an object's text is history,
    /// the providers answer for the worktree).
    fn escalate(
        &self,
        id: QueryId,
        snap: &Snap,
        kind: SemKind,
        at: &TextRef,
        position: Position,
        lang: &LangName,
    ) {
        if &*lang.0 != JAVA || at.origin != TextOrigin::Worktree {
            return;
        }
        let Some(worktree_root) = snap.root_path.clone() else {
            return;
        };
        let head_clean = self.head_clean(&snap.project.key, &at.path);
        let registered: Vec<Registered> = lock(&self.providers).list.clone();
        let mut chosen = Vec::new();
        for r in registered {
            let Some(_admission) = r.token.enter() else {
                self.unregister(r.seq);
                continue;
            };
            if !r.provider.handles(lang) {
                continue;
            }
            let head_only = r.provider.head_only();
            if head_only && !head_clean {
                continue;
            }
            chosen.push(SemRequest {
                id: SemanticRequestId(self.next_request.fetch_add(1, Ordering::AcqRel) + 1),
                seq: r.seq,
                provider_id: r.provider.id().into(),
                provider: r.provider.clone(),
                token: r.token.clone(),
                head_only,
                reply: None,
                live: true,
            });
        }
        if chosen.is_empty() {
            return;
        }
        chosen.sort_by(|a, b| (!a.head_only, &a.provider_id).cmp(&(!b.head_only, &b.provider_id)));
        let requests: Vec<(SemanticRequestId, u64, Arc<dyn NavigationProvider>, ActivationToken)> = chosen
            .iter()
            .map(|r| (r.id, r.seq, r.provider.clone(), r.token.clone()))
            .collect();
        let installed = self.publish_entry(id, |_, semantic| {
            *semantic = Some(SemQuery {
                kind,
                project: snap.project.key.clone(),
                worktree: snap.worktree.clone(),
                map: snap.map.clone(),
                requests: chosen,
                merged: None,
                version: 0,
            });
        });
        if !installed {
            return;
        }
        for (rid, seq, provider, token) in requests {
            let Some(_admission) = token.enter() else {
                self.unregister(seq);
                continue;
            };
            let request = SemanticRequest {
                id: rid,
                worktree_root: worktree_root.clone(),
                at: at.clone(),
                position,
                head_clean,
            };
            let me = self.me.clone();
            let reply = SemanticReply::new(move |result| {
                if let Some(inner) = me.upgrade() {
                    inner.semantic_reply(id, rid, result);
                }
            });
            match kind {
                SemKind::Definition => provider.definition(request, reply),
                SemKind::References => provider.references(request, reply),
            }
        }
    }

    /// `SemanticReply::send`: only queues the reply, never blocks.
    fn semantic_reply(
        &self,
        id: QueryId,
        rid: SemanticRequestId,
        result: Result<Arc<[SemanticLocation]>, SemanticError>,
    ) {
        {
            let mut replies = lock(&self.replies);
            replies.queue.push((id, rid, result));
            if replies.running {
                return;
            }
            replies.running = true;
        }
        let Some(me) = self.arc() else {
            return;
        };
        let spawned = self
            .host
            .spawn_busy("semantic", Priority::Background, move || me.take_replies());
        if !spawned {
            let mut replies = lock(&self.replies);
            replies.queue.clear();
            replies.running = false;
        }
    }

    fn take_replies(&self) {
        loop {
            let queue = {
                let mut replies = lock(&self.replies);
                if replies.queue.is_empty() {
                    replies.running = false;
                    return;
                }
                std::mem::take(&mut replies.queue)
            };
            let mut touched = Vec::new();
            {
                let mut q = lock(&self.queries);
                for (id, rid, result) in queue {
                    let Some(entry) = q.entries.iter_mut().find(|e| e.id == id) else {
                        continue;
                    };
                    let Some(s) = entry.semantic.as_mut() else {
                        continue;
                    };
                    // Only the query's current request IDs are taken.
                    if let Some(r) = s
                        .requests
                        .iter_mut()
                        .find(|r| r.id == rid && r.live && r.reply.is_none())
                    {
                        r.reply = Some(result);
                        s.version += 1;
                        if !touched.contains(&id) {
                            touched.push(id);
                        }
                    }
                }
            }
            for id in touched {
                self.remerge(id);
            }
        }
    }

    /// Merges a query's replies again and publishes the new state.
    fn remerge(&self, id: QueryId) {
        // Merged outside the queries' lock: `head_clean` locks the project.
        let view = {
            let q = lock(&self.queries);
            let Some(entry) = q.entries.iter().find(|e| e.id == id) else {
                return;
            };
            let Some(s) = &entry.semantic else {
                return;
            };
            SemQuery {
                kind: s.kind,
                project: s.project.clone(),
                worktree: s.worktree.clone(),
                map: s.map.clone(),
                requests: s
                    .requests
                    .iter()
                    .map(|r| SemRequest {
                        id: r.id,
                        seq: r.seq,
                        provider: r.provider.clone(),
                        token: r.token.clone(),
                        provider_id: r.provider_id.clone(),
                        head_only: r.head_only,
                        reply: r.reply.clone(),
                        live: r.live,
                    })
                    .collect(),
                merged: None,
                version: s.version,
            }
        };
        let project = view.project.clone();
        let merged = view.merge(&|path| self.head_clean(&project, path));
        self.publish_entry(id, |_, semantic| {
            if let Some(s) = semantic
                && s.version == view.version
            {
                s.merged = merged;
            }
        });
    }
}
