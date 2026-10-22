//! `logs_*`: the log records (docs/design/fs_watch.md「日志与诊断」), and
//! that file contents, reflog lines, ref names and a sink's panic message
//! never reach one.

use std::marker::PhantomData;
use std::path::Path;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lectern_executor::{Config as ExecutorConfig, Executor};
use lectern_kernel::BusyGuard;
use lectern_logging::testing::{self, Capture};

use super::support::*;
use crate::testing::{FakeBackend, FakeSource, RecordingSink};
use crate::{
    Coalesce, Config, DirBatch, DirSpec, Origin, ReflogFile, RepoBatch, RepoSpec, Sink, Watcher,
    WorktreeBatch, WorktreeName, WorktreeSpec,
};

/// In a watched file's content, a reflog message, a branch name and the
/// sinks' panic messages; never in a record.
const CANARY: &str = "canary5f8k2m7w1z";

/// The records with message `msg`.
fn records(log: &Capture, msg: &str) -> Vec<String> {
    let msg = format!("\"msg\":\"{msg}\"");
    log.lines()
        .into_iter()
        .filter(|l| l.contains(&msg))
        .collect()
}

/// The raw JSON text of field `name` in `line`.
fn field<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    let start = line.find(&format!("\"{name}\":"))? + name.len() + 3;
    let rest = &line[start..];
    let end = if let Some(text) = rest.strip_prefix('"') {
        text.find('"')? + 2
    } else {
        rest.find([',', '}'])?
    };
    Some(&rest[..end])
}

/// The one record with message `msg` whose `root` is `root`, under the
/// temporary directory `base`. The home directory may be written as `~`, so
/// only the part from `base`'s own name on is compared.
fn record_of_root(log: &Capture, msg: &str, base: &Path, root: &Path) -> String {
    let tail = format!(
        "/{}\"",
        root.strip_prefix(base.parent().unwrap()).unwrap().display()
    );
    let found: Vec<String> = records(log, msg)
        .into_iter()
        .filter(|l| field(l, "root").is_some_and(|v| v.ends_with(&tail)))
        .collect();
    assert_eq!(found.len(), 1, "{msg:?} for {root:?}: {:#?}", log.lines());
    found.into_iter().next().unwrap()
}

/// Panics on every delivery, quoting the canary.
struct Panicking<B>(PhantomData<fn(B)>);

impl<B: 'static> Sink<B> for Panicking<B> {
    fn deliver(&self, _batch: B, _busy: Option<BusyGuard>) {
        panic!("consumer bug while handling {CANARY}");
    }
}

fn panicking<B: 'static>() -> Arc<Panicking<B>> {
    Arc::new(Panicking(PhantomData))
}

