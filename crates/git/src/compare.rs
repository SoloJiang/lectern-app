//! The worktree comparison (docs/design/git.md「工作区比较」): the base tree,
//! the index and the worktree, path by path, plus untracked files. Read-only:
//! the index and the object database are never written, no external program
//! runs, and worktree files are read only through `SafeRoot`, the directory
//! walk for untracked files and `.gitignore` loading included (`untracked`,
//! ADR 0028).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, Weak};
use std::time::{SystemTime, UNIX_EPOCH};

use lectern_platform::{FileKind, Meta, Read, SafeError, SafeRoot, Timestamp};

use crate::changes::{
    Assembler, ChangeSet, Limits, Pending, PendingSide, Staged, Status, Target, Totals,
    pair_renames, peel_to_tree,
};
use crate::conversion::{AttrFile, ConversionState, WorktreeAttrs};
use crate::handle::Handle;
use crate::repo::{normalize, read_head};
use crate::tree::{self, Found};
use crate::untracked;
use crate::{
    Content, GitError, HashKind, HeadState, Mode, Oid, RefName, RepoPath, Unavailable, tracked,
};

/// The base of a worktree comparison: always a tree.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Base {
    /// The tree of the current HEAD; the empty tree on an unborn branch. The
    /// default for reviewing what an agent changed.
    Head,
    /// A commit's tree, such as HEAD when a session started.
    Commit(Oid),
    /// The tree of `merge_base(HEAD, branch)`.
    MergeBase(RefName),
}

/// What [`Worktree::refresh`](crate::Worktree::refresh) compares again.
#[derive(Clone, Copy, Debug)]
pub enum Refresh<'a> {
    /// Only these paths (as `fs_watch` reports them; a directory stands for
    /// every path beneath it).
    Paths(&'a [RepoPath]),
    /// HEAD, the index, `.gitignore`, `.gitattributes` or the configuration
    /// changed: everything, still skipping unchanged files by stat.
    Full,
}

/// What a `Worktree` keeps between comparisons, in memory only: the parsed
/// index, the stat cache and the conversion state its stat comparisons are
/// checked against. Locked only briefly, never across a call.
#[derive(Default)]
pub(crate) struct CompareState {
    index: Option<Arc<IndexSnapshot>>,
    cache: HashMap<RepoPath, Cached>,
    baseline: Option<Arc<ConversionState>>,
}

impl std::fmt::Debug for CompareState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompareState")
            .field("cached", &self.cache.len())
            .finish_non_exhaustive()
    }
}

/// A stat cache entry: the canonical result of reading a file whose stat was
/// `key`, under conversion state `conversion` and index blob `index_oid`.
struct Cached {
    key: StatKey,
    conversion: u64,
    index_oid: Option<Oid>,
    side: (Mode, Content),
    /// The bytes, while a change set still holds them.
    bytes: Option<Weak<[u8]>>,
    /// Whole seconds since the epoch when the comparison that read it began.
    computed: i64,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct StatKey {
    size: u64,
    mtime: Timestamp,
    ctime: Timestamp,
    ino: u64,
    dev: u64,
    mode: u32,
}

impl StatKey {
    fn of(meta: &Meta) -> StatKey {
        StatKey {
            size: meta.size,
            mtime: meta.mtime,
            ctime: meta.ctime,
            ino: meta.ino,
            dev: meta.dev,
            mode: meta.mode,
        }
    }
}

/// The parsed index of one worktree, keyed by its checksum.
pub(crate) struct IndexSnapshot {
    checksum: Option<Oid>,
    state: gix::index::State,
    /// In path order; sparse directory entries expanded.
    items: Vec<IndexItem>,
    invalid: Vec<Arc<[u8]>>,
    /// See `folded()`.
    folded: OnceLock<Vec<(Box<[u8]>, usize)>>,
}

struct IndexItem {
    path: RepoPath,
    /// The stage-0 entry.
    entry: Option<(Mode, Oid)>,
    /// The mode of the preferred conflict stage (ours, theirs, base) when the
    /// path has unresolved conflict entries.
    conflict: Option<Mode>,
    stat: gix::index::entry::Stat,
    skip_worktree: bool,
    intent_to_add: bool,
}

impl IndexSnapshot {
    fn find(&self, path: &[u8]) -> Option<&IndexItem> {
        self.items
            .binary_search_by(|i| i.path.as_bytes().cmp(path))
            .ok()
            .map(|i| &self.items[i])
    }
}

/// What a worktree change set remembers for `refresh`.
pub(crate) struct Origin {
    base: Base,
    git_dir: PathBuf,
    index_checksum: Option<Oid>,
    conversion: Arc<ConversionState>,
    /// Every change before rename pairing, in (path, untracked) order.
    records: Vec<Record>,
    base_invalid: Vec<Arc<[u8]>>,
    untracked_capped: bool,
}

#[derive(Clone)]
struct Record {
    pending: Pending,
    /// The worktree bytes weren't kept and aren't in the object database:
    /// a refresh reads the path again.
    reread: bool,
}

/// What a tracked path's worktree side turned out to be.
enum Seen {
    Absent,
    /// The worktree equals the index entry (stat, skip-worktree, the filter
    /// fast path, an uninitialized submodule).
    SameAsIndex,
    Side(PendingSide),
}

#[derive(Clone, Copy)]
enum Stat {
    Meta(Meta),
    NotFound,
    ThroughSymlink,
    Unreadable,
}

fn special(content: Unavailable) -> PendingSide {
    PendingSide {
        mode: Mode::Special,
        content: Content::Unavailable(content),
        bytes: None,
    }
}

fn unavailable(mode: Mode, why: Unavailable) -> PendingSide {
    PendingSide {
        mode,
        content: Content::Unavailable(why),
        bytes: None,
    }
}

fn index_stat(meta: &Meta) -> gix::index::entry::Stat {
    use gix::index::entry::stat::Time;
    let time = |t: Timestamp| {
        if t.secs < 0 {
            Time { secs: 0, nsecs: 0 }
        } else {
            Time {
                secs: t.secs as u32,
                nsecs: t.nanos,
            }
        }
    };
    gix::index::entry::Stat {
        mtime: time(meta.mtime),
        ctime: time(meta.ctime),
        dev: meta.dev as u32,
        ino: meta.ino as u32,
        uid: meta.uid,
        gid: meta.gid,
        size: meta.size as u32,
    }
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    let mut h = gix::hash::hasher(gix::hash::Kind::Sha256);
    h.update(bytes);
    let mut out = [0u8; 32];
    if let Ok(id) = h.try_finalize() {
        out.copy_from_slice(&id.as_bytes()[..32]);
    }
    out
}

fn same_kind(a: Mode, b: Mode) -> bool {
    let kind = |m| match m {
        Mode::File | Mode::Executable => 0,
        Mode::Symlink => 1,
        Mode::Gitlink => 2,
        Mode::Special => 3,
    };
    kind(a) == kind(b)
}

fn basename(path: &[u8]) -> &[u8] {
    path.rsplit(|&b| b == b'/').next().unwrap_or(path)
}

/// Paths a refresh compares again: each one and everything beneath it.
struct Scope(Vec<RepoPath>);

impl Scope {
    fn contains(&self, path: &[u8]) -> bool {
        self.0.iter().any(|s| {
            let s = s.as_bytes();
            path == s || (path.len() > s.len() && path.starts_with(s) && path[s.len()] == b'/')
        })
    }
}

/// The worktree a comparison runs on, and its state.
pub(crate) struct Subject<'a> {
    pub root: &'a SafeRoot,
    pub root_path: &'a Path,
    pub git_dir: &'a Path,
    pub id: &'a crate::WorktreeId,
    pub repo: &'a crate::Repo,
    pub state: &'a Mutex<CompareState>,
}

