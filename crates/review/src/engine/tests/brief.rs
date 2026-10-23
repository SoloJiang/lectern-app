//! The brief of a review (docs/design/review.md「测试」`brief_*`): the
//! rules from the review's own data, and the `BriefRules` providers
//! (asked once per round, batches, the 10-second timeout, a dropped
//! registration or a dead token, reconciliation). The rules themselves are
//! unit-tested in `brief::tests`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;

use lectern_services::review::{
    BriefItem, BriefReply, BriefRequest, BriefRule, BriefRules, DiffSide, LineAnchor, NoiseKind,
    ProvidedItem, SensitiveKind,
};

use super::*;
use crate::engine::brief::PROVIDER_TIMEOUT;

/// A provider that records what it was asked and replies when the test
/// says.
struct FakeRules {
    id: Arc<str>,
    requests: Mutex<Vec<(BriefRequest, BriefReply)>>,
}

impl FakeRules {
    fn new(id: &str) -> Arc<FakeRules> {
        Arc::new(FakeRules {
            id: Arc::from(id),
            requests: Mutex::new(Vec::new()),
        })
    }

    fn requests(&self) -> usize {
        self.requests.lock().unwrap().len()
    }

    fn request(&self, n: usize) -> BriefRequest {
        self.requests.lock().unwrap()[n].0.clone()
    }

    fn send(&self, n: usize, items: Vec<ProvidedItem>, done: bool) {
        self.requests.lock().unwrap()[n].1.send(items, done);
    }

    fn cancelled(&self, n: usize) -> bool {
        self.requests.lock().unwrap()[n].1.cancelled()
    }
}

impl BriefRules for FakeRules {
    fn id(&self) -> Arc<str> {
        self.id.clone()
    }

    fn evaluate(&self, request: BriefRequest, reply: BriefReply) {
        self.requests.lock().unwrap().push((request, reply));
    }
}

fn item(rule: &str, file: Option<&str>, line: Option<LineAnchor>) -> ProvidedItem {
    ProvidedItem {
        rule: Arc::from(rule),
        critical: false,
        file: file.map(modified),
        line,
        args: Arc::from(Vec::new()),
    }
}

fn rules(s: &ReviewSnapshot) -> Vec<String> {
    s.brief
        .items
        .iter()
        .map(|i| match &i.rule {
            BriefRule::Provided { provider, rule, .. } => format!("{provider}:{rule}"),
            other => format!("{other:?}"),
        })
        .collect()
}

fn provided(s: &ReviewSnapshot) -> Vec<&BriefItem> {
    s.brief
        .items
        .iter()
        .filter(|i| matches!(i.rule, BriefRule::Provided { .. }))
        .collect()
}

/// One commit with `a.txt`, a CI workflow and a lockfile; the session
/// `sid` edits all three in one turn, and the worktree has the edits.
fn fixture(sid: &str) -> (Fx, Arc<FakeSessions>) {
    let fx = Fx::new();
    let files = [
        ("a.txt", "one\n"),
        (".github/workflows/ci.yml", "on: push\n"),
        ("Cargo.lock", "v1\n"),
    ];
    let c1 = fx.commit(&files, None, T - 1000);
    fx.head("refs/heads/main", c1, &files);
    fx.reflog(&[(None, c1, T - 1000, "commit (initial): one")]);
    fx.write("a.txt", "one\ntwo\n");
    fx.write(".github/workflows/ci.yml", "on: push\njobs: {}\n");
    fx.write("Cargo.lock", "v2\n");
    let sessions = fx.sessions(vec![session(
        sid,
        &fx.root(),
        ms(T),
        vec![turn(
            "t1",
            ms(T + 10),
            vec![edit(
                "c1",
                ms(T + 20),
                vec![
                    replace("a.txt", "one\n", "one\ntwo\n"),
                    replace(
                        ".github/workflows/ci.yml",
                        "on: push\n",
                        "on: push\njobs: {}\n",
                    ),
                    replace("Cargo.lock", "v1\n", "v2\n"),
                ],
            )],
        )],
    )]);
    (fx, sessions)
}

fn timed(fx: &Fx, sessions: Arc<FakeSessions>) -> Driven {
    Driven::with_timers(Arc::clone(&fx.projects), vec![sessions], fx.dir.path(), NOW)
}

