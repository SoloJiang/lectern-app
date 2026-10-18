//! Grammars from language packs (ADR 0018): the interface the kernel's
//! grammar loader implements (docs/design/code_intel.md「本 crate 的公开项」).

use std::any::Any;
use std::fmt;
use std::sync::Arc;

use crate::LangName;

/// Language-pack grammars, loaded and verified by the kernel's grammar loader.
pub trait GrammarSource: Send + Sync {
    /// A loaded grammar. Blocking; called on the executor only.
    fn grammar(&self, lang: &LangName) -> Option<LoadedGrammar>;
    /// The language a pack claims an extension for, e.g. "kt" → "kotlin".
    /// Never blocks.
    fn language_for_extension(&self, ext: &str) -> Option<LangName>;
    /// The generation of the installed set: install, update, rollback and
    /// removal each add one. Never blocks.
    fn generation(&self) -> u64;
    /// The languages of the installed packs, in any order. Never blocks.
    /// A grammar round reads it, so packs installed before activation (or
    /// a notification that was missed) are picked up.
    fn installed(&self) -> Vec<LangName>;
    /// Calls `f` on any thread when the installed set changes; `f` only
    /// queues. Dropping the result unsubscribes.
    fn subscribe(&self, f: Box<dyn Fn(GrammarChange) + Send + Sync>) -> Box<dyn Send>;
}

/// One change of the installed set.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct GrammarChange {
    pub generation: u64,
    pub lang: LangName,
    pub kind: GrammarChangeKind,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GrammarChangeKind {
    /// A new pack; it starts claiming its extensions.
    Installed,
    /// A new version: `artifact_sha256` changed.
    Updated,
    /// The new version failed to activate; back to the previous one.
    RolledBack,
    /// Revoked or uninstalled: stop using it now (ADR 0018「撤销」).
    Removed,
}

/// A grammar the loader handed out.
#[derive(Clone)]
pub struct LoadedGrammar {
    /// Keeps the shared library loaded; see [`GrammarLease`].
    pub lease: GrammarLease,
    pub language: tree_sitter::Language,
    pub tags_query: Arc<str>,
    pub locals_query: Option<Arc<str>>,
    /// The pack's sha256 (ADR 0018's index entry); its first 8 bytes are the
    /// grammar version.
    pub artifact_sha256: [u8; 32],
    pub compat_group: Option<Arc<str>>,
}

impl fmt::Debug for LoadedGrammar {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoadedGrammar")
            .field("artifact_sha256", &self.artifact_sha256)
            .field("compat_group", &self.compat_group)
            .finish_non_exhaustive()
    }
}

/// A lease on a loaded grammar library. Clones only count references; the
/// loader unloads the library after the last one drops. Core grammars are
/// linked statically and have an empty lease.
#[derive(Clone, Default)]
pub struct GrammarLease(Option<Arc<dyn Any + Send + Sync>>);

impl GrammarLease {
    /// The empty lease of a statically linked grammar.
    pub fn none() -> GrammarLease {
        GrammarLease(None)
    }

    /// A lease holding the loader's module handle.
    pub fn new(handle: Arc<dyn Any + Send + Sync>) -> GrammarLease {
        GrammarLease(Some(handle))
    }

    /// A statically linked grammar: nothing is ever unloaded.
    pub fn is_static(&self) -> bool {
        self.0.is_none()
    }
}

impl fmt::Debug for GrammarLease {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(if self.0.is_some() {
            "GrammarLease(module)"
        } else {
            "GrammarLease(static)"
        })
    }
}

/// No language packs: what `app` passes until the plugin marketplace exists.
#[derive(Clone, Copy, Default, Debug)]
pub struct NoGrammarPacks;

impl GrammarSource for NoGrammarPacks {
    fn grammar(&self, _: &LangName) -> Option<LoadedGrammar> {
        None
    }

    fn language_for_extension(&self, _: &str) -> Option<LangName> {
        None
    }

    fn generation(&self) -> u64 {
        0
    }

    fn installed(&self) -> Vec<LangName> {
        Vec::new()
    }

    fn subscribe(&self, _: Box<dyn Fn(GrammarChange) + Send + Sync>) -> Box<dyn Send> {
        Box::new(())
    }
}
