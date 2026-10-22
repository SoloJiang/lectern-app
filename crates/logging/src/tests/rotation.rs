//! 「测试」滚动与清理.

use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::{Duration, SystemTime};

use super::{Fixture, base_time, config};
use crate::callsite::LECTERN_LIMIT;
use crate::engine::time::file_stamp;
use crate::sink::{AllCaps, Caps, Keep, Listed, Name, cleanup, file_name, list, parse_name, plan};

const DAY: Duration = Duration::from_secs(86_400);

/// A log file of `bytes` bytes for the run that started at `started`.
fn make(dir: &Path, started: SystemTime, pid: u32, seq: u32, bytes: usize) {
    std::fs::write(
        dir.join(file_name(&file_stamp(started), pid, seq)),
        vec![b'x'; bytes],
    )
    .expect("write");
}

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .expect("dir")
        .flatten()
        .filter(|e| e.path().is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn caps(total_bytes: u64, files: Option<usize>, age_days: u64) -> Caps {
    Caps {
        file_bytes: 4 << 20,
        total_bytes,
        files,
        age: DAY * age_days as u32,
    }
}

const CURRENT: Keep<'static> = Keep {
    stamp: "20261022T032701Z",
    pid: 1,
    open_seq: Some(0),
};

#[test]
fn names_round_trip() {
    let name = file_name("20261022T032701Z", 4821, 0);
    assert_eq!(name, "lectern-20261022T032701Z-4821.log");
    assert_eq!(parse_name(&name).map(|n| (n.pid, n.seq)), Some((4821, 0)));
    assert_eq!(
        parse_name("lectern-20261022T032701Z-4821.1.log").map(|n| n.seq),
        Some(1)
    );
    for bad in [
        "lectern-20261022T032701Z-48x1.log",
        "lectern-2026-1.log",
        "other.log",
        "lectern-20261022T032701Z-1.0.log",
    ] {
        assert_eq!(parse_name(bad), None, "{bad}");
    }
}

#[test]
fn rotates_at_the_file_cap_with_a_header() {
    let fx = Fixture::new();
    let small = AllCaps {
        default: Caps {
            file_bytes: 4096,
            ..Caps::DEFAULT
        },
        verbose: Caps::VERBOSE,
    };
    let logging = fx.install_with(None, None, small).start(config());
    LECTERN_LIMIT.store(u32::MAX, Ordering::Relaxed);
    for i in 0..200_u32 {
        crate::info!("filling the file"; i = i);
    }
    assert!(logging.flush(Duration::from_secs(5)));
    let files = list(fx.path());
    assert!(files.len() > 3, "{files:?}");
    for (seq, f) in files.iter().enumerate() {
        assert_eq!(f.name.seq as usize, seq);
        assert!(f.bytes <= 4096, "{} is {} bytes", f.path.display(), f.bytes);
        let text = std::fs::read_to_string(&f.path).expect("read");
        let header: serde_json::Value =
            serde_json::from_str(text.lines().next().expect("a line")).expect("JSON");
        assert_eq!(header["msg"], "file");
        assert_eq!(header["seq"], seq as u64);
    }
}

#[test]
fn total_cap_removes_oldest_first() {
    let dir = tempfile::tempdir().expect("dir");
    for day in 1..=5 {
        make(dir.path(), base_time() - DAY * day, 100 + day, 0, 100);
    }
    let result = cleanup(dir.path(), CURRENT, base_time(), caps(300, None, 14));
    assert_eq!(
        (result.removed, result.removed_bytes, result.remaining_bytes),
        (2, 200, 300)
    );
    let kept = names(dir.path());
    assert_eq!(kept.len(), 3);
    assert!(
        kept.iter()
            .all(|n| !n.contains("-105.") && !n.contains("-104.")),
        "{kept:?}"
    );
}

#[test]
fn age_cap_removes_old_runs() {
    let dir = tempfile::tempdir().expect("dir");
    make(dir.path(), base_time() - DAY * 20, 1, 0, 10);
    make(dir.path(), base_time() - DAY * 20, 1, 1, 10);
    make(dir.path(), base_time() - DAY * 15, 2, 0, 10);
    make(dir.path(), base_time() - DAY, 3, 0, 10);
    let result = cleanup(
        dir.path(),
        CURRENT,
        base_time(),
        caps(1 << 20, Some(50), 14),
    );
    assert_eq!(result.removed, 3);
    assert_eq!(names(dir.path()).len(), 1);
}

#[test]
fn file_count_cap() {
    let dir = tempfile::tempdir().expect("dir");
    for i in 0..60 {
        make(
            dir.path(),
            base_time() - Duration::from_secs(3600 * (60 - i)),
            10,
            0,
            1,
        );
    }
    let result = cleanup(dir.path(), CURRENT, base_time(), Caps::DEFAULT);
    assert_eq!(result.removed, 10);
    assert_eq!(names(dir.path()).len(), 50);
}

