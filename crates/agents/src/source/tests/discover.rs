//! 发现（`discover_*`）：枚举、候选路径、过滤、压缩会话的摘要与它的缓存。

use std::path::PathBuf;

use insta::assert_snapshot;
use lectern_cache::{Cache, Compression, Entry, Namespace};
use lectern_services::session::{ContentRef, Cursor, Format, SessionError, SessionKey};

use super::support::*;
use super::{copy_tree, fixtures};
use crate::codex;
use crate::source::cached::{CachedSummary, summary_key};
use crate::source::discover::FileId;

const MAIN: &str = "11111111-1111-4111-8111-111111111111";
const L1: &str = "aaaaaaaa-0000-4000-8000-000000000001";
const L2: &str = "aaaaaaaa-0000-4000-8000-000000000002";

fn copy_claude_fixtures(h: &Harness) {
    copy_tree(
        &fixtures().join("claude-code/2.1.302/projects"),
        &h.claude_root(),
    );
}

fn copy_codex_fixtures(h: &Harness) {
    for version in ["0.140.0-legacy", "0.140.0-paginated"] {
        copy_tree(
            &fixtures().join(format!("codex/{version}/sessions")),
            &h.codex_root(),
        );
    }
}

/// `codex/sessions/` 下 legacy 主会话的路径。
pub(crate) fn legacy_main(h: &Harness) -> PathBuf {
    h.codex_root()
        .join(format!("2026/10/02/rollout-2026-10-02T09-00-00-{L1}.jsonl"))
}

#[test]
fn discover_lists_the_fixtures_of_both_agents() {
    let mut h = Harness::new();
    copy_claude_fixtures(&h);
    copy_codex_fixtures(&h);
    h.start();

    let seen = h.take_seen(&claude_source());
    assert_eq!(seen.len(), 1, "one publication after the first discovery");
    assert_eq!(seen[0].event.generation, 1);
    assert_eq!(seen[0].event.changed.len(), 4);
    assert!(seen[0].event.removed.is_empty());
    assert!(
        seen[0]
            .event
            .changed
            .iter()
            .all(|k| k.source == claude_source())
    );
    assert_snapshot!("claude_list", h.render(&seen[0].list));

    let seen = h.take_seen(&codex_source());
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].event.changed.len(), 5);
    assert_snapshot!("codex_list", h.render(&seen[0].list));
    assert_eq!(
        seen[0].list.diagnostics,
        h.list(&codex_source()).diagnostics
    );
}

#[test]
fn discover_empty_and_missing_roots_publish_an_empty_list() {
    let mut h = Harness::new();
    std::fs::remove_dir_all(h.codex_root()).unwrap();
    h.start();
    for source in [claude_source(), codex_source()] {
        let seen = h.take_seen(&source);
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].event.generation, 1);
        assert!(seen[0].list.sessions.is_empty());
        assert!(seen[0].list.diagnostics.unreadable_dirs.is_empty());
    }
}

#[test]
fn discover_prefers_jsonl_over_zst_and_falls_back_when_it_goes() {
    let mut h = Harness::new();
    let jsonl = legacy_main(&h);
    h.copy(
        &fixtures().join(format!(
            "codex/0.140.0-legacy/sessions/2026/10/02/rollout-2026-10-02T09-00-00-{L1}.jsonl"
        )),
        &jsonl,
    );
    let zst = compress(&jsonl);
    h.start();
    let list = h.list(&codex_source());
    assert_eq!(list.sessions.len(), 1, "one session for both files");
    let before = list.sessions[0].clone();
    assert_eq!(&*before.file, jsonl.as_path());
    assert!(!before.tail_pending);

    // Codex compressed the cold file and deleted the original.
    std::fs::remove_file(&jsonl).unwrap();
    h.emit(&h.codex_root(), &[(&h.rel_to(&h.codex_root(), &jsonl), F)]);
    let list = h.list(&codex_source());
    let after = &list.sessions[0];
    assert_eq!(&*after.file, zst.as_path());
    assert!(
        !after.tail_pending,
        "same content: the tail fields carry over"
    );
    assert_eq!(after.last_activity, before.last_activity);
    assert_eq!(after.open_turn, before.open_turn);
    let seen = h.take_seen(&codex_source());
    assert_eq!(
        &*seen.last().unwrap().event.changed,
        std::slice::from_ref(&before.key)
    );

    std::fs::remove_file(&zst).unwrap();
    h.emit(&h.codex_root(), &[(&h.rel_to(&h.codex_root(), &zst), F)]);
    assert!(h.list(&codex_source()).sessions.is_empty());
    let seen = h.take_seen(&codex_source());
    assert_eq!(&*seen.last().unwrap().event.removed, [before.key]);
}

