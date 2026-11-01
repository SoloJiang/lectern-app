//! Reading a repository's reflog into the database (docs/design/usage.md
//! 「分支观察」「数据库」): the spans a repository was watched, the HEAD each
//! worktree had when the watch began, the reflog records `fs_watch`
//! delivered with the times they were read, and the segments they make.
//!
//! The records are stored parsed: what a replay of HEAD reads (the times,
//! the old and new commits, the branches a switch leaves and enters), not the
//! line, which carries a name, an address and a commit message.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use lectern_fs_watch::{
    ReflogFile, ReflogRecord, ReflogReset, RepoBatch, ResetReason, WorktreeName,
};
use lectern_git::{HashKind, ReflogAction, parse_reflog_line};
use lectern_logging::debug;
use rusqlite::{Connection, OptionalExtension, params};

use super::ingest::{Place, intern, location, path_bytes};
use super::segments;
use super::{Store, StoreError};
use crate::gitread::{BranchLog, WorktreeHead};

/// `reflog.file`.
pub(crate) const FILE_HEAD: i64 = 0;
pub(crate) const FILE_BRANCH: i64 = 1;
pub(crate) const FILE_STASH: i64 = 2;

/// `reflog.act`: what a record did, for the replay.
pub(crate) const ACT_OTHER: i64 = 0;
/// HEAD went to a branch (`b`), from `a`.
pub(crate) const ACT_SWITCH: i64 = 1;
/// HEAD went to a commit, from the branch `a`.
pub(crate) const ACT_DETACH: i64 = 2;
/// A commit was made (or picked): `old` to `new`.
pub(crate) const ACT_COMMIT: i64 = 3;
/// A branch was created.
pub(crate) const ACT_CREATED: i64 = 4;
/// A branch was renamed from `a` to `b`.
pub(crate) const ACT_RENAMED: i64 = 5;
pub(crate) const ACT_RESET: i64 = 6;

/// `reflog_reset.reason`.
const RESET_REWRITTEN: i64 = 1;
const RESET_REMOVED: i64 = 2;
const RESET_UNREADABLE: i64 = 3;
/// HEAD read after the records said something else: the calls around the
/// read are unsure.
const RESET_CORRECTED: i64 = 4;

/// What a watch starts from.
pub(crate) struct Arm<'a> {
    pub(crate) common_dir: &'a Path,
    /// The wall clock (nanoseconds) when the watch was armed.
    pub(crate) at_ns: i64,
    pub(crate) heads: &'a [WorktreeHead],
    /// The reflogs of the branches the worktrees are on.
    pub(crate) logs: &'a [(String, BranchLog)],
}

/// What a batch of records came to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Stored {
    pub(crate) records: usize,
    pub(crate) resets: usize,
    pub(crate) skipped: usize,
}

/// What the repository says at the time a batch is written (read off the
/// writer): which of the branches HEAD went to exist. None: not read.
#[derive(Clone, Debug, Default)]
pub(crate) struct Facts {
    pub(crate) branches: Option<HashSet<String>>,
}

/// Where a record left the HEAD of a worktree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Moved {
    /// HEAD is on this branch (None: detached) after it.
    To(Option<String>),
    /// HEAD moved but where is not known from the record alone.
    Unknown,
}

/// What the engine checks against HEAD after batches were written, per
/// repository: where the last records of a worktree left it, the worktrees
/// that have records and no baseline, and whether the list of worktrees
/// changed.
#[derive(Clone, Debug, Default)]
pub(crate) struct Reconcile {
    pub(crate) moved: HashMap<PathBuf, Moved>,
    pub(crate) fresh: HashSet<PathBuf>,
    pub(crate) scan: bool,
}

impl Reconcile {
    pub(crate) fn is_empty(&self) -> bool {
        self.moved.is_empty() && self.fresh.is_empty() && !self.scan
    }
}

/// The full reference names HEAD went to in a batch (the targets of
/// `checkout: moving from A to B`): the branches whose existence the writer
/// needs to tell a switch from a detach.
pub(crate) fn checkout_targets(batch: &RepoBatch) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for rec in &batch.reflog {
        if !matches!(rec.file, ReflogFile::Head(_)) {
            continue;
        }
        let Some(hash) = hash_of(&rec.line) else {
            continue;
        };
        if let Ok(entry) = parse_reflog_line(hash, &rec.line)
            && let ReflogAction::Checkout { to, .. } = entry.action
        {
            let name = format!("refs/heads/{}", String::from_utf8_lossy(&to));
            if !out.contains(&name) {
                out.push(name);
            }
        }
    }
    out
}

