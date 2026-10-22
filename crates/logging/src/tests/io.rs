//! 「测试」首帧前没有 IO, UI 线程上没有 IO, 退出.

use std::panic::catch_unwind;
use std::sync::atomic::Ordering;
use std::time::Duration;

use super::{Fixture, all_files, config, records, test_env, with_msg};
use crate::hooks::imp as hooks;
use crate::sink::{AllCaps, Caps, install_for_test};
use crate::{AlreadyInstalled, EarlyConfig, ExportOptions, Filter, Flushed, Verbose};

/// Threads of this process (Linux), when the test has the process to itself.
fn threads() -> Option<usize> {
    if std::env::var_os("NEXTEST").is_none() || !cfg!(target_os = "linux") {
        return None;
    }
    Some(std::fs::read_dir("/proc/self/task").ok()?.count())
}

#[test]
fn install_early_touches_no_files() {
    let fx = Fixture::new();
    hooks::record_file_ops();
    let _early = fx.install(None, None);
    crate::info!("before the first frame");
    crate::error!("an early error");
    assert_eq!(hooks::file_ops(), Vec::new());
    assert!(all_files(fx.path()).is_empty(), "the directory stays empty");
}

#[test]
fn install_early_spawns_no_thread() {
    let fx = Fixture::new();
    let threads_before = threads();
    let spawned = hooks::SPAWNED.load(Ordering::SeqCst);
    let early = fx.install(None, None);
    crate::info!("before the first frame");
    assert_eq!(hooks::SPAWNED.load(Ordering::SeqCst), spawned);
    assert_eq!(threads(), threads_before);
    let logging = early.start(config());
    assert_eq!(
        hooks::SPAWNED.load(Ordering::SeqCst),
        spawned + 1,
        "start spawns the writer"
    );
    assert!(logging.flush(Duration::from_secs(5)));
}

#[test]
fn install_twice_returns_already_installed() {
    let fx = Fixture::new();
    let _early = fx.install(None, None);
    let again = install_for_test(EarlyConfig::default(), test_env(None), AllCaps::STANDARD);
    assert_eq!(again.err(), Some(AlreadyInstalled));
}

#[test]
fn panic_before_start_writes_the_early_buffer() {
    let fx = Fixture::new();
    let early = fx.install(None, None);
    crate::info!("before the first frame");
    let result = catch_unwind(|| panic!("before start"));
    assert!(result.is_err());
    let records_now = records(fx.path());
    let messages: Vec<&str> = records_now
        .iter()
        .filter_map(|r| r["msg"].as_str())
        .collect();
    assert_eq!(messages, ["file", "before the first frame", "panic"]);
    // The writer takes the file over: no second header.
    let logging = early.start(config());
    crate::info!("after start");
    assert!(logging.flush(Duration::from_secs(5)));
    let records = records(fx.path());
    assert_eq!(with_msg(&records, "file").len(), 1, "{records:?}");
    assert_eq!(with_msg(&records, "after start").len(), 1);
    assert_eq!(crate::sink::list(fx.path()).len(), 1);
}

#[test]
fn file_operations_only_on_the_writer_thread() {
    let fx = Fixture::new();
    hooks::record_file_ops();
    let small = AllCaps {
        default: Caps {
            file_bytes: 2048,
            ..Caps::DEFAULT
        },
        verbose: Caps {
            file_bytes: 2048,
            ..Caps::VERBOSE
        },
    };
    let logging = fx.install_with(None, None, small).start(config());
    crate::callsite::LECTERN_LIMIT.store(u32::MAX, Ordering::Relaxed);
    let writers: Vec<_> = (0..3)
        .map(|t| {
            std::thread::Builder::new()
                .name(format!("caller-{t}"))
                .spawn(move || {
                    for i in 0..100_u32 {
                        crate::info!("from a caller"; caller = t, i = i);
                        crate::debug!("verbose from a caller"; i = i);
                    }
                })
                .expect("thread")
        })
        .collect();
    logging.set_verbose(Some(Verbose {
        filter: Filter::parse("debug").0,
        lasts: Duration::from_secs(3600),
    }));
    for w in writers {
        w.join().expect("no panic");
    }
    logging.set_verbose(None);
    assert!(logging.flush(Duration::from_secs(5)));
    let parent = tempfile::tempdir().expect("dir");
    let parent_path = parent.path().to_owned();
    let exporter = logging.clone();
    std::thread::Builder::new()
        .name("exporter".to_owned())
        .spawn(move || {
            exporter
                .export(&parent_path, &ExportOptions::default())
                .expect("export")
        })
        .expect("thread")
        .join()
        .expect("no panic");
    assert_eq!(logging.shutdown(Duration::from_secs(5)), Flushed::Clean);
    let ops = hooks::file_ops();
    assert!(
        ops.iter()
            .filter(|(t, op)| t == "lectern-log" && *op == "open")
            .count()
            > 2,
        "rotation happened: {ops:?}"
    );
    assert!(ops.iter().any(|(t, _)| t == "exporter"));
    for (thread, op) in &ops {
        assert!(
            thread == "lectern-log" || thread == "exporter",
            "{op} on {thread}"
        );
    }
}

#[test]
fn shutdown_writes_the_end_record() {
    let fx = Fixture::new();
    let logging = fx.start(None);
    crate::info!("last words");
    assert_eq!(logging.shutdown(Duration::from_secs(5)), Flushed::Clean);
    let dropped = logging.status().dropped;
    crate::info!("after shutdown");
    assert_eq!(
        logging.status().dropped,
        dropped + 1,
        "later records are only counted"
    );
    let records = records(fx.path());
    let last = records.last().expect("records");
    assert_eq!(last["msg"], "run end");
    assert_eq!(last["run"], records[0]["run"]);
    assert_eq!(with_msg(&records, "last words").len(), 1);
    assert!(with_msg(&records, "after shutdown").is_empty());
    assert_eq!(
        logging.shutdown(Duration::from_secs(1)),
        Flushed::Clean,
        "twice is fine"
    );
}

#[test]
fn missing_end_record_is_reported_next_run() {
    let fx = Fixture::new();
    let logging = fx.start(None);
    assert!(logging.flush(Duration::from_secs(5)));
    let first_run = records(fx.path())[0]["run"].clone();
    fx.crash(&logging);
    let logging = fx.start(None);
    let records = records(fx.path());
    let reports = with_msg(&records, "previous run ended unexpectedly");
    assert_eq!(reports.len(), 1, "{records:?}");
    assert_eq!(reports[0]["run"], first_run);
    fx.restart(&logging);
    let _logging = fx.start(None);
    let again = with_msg(
        &super::records(fx.path()),
        "previous run ended unexpectedly",
    )
    .len();
    assert_eq!(again, 1, "a run that ended normally is not reported");
}