#[test]
fn brief_rules_come_from_the_review() {
    let (fx, sessions) = fixture("s");
    let d = fx.driven(vec![sessions]);
    let (_, s) = open(&d, target("s"));
    assert_eq!(rules(&s), ["Sensitive(Ci)", "Noise(Lockfile)"], "{s:#?}");
    let ci = &s.brief.items[0];
    assert!(ci.critical);
    assert_eq!(ci.rule, BriefRule::Sensitive(SensitiveKind::Ci));
    assert_eq!(ci.file, Some(modified(".github/workflows/ci.yml")));
    let lock = &s.brief.items[1];
    assert_eq!(lock.rule, BriefRule::Noise(NoiseKind::Lockfile));
    assert_eq!(lock.count, 1);
    assert_eq!(s.brief.pending_providers, 0);
    assert_eq!(s.brief.omitted, 0);
}

#[test]
fn brief_edit_failed_comes_from_a_failed_call_on_a_claimed_file() {
    let fx = Fx::new();
    let c1 = fx.commit(&[("a.txt", "one\n"), ("b.txt", "bee\n")], None, T - 1000);
    fx.head(
        "refs/heads/main",
        c1,
        &[("a.txt", "one\n"), ("b.txt", "bee\n")],
    );
    fx.reflog(&[(None, c1, T - 1000, "commit (initial): one")]);
    fx.write("a.txt", "one\ntwo\n");
    fx.write("b.txt", "bee\n");
    let failed = json!({
        "edit": "c2", "at": ms(T + 30), "done": ms(T + 31), "status": "failed",
        "changes": [replace("a.txt", "two\n", "three\n"), replace("b.txt", "bee\n", "b\n")]
    });
    let sessions = fx.sessions(vec![session(
        "s",
        &fx.root(),
        ms(T),
        vec![turn(
            "t1",
            ms(T + 10),
            vec![
                edit(
                    "c1",
                    ms(T + 20),
                    vec![replace("a.txt", "one\n", "one\ntwo\n")],
                ),
                failed,
            ],
        )],
    )]);
    let d = fx.driven(vec![sessions]);
    let (_, s) = open(&d, target("s"));
    // `b.txt` did not change and is not in the snapshot: no item for it.
    // The failed step is not in `a.txt`'s chain: no Drifted.
    assert_eq!(rules(&s), ["EditFailed"], "{s:#?}");
    assert_eq!(s.brief.items[0].file, Some(modified("a.txt")));
    assert_eq!(s.files.len(), 1);
}

#[test]
fn brief_command_only_comes_from_a_command_claim() {
    let fx = Fx::new();
    let c0 = fx.commit(&[("h.txt", "raw\n")], None, T - 5000);
    fx.head("refs/heads/main", c0, &[("h.txt", "raw\n")]);
    fx.reflog(&[(None, c0, T - 5000, "commit (initial): start")]);
    fx.write("h.txt", "formatted\n");
    fx.mtime("h.txt", ms(T - 1500) + 300);
    let sessions = fx.sessions(vec![session(
        "a",
        &fx.root(),
        ms(T - 2000),
        vec![turn(
            "t",
            ms(T - 1990),
            vec![run("fmt", ms(T - 1500), ms(T - 1400))],
        )],
    )]);
    let d = fx.driven(vec![sessions]);
    let (_, s) = open(&d, target("a"));
    assert_eq!(rules(&s), ["CommandOnly"], "{s:#?}");
    assert_eq!(s.brief.items[0].file, Some(modified("h.txt")));
    assert_eq!(s.brief.items[0].count, 1);
}