#[test]
fn discover_prefers_sessions_over_archived_sessions() {
    let mut h = Harness::new();
    let fixture = fixtures().join(format!(
        "codex/0.140.0-legacy/sessions/2026/10/02/rollout-2026-10-02T09-00-00-{L1}.jsonl"
    ));
    let live = legacy_main(&h);
    let archived_root = h.base.join("codex/archived_sessions");
    let archived = archived_root.join(live.file_name().unwrap());
    h.copy(&fixture, &live);
    h.copy(&fixture, &archived);
    h.start();
    let list = h.list(&codex_source());
    assert_eq!(list.sessions.len(), 1);
    assert_eq!(&*list.sessions[0].file, live.as_path());
    assert!(!list.sessions[0].archived);

    std::fs::remove_file(&live).unwrap();
    h.emit(&h.codex_root(), &[(&h.rel_to(&h.codex_root(), &live), F)]);
    let list = h.list(&codex_source());
    assert_eq!(&*list.sessions[0].file, archived.as_path());
    assert!(list.sessions[0].archived, "listed and marked archived");
}

#[test]
fn discover_hidden_sessions_leave_the_list_and_give_no_records() {
    let mut h = Harness::new();
    copy_claude_fixtures(&h);
    h.start();
    let hidden = key(&claude_source(), MAIN);
    assert!(h.list(&claude_source()).get(&hidden).is_some());
    h.take_seen(&claude_source());

    h.filter.hide(hidden.clone());
    let source = h.source(&claude_source());
    assert_eq!(
        source.read(&hidden, &Cursor::START, u64::MAX),
        Err(SessionError::NotFound)
    );
    assert_eq!(source.turns(&hidden), Err(SessionError::NotFound));
    assert_eq!(
        source.load(&hidden, &ContentRef::new(0, 1, 0)),
        Err(SessionError::NotFound)
    );
    assert_eq!(source.resume(&hidden), None);
    drop(source);

    // The next refresh drops it from the list.
    h.emit(&h.claude_root(), &[("-Users-dev-fixture-repo", D)]);
    let list = h.list(&claude_source());
    assert!(list.get(&hidden).is_none());
    assert_eq!(list.sessions.len(), 3, "its subagents stay");
    let seen = h.take_seen(&claude_source());
    assert_eq!(&*seen.last().unwrap().event.removed, [hidden]);
}

#[test]
fn discover_zst_without_a_cached_summary_is_listed_first_then_completed() {
    let mut h = Harness::new();
    let jsonl = legacy_main(&h);
    h.copy(
        &fixtures().join(format!(
            "codex/0.140.0-legacy/sessions/2026/10/02/rollout-2026-10-02T09-00-00-{L1}.jsonl"
        )),
        &jsonl,
    );
    let zst = compress(&jsonl);
    let adapter = codex::Codex::without_decoded_copies(codex_source());
    let (plain, _) = adapter
        .summarize(&adapter.session_file(&jsonl).unwrap())
        .unwrap();
    std::fs::remove_file(&jsonl).unwrap();
    h.start();

    let seen = h.take_seen(&codex_source());
    assert_eq!(
        seen.len(),
        2,
        "published before the full decode, then again"
    );
    let first = &seen[0].list.sessions[0];
    assert!(first.tail_pending);
    assert!(!first.open_turn);
    let done = &seen[1].list.sessions[0];
    assert!(!done.tail_pending);
    assert_eq!(&*seen[1].event.changed, std::slice::from_ref(&done.key));
    // The last record of the session, not the file's modification time.
    assert_eq!(done.last_activity, plain.last_activity);
    assert_eq!(done.open_turn, plain.open_turn);
    assert!(
        !h.base.join("decoded").exists()
            || std::fs::read_dir(h.base.join("decoded/session-codex"))
                .map(|d| d.count() == 0)
                .unwrap_or(true),
        "completion builds no decoded copy"
    );

    // Restart: the summary comes from the cache, nothing left to complete.
    h.stop();
    h.start();
    let seen = h.take_seen(&codex_source());
    assert_eq!(seen.len(), 1);
    let cached = &seen[0].list.sessions[0];
    assert!(!cached.tail_pending);
    assert_eq!(cached.last_activity, done.last_activity);
    assert_eq!(&*cached.file, zst.as_path());
}

