use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use lectern_services::changes::{
    BaseSpec, ChangeDetail, ChangePath, ChangeStatus, FileChange, FileMode, FileSide, RawIdentity,
    SideIdentity, UnavailableKind,
};
use lectern_services::project::BlobId;
use lectern_services::review::{FileKey, SeenState};
use lectern_services::session::Timestamp;

use super::{BaseRecord, Index, Limits, LogFile, Op, Record, SessionId, file_name, flush};
use crate::seen::{SeenKey, TargetKey, mark_seen, mark_unseen, state};

const REPO: &str = "/work/repo/.git";
const NOW: Timestamp = Timestamp(1_760_000_000_000);
const DAY_MS: i64 = 24 * 60 * 60 * 1000;
const HEADER: &str = "{\"lectern\":\"review-log\",\"v\":1,\"repo\":\"/work/repo/.git\"}\n";

struct Fixture {
    _tmp: tempfile::TempDir,
    dir: PathBuf,
}

impl Fixture {
    fn new() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("review");
        Fixture { _tmp: tmp, dir }
    }

    fn file(&self) -> LogFile {
        LogFile::new(&self.dir, Path::new(REPO))
    }

    /// A log file that compacts above `compact_above`.
    fn small(&self, compact_above: u64) -> LogFile {
        let mut f = self.file();
        f.set_limits(Limits {
            compact_above,
            ..Limits::default()
        });
        f
    }

    fn path(&self) -> PathBuf {
        self.dir.join(file_name(Path::new(REPO)))
    }

    fn bytes(&self) -> Vec<u8> {
        fs::read(self.path()).unwrap()
    }

    fn text(&self) -> String {
        String::from_utf8(self.bytes()).unwrap()
    }

    fn ino(&self) -> u64 {
        fs::metadata(self.path()).unwrap().ino()
    }

    fn len(&self) -> u64 {
        fs::metadata(self.path()).unwrap().len()
    }

    /// Another writer: appends `bytes` directly.
    fn append_raw(&self, bytes: &[u8]) {
        let path = self.path();
        let mut f = OpenOptions::new().append(true).open(path).unwrap();
        f.write_all(bytes).unwrap();
    }

    /// Writes `records` with a log file of its own.
    fn write(&self, records: Vec<Record>) -> Index {
        write(&mut self.file(), records)
    }

    /// A fresh read by a new `LogFile`.
    fn reload(&self) -> (Index, super::ReadReport) {
        self.file().load(NOW)
    }
}

fn path(p: &[u8]) -> ChangePath {
    ChangePath(Arc::from(p))
}

fn blob(n: u8) -> SideIdentity {
    SideIdentity::Blob(BlobId::Sha1([n; 20]))
}

fn file_side(mode: FileMode, content: SideIdentity) -> Option<FileSide> {
    Some(FileSide { mode, content })
}

fn session(id: &str) -> TargetKey {
    TargetKey::Session {
        source: Arc::from("session-claude-code"),
        id: Arc::from(id),
    }
}

fn sid(id: &str) -> SessionId {
    SessionId {
        source: Arc::from("session-claude-code"),
        id: Arc::from(id),
    }
}

fn change(
    status: ChangeStatus,
    p: &[u8],
    old: Option<FileSide>,
    new: Option<FileSide>,
) -> FileChange {
    FileChange {
        status,
        path: path(p),
        old,
        new,
        detail: ChangeDetail::NoDiff,
        generated: false,
        untracked: false,
        staged: None,
    }
}

/// `p` modified from blob 1 to blob `new`.
fn modified(p: &[u8], new: u8) -> FileChange {
    let old = file_side(FileMode::File, blob(1));
    let new = file_side(FileMode::File, blob(new));
    change(ChangeStatus::Modified, p, old, new)
}

fn seen_at(target: &TargetKey, change: &FileChange, at: Timestamp) -> Record {
    mark_seen(SeenKey::new(target.clone(), change), at)
}

