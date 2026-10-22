//! The review engine driven without the plugin (docs/design/review.md
//! 「测试」: `base_*`, `claims_*`, `files_*`, `narrative_*`, `rows_*`).
//! Repositories from `TestRepo` with hand-written reflogs, sessions from
//! `FakeSessions` scripts; structured results are insta snapshots rendered
//! as text.

use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use serde_json::{Value, json};

use lectern_git::Oid;
use lectern_git::testing::{IndexEntry, TestHead, TestRepo};
use lectern_services::changes::{ChangePath, ChangeStatus};
use lectern_services::project::BlobId;
use lectern_services::review::{
    AttributionState, BaseHow, DiffRow, FileKey, LoadState, NarrativeItem, Need, ReviewCommand,
    ReviewId, ReviewSnapshot, ReviewTarget, RowsKey, SeenState,
};
use lectern_services::session::{SessionKey, SourceId, Timestamp};

use crate::log::{LogFile, SessionId};
use crate::testing::{Driven, FakeProjects, FakeSessions, ReflogLine, write_reflog};

mod logs;

const SOURCE: &str = "session-claude-code";
/// Unix seconds around which the scenarios happen.
const T: i64 = 1_760_000_000;
const NOW: Timestamp = Timestamp((T + 3600) * 1000);

fn ms(s: i64) -> i64 {
    s * 1000
}

struct Fx {
    repo: TestRepo,
    projects: Arc<FakeProjects>,
    dir: tempfile::TempDir,
}

impl Fx {
    fn new() -> Fx {
        let repo = TestRepo::new();
        let projects = Arc::new(FakeProjects::new(repo.common_dir(), repo.root()));
        Fx {
            repo,
            projects,
            dir: tempfile::tempdir().unwrap(),
        }
    }

    fn root(&self) -> String {
        self.repo.root().to_string_lossy().into_owned()
    }

    fn commit(&self, files: &[(&str, &str)], parent: Option<Oid>, at: i64) -> Oid {
        let files: Vec<(&str, &[u8])> = files.iter().map(|(p, c)| (*p, c.as_bytes())).collect();
        let tree = self.repo.tree(&files);
        let parents: Vec<Oid> = parent.into_iter().collect();
        self.repo.commit_at(tree, &parents, "commit", at)
    }

    /// HEAD on `branch` at `commit`; the index has `files` as committed.
    fn head(&self, branch: &str, commit: Oid, files: &[(&str, &str)]) {
        self.repo.set_ref(branch, commit);
        self.repo.set_head(TestHead::Branch(branch));
        let entries: Vec<IndexEntry> = files
            .iter()
            .map(|(p, c)| IndexEntry::new(p, lectern_git::Mode::File, self.repo.blob(c.as_bytes())))
            .collect();
        self.repo.write_index(self.repo.common_dir(), &entries);
    }

    fn reflog(&self, lines: &[(Option<Oid>, Oid, i64, &str)]) {
        let lines: Vec<ReflogLine<'_>> = lines
            .iter()
            .map(|(old, new, time, message)| ReflogLine {
                old: *old,
                new: *new,
                time: *time,
                message,
            })
            .collect();
        write_reflog(self.repo.common_dir(), self.repo.hash(), &lines);
    }

    fn write(&self, path: &str, content: &str) {
        self.repo.write_file(path, content.as_bytes());
    }

    fn mtime(&self, path: &str, at_ms: i64) {
        self.repo
            .set_mtime(path, UNIX_EPOCH + Duration::from_millis(at_ms as u64));
    }

    fn sessions(&self, sessions: Vec<Value>) -> Arc<FakeSessions> {
        Arc::new(FakeSessions::from_script(
            SOURCE,
            &json!({ "sessions": sessions }).to_string(),
        ))
    }

    fn driven(&self, sources: Vec<Arc<FakeSessions>>) -> Driven {
        self.driven_in(self.dir.path(), sources)
    }

    fn driven_in(&self, dir: &Path, sources: Vec<Arc<FakeSessions>>) -> Driven {
        Driven::new(Arc::clone(&self.projects), sources, dir, NOW)
    }
}

fn session(id: &str, cwd: &str, start: i64, turns: Vec<Value>) -> Value {
    json!({ "id": id, "cwd": cwd, "started_at": start, "turns": turns })
}

fn turn(id: &str, at: i64, items: Vec<Value>) -> Value {
    json!({ "id": id, "at": at, "prompt": format!("prompt {id}"), "items": items })
}

fn edit(call: &str, at: i64, changes: Vec<Value>) -> Value {
    json!({ "edit": call, "at": at, "done": at + 500, "changes": changes })
}

fn replace(path: &str, old: &str, new: &str) -> Value {
    json!({ "path": path, "replace": [old, new] })
}

fn write(path: &str, content: &str) -> Value {
    json!({ "path": path, "write": content })
}

fn run(call: &str, at: i64, done: i64) -> Value {
    json!({ "run": call, "command": "cargo fmt", "at": at, "done": done })
}

fn key(id: &str) -> SessionKey {
    SessionKey {
        source: SourceId(SOURCE.into()),
        id: id.into(),
    }
}

fn target(id: &str) -> ReviewTarget {
    ReviewTarget::Session(key(id))
}

fn path(p: &str) -> ChangePath {
    ChangePath(Arc::from(p.as_bytes()))
}

fn modified(p: &str) -> FileKey {
    FileKey {
        old: Some(path(p)),
        new: Some(path(p)),
    }
}

fn open(d: &Driven, t: ReviewTarget) -> (ReviewId, Arc<ReviewSnapshot>) {
    let id = d.open(t);
    d.run();
    let s = d.snapshot(id).unwrap();
    assert_eq!(s.state, LoadState::Ready, "{:?}", s.state);
    (id, s)
}

fn paths(s: &ReviewSnapshot) -> Vec<String> {
    s.files
        .iter()
        .map(|f| String::from_utf8_lossy(&f.change.path.0).into_owned())
        .collect()
}

fn hex(id: &Option<BlobId>) -> String {
    match id {
        Some(id) => id.as_bytes()[..4]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect(),
        None => "empty".into(),
    }
}

fn oid(o: Oid) -> Option<BlobId> {
    BlobId::from_bytes(o.as_bytes())
}

