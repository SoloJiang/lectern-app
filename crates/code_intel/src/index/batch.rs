//! The tiers (「建索引的分层」「调度、批次与取消」).
//!
//! Tiers 1 and 2 are small explicit queues on the worker threads
//! (`Interactive` / `Background`, `BusyGuard`). Tiers 3–5 run in
//! long-running `Batch` tasks holding a `BackgroundGuard`, one per batch
//! thread: each batch is an existence check of up to 500 files, or the
//! extraction of the files it missed, ending at 64 files, 2 MiB read or the
//! first file boundary after 50 ms; between batches the task asks its guard
//! whether to pause. A file the pause interrupts is abandoned (the watchdog
//! cancels it) and goes back to the front of its queue.
//!
//! Interaction yield (「交互让路」, ADR 0033): a batch task holds one guard
//! at a time, `background()` for tier 3 (it doesn't yield) and
//! `background_yielding()` for tiers 4 and 5, swapped at batch boundaries.
//! With the yielding guard a boundary asks `proceed_item` for the next
//! item's size; in a keepalive slot only that item runs. An extraction of
//! a core language stops at its next slice boundary when asked to yield and
//! keeps its state in the task (`Parked`) until the next grant; a language
//! pack's is given up. Items a slot can't do (over 256 KiB, a pack's
//! extraction longer than a slot, or a canonical-OID check still due once
//! the switch is on: `check_keys`) are set aside, in order, until a batch
//! runs with the switch off, and the tier goes on with the items behind
//! them.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use lectern_cache::Written;
use lectern_executor::Priority;
use lectern_kernel::{BackgroundGuard, Grant, SLOT_MAX_BYTES};
use lectern_platform::{Read, RelPath as SafeRel, SafeRoot};
use lectern_services::code_intel::TextOrigin;
use lectern_services::project::{BlobId, ProjectKey};

use super::host::yield_now;
use super::state::{
    Cursor, Item, OtherTree, OtherWalk, Owner, Project, Scope, Source, State, Tier, Wake, Walk,
    under,
};
use super::{Inner, bump, lock};
use crate::canonical::{Canonical, Pending};
use crate::extract::{
    BACKGROUND_DEADLINE, BACKGROUND_MAX_LEN, Deadline, ExtractStatus, Extraction,
    INTERACTIVE_MAX_LEN, Lang, LanguageTable, ResumeSlot,
};
use crate::git_access::{Tree, hash_blob, hash_kind};
use crate::map::{HintStamp, LangLookup};
use crate::store::{EXISTS_BATCH, Found, UnitMeta, Version};
use crate::util::log;

/// A batch ends after this many files…
pub const BATCH_FILES: usize = 64;
/// …or this many bytes read…
pub const BATCH_BYTES: usize = 2 << 20;
/// …or at the first file boundary after this long.
pub const BATCH_TIME: Duration = Duration::from_millis(50);
/// Entries a gathering step looks at, at most, under the project's lock.
const SCAN_LIMIT: usize = 4096;
/// Files larger than this are extracted by one batch task at a time.
pub const LARGE_FILE: usize = 256 << 10;

/// The one batch extraction of a large file (`LARGE_FILE`). A yielding
/// task's extraction stopped with the slot registers its saved state here
/// (`LargeTurn::park`): non-yielding work (tier 3) that wants the slot
/// takes it over at once, dropping that state, instead of waiting for the
/// parked task to run again (「交互让路」). So no batch task ever waits for
/// another one to be polled, whatever the number of tasks and threads.
#[derive(Default)]
pub(crate) struct LargeSlot {
    state: Mutex<LargeState>,
    freed: std::sync::Condvar,
}

#[derive(Default)]
struct LargeState {
    /// The turn holding the slot (`LargeTurn`'s number), if any.
    holder: Option<u64>,
    /// The last turn handed out.
    turns: u64,
    /// The holder is parked, its extraction's state here.
    parked: Option<ResumeSlot>,
}

/// The slot, held while the slot's holder is this turn; released on drop.
/// A parked turn taken over by non-yielding work is revoked: it holds
/// nothing any more.
pub(crate) struct LargeTurn {
    slot: Arc<LargeSlot>,
    turn: u64,
}

impl LargeSlot {
    /// Waits for the slot, checking `stop` every 10 ms; None once `stop`
    /// says so. `near`: non-yielding work (tier 3), which takes the slot
    /// over from a parked holder (its saved state is dropped, its turn
    /// revoked) and then runs `wake` (the batch tasks waiting at a
    /// boundary), so the holder puts its item back.
    pub(crate) fn acquire(
        self: &Arc<Self>,
        stop: impl Fn() -> bool,
        near: bool,
        wake: impl FnOnce(),
    ) -> Option<LargeTurn> {
        let mut st = lock(&self.state);
        let mut revoked = None;
        loop {
            if stop() {
                return None;
            }
            if st.holder.is_none() {
                break;
            }
            if near && let Some(resume) = st.parked.take() {
                revoked = Some(resume);
                break;
            }
            st = self
                .freed
                .wait_timeout(st, crate::CHECK_INTERVAL)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
        st.turns += 1;
        let turn = st.turns;
        st.holder = Some(turn);
        drop(st);
        if let Some(resume) = revoked {
            // The parked tree goes now, before this turn's work starts.
            drop(lock(&resume).take());
            wake();
        }
        Some(LargeTurn {
            slot: self.clone(),
            turn,
        })
    }
}

#[cfg(test)]
impl LargeSlot {
    /// Whether a turn holds the slot.
    pub(crate) fn busy(&self) -> bool {
        lock(&self.state).holder.is_some()
    }

    /// Whether the holder is parked, its saved state registered.
    pub(crate) fn parked(&self) -> bool {
        lock(&self.state).parked.is_some()
    }
}

impl LargeTurn {
    /// The holder's extraction stopped to yield, its state in `resume`:
    /// non-yielding work may take the slot over.
    pub(crate) fn park(&self, resume: &ResumeSlot) {
        let mut st = lock(&self.slot.state);
        if st.holder == Some(self.turn) {
            st.parked = Some(resume.clone());
            drop(st);
            self.slot.freed.notify_all();
        }
    }

    /// The holder goes on: false when its turn was revoked meanwhile (its
    /// saved state is gone and the slot is someone else's).
    pub(crate) fn unpark(&self) -> bool {
        let mut st = lock(&self.slot.state);
        if st.holder == Some(self.turn) {
            st.parked = None;
            true
        } else {
            false
        }
    }

    /// Taken over by non-yielding work.
    pub(crate) fn revoked(&self) -> bool {
        lock(&self.slot.state).holder != Some(self.turn)
    }
}

impl Drop for LargeTurn {
    fn drop(&mut self) {
        let mut st = lock(&self.slot.state);
        if st.holder == Some(self.turn) {
            st.holder = None;
            st.parked = None;
            drop(st);
            self.slot.freed.notify_all();
        }
    }
}

/// Which guard a batch's work needs (「交互让路」).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Kind {
    /// Tier 3: `background()`, doesn't yield.
    Near,
    /// Tiers 4 and 5: `background_yielding()`.
    Far,
}

/// The one guard a batch task holds: the kernel counts a task as stopped by
/// its guard, so a task with two would never read as paused.
enum TaskGuard {
    Near(Arc<BackgroundGuard>),
    Far(Arc<BackgroundGuard>),
    None,
}

impl TaskGuard {
    /// The guard for `kind`: the one held, or, after dropping it (which
    /// leaves the kernel's slot queue), a new one. None once the
    /// activation is gone.
    fn of(&mut self, kind: Kind, host: &super::host::Host) -> Option<Arc<BackgroundGuard>> {
        match (&*self, kind) {
            (TaskGuard::Near(g), Kind::Near) | (TaskGuard::Far(g), Kind::Far) => {
                return Some(g.clone());
            }
            _ => {}
        }
        *self = TaskGuard::None;
        let guard = Arc::new(host.background(kind == Kind::Far)?);
        *self = match kind {
            Kind::Near => TaskGuard::Near(guard.clone()),
            Kind::Far => TaskGuard::Far(guard.clone()),
        };
        Some(guard)
    }
}

/// An item a yielding batch task carries from one boundary to the next
/// (「交互让路」): read and registered in flight, stopped at a slice boundary
/// with its extraction's state, or waiting for the large slot. At most one
/// per task; one over `SLOT_MAX_BYTES` goes on only with the switch off,
/// and is set aside when the task's keepalive slot work stops in turn.
pub(crate) struct Parked {
    project: Arc<Project>,
    session: u64,
    t: usize,
    item: Item,
    flag: Arc<AtomicBool>,
    key: BlobId,
    /// `key` is the raw OID of worktree bytes that differ from the queued
    /// one: the batch's `canonical_oids` decides the key before the commit
    /// (`Done::check`).
    check: bool,
    /// `key` is the raw bytes' OID (`UnitMeta::canonical`).
    canonical: bool,
    bytes: Vec<u8>,
    /// The extraction's state (`extract::Resume`, with the deadline's
    /// budget left); empty until it starts.
    resume: ResumeSlot,
    /// Held from the start of a large file's extraction (`LARGE_FILE`).
    large: Option<LargeTurn>,
}

impl Parked {
    fn bytes(&self) -> u64 {
        self.bytes.len() as u64
    }
}

/// A yielding batch boundary's answer.
enum Boundary {
    Grant(Grant),
    /// Tier 3 work came, the project that runs tiers 3–5 changed, the
    /// parked item's session ended, or tier 3 took the large slot over
    /// from it: look again.
    Interrupted,
}

/// Over `SLOT_MAX_BYTES`: `proceed_item` returns only once the switch is
/// off (`Grant::Batch`).
const BATCH_ONLY: u64 = SLOT_MAX_BYTES + 1;

/// Waits at a yielding boundary for `proceed_item`, registered in the batch
/// tasks' wake list (`kick_batch` wakes it), so tier 3 work never waits for
/// a keepalive slot. The size asked for is worked out at every poll
/// (`Inner::boundary_bytes`): what a slot would run changes while it waits.
struct FarBoundary<'a> {
    inner: &'a Inner,
    project: &'a Arc<Project>,
    guard: &'a BackgroundGuard,
    /// The task's parked item: its session and size.
    parked: Option<(u64, u64)>,
    /// The parked item's large-slot turn.
    large: Option<&'a LargeTurn>,
}

