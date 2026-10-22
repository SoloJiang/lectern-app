//! Parsing reflog lines (docs/design/git.md「引用与历史」), pure functions on
//! any thread, and reading the tail of a worktree's HEAD reflog backwards
//! (`Worktree::head_reflog`, blocking).

use std::fmt;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;

use crate::{GitError, GitTime, HashKind, Oid, RefName, gitdir};

/// The end of a worktree's HEAD reflog (`logs/HEAD` in its git directory),
/// as [`Worktree::head_reflog`](crate::Worktree::head_reflog) read it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct HeadReflog {
    /// False without a reflog file (`core.logAllRefUpdates = false`); then
    /// `entries` is empty and `complete` true.
    pub exists: bool,
    /// Newest first.
    pub entries: Vec<ReflogEntry>,
    /// The reading reached the file's first line, so the file has no record
    /// older than the last of `entries`. False when it stopped at
    /// `max_entries`, at the 4 MiB bound or at the record older than
    /// `stop`, unless that record is the first line.
    pub complete: bool,
}

/// How much of the end of the HEAD reflog `head_reflog` reads at most.
pub(crate) const HEAD_REFLOG_MAX_BYTES: u64 = 4 * 1024 * 1024;
/// The reads backwards from the end, one chunk at a time.
const CHUNK_BYTES: usize = 64 * 1024;

/// When the backwards reading stops.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Tail {
    /// Stop after the first record older than this (Unix seconds).
    pub stop: i64,
    pub max_entries: usize,
    /// Bytes from the end of the file.
    pub max_bytes: u64,
    pub chunk: usize,
}

/// `Worktree::head_reflog` for the reflog at `path`.
pub(crate) fn head_reflog(
    path: &Path,
    hash: HashKind,
    stop: i64,
    max_entries: usize,
) -> Result<HeadReflog, GitError> {
    let Some(mut file) = gitdir::open(path)? else {
        return Ok(HeadReflog {
            exists: false,
            entries: Vec::new(),
            complete: true,
        });
    };
    let len = file.metadata()?.len();
    let tail = Tail {
        stop,
        max_entries,
        max_bytes: HEAD_REFLOG_MAX_BYTES,
        chunk: CHUNK_BYTES,
    };
    let (entries, complete) = read_tail(&mut file, len, hash, tail)?;
    Ok(HeadReflog {
        exists: true,
        entries,
        complete,
    })
}

/// Parses the complete lines of the first `len` bytes of `src` from the
/// last one backwards, reading at most `tail.max_bytes` from the end in
/// chunks. Bytes after the last `\n` are a line still being appended and
/// are dropped, as is a line cut by the byte bound; lines that don't parse
/// are skipped, as git skips them. Returns the records newest first and
/// whether the file's first line was reached.
pub(crate) fn read_tail<R: Read + Seek>(
    src: &mut R,
    len: u64,
    hash: HashKind,
    tail: Tail,
) -> io::Result<(Vec<ReflogEntry>, bool)> {
    let mut entries = Vec::new();
    if tail.max_entries == 0 {
        return Ok((entries, len == 0));
    }
    let floor = len.saturating_sub(tail.max_bytes);
    let chunk = tail.chunk.max(1) as u64;
    // `pending` holds the bytes from `start` that are not parsed yet. Once
    // the incomplete last line is dropped it ends with `\n`, and every line
    // in it but the first is known to be whole.
    let mut start = len;
    let mut pending: Vec<u8> = Vec::new();
    let mut trimmed = false;
    loop {
        if start > floor {
            let from = start.saturating_sub(chunk).max(floor);
            let mut bytes = vec![0; usize::try_from(start - from).unwrap_or(usize::MAX)];
            src.seek(SeekFrom::Start(from))?;
            src.read_exact(&mut bytes)?;
            bytes.extend_from_slice(&pending);
            pending = bytes;
            start = from;
        }
        let exhausted = start == floor;
        if !trimmed {
            match pending.iter().rposition(|&b| b == b'\n') {
                Some(newline) => pending.truncate(newline + 1),
                // Nothing but one incomplete line.
                None if exhausted => return Ok((entries, start == 0)),
                None => continue,
            }
            trimmed = true;
        }
        let mut end = pending.len();
        // Every line that starts after a `\n` in `pending`, newest first.
        while let Some(newline) = pending[..end - 1].iter().rposition(|&b| b == b'\n') {
            let at = start + newline as u64 + 1;
            if take(&pending[newline + 1..end], at, hash, tail, &mut entries) {
                return Ok((entries, false));
            }
            end = newline + 1;
        }
        pending.truncate(end);
        if start == 0 {
            // The first line of the file.
            take(&pending, 0, hash, tail, &mut entries);
            return Ok((entries, true));
        }
        if exhausted {
            // Cut by the byte bound: it may be the end of a longer line.
            return Ok((entries, false));
        }
    }
}

