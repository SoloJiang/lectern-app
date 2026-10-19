//! A query's completeness (「完整性的核算」): one accounting of every file
//! that could take part in its answer. Each file of the snapshot's map in the
//! query's languages, whatever its tier (the same file, the same directory,
//! the sub-project, a dependent sub-project, elsewhere), is classified once:
//!
//! - `Indexed`: its unit is in the store under the query's view with the
//!   file's definitions and references (its hint matches; its unit is not
//!   `TooLarge` or `TimedOut`; once the namespace is full, the store was
//!   checked);
//! - `Searched`: the query read it and looked at its content;
//! - `Unsearched(reason)`.
//!
//! `Complete` means nothing is `Unsearched`; nothing else decides it. The
//! walk ([`Snap::walk`]) reads every hint and fixes the class of every file
//! read so far, before the query's last view check; [`Snap::finish`] reads
//! no hint: a file read after the walk (a lookup made again) is counted only
//! when it could not be searched.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use lectern_services::code_intel::Completeness;
use lectern_services::project::{BlobId, RelPath};

use super::Snap;
use crate::LangName;
use crate::extract::{ExtractStatus, Lang};
use crate::index::lock;
use crate::index::state::{Unfinished, under};
use crate::map::{Effective, HintSlot, HintStamp};
use crate::store::{EXISTS_BATCH, Version};

/// OIDs per statement of the full-mode check outside the search's scope:
/// the batch layer's existence statement, the query's stop and time box
/// checked between two. Two in the tests, so a few files make several.
const VERIFY_CHUNK: usize = if cfg!(test) { 2 } else { EXISTS_BATCH };

/// Why a file that could take part was not searched.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub(crate) enum Reason {
    /// In the search's scope, not reached before the query's time ran out
    /// (`Named` searches nothing: every unindexed file of its scope).
    NotReached,
    /// Read, but its extraction or parse was cut short by the query's time.
    Cut,
    /// Its extraction or parse was cancelled.
    Cancelled,
    /// Its extraction or parse hit the file's own deadline, or its stored
    /// unit is `TimedOut`.
    TimedOut,
    /// Changed since the snapshot (or gone): not the snapshot's content.
    Stale,
    /// Over the read limit, or its stored unit is `TooLarge`.
    TooLarge,
    /// Binary content, read by the query.
    Binary,
    /// Reading it failed, or its grammar did.
    Unreadable,
    /// The worktree has it, over 1 MiB or unreadable when it was read
    /// (`WorktreeState::Unindexable`): its content is not known.
    Unindexable,
    /// Being extracted right now: neither in the index nor searched.
    InFlight,
    /// Outside the search's scope and not indexed.
    Outside,
    /// Its hint matches, but its unit is not in the store (the namespace
    /// is full: hints no longer prove it).
    Evicted,
    /// Its hint matches, the namespace is full, and the query's time ran
    /// out (or it stopped) before the store was checked for it.
    Unchecked,
    /// The snapshot's worktree content is not known (`Snap::incomplete`):
    /// an indexed file the query did not read may differ from its index
    /// entry.
    Unverified,
}

/// What the full-mode check found for a hinted key.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Presence {
    Present,
    Missing,
    /// Not checked: the query's time ran out first.
    Unchecked,
}

/// What one query learned about the files it read, and, once the namespace
/// is full, which hinted units the store really has.
#[derive(Clone, Default)]
pub(crate) struct Account {
    /// The files the query read or tried to, by (path, OID): searched
    /// (None), or why not. A later `Searched` replaces an earlier reason.
    read: HashMap<(RelPath, BlobId), Option<Reason>>,
    /// Full mode: what the store has of the (OID, language) of hinted files.
    present: HashMap<(BlobId, LangName), Presence>,
}

/// Where a file's classification counts.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Bucket {
    Indexed,
    Unsearched,
    Outside,
}

/// The walk's counts ([`Snap::walk`]): every file classified as if the
/// query had read nothing, and the class the walk gave each file read by
/// then, to be replaced by what was read ([`Snap::finish`]).
#[derive(Clone, Default, Debug)]
pub(crate) struct Walked {
    unsearched: i64,
    outside: i64,
    known: HashMap<(RelPath, BlobId), Bucket>,
}