impl Future for FarBoundary<'_> {
    type Output = Boundary;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Boundary> {
        {
            let mut run = lock(&self.inner.batch);
            if !run.waiters.iter().any(|w| w.will_wake(cx.waker())) {
                run.waiters.push(cx.waker().clone());
            }
        }
        // The kernel wakes waiting work for a pause: a parked item's state
        // goes first. A parked item of a project that no longer runs tiers
        // 3–5, or of an ended session, is given up at once: its state and
        // the large slot are not held while the other project waits.
        let moved = !self
            .inner
            .batch_project()
            .is_some_and(|p| Arc::ptr_eq(&p, self.project));
        let ended = self
            .parked
            .is_some_and(|(session, _)| !lock(&self.project.state).live(session));
        if moved
            || ended
            || (self.parked.is_some() && self.guard.pause_requested())
            || self.inner.plan(self.project) == Some(Kind::Near)
            || self.large.is_some_and(LargeTurn::revoked)
        {
            return Poll::Ready(Boundary::Interrupted);
        }
        let bytes = self
            .inner
            .boundary_bytes(self.project, self.parked.map(|(_, b)| b));
        self.guard.poll_proceed_item(cx, bytes).map(Boundary::Grant)
    }
}

/// How an item was extracted.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    /// Tier 1: the interactive deadline and size limit.
    Interactive,
    /// Tier 2: the background deadline.
    Background,
    /// Tiers 3–5: the background deadline, interrupted by the pause.
    Batch,
}

/// An extraction a query made of a file not stored yet (a search hit, an
/// import target), to be written like tier 2's (「输入代数与派生状态」).
pub(crate) struct Scanned {
    pub path: lectern_services::project::RelPath,
    /// The OID of the bytes extracted.
    pub oid: BlobId,
    /// The language as the query's (current) table had it.
    pub lang: Lang,
    pub extraction: Extraction,
    /// The bytes extracted hash to `oid` (`UnitMeta::canonical`).
    pub canonical: bool,
}

impl Clone for Scanned {
    fn clone(&self) -> Scanned {
        Scanned {
            path: self.path.clone(),
            oid: self.oid,
            lang: self.lang.clone(),
            extraction: self.extraction.clone(),
            canonical: self.canonical,
        }
    }
}

/// One extraction ready to be written.
struct Done {
    item: Item,
    /// The OID of the bytes actually read (「存储表」).
    key: BlobId,
    /// A batch tier's worktree bytes whose raw OID (`key`) is not the
    /// queued OID: kept until the batch's `canonical_oids` before the
    /// commit, which makes the queued OID the key when the built-in
    /// conversions turn the bytes into it (「规范内容 OID 的批量核对」).
    check: Option<Vec<u8>>,
    extraction: Extraction,
    /// The cancel flag the item was taken with (`State::finish`).
    flag: Arc<AtomicBool>,
    /// The unit's part and whether `key` is the raw bytes' OID
    /// (`UnitMeta::canonical`; a key `check` turns into the queued OID is
    /// not).
    meta: UnitMeta,
}

/// One item read ([`Inner::read_item`]).
struct ReadItem {
    bytes: Vec<u8>,
    /// The key: the raw bytes' OID, or the queued OID when the built-in
    /// conversions turn them into it.
    key: BlobId,
    /// `key` is the raw OID, left to the batch's `canonical_oids`.
    check: bool,
    /// `key` is the raw bytes' OID (`UnitMeta::canonical`).
    canonical: bool,
}

enum Outcome {
    Done(Done, usize),
    /// The pause interrupted it: back to the front of its queue.
    Paused,
    /// Nothing to write (gone, changed in another worktree, cancelled).
    Skip,
    /// Stopped to yield, its state kept (「交互让路」).
    Parked(Parked),
    /// Can't be done in a keepalive slot (a language pack's extraction
    /// gave way to the yield): set aside until the switch is off.
    Aside,
}

/// What a batch task should do next.
#[derive(PartialEq, Eq)]
enum Step {
    Worked,
    Idle,
}

/// The read side of one session, copied out of the lock.
struct Ctx {
    session: u64,
    cancel: Arc<AtomicBool>,
    root: Option<Arc<SafeRoot>>,
    tree: Option<Tree>,
}

impl Inner {
    /// Work for tiers 3–5 may have appeared: the pool runs `batch_tasks`
    /// tasks (one per batch thread), and a task about to stop goes on.
    ///
    /// The liveness rule: after every operation that can create batch work
    /// (open, close, a change of the project that runs tiers 3–5, focus,
    /// grammar, the end of a reconcile, and work queued from outside the
    /// batch tasks: hits to mark as used, a write that may evict, each with
    /// a [`Wake`] for [`Inner::wake`]), either no project has runnable tier
    /// 3–5 work or the pool is full (`Inner::live`, checked by the tests'
    /// harness).
    pub(crate) fn kick_batch(self: &Arc<Self>) {
        // `kicked` first: a task going idle after this sees it and steps
        // again; one that went idle before has left room in the pool, which
        // `fill_pool` fills. Set after, a full pool's last task could go
        // idle in between, leave, and the kick would wait for the next one.
        let waiters = {
            let mut run = lock(&self.batch);
            run.kicked = true;
            std::mem::take(&mut run.waiters)
        };
        // Tasks waiting at a yielding boundary look for tier 3 work.
        for waker in waiters {
            waker.wake();
        }
        self.fill_pool();
    }

    /// Wakes the batch tasks waiting at a yielding boundary (tier 3 wants
    /// the large slot).
    fn wake_waiters(&self) {
        let waiters = std::mem::take(&mut lock(&self.batch).waiters);
        for waker in waiters {
            waker.wake();
        }
    }

    /// Wakes the batch tasks for the work `_wake` stands for. Called with
    /// no project lock held.
    pub(crate) fn wake(&self, _wake: Wake) {
        if let Some(me) = self.arc() {
            me.kick_batch();
        }
    }

    /// Starts batch tasks up to `batch_tasks`.
    fn fill_pool(self: &Arc<Self>) {
        loop {
            {
                let mut run = lock(&self.batch);
                if run.running >= self.batch_tasks {
                    return;
                }
                run.running += 1;
                #[cfg(test)]
                {
                    run.peak = run.peak.max(run.running);
                }
            }
            let far = self
                .batch_project()
                .is_some_and(|p| self.plan(&p) == Some(Kind::Far));
            let me = self.clone();
            let spawned = self.host.spawn_batch("batch", far, move |guard| {
                let guard = Arc::new(guard);
                me.batch_loop(if far {
                    TaskGuard::Far(guard)
                } else {
                    TaskGuard::Near(guard)
                })
            });
            if !spawned {
                lock(&self.batch).running -= 1;
                return;
            }
        }
    }

    async fn batch_loop(self: Arc<Self>, guard: TaskGuard) {
        let mut guard = guard;
        let mut parked: Option<Parked> = None;
        // The kind whose step just found nothing: asked for again, the task
        // goes idle instead of spinning.
        let mut idled: Option<Kind> = None;
        loop {
            let project = self.batch_project();
            if let Some(p) = parked.take() {
                // A parked item of another project or session is given up.
                let live = project.as_ref().is_some_and(|q| Arc::ptr_eq(q, &p.project))
                    && lock(&p.project.state).live(p.session);
                if live {
                    parked = Some(p);
                } else {
                    self.abandon(p);
                }
            }
            let planned = project.as_ref().and_then(|p| self.plan(p));
            let kind = match planned {
                Some(Kind::Near) => Some(Kind::Near),
                _ if parked.is_some() => Some(Kind::Far),
                other => other,
            };
            let (Some(kind), Some(project)) = (kind.filter(|k| idled != Some(*k)), project) else {
                idled = None;
                let mut run = lock(&self.batch);
                if run.kicked {
                    run.kicked = false;
                    continue;
                }
                run.running -= 1;
                drop(run);
                if let Some(p) = parked.take() {
                    self.abandon(p);
                }
                return;
            };
            let Some(g) = guard.of(kind, &self.host) else {
                // The activation is gone.
                if let Some(p) = parked.take() {
                    self.abandon(p);
                }
                lock(&self.batch).running -= 1;
                return;
            };
            if g.pause_requested()
                && let Some(p) = parked.take()
            {
                // The pause wins over the yield: the saved state goes.
                self.abandon(p);
            }
            let step = match kind {
                Kind::Near => {
                    g.proceed().await;
                    self.near_step(&project, &g, &mut parked)
                }
                Kind::Far => {
                    let boundary = FarBoundary {
                        inner: &self,
                        project: &project,
                        guard: &g,
                        parked: parked.as_ref().map(|p| (p.session, p.bytes())),
                        large: parked.as_ref().and_then(|p| p.large.as_ref()),
                    };
                    match boundary.await {
                        Boundary::Interrupted => {
                            // Tier 3 took the large slot over: the saved
                            // state is gone, the item goes back.
                            if let Some(p) =
                                parked.take_if(|p| p.large.as_ref().is_some_and(LargeTurn::revoked))
                            {
                                self.abandon(p);
                            }
                            // The loop's top gives up a parked item of another
                            // project or an ended session.
                            idled = None;
                            continue;
                        }
                        Boundary::Grant(grant) => self.far_step(&project, &g, grant, &mut parked),
                    }
                }
            };
            drop(g);
            match step {
                Step::Worked => {
                    idled = None;
                    // A step can leave work for others (missed files in a
                    // tier's queue): tasks that stopped start again.
                    self.fill_pool();
                    yield_now().await;
                }
                Step::Idle => idled = Some(kind),
            }
        }
    }