/// The snapshot as text: base, files with their attribution and hunks,
/// seen states, the narrative.
fn render(d: &Driven, id: ReviewId) -> String {
    let s = d.snapshot(id).unwrap();
    let work = d.engine.work(id).unwrap();
    let results = work.results.lock().unwrap().clone();
    let mut out = String::new();
    if let Some(b) = &s.base {
        writeln!(out, "base {:?} {}", b.how, hex(&b.commit)).unwrap();
    }
    let session = |i: &lectern_services::session::SessionKey| i.id.to_string();
    writeln!(out, "unclaimed {} progress {:?}", s.unclaimed, s.progress).unwrap();
    for (n, f) in s.files.iter().enumerate() {
        let a = &f.attribution;
        let status = match &f.change.status {
            ChangeStatus::Renamed { from, .. } => {
                format!("renamed from {}", String::from_utf8_lossy(&from.0))
            }
            other => format!("{other:?}"),
        };
        writeln!(
            out,
            "file {} {status} {:?} turns {:?} commands {:?} others {:?} drifted_hunks {} seen {:?}",
            String::from_utf8_lossy(&f.change.path.0),
            a.state,
            a.turns
                .iter()
                .map(|t| (t.session, t.ordinal))
                .collect::<Vec<_>>(),
            a.commands
                .iter()
                .map(|c| c.call.0.to_string())
                .collect::<Vec<_>>(),
            a.other_sessions.iter().map(session).collect::<Vec<_>>(),
            a.drifted_hunks,
            s.seen[n],
        )
        .unwrap();
        if let Some(r) = &results[n] {
            for h in r.hunks.iter() {
                writeln!(
                    out,
                    "  hunk lines {} turns {:?} others {:?} drifted {}",
                    h.lines,
                    h.turns
                        .iter()
                        .map(|t| (t.session, t.ordinal))
                        .collect::<Vec<_>>(),
                    h.others.iter().map(session).collect::<Vec<_>>(),
                    h.drifted
                )
                .unwrap();
            }
        }
    }
    for t in s.turns.iter().flat_map(|t| t.iter()) {
        writeln!(
            out,
            "turn {}:{} {} {:?} after_last_seen {}",
            t.at.session, t.at.ordinal, t.id.0, t.prompt_preview, t.after_last_seen
        )
        .unwrap();
        for item in t.items.iter() {
            let line = match item {
                NarrativeItem::AgentText(_) => "text".to_string(),
                NarrativeItem::Injected(_) => "injected".to_string(),
                NarrativeItem::Step(st) => format!(
                    "step {} calls {:?} +{} -{} {:?} {:?}",
                    String::from_utf8_lossy(
                        &st.file.new.as_ref().or(st.file.old.as_ref()).unwrap().0
                    ),
                    st.calls.iter().map(|c| c.0.to_string()).collect::<Vec<_>>(),
                    st.added,
                    st.removed,
                    st.alignment,
                    st.survival
                ),
                NarrativeItem::Command { call, files, .. } => format!(
                    "command {} files {:?}",
                    call.0,
                    files
                        .iter()
                        .map(|f| String::from_utf8_lossy(&f.new.as_ref().unwrap().0).into_owned())
                        .collect::<Vec<_>>()
                ),
                NarrativeItem::Subagent { session, call } => {
                    format!(
                        "subagent {} by {}",
                        s.sessions[*session as usize].key.id, call.0
                    )
                }
                NarrativeItem::Tool { call, name, status } => {
                    format!("tool {} {name} {status:?}", call.0)
                }
            };
            writeln!(out, "  {line}").unwrap();
        }
    }
    out
}

// ---- base_* -------------------------------------------------------------

/// C1 before the session, C2 during it (the agent's commit); the worktree
/// changes `a.txt` further.
fn base_fixture() -> (Fx, Oid, Oid) {
    let fx = Fx::new();
    let c1 = fx.commit(&[("a.txt", "one\n")], None, T - 1000);
    let c2 = fx.commit(&[("a.txt", "two\n")], Some(c1), T + 100);
    fx.head("refs/heads/main", c2, &[("a.txt", "two\n")]);
    fx.write("a.txt", "two\nthree\n");
    (fx, c1, c2)
}

fn base_session(fx: &Fx) -> Arc<FakeSessions> {
    fx.sessions(vec![session(
        "s",
        &fx.root(),
        ms(T),
        vec![turn(
            "t1",
            ms(T + 10),
            vec![
                edit("c1", ms(T + 20), vec![replace("a.txt", "one\n", "two\n")]),
                edit(
                    "c2",
                    ms(T + 200),
                    vec![replace("a.txt", "two\n", "two\nthree\n")],
                ),
            ],
        )],
    )])
}

#[test]
fn base_from_the_reflog_record_before_the_session() {
    let (fx, c1, c2) = base_fixture();
    fx.reflog(&[
        (None, c1, T - 1000, "commit (initial): one"),
        (Some(c1), c2, T + 100, "commit: two"),
    ]);
    let d = fx.driven(vec![base_session(&fx)]);
    let (id, s) = open(&d, target("s"));
    let base = s.base.unwrap();
    assert_eq!((base.how, base.commit), (BaseHow::Reflog, oid(c1)));
    // The agent's own commit after the base does not cut its steps.
    insta::assert_snapshot!("base_reflog", render(&d, id));
}

#[test]
fn base_before_the_log_and_without_one() {
    let (fx, c1, c2) = base_fixture();
    // Every record is after the session started: the oldest's old value.
    fx.reflog(&[(Some(c1), c2, T + 100, "commit: two")]);
    let d = fx.driven(vec![base_session(&fx)]);
    let (_, s) = open(&d, target("s"));
    let base = s.base.unwrap();
    assert_eq!((base.how, base.commit), (BaseHow::BeforeLog, oid(c1)));

    // No reflog: the current HEAD.
    std::fs::remove_file(fx.repo.common_dir().join("logs/HEAD")).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let d = fx.driven_in(dir.path(), vec![base_session(&fx)]);
    let (_, s) = open(&d, target("s"));
    let base = s.base.unwrap();
    assert_eq!((base.how, base.commit), (BaseHow::CurrentHead, oid(c2)));
}

