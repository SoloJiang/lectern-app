//! Test and benchmark support (the `test-support` feature).
//!
//! The annotation log fixture of `review/log_load_16k` (step 1), the
//! change chain fixture of `review/chain_3000_lines` (step 2), and for the
//! review engine (step 3) `FakeSessions`, `FakeProjects`, `Driven`, a HEAD
//! reflog writer and the fixture of the `review/open_session_*` benches.
//! For review notes (step 6): the log of `review/resident_notes_bytes`
//! ([`write_notes_log`], [`load_notes_log`]) and the notes seeded for
//! `review/note_add` ([`MarkSeenFixture::seed_notes`]).

mod driven;
mod fake;
mod fixture;
mod host;

pub use driven::{Driven, ReflogLine, write_reflog};
pub use fake::{Calls, FakeProjects, FakeSessions};
pub use fixture::{BriefFixture, CutoffRun, MarkSeenFixture, SessionFixture};
pub use host::{TestHost, host};

use std::io;
use std::path::Path;
use std::sync::Arc;

use lectern_git::HashKind;
use lectern_services::changes::{ChangePath, FileMode, SideIdentity};
use lectern_services::project::BlobId;
use lectern_services::review::{BatchId, DiffSide, Excerpt, FileKey, NoteAnchor, NoteId, SentVia};
use lectern_services::session::Timestamp;

use crate::chain::{self, ChainInput, Detail, Edit, Op, Outcome, Side, StepInput};
use crate::log::{BaseRecord, Index, LogFile, NoteRecord, Record, SentRecord, SessionId};
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
            end: &self.path,
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

/// The notes of the shape of `review/resident_notes_bytes`: `pending` notes
/// not sent and `sent` notes sent in batches of 40, each with 200 bytes of
/// text and a five-line excerpt, spread over `path(i)` files. Written at
/// an hour before the time `load_log` reads at, so that none has expired.
pub(crate) fn note_records(
    target: &TargetKey,
    pending: usize,
    sent: usize,
    path: impl Fn(usize) -> ChangePath,
) -> Vec<Record> {
    // An hour before the clocks of the benches and `load_log`: the notes'
    // IDs are older than any a command makes.
    let start = 1_760_000_000_000i64 - 3_600_000;
    let mut out = Vec::new();
    let mut ids = Vec::new();
    for i in 0..pending + sent {
        let at = Timestamp(start + i as i64 * 1000);
        let id = NoteId(((start as u64) << 10) + i as u64);
        let p = path(i);
        let text: String = format!("Note {i}: why is this here, and does it still hold? ")
            .chars()
            .cycle()
            .take(200)
            .collect();
        let lines: Vec<Arc<str>> = (0..5)
            .map(|k| {
                Arc::from(format!("    let value_{} = compute({i}, \"fixture\");", i + k).as_str())
            })
            .collect();
        out.push(Record::Note(NoteRecord {
            at,
            target: target.clone(),
            id,
            anchor: NoteAnchor {
                file: FileKey {
                    old: None,
                    new: Some(p),
                },
                side: DiffSide::New,
                line: 10 + i as u32,
                oid: oid(i, 3),
            },
            excerpt: Some(Excerpt {
                first: 8 + i as u32,
                lines: lines.into(),
            }),
            text: Arc::from(text.as_str()),
            created: at,
        }));
        // The first `sent` notes are the sent ones; a batch each 40.
        if i < sent {
            ids.push(id);
            if ids.len() == 40 || i + 1 == sent {
                out.push(Record::NotesSent(SentRecord {
                    at,
                    target: target.clone(),
                    batch: BatchId(((start as u64) << 10) + 1_000_000 + i as u64),
                    notes: std::mem::take(&mut ids),
                    via: SentVia::Clipboard,
                }));
            }
        }
    }
    out
}

/// What [`load_notes_log`] holds: the index of a log with review notes.
pub struct NotesLog {
    index: Index,
}

impl NotesLog {
    /// The notes and batches in the index.
    pub fn records(&self) -> usize {
        self.index.len()
    }
}

/// Writes the annotation log of `common_dir` in `data_dir` with `pending`
/// notes and `sent` notes sent (`review/resident_notes_bytes`: 40 and 240),
/// all of one session's review.
pub fn write_notes_log(
    data_dir: &Path,
    common_dir: &Path,
    pending: usize,
    sent: usize,
) -> io::Result<()> {
    let target = TargetKey::Session {
        source: Arc::from("session-claude-code"),
        id: Arc::from("8f2c1a64-5b7e-4d3a-9c10-000000000001"),
    };
    let path = |i: usize| {
        ChangePath(Arc::from(
            format!("src/c{}/m{}.rs", i % 7, i % 40).as_bytes(),
        ))
    };
    let mut file = LogFile::new(data_dir, common_dir);
    let (_, report) = file.load(Timestamp(0));
    if let Some(error) = report.error {
        return Err(error);
    }
    file.append(&note_records(&target, pending, sent, path))
}

/// Reads the log of `common_dir` in `data_dir` and keeps its index, as an
/// open project does while any review of it is open or any overview watched.
pub fn load_notes_log(data_dir: &Path, common_dir: &Path) -> NotesLog {
    let mut file = LogFile::new(data_dir, common_dir);
    let (index, _) = file.load(Timestamp(1_760_000_000_000));
    NotesLog { index }
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
