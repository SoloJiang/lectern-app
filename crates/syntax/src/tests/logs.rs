//! `logs_*`: the log records (docs/design/file_reader.md「日志与诊断」),
//! and that file contents, find queries, the highlight query's text and
//! error messages never reach one.

use std::sync::Arc;
use std::time::Duration;

use lectern_cache::{Budgets, Cache, Config as CacheConfig};
use lectern_executor::Executor;
use lectern_fs_watch::Config as WatchConfig;
use lectern_fs_watch::testing::{FakeBackend, manual};
use lectern_logging::testing::{self, Capture};
use lectern_services::reader::Anchor;

use super::health::ManualClocks;
use super::rig::{FakeGit, Rig, VIEW, modified};
use crate::lang::{CoreSpec, Precedence};
use crate::limits::STUCK_GRACE;
use crate::{Engine, EngineDeps, FindQuery, Lang, Languages, Rules, TextEdit};

/// In identifiers, strings, comments, the find query, the edits, a git
/// error's text and the highlight query below; never in a record.
const CANARY: &str = "canary7q3x9d0e4b";

/// A highlight query that does not compile (the last pattern is never
/// closed) and quotes the canary.
const BAD_QUERY: &str = "((string) @string (#eq? @string \"canary7q3x9d0e4b\"))\n((pair";

/// The file every flow reads: not a canary in its path.
const FILE: &str = "logs/sample.ts";

fn source() -> String {
    format!(
        "// {CANARY} in a comment\n\
         const {CANARY} = \"{CANARY} in a string\";\n\
         function {CANARY}Fn(x: number): string {{\n\
         \x20 return `{CANARY} ${{x}} in a template`;\n\
         }}\n"
    )
}

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

/// The one record with message `msg` whose `path` is [`FILE`].
fn record_of_file(log: &Capture, msg: &str) -> String {
    let path = format!("\"{FILE}\"");
    let found: Vec<String> = records(log, msg)
        .into_iter()
        .filter(|l| field(l, "path") == Some(path.as_str()))
        .collect();
    assert_eq!(found.len(), 1, "{msg:?}: {:#?}", log.lines());
    found.into_iter().next().unwrap()
}

/// An engine that was never started.
fn unstarted() -> (Engine, tempfile::TempDir) {
    let cache_dir = tempfile::tempdir().unwrap();
    let executor = Executor::manual();
    let (watcher, _driver) = manual(FakeBackend::new(), executor.clone(), WatchConfig::default());
    let cache = Cache::new(CacheConfig {
        root: cache_dir.path().to_path_buf(),
        budgets: Budgets::default(),
        ui_thread: None,
    });
    let engine = Engine::with_git(
        EngineDeps {
            executor,
            cache,
            watcher,
            languages: Languages::core(),
        },
        Arc::new(FakeGit::default()),
    );
    (engine, cache_dir)
}