pub(crate) fn ns(t: SystemTime) -> i64 {
    t.duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_nanos()).unwrap_or(i64::MAX))
}

/// The key of a worktree: the path of its administrative directory.
pub(crate) fn worktree_key(common_dir: &Path, name: &WorktreeName) -> PathBuf {
    match name {
        WorktreeName::Main => common_dir.to_path_buf(),
        WorktreeName::Linked(id) => common_dir.join("worktrees").join(&**id),
    }
}

/// Whether `s`, a name in a `checkout: moving from A to B` line, is the
/// (abbreviated) commit `oid`: git writes the commit when HEAD goes
/// detached. A branch that is called like a number (`20261101`) is not one,
/// unless it is the abbreviation of the commit HEAD went to.
fn is_commit(s: &str, oid: Option<&lectern_git::Oid>) -> bool {
    (7..=64).contains(&s.len())
        && s.bytes().all(|b| b.is_ascii_hexdigit())
        && oid.is_some_and(|o| o.to_hex().starts_with(&s.to_ascii_lowercase()))
}

/// The worktrees whose HEAD reflog has a gap in a batch: a file rewritten or
/// unreadable, or (`lost`) every HEAD file the batch had records of.
pub(crate) fn gap_worktrees(common_dir: &Path, batch: &RepoBatch, lost: bool) -> HashSet<PathBuf> {
    let mut out = HashSet::new();
    for reset in &batch.reflog_resets {
        if let ReflogFile::Head(w) = &reset.file
            && !matches!(reset.reason, ResetReason::Removed)
        {
            out.insert(worktree_key(common_dir, w));
        }
    }
    if lost {
        for rec in &batch.reflog {
            if let ReflogFile::Head(w) = &rec.file {
                out.insert(worktree_key(common_dir, w));
            }
        }
    }
    out
}

fn hash_of(line: &[u8]) -> Option<HashKind> {
    match line.iter().position(|&b| b == b' ')? {
        40 => Some(HashKind::Sha1),
        64 => Some(HashKind::Sha256),
        _ => None,
    }
}

fn run_id(tx: &Connection, run: lectern_fs_watch::RunId) -> rusqlite::Result<i64> {
    let key = run.0 as i64;
    tx.prepare_cached("INSERT OR IGNORE INTO run (run) VALUES (?1)")?
        .execute(params![key])?;
    tx.prepare_cached("SELECT id FROM run WHERE run = ?1")?
        .query_row(params![key], |r| r.get(0))
}

fn repo_id(conn: &Connection, common_dir: &Path) -> rusqlite::Result<Option<i64>> {
    conn.prepare_cached("SELECT id FROM location WHERE kind = 0 AND path = ?1")?
        .query_row(params![path_bytes(common_dir)], |r| r.get(0))
        .optional()
}

