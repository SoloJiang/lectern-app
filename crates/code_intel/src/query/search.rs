//! The time-boxed search (「限时搜索」): the current sub-project's files that
//! are not indexed, nearest first, read and searched for the name as a whole
//! word until one second after the query started. Files being extracted
//! right now are not searched and count as unsearched.

use std::collections::HashSet;
use std::time::Instant;

use lectern_services::code_intel::Completeness;
use lectern_services::project::{BlobId, RelPath};

use super::rank::distance;
use super::{Run, Snap};
use crate::extract::{BACKGROUND_DEADLINE, ExtractStatus, Lang, compatible};
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
    /// limit. Such a file is neither stored nor searched: it counts as
    /// unsearched ([`Searched::Unsearched`]).
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
}

/// What a query learned about one file it read.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Searched {
    /// Searched: its definitions or references are known, or it has none.
    Done,
    /// Cut short by the query's time or cancelled: counts as unsearched.
    Unsearched,
}

/// The files a search covers.
pub(crate) struct Scope {
    /// Unindexed files of the sub-project, nearest first.
    pub files: Vec<Effective>,
    /// The sub-project's directory ("" for the whole worktree).
    pub dir: String,
    /// Its files being extracted right now.
    pub in_flight: usize,
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

    fn searchable(&self, eff: &Effective, lang: &Lang) -> bool {
        !self.disabled.contains(eff.lang.name())
            && compatible(&self.table, eff.lang.name(), lang.name())
    }

    fn in_flight(&self, eff: &Effective) -> bool {
        self.in_flight.contains(&(eff.oid, eff.lang.name().clone()))
    }

    /// Every file of the map under `dir` (or not under it, `inside` false).
    fn each(&self, dir: &str, inside: bool, mut f: impl FnMut(Effective)) {
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
    /// deepest sub-project holding it, the whole worktree without one.
    pub fn search_scope(
        &self,
        anchor: &str,
        lang: &Lang,
        skip: &HashSet<(RelPath, BlobId)>,
    ) -> Scope {
        let dir = self.subproject_of(anchor).unwrap_or("").to_owned();
        let mut files = Vec::new();
        let mut in_flight = 0;
        self.each(&dir, true, |eff| {
            if !self.searchable(&eff, lang) || !self.unindexed(&eff) {
                return;
            }
            if skip.contains(&(eff.path.clone(), eff.oid)) {
                return;
            }
            if self.in_flight(&eff) {
                in_flight += 1;
            } else {
                files.push(eff);
            }
        });
        // `distance` allocates: one key per file, not one per comparison.
        files.sort_by_cached_key(|e| (distance(anchor, &e.path), e.path.clone()));
        Scope {
            files,
            dir,
            in_flight,
        }
    }

    /// Unindexed files in `lang`'s group outside `dir` (`Partial::outside`).
    pub fn unindexed_outside(&self, dir: &str, lang: Option<&Lang>) -> u32 {
        if dir.is_empty() {
            return 0;
        }
        let mut n = 0u32;
        self.each(dir, false, |eff| {
            let fits = lang.is_none_or(|l| self.searchable(&eff, l));
            if fits && self.unindexed(&eff) {
                n = n.saturating_add(1);
            }
        });
        n
    }

    /// Unindexed files of every language under `dir`, and those in flight
    /// (`Named`'s completeness).
    pub fn unindexed_under(&self, dir: &str) -> (u32, u32) {
        let (mut n, mut flying) = (0u32, 0u32);
        self.each(dir, true, |eff| {
            if self.disabled.contains(eff.lang.name()) || !self.unindexed(&eff) {
                return;
            }
            if self.in_flight(&eff) {
                flying = flying.saturating_add(1);
            } else {
                n = n.saturating_add(1);
            }
        });
        (n, flying)
    }
}

impl Inner {
    /// Searches `scope` for `name` until `until`: every whole-word hit is
    /// handed to `hit` with the bytes read, their key and `until`, and says
    /// whether the file was searched. Returns the completeness: `Complete`
    /// when every file was searched and none was in flight; files not
    /// reached, in flight or cut short (`Searched::Unsearched`) are
    /// `unsearched`.
    #[allow(
        clippy::too_many_arguments,
        reason = "the search's scope, its word and its time box"
    )]
    pub(crate) fn search(
        &self,
        run: &Run,
        snap: &Snap,
        scope: &Scope,
        lang: &Lang,
        name: &str,
        until: Instant,
        mut hit: impl FnMut(&Effective, BlobId, Vec<u8>, Instant) -> Searched,
    ) -> Option<Completeness> {
        let clock = self.watchdog.clock().clone();
        let mut searched = 0usize;
        let mut cut = 0usize;
        for eff in &scope.files {
            if run.stopped(snap) {
                return None;
            }
            if clock.now() >= until {
                break;
            }
            searched += 1;
            let Some(bytes) = self.read_file(snap, &eff.path) else {
                continue;
            };
            if !whole_word(&bytes, name) {
                continue;
            }
            let key = self.key_of(snap, &eff.path, &bytes, eff.oid);
            if hit(eff, key, bytes, until) == Searched::Unsearched {
                cut += 1;
            }
        }
        if run.stopped(snap) {
            return None;
        }
        let left = scope.files.len() - searched + cut;
        if left == 0 && scope.in_flight == 0 {
            return Some(Completeness::Complete);
        }
        Some(Completeness::Partial {
            unsearched: u32::try_from(left + scope.in_flight).unwrap_or(u32::MAX),
            outside: snap.unindexed_outside(&scope.dir, Some(lang)),
        })
    }
}