#[test]
fn base_first_mark_seen_records_it_and_it_wins() {
    let (fx, c1, c2) = base_fixture();
    fx.reflog(&[
        (None, c1, T - 1000, "commit (initial): one"),
        (Some(c1), c2, T + 100, "commit: two"),
    ]);
    let d = fx.driven(vec![base_session(&fx)]);
    let (id, _) = open(&d, target("s"));
    d.command(id, ReviewCommand::MarkSeen(modified("a.txt")));
    // The seen array is replaced before `command` returns.
    assert_eq!(d.snapshot(id).unwrap().seen[0], SeenState::Seen);
    d.run();
    let mut log = LogFile::new(&fx.dir.path().join("review"), fx.repo.common_dir());
    let (index, _) = log.load(NOW);
    let recorded = index
        .base(&SessionId {
            source: SOURCE.into(),
            id: "s".into(),
        })
        .cloned()
        .unwrap();
    assert_eq!(
        (recorded.commit, recorded.entry_at),
        (oid(c1), Some(T - 1000))
    );
    drop(d);

    // HEAD moves on and the reflog's old records are gone: a new engine
    // keeps the recorded base, and the mark.
    let c3 = fx.commit(&[("a.txt", "two\nthree\n")], Some(c2), T + 500);
    fx.head("refs/heads/main", c3, &[("a.txt", "two\nthree\n")]);
    fx.reflog(&[(Some(c2), c3, T + 500, "commit: three")]);
    let d = fx.driven(vec![base_session(&fx)]);
    let (_, s) = open(&d, target("s"));
    let base = s.base.unwrap();
    assert_eq!((base.how, base.commit), (BaseHow::Recorded, oid(c1)));
    assert_eq!(s.seen[0], SeenState::Seen);
}

// ---- claims_* -----------------------------------------------------------

/// B changes f, the user commits an unrelated g, A changes f again.
fn unrelated_commit() -> (Fx, Arc<FakeSessions>) {
    let fx = Fx::new();
    let c0 = fx.commit(&[("f.txt", "a\nb\nc\n"), ("g.txt", "1\n")], None, T - 5000);
    let c1 = fx.commit(
        &[("f.txt", "a\nb\nc\n"), ("g.txt", "2\n")],
        Some(c0),
        T - 3000,
    );
    fx.head(
        "refs/heads/main",
        c1,
        &[("f.txt", "a\nb\nc\n"), ("g.txt", "2\n")],
    );
    fx.reflog(&[
        (None, c0, T - 5000, "commit (initial): start"),
        (Some(c0), c1, T - 3000, "commit: g"),
    ]);
    fx.write("f.txt", "A\nb\nC\n");
    fx.write("g.txt", "2\n");
    let root = fx.root();
    let sessions = fx.sessions(vec![
        session(
            "b",
            &root,
            ms(T - 4000),
            vec![turn(
                "bt",
                ms(T - 3950),
                vec![edit(
                    "b1",
                    ms(T - 3900),
                    vec![replace("f.txt", "a\n", "A\n")],
                )],
            )],
        ),
        session(
            "a",
            &root,
            ms(T - 2000),
            vec![turn(
                "at",
                ms(T - 1950),
                vec![edit(
                    "a1",
                    ms(T - 1900),
                    vec![replace("f.txt", "c\n", "C\n")],
                )],
            )],
        ),
    ]);
    (fx, sessions)
}

#[test]
fn claims_survive_unrelated_commit() {
    let (fx, sessions) = unrelated_commit();
    let d = fx.driven(vec![sessions]);
    let (a, _) = open(&d, target("a"));
    // B's step is still in f's chain, A's `Replace` applies, B's line is
    // `others` in A's review, nothing is Drifted.
    insta::assert_snapshot!("claims_survive_unrelated_commit", render(&d, a));
    let (b, _) = open(&d, target("b"));
    let s = d.snapshot(b).unwrap();
    assert_eq!(paths(&s), ["f.txt"]);
    let others = &s.files[0].attribution.other_sessions;
    assert_eq!(others.iter().map(|k| &*k.id).collect::<Vec<_>>(), ["a"]);
}

#[test]
fn claims_survive_checkout_b() {
    let fx = Fx::new();
    let c0 = fx.commit(&[("f.txt", "f\n"), ("g.txt", "g\n")], None, T - 5000);
    // `git checkout -b feature`: old = new, HEAD on another branch.
    fx.head(
        "refs/heads/feature",
        c0,
        &[("f.txt", "f\n"), ("g.txt", "g\n")],
    );
    fx.reflog(&[
        (None, c0, T - 5000, "commit (initial): start"),
        (
            Some(c0),
            c0,
            T - 2000,
            "checkout: moving from main to feature",
        ),
    ]);
    fx.write("f.txt", "f2\n");
    fx.write("g.txt", "g2\n");
    fx.write("h.txt", "manual\n");
    let root = fx.root();
    let sessions = fx.sessions(vec![
        session(
            "a",
            &root,
            ms(T - 4000),
            vec![turn(
                "at",
                ms(T - 3990),
                vec![edit(
                    "a1",
                    ms(T - 3980),
                    vec![replace("f.txt", "f\n", "f2\n")],
                )],
            )],
        ),
        session(
            "b",
            &root,
            ms(T - 3500),
            vec![turn(
                "bt",
                ms(T - 3490),
                vec![edit(
                    "b1",
                    ms(T - 3480),
                    vec![replace("g.txt", "g\n", "g2\n")],
                )],
            )],
        ),
    ]);
    let d = fx.driven(vec![sessions]);
    let (_, a) = open(&d, target("a"));
    assert_eq!(paths(&a), ["f.txt"]);
    assert_eq!(a.unclaimed, 2);
    assert_eq!(a.files[0].attribution.state, AttributionState::Lines);
    let (_, b) = open(&d, target("b"));
    assert_eq!(paths(&b), ["g.txt"]);
    let (_, outside) = open(&d, d.outside());
    assert_eq!(paths(&outside), ["h.txt"]);
    assert_eq!(outside.files[0].attribution.state, AttributionState::None);
    // A range has every file, file-level, with the sessions that claimed it.
    let (_, range) = open(&d, d.range(lectern_services::changes::BaseSpec::Head));
    assert_eq!(paths(&range), ["f.txt", "g.txt", "h.txt"]);
    let claimed: Vec<(AttributionState, Vec<&str>)> = range
        .files
        .iter()
        .map(|f| {
            let a = &f.attribution;
            (a.state, a.other_sessions.iter().map(|k| &*k.id).collect())
        })
        .collect();
    assert_eq!(
        claimed,
        [
            (AttributionState::FileOnly, vec!["a"]),
            (AttributionState::FileOnly, vec!["b"]),
            (AttributionState::None, vec![]),
        ]
    );
}