impl Store {
    /// A repository is being watched from now: a span begins, the worktrees'
    /// HEADs are the baselines the records that follow are replayed from,
    /// and the branches they are on have their segments.
    pub(crate) fn arm(&self, arm: &Arm<'_>) -> Result<(), StoreError> {
        self.transaction(|tx| {
            let repo = location(tx, &Place::Git(arm.common_dir.to_path_buf()))?;
            let at = arm.at_ns / 1_000_000;
            tx.prepare_cached(
                "UPDATE watch_span SET end_ns = ?2 WHERE repo = ?1 AND end_ns IS NULL",
            )?
            .execute(params![repo, arm.at_ns])?;
            tx.prepare_cached(
                "INSERT INTO watch_span (repo, begin_ns, last_ns, at) VALUES (?1, ?2, ?2, ?3)",
            )?
            .execute(params![repo, arm.at_ns, at])?;
            for head in arm.heads {
                let wt = intern(tx, path_bytes(&head.key))?;
                match &head.branch {
                    Some(name) => {
                        match arm.logs.iter().find(|(n, _)| n == name) {
                            Some((_, log)) if log.exists => {
                                segments::import(tx, repo, name, log)?;
                            }
                            _ => {
                                segments::ensure_open(tx, repo, name)?;
                            }
                        }
                        segments::reattached(tx, repo, wt, at)?;
                    }
                    None => {
                        segments::detached_now(tx, repo, wt)?;
                    }
                }
                tx.prepare_cached(
                    "INSERT INTO head_baseline (repo, wt, at_ns, branch, at)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                )?
                .execute(params![repo, wt, arm.at_ns, head.branch, at])?;
            }
            Ok(())
        })
    }

    /// HEAD of some worktrees was read again, off the writer, after a gap in
    /// their reflog (a rewrite, a read that failed, a batch that could not be
    /// written): each is a new baseline at `at_ns`, so that the replay of a
    /// later call starts after the gap.
    pub(crate) fn rebaseline(
        &self,
        common_dir: &Path,
        at_ns: i64,
        heads: &[WorktreeHead],
        only: &HashSet<PathBuf>,
    ) -> Result<(), StoreError> {
        self.transaction(|tx| {
            let repo = location(tx, &Place::Git(common_dir.to_path_buf()))?;
            for head in heads.iter().filter(|h| only.contains(&h.key)) {
                let wt = intern(tx, path_bytes(&head.key))?;
                match &head.branch {
                    Some(name) => {
                        segments::ensure_open(tx, repo, name)?;
                    }
                    None => {
                        segments::detached_now(tx, repo, wt)?;
                    }
                }
                tx.prepare_cached(
                    "INSERT INTO head_baseline (repo, wt, at_ns, branch, at)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                )?
                .execute(params![
                    repo,
                    wt,
                    at_ns,
                    head.branch,
                    at_ns / 1_000_000
                ])?;
            }
            Ok(())
        })
    }

    /// HEAD of the worktrees was read, off the writer, after batches were
    /// written (`at_ns` is the time before the read). A worktree where the
    /// records left HEAD somewhere else than it is: the records could not
    /// say (`checkout --detach main` is written as a switch to `main`), so
    /// HEAD is the new baseline and the calls up to it are unsure. A worktree
    /// that has records and no baseline, or that is new in the list of
    /// worktrees and has none (it was made while the watch ran, and git writes
    /// no switch for it), gets one.
    pub(crate) fn reconcile(
        &self,
        common_dir: &Path,
        at_ns: i64,
        heads: &[WorktreeHead],
        what: &Reconcile,
    ) -> Result<(), StoreError> {
        self.transaction(|tx| {
            let repo = location(tx, &Place::Git(common_dir.to_path_buf()))?;
            let open: Option<i64> = tx
                .prepare_cached(
                    "SELECT max(begin_ns) FROM watch_span WHERE repo = ?1 AND end_ns IS NULL",
                )?
                .query_row(params![repo], |r| r.get(0))?;
            if open.is_none() {
                return Ok(());
            }
            let at = at_ns / 1_000_000;
            for head in heads {
                let wt = intern(tx, path_bytes(&head.key))?;
                let corrected = matches!(
                    what.moved.get(&head.key),
                    Some(Moved::To(predicted)) if *predicted != head.branch
                );
                let missing =
                    (what.scan || what.fresh.contains(&head.key)) && !has_baseline(tx, repo, wt)?;
                if !corrected && !missing {
                    continue;
                }
                match &head.branch {
                    Some(name) => {
                        segments::ensure_open(tx, repo, name)?;
                        segments::reattached(tx, repo, wt, at)?;
                    }
                    None => {
                        segments::detached_now(tx, repo, wt)?;
                    }
                }
                tx.prepare_cached(
                    "INSERT INTO head_baseline (repo, wt, at_ns, branch, at)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                )?
                .execute(params![repo, wt, at_ns, head.branch, at])?;
                if corrected {
                    let last: Option<i64> = tx
                        .prepare_cached(
                            "SELECT run FROM reflog WHERE repo = ?1 AND file = ?2 AND wt = ?3
                             ORDER BY id DESC LIMIT 1",
                        )?
                        .query_row(params![repo, FILE_HEAD, wt], |r| r.get(0))
                        .optional()?;
                    let run = match last {
                        Some(run) => run,
                        None => run_id(tx, lectern_fs_watch::RunId(0))?,
                    };
                    tx.prepare_cached(
                        "INSERT INTO reflog_reset (repo, file, wt, run, seq, reason, obs_ns, at)
                         VALUES (?1, ?2, ?3, ?4, 0, ?5, ?6, ?7)",
                    )?
                    .execute(params![
                        repo,
                        FILE_HEAD,
                        wt,
                        run,
                        RESET_CORRECTED,
                        at_ns,
                        at
                    ])?;
                }
            }
            Ok(())
        })
    }

    /// The watch of a repository ended.
    pub(crate) fn disarm(&self, common_dir: &Path, at_ns: i64) -> Result<(), StoreError> {
        self.transaction(|tx| {
            if let Some(repo) = repo_id(tx, common_dir)? {
                tx.prepare_cached(
                    "UPDATE watch_span SET end_ns = ?2 WHERE repo = ?1 AND end_ns IS NULL",
                )?
                .execute(params![repo, at_ns])?;
            }
            Ok(())
        })
    }

    /// Spans a run left open (it did not end cleanly) end at the last record
    /// read in them.
    pub(crate) fn close_stale_spans(&self) -> Result<usize, StoreError> {
        self.transaction(|tx| {
            tx.prepare_cached("UPDATE watch_span SET end_ns = last_ns WHERE end_ns IS NULL")?
                .execute([])
        })
    }

    /// Writes the records of a batch and the segments they make, in the
    /// order they were read, with nothing known about the repository beyond
    /// the records.
    pub(crate) fn repo_batch(
        &self,
        common_dir: &Path,
        batch: &RepoBatch,
    ) -> Result<Stored, StoreError> {
        self.repo_batch_with(common_dir, batch, &Facts::default())
            .map(|(stored, _)| stored)
    }

    /// Writes a batch with what the repository says (`facts`, read off the
    /// writer): a checkout of a name that is not a branch is a detach. Also
    /// says where the batch left each worktree's HEAD, for the engine to
    /// check against the real HEAD, and which worktrees have records and no
    /// baseline.
    pub(crate) fn repo_batch_with(
        &self,
        common_dir: &Path,
        batch: &RepoBatch,
        facts: &Facts,
    ) -> Result<(Stored, Reconcile), StoreError> {
        self.transaction(|tx| {
            let repo = location(tx, &Place::Git(common_dir.to_path_buf()))?;
            let mut stored = Stored::default();
            let mut reconcile = Reconcile::default();
            // The branches whose logs went away in the batch: a switch to one
            // of them was a switch to a branch that is gone by now.
            let removed: HashSet<String> = batch
                .reflog_resets
                .iter()
                .filter(|r| matches!(r.reason, ResetReason::Removed))
                .filter_map(|r| match &r.file {
                    ReflogFile::Branch(name) => Some(String::from_utf8_lossy(name).into_owned()),
                    _ => None,
                })
                .collect();
            // A rename leaves the old name's file behind (removed) and starts
            // the new one with the old lines: neither is the branch going.
            let mut renamed_from: HashSet<String> = HashSet::new();
            let mut renamed_to: HashSet<String> = HashSet::new();
            for rec in &batch.reflog {
                if let ReflogFile::Branch(_) = rec.file
                    && let Some(hash) = hash_of(&rec.line)
                    && let Ok(entry) = parse_reflog_line(hash, &rec.line)
                    && let ReflogAction::BranchRenamed { from, to } = entry.action
                {
                    renamed_from.insert(from.as_str().to_owned());
                    renamed_to.insert(to.as_str().to_owned());
                }
            }
            enum Item<'a> {
                Record(&'a ReflogRecord),
                Reset(&'a ReflogReset),
            }
            let mut items: Vec<(u64, Item<'_>)> = batch
                .reflog
                .iter()
                .map(|r| (r.seq, Item::Record(r)))
                .chain(batch.reflog_resets.iter().map(|r| (r.seq, Item::Reset(r))))
                .collect();
            items.sort_by_key(|(seq, _)| *seq);
            // Where HEAD of each worktree was left by the batch, and the last
            // read of its records: the next baseline.
            let mut heads: HashMap<i64, Moved> = HashMap::new();
            let mut last_read: HashMap<i64, i64> = HashMap::new();
            let mut keys: HashMap<i64, PathBuf> = HashMap::new();
            for (_, item) in items {
                match item {
                    Item::Record(rec) => {
                        if let Some((wt, moved)) = record(
                            tx,
                            repo,
                            common_dir,
                            rec,
                            &Known {
                                renamed_to: &renamed_to,
                                removed: &removed,
                                facts,
                            },
                            &mut stored,
                        )? {
                            // A rename after a switch to the branch is known.
                            let known = match (&moved, heads.get(&wt)) {
                                (Moved::Unknown, Some(Moved::To(Some(now)))) => {
                                    renamed_head(tx, rec, now)
                                }
                                _ => moved,
                            };
                            heads.insert(wt, known);
                        }
                        if let ReflogFile::Head(w) = &rec.file {
                            let key = worktree_key(common_dir, w);
                            let wt = intern(tx, path_bytes(&key))?;
                            keys.insert(wt, key);
                            let at = last_read.entry(wt).or_insert(0);
                            *at = (*at).max(ns(rec.observed.at));
                        }
                    }
                    Item::Reset(reset) => {
                        self.reset(tx, repo, common_dir, reset, &renamed_from, &mut stored)?;
                        // Records may have been lost: no baseline over them.
                        if let ReflogFile::Head(w) = &reset.file
                            && !matches!(reset.reason, ResetReason::Removed)
                        {
                            let key = worktree_key(common_dir, w);
                            let wt = intern(tx, path_bytes(&key))?;
                            keys.insert(wt, key);
                            heads.insert(wt, Moved::Unknown);
                        }
                    }
                }
            }
            for (wt, moved) in &heads {
                if let Some(key) = keys.get(wt) {
                    reconcile.moved.insert(key.clone(), moved.clone());
                }
            }
            // HEAD read again: the replay of a later call starts from here,
            // not from the start of the watch, so that settling does not
            // cost more the longer a repository has been watched.
            for (wt, moved) in heads {
                if let (Moved::To(branch), Some(at_ns)) = (moved, last_read.get(&wt)) {
                    tx.prepare_cached(
                        "INSERT INTO head_baseline (repo, wt, at_ns, branch, at)
                         VALUES (?1, ?2, ?3, ?4, ?5)",
                    )?
                    .execute(params![
                        repo,
                        wt,
                        at_ns,
                        branch,
                        at_ns / 1_000_000
                    ])?;
                }
            }
            // A worktree with records and no baseline in this watch (made
            // while it ran: no switch is written for it) has to be read.
            for (wt, key) in &keys {
                if !has_baseline(tx, repo, *wt)? {
                    reconcile.fresh.insert(key.clone());
                }
            }
            reconcile.scan = batch.worktree_list;
            if let Some(last) = batch
                .reflog
                .iter()
                .map(|r| ns(r.observed.at))
                .chain(batch.reflog_resets.iter().map(|r| ns(r.observed.at)))
                .max()
            {
                tx.prepare_cached(
                    "UPDATE watch_span SET last_ns = max(last_ns, ?2)
                     WHERE repo = ?1 AND end_ns IS NULL",
                )?
                .execute(params![repo, last])?;
            }
            debug!(
                "reflog batch stored";
                records = stored.records,
                resets = stored.resets,
                skipped = stored.skipped
            );
            Ok((stored, reconcile))
        })
    }

    /// A batch could not be written: the HEAD files it holds records of have
    /// a gap, so that the calls around it are unsure (the records are lost).
    pub(crate) fn batch_lost(
        &self,
        common_dir: &Path,
        batch: &RepoBatch,
    ) -> Result<(), StoreError> {
        self.transaction(|tx| {
            let repo = location(tx, &Place::Git(common_dir.to_path_buf()))?;
            let mut seen: HashSet<WorktreeName> = HashSet::new();
            let at_ns = batch
                .reflog
                .iter()
                .map(|r| ns(r.observed.at))
                .max()
                .unwrap_or(0);
            let run = match batch.reflog.first() {
                Some(r) => run_id(tx, r.observed.run)?,
                None => return Ok(()),
            };
            for rec in &batch.reflog {
                if let ReflogFile::Head(w) = &rec.file
                    && seen.insert(w.clone())
                {
                    let wt = intern(tx, path_bytes(&worktree_key(common_dir, w)))?;
                    tx.prepare_cached(
                        "INSERT INTO reflog_reset (repo, file, wt, run, seq, reason, obs_ns, at)
                         VALUES (?1, ?2, ?3, ?4, 0, ?5, ?6, ?7)",
                    )?
                    .execute(params![
                        repo,
                        FILE_HEAD,
                        wt,
                        run,
                        RESET_UNREADABLE,
                        at_ns,
                        at_ns / 1_000_000
                    ])?;
                }
            }
            Ok(())
        })
    }

    fn reset(
        &self,
        tx: &Connection,
        repo: i64,
        common_dir: &Path,
        reset: &ReflogReset,
        renamed_from: &HashSet<String>,
        stored: &mut Stored,
    ) -> rusqlite::Result<()> {
        let (file, wt, name) = match &reset.file {
            ReflogFile::Head(w) => (
                FILE_HEAD,
                intern(tx, path_bytes(&worktree_key(common_dir, w)))?,
                None,
            ),
            ReflogFile::Branch(bytes) => {
                let name = String::from_utf8_lossy(bytes).into_owned();
                (FILE_BRANCH, intern(tx, name.as_bytes())?, Some(name))
            }
            ReflogFile::Stash => (FILE_STASH, 0, None),
        };
        let reason = match &reset.reason {
            ResetReason::Rewritten => RESET_REWRITTEN,
            ResetReason::Removed => RESET_REMOVED,
            ResetReason::Unreadable(_) => RESET_UNREADABLE,
        };
        let at_ns = ns(reset.observed.at);
        let run = run_id(tx, reset.observed.run)?;
        tx.prepare_cached(
            "INSERT INTO reflog_reset (repo, file, wt, run, seq, reason, obs_ns, at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )?
        .execute(params![
            repo,
            file,
            wt,
            run,
            i64::try_from(reset.seq).unwrap_or(i64::MAX),
            reason,
            at_ns,
            at_ns / 1_000_000
        ])?;
        stored.resets += 1;
        // A branch file removed: the branch is gone (unless it was renamed).
        if reason == RESET_REMOVED
            && let Some(name) = name
            && !renamed_from.contains(&name)
            && let Some((id, _, _)) = segments::open_named(tx, repo, &name)?
        {
            segments::end(tx, id, segments::END_REMOVED, at_ns / 1_000_000)?;
        }
        Ok(())
    }
}

/// Whether the worktree has a baseline in the watch that is open.
fn has_baseline(tx: &Connection, repo: i64, wt: i64) -> rusqlite::Result<bool> {
    tx.prepare_cached(
        "SELECT EXISTS (SELECT 1 FROM head_baseline
                        WHERE repo = ?1 AND wt = ?2
                          AND at_ns >= (SELECT coalesce(max(begin_ns), 0) FROM watch_span
                                        WHERE repo = ?1 AND end_ns IS NULL))",
    )?
    .query_row(params![repo, wt], |r| r.get(0))
}

/// What `record` knows besides the record.
struct Known<'a> {
    renamed_to: &'a HashSet<String>,
    removed: &'a HashSet<String>,
    facts: &'a Facts,
}

/// The state after a rename record of the branch HEAD was left on: the new
/// name when the old one is that branch.
fn renamed_head(tx: &Connection, rec: &ReflogRecord, now: &str) -> Moved {
    let _ = tx;
    let renamed = hash_of(&rec.line)
        .and_then(|hash| parse_reflog_line(hash, &rec.line).ok())
        .and_then(|entry| match entry.action {
            ReflogAction::BranchRenamed { from, to } if from.as_str() == now => {
                Some(to.as_str().to_owned())
            }
            _ => None,
        });
    match renamed {
        Some(to) => Moved::To(Some(to)),
        None => Moved::Unknown,
    }
}

fn record(
    tx: &Connection,
    repo: i64,
    common_dir: &Path,
    rec: &ReflogRecord,
    known_by: &Known<'_>,
    stored: &mut Stored,
) -> rusqlite::Result<Option<(i64, Moved)>> {
    let renamed_to = known_by.renamed_to;
    let Some(hash) = hash_of(&rec.line) else {
        stored.skipped += 1;
        return Ok(None);
    };
    let Ok(entry) = parse_reflog_line(hash, &rec.line) else {
        stored.skipped += 1;
        return Ok(None);
    };
    let at = entry.time.seconds.saturating_mul(1000);
    let (file, wt) = match &rec.file {
        ReflogFile::Head(w) => (
            FILE_HEAD,
            intern(tx, path_bytes(&worktree_key(common_dir, w)))?,
        ),
        ReflogFile::Branch(bytes) => (FILE_BRANCH, intern(tx, bytes)?),
        ReflogFile::Stash => (FILE_STASH, 0),
    };
    let mut act = ACT_OTHER;
    let mut a: Option<String> = None;
    let mut b: Option<String> = None;
    let mut moved: Option<Moved> = None;
    match &rec.file {
        ReflogFile::Head(_) => match &entry.action {
            ReflogAction::Checkout { from, to } => {
                let to = String::from_utf8_lossy(to).into_owned();
                let from = String::from_utf8_lossy(from).into_owned();
                let to_ref = format!("refs/heads/{to}");
                let known = segments::open_named(tx, repo, &to_ref)?.is_some();
                // HEAD went to a branch when the name is one. The line does
                // not say: the argument is written as typed, a tag, a
                // remote-tracking name or `main~1` as well. Without a read
                // of the repository, a name that is not the new commit is
                // taken for a branch.
                let branch = match &known_by.facts.branches {
                    Some(exist) => {
                        known || exist.contains(&to_ref) || known_by.removed.contains(&to_ref)
                    }
                    None => known || !is_commit(&to, entry.new.as_ref()),
                };
                let from_ref = (!is_commit(&from, entry.old.as_ref())
                    || segments::open_named(tx, repo, &format!("refs/heads/{from}"))?.is_some())
                .then(|| format!("refs/heads/{from}"));
                if let Some(from_ref) = &from_ref {
                    segments::ensure_open(tx, repo, from_ref)?;
                }
                if !branch {
                    act = ACT_DETACH;
                    segments::detached(tx, repo, wt, at)?;
                    moved = Some(Moved::To(None));
                } else {
                    act = ACT_SWITCH;
                    segments::ensure_open(tx, repo, &to_ref)?;
                    segments::reattached(tx, repo, wt, at)?;
                    moved = Some(Moved::To(Some(to_ref.clone())));
                    b = Some(to_ref);
                }
                a = from_ref;
            }
            // `git branch -m old new` on the branch HEAD is on: HEAD follows.
            ReflogAction::BranchRenamed { from, to } => {
                act = ACT_RENAMED;
                a = Some(from.as_str().to_owned());
                b = Some(to.as_str().to_owned());
                segments::renamed(tx, repo, from.as_str(), to.as_str(), at)?;
                moved = Some(Moved::Unknown);
            }
            ReflogAction::Commit { .. } | ReflogAction::CherryPick => act = ACT_COMMIT,
            ReflogAction::Reset => act = ACT_RESET,
            _ => {}
        },
        ReflogFile::Branch(bytes) => {
            let name = String::from_utf8_lossy(bytes).into_owned();
            if let ReflogAction::BranchRenamed { from, to } = &entry.action {
                act = ACT_RENAMED;
                a = Some(from.as_str().to_owned());
                b = Some(to.as_str().to_owned());
                segments::renamed(tx, repo, from.as_str(), to.as_str(), at)?;
            } else if rec.origin == lectern_fs_watch::Origin::NewFile && renamed_to.contains(&name)
            {
                // The lines the rename carried over.
                stored.skipped += 1;
                return Ok(None);
            } else if entry.old.is_none() {
                act = ACT_CREATED;
                segments::created(tx, repo, &name, at)?;
            } else if matches!(entry.action, ReflogAction::Reset) {
                act = ACT_RESET;
            }
        }
        ReflogFile::Stash => {}
    }
    let run = run_id(tx, rec.observed.run)?;
    tx.prepare_cached(
        "INSERT INTO reflog (repo, file, wt, run, seq, off, at, obs_ns, aft_ns, origin,
                             old, new, act, a, b)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
    )?
    .execute(params![
        repo,
        file,
        wt,
        run,
        i64::try_from(rec.seq).unwrap_or(i64::MAX),
        i64::try_from(rec.offset).unwrap_or(i64::MAX),
        at,
        ns(rec.observed.at),
        rec.after.map(|s| ns(s.at)),
        match rec.origin {
            lectern_fs_watch::Origin::Appended => 0,
            lectern_fs_watch::Origin::NewFile => 1,
        },
        entry.old.as_ref().map(|o| o.as_bytes().to_vec()),
        entry.new.as_ref().map(|o| o.as_bytes().to_vec()),
        act,
        a,
        b
    ])?;
    stored.records += 1;
    Ok(moved.map(|m| (wt, m)))
}
