//! Path cutoffs (docs/design/review.md「改动步与声明」「归因的缓存」).
//!
//! A path's cutoff is the time of the most recent HEAD reflog record, from
//! the base record back, within the 14-day window and with old ≠ new, at
//! most 256 of them, that changed the path between its two commits' trees.
//! Change steps and modification times before it do not take part in the
//! path's attribution: the base content changed under them.
//!
//! - `review.tree_changes` caches `Repo::tree_changes` per pair of commits
//!   (repository partition); a pair is computed once.
//! - [`Table`] keeps, per worktree in memory, each window record's changes
//!   restricted to the paths it was built for (the change set's), so it
//!   stays small whatever the commits touched. It holds no base: a query
//!   names the base record.
//! - A record whose changes are not known yet (not computed within the
//!   interactive budget) counts as changing every path: the cutoff only
//!   gets later, so fewer steps take part, never wrong ones.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use lectern_cache::{Compression, Entry, Key, Namespace, Partition};
use lectern_git::{HashKind, Oid, ReflogAction, ReflogEntry, Repo, RepoPath, TreeChanges};
use lectern_services::changes::ChangePath;

use crate::chain::codec::{Reader, bytes, uint};

/// Records in the window after the base record taken into account; the rest
/// of the paths are cut off at the last one's time.
pub(crate) const MAX_RECORDS: usize = 256;
/// `tree_changes` gives `All` above this many paths (part of the key).
pub(crate) const MAX_TREE_PATHS: usize = 20_000;
/// The claims window: sessions active within it, records within it.
pub(crate) const WINDOW_MS: i64 = 14 * 24 * 60 * 60 * 1000;
/// How long `open` computes `tree_changes` misses on its interactive task;
/// the rest go to the background.
pub(crate) const INTERACTIVE_BUDGET: Duration = Duration::from_millis(5);

/// One record of the window.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) struct Record {
    /// Unix seconds.
    pub time: i64,
    pub old: Option<Oid>,
    pub new: Oid,
}

impl Record {
    /// A reflog entry that moved HEAD between two commits (old ≠ new);
    /// None for `checkout -b` and a deleted reference.
    pub(crate) fn of(entry: &ReflogEntry) -> Option<Record> {
        let new = entry.new?;
        (entry.old != Some(new)).then_some(Record {
            time: entry.time.seconds,
            old: entry.old,
            new,
        })
    }
}

/// The records a review takes into account, newest first, within the
/// window, old ≠ new, at most [`MAX_RECORDS`]: from the base entry (index
/// into `entries`, newest first; `entries.len()` when there is none) back,
/// and after it the moves that are not commits. A commit after the base
/// (the agent's own, an amend) keeps the steps before it in the chain, as
/// its change is in the base-to-worktree diff; a branch switch, reset,
/// rebase or pull after it changed the paths' content under those steps.
/// The second value is the floor when more were there: the last record's
/// time.
pub(crate) fn window(
    entries: &[ReflogEntry],
    base: usize,
    window_start_s: i64,
) -> (Vec<Record>, Option<i64>) {
    let mut out = Vec::new();
    let after = entries[..base.min(entries.len())]
        .iter()
        .filter(|e| !matches!(e.action, ReflogAction::Commit { .. }));
    for e in after.chain(entries.iter().skip(base)) {
        if e.time.seconds < window_start_s {
            break;
        }
        if let Some(r) = Record::of(e) {
            if out.len() == MAX_RECORDS {
                let floor = out.last().map(|r: &Record| r.time);
                return (out, floor);
            }
            out.push(r);
        }
    }
    (out, None)
}

/// What a record changed, among the paths the table covers.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum Changed {
    All,
    Paths(HashSet<ChangePath>),
    /// Not computed yet: counts as `All`.
    Unknown,
}

impl Changed {
    fn contains(&self, path: &ChangePath) -> bool {
        match self {
            Changed::All | Changed::Unknown => true,
            Changed::Paths(paths) => paths.contains(path),
        }
    }
}

/// A worktree's cutoff table.
#[derive(Clone, Debug, Default)]
pub(crate) struct Table {
    /// The paths the changes are restricted to.
    pub covered: HashSet<ChangePath>,
    pub changes: HashMap<Record, Changed>,
}