#[test]
fn claims_after_branch_switch() {
    let fx = Fx::new();
    let f0 = fx.commit(&[("f.txt", "feature\n")], None, T - 21 * 86_400);
    let m0 = fx.commit(&[("f.txt", "m\n")], None, T - 10_000);
    let m1 = fx.commit(&[("f.txt", "m2\n")], Some(m0), T - 4000);
    fx.head("refs/heads/feature", f0, &[("f.txt", "feature\n")]);
    fx.repo.set_ref("refs/heads/main", m1);
    fx.reflog(&[
        (None, m0, T - 10_000, "commit (initial): main"),
        (Some(m0), m1, T - 4000, "commit: m2"),
        (
            Some(m1),
            f0,
            T - 3000,
            "checkout: moving from main to feature",
        ),
    ]);
    fx.write("f.txt", "feature\nmanual\n");
    let sessions = fx.sessions(vec![session(
        "b",
        &fx.root(),
        ms(T - 5000),
        vec![turn(
            "bt",
            ms(T - 4950),
            vec![edit(
                "b1",
                ms(T - 4900),
                vec![replace("f.txt", "m\n", "m2\n")],
            )],
        )],
    )]);
    let d = fx.driven(vec![sessions]);
    // B's step is not in f's chain: f is not B's, it is outside any session.
    let (_, b) = open(&d, target("b"));
    assert!(b.files.is_empty(), "{:?}", paths(&b));
    assert_eq!(b.unclaimed, 1);
    let (_, outside) = open(&d, d.outside());
    assert_eq!(paths(&outside), ["f.txt"]);
}

/// Session "s", started at T - 4000, replacing `(old, new)` lines of f in
/// one turn, one call per pair at the given times.
fn agent(fx: &Fx, steps: &[(i64, &str, &str)]) -> Arc<FakeSessions> {
    let items = steps
        .iter()
        .enumerate()
        .map(|(n, (at, old, new))| {
            edit(&format!("e{n}"), ms(*at), vec![replace("f.txt", old, new)])
        })
        .collect();
    fx.sessions(vec![session(
        "s",
        &fx.root(),
        ms(T - 4000),
        vec![turn("t", ms(T - 3990), items)],
    )])
}

/// The steps of f in the agent's review: every one of them, in the chain.
fn agent_steps(d: &Driven) -> (Arc<ReviewSnapshot>, usize) {
    let (id, s) = open(d, target("s"));
    let work = d.engine.work(id).unwrap();
    assert_eq!(paths(&s), ["f.txt"], "f is the agent's");
    (s, work.files[0].history.len())
}

#[test]
fn claims_survive_pull_rebase_after_an_agent_commit() {
    let fx = Fx::new();
    let a = fx.commit(&[("f.txt", "a\nb\n"), ("g.txt", "g\n")], None, T - 5000);
    // The agent commits its first change; upstream changed f and g.
    let c = fx.commit(&[("f.txt", "A\nb\n"), ("g.txt", "g\n")], Some(a), T - 3800);
    let u = fx.commit(&[("f.txt", "a\nb\n"), ("g.txt", "g2\n")], Some(a), T - 3700);
    // `git pull --rebase`: HEAD to upstream, the agent's commit picked on it.
    let picked = fx.commit(&[("f.txt", "A\nb\n"), ("g.txt", "g2\n")], Some(u), T - 3500);
    fx.head(
        "refs/heads/main",
        picked,
        &[("f.txt", "A\nb\n"), ("g.txt", "g2\n")],
    );
    fx.reflog(&[
        (None, a, T - 5000, "commit (initial): start"),
        (Some(a), c, T - 3800, "commit: A"),
        (Some(c), u, T - 3500, "pull --rebase (start): checkout u"),
        (Some(u), picked, T - 3500, "pull --rebase (pick): A"),
        (
            Some(picked),
            picked,
            T - 3500,
            "pull --rebase (finish): returning to refs/heads/main",
        ),
    ]);
    fx.write("f.txt", "A\nB\n");
    fx.write("g.txt", "g2\n");
    let d = fx.driven(vec![agent(
        &fx,
        &[(T - 3900, "a\n", "A\n"), (T - 3000, "b\n", "B\n")],
    )]);
    let (s, steps) = agent_steps(&d);
    assert_eq!(steps, 2, "both of the agent's steps stay in f's chain");
    let f = &s.files[0].attribution;
    assert_eq!((f.state, f.drifted_hunks), (AttributionState::Lines, 0));
    assert_eq!(s.unclaimed, 1, "g came from upstream");
}

#[test]
fn claims_survive_a_squash_by_reset_soft() {
    let fx = Fx::new();
    let a = fx.commit(&[("f.txt", "a\nb\nc\n")], None, T - 5000);
    let c1 = fx.commit(&[("f.txt", "A\nb\nc\n")], Some(a), T - 3800);
    let c2 = fx.commit(&[("f.txt", "A\nB\nc\n")], Some(c1), T - 3600);
    // `git reset --soft A && git commit`: one commit for both.
    let squash = fx.commit(&[("f.txt", "A\nB\nc\n")], Some(a), T - 3400);
    fx.head("refs/heads/main", squash, &[("f.txt", "A\nB\nc\n")]);
    fx.reflog(&[
        (None, a, T - 5000, "commit (initial): start"),
        (Some(a), c1, T - 3800, "commit: A"),
        (Some(c1), c2, T - 3600, "commit: B"),
        (Some(c2), a, T - 3500, "reset: moving to a"),
        (Some(a), squash, T - 3400, "commit: A and B"),
    ]);
    fx.write("f.txt", "A\nB\nC\n");
    let d = fx.driven(vec![agent(
        &fx,
        &[
            (T - 3900, "a\n", "A\n"),
            (T - 3700, "b\n", "B\n"),
            (T - 3000, "c\n", "C\n"),
        ],
    )]);
    let (s, steps) = agent_steps(&d);
    assert_eq!(steps, 3);
    let f = &s.files[0].attribution;
    assert_eq!((f.state, f.drifted_hunks), (AttributionState::Lines, 0));
}

