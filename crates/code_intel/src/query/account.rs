//! A query's completeness (「完整性的核算」): one accounting of every file
//! that could take part in its answer. Each file of the snapshot's map in the
//! query's languages, whatever its tier (the same file, the same directory,
//! the sub-project, a dependent sub-project, elsewhere), is classified once:
//!
//! - `Indexed`: its unit is in the store under the query's view (its hint
//!   matches; once the namespace is full, checked against the store);
//! - `Searched`: the query read it and looked at its content;
//! - `Unsearched(reason)`.
//!
//! `Complete` means nothing is `Unsearched`; nothing else decides it. The
//! walk ([`Snap::walk`]) reads every hint before the query's last view
//! check; what the query reads after it (a lookup made again for hints
//! installed since) replaces its files' classification in
//! [`Snap::finish`], without reading the hints again.

use std::collections::{HashMap, HashSet};

use lectern_services::code_intel::Completeness;
use lectern_services::project::{BlobId, RelPath};

use super::Snap;
use crate::LangName;
use crate::extract::Lang;
use crate::index::state::under;
use crate::map::{Effective, HintSlot};
use crate::store::Version;

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
    /// Its extraction or parse hit the file's own deadline.
    TimedOut,
    /// Changed since the snapshot (or gone): not the snapshot's content.
    Stale,
    /// Over the read limit.
    TooLarge,
    /// Binary content.
    Binary,
    /// Reading it failed, or its grammar did.
    Unreadable,
    /// Being extracted right now: neither in the index nor searched.
    InFlight,
    /// Outside the search's scope and not indexed.
    Outside,
    /// Its hint matches, but its unit is not in the store (the namespace
    /// is full: hints no longer prove it).
    Evicted,
    /// The snapshot's worktree content is not known (`Snap::incomplete`):
    /// an indexed file the query did not read may differ from its index
    /// entry.
    Unverified,
}

/// What one query learned about the files it read, and, once the namespace
/// is full, which hinted units the store really has.
#[derive(Clone, Default)]
pub(crate) struct Account {
    /// The files the query read or tried to, by (path, OID): searched
    /// (None), or why not. A later `Searched` replaces an earlier reason.
    read: HashMap<(RelPath, BlobId), Option<Reason>>,
    /// Full mode: whether the store has the (OID, language) of a hinted
    /// file, as checked.
    present: HashMap<(BlobId, LangName), bool>,
}

/// The walk's classification of the files the query did not read, before
/// [`Snap::finish`] applies what it read.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(crate) struct Walked {
    unsearched: i64,
    outside: i64,
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

    /// Whether the store was found not to have a hinted file's unit.
    fn missing(&self, oid: BlobId, lang: &Lang) -> bool {
        !self.present.is_empty() && self.present.get(&(oid, lang.name().clone())) == Some(&false)
    }
}

/// A file as the accounting looks at it.
struct Seen<'a> {
    oid: BlobId,
    lang: &'a Lang,
    slot: &'a HintSlot,
}

impl Snap {
    fn hinted(&self, file: &Seen<'_>) -> bool {
        self.map.hints.matches(file.slot, &file.oid, self.stamp)
    }