/// `p`, modified to blob `new`, marked seen in `target` now.
fn mark(target: &TargetKey, p: &[u8], new: u8) -> Record {
    seen_at(target, &modified(p, new), NOW)
}

fn base(id: &str, commit: Option<u8>, at: Timestamp) -> Record {
    Record::Base(BaseRecord {
        at,
        session: sid(id),
        commit: commit.map(|n| BlobId::Sha1([n; 20])),
        entry_at: Some(at.0 / 1000 - 30),
    })
}

/// The state of `p`, modified to blob `new`, in `target`.
fn state_of(index: &Index, target: &TargetKey, p: &[u8], new: u8) -> SeenState {
    state(index, &SeenKey::new(target.clone(), &modified(p, new)))
}

/// One record of every kind and shape, in a fixed order.
fn sample_records() -> Vec<Record> {
    let s = session("8f2c1a64-5b7e-4d3a-9c10-000000000001");
    let range = TargetKey::Range {
        worktree: Arc::from("worktrees/feature"),
        base: BaseSpec::MergeBase(Arc::from("refs/heads/main")),
    };
    let outside = TargetKey::Outside {
        worktree: Arc::from(""),
    };
    let moved = ChangeStatus::Renamed {
        from: path(b"src/old.rs"),
        similarity: 92,
    };
    let not_retained = SideIdentity::NotRetained(BlobId::Sha256([2; 32]));
    let renamed = change(
        moved,
        b"src/new.rs",
        file_side(FileMode::File, blob(1)),
        file_side(FileMode::Executable, not_retained),
    );
    let unhashed = SideIdentity::Unhashed {
        size: 1 << 40,
        mtime_ns: 1_760_000_000_123_456_789,
        inode: 42,
    };
    let odd_side = file_side(FileMode::File, unhashed);
    let odd = change(ChangeStatus::Added, b"data/\xff\xfe.bin", None, odd_side);
    let sha = SideIdentity::Filtered(RawIdentity::Sha256([0xab; 32]));
    let stat = SideIdentity::Filtered(RawIdentity::Stat {
        size: 7,
        mtime_ns: -1,
        inode: 9,
    });
    let filtered = change(
        ChangeStatus::TypeChanged,
        b"assets/logo.png",
        file_side(FileMode::Symlink, sha),
        file_side(FileMode::Gitlink, stat),
    );
    let unsupported = SideIdentity::Unavailable(UnavailableKind::UnsupportedType);
    let special = file_side(FileMode::Special, unsupported);
    let gone = change(ChangeStatus::Deleted, b"vendor/sock", special, None);
    let lib = FileKey {
        old: Some(path(b"src/lib.rs")),
        new: Some(path(b"src/lib.rs")),
    };
    let at = |n: i64| Timestamp(NOW.0 + n);
    vec![
        base("8f2c1a64-5b7e-4d3a-9c10-000000000001", Some(7), NOW),
        mark(&s, b"src/lib.rs", 2),
        seen_at(&s, &renamed, at(1)),
        seen_at(&range, &odd, at(2)),
        seen_at(&outside, &filtered, at(3)),
        seen_at(&outside, &gone, at(4)),
        mark_unseen(s.clone(), lib, at(5)),
        base("unborn", None, at(6)),
    ]
}

/// Loads, records `records` and flushes them.
fn write(f: &mut LogFile, records: Vec<Record>) -> Index {
    let (mut index, report) = f.load(NOW);
    assert!(report.error.is_none(), "{:?}", report.error);
    for r in records {
        index.record(r);
    }
    flush(f, &mut index, NOW).unwrap();
    index
}

fn compacted(f: &mut LogFile, index: &mut Index) -> bool {
    flush(f, index, NOW).unwrap().compacted
}

/// The records an index holds, in a comparable form.
fn records_of(index: &Index) -> Vec<String> {
    let marks = index.marks.values().flat_map(|m| m.values());
    let mut out: Vec<String> = marks
        .chain(index.bases.values())
        .map(|e| format!("{:?}", e.record))
        .collect();
    for (_, line) in &index.unknown {
        out.push(String::from_utf8_lossy(line).into_owned());
    }
    out.sort();
    out
}