/// A worktree comparison against `base`. With `refresh`, only the paths in
/// its scope are compared again and the rest is taken from the previous
/// change set.
pub(crate) fn run(
    subject: &Subject<'_>,
    base: &Base,
    limits: &Limits,
    cancel: &AtomicBool,
    refresh: Option<(&ChangeSet, &Origin, &[RepoPath])>,
) -> Result<Arc<ChangeSet>, GitError> {
    let started = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let handle = Handle::open(subject.git_dir, subject.repo.scope())?;
    let hash = subject.repo.hash();
    let index = snapshot(subject, &handle, limits, cancel)?;

    // A refresh of some paths stands only while HEAD (for `Base::Head`) and
    // the index are what the previous comparison saw; otherwise everything
    // is compared again.
    let mut refresh = refresh;
    if let Some((prev, origin, paths)) = refresh {
        let stale_head = *base == Base::Head
            && resolve_base(subject, &handle, base, cancel)? != prev.base_tree();
        // A reported directory stands for everything beneath it, so an
        // attribute file the previous comparison loaded from there counts
        // as reported too.
        let touches_rules = paths.iter().any(|p| {
            let name = basename(p.as_bytes());
            name == b".gitattributes"
                || name == b".gitignore"
                || origin
                    .conversion
                    .files
                    .iter()
                    .any(|f| Scope(vec![p.clone()]).contains(&f.path))
        });
        if stale_head || origin.index_checksum != index.checksum || touches_rules {
            refresh = None;
        }
    }
    let base_tree = match refresh {
        Some((prev, ..)) => prev.base_tree(),
        None => resolve_base(subject, &handle, base, cancel)?,
    };

    let scope = refresh.map(|(_, origin, paths)| {
        let mut scope: Vec<RepoPath> = paths.to_vec();
        scope.extend(
            origin
                .records
                .iter()
                .filter(|r| r.reread)
                .map(|r| r.pending.path.clone()),
        );
        Scope(scope)
    });

    // The base tree's entries in scope.
    let mut base_invalid = Vec::new();
    let base_entries = match &scope {
        None => {
            let mut out = Vec::new();
            if base_tree != Oid::empty_tree(hash) {
                tree::walk(&handle, base_tree, b"", cancel, &mut |e| {
                    match RepoPath::new(&e.path) {
                        Some(path) => {
                            if out.len() >= limits.entries {
                                return Err(GitError::TooManyEntries);
                            }
                            out.push((path, e.mode, e.oid));
                        }
                        None => base_invalid.push(Arc::from(&e.path[..])),
                    }
                    Ok(())
                })?;
            }
            out
        }
        Some(scope) => {
            let mut out = Vec::new();
            for path in &scope.0 {
                if base_tree == Oid::empty_tree(hash) {
                    break;
                }
                match tree::lookup(&handle, base_tree, path.as_bytes())? {
                    None => {}
                    Some(Found::File(mode, oid)) => out.push((path.clone(), mode, oid)),
                    Some(Found::Tree(oid)) => {
                        tree::walk(&handle, oid, path.as_bytes(), cancel, &mut |e| {
                            if let Some(path) = RepoPath::new(&e.path) {
                                out.push((path, e.mode, e.oid));
                            }
                            Ok(())
                        })?;
                    }
                }
            }
            out
        }
    };
    let mut base_entries = base_entries;
    if !base_entries.is_sorted_by(|a, b| a.0 < b.0) {
        base_entries.sort_by(|a, b| a.0.cmp(&b.0));
        base_entries.dedup_by(|a, b| a.0 == b.0);
    }

    // The index items in scope.
    let items: Vec<usize> = match &scope {
        None => (0..index.items.len()).collect(),
        Some(scope) => {
            let mut out = Vec::new();
            for path in &scope.0 {
                let p = path.as_bytes();
                if let Ok(i) = index.items.binary_search_by(|it| it.path.as_bytes().cmp(p)) {
                    out.push(i);
                }
                let mut lo = p.to_vec();
                lo.push(b'/');
                let mut hi = p.to_vec();
                hi.push(b'/' + 1);
                let start = index
                    .items
                    .partition_point(|it| it.path.as_bytes() < &lo[..]);
                let end = index
                    .items
                    .partition_point(|it| it.path.as_bytes() < &hi[..]);
                out.extend(start..end);
            }
            out.sort_unstable();
            out.dedup();
            out
        }
    };

    let config = handle.config_snapshot();
    let file_mode = config.boolean("core.fileMode").unwrap_or(true);
    let stat_options = handle.stat_options().map_err(GitError::from_gix)?;

    // Phase A: stat every index path in scope that has a worktree side.
    let mut stats: HashMap<usize, Stat> = HashMap::with_capacity(items.len());
    for (n, &i) in items.iter().enumerate() {
        if n % 256 == 0 && cancel.load(Ordering::Relaxed) {
            return Err(GitError::Cancelled);
        }
        let item = &index.items[i];
        if item.skip_worktree && item.conflict.is_none() {
            continue;
        }
        let Some(rel) = item.path.as_rel() else {
            continue;
        };
        let stat = match subject.root.stat(rel) {
            Ok(meta) => Stat::Meta(meta),
            Err(SafeError::NotFound) => Stat::NotFound,
            Err(SafeError::PathThroughSymlink) => Stat::ThroughSymlink,
            Err(SafeError::Io(_)) => Stat::Unreadable,
        };
        stats.insert(i, stat);
    }

    // Phase B: untracked files, listed through `SafeRoot` (ADR 0028).
    let outside_untracked = refresh.map_or(0, |(_, origin, _)| {
        origin
            .records
            .iter()
            .filter(|r| {
                r.pending.untracked
                    && !scope
                        .as_ref()
                        .is_some_and(|s| s.contains(r.pending.path.as_bytes()))
            })
            .count()
    });
    let untracked_limit = limits.untracked.saturating_sub(outside_untracked);
    let (untracked, mut untracked_capped) = if scope.as_ref().is_some_and(|s| s.0.is_empty()) {
        (Vec::new(), false)
    } else {
        let found = untracked::walk(
            &handle,
            hash,
            subject.root,
            &*index,
            scope.as_ref().map(|s| s as &dyn untracked::Covers),
            untracked_limit,
            limits.read_bytes,
            cancel,
            None,
        )?;
        (found.entries, found.capped)
    };
    if let Some((_, origin, _)) = refresh {
        // A new `.gitattributes` beneath a reported directory changes the
        // rules the previous conversion state doesn't have: compare
        // everything.
        if untracked
            .iter()
            .any(|(p, _)| basename(p.as_bytes()) == b".gitattributes")
        {
            return run(subject, base, limits, cancel, None);
        }
        untracked_capped |= origin.untracked_capped;
    }

    // Phase C: the attribute files in effect, read through `SafeRoot`.
    let conversion = match refresh {
        Some((_, origin, _)) => Arc::clone(&origin.conversion),
        None => {
            let mut files = Vec::new();
            for item in &index.items {
                if basename(item.path.as_bytes()) != b".gitattributes" || item.conflict.is_some() {
                    continue;
                }
                let Some((Mode::File | Mode::Executable, oid)) = item.entry else {
                    continue;
                };
                let content = if item.skip_worktree {
                    None
                } else {
                    read_attr_file(subject.root, &item.path, limits.read_bytes)
                };
                files.push(attr_file(hash, &item.path, oid, content));
            }
            for (path, kind) in &untracked {
                if basename(path.as_bytes()) == b".gitattributes"
                    && *kind == untracked::Kind::File
                    && let Some(content) = read_attr_file(subject.root, path, limits.read_bytes)
                {
                    files.push(attr_file(
                        hash,
                        path,
                        Oid::for_blob(hash, &content),
                        Some(content),
                    ));
                }
            }
            Arc::new(ConversionState::new(&handle, files)?)
        }
    };
    let mut attrs = WorktreeAttrs::new(&handle, hash, &conversion.files)?;

    // Rules that changed since this worktree's baseline skip the stat fast
    // paths.
    let (baseline, cache) = {
        let mut state = subject.state.lock().unwrap_or_else(PoisonError::into_inner);
        let baseline = Arc::clone(
            state
                .baseline
                .get_or_insert_with(|| Arc::clone(&conversion)),
        );
        (baseline, std::mem::take(&mut state.cache))
    };
    let mut rules = if baseline.fingerprint == conversion.fingerprint {
        Rules::Same
    } else if baseline.globals != conversion.globals {
        Rules::All
    } else {
        match WorktreeAttrs::new(&handle, hash, &baseline.files) {
            Ok(old) => Rules::Compare(Box::new(old)),
            Err(_) => Rules::All,
        }
    };

    let mut reader = Reader {
        root: subject.root,
        root_path: subject.root_path,
        common_dir: &subject.repo.location().common_dir,
        hash,
        limits,
        file_mode,
        conversion: conversion.fingerprint,
        started,
        cache,
        fresh: HashMap::new(),
    };

    // Phase D: each path of the base or the index.
    let mut records: Vec<Pending> = Vec::new();
    let timestamp = index.state.timestamp();
    let (mut b, mut i) = (0, 0);
    while b < base_entries.len() || i < items.len() {
        if (b + i) % 256 == 0 && cancel.load(Ordering::Relaxed) {
            return Err(GitError::Cancelled);
        }
        let base_path = base_entries.get(b).map(|e| &e.0);
        let item_path = items.get(i).map(|&i| &index.items[i].path);
        let order = match (base_path, item_path) {
            (Some(a), Some(c)) => a.cmp(c),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, _) => std::cmp::Ordering::Greater,
        };
        let (base_entry, item) = match order {
            std::cmp::Ordering::Less => {
                b += 1;
                (Some(&base_entries[b - 1]), None)
            }
            std::cmp::Ordering::Greater => {
                i += 1;
                (None, Some(items[i - 1]))
            }
            std::cmp::Ordering::Equal => {
                b += 1;
                i += 1;
                (Some(&base_entries[b - 1]), Some(items[i - 1]))
            }
        };
        let path = base_entry
            .map(|e| &e.0)
            .or(item.map(|i| &index.items[i].path))
            .cloned();
        let Some(path) = path else { continue };
        let base_side = base_entry.map(|e| (e.1, e.2));
        let Some(slot) = item else {
            // In the base, not in the index: deleted (`git rm --cached` when
            // the file is still there; the walk lists it as untracked).
            records.push(Pending::new(
                Status::Deleted,
                path,
                base_side.map(blob_side),
                None,
            ));
            continue;
        };
        let it = &index.items[slot];
        let stat = stats.get(&slot).copied();
        if let Some(mode) = it.conflict {
            let new = match stat {
                Some(stat) => reader.observe(&mut attrs, &path, stat, Some(mode), None)?,
                None => None,
            };
            records.push(Pending::new(
                Status::Conflicted,
                path,
                base_side.map(blob_side),
                new,
            ));
            continue;
        }
        if it.intent_to_add {
            // `git add -N`: a new file, not untracked.
            if let Some(stat) = stat
                && let Some(new) = reader.observe(&mut attrs, &path, stat, None, None)?
            {
                let mut p = Pending::new(Status::Added, path, base_side.map(blob_side), Some(new));
                if p.old.is_some() {
                    p.status = Status::Modified;
                }
                records.push(p);
            }
            continue;
        }
        let Some(entry) = it.entry else { continue };
        let seen = if it.skip_worktree {
            Seen::SameAsIndex
        } else {
            match stat {
                None => Seen::SameAsIndex,
                Some(Stat::NotFound) => Seen::Absent,
                Some(Stat::ThroughSymlink) => {
                    Seen::Side(unavailable(entry.0, Unavailable::PathThroughSymlink))
                }
                Some(Stat::Unreadable) => Seen::Side(unavailable(entry.0, Unavailable::Unreadable)),
                Some(Stat::Meta(meta)) => {
                    let racy = it.stat.is_racy(timestamp, stat_options);
                    let clean = !racy && it.stat.matches(&index_stat(&meta), stat_options);
                    tracked_side(
                        &mut reader,
                        &mut attrs,
                        &mut rules,
                        &path,
                        entry,
                        meta,
                        base_side,
                        clean,
                    )?
                }
            }
        };
        if let Some(p) = classify(path, base_side, entry, seen) {
            records.push(p);
        }
    }

    // Untracked files: always additions.
    for (path, kind) in untracked {
        use untracked::Kind;
        let side = match kind {
            Kind::Special => Some(special(Unavailable::UnsupportedType)),
            Kind::Repository => Some(special(Unavailable::NestedRepository)),
            Kind::File | Kind::Symlink => {
                let stat = match path.as_rel().map(|rel| subject.root.stat(rel)) {
                    Some(Ok(meta)) => Stat::Meta(meta),
                    Some(Err(SafeError::NotFound)) => Stat::NotFound,
                    Some(Err(SafeError::PathThroughSymlink)) => Stat::ThroughSymlink,
                    _ => Stat::Unreadable,
                };
                reader.observe(&mut attrs, &path, stat, None, None)?
            }
        };
        if let Some(side) = side {
            let mut p = Pending::new(Status::Added, path, None, Some(side));
            p.untracked = true;
            records.push(p);
        }
    }

    // Records outside a refresh's scope come from the previous comparison.
    if let (Some((_, origin, _)), Some(scope)) = (refresh, &scope) {
        records.extend(
            origin
                .records
                .iter()
                .filter(|r| !scope.contains(r.pending.path.as_bytes()))
                .map(|r| r.pending.clone()),
        );
    }
    records.sort_by(|a, b| a.path.cmp(&b.path).then(a.untracked.cmp(&b.untracked)));
    if records.len() > limits.entries {
        return Err(GitError::TooManyEntries);
    }

    // Phase E: renames, then each file's detail under the limits.
    let mut diff_attrs = |path: &RepoPath| attrs.get(path).map(|a| a.diff);
    let pending = pair_renames(&handle, records.clone(), limits, &mut diff_attrs, cancel)?;
    let assembled = Assembler {
        handle: &handle,
        limits,
        cancel,
        attrs: &mut diff_attrs,
    }
    .run(pending)?;

    let mut invalid: Vec<Arc<[u8]>> = index.invalid.clone();
    let base_invalid = match refresh {
        Some((_, origin, _)) => origin.base_invalid.clone(),
        None => base_invalid,
    };
    invalid.extend(base_invalid.iter().cloned());
    invalid.sort_unstable();
    invalid.dedup();
    let totals = Totals {
        untracked_capped,
        invalid_paths: invalid.into(),
        ..assembled.totals.clone()
    };

    // What the change set keeps: worktree bytes only where it retained them.
    let kept: HashSet<*const u8> = assembled
        .retained
        .iter()
        .flatten()
        .map(|b| b.as_ptr())
        .collect();
    let records: Vec<Record> = records
        .into_iter()
        .map(|mut pending| {
            let mut reread = false;
            let base_oid = pending.old.as_ref().and_then(|s| s.content.oid());
            let staged_oid = pending.staged.map(|s| s.oid);
            if let Some(new) = &mut pending.new
                && let Some(bytes) = &new.bytes
                && !kept.contains(&bytes.as_ptr())
            {
                let oid = new.content.oid();
                reread = oid.is_some() && oid != base_oid && oid != staged_oid;
                new.bytes = None;
            }
            Record { pending, reread }
        })
        .collect();

    // The stat cache: what this comparison read, plus, for a refresh, the
    // entries outside its scope.
    {
        let mut state = subject.state.lock().unwrap_or_else(PoisonError::into_inner);
        let mut cache = reader.fresh;
        if let Some(scope) = &scope {
            for (path, entry) in reader.cache {
                if !scope.contains(path.as_bytes()) {
                    cache.entry(path).or_insert(entry);
                }
            }
        }
        state.cache = cache;
    }

    let origin = Arc::new(Origin {
        base: base.clone(),
        git_dir: subject.git_dir.to_path_buf(),
        index_checksum: index.checksum,
        conversion,
        records,
        base_invalid,
        untracked_capped,
    });
    Ok(Arc::new(ChangeSet::from_parts(
        base_tree,
        Target::Worktree(subject.id.clone()),
        assembled,
        totals,
        limits.clone(),
        origin,
    )))
}