impl Table {
    /// Whether the table answers for these records and paths.
    pub(crate) fn covers<'a>(
        &self,
        records: &[Record],
        mut paths: impl Iterator<Item = &'a ChangePath>,
    ) -> bool {
        records.iter().all(|r| self.changes.contains_key(r))
            && paths.all(|p| self.covered.contains(p))
    }

    /// The records whose changes are still unknown.
    pub(crate) fn unknown(&self, records: &[Record]) -> Vec<Record> {
        records
            .iter()
            .filter(|r| matches!(self.changes.get(r), None | Some(Changed::Unknown)))
            .copied()
            .collect()
    }

    /// The cutoff of `path` (Unix seconds): the first of `records` (newest
    /// first) that changed it, else `floor`, else None (only the window).
    pub(crate) fn cutoff(
        &self,
        records: &[Record],
        floor: Option<i64>,
        path: &ChangePath,
    ) -> Option<i64> {
        records
            .iter()
            .find(|r| self.changes.get(r).is_none_or(|c| c.contains(path)))
            .map(|r| r.time)
            .or(floor)
    }

    /// Records the changes of `record`, restricted to the covered paths.
    pub(crate) fn set(&mut self, record: Record, changes: &TreeChanges) {
        let changed = match changes {
            TreeChanges::All => Changed::All,
            TreeChanges::Paths(paths) => Changed::Paths(
                paths
                    .iter()
                    .map(|p| ChangePath(Arc::clone(p.shared())))
                    .filter(|p| self.covered.contains(p))
                    .collect(),
            ),
        };
        self.changes.insert(record, changed);
    }
}

/// Builds the table for `records` and `paths`: cached pairs in one
/// `get_many`, misses computed until `budget` runs out (None: all of them),
/// the rest left `Unknown`. Blocking; executor tasks only.
pub(crate) fn build(
    repo: &Repo,
    partition: &Partition,
    records: &[Record],
    paths: HashSet<ChangePath>,
    budget: Option<Duration>,
    cancel: &AtomicBool,
) -> Table {
    let mut table = Table {
        covered: paths,
        changes: HashMap::with_capacity(records.len()),
    };
    let hash = repo.location().hash;
    let keys: Vec<Key> = records.iter().map(|r| key(hash, r)).collect();
    let cached = partition.get_many::<CachedTreeChanges>(&keys);
    let started = Instant::now();
    for ((record, key), hit) in records.iter().zip(&keys).zip(cached) {
        if let Some(CachedTreeChanges(changes)) = hit {
            table.set(*record, &changes);
            continue;
        }
        if budget.is_some_and(|b| started.elapsed() >= b) {
            table.changes.insert(*record, Changed::Unknown);
            continue;
        }
        match compute(repo, partition, record, key, cancel) {
            Some(changes) => table.set(*record, &changes),
            None => {
                table.changes.insert(*record, Changed::Unknown);
            }
        }
    }
    table
}

/// Completes the `Unknown` records of `table`. Blocking.
pub(crate) fn complete(
    repo: &Repo,
    partition: &Partition,
    table: &mut Table,
    records: &[Record],
    cancel: &AtomicBool,
) {
    let hash = repo.location().hash;
    for record in table.unknown(records) {
        let key = key(hash, &record);
        let changes = match partition.get::<CachedTreeChanges>(&key) {
            Some(CachedTreeChanges(changes)) => Some(changes),
            None => compute(repo, partition, &record, &key, cancel),
        };
        // An error (an object missing after a gc) stays `Unknown`: every
        // path is cut off there, the conservative side.
        if let Some(changes) = changes {
            table.set(record, &changes);
        }
    }
}

fn compute(
    repo: &Repo,
    partition: &Partition,
    record: &Record,
    key: &Key,
    cancel: &AtomicBool,
) -> Option<TreeChanges> {
    let changes = repo
        .tree_changes(record.old, record.new, MAX_TREE_PATHS, cancel)
        .ok()?;
    // A failed write only means computing it again.
    let _ = partition.put(key, &CachedTreeChanges(changes.clone()));
    Some(changes)
}

/// The key of a pair of commits: the hash algorithm, the old commit (empty
/// for all zeros), the new commit and the path limit.
pub(crate) fn key(hash: HashKind, record: &Record) -> Key {
    Key::new()
        .str(match hash {
            HashKind::Sha1 => "sha1",
            HashKind::Sha256 => "sha256",
        })
        .bytes(record.old.as_ref().map_or(&[][..], Oid::as_bytes))
        .bytes(record.new.as_bytes())
        .u64(MAX_TREE_PATHS as u64)
}

/// `review.tree_changes`: the sorted paths, or `All`.
pub(crate) struct CachedTreeChanges(pub TreeChanges);

impl Entry for CachedTreeChanges {
    const NAMESPACE: Namespace = Namespace {
        name: "review.tree_changes",
        version: 1,
        compression: Compression::Zstd,
    };

    /// `0` for all; `1`, the count, then each path length-prefixed.
    fn encode(&self, out: &mut Vec<u8>) {
        match &self.0 {
            TreeChanges::All => out.push(0),
            TreeChanges::Paths(paths) => {
                out.push(1);
                uint(out, paths.len() as u64);
                for p in paths.iter() {
                    bytes(out, p.as_bytes());
                }
            }
        }
    }