#[test]
fn log_file_name_matches_the_cache_partition() {
    // printf 'repo\0/work/repo/.git' | sha256sum, first 16 bytes.
    let expected = "72cb51c320ab0e1063f8b855ff29eac3.jsonl";
    assert_eq!(file_name(Path::new(REPO)), expected);
}

#[test]
fn log_format() {
    let fx = Fixture::new();
    fx.write(sample_records());
    insta::assert_snapshot!(fx.text());
}

#[test]
fn log_roundtrip() {
    let fx = Fixture::new();
    let written = fx.write(sample_records());
    let (read, report) = fx.reload();
    assert!(report.complete);
    let found = (report.bad_lines, report.unknown, report.torn);
    assert_eq!(found, (0, 0, false));
    assert_eq!(records_of(&read), records_of(&written));
    // The unseen replaced the seen of src/lib.rs: 5 marks and 2 bases.
    assert_eq!(read.len(), 7);
    let s = session("8f2c1a64-5b7e-4d3a-9c10-000000000001");
    let lib = state_of(&read, &s, b"src/lib.rs", 2);
    assert_eq!(lib, SeenState::Unseen);
    assert_eq!(read.base(&sid("unborn")).unwrap().commit, None);
}

#[test]
fn log_non_utf8_paths_roundtrip() {
    let fx = Fixture::new();
    let s = session("a");
    let name = b"src/\xc3\x28/caf\xe9.rs";
    fx.write(vec![mark(&s, name, 2)]);
    // Written as a byte array.
    let array = "[115,114,99,47,195,40,47,99,97,102,233,46,114,115]";
    assert!(fx.text().contains(array));
    let (read, _) = fx.reload();
    assert_eq!(state_of(&read, &s, name, 2), SeenState::Seen);
}

#[test]
fn log_load_of_a_missing_file_is_complete_and_creates_nothing() {
    let fx = Fixture::new();
    let (index, report) = fx.file().load(NOW);
    assert!(report.complete);
    assert_eq!(index.len(), 0);
    assert!(!fx.dir.exists());
    // The first append creates the directory and writes the header.
    fx.write(vec![base("a", Some(1), NOW)]);
    assert!(fx.text().starts_with(HEADER));
}

#[test]
fn log_torn_last_line_dropped_and_cut_before_the_next_append() {
    let fx = Fixture::new();
    let s = session("a");
    fx.write(vec![mark(&s, b"a.rs", 2)]);
    let whole = fx.bytes();
    let torn = b"{\"t\":\"seen\",\"at\":12345,\"tar";
    fx.append_raw(torn);

    let mut f = fx.file();
    let (mut index, report) = f.load(NOW);
    assert!(report.torn);
    assert!(report.complete);
    assert_eq!(report.bad_lines, 0);
    assert_eq!(index.len(), 1);

    index.record(mark(&s, b"b.rs", 2));
    flush(&mut f, &mut index, NOW).unwrap();
    let after = fx.bytes();
    assert!(after.starts_with(&whole));
    assert!(!after.windows(torn.len()).any(|w| w == torn));
    let (read, report) = fx.reload();
    assert_eq!((report.bad_lines, report.torn), (0, false));
    assert_eq!(read.len(), 2);
}

#[test]
fn log_torn_header_only() {
    let fx = Fixture::new();
    fs::create_dir_all(&fx.dir).unwrap();
    fs::write(fx.path(), b"{\"lectern\":\"revi").unwrap();
    let mut f = fx.file();
    let (mut index, report) = f.load(NOW);
    assert!(report.torn && report.complete);
    index.record(base("a", Some(1), NOW));
    flush(&mut f, &mut index, NOW).unwrap();
    assert!(fx.text().starts_with(HEADER));
    let (read, report) = fx.reload();
    assert!(report.complete);
    assert_eq!((read.len(), report.bad_lines), (1, 0));
}