/// `refresh`: checks that `prev` is a comparison of this worktree.
pub(crate) fn refresh(
    subject: &Subject<'_>,
    prev: &Arc<ChangeSet>,
    what: Refresh<'_>,
    cancel: &AtomicBool,
) -> Result<Arc<ChangeSet>, GitError> {
    let origin = match (&prev.origin, prev.target()) {
        (Some(origin), Target::Worktree(id))
            if id == subject.id && origin.git_dir == subject.git_dir =>
        {
            Arc::clone(origin)
        }
        _ => {
            return Err(GitError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "the change set is not a comparison of this worktree",
            )));
        }
    };
    let limits = prev.limits().clone();
    match what {
        Refresh::Full => run(subject, &origin.base, &limits, cancel, None),
        Refresh::Paths(paths) => run(
            subject,
            &origin.base,
            &limits,
            cancel,
            Some((prev, &origin, paths)),
        ),
    }
}

enum Rules<'h> {
    Same,
    All,
    Compare(Box<WorktreeAttrs<'h>>),
}

fn blob_side((mode, oid): (Mode, Oid)) -> PendingSide {
    PendingSide {
        mode,
        content: Content::Blob(oid),
        bytes: None,
    }
}

fn read_attr_file(root: &SafeRoot, path: &RepoPath, max: u64) -> Option<Arc<[u8]>> {
    let rel = path.as_rel()?;
    hook::read();
    match root.read(rel, max) {
        Ok(Read::File { bytes, .. }) => Some(bytes.into()),
        _ => None,
    }
}

