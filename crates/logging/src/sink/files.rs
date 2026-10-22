//! Log file names, caps and cleanup (docs/design/logging.md「文件」).
//!
//! `lectern-<YYYYMMDDTHHMMSSZ>-<pid>.log` is a run's first file,
//! `lectern-<stamp>-<pid>.<seq>.log` the ones rotation opens after it.
//! Only names of this form are ever removed.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use super::fs;
use crate::engine::time;

/// The caps of one kind of file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Caps {
    /// A file rotates before it would pass this many bytes.
    pub file_bytes: u64,
    pub total_bytes: u64,
    pub files: Option<usize>,
    pub age: Duration,
}

impl Caps {
    pub(crate) const DEFAULT: Caps = Caps {
        file_bytes: 4 << 20,
        total_bytes: 20 << 20,
        files: Some(50),
        age: Duration::from_secs(14 * 86_400),
    };
    pub(crate) const VERBOSE: Caps = Caps {
        file_bytes: 16 << 20,
        total_bytes: 200 << 20,
        files: None,
        age: Duration::from_secs(3 * 86_400),
    };
}

/// The caps of default and verbose files.
#[derive(Clone, Copy, Debug)]
pub(crate) struct AllCaps {
    pub default: Caps,
    pub verbose: Caps,
}

impl AllCaps {
    pub(crate) const STANDARD: AllCaps = AllCaps {
        default: Caps::DEFAULT,
        verbose: Caps::VERBOSE,
    };
}

pub(crate) fn file_name(stamp: &str, pid: u32, seq: u32) -> String {
    match seq {
        0 => format!("lectern-{stamp}-{pid}.log"),
        _ => format!("lectern-{stamp}-{pid}.{seq}.log"),
    }
}

/// A file name of the form above.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Name {
    pub stamp: String,
    pub pid: u32,
    pub seq: u32,
}

pub(crate) fn parse_name(name: &str) -> Option<Name> {
    let rest = name.strip_prefix("lectern-")?.strip_suffix(".log")?;
    let (stamp, rest) = rest.split_once('-')?;
    time::parse_file_stamp(stamp)?;
    let (pid, seq) = match rest.split_once('.') {
        Some((pid, seq)) => (pid, seq.parse().ok().filter(|s| *s > 0)?),
        None => (rest, 0),
    };
    if !pid.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(Name {
        stamp: stamp.to_owned(),
        pid: pid.parse().ok()?,
        seq,
    })
}

#[derive(Clone, Debug)]
pub(crate) struct Listed {
    pub name: Name,
    pub path: PathBuf,
    pub bytes: u64,
    pub modified: Option<SystemTime>,
}

/// The log files in `dir`, oldest run first, files of a run in order.
pub(crate) fn list(dir: &Path) -> Vec<Listed> {
    let mut out: Vec<Listed> = fs::list(dir)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|e| {
            Some(Listed {
                name: parse_name(&e.name)?,
                path: e.path,
                bytes: e.bytes,
                modified: e.modified,
            })
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// This run, and the file of it that is open (never removed).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Keep<'a> {
    pub stamp: &'a str,
    pub pid: u32,
    pub open_seq: Option<u32>,
}

impl Keep<'_> {
    fn is_current_run(&self, name: &Name) -> bool {
        name.stamp == self.stamp && name.pid == self.pid
    }
}

/// Which files to remove, oldest first: older than the age cap, or while
/// the total or the count is over its cap. Only the file that is open stays
/// whatever the caps; this run's earlier files are removed like any other,
/// so the caps hold within one long run. A file's age is the time in its
/// name (the run's start); for this run's files, which all carry the same
/// start, it is their last write.
pub(crate) fn plan(files: &[Listed], keep: Keep<'_>, now: SystemTime, caps: Caps) -> Vec<usize> {
    let mut total: u64 = files.iter().map(|f| f.bytes).sum();
    let mut count = files.len();
    let mut remove = Vec::new();
    for (i, f) in files.iter().enumerate() {
        let current = keep.is_current_run(&f.name);
        if current && Some(f.name.seq) == keep.open_seq {
            continue;
        }
        let stamp = time::parse_file_stamp(&f.name.stamp).unwrap_or(now);
        let started = if current {
            f.modified.unwrap_or(now)
        } else {
            stamp
        };
        let old = now.duration_since(started).unwrap_or_default() > caps.age;
        let over = total > caps.total_bytes || caps.files.is_some_and(|max| count > max);
        if !old && !over {
            break;
        }
        remove.push(i);
        total -= f.bytes;
        count -= 1;
    }
    remove
}

/// What a cleanup did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Cleanup {
    pub removed: usize,
    pub removed_bytes: u64,
    pub failed: usize,
    /// The bytes of the log files left in the directory.
    pub remaining_bytes: u64,
}

pub(crate) fn cleanup(dir: &Path, keep: Keep<'_>, now: SystemTime, caps: Caps) -> Cleanup {
    let files = list(dir);
    let mut result = Cleanup {
        remaining_bytes: files.iter().map(|f| f.bytes).sum(),
        ..Cleanup::default()
    };
    for i in plan(&files, keep, now, caps) {
        let f = &files[i];
        match fs::remove_file(&f.path) {
            Ok(()) => {
                result.removed += 1;
                result.removed_bytes += f.bytes;
                result.remaining_bytes -= f.bytes;
            }
            Err(_) => result.failed += 1,
        }
    }
    result
}

/// The run before this one.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Previous {
    pub run: Option<u64>,
    pub ended: bool,
}

/// Reads the last file of the latest other run: whether it ends with the
/// end record, and the run ID from its header.
pub(crate) fn previous_run(dir: &Path, current: (&str, u32)) -> Option<Previous> {
    let files = list(dir);
    let last = files
        .iter()
        .rev()
        .find(|f| (f.name.stamp.as_str(), f.name.pid) != current)?;
    let (head, tail) = fs::head_and_tail(&last.path, 4096).ok()?;
    let tail = String::from_utf8_lossy(&tail);
    let ended = tail.lines().any(|l| l.contains("\"msg\":\"run end\""));
    Some(Previous {
        run: run_id(&String::from_utf8_lossy(&head)),
        ended,
    })
}

/// The `"run":"<16 hex>"` of a header.
pub(crate) fn run_id(text: &str) -> Option<u64> {
    let at = text.find("\"run\":\"")? + 7;
    u64::from_str_radix(text.get(at..at + 16)?, 16).ok()
}

pub(crate) fn verbose_dir(dir: &Path) -> PathBuf {
    dir.join("verbose")
}