    /// Which kind of work the project that runs tiers 3–5 has for a batch
    /// task now, in the tiers' order: tier 3 (its queue, a walk to rebuild
    /// or to go on with) before tiers 4 and 5 (their queues and walks,
    /// other worktrees left, touches, items set aside). None: nothing to
    /// do.
    pub(crate) fn plan(&self, project: &Arc<Project>) -> Option<Kind> {
        self.plan_with(project, true)
    }

    /// Whether tiers 4 and 5 have work a keepalive slot can do: anything
    /// but the items set aside for the switch to turn off.
    fn slot_work(&self, project: &Arc<Project>) -> bool {
        self.plan_with(project, false) == Some(Kind::Far)
    }

    /// The size a yielding boundary asks `proceed_item` for. A parked item
    /// that fits a slot goes on in one. Otherwise a slot takes the next
    /// queued item (`far_step` sets a parked item over `SLOT_MAX_BYTES`
    /// aside first), when there is one; with only items that need the
    /// switch off (a parked large item, the set-aside lists), the task
    /// waits for `Grant::Batch`, its parked state kept.
    fn boundary_bytes(&self, project: &Arc<Project>, parked: Option<u64>) -> u64 {
        match parked {
            Some(bytes) if bytes <= SLOT_MAX_BYTES => bytes,
            _ if self.slot_work(project) => 0,
            _ => BATCH_ONLY,
        }
    }

    fn plan_with(&self, project: &Arc<Project>, aside: bool) -> Option<Kind> {
        let eviction = project.evictions();
        let st = lock(&project.state);
        if st.base_for.is_none() || st.cancel.load(Ordering::Acquire) {
            return None;
        }
        if !st.batch.resume[0].is_empty() {
            return Some(Kind::Near);
        }
        let deps = st.cursor_deps(eviction);
        let Some(cursor) = st
            .batch
            .cursor
            .as_ref()
            .filter(|c| c.deps == deps && c.scope == subproject(&st))
        else {
            // The walks are rebuilt first, by a tier 3 step.
            return Some(Kind::Near);
        };
        if !cursor.walks[0].done {
            return Some(Kind::Near);
        }
        if st.batch.resume[1..].iter().any(|q| !q.is_empty())
            || st.batch.has_touches()
            || !cursor.walks[1].done
        {
            return Some(Kind::Far);
        }
        let others = &st.batch.others;
        let stamp = st.stamp(eviction);
        if others.deps != Some((st.session, stamp.eviction, stamp.grammar)) {
            return Some(Kind::Far);
        }
        let active = st.worktree();
        let left = others.current.is_some()
            || st.snapshot.as_ref().is_some_and(|s| {
                s.worktrees
                    .iter()
                    .any(|w| !w.prunable && Some(&w.key) != active && !others.done.contains(&w.key))
            });
        let set_aside = aside && st.batch.aside[1..].iter().any(|q| !q.is_empty());
        (left || set_aside).then_some(Kind::Far)
    }

    /// Whether the project that runs tiers 3–5 has work a new batch task
    /// would do now: files to extract, a walk to rebuild or to go on with,
    /// other worktrees left, touches.
    #[cfg(test)]
    pub(crate) fn runnable(&self) -> bool {
        self.batch_project()
            .is_some_and(|p| self.plan(&p).is_some())
    }

    /// Gives up a parked item: its saved state goes, and it goes back to
    /// the front of its queue (unless tier 1 took it over or the session
    /// ended).
    fn abandon(&self, parked: Parked) {
        let Parked {
            project,
            session,
            t,
            item,
            flag,
            ..
        } = parked;
        self.requeue(&project, session, t, item, &flag);
    }

    /// Puts an interrupted item back at the front of tier `t`'s queue.
    fn requeue(
        &self,
        project: &Arc<Project>,
        session: u64,
        t: usize,
        item: Item,
        flag: &Arc<AtomicBool>,
    ) {
        let mut st = lock(&project.state);
        // Not when tier 1 took it over (`focus_task`).
        if st.finish(session, &item, flag) && !flag.load(Ordering::Acquire) {
            st.batch.queued.insert(item.key());
            st.batch.resume[t].push_front(item);
        }
    }

    /// Sets aside an item of tier `t` (4 or 5) that a keepalive slot can't
    /// do (over `SLOT_MAX_BYTES`, a language pack's extraction longer than
    /// a slot, or a canonical-OID check still due after the switch came on,
    /// its extraction dropped: `check_keys`): it waits in the tier's set-aside list, in the order it
    /// was taken, until a batch runs with the switch off
    /// (`drain_aside`), and the tier goes on with the items behind it.
    fn set_aside(
        &self,
        project: &Arc<Project>,
        session: u64,
        t: usize,
        item: Item,
        flag: &Arc<AtomicBool>,
    ) {
        let mut st = lock(&project.state);
        if st.finish(session, &item, flag) && !flag.load(Ordering::Acquire) {
            st.batch.queued.insert(item.key());
            st.batch.aside[t].push_back(item);
        }
    }

    /// A parked item set aside: its saved state and its large-slot turn go.
    fn set_aside_parked(&self, parked: Parked) {
        let Parked {
            project,
            session,
            t,
            item,
            flag,
            ..
        } = parked;
        self.set_aside(&project, session, t, item, &flag);
    }

    /// The switch is off (`Grant::Batch`): the items set aside go back to
    /// the front of their tiers' queues, in the order they were set aside.
    fn drain_aside(&self, project: &Arc<Project>) {
        let mut st = lock(&project.state);
        let batch = &mut st.batch;
        for t in [1, 2] {
            while let Some(item) = batch.aside[t].pop_back() {
                batch.resume[t].push_front(item);
            }
        }
    }

    /// The liveness rule of `kick_batch`: no runnable batch work, or a full
    /// pool. False is a missed kick.
    #[cfg(test)]
    pub(crate) fn live(&self) -> bool {
        let running = lock(&self.batch).running;
        running >= self.batch_tasks || !self.runnable()
    }

    fn batch_project(&self) -> Option<Arc<Project>> {
        let key: Option<ProjectKey> = lock(&self.batch_project).clone();
        self.project(&key?)
    }

    /// The session's read side, with tier 3 and 4's cursor up to date;
    /// None when the project doesn't run tiers 3–5 now.
    fn step_ctx(&self, project: &Arc<Project>) -> Option<(Ctx, HintStamp)> {
        let eviction = project.evictions();
        let mut st = lock(&project.state);
        if st.base_for.is_none() || st.cancel.load(Ordering::Acquire) {
            return None;
        }
        self.ensure_cursor(&mut st, eviction);
        Some((ctx(&st), st.stamp(eviction)))
    }

    /// One batch of tier 3 (`background()`, doesn't yield). `own`: the
    /// task's parked item, which gives the large slot up to a large file.
    fn near_step(
        self: &Arc<Self>,
        project: &Arc<Project>,
        guard: &Arc<BackgroundGuard>,
        own: &mut Option<Parked>,
    ) -> Step {
        let Some((ctx, stamp)) = self.step_ctx(project) else {
            return Step::Idle;
        };
        // Files known to be missing go first.
        if !lock(&project.state).batch.resume[0].is_empty() {
            self.extract_batch(project, &ctx, 0, guard, Kind::Near, Grant::Batch, own);
            return Step::Worked;
        }
        if let Some(items) = self.gather(project, 0, stamp) {
            self.exists(project, &ctx, items, stamp);
            return Step::Worked;
        }
        Step::Idle
    }

    /// One batch of tiers 4 and 5 (`background_yielding()`), or one item
    /// in a keepalive slot (`Grant::Slot`). A parked item goes first.
    fn far_step(
        self: &Arc<Self>,
        project: &Arc<Project>,
        guard: &Arc<BackgroundGuard>,
        grant: Grant,
        own: &mut Option<Parked>,
    ) -> Step {
        let Some((ctx, stamp)) = self.step_ctx(project) else {
            return Step::Idle;
        };
        // A parked item over 256 KiB never goes on in a keepalive slot: the
        // slot takes the next item, the parked one kept meanwhile. Only
        // when that item stops in turn (a task keeps one saved state) is the
        // large one set aside, its state dropped.
        let held = match grant {
            Grant::Slot { .. } => own.take_if(|p| p.bytes() > SLOT_MAX_BYTES),
            Grant::Batch => {
                self.drain_aside(project);
                None
            }
        };
        let step = self.far_work(project, &ctx, stamp, guard, grant, own);
        match held {
            Some(p) if own.is_some() => {
                self.set_aside_parked(p);
                step
            }
            Some(p) => {
                *own = Some(p);
                // It waits for the switch to turn off: the task doesn't
                // stop (`boundary_bytes`).
                Step::Worked
            }
            None => step,
        }
    }

    /// `far_step`'s work, with the parked item it can go on with in `own`.
    fn far_work(
        self: &Arc<Self>,
        project: &Arc<Project>,
        ctx: &Ctx,
        stamp: HintStamp,
        guard: &Arc<BackgroundGuard>,
        grant: Grant,
        own: &mut Option<Parked>,
    ) -> Step {
        if let Some(t) = own.as_ref().map(|p| p.t) {
            self.extract_batch(project, ctx, t, guard, Kind::Far, grant, own);
            return Step::Worked;
        }
        // Files known to be missing go first, tier by tier.
        for t in [1, 2] {
            if !lock(&project.state).batch.resume[t].is_empty() {
                self.extract_batch(project, ctx, t, guard, Kind::Far, grant, own);
                return Step::Worked;
            }
        }
        // An existence check is one item, of size 0, in a slot.
        if let Some(items) = self.gather(project, 1, stamp) {
            self.exists(project, ctx, items, stamp);
            return Step::Worked;
        }
        if let Some(items) = self.gather_other(project, ctx, stamp) {
            self.exists(project, ctx, items, stamp);
            return Step::Worked;
        }
        // Everything is checked: mark the warm start's hits as used.
        let mut touches = lock(&project.state).batch.take_touches();
        if !touches.is_empty() {
            project.store.touch_pending(&mut touches);
            let mut st = lock(&project.state);
            if st.session == ctx.session {
                st.batch.put_back_touches(touches);
            }
            return Step::Worked;
        }
        // Only items set aside are left: the next boundary waits for the
        // switch to turn off (`boundary_bytes`); the task doesn't stop, as
        // nothing would start it again when the switch turns off.
        let aside = lock(&project.state).batch.aside[1..]
            .iter()
            .any(|q| !q.is_empty());
        if aside {
            return Step::Worked;
        }
        Step::Idle
    }