fn attr_file(hash: HashKind, path: &RepoPath, oid: Oid, content: Option<Arc<[u8]>>) -> AttrFile {
    let oid = content.as_ref().map_or(oid, |c| Oid::for_blob(hash, c));
    AttrFile {
        path: path.as_bytes().to_vec(),
        oid,
        content,
    }
}

/// The tree a comparison's base names.
fn resolve_base(
    subject: &Subject<'_>,
    handle: &Handle,
    base: &Base,
    cancel: &AtomicBool,
) -> Result<Oid, GitError> {
    let head_commit = || -> Result<Option<Oid>, GitError> {
        let head = read_head(subject.repo, handle, subject.git_dir, subject.id)?
            .ok_or_else(|| GitError::Corrupt(format!("{}: no HEAD", subject.git_dir.display())))?;
        Ok(match head {
            HeadState::Branch { commit, .. } => commit,
            HeadState::Detached { commit } => Some(commit),
        })
    };
    match base {
        Base::Head => match head_commit()? {
            Some(commit) => peel_to_tree(handle, commit),
            None => Ok(Oid::empty_tree(subject.repo.hash())),
        },
        Base::Commit(id) => peel_to_tree(handle, *id),
        Base::MergeBase(name) => {
            let head = head_commit()?.ok_or(GitError::NoMergeBase)?;
            let Some(mut reference) = handle
                .try_find_reference(name.as_str())
                .map_err(crate::error::gix_err)?
            else {
                return Err(GitError::Io(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("no reference {name}"),
                )));
            };
            let other = reference.peel_to_id().map_err(GitError::from_gix)?;
            let other = Oid::from_gix(&other);
            let base = crate::refs::merge_base(handle, head, other, cancel)?
                .ok_or(GitError::NoMergeBase)?;
            peel_to_tree(handle, base)
        }
    }
}