/// Same name as `agents.summary`, the format version before it (written by an older build).
struct OtherVersion(Vec<u8>);

impl Entry for OtherVersion {
    const NAMESPACE: Namespace = Namespace {
        name: "agents.summary",
        version: CachedSummary::NAMESPACE.version - 1,
        compression: Compression::None,
    };

    fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.0);
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        Some(OtherVersion(bytes.to_vec()))
    }
}

#[test]
fn discover_summary_cache_misses_after_a_version_change() {
    let mut h = Harness::new();
    let jsonl = legacy_main(&h);
    h.copy(
        &fixtures().join(format!(
            "codex/0.140.0-legacy/sessions/2026/10/02/rollout-2026-10-02T09-00-00-{L1}.jsonl"
        )),
        &jsonl,
    );
    let zst = compress(&jsonl);
    std::fs::remove_file(&jsonl).unwrap();
    let id = FileId::stat(&zst).unwrap();
    let session = key(&codex_source(), L1);

    // A complete summary marked by its title.
    let adapter = codex::Codex::without_decoded_copies(codex_source());
    let (mut marked, stats) = adapter
        .complete(&adapter.session_file(&zst).unwrap())
        .unwrap();
    marked.title = Some("from the cache".into());
    let cached = CachedSummary::new(&crate::read::Summarized {
        summary: marked,
        stats,
        lineage: None,
    });
    let mut bytes = Vec::new();
    cached.encode(&mut bytes);

    let cache_dir = h.cache_dir();
    let cache = || {
        Cache::new(lectern_cache::Config {
            root: cache_dir.clone(),
            budgets: lectern_cache::Budgets::default(),
            ui_thread: None,
        })
    };
    {
        let cache = cache();
        let app = cache.app();
        // Another adapter version: another key.
        let _ = app.put(
            &summary_key(&session, &id, codex::ADAPTER_VERSION + 1),
            &cached,
        );
        // An older summary format under the right key.
        let _ = app.put(
            &summary_key(&session, &id, codex::ADAPTER_VERSION),
            &OtherVersion(bytes),
        );
    }
    h.start();
    let seen = h.take_seen(&codex_source());
    assert!(seen[0].list.sessions[0].tail_pending, "both are misses");
    assert_ne!(
        seen[0].list.sessions[0].title.as_deref(),
        Some("from the cache")
    );
    h.stop();

    // The same summary under the current versions is a hit.
    {
        let cache = cache();
        let written = cache
            .app()
            .put(&summary_key(&session, &id, codex::ADAPTER_VERSION), &cached);
        assert_eq!(written, lectern_cache::Written::Committed);
    }
    h.start();
    let list = h.list(&codex_source());
    assert_eq!(list.sessions[0].title.as_deref(), Some("from the cache"));
    assert!(!list.sessions[0].tail_pending);
}

#[test]
fn discover_lists_unrecognized_files_only_when_they_have_a_cwd() {
    let mut h = Harness::new();
    let dir = h.claude_root().join("-tmp-x");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("{}.jsonl", uuid(1))), "not json\n{}\n").unwrap();
    let codex_dir = h.codex_root().join("2026/10/09");
    std::fs::create_dir_all(&codex_dir).unwrap();
    // No `session_meta`, but a `turn_context` with a cwd.
    std::fs::write(
        codex_dir.join(format!("rollout-2026-10-09T09-00-00-{}.jsonl", uuid(2))),
        concat!(
            r#"{"timestamp":"2026-10-09T09:00:00.000Z","type":"turn_context","payload":{"turn_id":"t1","cwd":"/Users/dev/fixture/repo","model":"m"}}"#,
            "\n"
        ),
    )
    .unwrap();
    h.start();
    assert!(h.list(&claude_source()).sessions.is_empty());
    let list = h.list(&codex_source());
    assert_eq!(list.sessions.len(), 1);
    assert_eq!(list.sessions[0].format, Format::Unrecognized);
}