    /// Rebuilds tier 3 and 4's cursor when the map it walks, the
    /// sub-projects, the walks' eviction generation or tier 3's scope
    /// changed (a focus change alone does not).
    fn ensure_cursor(&self, st: &mut State, eviction: u64) {
        let deps = st.cursor_deps(eviction);
        let scope = subproject(st);
        if st
            .batch
            .cursor
            .as_ref()
            .is_some_and(|c| c.deps == deps && c.scope == scope)
        {
            return;
        }
        bump(&self.stats.cursor_builds, 1);
        let files = st.map.base.files().clone();
        let range = match &scope {
            Scope::Dir(dir) => files.range_under(dir),
            Scope::Whole => 0..files.len(),
            Scope::Empty => 0..0,
        };
        st.batch.cursor = Some(Cursor {
            deps,
            map: st.map.clone(),
            scope,
            range,
            walks: [Walk::default(); 2],
        });
        for queue in st.batch.resume[..2]
            .iter_mut()
            .chain(&mut st.batch.aside[..2])
        {
            queue.clear();
        }
        st.batch.queued.clear();
        let in_flight: Vec<_> = st.in_flight.keys().cloned().collect();
        st.batch.queued.extend(in_flight);
    }

    /// Up to 500 files of tier 3 (`t` 0) or 4 (`t` 1) whose hint does not
    /// match; None when the tier is walked through.
    fn gather(&self, project: &Arc<Project>, t: usize, stamp: HintStamp) -> Option<Vec<Item>> {
        let disabled = lock(&self.langs).disabled.clone();
        let mut st = lock(&project.state);
        let State {
            batch, in_flight, ..
        } = &mut *st;
        let cursor = batch.cursor.as_mut()?;
        if cursor.walks[t].done {
            return None;
        }
        let tier = if t == 0 {
            Tier::Subproject
        } else {
            Tier::Worktree
        };
        let mut items = Vec::new();
        let mut looked = 0;
        let mut consider = |eff: crate::map::Effective, items: &mut Vec<Item>| {
            if disabled.contains(eff.lang.name()) || cursor_hinted(&cursor.map, &eff, stamp) {
                return;
            }
            let key = (eff.oid, eff.lang.name().clone());
            if batch.queued.contains(&key) || in_flight.contains_key(&key) {
                return;
            }
            batch.queued.insert(key);
            items.push(Item {
                path: eff.path,
                oid: eff.oid,
                lang: eff.lang,
                source: Source::Worktree,
                tier,
            });
        };
        let overlay_len = cursor.map.overlay.len();
        let range = cursor.range.clone();
        let total = cursor.map.base.files().len();
        let base_len = if t == 0 {
            range.len()
        } else {
            total - range.len()
        };
        while items.len() < EXISTS_BATCH && looked < SCAN_LIMIT {
            let walk = &mut cursor.walks[t];
            if walk.overlay < overlay_len {
                let i = walk.overlay;
                walk.overlay += 1;
                looked += 1;
                let (path, _) = &cursor.map.overlay.entries()[i];
                if cursor.in_subproject(path) != (t == 0) {
                    continue;
                }
                if let Some(eff) = cursor.map.overlay_effective(i) {
                    consider(eff, &mut items);
                }
            } else if walk.base < base_len {
                let n = walk.base;
                walk.base += 1;
                looked += 1;
                let i = if t == 0 {
                    range.start + n
                } else if n < range.start {
                    n
                } else {
                    n + range.len()
                };
                if let Some(eff) = cursor.map.base_effective(i) {
                    consider(eff, &mut items);
                }
            } else {
                walk.done = true;
                break;
            }
        }
        if items.is_empty() && cursor.walks[t].done {
            return None;
        }
        Some(items)
    }

    /// Tier 5: up to 500 files of the next other worktree; None when every
    /// other worktree is done.
    fn gather_other(
        &self,
        project: &Arc<Project>,
        ctx: &Ctx,
        stamp: HintStamp,
    ) -> Option<Vec<Item>> {
        loop {
            let next = {
                let mut st = lock(&project.state);
                if !st.live(ctx.session) {
                    return None;
                }
                let deps = Some((st.session, stamp.eviction, stamp.grammar));
                if st.batch.others.deps != deps {
                    st.batch.others = super::state::Others {
                        deps,
                        ..Default::default()
                    };
                    st.batch.resume[2].clear();
                    st.batch.aside[2].clear();
                }
                if st.batch.others.current.is_some() {
                    None
                } else {
                    let snapshot = st.snapshot.clone()?;
                    let active = st.worktree().cloned();
                    let ProjectKey::Git { common_dir } = &project.key else {
                        return None;
                    };
                    let w = snapshot.worktrees.iter().find(|w| {
                        !w.prunable
                            && Some(&w.key) != active.as_ref()
                            && !st.batch.others.done.contains(&w.key)
                    })?;
                    Some((
                        w.key.clone(),
                        Tree {
                            common_dir: common_dir.clone(),
                            git_dir: w.key.0.clone(),
                            root: w.root.clone(),
                        },
                    ))
                }
            };
            if let Some((key, tree)) = next {
                // Read that worktree's index; it is dropped once walked.
                let loaded = SafeRoot::open(&tree.root)
                    .map_err(|e| e.to_string())
                    .and_then(|root| {
                        self.git
                            .tracked(&tree, &ctx.cancel)
                            .map(|entries| (root, entries))
                            .map_err(|e| e.to_string())
                    });
                let mut st = lock(&project.state);
                if !st.live(ctx.session) {
                    return None;
                }
                match loaded {
                    Ok((root, entries)) => {
                        let other = Arc::new(OtherTree {
                            key: key.clone(),
                            root,
                        });
                        st.batch.others.current = Some(OtherWalk {
                            tree: other,
                            entries: entries.into(),
                            pos: 0,
                        });
                    }
                    Err(error) => {
                        log(format_args!(
                            "reading worktree {}: {error}",
                            tree.root.display()
                        ));
                        st.batch.others.done.insert(key);
                    }
                }
                continue;
            }
            let disabled = lock(&self.langs).disabled.clone();
            let mut st = lock(&project.state);
            if st.batch.others.deps != Some((st.session, stamp.eviction, stamp.grammar)) {
                continue;
            }
            // Languages as the map knows them (its provenance), like tiers
            // 3 and 4.
            let map = st.map.clone();
            let table = map.base.table().clone();
            let mut lookup = LangLookup::new(&table, &*self.packs);
            let State {
                batch, in_flight, ..
            } = &mut *st;
            let walk = batch.others.current.as_mut()?;
            let mut items = Vec::new();
            let mut looked = 0;
            while walk.pos < walk.entries.len() && items.len() < EXISTS_BATCH && looked < SCAN_LIMIT
            {
                let (path, oid) = &walk.entries[walk.pos];
                walk.pos += 1;
                looked += 1;
                let code = lookup.code(path);
                let Some(lang) = code
                    .checked_sub(1)
                    .and_then(|i| table.langs().get(usize::from(i)))
                else {
                    continue;
                };
                if disabled.contains(lang.name()) {
                    continue;
                }
                let key = (*oid, lang.name().clone());
                // Known stored: the current worktree has it indexed, or this
                // session already found it for another worktree.
                if batch.others.confirmed.contains(oid, lang.name())
                    || map.hinted(oid, &lang.name().0, stamp)
                    || batch.queued.contains(&key)
                    || in_flight.contains_key(&key)
                {
                    continue;
                }
                batch.queued.insert(key);
                items.push(Item {
                    path: path.clone(),
                    oid: *oid,
                    lang: lang.clone(),
                    source: Source::Other(walk.tree.clone()),
                    tier: Tier::Other,
                });
            }
            if walk.pos >= walk.entries.len() {
                let key = walk.tree.key.clone();
                batch.others.current = None;
                batch.others.done.insert(key);
            }
            if items.is_empty() && batch.others.current.is_none() {
                continue;
            }
            return Some(items);
        }
    }

    /// The existence check of gathered items: hits set their hints (and
    /// those of every path with the same content), misses wait in their
    /// tier's queue.
    fn exists(&self, project: &Arc<Project>, ctx: &Ctx, items: Vec<Item>, stamp: HintStamp) {
        // Read before the queries: the touches are queued under it.
        let eviction = project.store.evictions();
        let mut groups: Vec<(Version, Vec<Item>)> = Vec::new();
        for item in items {
            let version = Version::of(&item.lang);
            match groups.iter_mut().find(|(v, _)| *v == version) {
                Some((_, group)) => group.push(item),
                None => groups.push((version, vec![item])),
            }
        }
        let mut found = Vec::new();
        let mut hits = Vec::new();
        let mut statuses = Vec::new();
        for (version, group) in &groups {
            let oids: Vec<BlobId> = group.iter().map(|i| i.oid).collect();
            bump(&self.stats.exists_checks, oids.len().div_ceil(EXISTS_BATCH));
            let hit = project.store.exists(version, &oids).unwrap_or_default();
            for f in &hit {
                hits.push((f.oid, version.lang.clone()));
                statuses.push(f.status);
            }
            found.extend(hit);
        }
        let table = lock(&self.langs).table.clone();
        let mut st = lock(&project.state);
        if !st.live(ctx.session) {
            return;
        }
        // From a batch task, which goes on with them: the wake only makes
        // it step once more before it stops.
        let wake = st.batch.queue_touches(eviction, &found);
        for ((oid, lang), status) in hits.iter().zip(&statuses) {
            // A hit stamps hints only for the version the map and the current
            // language table both have, and with the map's own stamp: a hit
            // of an older grammar never makes a newer map's path look stored.
            if stamp.grammar != st.map.provenance.grammar
                || !same_version(&table, &st.map, lang, &groups)
            {
                continue;
            }
            st.hint_paths(oid, &lang.0, stamp, *status);
        }
        for (_, group) in groups {
            for item in group {
                let key = item.key();
                if hits.contains(&key) {
                    st.batch.queued.remove(&key);
                    if item.tier == Tier::Other {
                        st.batch
                            .others
                            .confirmed
                            .insert(&item.oid, item.lang.name());
                    }
                    continue;
                }
                let Some(t) = item.tier.batch() else {
                    continue;
                };
                st.batch.resume[t].push_back(item);
            }
        }
        drop(st);
        self.wake(wake);
    }