/// The parsed index, reused while its checksum is unchanged.
fn snapshot(
    subject: &Subject<'_>,
    handle: &Handle,
    limits: &Limits,
    cancel: &AtomicBool,
) -> Result<Arc<IndexSnapshot>, GitError> {
    let path = subject.git_dir.join("index");
    let hash = subject.repo.hash();
    let current = tracked::checksum(&path, hash)?;
    {
        let state = subject.state.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(index) = &state.index
            && index.checksum == current
        {
            if index.items.len() > limits.entries {
                return Err(GitError::TooManyEntries);
            }
            return Ok(Arc::clone(index));
        }
    }
    let snapshot = match tracked::parse(&path, hash)? {
        None => IndexSnapshot {
            checksum: None,
            state: gix::index::State::new(hash.to_gix()),
            items: Vec::new(),
            invalid: Vec::new(),
            folded: OnceLock::new(),
        },
        Some((file, checksum)) => {
            let state: gix::index::State = file.into();
            let (items, invalid) = index_items(handle, &state, limits, cancel)?;
            IndexSnapshot {
                checksum: Some(checksum),
                state,
                items,
                invalid,
                folded: OnceLock::new(),
            }
        }
    };
    let snapshot = Arc::new(snapshot);
    subject
        .state
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .index = Some(Arc::clone(&snapshot));
    Ok(snapshot)
}

type Items = (Vec<IndexItem>, Vec<Arc<[u8]>>);

fn index_items(
    handle: &Handle,
    state: &gix::index::State,
    limits: &Limits,
    cancel: &AtomicBool,
) -> Result<Items, GitError> {
    use gix::index::entry::{Flags, Mode as IndexMode, Stage};

    let mut items = Vec::new();
    let mut invalid = Vec::new();
    let entries = state.entries();
    let mut i = 0;
    while i < entries.len() {
        if i % 4096 == 0 && cancel.load(Ordering::Relaxed) {
            return Err(GitError::Cancelled);
        }
        let path = entries[i].path(state);
        let mut end = i + 1;
        while end < entries.len() && entries[end].path(state) == path {
            end += 1;
        }
        let group = &entries[i..end];
        i = end;
        let live: Vec<&gix::index::Entry> = group
            .iter()
            .filter(|e| !e.flags.contains(Flags::REMOVE))
            .collect();
        let Some(first) = live.first() else { continue };
        if first.mode == IndexMode::DIR {
            // A sparse directory entry: its files are skip-worktree entries.
            let dir = path.strip_suffix(b"/").unwrap_or(path);
            if RepoPath::new(dir).is_none() {
                invalid.push(Arc::from(&**path));
                continue;
            }
            tree::walk(handle, Oid::from_gix(&first.id), dir, cancel, &mut |e| {
                let Some(path) = RepoPath::new(&e.path) else {
                    invalid.push(Arc::from(&e.path[..]));
                    return Ok(());
                };
                if items.len() >= limits.entries {
                    return Err(GitError::TooManyEntries);
                }
                items.push(IndexItem {
                    path,
                    entry: Some((e.mode, e.oid)),
                    conflict: None,
                    stat: Default::default(),
                    skip_worktree: true,
                    intent_to_add: false,
                });
                Ok(())
            })?;
            continue;
        }
        let Some(repo_path) = RepoPath::new(path) else {
            invalid.push(Arc::from(&**path));
            continue;
        };
        let mode_of = |e: &gix::index::Entry| {
            Mode::from_index(e.mode).ok_or_else(|| {
                GitError::Corrupt(format!(
                    "index entry {:?} has mode {:o}",
                    String::from_utf8_lossy(path),
                    e.mode.bits()
                ))
            })
        };
        let stage0 = live.iter().find(|e| e.stage() == Stage::Unconflicted);
        let rank = |stage: Stage| match stage {
            Stage::Unconflicted => 0,
            Stage::Ours => 1,
            Stage::Theirs => 2,
            Stage::Base => 3,
        };
        let conflict = live
            .iter()
            .filter(|e| e.stage() != Stage::Unconflicted)
            .min_by_key(|e| rank(e.stage()))
            .map(|e| mode_of(e))
            .transpose()?;
        let entry = stage0
            .map(|e| Ok::<_, GitError>((mode_of(e)?, Oid::from_gix(&e.id))))
            .transpose()?;
        let main = stage0.copied().unwrap_or(first);
        if items.len() >= limits.entries {
            return Err(GitError::TooManyEntries);
        }
        items.push(IndexItem {
            path: repo_path,
            entry,
            conflict,
            stat: main.stat,
            skip_worktree: main.flags.contains(Flags::SKIP_WORKTREE),
            intent_to_add: main.flags.contains(Flags::INTENT_TO_ADD),
        });
    }
    if !items.is_sorted_by(|a, b| a.path < b.path) {
        items.sort_by(|a, b| a.path.cmp(&b.path));
        items.dedup_by(|a, b| a.path == b.path);
    }
    Ok((items, invalid))
}

