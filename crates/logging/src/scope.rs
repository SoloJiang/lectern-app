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

/// The most scopes in effect at once on one thread. A [`scope`] or
/// [`within`] beyond it changes nothing (its records carry the outer scope).
pub(crate) const MAX_DEPTH: usize = 32;

/// The context outside any scope: `frames[0]` of every thread.
static EMPTY: Ctx = Ctx::EMPTY;

/// The thread's scopes: a stack of pointers to contexts. `depth` is the
/// number of scopes in effect and `frames[depth]` the current context.
///
/// Invariant: for every `i <= depth`, `frames[i]` points at a live `Ctx`:
/// [`EMPTY`] (`i == 0`, never written), `slots[i - 1]` (memory of this
/// thread-local, alive for the thread) or the context borrowed by a
/// [`within`] call still running on this thread.
/// - The depth grows only by one push at a time, which writes `frames[d + 1]`
///   before setting the depth to `d + 1`.
/// - Leaving a scope only ever lowers the depth (below the scope's index),
///   never raises it. A `within` lowers it before its borrow ends, also when
///   unwinding and whatever guards inside it were forgotten or dropped out
///   of order. So once a `within` at index `i` returned, the depth can reach
///   `i` again only through a new push that overwrites `frames[i]`.
///
/// No `Drop`, so the thread-local is never torn down before the thread ends.
struct Frames {
    depth: Cell<usize>,
    frames: [Cell<*const Ctx>; MAX_DEPTH + 1],
    /// The merged contexts of overlay scopes, `slots[i - 1]` for the one at
    /// index `i`.
    slots: [Cell<Ctx>; MAX_DEPTH],
}

thread_local! {
    static FRAMES: Frames = const {
        Frames {
            depth: Cell::new(0),
            frames: [const { Cell::new(&raw const EMPTY) }; MAX_DEPTH + 1],
            slots: [const { Cell::new(Ctx::EMPTY) }; MAX_DEPTH],
        }
    };
}

impl Frames {
    /// The context in effect.
    fn top(&self) -> Ctx {
        // SAFETY: by the invariant on `Frames`, `frames[depth]` is live;
        // contexts are only written through `Cell::set` on a slot that is not
        // in effect (and `EMPTY` and borrowed contexts not at all), so
        // nothing writes it now.
        unsafe { *self.frames[self.depth.get()].get() }
    }

    /// Makes `ctx` the current context; returns the new depth, or 0 (and
    /// changes nothing) at `MAX_DEPTH`.
    fn push(&self, ctx: *const Ctx) -> usize {
        let d = self.depth.get() + 1;
        if d > MAX_DEPTH {
            return 0;
        }
        self.frames[d].set(ctx);
        self.depth.set(d);
        d
    }

    /// Ends the scope pushed at `depth` (0 is a scope that was not pushed).
    /// Lowers the depth first, then checks the nesting order in debug
    /// builds.
    fn leave(&self, depth: usize) {
        if depth == 0 {
            return;
        }
        let now = self.depth.get();
        if now >= depth {
            self.depth.set(depth - 1);
        }
        // `cfg!` first: release builds do not even ask whether the thread is
        // panicking.
        if cfg!(debug_assertions) && !std::thread::panicking() {
            assert_eq!(
                now, depth,
                "lectern_logging::Scope guards dropped out of order"
            );
        }
    }
}

/// The current thread's scope; all None outside any [`Scope`].
pub fn current() -> Ctx {
    FRAMES.try_with(Frames::top).unwrap_or_default()
}

/// Sets every field of `ctx` that is Some on top of the current scope, until
/// the guard drops. Guards drop in nesting order (checked in debug builds).
/// The merged context is written once into this thread's slot for the new
/// depth; dropping the guard only lowers the depth.
///
/// Not held across `.await` (clippy's `await_holding_invalid_type` names
/// this type): a task's scope is set by the executor around each poll.
#[must_use = "the scope ends when the guard drops"]
pub fn scope(ctx: Ctx) -> Scope {
    let depth = FRAMES
        .try_with(|f| {
            let d = f.depth.get();
            if d >= MAX_DEPTH {
                return 0;
            }
            // `slots[d]` belongs to index `d + 1`, which is not in effect.
            let slot = &f.slots[d];
            slot.set(f.top().overlay(ctx));
            f.push(slot.as_ptr().cast_const())
        })
        .unwrap_or(0);
    Scope {
        depth,
        _not_send: PhantomData,
    }
}

/// Runs `f` with `ctx` as this thread's whole scope (it replaces the current
/// one, it does not overlay it), then restores the previous scope, also when
/// `f` unwinds. Entering and leaving store a pointer and a depth, nothing is
/// copied: the executor enters each task's precomputed context around every
/// poll (docs/design/executor.md「日志与诊断」). [`scope`] inside `f` overlays
/// `ctx` as usual.
pub fn within<R>(ctx: &Ctx, f: impl FnOnce() -> R) -> R {
    /// Leaves on the way out of `within`. Private and never moved out of the
    /// call, so it cannot be forgotten: the borrow of `ctx` outlives the
    /// frame that points at it.
    struct Exit(usize);
    impl Drop for Exit {
        fn drop(&mut self) {
            let _ = FRAMES.try_with(|f| f.leave(self.0));
        }
    }
    let depth = FRAMES
        .try_with(|frames| frames.push(std::ptr::from_ref(ctx)))
        .unwrap_or(0);
    let _exit = Exit(depth);
    f()
}

/// Restores the previous scope when dropped. `!Send`.
#[must_use = "the scope ends when the guard drops"]
pub struct Scope {
    /// Its depth once pushed, 0 when it was not.
    depth: usize,
    _not_send: PhantomData<*const ()>,
}

impl Drop for Scope {
    fn drop(&mut self) {
        let _ = FRAMES.try_with(|f| f.leave(self.depth));
    }
}