#[test]
fn verbose_files_have_their_own_caps() {
    assert_eq!(
        (
            Caps::VERBOSE.file_bytes,
            Caps::VERBOSE.total_bytes,
            Caps::VERBOSE.files
        ),
        (16 << 20, 200 << 20, None)
    );
    assert_eq!(Caps::VERBOSE.age, DAY * 3);
    assert_eq!(
        (
            Caps::DEFAULT.file_bytes,
            Caps::DEFAULT.total_bytes,
            Caps::DEFAULT.files
        ),
        (4 << 20, 20 << 20, Some(50))
    );
    assert_eq!(Caps::DEFAULT.age, DAY * 14);
    let set = |dir: &Path| {
        make(dir, base_time() - DAY * 4, 1, 0, 10);
        for i in 0..60 {
            make(
                dir,
                base_time() - Duration::from_secs(60 * (60 - i)),
                2 + i as u32,
                0,
                1,
            );
        }
    };
    let default = tempfile::tempdir().expect("dir");
    set(default.path());
    let verbose = tempfile::tempdir().expect("dir");
    set(verbose.path());
    // Default: four days are within 14, but 61 files are over 50.
    assert_eq!(
        cleanup(default.path(), CURRENT, base_time(), Caps::DEFAULT).removed,
        11
    );
    // Verbose: no count cap, but four days are over 3.
    assert_eq!(
        cleanup(verbose.path(), CURRENT, base_time(), Caps::VERBOSE).removed,
        1
    );
}

#[test]
fn open_file_is_never_removed() {
    let dir = tempfile::tempdir().expect("dir");
    let started = base_time() - DAY * 30;
    for seq in 0..3 {
        make(dir.path(), started, 7, seq, 500);
    }
    make(dir.path(), base_time() - DAY * 31, 8, 0, 500);
    let current = file_stamp(started);
    let keep = Keep {
        stamp: &current,
        pid: 7,
        open_seq: Some(2),
    };
    // Over every cap: the other run and this run's earlier files go, the
    // open file stays.
    let result = cleanup(dir.path(), keep, base_time(), caps(100, Some(1), 14));
    assert_eq!(result.removed, 3);
    assert_eq!(names(dir.path()), [file_name(&current, 7, 2)]);
}

#[test]
fn current_run_files_age_by_their_last_write() {
    let stamp = file_stamp(base_time() - DAY * 30);
    let file = |seq: u32, written: SystemTime| Listed {
        name: Name {
            stamp: stamp.clone(),
            pid: 7,
            seq,
        },
        path: file_name(&stamp, 7, seq).into(),
        bytes: 10,
        modified: Some(written),
    };
    let files = [
        file(0, base_time() - DAY * 20),
        file(1, base_time() - DAY),
        file(2, base_time()),
    ];
    let keep = Keep {
        stamp: &stamp,
        pid: 7,
        open_seq: Some(2),
    };
    // The run started 30 days ago; only the file last written 20 days ago
    // is past the 14-day cap.
    assert_eq!(plan(&files, keep, base_time(), Caps::DEFAULT), [0]);
}

#[test]
fn rotation_applies_the_caps_within_a_run() {
    let fx = Fixture::new();
    let small = AllCaps {
        default: Caps {
            file_bytes: 2048,
            total_bytes: 8192,
            ..Caps::DEFAULT
        },
        verbose: Caps::VERBOSE,
    };
    let logging = fx.install_with(None, None, small).start(config());
    LECTERN_LIMIT.store(u32::MAX, Ordering::Relaxed);
    for i in 0..400_u32 {
        crate::info!("filling the run"; i = i);
    }
    assert!(logging.flush(Duration::from_secs(5)));
    let files = list(fx.path());
    let total: u64 = files.iter().map(|f| f.bytes).sum();
    assert!(total <= 8192 + 2048, "{total} bytes in {files:?}");
    assert!(files.len() < 8, "{files:?}");
    assert!(
        files.iter().all(|f| f.name.seq > 0),
        "the first file was removed: {files:?}"
    );
    let open = files.last().expect("the open file");
    assert!(open.name.seq > 10, "{files:?}");
}

#[test]
fn unrelated_files_are_untouched() {
    let dir = tempfile::tempdir().expect("dir");
    for name in [
        "notes.txt",
        "lectern-bad.log",
        "verbose.json",
        "lectern-20261022T032701Z-x.log",
        "lectern-19990101T000000Z-1.log.bak",
    ] {
        std::fs::write(dir.path().join(name), b"keep").expect("write");
    }
    std::fs::create_dir(dir.path().join("lectern-19990101T000000Z-2.log")).expect("dir");
    make(dir.path(), base_time() - DAY * 100, 1, 0, 10);
    let result = cleanup(dir.path(), CURRENT, base_time(), Caps::DEFAULT);
    assert_eq!(result.removed, 1);
    assert_eq!(names(dir.path()).len(), 5);
    assert!(dir.path().join("lectern-19990101T000000Z-2.log").is_dir());
}
