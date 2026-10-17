//! tree-sitter symbol index, Java and TypeScript resolution, dependency sources.
//!
//! This is step 1 of docs/design/code_intel.md「验收」: the language registry,
//! extraction keyed by (blob OID, language, grammar version, query version),
//! the per-file deadline watchdog, and the reference-site analysis of one
//! parsed file. Storage, the blob → path map, scheduling, queries and the
//! plugin are later steps.

mod grammar;
mod lang;
mod rules;
mod util;
mod watchdog;

pub mod extract;
pub mod site;

#[cfg(any(test, feature = "test-support"))]
pub mod testing;

#[cfg(test)]
mod tests;

use std::fmt;
use std::sync::Arc;

pub use crate::grammar::{
    GrammarChange, GrammarChangeKind, GrammarLease, GrammarSource, LoadedGrammar, NoGrammarPacks,
};
pub use crate::watchdog::{
    CHECK_INTERVAL, Clock, PauseProbe, Starter, SystemClock, WATCHDOG_THREAD, Watchdog,
    WatchdogRun, WatchdogStop,
};

/// A language's stable short name: "ts", "tsx", "js", "rust", "java",
/// "python", "go"; language packs register theirs.
///
/// Defined here until `services::code_intel` exists (prerequisite A), which
/// then owns it and this crate re-exports it.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct LangName(pub Arc<str>);

impl LangName {
    pub fn new(name: &str) -> LangName {
        LangName(Arc::from(name))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for LangName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The kind of a definition, from the tags capture `@definition.<kind>`.
/// The declaration order is the bit order of `NameTable::kinds` and the
/// stored `ci_def.kind` code: append only.
///
/// Defined here until `services::code_intel` exists (prerequisite A).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum DefKind {
    Function,
    Method,
    Class,
    Interface,
    Struct,
    Enum,
    Trait,
    Type,
    Module,
    Macro,
    Constant,
    Field,
    Variable,
    Other,
}

impl DefKind {
    pub const ALL: [DefKind; 14] = [
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
    pub fn from_tag_kind(kind: &str) -> DefKind {
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

    /// The position in the declaration order.
    pub fn code(self) -> u8 {
        self as u8
    }

    /// The bit of this kind in a `u16` kind set.
    pub const fn bit(self) -> u16 {
        1 << self as u16
    }

    pub fn as_str(self) -> &'static str {
        match self {
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
}

/// The syntax of a reference site; decides which definition kinds it accepts
/// ([`site::accepts`]).
///
/// Defined here until `services::code_intel` exists (prerequisite A).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum RefSyntax {
    Plain,
    Call,
    MemberCall,
    Member,
    Type,
    Construct,
}