/// Parses one line (starting at byte `at` of the file) into `entries`.
/// True when the reading stops here and older lines remain.
fn take(line: &[u8], at: u64, hash: HashKind, tail: Tail, entries: &mut Vec<ReflogEntry>) -> bool {
    let Ok(entry) = parse_reflog_line(hash, line) else {
        return false;
    };
    let older = entry.time.seconds < tail.stop;
    entries.push(entry);
    (older || entries.len() >= tail.max_entries) && at > 0
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ReflogEntry {
    /// None for all zeros (the reference didn't exist before).
    pub old: Option<Oid>,
    /// None for all zeros (the reference was deleted).
    pub new: Option<Oid>,
    /// Seconds and UTC offset, as written.
    pub time: GitTime,
    pub action: ReflogAction,
    /// The whole message after the tab, raw.
    pub message: Arc<[u8]>,
}

/// Classified by the message prefix git writes (git doesn't translate
/// reflog messages). Anything not recognised is `Other`; nothing is guessed.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ReflogAction {
    Commit {
        amend: bool,
        initial: bool,
    },
    /// `checkout: moving from A to B`; `git switch` writes the same.
    Checkout {
        from: Arc<[u8]>,
        to: Arc<[u8]>,
    },
    Reset,
    Merge,
    Rebase,
    CherryPick,
    Pull,
    BranchCreated,
    BranchRenamed {
        from: RefName,
        to: RefName,
    },
    Other,
}

/// Why a line isn't a reflog line.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ReflogParseError {
    /// The old or new OID isn't `hash.hex_len()` hex digits followed by a space.
    Oid,
    /// The identity isn't `name <email>`.
    Identity,
    /// The time isn't `<seconds> <+|-hhmm>`.
    Time,
}

impl fmt::Display for ReflogParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ReflogParseError::Oid => "reflog line: bad object ID",
            ReflogParseError::Identity => "reflog line: bad identity",
            ReflogParseError::Time => "reflog line: bad time",
        })
    }
}

impl std::error::Error for ReflogParseError {}

/// Parses one reflog line, `<old> <new> <name> <<email>> <seconds> <tz>\t<message>`
/// (a trailing newline is allowed; a line without a tab has an empty
/// message). `fs_watch` delivers new lines with their arrival time and
/// `usage` parses them (ADR 0020).
pub fn parse_reflog_line(hash: HashKind, line: &[u8]) -> Result<ReflogEntry, ReflogParseError> {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    let hex = hash.hex_len();
    let oid = |field: &[u8]| -> Result<Option<Oid>, ReflogParseError> {
        let oid = Oid::from_hex_bytes(hash, field).ok_or(ReflogParseError::Oid)?;
        Ok((!oid.is_null()).then_some(oid))
    };
    if line.len() < 2 * hex + 2 || line[hex] != b' ' || line[2 * hex + 1] != b' ' {
        return Err(ReflogParseError::Oid);
    }
    let old = oid(&line[..hex])?;
    let new = oid(&line[hex + 1..2 * hex + 1])?;
    let rest = &line[2 * hex + 2..];
    let (header, message) = match rest.iter().position(|&b| b == b'\t') {
        Some(tab) => (&rest[..tab], &rest[tab + 1..]),
        None => (rest, &b""[..]),
    };
    // `name <email> seconds tz`: the identity ends at the last `>`.
    let close = header
        .iter()
        .rposition(|&b| b == b'>')
        .ok_or(ReflogParseError::Identity)?;
    if !header[..close].contains(&b'<') {
        return Err(ReflogParseError::Identity);
    }
    let time = parse_time(&header[close + 1..]).ok_or(ReflogParseError::Time)?;
    Ok(ReflogEntry {
        old,
        new,
        time,
        action: classify(message),
        message: Arc::from(message),
    })
}

