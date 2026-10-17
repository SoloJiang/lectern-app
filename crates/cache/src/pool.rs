//! The open project partitions: at most `Budgets::open_projects` of them
//! hold connections; opening one more swaps out the least recently used
//! ("连接与内存" in the design). The app partition is not counted.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

use crate::slot::Slot;
use crate::util::lock;

/// The sessions of project partitions that are open or opening.
///
/// A leaf lock: nothing else is locked while it is held, and no `Slot` is
/// dropped under it (it holds no strong reference). It is taken with a
/// partition's session lock held (opening, and dropping a session).
#[derive(Default)]
pub(crate) struct Pool {
    open: Mutex<Vec<Open>>,
}

struct Open {
    session: u64,
    slot: Weak<Slot>,
    /// `Slot::last_used`.
    used: Arc<AtomicU64>,
    file: PathBuf,
}

impl Pool {
    /// Takes a place for session `session` of `slot`, which is about to
    /// open. When `cap` places are taken, returns the least recently used
    /// partition and its session, to be swapped out: the caller closes it
    /// before opening its own file, so the connections stay within the limit.
    pub(crate) fn reserve(
        &self,
        slot: &Slot,
        session: u64,
        cap: usize,
    ) -> Option<(Weak<Slot>, u64)> {
        let mut open = lock(&self.open);
        let mut victim = None;
        if open.len() >= cap.max(1) {
            let lru = open
                .iter()
                .enumerate()
                .min_by_key(|(_, o)| o.used.load(Ordering::Relaxed))
                .map(|(i, _)| i);
            if let Some(i) = lru {
                let o = open.swap_remove(i);
                victim = Some((o.slot, o.session));
            }
        }
        open.push(Open {
            session,
            slot: slot.me.clone(),
            used: slot.last_used.clone(),
            file: slot.file.clone(),
        });
        victim
    }

    /// Gives the place of `session` back: it failed to open or was closed.
    pub(crate) fn release(&self, session: u64) {
        lock(&self.open).retain(|o| o.session != session);
    }

    /// The `.sqlite` files of the partitions that are open or opening.
    pub(crate) fn files(&self) -> Vec<PathBuf> {
        lock(&self.open).iter().map(|o| o.file.clone()).collect()
    }
}