#[test]
fn discover_fork_summary_is_redone_once_its_source_was_read() {
    let mut h = Harness::new();
    copy_codex_fixtures(&h);
    h.start();
    let fork = key(&codex_source(), L2);
    assert!(
        h.list(&codex_source())
            .get(&fork)
            .unwrap()
            .inheritance_unknown,
        "discovery does not scan the source session"
    );
    h.take_seen(&codex_source());

    let batch = h
        .source(&codex_source())
        .read(&fork, &Cursor::START, u64::MAX)
        .unwrap();
    assert!(batch.records.iter().any(|r| r.inherited));
    h.settle();
    let summary = h.list(&codex_source()).get(&fork).unwrap().clone();
    assert!(!summary.inheritance_unknown);
    let seen = h.take_seen(&codex_source());
    assert_eq!(
        &*seen.last().unwrap().event.changed,
        std::slice::from_ref(&fork)
    );

    // Reading again changes nothing.
    h.source(&codex_source())
        .read(&fork, &Cursor::START, u64::MAX)
        .unwrap();
    h.settle();
    assert!(h.take_seen(&codex_source()).is_empty());
}

#[test]
fn discover_fork_read_before_its_source_appears_is_redone_when_the_source_is_listed() {
    let mut h = Harness::new();
    let rel_fork = format!("2026/10/03/rollout-2026-10-03T09-00-00-{L2}.jsonl");
    let rel_main = format!("2026/10/02/rollout-2026-10-02T09-00-00-{L1}.jsonl");
    let legacy = fixtures().join("codex/0.140.0-legacy/sessions");
    h.copy(&legacy.join(&rel_fork), &h.codex_root().join(&rel_fork));
    h.start();
    let fork = key(&codex_source(), L2);
    let before = h.list(&codex_source()).get(&fork).unwrap().clone();
    assert!(before.inheritance_unknown);

    // Read while the source session is not on this machine: the boundary stays unknown.
    h.source(&codex_source())
        .read(&fork, &Cursor::START, u64::MAX)
        .unwrap();
    h.settle();
    assert!(
        h.list(&codex_source())
            .get(&fork)
            .unwrap()
            .inheritance_unknown
    );
    h.take_seen(&codex_source());

    // The source appears; nobody reads the unchanged fork again.
    h.copy(&legacy.join(&rel_main), &h.codex_root().join(&rel_main));
    h.emit(&h.codex_root(), &[(&rel_main, F)]);
    let after = h.list(&codex_source()).get(&fork).unwrap().clone();
    assert!(!after.inheritance_unknown, "the boundary is known now");
    assert_ne!(
        after.title, before.title,
        "the title is the fork's own prompt"
    );
    let changed: Vec<_> = h
        .take_seen(&codex_source())
        .iter()
        .flat_map(|s| s.event.changed.iter().cloned())
        .collect();
    assert!(changed.contains(&fork));
}

/// The legacy fixtures' paths under `codex/sessions/`.
fn legacy_rel(thread: &str) -> String {
    match thread {
        L1 => format!("2026/10/02/rollout-2026-10-02T09-00-00-{L1}.jsonl"),
        L2 => format!("2026/10/03/rollout-2026-10-03T09-00-00-{L2}.jsonl"),
        _ => unreachable!(),
    }
}

fn copy_legacy(h: &Harness, thread: &str) -> PathBuf {
    let rel = legacy_rel(thread);
    let to = h.codex_root().join(&rel);
    h.copy(
        &fixtures().join("codex/0.140.0-legacy/sessions").join(&rel),
        &to,
    );
    to
}

/// Review comment 4303158820: a forced re-summary of a compressed fork must not be answered from
/// the summary cache, which still says its boundary is unknown.
#[test]
fn discover_compressed_fork_is_redone_past_its_cached_summary_when_the_source_appears() {
    let mut h = Harness::new();
    let fork_path = copy_legacy(&h, L2);
    compress(&fork_path);
    std::fs::remove_file(&fork_path).unwrap();
    h.start();
    let fork = key(&codex_source(), L2);
    let cached = h.list(&codex_source()).get(&fork).unwrap().clone();
    assert!(cached.inheritance_unknown);
    assert!(
        !cached.tail_pending,
        "completed, so the summary is in the cache"
    );

    h.source(&codex_source())
        .read(&fork, &Cursor::START, u64::MAX)
        .unwrap();
    h.settle();
    assert!(
        h.list(&codex_source())
            .get(&fork)
            .unwrap()
            .inheritance_unknown
    );

    let main = copy_legacy(&h, L1);
    h.emit(&h.codex_root(), &[(&h.rel_to(&h.codex_root(), &main), F)]);
    let after = h.list(&codex_source()).get(&fork).unwrap().clone();
    assert!(
        !after.inheritance_unknown,
        "not taken from the cached summary"
    );
    assert!(
        !after.tail_pending,
        "the tail fields still come from the cache"
    );
    assert_eq!(after.last_activity, cached.last_activity);
}