#[test]
fn log_bad_lines_skipped_and_counted() {
    let fx = Fixture::new();
    let s = session("a");
    fx.write(vec![mark(&s, b"a.rs", 2)]);
    fx.append_raw(
        b"not json\n\
          [1,2,3]\n\
          {\"at\":1}\n\
          {\"t\":7}\n\
          {\"t\":\"seen\",\"at\":1}\n\
          {\"t\":\"seen\",\"at\":1,\"target\":{\"outside\":{\"worktree\":\"\"}},\
           \"key\":{\"old_path\":null,\"new_path\":null,\"old\":null,\"new\":null,\
           \"status\":\"added\"}}\n\
          {\"t\":\"base\",\"at\":1,\"session\":{\"source\":\"s\",\"id\":\"i\"},\
           \"commit\":\"xyz\",\"entry_at\":null}\n\
          {\"lectern\":\"review-log\",\"v\":1,\"repo\":\"/work/repo/.git\"}\n\
          \n",
    );
    fx.write(vec![mark(&s, b"b.rs", 2)]);
    let (read, report) = fx.reload();
    assert_eq!(report.bad_lines, 8);
    assert!(report.complete);
    assert_eq!(read.len(), 2);
    assert_eq!(state_of(&read, &s, b"b.rs", 2), SeenState::Seen);
}

const NOTE: &[u8] = b"{\"t\":\"note\",\"at\":1760000000000,\
    \"anchor\":{\"side\":\"new\",\"line\":3},\"text\":\"why \\u00e9?\"}\n";
const NOTES_SENT: &[u8] = b"{\"t\":\"notes_sent\",\"at\":1760000000001,\"to\":[1,2]}\n";

#[test]
fn log_unknown_records_survive_compaction() {
    let fx = Fixture::new();
    let s = session("a");
    fx.write(vec![mark(&s, b"a.rs", 2)]);
    fx.append_raw(NOTE);
    fx.write(vec![mark(&s, b"b.rs", 2)]);
    fx.append_raw(NOTES_SENT);

    let mut f = fx.small(1);
    let (mut index, report) = f.load(NOW);
    assert_eq!((report.unknown, report.bad_lines), (2, 0));
    // Rewrite a.rs's mark many times, then compact.
    for n in 3..40 {
        index.record(mark(&s, b"a.rs", n));
    }
    let ino = fx.ino();
    let flushed = flush(&mut f, &mut index, NOW).unwrap();
    assert!(flushed.compacted, "{flushed:?}");
    assert_ne!(fx.ino(), ino);
    insta::assert_snapshot!(fx.text());
    let (read, report) = fx.reload();
    assert_eq!((report.unknown, report.bad_lines), (2, 0));
    assert_eq!(records_of(&read), records_of(&index));
}

#[test]
fn log_compacts_above_4_mib() {
    let fx = Fixture::new();
    let id = "8f2c1a64-5b7e-4d3a-9c10-000000000001";
    let s = session(id);
    let mut f = fx.file();
    let (mut index, _) = f.load(NOW);
    index.record(base(id, Some(1), NOW));
    // 100 files marked over and over: about 300 bytes a record.
    let name = |n: u32| format!("crates/review/src/module_{}.rs", n % 100);
    let mut n = 0u32;
    let mut done = false;
    while !done {
        for _ in 0..512 {
            index.record(mark(&s, name(n).as_bytes(), (n % 251) as u8));
            n += 1;
        }
        let before = if fx.path().exists() { fx.len() } else { 0 };
        done = compacted(&mut f, &mut index);
        if done {
            assert!(before > 3 << 20, "compacted at {before}");
        } else {
            assert!(fx.len() <= super::COMPACT_ABOVE, "{}", fx.len());
        }
    }
    assert!(fx.len() < 64 << 10, "{}", fx.len());
    let (read, report) = fx.reload();
    assert!(report.complete);
    assert_eq!(read.len(), 101);
    assert_eq!(records_of(&read), records_of(&index));
    let last = n - 1;
    let state = state_of(&read, &s, name(last).as_bytes(), (last % 251) as u8);
    assert_eq!(state, SeenState::Seen);
}