    /// Extracts files of tier `t`'s queue until the batch's limits, or one
    /// file in a keepalive slot (`Grant::Slot`). `own` is the task's parked
    /// item: a far batch goes on with it first and leaves the next one
    /// there; a near batch's large file takes the large slot from it.
    #[allow(clippy::too_many_arguments, reason = "one batch's whole context")]
    fn extract_batch(
        self: &Arc<Self>,
        project: &Arc<Project>,
        ctx: &Ctx,
        t: usize,
        guard: &Arc<BackgroundGuard>,
        kind: Kind,
        grant: Grant,
        own: &mut Option<Parked>,
    ) {
        let clock = self.watchdog.clock().clone();
        let start = clock.now();
        let one = matches!(grant, Grant::Slot { .. });
        let (mut files, mut bytes) = (0, 0);
        let mut done = Vec::new();
        while files < BATCH_FILES && bytes < BATCH_BYTES && clock.now() - start < BATCH_TIME {
            if guard.pause_requested() || ctx.cancel.load(Ordering::Acquire) {
                break;
            }
            // The switch came on during a batch: its next boundary waits.
            if kind == Kind::Far && !one && guard.yield_requested() {
                break;
            }
            let outcome = match kind {
                Kind::Near => {
                    let Some((item, flag)) = self.take(project, ctx, t) else {
                        break;
                    };
                    let outcome = match self.read_item(ctx, &item, Mode::Batch, true, &flag) {
                        Err(outcome) => outcome,
                        Ok(ReadItem {
                            bytes,
                            key,
                            check,
                            canonical,
                        }) => {
                            if bytes.len() > LARGE_FILE
                                && let Some(p) = own.take_if(|p| p.large.is_some())
                            {
                                // Tier 3 doesn't wait for a parked large
                                // file: it starts over later.
                                self.abandon(p);
                            }
                            self.extract_bytes(
                                ctx,
                                &item,
                                (key, check, canonical),
                                bytes,
                                Mode::Batch,
                                Some(guard),
                                &flag,
                            )
                        }
                    };
                    (outcome, item, flag)
                }
                Kind::Far => {
                    let parked = match own.take() {
                        Some(p) => p,
                        None => {
                            let Some((item, flag)) = self.take(project, ctx, t) else {
                                break;
                            };
                            // A keepalive slot converts its one file now,
                            // inside the slot and before the extraction.
                            match self.read_item(ctx, &item, Mode::Batch, !one, &flag) {
                                Err(outcome) => {
                                    self.settle_outcome(
                                        project, ctx, t, outcome, item, flag, &mut files,
                                        &mut bytes, &mut done, own,
                                    );
                                    if one {
                                        break;
                                    }
                                    continue;
                                }
                                Ok(read) if one && read.bytes.len() as u64 > SLOT_MAX_BYTES => {
                                    // Never in a keepalive slot: set aside for
                                    // the switch to turn off; the slot goes on
                                    // with the next item. What was read counts
                                    // toward the batch's bytes.
                                    bytes += read.bytes.len();
                                    self.set_aside(project, ctx.session, t, item, &flag);
                                    continue;
                                }
                                Ok(read) => Parked {
                                    project: project.clone(),
                                    session: ctx.session,
                                    t,
                                    item,
                                    flag,
                                    key: read.key,
                                    check: read.check,
                                    canonical: read.canonical,
                                    bytes: read.bytes,
                                    resume: Arc::new(Mutex::new(None)),
                                    large: None,
                                },
                            }
                        }
                    };
                    let (item, flag) = (parked.item.clone(), parked.flag.clone());
                    (self.extract_far(ctx, parked, guard), item, flag)
                }
            };
            let (outcome, item, flag) = outcome;
            let stop = self.settle_outcome(
                project, ctx, t, outcome, item, flag, &mut files, &mut bytes, &mut done, own,
            );
            if stop || one {
                break;
            }
        }
        #[cfg(test)]
        lock(&self.batch_log).push(files);
        // Tier 3 never yields. A yielding tier converts only in a batch
        // under `Grant::Batch` with the switch still off: no index parse
        // after a keepalive slot's file nor once the switch is on.
        let convert = kind == Kind::Near || (!one && !guard.yield_requested());
        let done = self.check_keys(project, ctx, t, done, convert);
        self.commit(project, ctx, done, true);
    }

    /// 「规范内容 OID 的批量核对」 for a batch about to be committed: the
    /// items read with another OID than queued (`Done::check`) get their
    /// key from one `canonical_oids` per at most 256 items and 2 MiB, on
    /// this boundary's side of the batch (the batch's own work, as its
    /// commit is). The queued OID becomes the key when the built-in
    /// conversions turn the bytes into it; otherwise (changed since queued,
    /// a filter) the raw OID stays. An item whose conversion failed is as
    /// one that could not be read: not written, released. Items the commit
    /// would drop (cancelled, taken over by tier 1) are not checked, and
    /// once the session ends nothing more is.
    ///
    /// Without `convert` (tier 4 or 5 in a keepalive slot, or the switch
    /// came on during the batch) no conversion runs: those items are work
    /// a slot can't do, set aside with their extraction dropped, to be
    /// read and extracted again once the switch is off (「交互让路」's
    /// set-aside list); the rest of the batch is committed.
    fn check_keys(
        &self,
        project: &Arc<Project>,
        ctx: &Ctx,
        t: usize,
        mut done: Vec<Done>,
        convert: bool,
    ) -> Vec<Done> {
        let Some(tree) = &ctx.tree else {
            return done;
        };
        if !convert {
            let mut kept = Vec::with_capacity(done.len());
            for d in done {
                if d.check.is_some() && !d.flag.load(Ordering::Acquire) {
                    self.set_aside(project, ctx.session, t, d.item, &d.flag);
                } else {
                    kept.push(d);
                }
            }
            return kept;
        }
        let mut checks: Pending<usize, &[u8]> = Pending::new();
        let mut results = Vec::new();
        for (i, d) in done.iter().enumerate() {
            let Some(bytes) = &d.check else {
                continue;
            };
            if d.flag.load(Ordering::Acquire) {
                continue;
            }
            if checks.full_for(bytes.len()) {
                if ctx.cancel.load(Ordering::Acquire) {
                    break;
                }
                results.extend(checks.flush(&*self.git, tree));
            }
            checks.push(i, d.item.path.clone(), bytes.as_slice());
        }
        if !ctx.cancel.load(Ordering::Acquire) {
            results.extend(checks.flush(&*self.git, tree));
        }
        let mut failed = vec![false; done.len()];
        for (i, canonical) in results {
            let d = &mut done[i];
            match canonical {
                Canonical::Oid(oid) if oid == d.item.oid => {
                    // The conversion made them that content: no byte
                    // offsets for views (「字节偏移」).
                    d.key = oid;
                    d.meta.canonical = false;
                }
                Canonical::Oid(_) | Canonical::Filtered => {}
                Canonical::Failed => failed[i] = true,
            }
        }
        let mut kept = Vec::with_capacity(done.len());
        for (mut d, failed) in done.into_iter().zip(failed) {
            if failed {
                self.release(project, ctx, &d.item, &d.flag);
            } else {
                d.check = None;
                kept.push(d);
            }
        }
        kept
    }

    /// Books one item's outcome in a batch; true when the batch is to end
    /// (paused, parked).
    #[allow(clippy::too_many_arguments, reason = "the batch's running totals")]
    fn settle_outcome(
        &self,
        project: &Arc<Project>,
        ctx: &Ctx,
        t: usize,
        outcome: Outcome,
        item: Item,
        flag: Arc<AtomicBool>,
        files: &mut usize,
        bytes: &mut usize,
        done: &mut Vec<Done>,
        own: &mut Option<Parked>,
    ) -> bool {
        match outcome {
            Outcome::Done(d, len) => {
                *files += 1;
                *bytes += len;
                done.push(d);
                false
            }
            Outcome::Paused => {
                // Still queued: back to the front.
                self.requeue(project, ctx.session, t, item, &flag);
                true
            }
            Outcome::Skip => {
                self.release(project, ctx, &item, &flag);
                false
            }
            Outcome::Parked(p) => {
                *own = Some(p);
                true
            }
            Outcome::Aside => {
                self.set_aside(project, ctx.session, t, item, &flag);
                true
            }
        }
    }