#[test]
fn claims_survive_merging_main_into_the_agent_branch() {
    let fx = Fx::new();
    let a = fx.commit(&[("f.txt", "a\nb\nc\n"), ("g.txt", "g\n")], None, T - 5000);
    let c = fx.commit(
        &[("f.txt", "A\nb\nc\n"), ("g.txt", "g\n")],
        Some(a),
        T - 3800,
    );
    // main changed f's last line and g meanwhile; `git merge main`.
    let m1 = fx.commit(
        &[("f.txt", "a\nb\nc2\n"), ("g.txt", "g2\n")],
        Some(a),
        T - 3700,
    );
    let merge = fx.repo.commit_at(
        fx.repo
            .tree(&[("f.txt", b"A\nb\nc2\n"), ("g.txt", b"g2\n")]),
        &[c, m1],
        "merge",
        T - 3500,
    );
    fx.repo.set_ref("refs/heads/main", m1);
    fx.head(
        "refs/heads/agent",
        merge,
        &[("f.txt", "A\nb\nc2\n"), ("g.txt", "g2\n")],
    );
    fx.reflog(&[
        (None, a, T - 5000, "commit (initial): start"),
        (Some(a), c, T - 3800, "commit: A"),
        (
            Some(c),
            merge,
            T - 3500,
            "merge main: Merge made by the 'ort' strategy.",
        ),
    ]);
    fx.write("f.txt", "A\nB\nc2\n");
    fx.write("g.txt", "g2\n");
    let d = fx.driven(vec![agent(
        &fx,
        &[(T - 3900, "a\n", "A\n"), (T - 3000, "b\n", "B\n")],
    )]);
    let (s, steps) = agent_steps(&d);
    assert_eq!(steps, 2, "the merge does not cut the agent's steps");
    let f = &s.files[0].attribution;
    assert_eq!(f.state, AttributionState::Lines);
    assert_eq!(f.turns.len(), 1);
}

#[test]
fn claims_committed_changes_are_not_applied_again() {
    let fx = Fx::new();
    let c0 = fx.commit(&[("f.txt", "a\nb\n")], None, T - 5000);
    let c1 = fx.commit(&[("f.txt", "A\nb\n")], Some(c0), T - 2000);
    fx.head("refs/heads/main", c1, &[("f.txt", "A\nb\n")]);
    fx.reflog(&[
        (None, c0, T - 5000, "commit (initial): start"),
        (Some(c0), c1, T - 2000, "commit: A"),
    ]);
    fx.write("f.txt", "A\nB\n");
    let root = fx.root();
    let sessions = fx.sessions(vec![
        // A's change was committed (by the user) before B started.
        session(
            "a",
            &root,
            ms(T - 3000),
            vec![turn(
                "at",
                ms(T - 2990),
                vec![edit(
                    "a1",
                    ms(T - 2980),
                    vec![replace("f.txt", "a\n", "A\n")],
                )],
            )],
        ),
        session(
            "b",
            &root,
            ms(T - 1000),
            vec![turn(
                "bt",
                ms(T - 990),
                vec![edit(
                    "b1",
                    ms(T - 980),
                    vec![replace("f.txt", "b\n", "B\n")],
                )],
            )],
        ),
    ]);
    let d = fx.driven(vec![Arc::clone(&sessions)]);
    let (id, b) = open(&d, target("b"));
    assert_eq!(b.files[0].attribution.drifted_hunks, 0);
    assert!(b.files[0].attribution.other_sessions.is_empty());
    let work = d.engine.work(id).unwrap();
    assert_eq!(work.files[0].history.len(), 1, "A's step is cut off");
    // Without a reflog every path is cut off at the base commit's time.
    std::fs::remove_file(fx.repo.common_dir().join("logs/HEAD")).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let d = fx.driven_in(dir.path(), vec![sessions]);
    let (id, b) = open(&d, target("b"));
    assert_eq!(b.base.unwrap().how, BaseHow::CurrentHead);
    assert_eq!(d.engine.work(id).unwrap().files[0].history.len(), 1);
}

#[test]
fn claims_old_sessions_do_not_take_part() {
    let fx = Fx::new();
    let c0 = fx.commit(&[("f.txt", "a\n")], None, T - 30 * 86_400);
    fx.head("refs/heads/main", c0, &[("f.txt", "a\n")]);
    fx.reflog(&[(None, c0, T - 30 * 86_400, "commit (initial): start")]);
    fx.write("f.txt", "A\n");
    let old = ms(T - 20 * 86_400);
    let sessions = fx.sessions(vec![session(
        "old",
        &fx.root(),
        old,
        vec![turn(
            "t",
            old,
            vec![edit("o1", old + 10, vec![replace("f.txt", "a\n", "A\n")])],
        )],
    )]);
    let d = fx.driven(vec![sessions]);
    let (_, outside) = open(&d, d.outside());
    assert_eq!(paths(&outside), ["f.txt"]);
}

#[test]
fn claims_only_command_claims_are_file_level() {
    let fx = Fx::new();
    let c0 = fx.commit(&[("f.txt", "a\n"), ("h.txt", "x\n")], None, T - 5000);
    fx.head("refs/heads/main", c0, &[("f.txt", "a\n"), ("h.txt", "x\n")]);
    fx.reflog(&[(None, c0, T - 5000, "commit (initial): start")]);
    fx.write("f.txt", "A\n");
    fx.write("h.txt", "formatted\n");
    fx.mtime("h.txt", ms(T - 1500) + 300);
    let sessions = fx.sessions(vec![session(
        "a",
        &fx.root(),
        ms(T - 2000),
        vec![turn(
            "t",
            ms(T - 1990),
            vec![
                edit("a1", ms(T - 1980), vec![replace("f.txt", "a\n", "A\n")]),
                run("fmt", ms(T - 1500), ms(T - 1400)),
            ],
        )],
    )]);
    let d = fx.driven(vec![sessions]);
    let (id, s) = open(&d, target("a"));
    assert_eq!(paths(&s), ["f.txt", "h.txt"]);
    insta::assert_snapshot!("claims_only_command_claims", render(&d, id));
}

#[test]
fn claims_command_window_in_two_sessions_claims_nothing() {
    let fx = Fx::new();
    let c0 = fx.commit(&[("h.txt", "x\n")], None, T - 5000);
    fx.head("refs/heads/main", c0, &[("h.txt", "x\n")]);
    fx.reflog(&[(None, c0, T - 5000, "commit (initial): start")]);
    fx.write("h.txt", "formatted\n");
    fx.mtime("h.txt", ms(T - 1500) + 300);
    let root = fx.root();
    let sessions = fx.sessions(vec![
        session(
            "a",
            &root,
            ms(T - 2000),
            vec![turn(
                "t",
                ms(T - 1990),
                vec![run("fa", ms(T - 1500), ms(T - 1400))],
            )],
        ),
        session(
            "b",
            &root,
            ms(T - 1800),
            vec![turn(
                "t",
                ms(T - 1790),
                vec![run("fb", ms(T - 1499), ms(T - 1300))],
            )],
        ),
    ]);
    let d = fx.driven(vec![sessions]);
    let (_, a) = open(&d, target("a"));
    assert!(a.files.is_empty());
    let (_, outside) = open(&d, d.outside());
    assert_eq!(paths(&outside), ["h.txt"]);
}

