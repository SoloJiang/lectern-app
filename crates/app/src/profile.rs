//! Profiles: which plugins a run assembles and how its startup barrier is
//! released (docs/design/app.md「接口」, docs/design/kernel.md「启动」).

use lectern_kernel::PluginSet;
use lectern_logging::{Field, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    /// What the release build runs.
    Desktop,
    /// `#[gpui::test]`: no real frames, so the barrier is released as soon as
    /// the window exists.
    Test,
}

impl Profile {
    #[cfg(test)]
    pub const ALL: [Profile; 2] = [Profile::Desktop, Profile::Test];

    /// The plugins this profile assembles. Empty for now: assembling the
    /// first plugin is its own issue (docs/design/app.md「范围」).
    pub fn plugins(self) -> PluginSet {
        match self {
            Profile::Desktop | Profile::Test => PluginSet::new(),
        }
    }
}

impl Field for Profile {
    fn value(&self) -> Value<'_> {
        Value::text(match self {
            Profile::Desktop => "desktop",
            Profile::Test => "test",
        })
    }
}