    /// A yielding tier's extraction of a read item: from where it stopped,
    /// with the deadline's budget left (the deadline counts only the time
    /// it runs), stopping again at a slice boundary when asked to yield.
    fn extract_far(&self, ctx: &Ctx, mut p: Parked, guard: &Arc<BackgroundGuard>) -> Outcome {
        if p.large.as_ref().is_some_and(|turn| !turn.unpark()) {
            // Tier 3 took the slot over while it was parked: its saved
            // state is gone; it starts over once it has the slot again.
            p.large = None;
            *lock(&p.resume) = None;
        }
        if p.bytes.len() > LARGE_FILE && p.large.is_none() {
            // A large file's syntax tree is most of a batch's memory: one
            // batch task at a time extracts one. Given up on a pause or a
            // cancel; with the switch on it waits, read, at the boundary.
            let stop = || {
                guard.pause_requested()
                    || ctx.cancel.load(Ordering::Acquire)
                    || guard.yield_requested()
            };
            match self.large.acquire(stop, false, || {}) {
                Some(turn) => p.large = Some(turn),
                None if guard.pause_requested() => return Outcome::Paused,
                None if guard.yield_requested() && !ctx.cancel.load(Ordering::Acquire) => {
                    return Outcome::Parked(p);
                }
                None => return Outcome::Skip,
            }
        }
        let budget = lock(&p.resume)
            .as_ref()
            .map_or(BACKGROUND_DEADLINE, |r| r.budget);
        let at = self.watchdog.clock().now() + budget;
        let (paused, yields) = (guard.clone(), guard.clone());
        #[cfg(test)]
        let hook = self.yield_hook.clone();
        let deadline = Deadline::at(&self.watchdog, at, BACKGROUND_MAX_LEN)
            .with_pause(Arc::new(move || paused.pause_requested()))
            .yielding(
                Arc::new(move || {
                    #[cfg(test)]
                    if let Some(hook) = lock(&hook).as_mut() {
                        hook();
                    }
                    yields.yield_requested()
                }),
                p.resume.clone(),
            );
        let extract = lock(&self.extract).clone();
        bump(&self.stats.extractions, 1);
        let extraction = extract(&p.item.lang, &p.bytes, &deadline, &p.flag);
        match extraction.status {
            // A core language kept its state (tier 3 may take a large
            // file's slot over meanwhile); a language pack's work is given
            // up and set aside, to be done again with the switch off.
            ExtractStatus::Parked if lock(&p.resume).is_some() => {
                if let Some(turn) = &p.large {
                    turn.park(&p.resume);
                }
                Outcome::Parked(p)
            }
            ExtractStatus::Parked => Outcome::Aside,
            ExtractStatus::Cancelled => {
                if guard.pause_requested() && !ctx.cancel.load(Ordering::Acquire) {
                    Outcome::Paused
                } else {
                    Outcome::Skip
                }
            }
            ExtractStatus::Unsupported => {
                lock(&self.langs)
                    .disabled
                    .insert(p.item.lang.name().clone());
                Outcome::Skip
            }
            _ => {
                let len = p.bytes.len();
                Outcome::Done(
                    Done {
                        item: p.item,
                        key: p.key,
                        check: p.check.then_some(p.bytes),
                        extraction,
                        flag: p.flag,
                        meta: UnitMeta::whole(p.canonical),
                    },
                    len,
                )
            }
        }
    }

    /// Takes the next item of tier `t`'s queue and registers it in flight.
    /// Items stored since their existence check (by tier 1 or 2, or for
    /// another worktree) are dropped.
    fn take(&self, project: &Arc<Project>, ctx: &Ctx, t: usize) -> Option<(Item, Arc<AtomicBool>)> {
        let eviction = project.evictions();
        let mut st = lock(&project.state);
        if !st.live(ctx.session) {
            return None;
        }
        let stamp = st.stamp(eviction);
        loop {
            let item = st.batch.resume[t].pop_front()?;
            let key = item.key();
            if st.in_flight.contains_key(&key)
                || st
                    .batch
                    .others
                    .confirmed
                    .contains(&item.oid, item.lang.name())
                || st.map.hinted(&item.oid, &item.lang.name().0, stamp)
            {
                st.batch.queued.remove(&key);
                continue;
            }
            let flag = Arc::new(AtomicBool::new(ctx.cancel.load(Ordering::Acquire)));
            let owner = Owner {
                flag: flag.clone(),
                tier: item.tier,
            };
            st.in_flight.insert(key, owner);
            st.batch.active[t] += 1;
            return Some((item, flag));
        }
    }

    /// Forgets an item that gave nothing to write.
    fn release(&self, project: &Arc<Project>, ctx: &Ctx, item: &Item, flag: &Arc<AtomicBool>) {
        lock(&project.state).finish(ctx.session, item, flag);
    }

    /// Reads and extracts one item (`read_item`, `extract_bytes`).
    fn extract_item(
        &self,
        ctx: &Ctx,
        item: &Item,
        mode: Mode,
        guard: Option<&Arc<BackgroundGuard>>,
        cancel: &Arc<AtomicBool>,
    ) -> Outcome {
        match self.read_item(ctx, item, mode, false, cancel) {
            Ok(ReadItem {
                bytes,
                key,
                check,
                canonical,
            }) => self.extract_bytes(
                ctx,
                item,
                (key, check, canonical),
                bytes,
                mode,
                guard,
                cancel,
            ),
            Err(outcome) => outcome,
        }
    }

