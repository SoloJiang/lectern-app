//! The time-boxed search (「限时搜索」): the current sub-project's files that
//! are not indexed, nearest first, read and searched for the name as a whole
//! word until one second after the query started. Files being extracted
//! right now are not searched. What it reads goes into the query's
//! [`Account`]; the accounting decides the completeness.

use std::collections::HashSet;
use std::time::Instant;

use lectern_services::project::{BlobId, RelPath};

use super::account::{Account, Reason};
use super::rank::distance;
use super::{Run, Snap};
use crate::extract::{
    BACKGROUND_DEADLINE, BACKGROUND_MAX_LEN, ExtractStatus, INTERACTIVE_MAX_LEN, Lang, compatible,
};
use crate::index::Inner;
use crate::index::state::under;
use crate::map::Effective;

/// One file's time in a query (step 2's reads, the time-boxed search, the
/// references' parses): the background limit from now, cut at the query's
/// own end when that comes first. Every query path that reads a file for
/// a query takes its deadline from here.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct FileDeadline {
    pub at: Instant,
    /// The query's end came before the background limit.
    cut: bool,
}

impl FileDeadline {
    pub(crate) fn new(now: Instant, until: Option<Instant>) -> FileDeadline {
        let full = now + BACKGROUND_DEADLINE;
        match until {
            Some(until) if until < full => FileDeadline {
                at: until,
                cut: true,
            },
            _ => FileDeadline {
                at: full,
                cut: false,
            },
        }
    }

    /// Whether an extraction with `status` made under this deadline says
    /// something about the file, so that it may be stored like the batch
    /// tiers' results. `Cancelled` never does, and neither does `TimedOut`
    /// under a cut deadline: the query's time ran out, not the file's
    /// limit. Such a file is neither stored nor searched.
    pub(crate) fn settles(&self, status: ExtractStatus) -> bool {
        match status {
            ExtractStatus::Cancelled => false,
            ExtractStatus::TimedOut => !self.cut,
            ExtractStatus::Done
            | ExtractStatus::TooLarge
            | ExtractStatus::Binary
            | ExtractStatus::Unsupported => true,
        }
    }

    /// Why a file whose extraction or parse ended with `status` was not
    /// searched; None for `Done`.
    pub(crate) fn reason(&self, status: ExtractStatus) -> Option<Reason> {
        match status {
            ExtractStatus::Done => None,
            ExtractStatus::Cancelled => Some(Reason::Cancelled),
            ExtractStatus::TimedOut if self.cut => Some(Reason::Cut),
            ExtractStatus::TimedOut => Some(Reason::TimedOut),
            ExtractStatus::TooLarge => Some(Reason::TooLarge),
            ExtractStatus::Binary => Some(Reason::Binary),
            ExtractStatus::Unsupported => Some(Reason::Unreadable),
        }
    }
}

/// What a query learned about one file it read.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Searched {
    /// Searched: its definitions or references are known, or it has none.
    Done,
    /// Not searched, and why.
    Unsearched(Reason),
}

/// The files a search covers.
pub(crate) struct Scope {
    /// The sub-project's files to search, nearest first: not indexed, or
    /// (full mode) hinted with their unit missing.
    pub files: Vec<Effective>,
    /// The sub-project's directory ("" for the whole worktree).
    pub dir: String,
}

/// Whether `name` occurs in `bytes` as a whole word: the bytes around it
/// are not identifier characters (ASCII letters, digits, `_`, `$`, or part
/// of a UTF-8 multi-byte sequence).
pub fn whole_word(bytes: &[u8], name: &str) -> bool {
    let needle = name.as_bytes();
    if needle.is_empty() {
        return false;
    }
    let ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'$' || b >= 0x80;
    memchr::memmem::find_iter(bytes, needle).any(|at| {
        let before = at.checked_sub(1).map(|i| bytes[i]);
        let after = bytes.get(at + needle.len()).copied();
        !before.is_some_and(ident) && !after.is_some_and(ident)
    })
}

impl Snap {
    /// Whether a file's content is not known to be indexed: its hint does
    /// not match its effective OID and the map's generations.
    pub fn unindexed(&self, eff: &Effective) -> bool {
        !self.map.hints.matches(&eff.slot, &eff.oid, self.stamp)
    }

    /// Whether a file is in `lang`'s group and its language enabled: the
    /// files a jump or find references in `lang` accounts for.
    pub fn searchable(&self, eff: &Effective, lang: &Lang) -> bool {
        self.in_group(&eff.lang, lang)
    }