impl IndexSnapshot {
    /// The items' paths ASCII-lowercased, sorted, with their item index;
    /// built the first time a case-insensitive lookup needs it.
    fn folded(&self) -> &[(Box<[u8]>, usize)] {
        self.folded.get_or_init(|| {
            let mut out: Vec<(Box<[u8]>, usize)> = self
                .items
                .iter()
                .enumerate()
                .map(|(i, item)| (item.path.as_bytes().to_ascii_lowercase().into(), i))
                .collect();
            out.sort();
            out
        })
    }
}

impl untracked::Index for IndexSnapshot {
    fn entry(&self, path: &[u8], fold: bool) -> Option<Mode> {
        let item = if fold {
            let folded = self.folded();
            let key = path.to_ascii_lowercase();
            let at = folded.partition_point(|(p, _)| p[..] < key[..]);
            let (p, i) = folded.get(at)?;
            if p[..] != key[..] {
                return None;
            }
            &self.items[*i]
        } else {
            self.find(path)?
        };
        item.entry.map(|e| e.0).or(item.conflict)
    }

    fn has_beneath(&self, dir: &[u8], fold: bool) -> bool {
        let mut prefix = if fold {
            dir.to_ascii_lowercase()
        } else {
            dir.to_vec()
        };
        prefix.push(b'/');
        if fold {
            let folded = self.folded();
            let start = folded.partition_point(|(p, _)| p[..] < prefix[..]);
            return folded
                .get(start)
                .is_some_and(|(p, _)| p.starts_with(&prefix));
        }
        let start = self
            .items
            .partition_point(|i| i.path.as_bytes() < prefix.as_slice());
        self.items
            .get(start)
            .is_some_and(|i| i.path.as_bytes().starts_with(&prefix))
    }
}

impl untracked::Covers for Scope {
    fn covers(&self, path: &[u8]) -> bool {
        self.contains(path)
    }

    fn leads_to(&self, dir: &[u8]) -> bool {
        self.0.iter().any(|s| {
            let s = s.as_bytes();
            s.len() > dir.len() && s.starts_with(dir) && s[dir.len()] == b'/'
        })
    }
}

/// Reads worktree files: through the stat cache, then `SafeRoot`, then the
/// built-in conversions.
struct Reader<'a> {
    root: &'a SafeRoot,
    root_path: &'a Path,
    common_dir: &'a Path,
    hash: HashKind,
    limits: &'a Limits,
    file_mode: bool,
    conversion: u64,
    started: i64,
    /// The previous comparisons' entries.
    cache: HashMap<RepoPath, Cached>,
    /// This comparison's.
    fresh: HashMap<RepoPath, Cached>,
}