#[test]
fn claims_tree_changes_cached_and_finished_in_the_background() {
    let (fx, sessions) = unrelated_commit();
    // No interactive budget: the first snapshot cuts every path at the
    // newest record (conservative), the background computes the rest.
    let d = Driven::with_budget(
        Arc::clone(&fx.projects),
        vec![Arc::clone(&sessions)],
        fx.dir.path(),
        NOW,
        Duration::ZERO,
    );
    d.capture_snapshots();
    let id = d.open(target("a"));
    d.run();
    // The first snapshot was provisional: f cut at the newest record, B's
    // step out of the chain and B's line Drifted.
    let snapshots = d.take_snapshots();
    assert!(snapshots.len() >= 2);
    let first = &snapshots[0].files[0].attribution;
    assert_eq!((first.drifted_hunks, first.other_sessions.len()), (1, 0));
    let settled = render(&d, id);
    let (fresh, _) = {
        let dir = tempfile::tempdir().unwrap();
        let other = fx.driven_in(dir.path(), vec![Arc::clone(&sessions)]);
        let (id, _) = open(&other, target("a"));
        (render(&other, id), dir)
    };
    assert_eq!(settled, fresh, "the same as computing it at once");
    // The pairs are in `review.tree_changes` now: built again with no
    // budget at all, nothing is left unknown, so nothing was computed.
    let work = d.engine.work(id).unwrap();
    assert!(work.pending.is_empty(), "the last round had it all");
    let reflog = work.git.worktree.head_reflog(0, 100).unwrap();
    let (records, _) = crate::cutoff::window(&reflog.entries, 0, 0, &mut |_| false);
    assert_eq!(records.len(), 2);
    let partition = d.engine.cfg.cache.repo(work.git.common_dir());
    let paths = [path("f.txt")].into_iter().collect();
    let table = crate::cutoff::build(
        &work.git.repo,
        &partition,
        &records,
        paths,
        Some(Duration::ZERO),
        &std::sync::atomic::AtomicBool::new(false),
    );
    assert!(table.unknown(&records).is_empty());
}

#[test]
fn claims_others_first_snapshot_does_not_wait() {
    let (fx, sessions) = unrelated_commit();
    let d = fx.driven(vec![Arc::clone(&sessions)]);
    d.capture_snapshots();
    let id = d.open(target("a"));
    d.run();
    // B's claims were not cached: the first snapshot shows B's line as
    // Drifted, the background reads B's index and fixes it.
    let snapshots = d.take_snapshots();
    assert!(snapshots.len() >= 2);
    let first = &snapshots[0].files[0].attribution;
    assert_eq!((first.drifted_hunks, first.other_sessions.len()), (1, 0));
    let s = d.snapshot(id).unwrap();
    assert_eq!(s.files[0].attribution.drifted_hunks, 0);
    assert_eq!(&*s.files[0].attribution.other_sessions[0].id, "b");
    let b = sessions.calls("b");
    assert_eq!(b.turns, 0, "B's index is never kept in the agents' LRU");
    assert_eq!(b.turns_unretained, 1);
}

#[test]
fn claims_warm_open_uses_cached_steps() {
    let (fx, sessions) = unrelated_commit();
    let d = fx.driven(vec![Arc::clone(&sessions)]);
    let (_, cold) = open(&d, target("a"));
    let cold = render(&d, cold.id);
    d.close_cache();
    drop(d);
    sessions.reset_calls();
    // A new run on the same cache: B's claims come from `review.claims`,
    // the chain from `review.attribution`.
    let d = fx.driven(vec![Arc::clone(&sessions)]);
    let id = d.open(target("a"));
    d.run();
    // The first round left nothing for the background (the second
    // snapshot is the log's seen states), and gave the cold result.
    assert!(d.engine.work(id).unwrap().pending.is_empty());
    assert_eq!(render(&d, id), cold);
    let b = sessions.calls("b");
    assert_eq!((b.turns, b.turns_unretained, b.load), (0, 0, 0));
    d.close_cache();
    drop(d);

    // B has new activity: its entry does not stand.
    sessions.touch("b", Timestamp(ms(T - 1000)));
    sessions.reset_calls();
    let d = fx.driven(vec![Arc::clone(&sessions)]);
    open(&d, target("a"));
    assert_eq!(sessions.calls("b").turns_unretained, 1);
    d.close_cache();
    drop(d);

    // Another adapter version: neither does any entry of the source.
    sessions.set_adapter_version(2);
    sessions.reset_calls();
    let d = fx.driven(vec![Arc::clone(&sessions)]);
    open(&d, target("a"));
    assert_eq!(sessions.calls("b").turns_unretained, 1);
}

#[test]
fn claims_unread_source_is_used_then_verified() {
    let (fx, _) = unrelated_commit();
    let root = fx.root();
    // B is of another source, which A's open does not read.
    let a = fx.sessions(vec![session(
        "a",
        &root,
        ms(T - 2000),
        vec![turn(
            "at",
            ms(T - 1950),
            vec![edit(
                "a1",
                ms(T - 1900),
                vec![replace("f.txt", "c\n", "C\n")],
            )],
        )],
    )]);
    let script = |adapter: u32| {
        json!({ "adapter": adapter, "sessions": [session("b", &root, ms(T - 4000), vec![turn("bt", ms(T - 3950), vec![
            edit("b1", ms(T - 3900), vec![replace("f.txt", "a\n", "A\n")]),
        ])])] })
        .to_string()
    };
    let b = Arc::new(FakeSessions::from_script("session-codex", &script(1)));
    let d = fx.driven(vec![Arc::clone(&a), Arc::clone(&b)]);
    open(&d, target("a"));
    d.close_cache();
    drop(d);
    // The cache has B's claims of version 1; the source is at 2 now.
    b.set_adapter_version(2);
    b.reset_calls();
    let d = fx.driven(vec![Arc::clone(&a), Arc::clone(&b)]);
    let id = d.open(target("a"));
    d.run();
    let events = d.take_events();
    assert!(events.len() >= 2, "used first, recomputed after the check");
    assert!(b.calls("b").turns_unretained >= 1);
    let s = d.snapshot(id).unwrap();
    assert_eq!(&*s.files[0].attribution.other_sessions[0].id, "b");
    assert_eq!(s.files[0].attribution.drifted_hunks, 0);
}

