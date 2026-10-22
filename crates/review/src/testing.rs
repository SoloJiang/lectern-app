//! Test and benchmark support (the `test-support` feature).
//!
//! Step 1 has only the annotation log fixture of `review/log_load_16k`;
//! `FakeSessions`, `FakeProjects` and `Driven` come with step 3.

use std::io;
use std::path::Path;
use std::sync::Arc;

use lectern_services::project::BlobId;
use lectern_services::session::Timestamp;

use crate::log::{BaseRecord, LogFile, Record, SessionId};
use crate::seen::{SeenKey, StatusKind, TargetKey};
use crate::types::changes::{ChangePath, FileMode, SideIdentity};
use crate::types::review::FileKey;

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