impl Reader<'_> {
    fn mode(&self, meta: &Meta, index_mode: Option<Mode>) -> Mode {
        match meta.kind {
            FileKind::Symlink => Mode::Symlink,
            FileKind::File => {
                if !self.file_mode {
                    match index_mode {
                        Some(Mode::Executable) => Mode::Executable,
                        _ => Mode::File,
                    }
                } else if meta.is_executable() {
                    Mode::Executable
                } else {
                    Mode::File
                }
            }
            _ => Mode::Special,
        }
    }

    /// The worktree side of a path without an index entry to compare with
    /// (untracked, `git add -N`, conflicted). None when there is nothing.
    fn observe(
        &mut self,
        attrs: &mut WorktreeAttrs<'_>,
        path: &RepoPath,
        stat: Stat,
        index_mode: Option<Mode>,
        index_oid: Option<Oid>,
    ) -> Result<Option<PendingSide>, GitError> {
        let mode = index_mode.unwrap_or(Mode::File);
        Ok(match stat {
            Stat::NotFound => None,
            Stat::ThroughSymlink => Some(unavailable(mode, Unavailable::PathThroughSymlink)),
            Stat::Unreadable => Some(unavailable(mode, Unavailable::Unreadable)),
            Stat::Meta(meta) => match meta.kind {
                FileKind::Dir => None,
                FileKind::File | FileKind::Symlink => {
                    match self.read(attrs, path, &meta, index_mode, index_oid)? {
                        Seen::Side(side) => Some(side),
                        Seen::Absent | Seen::SameAsIndex => None,
                    }
                }
                _ => Some(special(Unavailable::UnsupportedType)),
            },
        })
    }

    /// Reads a regular file or symbolic link and returns its canonical side.
    fn read(
        &mut self,
        attrs: &mut WorktreeAttrs<'_>,
        path: &RepoPath,
        meta: &Meta,
        index_mode: Option<Mode>,
        index_oid: Option<Oid>,
    ) -> Result<Seen, GitError> {
        let filtered = meta.kind == FileKind::File && attrs.get(path)?.filtered;
        if let Some(side) = self.cached(path, meta, index_oid) {
            return Ok(Seen::Side(side));
        }
        let mode = self.mode(meta, index_mode);
        let Some(rel) = path.as_rel() else {
            return Ok(Seen::Side(unavailable(mode, Unavailable::Unreadable)));
        };
        hook::read();
        let (side, meta) = match self.root.read(rel, self.limits.read_bytes) {
            Ok(Read::File { meta, bytes }) => {
                let mode = self.mode(&meta, index_mode);
                let side = if filtered {
                    PendingSide {
                        mode,
                        content: Content::Unavailable(Unavailable::Filtered {
                            raw: crate::RawIdentity::Sha256(sha256(&bytes)),
                        }),
                        bytes: None,
                    }
                } else {
                    let canonical = attrs.canonical(path, bytes, index_oid)?;
                    PendingSide {
                        mode,
                        content: Content::Blob(Oid::for_blob(self.hash, &canonical)),
                        bytes: Some(canonical.into()),
                    }
                };
                (side, meta)
            }
            Ok(Read::Symlink { meta, target }) => (
                PendingSide {
                    mode: Mode::Symlink,
                    content: Content::Blob(Oid::for_blob(self.hash, &target)),
                    bytes: Some(target.into()),
                },
                meta,
            ),
            Ok(Read::TooLarge { meta }) => {
                let (size, mtime_ns, inode) = (meta.size, meta.mtime.as_nanos(), meta.ino);
                let content = if filtered {
                    Content::Unavailable(Unavailable::Filtered {
                        raw: crate::RawIdentity::Stat {
                            size,
                            mtime_ns,
                            inode,
                        },
                    })
                } else {
                    Content::Unhashed {
                        size,
                        mtime_ns,
                        inode,
                    }
                };
                let mode = self.mode(&meta, index_mode);
                (
                    PendingSide {
                        mode,
                        content,
                        bytes: None,
                    },
                    meta,
                )
            }
            Ok(Read::Unsupported { meta }) => {
                if meta.kind == FileKind::Dir {
                    return Ok(Seen::Absent);
                }
                return Ok(Seen::Side(special(Unavailable::UnsupportedType)));
            }
            Err(SafeError::NotFound) => return Ok(Seen::Absent),
            Err(SafeError::PathThroughSymlink) => {
                return Ok(Seen::Side(unavailable(
                    mode,
                    Unavailable::PathThroughSymlink,
                )));
            }
            Err(SafeError::Io(_)) => {
                return Ok(Seen::Side(unavailable(mode, Unavailable::Unreadable)));
            }
        };
        self.fresh.insert(
            path.clone(),
            Cached {
                key: StatKey::of(&meta),
                conversion: self.conversion,
                index_oid,
                side: (side.mode, side.content.clone()),
                bytes: side.bytes.as_ref().map(Arc::downgrade),
                computed: self.started,
            },
        );
        Ok(Seen::Side(side))
    }

    /// A stat cache hit: same stat, conversion state and index blob, an
    /// mtime more than a whole second before the comparison that read it
    /// began (as git's racy-clean rule: a write in the same second as the
    /// read can leave the stat equal; the extra second covers the kernel's
    /// coarse file clock lagging the system clock), and its bytes still held
    /// by a change set when they are needed.
    fn cached(
        &mut self,
        path: &RepoPath,
        meta: &Meta,
        index_oid: Option<Oid>,
    ) -> Option<PendingSide> {
        let entry = self.cache.get(path)?;
        let fresh = entry.key == StatKey::of(meta)
            && entry.conversion == self.conversion
            && entry.index_oid == index_oid
            && meta.mtime.secs.saturating_add(1) < entry.computed;
        if !fresh {
            return None;
        }
        let (mode, content) = entry.side.clone();
        let bytes = match &content {
            Content::Blob(_) => Some(entry.bytes.as_ref()?.upgrade()?),
            _ => None,
        };
        let entry = self.cache.remove(path)?;
        let side = PendingSide {
            mode,
            content,
            bytes,
        };
        self.fresh.insert(path.clone(), entry);
        Some(side)
    }

    /// A submodule's worktree side: the commit its HEAD names, read through
    /// its git directory without touching its worktree. An uninitialized
    /// submodule (no `.git`) or an unborn HEAD equals the index.
    fn submodule(&mut self, path: &RepoPath) -> Seen {
        let mut dot_git = path.as_bytes().to_vec();
        dot_git.extend_from_slice(b"/.git");
        let Some(dot_git) = RepoPath::new(&dot_git) else {
            return Seen::SameAsIndex;
        };
        let Some(rel) = dot_git.as_rel() else {
            return Seen::SameAsIndex;
        };
        let unreadable = || Seen::Side(unavailable(Mode::Gitlink, Unavailable::Unreadable));
        const MAX: u64 = 1024 * 1024;
        let head = match self.root.read(rel, MAX) {
            Err(SafeError::NotFound) => return Seen::SameAsIndex,
            Ok(Read::File { bytes, .. }) => {
                // `gitdir: …` of an absorbed submodule: only a directory under
                // this repository's `modules/` is read, never one the worktree
                // file points elsewhere, and only through a `SafeRoot` at the
                // common directory: a symbolic link at any level of
                // `modules/<name>/…` makes it unreadable.
                let Some(target) = bytes.trim_ascii_end().strip_prefix(b"gitdir:") else {
                    return unreadable();
                };
                let dir = rela(self.root_path, path.as_bytes());
                let git_dir = normalize(&dir.join(rela(Path::new(""), target.trim_ascii())));
                let Some(beneath) = git_dir
                    .strip_prefix(self.common_dir)
                    .ok()
                    .filter(|rel| rel.starts_with("modules"))
                    .and_then(|rel| rel.to_str())
                    .map(|rel| rel.as_bytes().to_vec())
                else {
                    return unreadable();
                };
                let Ok(common) = SafeRoot::open(self.common_dir) else {
                    return unreadable();
                };
                resolve_head(self.hash, &mut |name: &[u8]| {
                    read_git_file(&common, &beneath, name)
                })
            }
            // A `.git` directory in the worktree: read through `SafeRoot`.
            Ok(Read::Unsupported { meta }) if meta.kind == FileKind::Dir => {
                let root = self.root;
                resolve_head(self.hash, &mut |name: &[u8]| {
                    read_git_file(root, dot_git.as_bytes(), name)
                })
            }
            _ => return unreadable(),
        };
        match head {
            Head::Commit(oid) => Seen::Side(PendingSide {
                mode: Mode::Gitlink,
                content: Content::Blob(oid),
                bytes: None,
            }),
            Head::Unborn => Seen::SameAsIndex,
            Head::Unreadable => unreadable(),
        }
    }
}

enum Head {
    Commit(Oid),
    Unborn,
    Unreadable,
}

/// One file of a git directory: Ok(None) when it doesn't exist, Err when it
/// can't be used (a symbolic link at any level, not a regular file, an
/// error).
type GitFile = Result<Option<Vec<u8>>, ()>;

/// `<dir>/<name>` beneath `root`, a regular file read without following a
/// symbolic link at any level.
fn read_git_file(root: &SafeRoot, dir: &[u8], name: &[u8]) -> GitFile {
    const MAX: u64 = 1024 * 1024;
    let mut rel = dir.to_vec();
    rel.push(b'/');
    rel.extend_from_slice(name);
    let rel = RepoPath::new(&rel).ok_or(())?;
    match root.read(rel.as_rel().ok_or(())?, MAX) {
        Ok(Read::File { bytes, .. }) => Ok(Some(bytes)),
        Err(SafeError::NotFound) => Ok(None),
        _ => Err(()),
    }
}

