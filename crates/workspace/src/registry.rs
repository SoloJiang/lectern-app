//! The contribution registry (docs/design/workspace.md「贡献注册表」): what
//! `dyn Contributions` writes from any thread, and what the main thread
//! applies, removes and reads.
//!
//! An entry's life: `register` (any thread) stages it under its `(kind, id)`
//! and returns the `Registration` guard; `commit(token)` (the kernel thread,
//! at the activation's commit) records the token and queues an *apply*;
//! the main thread applies it when the token is live, defers it while the
//! token is still pending, drops it when the token is dead; the guard's drop
//! (any thread) frees the `(kind, id)` at once and queues a *remove* that
//! drops the payload on the main thread.
//!
//! The payload types live in `ui::contrib`, which this step does not depend
//! on: the registry checks and shares payloads through a [`Payloads`] it is
//! built with.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use lectern_kernel::{ActivationToken, Name, TokenState, contain};
use lectern_logging::{Ctx, Field, Value, debug, info, warn};
use lectern_services::workspace::{
    CommandCall, CommandHandler, Contribution, ContributionKind, LeaveGuard, Payload, Registration,
    ScreenLayout, SharedPayload, SidebarEntry, SidebarSection,
};

use crate::diag::{id_text, is_valid_command_name, is_valid_id};
use crate::hub::{Item, Queued, Sender};

/// A registration's number, process-wide from 1.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Serial(pub u64);

/// The kind of an entry: a `ContributionKind` without its data, a command
/// handler or a leave guard.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Screen,
    SidebarPanel,
    SidebarEntry,
    InspectorTab,
    Panel,
    Overlay,
    KeyBindings,
    Command,
    LeaveGuard,
}

impl Kind {
    pub fn of(kind: &ContributionKind) -> Kind {
        match kind {
            ContributionKind::Screen(_) => Kind::Screen,
            ContributionKind::SidebarPanel => Kind::SidebarPanel,
            ContributionKind::SidebarEntry { .. } => Kind::SidebarEntry,
            ContributionKind::InspectorTab => Kind::InspectorTab,
            ContributionKind::Panel => Kind::Panel,
            ContributionKind::Overlay => Kind::Overlay,
            ContributionKind::KeyBindings => Kind::KeyBindings,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Kind::Screen => "screen",
            Kind::SidebarPanel => "sidebar_panel",
            Kind::SidebarEntry => "sidebar_entry",
            Kind::InspectorTab => "inspector_tab",
            Kind::Panel => "panel",
            Kind::Overlay => "overlay",
            Kind::KeyBindings => "key_bindings",
            Kind::Command => "command",
            Kind::LeaveGuard => "leave_guard",
        }
    }
}

impl Field for Kind {
    fn value(&self) -> Value<'_> {
        Value::text(self.name())
    }
}

/// Why a registration was rejected (`contribution rejected`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    Duplicate,
    WrongPayload,
    InvalidId,
    TokenDead,
}

impl Field for Reason {
    fn value(&self) -> Value<'_> {
        Value::text(match self {
            Reason::Duplicate => "duplicate",
            Reason::WrongPayload => "wrong-payload",
            Reason::InvalidId => "invalid-id",
            Reason::TokenDead => "token-dead",
        })
    }
}

/// The payload types of `ui::contrib`, which the root view (step 2) knows
/// and this crate's tests fake.
pub trait Payloads: Send + Sync {
    /// Whether `payload` is the type `kind` requires.
    fn accepts(&self, kind: &ContributionKind, payload: &Payload) -> bool;

    /// A `SidebarEntry` payload as the shared `Arc` that `sidebar_entries`
    /// hands out. None when it is not the sidebar item type.
    fn share(&self, payload: Payload) -> Option<SharedPayload>;
}

/// The shared handler of a `handle_command` registration.
pub type SharedHandler = Arc<dyn Fn(&CommandCall) + Send + Sync>;