impl Walked {
    fn add(&mut self, bucket: Bucket, n: i64) {
        match bucket {
            Bucket::Indexed => {}
            Bucket::Unsearched => self.unsearched += n,
            Bucket::Outside => self.outside += n,
        }
    }
}

/// The key of a cached walk (`Project::walks`): everything its counts
/// depend on. The hints are covered by the installation count (`hinted`)
/// and the stamp, the unfinished units by `hinted` too (a change of their
/// record counts as an installation), the map, its table, the disabled
/// languages and whether the worktree's content is known by the published
/// snapshot.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct WalkKey {
    published: u64,
    stamp: HintStamp,
    hinted: u64,
    dir: String,
    /// The query's language (its group), None for every language (`Named`).
    group: Option<LangName>,
}

/// Walks cached per project: a jump repeated on an unchanged index skips
/// the walk over every file.
pub(crate) const WALKS_KEPT: usize = 2;

/// What stops the walk: the query stopped (`stopped`), its time box ran
/// out (`due`). Checked between two full-mode statements outside the
/// scope.
pub(crate) struct Stops<'a> {
    pub stopped: &'a dyn Fn() -> bool,
    pub due: &'a dyn Fn() -> bool,
}

impl Account {
    /// The query read `path` at `oid` and looked at its content.
    pub fn searched(&mut self, path: &RelPath, oid: BlobId) {
        self.read.insert((path.clone(), oid), None);
    }

    /// The query tried to read `path` at `oid` and could not search it; a
    /// file already searched stays searched.
    pub fn unsearched(&mut self, path: &RelPath, oid: BlobId, reason: Reason) {
        self.read
            .entry((path.clone(), oid))
            .and_modify(|r| {
                if r.is_some() {
                    *r = Some(reason);
                }
            })
            .or_insert(Some(reason));
    }

    /// Whether the query has read `path` at `oid` (Some), and why it could
    /// not search it (the inner Some).
    pub fn outcome(&self, path: &RelPath, oid: BlobId) -> Option<Option<Reason>> {
        self.read.get(&(path.clone(), oid)).copied()
    }

    /// Adds what `other` learned (a prefetch's steps, the query's own).
    pub fn merge(&mut self, other: Account) {
        for ((path, oid), reason) in other.read {
            match reason {
                None => self.searched(&path, oid),
                Some(r) => self.unsearched(&path, oid, r),
            }
        }
        self.present.extend(other.present);
    }

    /// What the full-mode check found for a hinted file (Present when not
    /// checked: outside full mode nothing is).
    fn presence(&self, oid: BlobId, lang: &Lang) -> Presence {
        if self.present.is_empty() {
            return Presence::Present;
        }
        self.present
            .get(&(oid, lang.name().clone()))
            .copied()
            .unwrap_or(Presence::Present)
    }
}

/// A file as the accounting looks at it.
struct Seen<'a> {
    oid: Option<BlobId>,
    lang: &'a Lang,
    slot: &'a HintSlot,
}

/// The status of an unfinished unit of (`oid`, `lang`), if any.
fn unfinished_in(set: &Unfinished, oid: &BlobId, lang: &Lang) -> Option<ExtractStatus> {
    set.get(oid)?
        .iter()
        .find(|(l, _)| l == lang.name())
        .map(|(_, s)| *s)
}

impl Snap {
    fn hinted(&self, file: &Seen<'_>) -> bool {
        file.oid
            .is_some_and(|oid| self.map.hints.matches(file.slot, &oid, self.stamp))
    }

    /// Whether the snapshot's record has the unit of `eff` as `TooLarge` or
    /// `TimedOut`: the search reads such a file (`search_scope`).
    pub fn unfinished(&self, eff: &Effective) -> bool {
        unfinished_in(&self.unfinished, &eff.oid, &eff.lang).is_some()
    }