fn fail_on(op: Op) -> impl Fn(Op) -> io::Result<()> + Send + Sync + 'static {
    move |o| {
        if o == op {
            Err(io::Error::other(format!("injected {op:?}")))
        } else {
            Ok(())
        }
    }
}

#[test]
fn log_failed_compaction_leaves_the_file() {
    for op in [Op::CompactWrite, Op::CompactRename] {
        let fx = Fixture::new();
        let s = session("a");
        fx.write(vec![mark(&s, b"a.rs", 2)]);
        let mut f = fx.small(1);
        f.set_faults(fail_on(op));
        let (mut index, _) = f.load(NOW);
        index.record(mark(&s, b"a.rs", 3));
        let flushed = flush(&mut f, &mut index, NOW).unwrap();
        assert!(!flushed.compacted);
        assert!(flushed.compaction_error.is_some(), "{op:?}");
        // The append went through; the log is that, untouched.
        let (read, report) = fx.reload();
        assert!(report.complete);
        assert_eq!(state_of(&read, &s, b"a.rs", 3), SeenState::Seen);
        assert_eq!(fx.text().lines().count(), 3, "{op:?}");
        let names: Vec<_> = fs::read_dir(&fx.dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names.len(), 1, "{names:?}");
        // The next write compacts.
        f.set_faults(|_| Ok(()));
        index.record(mark(&s, b"a.rs", 4));
        assert!(compacted(&mut f, &mut index));
        assert_eq!(fx.text().lines().count(), 2);
    }
}

#[test]
fn log_no_compaction_after_failed_read() {
    let fx = Fixture::new();
    let s = session("a");
    let old = |k: u8| format!("old_{k}.rs");
    let mut originals: Vec<Record> = (0..20).map(|k| mark(&s, old(k).as_bytes(), k)).collect();
    originals.push(base("a", Some(5), NOW));
    fx.write(originals);
    let original = fx.bytes();
    let ino = fx.ino();

    let mut f = fx.small(8 << 10);
    f.set_faults(fail_on(Op::Read));
    let (mut index, report) = f.load(NOW);
    assert!(report.error.is_some());
    assert!(!report.complete);
    // Files already seen read as unseen; marking goes on.
    assert_eq!(state_of(&index, &s, b"old_3.rs", 3), SeenState::Unseen);
    let mut n = 0u32;
    while fx.len() < 32 << 10 {
        for _ in 0..16 {
            index.record(mark(&s, b"new.rs", (n % 200) as u8));
            n += 1;
        }
        let flushed = flush(&mut f, &mut index, NOW).unwrap();
        assert!(!flushed.compacted);
        assert!(flushed.compaction_error.is_none());
    }
    // Appended only: the same file, the original bytes still in front.
    assert_eq!(fx.ino(), ino);
    assert!(fx.bytes().starts_with(&original));

    let (read, report) = fx.reload();
    assert!(report.complete);
    assert_eq!(report.bad_lines, 0);
    for k in 0..20 {
        let state = state_of(&read, &s, old(k).as_bytes(), k);
        assert_eq!(state, SeenState::Seen, "{}", old(k));
    }
    let last = ((n - 1) % 200) as u8;
    assert_eq!(state_of(&read, &s, b"new.rs", last), SeenState::Seen);
    assert!(read.base(&sid("a")).is_some());

    // That complete read allows compaction again.
    let mut f = fx.small(8 << 10);
    let (mut index, _) = f.load(NOW);
    index.record(mark(&s, b"new.rs", 250));
    assert!(compacted(&mut f, &mut index));
    assert_eq!(fx.reload().0.len(), 22);
}