// ---- files_* --------------------------------------------------------------

#[test]
fn files_of_each_session_and_looks_good() {
    let fx = Fx::new();
    let c0 = fx.commit(
        &[("a.txt", "a\n"), ("b.txt", "b\n"), ("m.txt", "m\n")],
        None,
        T - 5000,
    );
    fx.head(
        "refs/heads/main",
        c0,
        &[("a.txt", "a\n"), ("b.txt", "b\n"), ("m.txt", "m\n")],
    );
    fx.reflog(&[(None, c0, T - 5000, "commit (initial): start")]);
    fx.write("a.txt", "A\n");
    fx.write("b.txt", "B\n");
    fx.write("m.txt", "manual\n");
    let root = fx.root();
    let sessions = fx.sessions(vec![
        session(
            "a",
            &root,
            ms(T - 3000),
            vec![turn(
                "at",
                ms(T - 2990),
                vec![edit(
                    "a1",
                    ms(T - 2980),
                    vec![replace("a.txt", "a\n", "A\n")],
                )],
            )],
        ),
        session(
            "b",
            &root,
            ms(T - 2000),
            vec![turn(
                "bt",
                ms(T - 1990),
                vec![edit("b1", ms(T - 1980), vec![write("b.txt", "B\n")])],
            )],
        ),
    ]);
    let d = fx.driven(vec![sessions]);
    let (a, sa) = open(&d, target("a"));
    assert_eq!((paths(&sa), sa.unclaimed), (vec!["a.txt".to_string()], 2));
    let (_, sb) = open(&d, target("b"));
    assert_eq!((paths(&sb), sb.unclaimed), (vec!["b.txt".to_string()], 2));
    d.command(a, ReviewCommand::LooksGood);
    let sa = d.snapshot(a).unwrap();
    assert_eq!(sa.progress.seen, 1);
    assert_eq!(sa.progress.total, 1);
    d.run();
    // B's file and the manual one were not marked.
    let (_, outside) = open(&d, d.outside());
    assert_eq!(paths(&outside), ["m.txt"]);
    assert_eq!(outside.seen[0], SeenState::Unseen);
    assert_eq!(d.snapshot(sb.id).unwrap().seen[0], SeenState::Unseen);
}

#[test]
fn files_unrecognized_session_claims_by_mtime() {
    let fx = Fx::new();
    let c0 = fx.commit(&[("x.txt", "x\n"), ("y.txt", "y\n")], None, T - 5000);
    fx.head("refs/heads/main", c0, &[("x.txt", "x\n"), ("y.txt", "y\n")]);
    fx.reflog(&[(None, c0, T - 5000, "commit (initial): start")]);
    fx.write("x.txt", "X\n");
    fx.write("y.txt", "Y\n");
    fx.mtime("x.txt", ms(T - 500));
    fx.mtime("y.txt", ms(T - 5000));
    let mut s = session("u", &fx.root(), ms(T - 1000), vec![]);
    s["unrecognized"] = json!(true);
    s["last_activity"] = json!(ms(T));
    let d = fx.driven(vec![fx.sessions(vec![s])]);
    let (_, snap) = open(&d, target("u"));
    assert_eq!(paths(&snap), ["x.txt"]);
    assert!(snap.turns.is_none());
    assert_eq!(
        snap.degraded,
        Some(lectern_services::review::Degraded::UnrecognizedSession)
    );
    assert_eq!(snap.files[0].attribution.state, AttributionState::FileOnly);
}

// ---- narrative_* ------------------------------------------------------------

#[test]
fn narrative_merges_calls_and_places_subagents() {
    let fx = Fx::new();
    let c0 = fx.commit(&[("f.txt", "1\n2\n3\n"), ("g.txt", "g\n")], None, T - 5000);
    fx.head(
        "refs/heads/main",
        c0,
        &[("f.txt", "1\n2\n3\n"), ("g.txt", "g\n")],
    );
    fx.reflog(&[(None, c0, T - 5000, "commit (initial): start")]);
    fx.write("f.txt", "one\n2\nthree\n");
    fx.write("g.txt", "G\n");
    fx.write("n.txt", "new\n");
    let root = fx.root();
    let mut child = session(
        "p/agent-1",
        &root,
        ms(T - 2800),
        vec![turn(
            "ct",
            ms(T - 2790),
            vec![edit(
                "k1",
                ms(T - 2780),
                vec![replace("g.txt", "g\n", "G\n")],
            )],
        )],
    );
    child["parent"] = json!("p");
    child["spawned_by"] = json!("sp1");
    // A subagent without its call: placed by time, after turn 2.
    let mut late = session(
        "p/agent-2",
        &root,
        ms(T - 1500),
        vec![turn(
            "lt",
            ms(T - 1490),
            vec![edit("l1", ms(T - 1480), vec![write("n.txt", "new\n")])],
        )],
    );
    late["parent"] = json!("p");
    let parent = session(
        "p",
        &root,
        ms(T - 3000),
        vec![
            turn(
                "t1",
                ms(T - 2990),
                vec![
                    json!({ "text": "Changing f." }),
                    edit("e1", ms(T - 2980), vec![replace("f.txt", "1\n", "one\n")]),
                    json!({ "read": "r1", "paths": ["g.txt"], "at": ms(T - 2970), "done": ms(T - 2969) }),
                    edit("e2", ms(T - 2960), vec![replace("f.txt", "3\n", "three\n")]),
                    json!({ "spawn": "sp1", "at": ms(T - 2900) }),
                    json!({ "tool": "w1", "name": "WebFetch", "at": ms(T - 2850), "done": ms(T - 2840) }),
                ],
            ),
            turn(
                "t2",
                ms(T - 2000),
                vec![json!({ "injected": "background task done" })],
            ),
        ],
    );
    let d = fx.driven(vec![fx.sessions(vec![parent, child, late])]);
    let (id, s) = open(&d, target("p"));
    assert_eq!(s.sessions.len(), 3);
    assert_eq!(
        s.sessions[1]
            .spawned_by
            .as_ref()
            .map(|(t, c)| (t.ordinal, &*c.0)),
        Some((1, "sp1"))
    );
    assert!(s.sessions[2].spawned_by.is_none());
    // Two calls on f in turn 1 are one step, at the first; the subagent's
    // turn follows the turn that spawned it, the other one by time.
    insta::assert_snapshot!(
        "narrative_merges_calls_and_places_subagents",
        render(&d, id)
    );
}

