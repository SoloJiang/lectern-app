//! The time-boxed search (「限时搜索」): the current sub-project's files that
//! are not indexed, nearest first, read and searched for the name as a whole
//! word until one second after the query started. Files being extracted
//! right now are not searched. What it reads goes into the query's
//! [`Account`]; the accounting decides the completeness.

use std::collections::HashSet;
use std::time::Instant;

use lectern_services::project::{BlobId, RelPath};

use super::account::{Account, Reason, Stops};
use super::rank::distance;
use super::{Run, Snap};
use crate::extract::{
    BACKGROUND_DEADLINE, BACKGROUND_MAX_LEN, ExtractStatus, INTERACTIVE_DEADLINE,
    INTERACTIVE_MAX_LEN, Lang, compatible,
};
use crate::index::Inner;
use crate::index::state::under;
use crate::map::Effective;

/// Files whose OIDs the first batch's lookup of a common name reads
/// ([`Snap::near_oids`]): four statements of 500 OIDs.
pub const NEAR_FILES: usize = 2048;

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

    /// The deadline of a file read under tier 1's limits or not
    /// (`FileLimits::interactive`, [`Snap::file_limits`]): the interactive
    /// one when its unit was or may have been made under them, the
    /// background one otherwise; cut at `until`.
    pub(crate) fn of(interactive: bool, now: Instant, until: Option<Instant>) -> FileDeadline {
        if interactive {
            FileDeadline::interactive(now, until)
        } else {
            FileDeadline::new(now, until)
        }
    }

    /// The interactive limit from now (tier 1's): the text a query is about
    /// (the definition's own file of find references), and files read under
    /// tier 1's limits ([`FileLimits`]), cut at `until`.
    pub(crate) fn interactive(now: Instant, until: Option<Instant>) -> FileDeadline {
        let full = now + INTERACTIVE_DEADLINE;
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

/// The limits a query reads and parses one file of the map under: those
/// its unit was, or may have been, made under (「限时搜索」). One rule for
/// the read limit and the deadline, so a file is never read under one
/// tier's limit and parsed under the other's.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct FileLimits {
    /// The most bytes read.
    pub max_len: usize,
    /// Tier 1's limits (4 MiB, 1 s) rather than the background's (1 MiB,
    /// 250 ms).
    pub interactive: bool,
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

    /// The limits of a file for a query: those its unit was (or is being,
    /// or would be) extracted under. A file with a unit (its hint matches:
    /// done under either tier's limits, or `TooLarge` or `TimedOut` under the
    /// background's and redone under tier 1's) or in tier 1 takes the
    /// interactive limits (4 MiB, 1 s); the others are the background
    /// tiers' (1 MiB, 250 ms).
    pub fn file_limits(&self, eff: &Effective) -> FileLimits {
        let interactive = !self.unindexed(eff) || self.focused.contains(&eff.path);
        FileLimits {
            max_len: if interactive {
                INTERACTIVE_MAX_LEN
            } else {
                BACKGROUND_MAX_LEN
            },
            interactive,
        }
    }

    /// The read limit of a file for a query ([`file_limits`](Self::file_limits)).
    pub fn read_limit(&self, eff: &Effective) -> usize {
        self.file_limits(eff).max_len
    }

    /// The OIDs of the files nearest `from` in the languages `part` admits,
    /// for the first batch's lookup of a name many units define (「候选的分层
    /// 与过滤」), in the ranker's order and in two parts, each looked up by
    /// its own statements so that one statement's row limit never trades a
    /// near file for a far one: `from`'s directory's own files (not its
    /// subdirectories; worktree changes included), those in the review set
    /// (`in_review`) first; then the other files of its sub-project (of its
    /// directory's subtree without one; nothing more at the root), the
    /// review set first, then by path distance from `from`. At most
    /// [`NEAR_FILES`] in all, each once.
    pub fn near_oids(
        &self,
        from: &str,
        part: &dyn Fn(&Lang) -> bool,
        in_review: &dyn Fn(&str) -> bool,
    ) -> (Vec<BlobId>, Vec<BlobId>) {
        let mut seen = HashSet::new();
        let dir = from.rsplit_once('/').map_or("", |(d, _)| d);
        let mut admit = |l: &Lang| part(l);
        let own = |path: &str| {
            let rest = if dir.is_empty() {
                Some(path)
            } else {
                path.strip_prefix(dir).and_then(|r| r.strip_prefix('/'))
            };
            rest.is_some_and(|r| !r.contains('/'))
        };
        // (not in review, distance, walk order, OID): one key per file.
        let mut siblings: Vec<(bool, usize, usize, BlobId)> = Vec::new();
        self.map.each_file(dir, true, &mut admit, |f| {
            let path = f.path();
            if let Some(oid) = f.oid
                && own(path)
            {
                siblings.push((!in_review(path), 0, siblings.len(), oid));
            }
        });
        siblings.sort_unstable();
        let siblings: Vec<BlobId> = siblings
            .into_iter()
            .map(|(.., oid)| oid)
            .filter(|oid| seen.insert(*oid))
            .take(NEAR_FILES)
            .collect();
        let scope = self
            .subproject_of(from)
            .or((!dir.is_empty()).then_some(dir));
        let room = NEAR_FILES - siblings.len();
        let mut rest: Vec<(bool, usize, usize, BlobId)> = Vec::new();
        if let Some(scope) = scope
            && room > 0
        {
            self.map.each_file(scope, true, &mut admit, |f| {
                let path = f.path();
                if let Some(oid) = f.oid
                    && !own(path)
                    && !seen.contains(&oid)
                {
                    let key = (!in_review(path), distance(from, path), rest.len(), oid);
                    rest.push(key);
                }
            });
        }
        // The nearest `room` (and a few more for repeated OIDs), in order.
        let keep = (2 * room).min(rest.len());
        if keep < rest.len() {
            rest.select_nth_unstable(keep);
            rest.truncate(keep);
        }
        rest.sort_unstable();
        let rest: Vec<BlobId> = rest
            .into_iter()
            .map(|(.., oid)| oid)
            .filter(|oid| seen.insert(*oid))
            .take(room)
            .collect();
        (siblings, rest)
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
    /// right now, nearest first; those stored `TooLarge` or `TimedOut` too
    /// (their units hold nothing). Once the namespace is full a matching hint
    /// does not prove its unit: the hinted files of the scope are checked
    /// against the store first (`verify_hinted`, stopped by `stops`), and
    /// those missing are searched too (those left unchecked as well, while
    /// time remains). None when the query stopped.
    pub fn search_scope(
        &self,
        anchor: &str,
        lang: &Lang,
        skip: &HashSet<(RelPath, BlobId)>,
        account: &mut Account,
        stops: &Stops<'_>,
    ) -> Option<Scope> {
        let dir = self.subproject_of(anchor).unwrap_or("").to_owned();
        let mut files = Vec::new();
        let mut hinted = Vec::new();
        self.each(&dir, true, |eff| {
            if !self.searchable(&eff, lang) || skip.contains(&(eff.path.clone(), eff.oid)) {
                return;
            }
            if !self.unindexed(&eff) {
                // Stored without its definitions (`TooLarge`, `TimedOut`):
                // read and searched, up to the interactive limit.
                if self.unfinished(&eff) {
                    if !self.in_flight(&eff) {
                        files.push(eff);
                    }
                } else if self.full {
                    hinted.push(eff);
                }
                return;
            }
            if !self.in_flight(&eff) {
                files.push(eff);
            }
        });
        if !self.verify_hinted(account, &hinted, stops) {
            return None;
        }
        files.extend(
            hinted
                .into_iter()
                .filter(|e| !self.indexed(account, e) && !self.in_flight(e)),
        );
        // `distance` allocates: one key per file, not one per comparison.
        files.sort_by_cached_key(|e| (distance(anchor, &e.path), e.path.clone()));
        Some(Scope { files, dir })
    }
}

impl Inner {
    /// Searches `scope` for `name` until `until`: every whole-word hit is
    /// handed to `hit` with the bytes read, their key and `until`, and says
    /// whether the file was searched; `tick` runs after every file, hit or
    /// not (the caller's timed batches). Every file read goes into
    /// `account` (searched, or why not); a file not reached is left to the
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
        mut tick: impl FnMut(),
    ) -> bool {
        let clock = self.watchdog.clock().clone();
        for eff in &scope.files {
            if run.stopped(snap) {
                return false;
            }
            if clock.now() >= until {
                break;
            }
            match self.read_content(snap, eff, snap.read_limit(eff)) {
                Err(reason) => account.unsearched(&eff.path, eff.oid, reason),
                Ok(bytes) if !whole_word(&bytes, name) => account.searched(&eff.path, eff.oid),
                Ok(bytes) => match hit(eff, eff.oid, bytes, until) {
                    Searched::Done => account.searched(&eff.path, eff.oid),
                    Searched::Unsearched(reason) => {
                        account.unsearched(&eff.path, eff.oid, reason);
                    }
                },
            }
            tick();
        }
        !run.stopped(snap)
    }
}