    /// Full mode (`Snap::full`): checks the store for every hinted file
    /// among `files` (OID and language) not checked yet, with the batch
    /// layer's existence statement (500 OIDs each, one group per version),
    /// and records what it has. Nothing outside full mode: there a matching
    /// hint proves the unit.
    fn verify_keys<'a>(
        &self,
        account: &mut Account,
        files: impl Iterator<Item = (BlobId, &'a Lang)>,
    ) {
        let mut groups: HashMap<Version, (HashSet<BlobId>, LangName)> = HashMap::new();
        for (oid, lang) in files {
            let key = (oid, lang.name().clone());
            if account.present.contains_key(&key) {
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
        for (version, (oids, lang)) in groups {
            let oids: Vec<BlobId> = oids.into_iter().collect();
            // Unreadable: nothing is proven, every one is missing.
            let found: HashSet<BlobId> = self
                .project
                .store
                .exists(&version, &oids)
                .unwrap_or_default()
                .into_iter()
                .map(|f| f.oid)
                .collect();
            for oid in oids {
                let has = found.contains(&oid);
                account.present.insert((oid, lang.clone()), has);
            }
        }
    }

    /// [`verify_keys`](Self::verify_keys) for the hinted files among `files`
    /// (the search's scope).
    pub fn verify_hinted(&self, account: &mut Account, files: &[Effective]) {
        if !self.full {
            return;
        }
        let hinted = files
            .iter()
            .filter(|e| !self.unindexed(e))
            .map(|e| (e.oid, &e.lang));
        self.verify_keys(account, hinted);
    }

    /// Whether a file counts as indexed without being read: its hint
    /// matches, the store was not found to lack its unit (full mode checks
    /// first), and the snapshot's worktree content is known.
    pub fn indexed(&self, account: &Account, eff: &Effective) -> bool {
        self.default_class(
            account,
            &Seen {
                oid: eff.oid,
                lang: &eff.lang,
                slot: &eff.slot,
            },
            true,
        )
        .is_none()
    }

    /// Why a file the query did not read is not indexed (None: it is),
    /// with `inside` whether it is in the search's scope.
    fn default_class(&self, account: &Account, file: &Seen<'_>, inside: bool) -> Option<Reason> {
        let hinted = self.hinted(file);
        if hinted && !self.incomplete && !account.missing(file.oid, file.lang) {
            return None;
        }
        Some(if hinted && self.incomplete {
            Reason::Unverified
        } else if hinted {
            Reason::Evicted
        } else if self
            .in_flight
            .contains(&(file.oid, file.lang.name().clone()))
        {
            Reason::InFlight
        } else if inside {
            Reason::NotReached
        } else {
            Reason::Outside
        })
    }

    /// The walk of the accounting: every file of the map whose language
    /// `part` admits, classified as if the query had read nothing, with the
    /// search's scope `dir` ("" for the whole worktree): indexed, else
    /// unsearched inside the scope and outside it. Full mode checks the
    /// store for the hinted files first. Every hint the accounting uses is
    /// read here, before the query's last view check.
    pub fn walk(&self, account: &mut Account, dir: &str, part: &dyn Fn(&Lang) -> bool) -> Walked {
        let mut admit = |l: &Lang| part(l);
        if self.full {
            let mut hinted: Vec<(BlobId, Lang)> = Vec::new();
            for inside in [true, false] {
                self.map.each_file(dir, inside, &mut admit, |f| {
                    let seen = Seen {
                        oid: f.oid,
                        lang: f.lang,
                        slot: &f.slot,
                    };
                    if self.hinted(&seen) {
                        hinted.push((f.oid, f.lang.clone()));
                    }
                });
            }
            self.verify_keys(account, hinted.iter().map(|(o, l)| (*o, l)));
        }
        let mut walked = Walked::default();
        for inside in [true, false] {
            self.map.each_file(dir, inside, &mut admit, |f| {
                let seen = Seen {
                    oid: f.oid,
                    lang: f.lang,
                    slot: &f.slot,
                };
                if self.default_class(account, &seen, inside).is_some() {
                    if inside {
                        walked.unsearched += 1;
                    } else {
                        walked.outside += 1;
                    }
                }
            });
        }
        walked
    }

    /// The completeness: the walk's classification, with every file the
    /// query read classified by what it learned instead (once each). The
    /// hints are not read again for the walk's files: a file's walk class
    /// is recomputed from what the walk recorded (`account`'s checks) and
    /// its hint now, which can only have matched since (conservative).
    pub fn finish(
        &self,
        account: &Account,
        dir: &str,
        part: &dyn Fn(&Lang) -> bool,
        walked: Walked,
    ) -> Completeness {
        let mut counts = walked;
        for ((path, oid), reason) in &account.read {
            // Only the map's own content of the path: a blob read by OID
            // (the definition's file as an object) is not one of its files.
            let Some(eff) = self.map.effective(path).filter(|e| e.oid == *oid) else {
                continue;
            };
            if !part(&eff.lang) {
                continue;
            }
            let inside = under(path, dir);
            let seen = Seen {
                oid: eff.oid,
                lang: &eff.lang,
                slot: &eff.slot,
            };
            if self.default_class(account, &seen, inside).is_some() {
                if inside {
                    counts.unsearched -= 1;
                } else {
                    counts.outside -= 1;
                }
            }
            if reason.is_some() {
                counts.unsearched += 1;
            }
        }
        let clamp = |n: i64| u32::try_from(n.max(0)).unwrap_or(u32::MAX);
        let (mut unsearched, outside) = (clamp(counts.unsearched), clamp(counts.outside));
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

    /// The walk's reasons file by file (tests): (path, why) of every file
    /// not taken as indexed or searched.
    #[cfg(test)]
    pub fn unsearched_files(
        &self,
        account: &Account,
        dir: &str,
        part: &dyn Fn(&Lang) -> bool,
    ) -> Vec<(String, Reason)> {
        let mut out = Vec::new();
        let mut admit = |l: &Lang| part(l);
        for inside in [true, false] {
            self.map.each_file(dir, inside, &mut admit, |f| {
                let seen = Seen {
                    oid: f.oid,
                    lang: f.lang,
                    slot: &f.slot,
                };
                let path: RelPath = f.path().into();
                let why = match account.outcome(&path, f.oid) {
                    Some(read) => read,
                    None => self.default_class(account, &seen, inside),
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