/// A repository's HEAD from its files (`HEAD`, a loose reference or
/// `packed-refs`), read by `read`, following up to five symbolic levels.
fn resolve_head(hash: HashKind, read: &mut dyn FnMut(&[u8]) -> GitFile) -> Head {
    let Ok(Some(head)) = read(b"HEAD") else {
        return Head::Unreadable;
    };
    let mut content = head;
    for _ in 0..5 {
        let text = content.trim_ascii();
        let Some(name) = text.strip_prefix(b"ref:") else {
            return Oid::from_hex_bytes(hash, text).map_or(Head::Unreadable, Head::Commit);
        };
        let name = name.trim_ascii().to_vec();
        if !name.starts_with(b"refs/")
            || name
                .split(|&b| b == b'/')
                .any(|c| c.is_empty() || c == b"." || c == b"..")
        {
            return Head::Unreadable;
        }
        match read(&name) {
            Ok(Some(next)) => {
                content = next;
                continue;
            }
            Ok(None) => {}
            Err(()) => return Head::Unreadable,
        }
        let Ok(packed) = read(b"packed-refs") else {
            return Head::Unreadable;
        };
        let packed = packed.unwrap_or_default();
        let found = packed.split(|&b| b == b'\n').find_map(|line| {
            let space = line.iter().position(|&b| b == b' ')?;
            let (hex, rest) = (&line[..space], &line[space + 1..]);
            (rest.trim_ascii_end() == &name[..]).then_some(hex)
        });
        return match found {
            Some(hex) => Oid::from_hex_bytes(hash, hex).map_or(Head::Unreadable, Head::Commit),
            None => Head::Unborn,
        };
    }
    Head::Unreadable
}

#[cfg(unix)]
fn rela(base: &Path, bytes: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    base.join(std::ffi::OsStr::from_bytes(bytes))
}

#[cfg(not(unix))]
fn rela(base: &Path, bytes: &[u8]) -> PathBuf {
    base.join(String::from_utf8_lossy(bytes).into_owned())
}

impl Rules<'_> {
    /// Whether the conversion rules of `path` differ from the baseline's.
    fn changed(
        &mut self,
        attrs: &mut WorktreeAttrs<'_>,
        path: &RepoPath,
    ) -> Result<bool, GitError> {
        Ok(match self {
            Rules::Same => false,
            Rules::All => true,
            Rules::Compare(old) => match old.get(path) {
                Ok(o) => o.rule != attrs.get(path)?.rule,
                Err(_) => true,
            },
        })
    }
}

/// The worktree side of a tracked path with a stage-0 entry that isn't
/// skip-worktree (docs/design/git.md「工作区比较」第 1–3 步). `stat_clean`:
/// the index entry's stat matches and isn't racy.
#[allow(
    clippy::too_many_arguments,
    reason = "one step of `run`, kept apart for reading"
)]
fn tracked_side(
    reader: &mut Reader<'_>,
    attrs: &mut WorktreeAttrs<'_>,
    rules: &mut Rules<'_>,
    path: &RepoPath,
    entry: (Mode, Oid),
    meta: Meta,
    base: Option<(Mode, Oid)>,
    stat_clean: bool,
) -> Result<Seen, GitError> {
    let (mode, oid) = entry;
    match meta.kind {
        FileKind::Dir if mode == Mode::Gitlink => return Ok(reader.submodule(path)),
        // The file was replaced by a directory; the walk lists its files.
        FileKind::Dir => return Ok(Seen::Absent),
        FileKind::File | FileKind::Symlink => {}
        _ => return Ok(Seen::Side(special(Unavailable::UnsupportedType))),
    }
    let worktree_mode = reader.mode(&meta, Some(mode));
    let clean = mode != Mode::Gitlink
        && worktree_mode == mode
        && stat_clean
        && !rules.changed(attrs, path)?;
    if clean {
        // The filter fast path: unchanged only when the index also equals
        // the base; otherwise the index blob may not be what the worktree
        // file cleans to.
        if base == Some(entry) || meta.kind != FileKind::File || !attrs.get(path)?.filtered {
            return Ok(Seen::SameAsIndex);
        }
    }
    let index_mode = (mode != Mode::Gitlink).then_some(mode);
    reader.read(attrs, path, &meta, index_mode, Some(oid))
}

/// The table in docs/design/git.md「工作区比较」, for a path with a stage-0
/// index entry.
fn classify(
    path: RepoPath,
    base: Option<(Mode, Oid)>,
    index: (Mode, Oid),
    seen: Seen,
) -> Option<Pending> {
    let staged = Some(Staged {
        oid: index.1,
        mode: index.0,
    });
    let side = match seen {
        Seen::Absent => {
            let mut p = match base {
                None => {
                    let mut p = Pending::new(Status::Added, path, None, Some(blob_side(index)));
                    p.staged_only = true;
                    p
                }
                Some(b) => Pending::new(Status::Deleted, path, Some(blob_side(b)), None),
            };
            p.staged = staged;
            return Some(p);
        }
        Seen::SameAsIndex => blob_side(index),
        Seen::Side(mut side) => {
            // Content the index (or the base) has is in the object database.
            if let Content::Blob(oid) = side.content
                && (oid == index.1 || base.is_some_and(|b| b.1 == oid))
            {
                side.bytes = None;
            }
            side
        }
    };
    let w = match side.content {
        Content::Blob(oid) => Some((side.mode, oid)),
        _ => None,
    };
    let w_is_index = w == Some(index);
    if base == Some(index) && w_is_index {
        return None;
    }
    if let Some(b) = base
        && w == Some(b)
    {
        // Only the index differs: base → index.
        let status = if same_kind(b.0, index.0) {
            Status::Modified
        } else {
            Status::TypeChanged
        };
        let mut p = Pending::new(status, path, Some(blob_side(b)), Some(blob_side(index)));
        p.staged = staged;
        p.staged_only = true;
        return Some(p);
    }
    let status = match base {
        None => Status::Added,
        Some(b) if same_kind(b.0, side.mode) => Status::Modified,
        Some(_) => Status::TypeChanged,
    };
    let comparable = w.is_some() || side.mode == Mode::Special;
    let mut p = Pending::new(status, path, base.map(blob_side), Some(side));
    p.staged = if w_is_index || !comparable {
        None
    } else {
        staged
    };
    Some(p)
}

/// Counting hook: worktree content reads on this thread.
#[cfg(test)]
pub(crate) mod hook {
    use std::cell::Cell;

    thread_local! {
        static READS: Cell<usize> = const { Cell::new(0) };
    }

    pub(crate) fn read() {
        READS.with(|c| c.set(c.get() + 1));
    }

    pub(crate) fn reads() -> usize {
        READS.with(Cell::get)
    }
}

#[cfg(not(test))]
pub(crate) mod hook {
    pub(crate) fn read() {}
}

#[cfg(test)]
mod tests;
