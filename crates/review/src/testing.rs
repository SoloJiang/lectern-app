//! Test and benchmark support (the `test-support` feature).
//!
//! The annotation log fixture of `review/log_load_16k` (step 1) and the
//! change chain fixture of `review/chain_3000_lines` (step 2);
//! `FakeSessions`, `FakeProjects` and `Driven` come with step 3.

use std::io;
use std::path::Path;
use std::sync::Arc;

use lectern_git::HashKind;
use lectern_services::changes::{ChangePath, FileMode, SideIdentity};
use lectern_services::project::BlobId;
use lectern_services::review::FileKey;
use lectern_services::session::Timestamp;

use crate::chain::{self, ChainInput, Detail, Edit, Op, Outcome, Side, StepInput};
use crate::log::{BaseRecord, LogFile, Record, SessionId};
use crate::seen::{SeenKey, StatusKind, TargetKey};

/// A file's change chain with its edits already loaded: `lines` lines,
/// `replaces` `Replace` steps spread over the file, five to a turn, and a
/// worktree that is the last version.
pub struct ChainFixture {
    path: ChangePath,
    base: Vec<u8>,
    worktree: Vec<u8>,
    steps: Vec<StepInput>,
}

/// What one [`ChainFixture::run`] gave.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ChainRun {
    pub applied: usize,
    pub aligned: bool,
    pub groups: usize,
}

/// The fixture of `review/chain_3000_lines` with `lines` = 3000 and
/// `replaces` = 40: each step replaces one line, about 70 lines apart.
pub fn chain_fixture(lines: usize, replaces: usize) -> ChainFixture {
    let line = |i: usize| format!("    let value_{i} = compute({i}, \"fixture line\");\n");
    let mut text: Vec<String> = (0..lines).map(line).collect();
    let base = text.concat().into_bytes();
    let path = ChangePath(Arc::from(&b"src/fixture.rs"[..]));
    let stride = (lines / replaces.max(1)).max(1);
    let steps = (0..replaces)
        .map(|k| {
            let i = (k * stride + stride / 2).min(lines - 1);
            let new = format!("    let value_{i} = compute_changed({i}, {k});\n");
            let old = std::mem::replace(&mut text[i], new.clone());
            StepInput {
                group: (k / 5) as u32,
                completed: true,
                edits: vec![Edit {
                    path: path.clone(),
                    op: Op::Replace {
                        old: old.into(),
                        new: new.into(),
                        all: false,
                    },
                }]
                .into(),
            }
        })
        .collect();
    ChainFixture {
        path,
        base,
        worktree: text.concat().into_bytes(),
        steps,
    }
}

impl ChainFixture {
    /// Builds the chain: every step applied, each followed by a
    /// `line_diff` and the version's OID, then the narrative steps' diffs
    /// and the alignment with the worktree. No load: the edits are here.
    pub fn run(&self) -> ChainRun {
        let chain = chain::build(&ChainInput {
            hash: HashKind::Sha1,
            start: &self.path,
            base: Side::Text(&self.base),
            worktree: Side::Text(&self.worktree),
            steps: &self.steps,
        });
        ChainRun {
            applied: chain
                .steps
                .iter()
                .filter(|s| s.outcome == Outcome::Applied)
                .count(),
            aligned: matches!(&chain.detail, Detail::Lines(l) if l.aligned),
            groups: chain.groups.len(),
        }
    }
}

/// What [`load_log`] read.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct LogLoad {
    /// Records in the index.
    pub records: usize,
    pub bad_lines: u32,
    /// The read was complete (compaction allowed).
    pub complete: bool,
}

/// Writes the annotation log of the repository `common_dir` into `data_dir`
/// in one append: `records` synthetic records, each for its own file, eight
/// in ten `seen` (a session target, both sides blobs), one `unseen`, one
/// session `base`. Returns the file's length: 16 000 records, as in
/// `review/log_load_16k`, are about 5.3 MiB.
pub fn write_log(data_dir: &Path, common_dir: &Path, records: usize) -> io::Result<u64> {
    let mut file = LogFile::new(data_dir, common_dir);
    let (_, report) = file.load(Timestamp(0));
    if let Some(error) = report.error {
        return Err(error);
    }
    let batch: Vec<Record> = (0..records).map(fixture_record).collect();
    file.append(&batch)?;
    Ok(std::fs::metadata(file.path())?.len())
}

/// Reads the log of `common_dir` in `data_dir` whole and builds its index,
/// as opening a project does.
pub fn load_log(data_dir: &Path, common_dir: &Path) -> LogLoad {
    let mut file = LogFile::new(data_dir, common_dir);
    let (index, report) = file.load(Timestamp(1_760_000_000_000));
    LogLoad {
        records: index.len(),
        bad_lines: report.bad_lines,
        complete: report.complete,
    }
}

fn fixture_record(i: usize) -> Record {
    let at = Timestamp(1_760_000_000_000 + i as i64 * 1000);
    let session = SessionId {
        source: Arc::from("session-claude-code"),
        id: Arc::from(format!("8f2c1a64-5b7e-4d3a-9c10-{:012x}", i / 10).as_str()),
    };
    let path = ChangePath(Arc::from(format!("src/c{}/m{i}.rs", i % 37).as_bytes()));
    let target = TargetKey::Session {
        source: session.source.clone(),
        id: session.id.clone(),
    };
    match i % 10 {
        9 => Record::Base(BaseRecord {
            at,
            session,
            commit: Some(oid(i, 0)),
            entry_at: Some(at.0 / 1000 - 60),
        }),
        8 => Record::Unseen {
            at,
            target,
            file: FileKey {
                old: Some(path.clone()),
                new: Some(path),
            },
        },
        _ => Record::Seen {
            at,
            key: SeenKey {
                target,
                old_path: Some(path.clone()),
                new_path: Some(path),
                old: Some((FileMode::File, SideIdentity::Blob(oid(i, 1)))),
                new: Some((FileMode::File, SideIdentity::Blob(oid(i, 2)))),
                status: StatusKind::Modified,
            },
        },
    }
}

/// A SHA-1 OID from `i` and `side`.
fn oid(i: usize, side: u8) -> BlobId {
    let mut bytes = [side; 20];
    bytes[..8].copy_from_slice(&(i as u64).to_be_bytes());
    BlobId::Sha1(bytes)
}
