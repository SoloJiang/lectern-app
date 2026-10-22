//! Logging helpers (`docs/design/kernel.md`, 日志与诊断): the phase of a
//! `plugin failed` record and the `plugin` / `act` scope.

use std::borrow::Cow;

use lectern_logging::{Ctx, Field, PluginTag, Scope, Value};

use crate::plugin::Name;

/// The plugin code that was running when a panic was caught: the `phase`
/// field of `plugin failed`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    Manifest,
    Activate,
    /// `Staged::commit`.
    Commit,
    /// `started`, and the events and `start`s held until the activation went
    /// live.
    Started,
    /// The `start` of `PluginHandle::own_with`.
    OwnWith,
    Listener,
    /// The `done` of a command request.
    Command,
    /// `WorkspaceFacts::contains`.
    Workspace,
    /// A value the kernel releases: a guard, a task handle, the `Active`
    /// state, a rejected or replaced value.
    Drop,
}

impl Phase {
    fn name(self) -> &'static str {
        match self {
            Phase::Manifest => "manifest",
            Phase::Activate => "activate",
            Phase::Commit => "commit",
            Phase::Started => "started",
            Phase::OwnWith => "own_with",
            Phase::Listener => "listener",
            Phase::Command => "command",
            Phase::Workspace => "workspace",
            Phase::Drop => "drop",
        }
    }
}

impl Field for Phase {
    fn value(&self) -> Value<'_> {
        Value::text(self.name())
    }
}

/// The log tag of a plugin ID. A built-in plugin's ID is `&'static str`; an
/// ID read at runtime (a process plugin's) is registered once, and is None
/// when `PluginTag::register` refuses it.
pub(crate) fn plugin_tag(id: &Name) -> Option<PluginTag> {
    match id {
        Cow::Borrowed(id) => Some(PluginTag::new(id)),
        Cow::Owned(id) => PluginTag::register(id),
    }
}

/// The scope of a plugin's code outside any activation (reading its
/// manifest, dropping the plugin object).
pub(crate) fn plugin_scope(tag: Option<PluginTag>) -> Scope {
    lectern_logging::scope(Ctx {
        plugin: tag,
        ..Ctx::default()
    })
}