#[test]
fn log_compaction_needs_the_index_to_be_the_file() {
    let s = session("a");
    let mut other = Vec::new();
    super::wire::encode(&mark(&session("b"), b"z.rs", 2), &mut other);

    // Another writer appends after the read: append only.
    let fx = Fixture::new();
    fx.write(vec![mark(&s, b"a.rs", 2)]);
    let mut f = fx.small(1);
    let (mut index, _) = f.load(NOW);
    fx.append_raw(&other);
    index.record(mark(&s, b"a.rs", 3));
    assert!(!compacted(&mut f, &mut index));
    index.record(mark(&s, b"a.rs", 4));
    assert!(!compacted(&mut f, &mut index));
    // The other writer's mark is kept.
    assert_eq!(fx.reload().0.len(), 2);

    // Another writer between this process's append and the compaction:
    // the identity check right before rewriting sees the longer file.
    let fx = Fixture::new();
    fx.write(vec![mark(&s, b"a.rs", 2)]);
    let mut f = fx.small(1);
    let started = Arc::new(AtomicBool::new(false));
    let flag = started.clone();
    f.set_faults(move |op| {
        if op == Op::CompactWrite {
            flag.store(true, Ordering::SeqCst);
        }
        Ok(())
    });
    let (mut index, _) = f.load(NOW);
    index.record(mark(&s, b"a.rs", 3));
    f.append(&index.take_pending()).unwrap();
    assert!(f.should_compact());
    fx.append_raw(&other);
    let before = fx.bytes();
    assert!(!f.compact(&mut index, NOW).unwrap());
    assert!(!started.load(Ordering::SeqCst));
    assert_eq!(fx.bytes(), before);
    // And it stays append only.
    index.record(mark(&s, b"a.rs", 4));
    assert!(!compacted(&mut f, &mut index));
    assert_eq!(fx.reload().0.len(), 2);

    // A read limited to the last part of the file: append only.
    let fx = Fixture::new();
    let name = |k: u8| format!("f{k}.rs");
    fx.write((0..50).map(|k| mark(&s, name(k).as_bytes(), 2)).collect());
    let mut f = fx.file();
    f.set_limits(Limits {
        read_max: 4 << 10,
        compact_above: 1,
    });
    let (mut index, report) = f.load(NOW);
    assert!(!report.complete);
    assert!(index.len() > 0 && index.len() < 50, "{}", index.len());
    let ino = fx.ino();
    index.record(mark(&s, b"f0.rs", 3));
    assert!(!compacted(&mut f, &mut index));
    assert_eq!(fx.ino(), ino);
    let (read, report) = fx.reload();
    assert!(report.complete);
    assert_eq!(read.len(), 50);

    // A newer format version: read, append only.
    let fx = Fixture::new();
    fs::create_dir_all(&fx.dir).unwrap();
    let v2 = "{\"lectern\":\"review-log\",\"v\":2,\"repo\":\"/work/repo/.git\"}\n";
    fs::write(fx.path(), v2).unwrap();
    let mut f = fx.small(1);
    let (mut index, report) = f.load(NOW);
    assert!(!report.complete);
    index.record(mark(&s, b"a.rs", 2));
    assert!(!compacted(&mut f, &mut index));
    assert!(fx.text().starts_with(v2));

    // A complete read followed by own appends: compacts.
    let fx = Fixture::new();
    fx.write(vec![mark(&s, b"a.rs", 2)]);
    let mut f = fx.small(1);
    let (mut index, _) = f.load(NOW);
    index.record(mark(&s, b"a.rs", 3));
    assert!(compacted(&mut f, &mut index));
}

#[test]
fn log_unknown_tail_after_failed_read_gets_a_newline() {
    let fx = Fixture::new();
    let s = session("a");
    fx.write(vec![mark(&s, b"a.rs", 2)]);
    fx.append_raw(b"{\"t\":\"seen\",\"at\":1");
    let mut f = fx.file();
    f.set_faults(fail_on(Op::Read));
    let (mut index, _) = f.load(NOW);
    index.record(mark(&s, b"b.rs", 2));
    flush(&mut f, &mut index, NOW).unwrap();
    let (read, report) = fx.reload();
    // The fragment is one bad line; both marks are there.
    assert_eq!(report.bad_lines, 1);
    assert_eq!(read.len(), 2);
}