/// Review comment 4303158829: when the source of a resolved fork is deleted, the fork's boundary
/// is unknown again (`usage` warns about possibly inherited usage).
#[test]
fn discover_fork_whose_source_is_deleted_is_marked_unknown_again() {
    let mut h = Harness::new();
    let main = copy_legacy(&h, L1);
    copy_legacy(&h, L2);
    h.start();
    let fork = key(&codex_source(), L2);
    h.source(&codex_source())
        .read(&fork, &Cursor::START, u64::MAX)
        .unwrap();
    h.settle();
    assert!(
        !h.list(&codex_source())
            .get(&fork)
            .unwrap()
            .inheritance_unknown
    );
    h.take_seen(&codex_source());

    std::fs::remove_file(&main).unwrap();
    h.emit(&h.codex_root(), &[(&h.rel_to(&h.codex_root(), &main), F)]);
    let list = h.list(&codex_source());
    assert!(list.get(&key(&codex_source(), L1)).is_none());
    assert!(list.get(&fork).unwrap().inheritance_unknown);
    let changed: Vec<SessionKey> = h
        .take_seen(&codex_source())
        .iter()
        .flat_map(|s| s.event.changed.iter().cloned())
        .collect();
    assert!(changed.contains(&fork));
}

/// Moves a compressed session from `sessions/` to `archived_sessions/` (same device, inode, size
/// and modification time) and reports both paths in one round.
fn archive(h: &Harness, zst: &std::path::Path) -> PathBuf {
    let archived = h
        .base
        .join("codex/archived_sessions")
        .join(zst.file_name().unwrap());
    std::fs::rename(zst, &archived).unwrap();
    let backend = &h.run().backend;
    backend.emit(&h.codex_root(), &[(&h.rel_to(&h.codex_root(), zst), F)]);
    backend.emit(
        &h.base.join("codex/archived_sessions"),
        &[(archived.file_name().unwrap().to_str().unwrap(), F)],
    );
    h.advance();
    archived
}

/// Review comment 4303158825: a summary cache hit for a moved archive takes `archived` (and `file`)
/// from the new path.
#[test]
fn discover_archived_move_recomputes_the_path_fields_of_a_cached_summary() {
    let mut h = Harness::new();
    std::fs::create_dir_all(h.base.join("codex/archived_sessions")).unwrap();
    let main = copy_legacy(&h, L1);
    let zst = compress(&main);
    std::fs::remove_file(&main).unwrap();
    h.start();
    let session = key(&codex_source(), L1);
    let before = h.list(&codex_source()).get(&session).unwrap().clone();
    assert!(!before.archived && !before.tail_pending);

    let archived = archive(&h, &zst);
    let after = h.list(&codex_source()).get(&session).unwrap().clone();
    assert!(after.archived, "from the new path, not the cached summary");
    assert_eq!(&*after.file, archived.as_path());
    assert!(!after.tail_pending);
    assert_eq!(after.last_activity, before.last_activity);
}

/// Review comment 4303158833: moving the only compressed candidate (or removing one of several)
/// keeps the decoded copy while another candidate of the session exists.
#[test]
fn discover_archived_move_keeps_the_decoded_copy() {
    let mut h = Harness::new();
    std::fs::create_dir_all(h.base.join("codex/archived_sessions")).unwrap();
    let main = copy_legacy(&h, L1);
    let zst = compress(&main);
    std::fs::remove_file(&main).unwrap();
    // A second compressed candidate of the same session in the archive.
    let other = h
        .base
        .join("codex/archived_sessions")
        .join(format!("rollout-2026-10-02T09-00-00-{L1}_0001.jsonl.zst"));
    std::fs::copy(&zst, &other).unwrap();
    h.start();
    let session = key(&codex_source(), L1);
    let engine = h.codex_engine();
    h.source(&codex_source())
        .read(&session, &Cursor::START, u64::MAX)
        .unwrap();
    assert_eq!(engine.adapter.decodes(), 1);

    // Removing the other candidate keeps the copy of the chosen one.
    std::fs::remove_file(&other).unwrap();
    h.emit(
        &h.base.join("codex/archived_sessions"),
        &[(other.file_name().unwrap().to_str().unwrap(), F)],
    );
    // Moving the chosen one keeps it too: same file identity, same copy.
    archive(&h, &zst);
    h.source(&codex_source())
        .read(&session, &Cursor::START, u64::MAX)
        .unwrap();
    assert_eq!(
        engine.adapter.decodes(),
        1,
        "no second decode of the archive"
    );
}
