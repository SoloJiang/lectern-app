//! tree-sitter symbol index, Java and TypeScript resolution, dependency sources.
//!
//! Steps 1–4 of docs/design/code_intel.md「验收」: the language registry,
//! extraction keyed by (blob OID, language, grammar version, query version),
//! the per-file deadline watchdog, and the reference-site analysis of one
//! parsed file (step 1); the storage of extractions in `cache`'s custom
//! tables (`store`, step 2); the blob → path map of the current worktree
//! (`map`), its incremental updates and the five indexing tiers (`index`,
//! step 3); the queries (`query`), the ⌘K name table (`names`), semantic
//! navigation's arbitration (`semantic`) and the `code-intel` plugin
//! (`plugin`, step 4).

mod canonical;
mod forms;
mod git_access;
mod grammar;
mod lang;
mod locals;
mod resolve;
mod rules;
mod semantic;
mod util;
mod walk;
mod watchdog;

pub mod extract;
pub mod index;
pub mod map;
pub mod names;
pub mod phases;
pub mod plugin;
pub mod query;
pub mod site;
pub mod store;

#[cfg(any(test, feature = "test-support"))]
pub mod testing;

#[cfg(test)]
mod tests;

pub use crate::grammar::{
    GrammarChange, GrammarChangeKind, GrammarLease, GrammarSource, LoadedGrammar, NoGrammarPacks,
};
pub use crate::plugin::{CodeIntelDeps, CodeIntelPlugin, CodeIntelService};
pub use crate::watchdog::{
    CHECK_INTERVAL, Clock, Due, PauseProbe, Starter, SystemClock, WATCHDOG_THREAD, Watchdog,
    WatchdogRun, WatchdogStop,
};
/// The shared types of `services::code_intel` this crate's own interface
/// uses.
pub use lectern_services::code_intel::{DefKind, LangName, Position, RefSyntax};

/// A [`LangName`] from a short name.
pub fn lang_name(name: &str) -> LangName {
    LangName(name.into())
}

/// Every [`DefKind`] in declaration order: the bit order of
/// `NameTable::kinds` and the stored `ci_def.kind` code.
pub const DEF_KINDS: [DefKind; 14] = [
    DefKind::Function,
    DefKind::Method,
    DefKind::Class,
    DefKind::Interface,
    DefKind::Struct,
    DefKind::Enum,
    DefKind::Trait,
    DefKind::Type,
    DefKind::Module,
    DefKind::Macro,
    DefKind::Constant,
    DefKind::Field,
    DefKind::Variable,
    DefKind::Other,
];

/// The kind named after `definition.` in a tags query; unknown names are
/// `Other`.
pub fn def_kind_from_tag(kind: &str) -> DefKind {
    match kind {
        "function" => DefKind::Function,
        "method" => DefKind::Method,
        "class" => DefKind::Class,
        "interface" => DefKind::Interface,
        "struct" => DefKind::Struct,
        "enum" => DefKind::Enum,
        "trait" => DefKind::Trait,
        "type" => DefKind::Type,
        "module" => DefKind::Module,
        "macro" => DefKind::Macro,
        "constant" => DefKind::Constant,
        "field" => DefKind::Field,
        "variable" => DefKind::Variable,
        _ => DefKind::Other,
    }
}

/// The bit of a kind in a `u16` kind set (`NameTable::kinds`).
pub const fn def_kind_bit(kind: DefKind) -> u16 {
    1 << kind as u16
}

/// The tags capture name of a kind ("function", …, "other").
pub fn def_kind_name(kind: DefKind) -> &'static str {
    match kind {
        DefKind::Function => "function",
        DefKind::Method => "method",
        DefKind::Class => "class",
        DefKind::Interface => "interface",
        DefKind::Struct => "struct",
        DefKind::Enum => "enum",
        DefKind::Trait => "trait",
        DefKind::Type => "type",
        DefKind::Module => "module",
        DefKind::Macro => "macro",
        DefKind::Constant => "constant",
        DefKind::Field => "field",
        DefKind::Variable => "variable",
        DefKind::Other => "other",
    }
}