    /// Reads one item: its bytes and their key, the OID of the bytes read:
    /// the raw bytes', or the queued OID when the built-in conversions turn
    /// them into it (「存储表」). With `defer` (a batch tier's batch under
    /// `Grant::Batch`), worktree bytes whose raw OID is not the queued one
    /// are left to the batch's `canonical_oids` before the commit (true in
    /// the result: the key is the raw OID until then). Otherwise this one
    /// file is converted now (`canonical_oid`): tiers 1 and 2, and a
    /// keepalive slot, whose one file is converted inside the slot and
    /// before its extraction; there a failed conversion is as an unreadable
    /// file (`Skip`), as in a batch. A slot's file over `SLOT_MAX_BYTES` is
    /// not converted: the slot sets it aside at once. Err: what to do without extracting
    /// (`TooLarge`, gone).
    #[allow(
        clippy::result_large_err,
        reason = "the outcome is used at once, once per file read"
    )]
    fn read_item(
        &self,
        ctx: &Ctx,
        item: &Item,
        mode: Mode,
        defer: bool,
        cancel: &Arc<AtomicBool>,
    ) -> Result<ReadItem, Outcome> {
        let max = match mode {
            Mode::Interactive => INTERACTIVE_MAX_LEN,
            Mode::Background | Mode::Batch => BACKGROUND_MAX_LEN,
        };
        let too_large = |item: &Item| {
            Outcome::Done(
                Done {
                    item: item.clone(),
                    key: item.oid,
                    check: None,
                    extraction: Extraction {
                        status: ExtractStatus::TooLarge,
                        defs: Vec::new(),
                        refs: Vec::new(),
                    },
                    flag: cancel.clone(),
                    meta: UnitMeta::whole(true),
                },
                0,
            )
        };
        let read = |root: &SafeRoot| {
            let rel = SafeRel::new(item.path.as_bytes())?;
            match root.read(rel, max as u64) {
                Ok(Read::File { bytes, .. }) => Some(Ok(bytes)),
                Ok(Read::TooLarge { .. }) => Some(Err(())),
                _ => None,
            }
        };
        let bytes: Vec<u8> = match &item.source {
            Source::Worktree => match ctx.root.as_deref().and_then(read) {
                Some(Ok(bytes)) => bytes,
                Some(Err(())) => return Err(too_large(item)),
                None => return Err(Outcome::Skip),
            },
            Source::Other(other) => match read(&other.root) {
                Some(Ok(bytes)) => bytes,
                Some(Err(())) => return Err(too_large(item)),
                None => return Err(Outcome::Skip),
            },
            Source::Object => {
                let Some(tree) = &ctx.tree else {
                    return Err(Outcome::Skip);
                };
                match self.git.blob(tree, item.oid, max as u64) {
                    Ok(bytes) => bytes.to_vec(),
                    Err(lectern_git::GitError::TooLarge { .. }) => return Err(too_large(item)),
                    Err(_) => return Err(Outcome::Skip),
                }
            }
        };
        let raw = hash_blob(hash_kind(&item.oid), &bytes);
        let read = |bytes, key, check| ReadItem {
            bytes,
            key,
            check,
            canonical: key == raw,
        };
        if raw == item.oid {
            return Ok(read(bytes, raw, false));
        }
        let key = match &item.source {
            // Another worktree's file that differs from its index waits
            // until that worktree is the current one.
            Source::Other(_) => return Err(Outcome::Skip),
            Source::Object => raw,
            Source::Worktree => match &ctx.tree {
                // A slot's file over `SLOT_MAX_BYTES` is set aside right
                // after this read: not converted in the slot either.
                Some(_)
                    if mode == Mode::Batch && (defer || bytes.len() as u64 > SLOT_MAX_BYTES) =>
                {
                    return Ok(read(bytes, raw, true));
                }
                Some(tree) => match self.git.canonical_oid(tree, &item.path, &bytes) {
                    Ok(Some(canonical)) if canonical == item.oid => item.oid,
                    Err(error) if mode == Mode::Batch => {
                        log(format_args!("converting {} failed: {error}", item.path));
                        return Err(Outcome::Skip);
                    }
                    _ => raw,
                },
                None => raw,
            },
        };
        Ok(read(bytes, key, false))
    }

    /// Extracts one read item, without yielding: tiers 1 and 2, and tier 3
    /// (`Mode::Batch`, interrupted by the pause). `(key, check)` as
    /// [`read_item`](Self::read_item) gave them: with `check` the bytes go
    /// on to the batch's `canonical_oids` (`Done::check`).
    #[allow(
        clippy::too_many_arguments,
        reason = "an item, its bytes and their limits"
    )]
    fn extract_bytes(
        &self,
        ctx: &Ctx,
        item: &Item,
        (key, check, canonical): (BlobId, bool, bool),
        bytes: Vec<u8>,
        mode: Mode,
        guard: Option<&Arc<BackgroundGuard>>,
        cancel: &Arc<AtomicBool>,
    ) -> Outcome {
        // A large file's syntax tree is most of a batch's memory (a file near
        // 1 MiB about 31 MiB): one batch task at a time extracts one, so the
        // temporary memory stays within its share with several batch
        // threads. The wait gives up on a pause or a cancel. Tier 3 doesn't
        // yield: it takes the slot over from a yielding task parked with it.
        let _large = if mode == Mode::Batch && bytes.len() > LARGE_FILE {
            let stop =
                || guard.is_some_and(|g| g.pause_requested()) || ctx.cancel.load(Ordering::Acquire);
            match self.large.acquire(stop, true, || self.wake_waiters()) {
                Some(slot) => Some(slot),
                None if guard.is_some_and(|g| g.pause_requested()) => return Outcome::Paused,
                None => return Outcome::Skip,
            }
        } else {
            None
        };
        let deadline = match mode {
            Mode::Interactive => Deadline::interactive(&self.watchdog),
            Mode::Background => Deadline::background(&self.watchdog),
            Mode::Batch => {
                let deadline = Deadline::background(&self.watchdog);
                match guard {
                    Some(guard) => {
                        let guard = guard.clone();
                        deadline.with_pause(Arc::new(move || guard.pause_requested()))
                    }
                    None => deadline,
                }
            }
        };
        let extract = lock(&self.extract).clone();
        bump(&self.stats.extractions, 1);
        let extraction = extract(&item.lang, &bytes, &deadline, cancel);
        match extraction.status {
            ExtractStatus::Cancelled => {
                let paused = guard.is_some_and(|g| g.pause_requested())
                    && !ctx.cancel.load(Ordering::Acquire);
                if paused {
                    Outcome::Paused
                } else {
                    Outcome::Skip
                }
            }
            ExtractStatus::Unsupported => {
                lock(&self.langs).disabled.insert(item.lang.name().clone());
                Outcome::Skip
            }
            // Without a yielding deadline nothing parks.
            ExtractStatus::Parked => Outcome::Skip,
            _ => {
                let len = bytes.len();
                Outcome::Done(
                    Done {
                        item: item.clone(),
                        key,
                        check: check.then_some(bytes),
                        extraction,
                        flag: cancel.clone(),
                        meta: UnitMeta::whole(canonical),
                    },
                    len,
                )
            }
        }
    }

    /// Writes extractions in one transaction, after checking each was made
    /// with the current grammar; then sets the hints of every path with that
    /// content.
    ///
    /// A key already stored is written again only when the extraction
    /// outranks the stored unit (`outranks`: `Done` over `TimedOut` or
    /// `TooLarge`), whichever tier writes; an unfinished extraction never
    /// replaces anything. The check runs under the project's commit lock,
    /// which every store write takes, so a unit written by another commit
    /// between the extraction and this one is seen. A replace is counted
    /// by the cache as an eviction and is not taken as the quota's: the
    /// transaction's own bump is subtracted (`Project::replaced`), so it
    /// does not set `Batch::full`.
    ///
    /// True when every item is now stored (written, or found stored).
    fn commit(&self, project: &Arc<Project>, ctx: &Ctx, done: Vec<Done>, batch: bool) -> bool {
        if done.is_empty() {
            return true;
        }
        let table = lock(&self.langs).table.clone();
        let live = |d: &Done| {
            !ctx.cancel.load(Ordering::Acquire)
                && !d.flag.load(Ordering::Acquire)
                && table
                    .get(&d.item.lang.name().0)
                    .is_some_and(|l| Version::of(l) == Version::of(&d.item.lang))
        };
        // The touches keep the store's own eviction generation, the hints
        // the one without our replaces.
        let (before_raw, before) = (project.store.evictions(), project.evictions());
        let mut groups: Vec<(Version, Vec<BlobId>)> = Vec::new();
        for d in done.iter().filter(|d| live(d)) {
            let version = Version::of(&d.item.lang);
            match groups.iter_mut().find(|(v, _)| *v == version) {
                Some((_, keys)) => keys.push(d.key),
                None => groups.push((version, vec![d.key])),
            }
        }
        let mut stored = Vec::new();
        let mut written = Vec::new();
        let mut hits = Vec::new();
        let (eviction, committed, after) = {
            let _turn = lock(&project.commits);
            // Keys stored since the extraction: one query per version,
            // under the lock every store write takes, so no commit lands
            // between this check and the write.
            let mut found: Vec<(Version, Found)> = Vec::new();
            for (version, keys) in &groups {
                bump(&self.stats.exists_checks, 1);
                for f in project.store.exists(version, keys).unwrap_or_default() {
                    found.push((version.clone(), f));
                }
            }
            let eviction = project.store.evictions();
            let mut write = project.store.write();
            let mut replaces = false;
            for d in done.iter().filter(|d| live(d)) {
                let version = Version::of(&d.item.lang);
                let stored_as = found.iter().find(|(v, f)| *v == version && f.oid == d.key);
                if let Some((_, f)) =
                    stored_as.filter(|(_, f)| !outranks(d.extraction.status, f.status))
                {
                    hits.push(*f);
                    stored.push((d.key, version, d.item.tier, f.status));
                    continue;
                }
                if write.add_unit(d.key, &version, &d.extraction, d.meta) {
                    replaces |= stored_as.is_some();
                    written.push((d.key, version, d.item.tier, d.extraction.status));
                }
            }
            if replaces {
                project.begin_replace();
            }
            let committed = !write.is_empty() && matches!(write.commit(), Written::Committed);
            let after = project.store.evictions();
            // Under the lock nobody else changes `replaced`.
            let replaced = project.replaced.load(Ordering::Acquire);
            // Our own replace's one bump (one per transaction): hints and
            // cursors do not count it. Should the same transaction also
            // evict for the quota, that eviction is not seen either; its
            // units are extracted again when used or on the next open.
            let own = committed && replaces && after == eviction + 1;
            if own {
                project.replaced.fetch_add(1, Ordering::SeqCst);
            }
            if replaces {
                project.end_replace();
            }
            (
                eviction.saturating_sub(replaced),
                committed,
                after.saturating_sub(replaced + u64::from(own)),
            )
        };
        if !committed {
            written.clear();
        }
        let settled = stored.len() + written.len() == done.len();
        let mut wakes = Vec::new();
        let mut st = lock(&project.state);
        if st.session == ctx.session {
            if committed {
                st.gens.writes += 1;
                if batch && after != eviction && st.batch.full.is_none() {
                    // Our own write evicted units: the namespace is at its
                    // quota. From now on an eviction does not restart the
                    // batch tiers' walks, or they would extract what they
                    // evicted and evict again; each file is extracted at
                    // most once per session.
                    let walked = st
                        .batch
                        .cursor
                        .as_ref()
                        .map_or(eviction, |c| c.deps.eviction);
                    st.batch.full = Some(walked);
                }
            }
            wakes.push(st.batch.queue_touches(before_raw, &hits));
            // The units just written are the newest: they outlive the
            // eviction they caused.
            let sets = [(stored, st.stamp(before)), (written, st.stamp(after))];
            for (units, stamp) in sets {
                for (key, version, tier, status) in units {
                    if !current(&table, &st.map, &version) {
                        continue;
                    }
                    if tier == Tier::Other {
                        st.batch.others.confirmed.insert(&key, &version.lang);
                    }
                    st.hint_paths(&key, &version.lang.0, stamp, status);
                }
            }
        }
        for d in &done {
            st.finish(ctx.session, &d.item, &d.flag);
        }
        drop(st);
        if committed && let Some(me) = self.arc() {
            // A written batch makes a held name table behind.
            me.names_changed(project);
        }
        if committed && !batch {
            // A write from outside the batch tasks (tiers 1 and 2, a
            // query's search) may have evicted, which starts the walks over.
            wakes.push(Wake::wrote());
        }
        for wake in wakes {
            self.wake(wake);
        }
        settled
    }

    /// Writes a query's extractions in a `Background` task, as tier 2 would:
    /// a key stored meanwhile is not written again, the hints of every path
    /// with that content are set once committed. Nothing for an ended
    /// session.
    pub(crate) fn store_scanned(
        &self,
        project: &Arc<Project>,
        session: u64,
        scanned: Vec<Scanned>,
    ) {
        if scanned.is_empty() {
            return;
        }
        let Some(me) = self.arc() else {
            return;
        };
        let p = project.clone();
        self.host
            .spawn_busy("store", Priority::Background, move || {
                me.commit_scanned(&p, session, scanned);
            });
    }

    /// [`store_scanned`](Self::store_scanned)'s write, on this thread: one
    /// transaction, then the hints, as a batch commits. Blocking (SQLite).
    pub(crate) fn commit_scanned(
        &self,
        project: &Arc<Project>,
        session: u64,
        scanned: Vec<Scanned>,
    ) {
        let ctx = {
            let st = lock(&project.state);
            if !st.live(session) {
                return;
            }
            ctx(&st)
        };
        let done = scanned
            .into_iter()
            .map(|s| Done {
                item: Item {
                    path: s.path,
                    oid: s.oid,
                    lang: s.lang,
                    source: Source::Worktree,
                    tier: Tier::Review,
                },
                key: s.oid,
                // The query read the bytes as `s.oid`'s content: checked.
                check: None,
                extraction: s.extraction,
                flag: Arc::new(AtomicBool::new(false)),
                meta: UnitMeta::whole(s.canonical),
            })
            .collect();
        self.commit(project, &ctx, done, false);
    }

    /// Queues the focus's files that are not indexed for tiers 1 and 2, and
    /// starts their tasks. Tier 1 needs `Done`, which a hint does not tell
    /// (it also stands for `TimedOut` and `TooLarge`): it checks each blob
    /// once per session (`Focused::satisfied`). Tier 2 trusts the hints.
    pub(crate) fn enqueue_focus(self: &Arc<Self>, project: &Arc<Project>) {
        let eviction = project.evictions();
        let disabled = lock(&self.langs).disabled.clone();
        let mut st = lock(&project.state);
        if st.base_for.is_none() || st.cancel.load(Ordering::Acquire) {
            return;
        }
        let wt = st.worktree().cloned();
        let stamp = st.stamp(eviction);
        let map = st.map.clone();
        let focus = st.focus.clone();
        let checked = st.focused.satisfied.clone();
        let satisfied = |oid: BlobId, lang: &Lang| checked.contains(&(oid, Version::of(lang)));
        let mut wanted: [Vec<Item>; 2] = Default::default();
        if let Some((w, text)) = &focus.current
            && *w == wt
        {
            match text.origin {
                TextOrigin::Worktree => {
                    if let Some(eff) = map.effective(&text.path)
                        && !satisfied(eff.oid, &eff.lang)
                    {
                        wanted[0].push(item(eff, Tier::Current));
                    }
                }
                TextOrigin::Object => {
                    let table = map.base.table().clone();
                    let mut lookup = LangLookup::new(&table, &*self.packs);
                    let code = lookup.code(&text.path);
                    if let Some(lang) = map.base.lang_of_code(code)
                        && !satisfied(text.oid, lang)
                    {
                        wanted[0].push(Item {
                            path: text.path.clone(),
                            oid: text.oid,
                            lang: lang.clone(),
                            source: Source::Object,
                            tier: Tier::Current,
                        });
                    }
                }
            }
        }
        let lists = [
            (&focus.drawer, 0, Tier::Current),
            (&focus.review, 1, Tier::Review),
            (&focus.touched, 1, Tier::Review),
        ];
        for (list, t, tier) in lists {
            let Some((w, paths)) = list else {
                continue;
            };
            if *w != wt {
                continue;
            }
            for path in paths.iter() {
                let Some(eff) = map.effective(path) else {
                    continue;
                };
                let indexed = if t == 0 {
                    satisfied(eff.oid, &eff.lang)
                } else {
                    cursor_hinted(&map, &eff, stamp)
                };
                if !indexed {
                    wanted[t].push(item(eff, tier));
                }
            }
        }
        for (t, items) in wanted.into_iter().enumerate() {
            for item in items {
                // Tiers 1 and 2 take a key over from a batch (`focus_task`);
                // tier 2 leaves one that tier 1 or 2 is extracting.
                let focused = st
                    .in_flight
                    .get(&item.key())
                    .is_some_and(|o| o.tier.batch().is_none());
                if disabled.contains(item.lang.name()) || (t == 1 && focused) {
                    continue;
                }
                let queue = &mut st.focused.queue[t];
                if queue
                    .iter()
                    .any(|q| q.path == item.path && q.oid == item.oid)
                {
                    continue;
                }
                queue.push_back(item);
            }
        }
        drop(st);
        self.kick_focus(project);
    }

    /// Starts tier 1 (one task) and tier 2 (up to two) tasks for queued
    /// files.
    fn kick_focus(self: &Arc<Self>, project: &Arc<Project>) {
        for (t, limit, priority) in [(0, 1, Priority::Interactive), (1, 2, Priority::Background)] {
            loop {
                let session = {
                    let mut st = lock(&project.state);
                    if st.cancel.load(Ordering::Acquire) {
                        return;
                    }
                    let f = &mut st.focused;
                    if f.running[t] >= limit || f.queue[t].len() <= f.running[t] {
                        break;
                    }
                    f.running[t] += 1;
                    st.session
                };
                let me = self.clone();
                let p = project.clone();
                let name = if t == 0 { "tier1" } else { "tier2" };
                if !self
                    .host
                    .spawn_busy(name, priority, move || me.focus_task(&p, t, session))
                {
                    let mut st = lock(&project.state);
                    if st.session == session {
                        st.focused.running[t] -= 1;
                    }
                    break;
                }
            }
        }
    }

    /// A tier 1 or 2 task: takes files from its queue until it is empty.
    fn focus_task(self: &Arc<Self>, project: &Arc<Project>, t: usize, session: u64) {
        loop {
            let (item, ctx, flag) = {
                let mut st = lock(&project.state);
                if !st.live(session) {
                    // Ended, or a new session (which counts its own tasks):
                    // nothing more is read.
                    return;
                }
                let Some(item) = st.focused.queue[t].pop_front() else {
                    st.focused.running[t] -= 1;
                    return;
                };
                let c = ctx(&st);
                let key = item.key();
                let version = Version::of(&item.lang);
                if t == 0 && st.focused.satisfied.contains(&(item.oid, version)) {
                    // Once per session (`enqueue_focus`).
                    continue;
                }
                if let Some(other) = st.in_flight.get(&key) {
                    // Tiers 1 and 2 never wait on batch work (ADR 0029):
                    // they cancel the extraction in flight and own the key.
                    // Tier 1 also takes one over from tier 2 (the
                    // interactive limits); tier 2 leaves tier 1's and
                    // tier 2's to them.
                    let batch = other.tier.batch().is_some();
                    if t == 1 && !batch {
                        continue;
                    }
                    other.flag.store(true, Ordering::Release);
                }
                let flag = Arc::new(AtomicBool::new(c.cancel.load(Ordering::Acquire)));
                let owner = Owner {
                    flag: flag.clone(),
                    tier: item.tier,
                };
                st.in_flight.insert(key, owner);
                (item, c, flag)
            };
            #[cfg(test)]
            if let Some(hook) = lock(&self.before_focus_check).take() {
                hook();
            }
            let (before_raw, before) = (project.store.evictions(), project.evictions());
            let version = Version::of(&item.lang);
            bump(&self.stats.exists_checks, 1);
            let found = project
                .store
                .exists(&version, &[item.oid])
                .unwrap_or_default();
            if let Some(hit) = found.iter().find(|f| satisfies(item.tier, f.status)) {
                let status = hit.status;
                let table = lock(&self.langs).table.clone();
                let mut st = lock(&project.state);
                let mut wake = None;
                if st.finish(session, &item, &flag) {
                    if t == 0 {
                        st.focused.satisfied.insert((item.oid, version.clone()));
                    }
                    wake = Some(st.batch.queue_touches(before_raw, &found));
                    if current(&table, &st.map, &version) {
                        let stamp = st.stamp(before);
                        st.hint_paths(&item.oid, &item.lang.name().0, stamp, status);
                    }
                }
                drop(st);
                if let Some(wake) = wake {
                    self.wake(wake);
                }
                continue;
            }
            // Not stored, or stored `TimedOut` or `TooLarge` and opened: tier
            // 1 extracts it again with the interactive limits.
            let mode = if t == 0 {
                Mode::Interactive
            } else {
                Mode::Background
            };
            match self.extract_item(&ctx, &item, mode, None, &flag) {
                Outcome::Done(d, _) => {
                    // Settled for the session only once a result is stored:
                    // a file that could not be read is tried again.
                    if self.commit(project, &ctx, vec![d], false) && t == 0 {
                        let mut st = lock(&project.state);
                        if st.session == session {
                            st.focused.satisfied.insert((item.oid, version));
                        }
                    }
                }
                Outcome::Paused | Outcome::Skip | Outcome::Parked(_) | Outcome::Aside => {
                    lock(&project.state).finish(session, &item, &flag);
                }
            }
        }
    }
}

