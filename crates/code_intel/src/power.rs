//! The power source (docs/design/code_intel.md「本 crate 的公开项」): step
//! 3's battery rule (tiers 4 and 5 wait on battery, ADR 0029), no longer
//! the design since ADR 0032: indexing does not depend on the power source,
//! `platform` has no power interface and `app` passes `AlwaysOnAc`. This
//! code and its `battery_*` tests go in a separate PR (ADR 0032「影响」).

/// Whether the machine runs on battery, and its changes.
pub trait PowerSource: Send + Sync {
    /// Whether the machine runs on battery now. Does not block.
    fn on_battery(&self) -> bool;

    /// Calls `f` on any thread when the power source changes; `f` only
    /// queues work and never blocks. Dropping the returned value
    /// unsubscribes.
    fn subscribe(&self, f: Box<dyn Fn(bool) + Send + Sync>) -> Box<dyn Send>;
}

/// Always on AC power: the only source `app` passes (ADR 0032).
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