    fn decode(input: &[u8]) -> Option<Self> {
        let mut r = Reader::new(input);
        let changes = match r.byte()? {
            0 => TreeChanges::All,
            1 => {
                let n = r.len()?;
                let mut paths: Vec<RepoPath> = Vec::with_capacity(n);
                for _ in 0..n {
                    let p = RepoPath::new(r.bytes()?)?;
                    // Sorted, each once, as `tree_changes` gives them.
                    if paths.last().is_some_and(|last| *last >= p) {
                        return None;
                    }
                    paths.push(p);
                }
                TreeChanges::Paths(paths.into())
            }
            _ => return None,
        };
        r.done().then_some(CachedTreeChanges(changes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lectern_git::{GitTime, ReflogAction};

    fn oid(n: u8) -> Oid {
        Oid::from_hex(HashKind::Sha1, &format!("{n:02x}").repeat(20)).unwrap()
    }

    fn entry(time: i64, old: u8, new: u8) -> ReflogEntry {
        ReflogEntry {
            old: (old != 0).then(|| oid(old)),
            new: Some(oid(new)),
            time: GitTime {
                seconds: time,
                offset: 0,
            },
            action: ReflogAction::Other,
            message: Arc::from(&b""[..]),
        }
    }

    fn commit(time: i64, old: u8, new: u8) -> ReflogEntry {
        ReflogEntry {
            action: ReflogAction::Commit {
                amend: false,
                initial: false,
            },
            ..entry(time, old, new)
        }
    }

    fn path(p: &str) -> ChangePath {
        ChangePath(Arc::from(p.as_bytes()))
    }

    #[test]
    fn claims_window_skips_unmoved_records_and_caps() {
        // Newest first: a `checkout -b` (old = new) is not a record.
        let entries = vec![
            entry(500, 3, 3),
            entry(400, 2, 3),
            entry(300, 1, 2),
            entry(10, 0, 1),
        ];
        let (records, floor) = window(&entries, 0, 100);
        assert_eq!(
            records.iter().map(|r| r.time).collect::<Vec<_>>(),
            [400, 300]
        );
        assert_eq!(floor, None);
        // After the base entry (index 2) only moves that are not commits:
        // a checkout at 400 takes part, a commit at 450 does not.
        let entries = vec![
            entry(500, 3, 3),
            commit(450, 4, 5),
            entry(400, 2, 3),
            entry(300, 1, 2),
            entry(10, 0, 1),
        ];
        let (records, _) = window(&entries, 3, 0);
        assert_eq!(
            records.iter().map(|r| r.time).collect::<Vec<_>>(),
            [400, 300, 10]
        );
        // No base entry: every move that is not a commit.
        let (records, _) = window(&entries, entries.len(), 0);
        assert_eq!(
            records.iter().map(|r| r.time).collect::<Vec<_>>(),
            [400, 300, 10]
        );

        let many: Vec<ReflogEntry> = (0..300)
            .map(|i| entry(10_000 - i, (i % 200 + 1) as u8, (i % 200 + 2) as u8))
            .collect();
        let (records, floor) = window(&many, 0, 0);
        assert_eq!(records.len(), MAX_RECORDS);
        assert_eq!(floor, Some(10_000 - 255));
    }

    #[test]
    fn claims_cutoff_per_path() {
        let a = Record {
            time: 300,
            old: Some(oid(1)),
            new: oid(2),
        };
        let b = Record {
            time: 200,
            old: Some(oid(2)),
            new: oid(3),
        };
        let mut table = Table {
            covered: [path("f"), path("g")].into_iter().collect(),
            changes: HashMap::new(),
        };
        table.set(
            a,
            &TreeChanges::Paths(
                vec![RepoPath::new(b"g").unwrap(), RepoPath::new(b"h").unwrap()].into(),
            ),
        );
        table.set(
            b,
            &TreeChanges::Paths(vec![RepoPath::new(b"f").unwrap()].into()),
        );
        let records = [a, b];
        assert_eq!(table.cutoff(&records, None, &path("g")), Some(300));
        assert_eq!(table.cutoff(&records, None, &path("f")), Some(200));
        assert_eq!(table.cutoff(&records[..1], None, &path("f")), None);
        assert_eq!(
            table.cutoff(&records[..1], Some(150), &path("f")),
            Some(150)
        );
        // Only covered paths are kept.
        assert!(matches!(table.changes.get(&a), Some(Changed::Paths(p)) if p.len() == 1));
        // Unknown counts as every path.
        table.changes.insert(a, Changed::Unknown);
        assert_eq!(table.cutoff(&records, None, &path("f")), Some(300));
    }

    #[test]
    fn claims_tree_changes_value_round_trips() {
        for changes in [
            TreeChanges::All,
            TreeChanges::Paths(Arc::from(Vec::new())),
            TreeChanges::Paths(
                vec![
                    RepoPath::new(b"a/b").unwrap(),
                    RepoPath::new(b"a/c\xff").unwrap(),
                ]
                .into(),
            ),
        ] {
            let mut out = Vec::new();
            CachedTreeChanges(changes.clone()).encode(&mut out);
            assert_eq!(CachedTreeChanges::decode(&out).map(|c| c.0), Some(changes));
        }
        let mut unsorted = Vec::new();
        CachedTreeChanges(TreeChanges::Paths(
            vec![RepoPath::new(b"b").unwrap(), RepoPath::new(b"a").unwrap()].into(),
        ))
        .encode(&mut unsorted);
        assert!(CachedTreeChanges::decode(&unsorted).is_none());
    }
}