#[test]
fn brief_providers_are_asked_once_per_round_and_reply_in_batches() {
    let (fx, sessions) = fixture("s");
    let d = timed(&fx, sessions);
    let host = crate::testing::host();
    let p = FakeRules::new("code-intel");
    let _reg = d.register_brief_rules(p.clone(), host.handle.token());
    let (id, s) = open(&d, target("s"));
    // Several snapshots were published during the round; one request.
    assert_eq!(p.requests(), 1);
    let request = p.request(0);
    assert_eq!(request.worktree, Some(fx.projects.main()));
    // Noise files are not in the request; the others come with their key,
    // path, OIDs and hunks.
    let paths: Vec<&str> = request.files.iter().map(|f| &*f.path).collect();
    assert_eq!(paths, [".github/workflows/ci.yml", "a.txt"], "{request:#?}");
    let a = &request.files[1];
    assert_eq!(a.key, modified("a.txt"));
    assert!(a.old_oid.is_some() && a.new_oid.is_some());
    assert_eq!(a.hunks.len(), 1);
    assert_eq!(s.brief.pending_providers, 1);
    assert!(provided(&s).is_empty());

    // A first batch: shown, still pending.
    let shown = LineAnchor {
        side: DiffSide::New,
        line: 1,
    };
    let hidden = LineAnchor {
        side: DiffSide::New,
        line: 9,
    };
    p.send(
        0,
        vec![
            item("public-api", Some("a.txt"), Some(shown)),
            item("callers", Some("a.txt"), Some(hidden)),
            item("callers", Some("zz.txt"), None),
        ],
        false,
    );
    d.run();
    let s = d.snapshot(id).unwrap();
    assert_eq!(s.brief.pending_providers, 1);
    let items = provided(&s);
    assert_eq!(items.len(), 2, "{s:#?}");
    assert_eq!(items[0].line, Some(shown));
    assert_eq!(
        items[1].line, None,
        "a line outside the hunks is a file note"
    );
    assert!(
        rules(&s).contains(&"code-intel:public-api".to_owned()),
        "{:?}",
        rules(&s)
    );

    // The last batch.
    p.send(0, vec![item("summary", None, None)], true);
    d.run();
    let s = d.snapshot(id).unwrap();
    assert_eq!(s.brief.pending_providers, 0);
    assert_eq!(provided(&s).len(), 3);
    assert!(!p.cancelled(0));

    // A new round: asked again, the earlier reply cancelled, its items
    // gone until the provider answers.
    d.refresh(id, None);
    d.run();
    assert_eq!(p.requests(), 2);
    assert!(p.cancelled(0));
    let s = d.snapshot(id).unwrap();
    assert_eq!(s.brief.pending_providers, 1);
    assert!(provided(&s).is_empty());
    // A late batch of the old request is ignored.
    p.send(0, vec![item("late", None, None)], true);
    d.run();
    let s = d.snapshot(id).unwrap();
    assert!(provided(&s).is_empty());
    assert_eq!(s.brief.pending_providers, 1);
}

#[test]
fn brief_a_silent_provider_counts_as_done_after_ten_seconds() {
    let (fx, sessions) = fixture("s");
    let d = timed(&fx, sessions);
    let host = crate::testing::host();
    let p = FakeRules::new("slow");
    let _reg = d.register_brief_rules(p.clone(), host.handle.token());
    let (id, s) = open(&d, target("s"));
    assert_eq!(s.brief.pending_providers, 1);
    d.advance(PROVIDER_TIMEOUT - Duration::from_millis(1));
    d.run();
    assert_eq!(d.snapshot(id).unwrap().brief.pending_providers, 1);
    d.advance(Duration::from_millis(1));
    d.run();
    let s = d.snapshot(id).unwrap();
    assert_eq!(s.brief.pending_providers, 0);
    // What arrives after the timeout is dropped.
    p.send(0, vec![item("late", None, None)], true);
    d.run();
    assert!(provided(&d.snapshot(id).unwrap()).is_empty());
}

#[test]
fn brief_a_dropped_registration_is_not_called_and_its_items_go() {
    let (fx, sessions) = fixture("s");
    let d = timed(&fx, sessions);
    let host = crate::testing::host();
    let p = FakeRules::new("code-intel");
    let reg = d.register_brief_rules(p.clone(), host.handle.token());
    let (id, _) = open(&d, target("s"));
    p.send(0, vec![item("public-api", Some("a.txt"), None)], true);
    d.run();
    assert_eq!(provided(&d.snapshot(id).unwrap()).len(), 1);
    drop(reg);
    d.run();
    let s = d.snapshot(id).unwrap();
    assert!(provided(&s).is_empty(), "{s:#?}");
    assert_eq!(s.brief.pending_providers, 0);
    d.refresh(id, None);
    d.run();
    assert_eq!(p.requests(), 1, "not asked again");
}

#[test]
fn brief_a_dead_token_is_not_called() {
    let (fx, sessions) = fixture("s");
    let d = timed(&fx, sessions);
    let host = crate::testing::host();
    let p = FakeRules::new("code-intel");
    let _reg = d.register_brief_rules(p.clone(), host.handle.token());
    host.deactivate();
    let (_, s) = open(&d, target("s"));
    assert_eq!(p.requests(), 0);
    assert_eq!(s.brief.pending_providers, 0);
}

#[test]
fn brief_a_provider_registered_later_is_asked_about_open_reviews() {
    let (fx, sessions) = fixture("s");
    let d = timed(&fx, sessions);
    let host = crate::testing::host();
    let (id, s) = open(&d, target("s"));
    assert_eq!(s.brief.pending_providers, 0);
    let p = FakeRules::new("code-intel");
    let _reg = d.register_brief_rules(p.clone(), host.handle.token());
    assert_eq!(p.requests(), 1);
    p.send(0, vec![item("public-api", Some("a.txt"), None)], true);
    d.run();
    assert_eq!(provided(&d.snapshot(id).unwrap()).len(), 1);
}