/// Whether a stored unit of status `status` satisfies `tier`, so that the
/// tier does not extract the file again: an opened file (tier 1) needs
/// `Done`, or `Binary`, which no limit changes; the other tiers take any
/// stored status (their limits would give the same). It never decides
/// against a better result already made ([`outranks`]).
fn satisfies(tier: Tier, status: ExtractStatus) -> bool {
    tier != Tier::Current || !unfinished(status)
}

/// A unit without the file's definitions (`State::unfinished`).
fn unfinished(status: ExtractStatus) -> bool {
    matches!(status, ExtractStatus::TimedOut | ExtractStatus::TooLarge)
}

/// Whether an extraction with `new` replaces a stored unit with `stored`,
/// whatever the tier that made it: a `Done` result (tier 1's interactive
/// limits, a query's search), or the `Windowed` marker of a window chain
/// that reached the file's end (its content is in the window units), over
/// a `TooLarge` or `TimedOut` unit. The only replaces `commit` makes: never
/// the other way round, nor an unfinished result over an unfinished unit,
/// nor anything over `Done` or `Windowed`.
fn outranks(new: ExtractStatus, stored: ExtractStatus) -> bool {
    matches!(new, ExtractStatus::Done | ExtractStatus::Windowed) && unfinished(stored)
}

/// Whether `version` is the version of its language both in the current
/// language table and in the table `map` was built with: only then may a
/// unit stored under it stamp the map's hints.
fn current(table: &LanguageTable, map: &crate::map::LiveMap, version: &Version) -> bool {
    let of = |l: Option<&Lang>| l.is_some_and(|l| Version::of(l) == *version);
    of(table.get(&version.lang.0)) && of(map.lang(&version.lang.0))
}

/// `current` for the version a group of `exists` queried `lang` under.
fn same_version(
    table: &LanguageTable,
    map: &crate::map::LiveMap,
    lang: &crate::LangName,
    groups: &[(Version, Vec<Item>)],
) -> bool {
    groups
        .iter()
        .find(|(v, _)| v.lang == *lang)
        .is_some_and(|(v, _)| current(table, map, v))
}

fn item(eff: crate::map::Effective, tier: Tier) -> Item {
    Item {
        path: eff.path,
        oid: eff.oid,
        lang: eff.lang,
        source: Source::Worktree,
        tier,
    }
}

fn ctx(st: &State) -> Ctx {
    Ctx {
        session: st.session,
        cancel: st.cancel.clone(),
        root: st.root.clone(),
        tree: st.tree.clone(),
    }
}

fn cursor_hinted(map: &crate::map::LiveMap, eff: &crate::map::Effective, stamp: HintStamp) -> bool {
    map.hints.matches(&eff.slot, &eff.oid, stamp)
}

/// Tier 3: the deepest sub-project containing the current text, else the
/// review set's first file, else the path the project was opened at (when
/// not its root). The whole worktree without sub-projects; empty when no
/// sub-project contains the anchor or there is none.
pub(super) fn subproject(st: &State) -> Scope {
    let Some(packages) = st.seen.packages.as_ref().filter(|p| !p.is_empty()) else {
        return Scope::Whole;
    };
    let wt = st.worktree();
    let opened = st
        .snapshot
        .as_ref()
        .and_then(|s| s.active.as_ref())
        .map(|a| a.opened_at.clone())
        .filter(|p| !p.is_empty());
    let anchor = st
        .focus
        .current
        .as_ref()
        .filter(|(w, _)| w.as_ref() == wt)
        .map(|(_, t)| t.path.clone())
        .or_else(|| {
            st.focus
                .review
                .as_ref()
                .filter(|(w, _)| w.as_ref() == wt)
                .and_then(|(_, p)| p.first().cloned())
        })
        .or(opened);
    let Some(anchor) = anchor else {
        return Scope::Empty;
    };
    match packages
        .iter()
        .filter(|p| under(&anchor, &p.dir))
        .max_by_key(|p| p.dir.len())
    {
        Some(p) if p.dir.is_empty() => Scope::Whole,
        Some(p) => Scope::Dir(p.dir.clone()),
        None => Scope::Empty,
    }
}