#[test]
fn log_failed_append_keeps_the_queue() {
    let fx = Fixture::new();
    let s = session("a");
    let mut f = fx.file();
    f.set_faults(fail_on(Op::Append));
    let (mut index, _) = f.load(NOW);
    index.record(mark(&s, b"a.rs", 2));
    assert!(flush(&mut f, &mut index, NOW).is_err());
    index.record(mark(&s, b"b.rs", 2));
    assert_eq!(index.pending().len(), 2);
    // In memory the marks hold meanwhile.
    assert_eq!(state_of(&index, &s, b"a.rs", 2), SeenState::Seen);
    f.set_faults(|_| Ok(()));
    let flushed = flush(&mut f, &mut index, NOW).unwrap();
    assert_eq!(flushed.appended, 2);
    assert!(index.pending().is_empty());
    let text = fx.text();
    let (a, b) = (text.find("a.rs").unwrap(), text.find("b.rs").unwrap());
    assert!(a < b, "the queue keeps its order");
    assert_eq!(fx.reload().0.len(), 2);
}

#[test]
fn log_foreign_header_is_renamed_aside() {
    let fx = Fixture::new();
    fs::create_dir_all(&fx.dir).unwrap();
    let foreign = b"{\"lectern\":\"review-log\",\"v\":1,\"repo\":\"/other/repo/.git\"}\n\
        {\"t\":\"base\",\"at\":1,\"session\":{\"source\":\"s\",\"id\":\"i\"},\
        \"commit\":null,\"entry_at\":null}\n";
    fs::write(fx.path(), foreign).unwrap();
    let mut f = fx.file();
    let (mut index, report) = f.load(NOW);
    let aside = report.foreign.unwrap();
    assert_eq!(index.len(), 0);
    assert!(report.complete);
    let name = format!("{}.foreign-{}", file_name(Path::new(REPO)), NOW.0);
    assert_eq!(aside.file_name().unwrap().to_str().unwrap(), name);
    assert_eq!(fs::read(&aside).unwrap(), foreign);
    assert!(!fx.path().exists());
    index.record(base("a", Some(1), NOW));
    flush(&mut f, &mut index, NOW).unwrap();
    assert!(fx.text().starts_with(HEADER));
    assert_eq!(fx.reload().0.len(), 1);
}

#[test]
fn log_compaction_drops_marks_older_than_a_year() {
    let fx = Fixture::new();
    let s = session("a");
    let old = Timestamp(NOW.0 - 366 * DAY_MS);
    let recent = Timestamp(NOW.0 - 364 * DAY_MS);
    let gone = FileKey {
        old: None,
        new: Some(path(b"gone.rs")),
    };
    let mut f = fx.small(1);
    let (mut index, _) = f.load(NOW);
    index.record(seen_at(&s, &modified(b"old.rs", 2), old));
    index.record(mark_unseen(s.clone(), gone, old));
    index.record(seen_at(&s, &modified(b"recent.rs", 2), recent));
    index.record(base("a", Some(1), old));
    assert!(compacted(&mut f, &mut index));
    // Dropped from the file and the index alike; bases stay.
    assert_eq!(index.len(), 2);
    let (read, _) = fx.reload();
    assert_eq!(records_of(&read), records_of(&index));
    assert_eq!(state_of(&read, &s, b"old.rs", 2), SeenState::Unseen);
    assert_eq!(state_of(&read, &s, b"recent.rs", 2), SeenState::Seen);
}

#[test]
fn log_first_base_wins() {
    let fx = Fixture::new();
    fx.write(vec![base("a", Some(1), NOW), base("a", Some(2), NOW)]);
    let (read, _) = fx.reload();
    let first = Some(BlobId::Sha1([1; 20]));
    assert_eq!(read.base(&sid("a")).unwrap().commit, first);
}