#[test]
fn narrative_after_last_seen() {
    let (fx, c1, c2) = base_fixture();
    fx.reflog(&[
        (None, c1, T - 1000, "commit (initial): one"),
        (Some(c1), c2, T + 100, "commit: two"),
    ]);
    let sessions = fx.sessions(vec![session(
        "s",
        &fx.root(),
        ms(T),
        vec![
            turn(
                "t1",
                ms(T + 10),
                vec![edit(
                    "c1",
                    ms(T + 20),
                    vec![replace("a.txt", "one\n", "two\n")],
                )],
            ),
            turn(
                "t2",
                ms(T + 300),
                vec![edit(
                    "c2",
                    ms(T + 310),
                    vec![replace("a.txt", "two\n", "two\nthree\n")],
                )],
            ),
        ],
    )]);
    let d = fx.driven(vec![sessions]);
    let (id, _) = open(&d, target("s"));
    d.set_now(Timestamp(ms(T + 200)));
    d.command(id, ReviewCommand::MarkSeen(modified("a.txt")));
    let turns = d.snapshot(id).unwrap().turns.clone().unwrap();
    assert_eq!(
        turns.iter().map(|t| t.after_last_seen).collect::<Vec<_>>(),
        [false, true]
    );
}

// ---- rows_* -------------------------------------------------------------

#[test]
fn rows_final_and_step_rows() {
    let (fx, sessions) = unrelated_commit();
    let d = fx.driven(vec![sessions]);
    let (id, s) = open(&d, target("a"));
    let step = s.turns.as_ref().unwrap()[0]
        .items
        .iter()
        .find_map(|i| match i {
            NarrativeItem::Step(st) => Some(st.step),
            _ => None,
        })
        .unwrap();
    let final_key = RowsKey::Final {
        file: modified("f.txt"),
        context: 3,
    };
    let step_key = RowsKey::Step { step, context: 3 };
    d.command(
        id,
        ReviewCommand::Prepare(
            vec![Need::Rows(final_key.clone()), Need::Rows(step_key.clone())].into(),
        ),
    );
    assert!(d.rows(id, &final_key).is_none(), "prepared on a task");
    d.run();
    let rows = d.rows(id, &final_key).unwrap();
    let text = |rows: &lectern_services::review::FileRows| {
        let mut out = String::new();
        for r in rows.rows.iter() {
            writeln!(out, "{r:?}").unwrap();
        }
        for h in rows.hunks.iter() {
            writeln!(
                out,
                "hunk {:?} {:?} turns {:?} others {:?}",
                h.old,
                h.new,
                h.attribution.turns,
                h.attribution.others.len()
            )
            .unwrap();
        }
        writeln!(out, "inline {}", rows.inline.len()).unwrap();
        out
    };
    // Offsets as git's rows give them: "A\nb\nC\n" against "a\nb\nc\n".
    assert!(matches!(
        rows.rows[0],
        DiffRow::Removed {
            old: 0,
            old_byte: Some(0),
            hunk: 0,
            ..
        }
    ));
    insta::assert_snapshot!("rows_final", text(&rows));
    // The step: the version before A's turn (B's) to the one after it.
    let rows = d.rows(id, &step_key).unwrap();
    let work = d.engine.work(id).unwrap();
    let results = work.results.lock().unwrap().clone();
    let chain = results[0].as_ref().unwrap().chain.clone().unwrap();
    assert_eq!(rows.new.oid, chain.steps[1].version, "replayed, same OID");
    assert_eq!(rows.old.oid, chain.steps[0].version);
    assert!(d.version_bytes() > 0 && d.version_bytes() <= crate::lru::MAX_BYTES);
    insta::assert_snapshot!("rows_step", text(&rows));
    // A new request drops what it does not name.
    d.command(
        id,
        ReviewCommand::Prepare(vec![Need::Rows(step_key.clone())].into()),
    );
    assert!(d.rows(id, &final_key).is_none());
    assert!(d.rows(id, &step_key).is_some());
}

#[test]
fn rows_prepared_again_after_a_refresh() {
    let (fx, sessions) = unrelated_commit();
    let d = fx.driven(vec![sessions]);
    let (id, _) = open(&d, target("a"));
    let key = RowsKey::Final {
        file: modified("f.txt"),
        context: 3,
    };
    let prepare = || {
        d.command(
            id,
            ReviewCommand::Prepare(vec![Need::Rows(key.clone())].into()),
        )
    };
    prepare();
    d.run();
    let before = d.rows(id, &key).unwrap();
    // The file changes by hand, the user refreshes.
    fx.write("f.txt", "A\nb\nC\nD\n");
    d.command(id, ReviewCommand::Refresh);
    d.run();
    // The new work's install prepared the request again.
    let again = d.rows(id, &key).unwrap();
    assert_ne!(again.rows, before.rows);
    assert_eq!(again.hunks.len(), 2);
    assert_eq!(again.hunks[1].attribution.drifted, 1, "D has no source");
    // The view sending the same request again gets the new rows too.
    prepare();
    d.run();
    assert_eq!(d.rows(id, &key).unwrap().rows, again.rows);
}

#[test]
fn rows_of_a_provisional_round_are_replaced() {
    let (fx, sessions) = unrelated_commit();
    let d = fx.driven(vec![sessions]);
    let key = RowsKey::Final {
        file: modified("f.txt"),
        context: 3,
    };
    // Asked for before the first snapshot: prepared once the first round
    // is in, when B's claims are not known yet, and again after the
    // background's round with them.
    let id = d.open(target("a"));
    d.command(
        id,
        ReviewCommand::Prepare(vec![Need::Rows(key.clone())].into()),
    );
    d.run();
    let prepared = d
        .take_events()
        .iter()
        .filter(|(e, _)| matches!(e, lectern_services::review::ReviewEvent::Prepared { .. }))
        .count();
    assert!(prepared >= 2, "{prepared}");
    let rows = d.rows(id, &key).unwrap();
    let first = &rows.hunks[0].attribution;
    assert_eq!((first.others.len(), first.drifted), (1, 0), "B's line");
}

#[test]
fn rows_text_of_a_prompt() {
    let (fx, sessions) = unrelated_commit();
    let d = fx.driven(vec![sessions]);
    let (id, s) = open(&d, target("a"));
    let prompt = s.turns.as_ref().unwrap()[0].prompt.unwrap();
    d.command(id, ReviewCommand::Prepare(vec![Need::Text(prompt)].into()));
    d.run();
    assert_eq!(d.text(id, &prompt).as_deref(), Some("prompt at"));
}