/// A tracked file and HEAD's reflog with the canary in their contents, a
/// new branch named after it, and sinks of every kind that panic with it:
/// the records are written and none holds the canary.
#[test]
fn logs_hold_no_file_or_reflog_contents() {
    let log = testing::capture();
    let fx = Fixture::new();
    fx.source.track(&["src/notes.txt"]);
    let head = fx.common.join("logs/HEAD");
    write(&head, b"");
    let o = fx.opened();

    // A tracked file whose content is the canary.
    write(
        &fx.root.join("src/notes.txt"),
        format!("{CANARY} in a file\n").as_bytes(),
    );
    fx.emit_root(&[("src/notes.txt", F)]);
    // A reflog line whose message quotes the canary, and a branch named
    // after it.
    let line = format!(
        "{} {} A U Thor <author@example.com> 1700000000 +0000\tcommit: {CANARY} in a message\n",
        "0".repeat(40),
        "1".repeat(40)
    );
    append(&head, line.as_bytes());
    let branch = format!("logs/refs/heads/{CANARY}-topic");
    write(&fx.common.join(&branch), line.as_bytes());
    fx.emit_common(&[("logs/HEAD", F), (branch.as_str(), F)]);
    fx.settle();
    fx.wait();
    let batches = o.sink.take();
    assert!(
        batches
            .iter()
            .any(|(b, _)| b.paths.contains(&wp("src/notes.txt")))
    );
    let repo = o.repo_sink.take();
    let reflog: Vec<_> = repo.iter().flat_map(|(b, _)| &b.reflog).collect();
    assert!(
        reflog
            .iter()
            .any(|r| r.file == ReflogFile::Head(WorktreeName::Main)
                && r.origin == Origin::Appended
                && r.line.windows(CANARY.len()).any(|w| w == CANARY.as_bytes()))
    );
    assert!(reflog.iter().any(|r| matches!(&r.file,
        ReflogFile::Branch(name) if name.windows(CANARY.len()).any(|w| w == CANARY.as_bytes()))));

    // Sinks that panic on their first batch: one of each kind.
    let _bad_repo = fx.watcher.watch_repo(
        RepoSpec {
            common_dir: fx.common.clone(),
            coalesce: Coalesce::REPO,
        },
        panicking::<RepoBatch>(),
    );
    let _bad_worktree = o.repo.watch_worktree(
        WorktreeSpec {
            name: WorktreeName::Main,
            root: fx.root.clone(),
            source: fx.source.clone(),
            coalesce: Coalesce::WORKTREE,
        },
        panicking::<WorktreeBatch>(),
    );
    let sessions = fx.base.join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let _bad_dir = fx.watcher.watch_dir(
        DirSpec {
            root: sessions.clone(),
            coalesce: Coalesce::DIR,
        },
        panicking::<DirBatch>(),
    );
    fx.settle();
    // Later events reach the other subscriptions only.
    fx.emit_root(&[("src/notes.txt", F)]);
    fx.settle();
    fx.wait();
    assert_eq!(o.sink.take().len(), 1);

    let msg = "a sink panicked; it is no longer delivered to";
    for (watch, root) in [
        ("\"repo\"", &fx.common),
        ("\"worktree\"", &fx.root),
        ("\"dir\"", &sessions),
    ] {
        let record = record_of_root(log, msg, &fx.base, root);
        assert_eq!(field(&record, "lv"), Some("\"error\""));
        assert_eq!(field(&record, "tg"), Some("\"fs_watch\""));
        assert_eq!(field(&record, "watch"), Some(watch));
    }

    log.assert_absent(CANARY);
}

/// A sink whose first delivery blocks until released.
struct Blocking {
    entered: Mutex<Option<mpsc::Sender<()>>>,
    release: Mutex<Option<mpsc::Receiver<()>>>,
}

impl Sink<WorktreeBatch> for Blocking {
    fn deliver(&self, _batch: WorktreeBatch, _busy: Option<BusyGuard>) {
        if let Some(entered) = self.entered.lock().unwrap().take() {
            let _ = entered.send(());
            if let Some(release) = self.release.lock().unwrap().take() {
                let _ = release.recv_timeout(Duration::from_secs(10));
            }
        }
    }
}

/// `shutdown` while a delivery holds the fs_watch thread past the timeout
/// writes a warn with the timeout.
#[test]
fn logs_shutdown_past_the_timeout_is_a_warn() {
    let log = testing::capture();
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let root = base.join("repo");
    std::fs::create_dir_all(root.join(".git")).unwrap();
    let executor = Executor::new(ExecutorConfig {
        workers: 2,
        batch_workers: 1,
        on_thread_start: None,
    });
    // A timeout no other test uses: the records are told apart by it.
    let timeout = Duration::from_millis(150);
    let watcher = Watcher::new(
        FakeBackend::new(),
        executor.clone(),
        Config {
            flush_timeout: timeout,
            ..Config::default()
        },
    );
    let (entered_tx, entered) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    let repo = watcher.watch_repo(
        RepoSpec {
            common_dir: root.join(".git"),
            coalesce: Coalesce::REPO,
        },
        RecordingSink::<RepoBatch>::new(None),
    );
    let _worktree = repo.watch_worktree(
        WorktreeSpec {
            name: WorktreeName::Main,
            root,
            source: FakeSource::new(),
            coalesce: Coalesce::WORKTREE,
        },
        Arc::new(Blocking {
            entered: Mutex::new(Some(entered_tx)),
            release: Mutex::new(Some(release_rx)),
        }),
    );
    entered.recv_timeout(Duration::from_secs(10)).unwrap();
    watcher.shutdown();
    release.send(()).unwrap();
    executor.shutdown(Duration::from_secs(5));

    let warns: Vec<String> = records(log, "the fs_watch thread did not stop in time")
        .into_iter()
        .filter(|l| field(l, "timeout_us") == Some("150000"))
        .collect();
    assert_eq!(warns.len(), 1, "{:#?}", log.lines());
    assert_eq!(field(&warns[0], "lv"), Some("\"warn\""));
    assert_eq!(field(&warns[0], "tg"), Some("\"fs_watch\""));
}