#[test]
fn log_compaction_waits_for_twice_the_live_size() {
    let fx = Fixture::new();
    let s = session("a");
    let name = |k: u32| format!("f{k}.rs");
    // About 6 KiB of live marks, three times the limit.
    fx.write((0..20).map(|k| mark(&s, name(k).as_bytes(), 2)).collect());
    let mut f = fx.small(2 << 10);
    let (mut index, _) = f.load(NOW);
    index.record(mark(&s, b"f0.rs", 3));
    assert!(compacted(&mut f, &mut index));
    let live = fx.len();
    assert!(live > 2 << 10, "{live}");
    // Over the limit, but under twice the live size: append only.
    let mut n = 4u32;
    while fx.len() + 400 < 2 * live {
        index.record(mark(&s, b"f0.rs", n as u8));
        assert!(!compacted(&mut f, &mut index), "at {}", fx.len());
        n += 1;
    }
    // Past twice the live size: compacts again, back to the live size.
    while !compacted(&mut f, &mut index) {
        assert!(fx.len() <= 2 * live, "{}", fx.len());
        index.record(mark(&s, b"f0.rs", n as u8));
        n += 1;
    }
    assert!(fx.len().abs_diff(live) < 64, "{} vs {live}", fx.len());
}

#[test]
fn log_failed_directory_sync_goes_append_only() {
    let fx = Fixture::new();
    let s = session("a");
    fx.write(vec![mark(&s, b"a.rs", 2)]);
    let mut f = fx.small(1);
    f.set_faults(fail_on(Op::CompactSyncDir));
    let (mut index, _) = f.load(NOW);
    index.record(mark(&s, b"a.rs", 3));
    // The rename happened; the directory sync did not.
    assert!(compacted(&mut f, &mut index));
    f.set_faults(|_| Ok(()));
    let ino = fx.ino();
    for n in 4..8 {
        index.record(mark(&s, b"a.rs", n));
        assert!(!compacted(&mut f, &mut index));
    }
    assert_eq!(fx.ino(), ino);
    let (read, report) = fx.reload();
    assert!(report.complete);
    assert_eq!(state_of(&read, &s, b"a.rs", 7), SeenState::Seen);
}

#[test]
fn log_rename_retires_the_old_path_on_replay() {
    let fx = Fixture::new();
    let s = session("a");
    let moved = ChangeStatus::Renamed {
        from: path(b"a.rs"),
        similarity: 90,
    };
    let side = |n: u8| file_side(FileMode::File, blob(n));
    let rename = change(moved, b"b.rs", side(2), side(3));
    let written = fx.write(vec![mark(&s, b"a.rs", 2), seen_at(&s, &rename, NOW)]);
    assert_eq!(written.len(), 1);
    let (read, _) = fx.reload();
    assert_eq!(records_of(&read), records_of(&written));
    let created = change(ChangeStatus::Added, b"a.rs", None, side(9));
    let key = SeenKey::new(s.clone(), &created);
    assert_eq!(state(&read, &key), SeenState::Unseen);
}

#[test]
fn log_read_shares_repeated_strings() {
    let fx = Fixture::new();
    let s = session("a");
    fx.write(vec![mark(&s, b"a.rs", 2), mark(&s, b"b.rs", 2)]);
    let (read, _) = fx.reload();
    let key = |p: &[u8]| match read.mark(&s, &path(p)) {
        Some(Record::Seen { key, .. }) => key.clone(),
        other => panic!("{other:?}"),
    };
    let (a, b) = (key(b"a.rs"), key(b"b.rs"));
    let id = |k: &SeenKey| match &k.target {
        TargetKey::Session { id, .. } => id.clone(),
        other => panic!("{other:?}"),
    };
    assert!(Arc::ptr_eq(&id(&a), &id(&b)));
    let (old, new) = (a.old_path.unwrap(), a.new_path.unwrap());
    assert!(Arc::ptr_eq(&old.0, &new.0));
}

#[test]
fn log_bench_fixture_reads_back_whole() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = Path::new(REPO);
    let len = crate::testing::write_log(tmp.path(), repo, 16_000).unwrap();
    assert!((3 << 20..6 << 20).contains(&len), "{len}");
    let load = crate::testing::load_log(tmp.path(), repo);
    assert_eq!(load.bad_lines, 0);
    assert!(load.complete);
    assert_eq!(load.records, 16_000);
}