/// Opens, highlights, finds, edits and saves a file with the canary, and
/// runs every failure path a test can reach; the records are written and
/// none holds the canary.
#[test]
fn logs_hold_no_file_contents() {
    let log = testing::capture();
    let text = source();

    // The reading view: the first screen, the full read, the whole-file
    // parse and its highlight; then a find for the canary.
    let rig = Rig::new();
    rig.write(FILE, text.as_bytes());
    let (doc, snapshot) = rig.open_settled(rig.target(FILE, Anchor::Start));
    assert!(
        snapshot
            .spans
            .first()
            .is_some_and(|s| s.lines().count() > 0),
        "highlighted"
    );
    doc.find(FindQuery {
        text: CANARY.into(),
    });
    rig.settle();
    let found = doc.snapshot().find.clone().expect("find results");
    assert!(!found.ranges.is_empty());

    // The same content from the object store.
    let oid = rig.git.add_blob(text.as_bytes());
    let (_object, shown) = rig.open_settled(rig.object("logs/old.ts", oid, Anchor::Start));
    assert!(shown.spans.first().is_some_and(|s| s.lines().count() > 0));

    // Edit mode: an edit typing the canary, a save, the incremental parse.
    let session = doc.edit().expect("editable");
    rig.settle();
    let typed = format!("let {CANARY}Typed = \"{CANARY}\";\n");
    let next = format!("{typed}{text}");
    session.changed(
        Arc::from(next.as_str()),
        &[TextEdit {
            start: 0,
            old_end: 0,
            new_end: typed.len(),
        }],
    );
    session.save(Arc::from(next.as_str()));
    rig.settle();
    assert!(session.idle());
    assert!(session.spans().lines().count() > 0);

    // The file's directory becomes a symbolic link: after the event the
    // session cannot read the file (a warn), nor can a merge (an error).
    std::fs::rename(rig.path("logs"), rig.path("logs-real")).unwrap();
    std::os::unix::fs::symlink(rig.path("logs-real"), rig.path("logs")).unwrap();
    rig.touch(FILE, modified());
    let state = session.state();
    assert!(state.dirty_on_disk && state.unavailable, "{state:?}");
    session.merge(Arc::from(next.as_str()));
    rig.settle();
    assert!(session.state().merged.is_none());

    // No watch: the git source fails with the canary in its text.
    let unwatched = Rig::new();
    *unwatched.git.watch_error.lock().unwrap() = Some(format!("{CANARY} in an error"));
    unwatched.write(FILE, text.as_bytes());
    let (_doc, _) = unwatched.open_settled(unwatched.target(FILE, Anchor::Start));

    // An engine that was never started.
    let (engine, _cache_dir) = unstarted();
    let _doc = engine.open(
        unwatched.target(FILE, Anchor::Start),
        unwatched.root.clone(),
        VIEW,
        Arc::new(|| {}),
    );

    // A highlight query that does not compile.
    let spec: &'static CoreSpec = Box::leak(Box::new(CoreSpec {
        name: "json",
        extensions: &[],
        grammar_crate: "tree-sitter-json",
        grammar_crate_version: "0.24.8",
        language: || tree_sitter_json::LANGUAGE.into(),
        query: BAD_QUERY,
        rules: Rules::Json,
        precedence: Precedence::First,
    }));
    assert!(BAD_QUERY.contains(CANARY));
    assert!(!Lang::core(spec).prepare());

    // A stuck parse disables its grammar version.
    let clocks = ManualClocks::new();
    let health = clocks.health();
    let (a, b) = (clocks.new_thread(), clocks.new_thread());
    clocks.run_on(&a);
    let _running = health.enter("yaml", 7, Duration::from_millis(8)).unwrap();
    a.add_cpu(Duration::from_millis(8) + STUCK_GRACE + Duration::from_nanos(1));
    clocks.run_on(&b);
    assert!(health.enter("yaml", 7, Duration::from_millis(8)).is_none());

    let check = record_of_file(log, "cannot read the edited file after a change on disk");
    assert_eq!(field(&check, "lv"), Some("\"warn\""));
    assert_eq!(field(&check, "tg"), Some("\"syntax\""));
    assert_eq!(field(&check, "op"), Some("\"open\""));
    assert_eq!(field(&check, "error"), Some("\"path_through_symlink\""));
    let merge = record_of_file(log, "cannot read the file to merge");
    assert_eq!(field(&merge, "lv"), Some("\"error\""));
    assert_eq!(field(&merge, "error"), Some("\"path_through_symlink\""));
    let watch = record_of_file(
        log,
        "no watch for the document; it is not refreshed on changes",
    );
    assert_eq!(field(&watch, "lv"), Some("\"warn\""));
    assert_eq!(field(&watch, "error"), Some("\"corrupt\""));
    let open = record_of_file(
        log,
        "open before the engine started; the document stays empty",
    );
    assert_eq!(field(&open, "lv"), Some("\"error\""));
    let query = records(
        log,
        "the highlight query does not compile; the language is shown as plain text",
    );
    assert_eq!(query.len(), 1, "{query:#?}");
    assert_eq!(field(&query[0], "lang"), Some("\"json\""));
    assert_eq!(field(&query[0], "error"), Some("\"syntax\""));
    assert!(field(&query[0], "row").is_some());
    let stuck: Vec<String> = records(
        log,
        "a parse ran past its budget plus the grace; the grammar is disabled for this run",
    )
    .into_iter()
    .filter(|l| field(l, "grammar") == Some("\"0000000000000007\""))
    .collect();
    assert_eq!(stuck.len(), 1, "{:#?}", log.lines());
    assert_eq!(field(&stuck[0], "lang"), Some("\"yaml\""));
    assert_eq!(field(&stuck[0], "budget_us"), Some("8000"));

    log.assert_absent(CANARY);
}