    /// Whether `file`'s language is enabled and in `lang`'s group.
    pub fn in_group(&self, file: &Lang, lang: &Lang) -> bool {
        !self.disabled.contains(file.name()) && compatible(&self.table, file.name(), lang.name())
    }

    pub(super) fn in_flight(&self, eff: &Effective) -> bool {
        self.in_flight.contains(&(eff.oid, eff.lang.name().clone()))
    }

    /// The read limit of a file for a query: the limit its unit was (or is
    /// being) extracted under. A file with a unit (its hint matches) or in
    /// tier 1 may be up to the interactive limit (4 MiB); the others are
    /// extracted by the background tiers (1 MiB).
    pub fn read_limit(&self, eff: &Effective) -> usize {
        if !self.unindexed(eff) || self.focused.contains(&eff.path) {
            INTERACTIVE_MAX_LEN
        } else {
            BACKGROUND_MAX_LEN
        }
    }

    /// Every file of the map under `dir` (or not under it, `inside` false).
    pub(super) fn each(&self, dir: &str, inside: bool, mut f: impl FnMut(Effective)) {
        let files = self.map.base.files();
        let range = if inside {
            files.range_under(dir)
        } else {
            0..files.len()
        };
        for i in range {
            if !inside && files.get(i).is_some_and(|(p, _)| under(p, dir)) {
                continue;
            }
            if let Some(eff) = self.map.base_effective(i) {
                f(eff);
            }
        }
        for (i, (path, _)) in self.map.overlay.entries().iter().enumerate() {
            if under(path, dir) != inside {
                continue;
            }
            // A `Content` entry: tracked (hiding its index entry) or not.
            if let Some(eff) = self.map.overlay_effective(i) {
                f(eff);
            }
        }
    }

    /// The search's scope for a reference at `anchor` in `lang`: the
    /// deepest sub-project holding it, the whole worktree without one; its
    /// files in `lang`'s group that are not indexed and not being extracted
    /// right now, nearest first. Once the namespace is full a matching hint
    /// does not prove its unit: the hinted files of the scope are checked
    /// against the store first (`verify_hinted`), and those missing are
    /// searched too.
    pub fn search_scope(
        &self,
        anchor: &str,
        lang: &Lang,
        skip: &HashSet<(RelPath, BlobId)>,
        account: &mut Account,
    ) -> Scope {
        let dir = self.subproject_of(anchor).unwrap_or("").to_owned();
        let mut files = Vec::new();
        let mut hinted = Vec::new();
        self.each(&dir, true, |eff| {
            if !self.searchable(&eff, lang) || skip.contains(&(eff.path.clone(), eff.oid)) {
                return;
            }
            if !self.unindexed(&eff) {
                if self.full {
                    hinted.push(eff);
                }
                return;
            }
            if !self.in_flight(&eff) {
                files.push(eff);
            }
        });
        self.verify_hinted(account, &hinted);
        files.extend(
            hinted
                .into_iter()
                .filter(|e| !self.indexed(account, e) && !self.in_flight(e)),
        );
        // `distance` allocates: one key per file, not one per comparison.
        files.sort_by_cached_key(|e| (distance(anchor, &e.path), e.path.clone()));
        Scope { files, dir }
    }
}

impl Inner {
    /// Searches `scope` for `name` until `until`: every whole-word hit is
    /// handed to `hit` with the bytes read, their key and `until`, and says
    /// whether the file was searched. Every file read goes into `account`
    /// (searched, or why not); a file not reached is left to the
    /// accounting. False when the query stopped.
    #[allow(
        clippy::too_many_arguments,
        reason = "the search's scope, its word, its time box and its account"
    )]
    pub(crate) fn search(
        &self,
        run: &Run,
        snap: &Snap,
        scope: &Scope,
        name: &str,
        until: Instant,
        account: &mut Account,
        mut hit: impl FnMut(&Effective, BlobId, Vec<u8>, Instant) -> Searched,
    ) -> bool {
        let clock = self.watchdog.clock().clone();
        for eff in &scope.files {
            if run.stopped(snap) {
                return false;
            }
            if clock.now() >= until {
                break;
            }
            let bytes = match self.read_content(snap, eff, snap.read_limit(eff)) {
                Ok(bytes) => bytes,
                Err(reason) => {
                    account.unsearched(&eff.path, eff.oid, reason);
                    continue;
                }
            };
            if !whole_word(&bytes, name) {
                account.searched(&eff.path, eff.oid);
                continue;
            }
            match hit(eff, eff.oid, bytes, until) {
                Searched::Done => account.searched(&eff.path, eff.oid),
                Searched::Unsearched(reason) => account.unsearched(&eff.path, eff.oid, reason),
            }
        }
        !run.stopped(snap)
    }
}