fn parse_time(field: &[u8]) -> Option<GitTime> {
    let text = std::str::from_utf8(field).ok()?;
    let mut parts = text.split_ascii_whitespace();
    let seconds: i64 = parts.next()?.parse().ok()?;
    let tz = parts.next()?.as_bytes();
    if parts.next().is_some() || tz.len() != 5 {
        return None;
    }
    let sign = match tz[0] {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    if !tz[1..].iter().all(u8::is_ascii_digit) {
        return None;
    }
    let digit = |i: usize| i32::from(tz[i] - b'0');
    let hours = digit(1) * 10 + digit(2);
    let minutes = digit(3) * 10 + digit(4);
    Some(GitTime {
        seconds,
        offset: sign * (hours * 3600 + minutes * 60),
    })
}

fn classify(message: &[u8]) -> ReflogAction {
    let starts = |p: &str| message.starts_with(p.as_bytes());
    if let Some(rest) = message.strip_prefix(b"checkout: moving from ") {
        if let Some(at) = find(rest, b" to ") {
            return ReflogAction::Checkout {
                from: Arc::from(&rest[..at]),
                to: Arc::from(&rest[at + 4..]),
            };
        }
        return ReflogAction::Other;
    }
    if starts("commit (initial): ") {
        return ReflogAction::Commit {
            amend: false,
            initial: true,
        };
    }
    if starts("commit (amend): ") {
        return ReflogAction::Commit {
            amend: true,
            initial: false,
        };
    }
    // Concluding a conflicted merge or cherry-pick.
    if starts("commit (merge): ") {
        return ReflogAction::Merge;
    }
    if starts("commit (cherry-pick): ") {
        return ReflogAction::CherryPick;
    }
    if starts("commit: ") {
        return ReflogAction::Commit {
            amend: false,
            initial: false,
        };
    }
    if starts("reset: ") {
        return ReflogAction::Reset;
    }
    if starts("merge ") {
        return ReflogAction::Merge;
    }
    // `rebase (start): …`, `rebase -i (finish): …`, `rebase: …`.
    if starts("rebase ") || starts("rebase:") {
        return ReflogAction::Rebase;
    }
    if starts("cherry-pick: ") || starts("cherry-pick ") {
        return ReflogAction::CherryPick;
    }
    // `pull: Fast-forward`, `pull --rebase …`, `pull origin main: …`.
    if starts("pull: ") || starts("pull ") {
        return ReflogAction::Pull;
    }
    if starts("branch: Created from ") {
        return ReflogAction::BranchCreated;
    }
    // `git branch -m` writes `Branch: renamed <old> to <new>`.
    let renamed = message
        .strip_prefix(b"Branch: renamed ")
        .or_else(|| message.strip_prefix(b"branch: renamed "));
    if let Some(rest) = renamed
        && let Some(at) = find(rest, b" to ")
        && let (Ok(from), Ok(to)) = (
            std::str::from_utf8(&rest[..at]),
            std::str::from_utf8(&rest[at + 4..]),
        )
        && let (Some(from), Some(to)) = (RefName::new(from), RefName::new(to))
    {
        return ReflogAction::BranchRenamed { from, to };
    }
    ReflogAction::Other
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "1111111111111111111111111111111111111111";
    const B: &str = "2222222222222222222222222222222222222222";
    const Z: &str = "0000000000000000000000000000000000000000";

    fn line(old: &str, new: &str, message: &str) -> Vec<u8> {
        format!("{old} {new} Jane Doe <jane@example.com> 1700000000 +0130\t{message}\n")
            .into_bytes()
    }

    fn action(message: &str) -> ReflogAction {
        parse_reflog_line(HashKind::Sha1, &line(A, B, message))
            .unwrap()
            .action
    }

    #[test]
    fn reflog_parses_fields() {
        let entry =
            parse_reflog_line(HashKind::Sha1, &line(Z, B, "commit (initial): first")).unwrap();
        assert_eq!(entry.old, None);
        assert_eq!(entry.new, Oid::from_hex(HashKind::Sha1, B));
        assert_eq!(
            entry.time,
            GitTime {
                seconds: 1_700_000_000,
                offset: 5400
            }
        );
        assert_eq!(&*entry.message, b"commit (initial): first");
        let deleted = parse_reflog_line(HashKind::Sha1, &line(A, Z, "x")).unwrap();
        assert_eq!(deleted.new, None);
        let west = format!("{A} {B} J <j@x> 5 -0800\tm");
        assert_eq!(
            parse_reflog_line(HashKind::Sha1, west.as_bytes())
                .unwrap()
                .time
                .offset,
            -8 * 3600
        );
        let no_message = format!("{A} {B} J <j@x> 5 +0000");
        let entry = parse_reflog_line(HashKind::Sha1, no_message.as_bytes()).unwrap();
        assert_eq!(
            (&*entry.message, entry.action),
            (&b""[..], ReflogAction::Other)
        );
    }

    #[test]
    fn reflog_parses_sha256_lines() {
        let old = "a".repeat(64);
        let new = "b".repeat(64);
        let text = format!("{old} {new} J <j@x> 5 +0000\tcommit: x");
        let entry = parse_reflog_line(HashKind::Sha256, text.as_bytes()).unwrap();
        assert_eq!(entry.new.unwrap().to_hex(), new);
        assert_eq!(
            parse_reflog_line(HashKind::Sha1, text.as_bytes()),
            Err(ReflogParseError::Oid)
        );
    }

    #[test]
    fn reflog_rejects_malformed_lines() {
        let bad_oid = format!("{} {B} J <j@x> 5 +0000\tm", "z".repeat(40));
        assert_eq!(
            parse_reflog_line(HashKind::Sha1, bad_oid.as_bytes()),
            Err(ReflogParseError::Oid)
        );
        assert_eq!(
            parse_reflog_line(HashKind::Sha1, b"short"),
            Err(ReflogParseError::Oid)
        );
        let no_identity = format!("{A} {B} 5 +0000\tm");
        assert_eq!(
            parse_reflog_line(HashKind::Sha1, no_identity.as_bytes()),
            Err(ReflogParseError::Identity)
        );
        for time in ["x +0000", "5 0000", "5 +00a0", "5", "5 +0000 extra"] {
            let text = format!("{A} {B} J <j@x> {time}\tm");
            assert_eq!(
                parse_reflog_line(HashKind::Sha1, text.as_bytes()),
                Err(ReflogParseError::Time),
                "{time}"
            );
        }
    }

    #[test]
    fn reflog_classifies_every_git_message() {
        use ReflogAction::*;
        let cases: &[(&str, ReflogAction)] = &[
            (
                "commit: fix the thing",
                Commit {
                    amend: false,
                    initial: false,
                },
            ),
            (
                "commit (initial): first",
                Commit {
                    amend: false,
                    initial: true,
                },
            ),
            (
                "commit (amend): fix the thing",
                Commit {
                    amend: true,
                    initial: false,
                },
            ),
            ("commit (merge): Merge branch 'x'", Merge),
            ("commit (cherry-pick): picked", CherryPick),
            ("reset: moving to HEAD~1", Reset),
            ("merge feature: Fast-forward", Merge),
            (
                "merge origin/main: Merge made by the 'ort' strategy.",
                Merge,
            ),
            ("rebase (start): checkout main", Rebase),
            ("rebase (pick): a commit", Rebase),
            ("rebase (finish): returning to refs/heads/x", Rebase),
            ("rebase -i (start): checkout HEAD~3", Rebase),
            ("rebase: aborting", Rebase),
            ("cherry-pick: picked", CherryPick),
            ("pull: Fast-forward", Pull),
            ("pull --rebase (start): checkout abc", Pull),
            ("pull origin main: Fast-forward", Pull),
            ("branch: Created from HEAD", BranchCreated),
            ("branch: Created from origin/main", BranchCreated),
            ("clone: from https://example.com/x.git", Other),
            ("update by push", Other),
            ("fetch: fast-forward", Other),
            ("checkout: moving from", Other),
            ("something new", Other),
        ];
        for (message, expected) in cases {
            assert_eq!(&action(message), expected, "{message}");
        }
        assert_eq!(
            action("checkout: moving from main to feature/x"),
            Checkout {
                from: Arc::from(&b"main"[..]),
                to: Arc::from(&b"feature/x"[..])
            }
        );
        assert_eq!(
            action("checkout: moving from 1111111 to main"),
            Checkout {
                from: Arc::from(&b"1111111"[..]),
                to: Arc::from(&b"main"[..])
            }
        );
        let renamed = BranchRenamed {
            from: RefName::new("refs/heads/old").unwrap(),
            to: RefName::new("refs/heads/new").unwrap(),
        };
        assert_eq!(
            action("Branch: renamed refs/heads/old to refs/heads/new"),
            renamed
        );
        assert_eq!(
            action("branch: renamed refs/heads/old to refs/heads/new"),
            renamed
        );
        assert_eq!(action("Branch: renamed nonsense"), Other);
    }
}
