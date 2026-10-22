//! The thread's correlation scope (docs/design/logging.md「作用域与关联 ID」).

use std::cell::Cell;
use std::marker::PhantomData;
use std::sync::Mutex;

/// Correlation IDs added to every record written on this thread while a
/// [`Scope`] holding them is alive.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Ctx {
    pub plugin: Option<PluginTag>,
    /// The kernel's activation number, from 1 within a process.
    pub act: Option<u64>,
    /// The executor's task number, from 1 within a process.
    pub task: Option<u64>,
    /// A session UUID (Claude Code / Codex session ID, as `agents` parses it).
    pub session: Option<[u8; 16]>,
    /// The 64-bit hash of a review subject, defined by `review`.
    pub review: Option<u64>,
    /// The number of one outgoing request (forge, Analyze forwarding, a
    /// process plugin's JSON-RPC).
    pub req: Option<u64>,
    /// The number of one Analyze run.
    pub analysis: Option<u64>,
}

impl Ctx {
    const EMPTY: Ctx = Ctx {
        plugin: None,
        act: None,
        task: None,
        session: None,
        review: None,
        req: None,
        analysis: None,
    };

    /// `self` with every field `top` sets replaced.
    fn overlay(self, top: Ctx) -> Ctx {
        Ctx {
            plugin: top.plugin.or(self.plugin),
            act: top.act.or(self.act),
            task: top.task.or(self.task),
            session: top.session.or(self.session),
            review: top.review.or(self.review),
            req: top.req.or(self.req),
            analysis: top.analysis.or(self.analysis),
        }
    }
}

/// A plugin's stable ID. Built-in plugins use [`PluginTag::new`]; the kernel
/// registers a process plugin's name once with [`PluginTag::register`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PluginTag(&'static str);

/// Names registered by [`PluginTag::register`] stay for the process.
const MAX_REGISTERED: usize = 1024;
static REGISTERED: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());

impl PluginTag {
    pub const fn new(id: &'static str) -> PluginTag {
        PluginTag(id)
    }

    /// Registers a process plugin's ID: `[a-z0-9_-]`, 1 to 64 bytes, what an
    /// `@plugin=level` directive can name (and `verbose.json` can keep).
    /// The same ID returns the same tag. None for another ID, or once 1024
    /// IDs are registered (the record then has no `plugin`).
    pub fn register(id: &str) -> Option<PluginTag> {
        if !crate::filter::is_plugin_id(id) {
            return None;
        }
        let mut names = REGISTERED.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(name) = names.iter().find(|n| **n == id) {
            return Some(PluginTag(name));
        }
        if names.len() >= MAX_REGISTERED {
            return None;
        }
        let name: &'static str = Box::leak(id.to_owned().into_boxed_str());
        names.push(name);
        Some(PluginTag(name))
    }

    pub fn as_str(self) -> &'static str {
        self.0
    }
}

thread_local! {
    static CURRENT: Cell<Ctx> = const { Cell::new(Ctx::EMPTY) };
    static DEPTH: Cell<u32> = const { Cell::new(0) };
}

/// The current thread's scope; all None outside any [`Scope`].
pub fn current() -> Ctx {
    CURRENT.try_with(Cell::get).unwrap_or_default()
}

/// Sets every field of `ctx` that is Some on top of the current scope, until
/// the guard drops. Guards drop in nesting order (checked in debug builds).
///
/// Not held across `.await` (clippy's `await_holding_invalid_type` names
/// this type): a task's scope is set by the executor around each poll.
#[must_use = "the scope ends when the guard drops"]
pub fn scope(ctx: Ctx) -> Scope {
    let prev = current();
    let _ = CURRENT.try_with(|c| c.set(prev.overlay(ctx)));
    let depth = DEPTH.try_with(|d| {
        let depth = d.get() + 1;
        d.set(depth);
        depth
    });
    Scope {
        prev,
        depth: depth.unwrap_or(0),
        _not_send: PhantomData,
    }
}

/// Restores the previous scope when dropped. `!Send`.
#[must_use = "the scope ends when the guard drops"]
pub struct Scope {
    prev: Ctx,
    depth: u32,
    _not_send: PhantomData<*const ()>,
}

impl Drop for Scope {
    fn drop(&mut self) {
        let _ = DEPTH.try_with(|d| {
            if !std::thread::panicking() {
                debug_assert_eq!(
                    d.get(),
                    self.depth,
                    "lectern_logging::Scope guards dropped out of order"
                );
            }
            d.set(self.depth.saturating_sub(1));
        });
        let _ = CURRENT.try_with(|c| c.set(self.prev));
    }
}
