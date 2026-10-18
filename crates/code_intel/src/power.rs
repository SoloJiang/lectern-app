//! The power source (docs/design/code_intel.md「本 crate 的公开项」): on
//! battery, tiers 4 and 5 wait (「建索引的分层」, ADR 0029). `app` wraps
//! `platform`'s power interface; this crate never reads it itself
//! (architecture rule 6), and its tests use `testing::FakePower`.

/// Whether the machine runs on battery, and its changes.
pub trait PowerSource: Send + Sync {
    /// Whether the machine runs on battery now. Does not block.
    fn on_battery(&self) -> bool;

    /// Calls `f` on any thread when the power source changes; `f` only
    /// queues work and never blocks. Dropping the returned value
    /// unsubscribes.
    fn subscribe(&self, f: Box<dyn Fn(bool) + Send + Sync>) -> Box<dyn Send>;
}

/// Always on AC power (Linux, and before `app` wires `platform`'s power
/// interface).
#[derive(Clone, Copy, Default, Debug)]
pub struct AlwaysOnAc;

impl PowerSource for AlwaysOnAc {
    fn on_battery(&self) -> bool {
        false
    }

    fn subscribe(&self, _f: Box<dyn Fn(bool) + Send + Sync>) -> Box<dyn Send> {
        Box::new(())
    }
}