/// What an entry holds.
pub enum Content {
    /// A contribution's payload, as registered.
    Contribution {
        kind: ContributionKind,
        order: u16,
        payload: Payload,
    },
    /// An applied `SidebarEntry`: its payload is shared.
    Shared {
        section: SidebarSection,
        order: u16,
        payload: SharedPayload,
    },
    Command(SharedHandler),
    Guard(Arc<dyn LeaveGuard>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    /// Registered, waiting for its commit.
    Staged,
    /// Committed, its apply queued to the main thread.
    Queued,
    Applied,
}

struct Entry {
    kind: Kind,
    id: Name,
    token: Option<ActivationToken>,
    /// The registering thread's log scope, restored around its closures.
    ctx: Ctx,
    state: State,
    content: Content,
}

#[derive(Default)]
struct Tables {
    entries: HashMap<Serial, Entry>,
    /// The live `(kind, id)`s: staged, queued or applied, not yet released.
    index: HashMap<(Kind, Name), Serial>,
    /// Tokens seen at commit, by activation number: a registration after
    /// `started` from the same activation applies without a commit.
    known: HashMap<u64, ActivationToken>,
    /// Queued applies not yet applied or dropped, per activation.
    outstanding: HashMap<u64, usize>,
    /// Activations whose queued applies have all been applied or dropped.
    applied: HashSet<u64>,
}

/// What `apply` did.
#[derive(Debug, PartialEq, Eq)]
pub enum Applied {
    Applied {
        kind: Kind,
        id: Name,
    },
    /// The token is still pending: queue the item again later.
    Deferred,
    /// The token is dead, or the entry was released first.
    Dropped,
}

/// What `remove` did.
#[derive(Debug, PartialEq, Eq)]
pub enum Removed {
    Removed { kind: Kind, id: Name },
    Nothing,
}

/// A guard to ask, as the driver takes it out of the registry.
pub struct GuardRef {
    pub token: ActivationToken,
    pub ctx: Ctx,
    pub guard: Arc<dyn LeaveGuard>,
}

/// A command handler, as the driver takes it out of the registry.
pub struct HandlerRef {
    pub token: ActivationToken,
    pub ctx: Ctx,
    pub handler: SharedHandler,
}

/// One per process, shared by the `Hub` (`dyn Contributions`) and the driver.
pub struct Registry {
    tables: Mutex<Tables>,
    /// The sorted sidebar entries, replaced whole when they change.
    sidebar: Mutex<Arc<[SidebarEntry]>>,
    payloads: Box<dyn Payloads>,
    sender: Sender,
    next_serial: AtomicU64,
}

impl Registry {
    pub fn new(payloads: Box<dyn Payloads>, sender: Sender) -> Arc<Registry> {
        Arc::new(Registry {
            tables: Mutex::new(Tables::default()),
            sidebar: Mutex::new(Arc::from(Vec::new())),
            payloads,
            sender,
            next_serial: AtomicU64::new(1),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Tables> {
        self.tables.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn lock_sidebar(&self) -> MutexGuard<'_, Arc<[SidebarEntry]>> {
        self.sidebar.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Swaps in a new sidebar snapshot; the old one (whose last reference
    /// may run contributor `Drop` code) is dropped after the lock, under
    /// `contain`.
    fn replace_sidebar(&self, entries: Arc<[SidebarEntry]>) {
        let old = std::mem::replace(&mut *self.lock_sidebar(), entries);
        if contain(move || drop(old)).is_err() {
            lectern_logging::error!("contribution drop panicked"; kind = Kind::SidebarEntry);
        }
    }

    /// A guard that does nothing: the registration was rejected.
    fn rejected(kind: Kind, id: &Name, reason: Reason) -> Registration {
        warn!("contribution rejected"; kind = kind, id = id_text(id), reason = reason);
        Registration::new(|_| {}, || {})
    }

    /// `Contributions::register`.
    pub fn register(self: &Arc<Self>, contribution: Contribution) -> Registration {
        let Contribution {
            kind,
            id,
            order,
            payload,
        } = contribution;
        let key = Kind::of(&kind);
        if !is_valid_id(&id) {
            // Not a valid ID, so not logged as one either.
            warn!("contribution rejected"; kind = key, reason = Reason::InvalidId);
            return Registration::new(|_| {}, || {});
        }
        if !self.payloads.accepts(&kind, &payload) {
            return Registry::rejected(key, &id, Reason::WrongPayload);
        }
        self.insert(
            key,
            id,
            Content::Contribution {
                kind,
                order,
                payload,
            },
        )
    }

    /// `Contributions::handle_command`.
    pub fn handle_command(self: &Arc<Self>, name: Name, handler: CommandHandler) -> Registration {
        if !is_valid_command_name(&name) {
            warn!("contribution rejected"; kind = Kind::Command, reason = Reason::InvalidId);
            return Registration::new(|_| {}, || {});
        }
        self.insert(Kind::Command, name, Content::Command(Arc::from(handler)))
    }

    /// `Contributions::register_leave_guard`. Guards have no ID of their
    /// own: the serial is the ID, so there are no duplicates.
    pub fn register_leave_guard(self: &Arc<Self>, guard: Arc<dyn LeaveGuard>) -> Registration {
        let serial = self.next_serial.fetch_add(1, Ordering::Relaxed);
        let id = Name::Owned(format!("guard-{serial}"));
        self.insert(Kind::LeaveGuard, id, Content::Guard(guard))
    }

    fn insert(self: &Arc<Self>, kind: Kind, id: Name, content: Content) -> Registration {
        let serial = Serial(self.next_serial.fetch_add(1, Ordering::Relaxed));
        let ctx = lectern_logging::current();
        let mut tables = self.lock();
        if tables.index.contains_key(&(kind, id.clone())) {
            drop(tables);
            return Registry::rejected(kind, &id, Reason::Duplicate);
        }
        tables.index.insert((kind, id.clone()), serial);
        tables.entries.insert(
            serial,
            Entry {
                kind,
                id,
                token: None,
                ctx,
                state: State::Staged,
                content,
            },
        );
        // After the commit (in `started`, a listener or a task of the same
        // activation) the token is known: apply now, no commit follows. A
        // pending token (inside `started`) defers at apply time.
        // Inside `activate` the token is not known yet (nothing committed):
        // the entry stays staged for the commit. An activation that commits
        // nothing and registers later gets the same treatment, and the
        // entry is reported when it is released without ever being
        // committed (docs/design/workspace.md「与设计的差异」第 1 步).
        if let Some(act) = ctx.act
            && let Some(token) = tables.known.get(&act).cloned()
            && token.state() != TokenState::Dead
        {
            let dropped = self.queue_apply(&mut tables, serial, token);
            drop(tables);
            if let Some(entry) = dropped {
                drop_content(entry);
            }
        } else {
            drop(tables);
        }
        let registry = self.clone();
        let committing = self.clone();
        Registration::new(
            move |token| committing.commit(serial, token),
            move || registry.release(serial),
        )
    }

    /// `Staged::commit`: records the token and queues the apply.
    fn commit(&self, serial: Serial, token: ActivationToken) {
        let mut tables = self.lock();
        tables.known.insert(token.id(), token.clone());
        let Some(entry) = tables.entries.get(&serial) else {
            return;
        };
        if entry.state != State::Staged {
            return;
        }
        let dropped = self.queue_apply(&mut tables, serial, token);
        drop(tables);
        if let Some(entry) = dropped {
            drop_content(entry);
        }
    }

    /// Queues the apply of a staged entry with the token's pending guard.
    /// Without one (`shutdown` closed it) the entry is taken out and returned,
    /// for the caller to drop after the lock.
    fn queue_apply(
        &self,
        tables: &mut Tables,
        serial: Serial,
        token: ActivationToken,
    ) -> Option<Entry> {
        let Some(guard) = token.pending() else {
            let entry = tables.entries.remove(&serial)?;
            tables.index.remove(&(entry.kind, entry.id.clone()));
            return Some(entry);
        };
        let act = token.id();
        let entry = tables.entries.get_mut(&serial)?;
        entry.token = Some(token);
        entry.state = State::Queued;
        *tables.outstanding.entry(act).or_insert(0) += 1;
        tables.applied.remove(&act);
        (self.sender)(Queued {
            item: Item::Apply(serial),
            guard: Some(guard),
        });
        None
    }

    /// The registration guard dropped: frees the `(kind, id)` now and queues
    /// the removal of an applied or queued entry; a staged one is dropped
    /// here.
    fn release(&self, serial: Serial) {
        let mut tables = self.lock();
        let Some(entry) = tables.entries.get(&serial) else {
            return;
        };
        let slot = (entry.kind, entry.id.clone());
        let token = entry.token.clone();
        if tables.index.get(&slot) == Some(&serial) {
            tables.index.remove(&slot);
        }
        match token {
            None => {
                let entry = tables.entries.remove(&serial);
                drop(tables);
                if let Some(entry) = entry {
                    // Never committed: a failed activation (the kernel logged
                    // it), or a registration made after `started` by an
                    // activation that committed nothing, which nobody could
                    // apply (docs/design/workspace.md「与设计的差异」第 1 步).
                    warn!(
                        "contribution released without a commit";
                        kind = entry.kind,
                        id = id_text(&entry.id),
                        activation = entry.ctx.act
                    );
                    drop_content(entry);
                }
            }
            Some(token) => {
                let Some(guard) = token.pending() else {
                    // The main thread's queue no longer runs: drop here.
                    let entry = tables.entries.remove(&serial);
                    drop(tables);
                    if let Some(entry) = entry {
                        drop_content(entry);
                    }
                    return;
                };
                drop(tables);
                (self.sender)(Queued {
                    item: Item::Remove(serial),
                    guard: Some(guard),
                });
            }
        }
    }

    /// Main thread: applies a queued entry.
    pub fn apply(&self, serial: Serial) -> Applied {
        let mut tables = self.lock();
        let Some(entry) = tables.entries.get(&serial) else {
            return Applied::Dropped;
        };
        if entry.state != State::Queued {
            return Applied::Dropped;
        }
        let Some(token) = entry.token.clone() else {
            return Applied::Dropped;
        };
        match token.state() {
            TokenState::Pending => return Applied::Deferred,
            TokenState::Dead => {
                let kind = entry.kind;
                let id = entry.id.clone();
                let entry = tables.entries.remove(&serial);
                settle(&mut tables, token.id());
                // The slot stays free only if this entry still owns it.
                if tables.index.get(&(kind, id.clone())) == Some(&serial) {
                    tables.index.remove(&(kind, id.clone()));
                }
                drop(tables);
                let _scope = entry.as_ref().map(|e| lectern_logging::scope(e.ctx));
                warn!("contribution rejected"; kind = kind, id = id_text(&id), reason = Reason::TokenDead);
                if let Some(entry) = entry {
                    drop_content(entry);
                }
                return Applied::Dropped;
            }
            TokenState::Live => {}
        }
        let entry = tables.entries.get_mut(&serial).expect("checked above");
        entry.state = State::Applied;
        let kind = entry.kind;
        let id = entry.id.clone();
        let ctx = entry.ctx;
        if kind == Kind::SidebarEntry {
            let content = std::mem::replace(&mut entry.content, Content::Command(Arc::new(|_| {})));
            match content {
                Content::Contribution {
                    kind: ContributionKind::SidebarEntry { section },
                    order,
                    payload,
                } => match self.payloads.share(payload) {
                    Some(payload) => {
                        entry.content = Content::Shared {
                            section,
                            order,
                            payload,
                        };
                    }
                    None => {
                        // `accepts` said yes at registration; the shared
                        // form is the same type, so this does not happen.
                        tables.entries.remove(&serial);
                        tables.index.remove(&(kind, id.clone()));
                        settle(&mut tables, token.id());
                        drop(tables);
                        let _scope = lectern_logging::scope(ctx);
                        warn!("contribution rejected"; kind = kind, id = id_text(&id), reason = Reason::WrongPayload);
                        return Applied::Dropped;
                    }
                },
                other => {
                    entry.content = other;
                }
            }
        }
        settle(&mut tables, token.id());
        if kind == Kind::SidebarEntry {
            let entries = sidebar_entries(&tables);
            drop(tables);
            self.replace_sidebar(entries);
        } else {
            drop(tables);
        }
        let _scope = lectern_logging::scope(ctx);
        info!("contribution applied"; kind = kind, id = id_text(&id));
        Applied::Applied { kind, id }
    }

    /// Main thread: removes a released entry and drops its payload under
    /// `contain`.
    pub fn remove(&self, serial: Serial) -> Removed {
        let mut tables = self.lock();
        let Some(entry) = tables.entries.remove(&serial) else {
            return Removed::Nothing;
        };
        if tables.index.get(&(entry.kind, entry.id.clone())) == Some(&serial) {
            tables.index.remove(&(entry.kind, entry.id.clone()));
        }
        if let Some(token) = &entry.token
            && entry.state == State::Queued
        {
            settle(&mut tables, token.id());
        }
        let applied = entry.state == State::Applied;
        let kind = entry.kind;
        let id = entry.id.clone();
        let ctx = entry.ctx;
        if applied && kind == Kind::SidebarEntry {
            let entries = sidebar_entries(&tables);
            drop(tables);
            self.replace_sidebar(entries);
        } else {
            drop(tables);
        }
        let _scope = lectern_logging::scope(ctx);
        drop_content(entry);
        if applied {
            info!("contribution removed"; kind = kind, id = id_text(&id));
            Removed::Removed { kind, id }
        } else {
            debug!("contribution dropped before it applied"; kind = kind, id = id_text(&id));
            Removed::Nothing
        }
    }

    /// Whether every apply queued for the activation has been applied or
    /// dropped (「按命令激活」step 3). False for an activation that queued
    /// nothing.
    pub fn activation_applied(&self, act: u64) -> bool {
        self.lock().applied.contains(&act)
    }

    /// `Contributions::sidebar_entries`.
    pub fn sidebar_entries(&self) -> Arc<[SidebarEntry]> {
        self.lock_sidebar().clone()
    }

    fn applied_serial(&self, kind: Kind, id: &str) -> Option<Serial> {
        let tables = self.lock();
        let serial = *tables.index.get(&(kind, Name::Owned(id.to_owned())))?;
        let entry = tables.entries.get(&serial)?;
        (entry.state == State::Applied).then_some(serial)
    }

    /// Whether an applied entry of this kind and ID exists.
    pub fn has(&self, kind: Kind, id: &str) -> bool {
        self.applied_serial(kind, id).is_some()
    }

    /// The layout of an applied screen.
    pub fn screen_layout(&self, id: &str) -> Option<ScreenLayout> {
        let tables = self.lock();
        let serial = *tables
            .index
            .get(&(Kind::Screen, Name::Owned(id.to_owned())))?;
        let entry = tables.entries.get(&serial)?;
        match (&entry.state, &entry.content) {
            (
                State::Applied,
                Content::Contribution {
                    kind: ContributionKind::Screen(layout),
                    ..
                },
            ) => Some(*layout),
            _ => None,
        }
    }

    /// Runs `f` on the payload of an applied contribution, under the lock.
    /// The root view takes the view factory (an `Arc`) out this way; `f`
    /// must not call the registry.
    pub fn with_payload<R>(
        &self,
        kind: Kind,
        id: &str,
        f: impl FnOnce(&Payload) -> R,
    ) -> Option<R> {
        let serial = self.applied_serial(kind, id)?;
        self.with_serial(serial, f)
    }

    /// Like [`with_payload`](Registry::with_payload), by serial (a
    /// `BindKeys` effect).
    pub fn with_serial<R>(&self, serial: Serial, f: impl FnOnce(&Payload) -> R) -> Option<R> {
        let tables = self.lock();
        let entry = tables.entries.get(&serial)?;
        match &entry.content {
            Content::Contribution { payload, .. } if entry.state == State::Applied => {
                Some(f(payload))
            }
            _ => None,
        }
    }

    /// The token and log scope of an applied contribution, to call its
    /// closures with.
    pub fn token_of(&self, kind: Kind, id: &str) -> Option<(ActivationToken, Ctx)> {
        let serial = self.applied_serial(kind, id)?;
        let tables = self.lock();
        let entry = tables.entries.get(&serial)?;
        Some((entry.token.clone()?, entry.ctx))
    }

    /// The applied leave guards, in registration order.
    pub fn guards(&self) -> Vec<Serial> {
        let tables = self.lock();
        let mut serials: Vec<Serial> = tables
            .entries
            .iter()
            .filter(|(_, e)| e.kind == Kind::LeaveGuard && e.state == State::Applied)
            .map(|(s, _)| *s)
            .collect();
        serials.sort_unstable();
        serials
    }

    /// An applied leave guard; None once it is released.
    pub fn guard(&self, serial: Serial) -> Option<GuardRef> {
        let tables = self.lock();
        let entry = tables.entries.get(&serial)?;
        match &entry.content {
            Content::Guard(guard) if entry.state == State::Applied => Some(GuardRef {
                token: entry.token.clone()?,
                ctx: entry.ctx,
                guard: guard.clone(),
            }),
            _ => None,
        }
    }

    /// The applied handler of a command.
    pub fn handler(&self, name: &str) -> Option<HandlerRef> {
        let serial = self.applied_serial(Kind::Command, name)?;
        let tables = self.lock();
        let entry = tables.entries.get(&serial)?;
        match &entry.content {
            Content::Command(handler) => Some(HandlerRef {
                token: entry.token.clone()?,
                ctx: entry.ctx,
                handler: handler.clone(),
            }),
            _ => None,
        }
    }

    /// The applied entries, for the probe and the tests: `(kind, id)` in
    /// serial order.
    pub fn applied(&self) -> Vec<(Kind, Name)> {
        let tables = self.lock();
        let mut entries: Vec<(Serial, Kind, Name)> = tables
            .entries
            .iter()
            .filter(|(_, e)| e.state == State::Applied)
            .map(|(s, e)| (*s, e.kind, e.id.clone()))
            .collect();
        entries.sort_unstable_by_key(|(s, _, _)| *s);
        entries.into_iter().map(|(_, k, id)| (k, id)).collect()
    }
}

/// One queued apply of the activation is done: the activation counts as
/// applied once none is outstanding.
fn settle(tables: &mut Tables, act: u64) {
    let left = match tables.outstanding.get_mut(&act) {
        Some(n) => {
            *n = n.saturating_sub(1);
            *n
        }
        None => 0,
    };
    if left == 0 {
        tables.outstanding.remove(&act);
        tables.applied.insert(act);
    }
}

fn sidebar_entries(tables: &Tables) -> Arc<[SidebarEntry]> {
    let mut entries: Vec<SidebarEntry> = tables
        .entries
        .values()
        .filter(|e| e.state == State::Applied)
        .filter_map(|e| match &e.content {
            Content::Shared {
                section,
                order,
                payload,
            } => Some(SidebarEntry {
                id: e.id.clone(),
                section: *section,
                order: *order,
                payload: payload.clone(),
            }),
            _ => None,
        })
        .collect();
    entries.sort_by(|a, b| (a.section, a.order, &a.id).cmp(&(b.section, b.order, &b.id)));
    Arc::from(entries)
}

/// Drops an entry's content under `contain`: a payload's `Drop` is the
/// contributor's code.
fn drop_content(entry: Entry) {
    let kind = entry.kind;
    let id = entry.id.clone();
    let act = entry.token.as_ref().map(ActivationToken::id);
    if contain(move || drop(entry)).is_err() {
        lectern_logging::error!("contribution drop panicked"; kind = kind, id = id_text(&id), activation = act);
    }
}
