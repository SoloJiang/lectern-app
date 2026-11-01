//! Reading a repository's reflog into the database (docs/design/usage.md
//! 「分支观察」「数据库」): the spans a repository was watched, the HEAD each
//! worktree had when the watch began, the reflog records `fs_watch`
//! delivered with the times they were read, and the segments they make.
//!
//! The records are stored parsed: what a replay of HEAD reads (the times,
//! the old and new commits, the branches a switch leaves and enters), not the
//! line, which carries a name, an address and a commit message.

use std::collections::HashSet;
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

fn looks_like_commit(s: &str) -> bool {
    (7..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit())
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
    /// order they were read.
    pub(crate) fn repo_batch(
        &self,
        common_dir: &Path,
        batch: &RepoBatch,
    ) -> Result<Stored, StoreError> {
        self.transaction(|tx| {
            let repo = location(tx, &Place::Git(common_dir.to_path_buf()))?;
            let mut stored = Stored::default();
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
            for (_, item) in items {
                match item {
                    Item::Record(rec) => {
                        record(tx, repo, common_dir, rec, &renamed_to, &mut stored)?
                    }
                    Item::Reset(reset) => {
                        self.reset(tx, repo, common_dir, reset, &renamed_from, &mut stored)?;
                    }
                }
            }
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
            Ok(stored)
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

fn record(
    tx: &Connection,
    repo: i64,
    common_dir: &Path,
    rec: &ReflogRecord,
    renamed_to: &HashSet<String>,
    stored: &mut Stored,
) -> rusqlite::Result<()> {
    let Some(hash) = hash_of(&rec.line) else {
        stored.skipped += 1;
        return Ok(());
    };
    let Ok(entry) = parse_reflog_line(hash, &rec.line) else {
        stored.skipped += 1;
        return Ok(());
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
    match &rec.file {
        ReflogFile::Head(_) => match &entry.action {
            ReflogAction::Checkout { from, to } => {
                let to = String::from_utf8_lossy(to).into_owned();
                let from = String::from_utf8_lossy(from).into_owned();
                let to_ref = format!("refs/heads/{to}");
                let known = segments::open_named(tx, repo, &to_ref)?.is_some();
                let from_ref = (!looks_like_commit(&from)
                    || segments::open_named(tx, repo, &format!("refs/heads/{from}"))?.is_some())
                .then(|| format!("refs/heads/{from}"));
                if let Some(from_ref) = &from_ref {
                    segments::ensure_open(tx, repo, from_ref)?;
                }
                if !known && looks_like_commit(&to) {
                    act = ACT_DETACH;
                    segments::detached(tx, repo, wt, at)?;
                } else {
                    act = ACT_SWITCH;
                    segments::ensure_open(tx, repo, &to_ref)?;
                    segments::reattached(tx, repo, wt, at)?;
                    b = Some(to_ref);
                }
                a = from_ref;
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
                return Ok(());
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
    Ok(())
}