    /// Full mode (`Snap::full`): checks the store for the hinted files among
    /// `files` (OID and language) not checked yet, with the batch layer's
    /// existence statement (`VERIFY_CHUNK` OIDs each, by version), and
    /// records what it has. `stops`, when given, is checked between two
    /// statements: once it says stop, the rest are `Unchecked`. False when
    /// the query stopped.
    fn verify_keys<'a>(
        &self,
        account: &mut Account,
        files: impl Iterator<Item = (BlobId, &'a Lang)>,
        stops: Option<&Stops<'_>>,
    ) -> bool {
        let mut groups: HashMap<Version, (HashSet<BlobId>, LangName)> = HashMap::new();
        for (oid, lang) in files {
            let key = (oid, lang.name().clone());
            if matches!(
                account.present.get(&key),
                Some(Presence::Present | Presence::Missing)
            ) {
                continue;
            }
            let Some(lang) = self.table.get(&lang.name().0) else {
                continue;
            };
            groups
                .entry(Version::of(lang))
                .or_insert_with(|| (HashSet::new(), lang.name().clone()))
                .0
                .insert(oid);
        }
        crate::phases::count(
            "verified hints",
            groups.values().map(|(o, _)| o.len() as u64).sum(),
        );
        let mut cut = false;
        for (version, (oids, lang)) in groups {
            let oids: Vec<BlobId> = oids.into_iter().collect();
            for chunk in oids.chunks(VERIFY_CHUNK) {
                if let Some(stops) = stops {
                    if (stops.stopped)() {
                        return false;
                    }
                    cut = cut || (stops.due)();
                }
                if cut {
                    for oid in chunk {
                        account
                            .present
                            .insert((*oid, lang.clone()), Presence::Unchecked);
                    }
                    continue;
                }
                // Unreadable: nothing is proven, every one is missing.
                let found: HashSet<BlobId> = self
                    .project
                    .store
                    .exists(&version, chunk)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|f| f.oid)
                    .collect();
                for oid in chunk {
                    let has = if found.contains(oid) {
                        Presence::Present
                    } else {
                        Presence::Missing
                    };
                    account.present.insert((*oid, lang.clone()), has);
                }
            }
        }
        true
    }

    /// [`verify_keys`](Self::verify_keys) for the hinted files among `files`
    /// (the search's scope), all of them.
    pub fn verify_hinted(&self, account: &mut Account, files: &[Effective]) {
        if !self.full {
            return;
        }
        let hinted = files
            .iter()
            .filter(|e| !self.unindexed(e))
            .map(|e| (e.oid, &e.lang));
        self.verify_keys(account, hinted, None);
    }

    /// Whether a file counts as indexed without being read: its hint
    /// matches, its unit has the file's definitions, the store was not found
    /// to lack it, and the snapshot's worktree content is known.
    pub fn indexed(&self, account: &Account, eff: &Effective) -> bool {
        let seen = Seen {
            oid: Some(eff.oid),
            lang: &eff.lang,
            slot: &eff.slot,
        };
        self.default_class(account, &self.unfinished, &seen, true)
            .is_none()
    }

    /// Why a file the query did not read is not indexed (None: it is),
    /// with `inside` whether it is in the search's scope and `unfinished`
    /// the record of units without definitions.
    fn default_class(
        &self,
        account: &Account,
        unfinished: &Unfinished,
        file: &Seen<'_>,
        inside: bool,
    ) -> Option<Reason> {
        let Some(oid) = file.oid else {
            return Some(Reason::Unindexable);
        };
        if self.hinted(file) {
            if self.incomplete {
                return Some(Reason::Unverified);
            }
            match account.presence(oid, file.lang) {
                Presence::Missing => return Some(Reason::Evicted),
                Presence::Unchecked => return Some(Reason::Unchecked),
                Presence::Present => {}
            }
            return match unfinished_in(unfinished, &oid, file.lang) {
                Some(ExtractStatus::TimedOut) => Some(Reason::TimedOut),
                Some(_) => Some(Reason::TooLarge),
                None => None,
            };
        }
        Some(
            if self.in_flight.contains(&(oid, file.lang.name().clone())) {
                Reason::InFlight
            } else if inside {
                Reason::NotReached
            } else {
                Reason::Outside
            },
        )
    }

    fn bucket(&self, class: Option<Reason>, inside: bool) -> Bucket {
        match class {
            None => Bucket::Indexed,
            Some(_) if inside => Bucket::Unsearched,
            Some(_) => Bucket::Outside,
        }
    }

    /// The walk of the accounting: every file of the map whose language
    /// `part` admits, with the search's scope `dir` ("" for the whole
    /// worktree), classified as if the query had read nothing: indexed, else
    /// unsearched inside the scope and outside it; and the class of every
    /// file read so far. `group` names `part` for the cache (the query's
    /// language, None for every language).
    ///
    /// Full mode checks the store for the hinted files first: those of the
    /// scope all, the others in statements between which `stops` is
    /// checked; unchecked ones count. Outside full mode the counts are
    /// cached per project (`WalkKey`). Every hint the accounting uses is
    /// read here, before the query's last view check. None when the query
    /// stopped.
    pub fn walk(
        &self,
        account: &mut Account,
        dir: &str,
        part: &dyn Fn(&Lang) -> bool,
        group: Option<&LangName>,
        stops: &Stops<'_>,
    ) -> Option<Walked> {
        let (hinted0, unfinished0) = {
            let st = lock(&self.project.state);
            (st.hinted, st.unfinished.clone())
        };
        let key = WalkKey {
            published: self.published,
            stamp: self.stamp,
            hinted: hinted0,
            dir: dir.to_owned(),
            group: group.cloned(),
        };
        let cached = if self.full {
            None
        } else {
            lock(&self.project.walks)
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, w)| *w)
        };
        let (unsearched, outside) = match cached {
            Some(counts) => {
                crate::phases::count("cached walks", 1);
                counts
            }
            None => {
                if self.full && !self.verify_all(account, dir, part, stops) {
                    return None;
                }
                let mut walked = Walked::default();
                let mut admit = |l: &Lang| part(l);
                for inside in [true, false] {
                    self.map.each_file(dir, inside, &mut admit, |f| {
                        let seen = Seen {
                            oid: f.oid,
                            lang: f.lang,
                            slot: &f.slot,
                        };
                        // Without the unfinished record: added below for its
                        // few keys, not looked up for every file.
                        let class = self.default_class(account, &Unfinished::new(), &seen, inside);
                        walked.add(self.bucket(class, inside), 1);
                    });
                }
                // The record as it is now, with the one read before the
                // hints: a unit stored meanwhile is in one of them
                // (conservative, a key in either counts).
                let (hinted1, unfinished1) = {
                    let st = lock(&self.project.state);
                    (st.hinted, st.unfinished.clone())
                };
                let both = union(&unfinished0, &unfinished1);
                self.add_unfinished(account, &both, dir, part, &mut walked);
                let counts = (walked.unsearched, walked.outside);
                // Cached only when no hint changed during the walk.
                if !self.full && hinted1 == hinted0 {
                    let mut walks = lock(&self.project.walks);
                    walks.retain(|(k, _)| *k != key);
                    if walks.len() >= WALKS_KEPT {
                        walks.remove(0);
                    }
                    walks.push((key, counts));
                }
                counts
            }
        };
        let mut walked = Walked {
            unsearched,
            outside,
            known: HashMap::new(),
        };
        // The files read so far: their class in the walk, fixed now.
        let unfinished = union(&unfinished0, &lock(&self.project.state).unfinished.clone());
        for (path, oid) in account.read.keys() {
            let Some(eff) = self.map.effective(path).filter(|e| e.oid == *oid) else {
                continue;
            };
            if !part(&eff.lang) {
                continue;
            }
            let inside = under(path, dir);
            let seen = Seen {
                oid: Some(eff.oid),
                lang: &eff.lang,
                slot: &eff.slot,
            };
            let class = self.default_class(account, &unfinished, &seen, inside);
            walked
                .known
                .insert((path.clone(), *oid), self.bucket(class, inside));
        }
        Some(walked)
    }

    /// Full mode: checks every hinted file the walk covers, those of the
    /// scope all, the others until `stops` says stop. False when the query
    /// stopped.
    fn verify_all(
        &self,
        account: &mut Account,
        dir: &str,
        part: &dyn Fn(&Lang) -> bool,
        stops: &Stops<'_>,
    ) -> bool {
        let mut admit = |l: &Lang| part(l);
        for inside in [true, false] {
            let mut hinted: Vec<(BlobId, Lang)> = Vec::new();
            self.map.each_file(dir, inside, &mut admit, |f| {
                let seen = Seen {
                    oid: f.oid,
                    lang: f.lang,
                    slot: &f.slot,
                };
                if let (true, Some(oid)) = (self.hinted(&seen), f.oid) {
                    hinted.push((oid, f.lang.clone()));
                }
            });
            let stops = (!inside).then_some(stops);
            if !self.verify_keys(account, hinted.iter().map(|(o, l)| (*o, l)), stops) {
                return false;
            }
        }
        true
    }

    /// Adds the files whose unit is unfinished (`set`) and that the bulk of
    /// the walk took as indexed.
    fn add_unfinished(
        &self,
        account: &Account,
        set: &Unfinished,
        dir: &str,
        part: &dyn Fn(&Lang) -> bool,
        walked: &mut Walked,
    ) {
        for (oid, langs) in set {
            for (lang, _) in langs {
                for eff in self.map.paths(oid, &lang.0) {
                    if !part(&eff.lang) {
                        continue;
                    }
                    let inside = under(&eff.path, dir);
                    let seen = Seen {
                        oid: Some(eff.oid),
                        lang: &eff.lang,
                        slot: &eff.slot,
                    };
                    // Only where the bulk counted it as indexed.
                    if self
                        .default_class(account, &Unfinished::new(), &seen, inside)
                        .is_none()
                    {
                        walked.add(self.bucket(Some(Reason::TooLarge), inside), 1);
                    }
                }
            }
        }
    }

    /// The completeness: the walk's classification, with every file the
    /// query read classified by what it learned instead (once each). No
    /// hint is read: a file the walk classified is replaced by its outcome;
    /// a file read after the walk (its class there not fixed) only adds
    /// when it could not be searched (conservative), and so does a blob read
    /// by OID that is not the map's content of its path (the definition's
    /// file as an object, changed since).
    pub fn finish(&self, account: &Account, walked: Walked) -> Completeness {
        let mut counts = (walked.unsearched, walked.outside);
        for (key, reason) in &account.read {
            match walked.known.get(key) {
                Some(Bucket::Unsearched) => counts.0 -= 1,
                Some(Bucket::Outside) => counts.1 -= 1,
                Some(Bucket::Indexed) | None => {}
            }
            if reason.is_some() {
                counts.0 += 1;
            }
        }
        let clamp = |n: i64| u32::try_from(n.max(0)).unwrap_or(u32::MAX);
        let (mut unsearched, outside) = (clamp(counts.0), clamp(counts.1));
        // Without the worktree's content the map does not know the files
        // the comparison would list (untracked, changed): never complete,
        // whatever was read; they count as one at least.
        if self.incomplete {
            unsearched = unsearched.max(1);
        }
        if unsearched == 0 && outside == 0 {
            Completeness::Complete
        } else {
            Completeness::Partial {
                unsearched,
                outside,
            }
        }
    }

    /// [`walk`](Self::walk) for every language, never stopped (tests).
    #[cfg(test)]
    pub fn walk_all(&self, account: &mut Account, dir: &str) -> Walked {
        let never = || false;
        let stops = Stops {
            stopped: &never,
            due: &never,
        };
        self.walk(account, dir, &|_| true, None, &stops)
            .expect("not stopped")
    }

    /// The accounting's reasons file by file (tests): (path, why) of every
    /// file not taken as indexed or searched, by what the query read, else
    /// by the walk's rules with the current record.
    #[cfg(test)]
    pub fn unsearched_files(
        &self,
        account: &Account,
        dir: &str,
        part: &dyn Fn(&Lang) -> bool,
    ) -> Vec<(String, Reason)> {
        let mut out = Vec::new();
        let unfinished = lock(&self.project.state).unfinished.clone();
        let mut admit = |l: &Lang| part(l);
        for inside in [true, false] {
            self.map.each_file(dir, inside, &mut admit, |f| {
                let seen = Seen {
                    oid: f.oid,
                    lang: f.lang,
                    slot: &f.slot,
                };
                let path: RelPath = f.path().into();
                let read = f.oid.and_then(|oid| account.outcome(&path, oid));
                let why = match read {
                    Some(read) => read,
                    None => self.default_class(account, &unfinished, &seen, inside),
                };
                if let Some(why) = why {
                    out.push((f.path().to_owned(), why));
                }
            });
        }
        out.sort();
        out
    }
}

/// The union of two records (the same `Arc` most of the time).
fn union(a: &Arc<Unfinished>, b: &Arc<Unfinished>) -> Arc<Unfinished> {
    if Arc::ptr_eq(a, b) {
        return a.clone();
    }
    let mut out = (**a).clone();
    for (oid, langs) in b.iter() {
        let keys = out.entry(*oid).or_default();
        for (lang, status) in langs {
            if !keys.iter().any(|(l, _)| l == lang) {
                keys.push((lang.clone(), *status));
            }
        }
    }
    Arc::new(out)
}
