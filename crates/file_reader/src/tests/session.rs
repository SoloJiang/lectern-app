//! `session_*`: the queue, the document shown, the history and the focus,
//! driven the way the view will drive them (docs/design/file_reader.md
//! 「本 crate 的公开项」「数据流与线程」).

use std::collections::HashSet;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use lectern_services::code_intel::{Focus, TextOrigin};
use lectern_services::project::ProjectKey;
use lectern_services::reader::{Anchor, OpenOutcome, OpenTicket, Reader, ReaderEvent};
use lectern_syntax::Canonical;
use lectern_syntax::{DocState, RowPos, testing};

use super::rig::{Rig, Tree, file, git_snapshot, lock};
use crate::HISTORY_MAX;

fn shown(ticket: OpenTicket) -> ReaderEvent {
    ReaderEvent::Open {
        ticket,
        outcome: OpenOutcome::Shown,
    }
}

fn cancelled(ticket: OpenTicket) -> ReaderEvent {
    ReaderEvent::Open {
        ticket,
        outcome: OpenOutcome::Cancelled,
    }
}

fn path_of(rig: &Rig) -> String {
    rig.session
        .snapshot()
        .map(|s| s.target.path.to_string())
        .unwrap_or_default()
}

#[test]
fn session_open_queues_and_wakes_and_changes_nothing() {
    let rig = Rig::new();
    let (tree, key) = rig.folder();
    tree.write("a.ts", &testing::ts(2 << 10, 0));
    let ticket = rig.session.open(file(&key, "a.ts", Anchor::Start));
    assert_eq!(rig.session.pending(), 1);
    assert_eq!(rig.woken.load(Ordering::SeqCst), 1);
    assert!(rig.session.snapshot().is_none(), "nothing is opened yet");
    assert!(
        rig.host.events().is_empty(),
        "no event before the view acts"
    );
    assert_eq!(rig.res.executor.run_until_idle(), 0, "no task either");
    let request = rig.session.next_request().expect("the request");
    assert_eq!(request.ticket, ticket);
    assert_eq!(&*request.target.path, "a.ts");
    assert!(rig.session.next_request().is_none());
}

#[test]
fn session_tickets_are_distinct_across_threads() {
    let rig = Rig::new();
    let (_tree, key) = rig.folder();
    let tickets = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..8 {
            scope.spawn(|| {
                let mine: Vec<_> = (0..50)
                    .map(|_| rig.session.open(file(&key, "a.ts", Anchor::Start)))
                    .collect();
                lock(&tickets).extend(mine);
            });
        }
    });
    let tickets = lock(&tickets);
    assert_eq!(tickets.len(), 400);
    assert_eq!(tickets.iter().collect::<HashSet<_>>().len(), 400);
    assert_eq!(rig.session.pending(), 400);
    assert_eq!(rig.woken.load(Ordering::SeqCst), 400);
    // Every queued request is taken once, with its own ticket.
    let taken: HashSet<_> = std::iter::from_fn(|| rig.session.next_request())
        .map(|request| request.ticket)
        .collect();
    assert_eq!(taken, tickets.iter().copied().collect::<HashSet<_>>());
}

#[test]
fn session_a_waker_installed_late_hears_of_queued_requests() {
    let rig = Rig::new();
    rig.session.set_waker(None);
    let (_tree, key) = rig.folder();
    rig.session.open(file(&key, "a.ts", Anchor::Start));
    assert_eq!(rig.woken.load(Ordering::SeqCst), 0);
    let heard = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = heard.clone();
    rig.session.set_waker(Some(Arc::new(move || {
        count.fetch_add(1, Ordering::SeqCst);
    })));
    assert_eq!(heard.load(Ordering::SeqCst), 1);
}

#[test]
fn session_apply_shows_the_document_and_reports_it() {
    let rig = Rig::new();
    let (tree, key) = rig.folder();
    tree.write("a.ts", &testing::ts(4 << 10, 0));
    let ticket = rig.session.open(file(&key, "a.ts", Anchor::Start));
    let request = rig.session.next_request().unwrap();
    let before = rig.woken.load(Ordering::SeqCst);
    assert_eq!(rig.session.apply(request), OpenOutcome::Shown);
    assert_eq!(rig.host.events(), vec![shown(ticket)]);
    rig.res.settle();
    assert!(
        rig.woken.load(Ordering::SeqCst) > before,
        "the document's snapshots wake the view"
    );
    let snapshot = rig.session.snapshot().expect("shown");
    assert_eq!(snapshot.state, DocState::Ready);
    assert_eq!(&*snapshot.target.path, "a.ts");
    assert_eq!(rig.session.history_len(), (0, 0), "nothing was left behind");
}

#[test]
fn session_a_second_open_leaves_a_position_in_the_history() {
    let rig = Rig::new();
    let (tree, key) = rig.folder();
    tree.write("a.ts", &testing::ts(64 << 10, 0));
    tree.write("b.ts", &testing::ts(2 << 10, 1));
    let a = rig.show(file(&key, "a.ts", Anchor::Start));
    // The view scrolled to the middle of the file.
    let top = a.at_fraction(0.5, 100);
    rig.session.scrolled(top);
    rig.show(file(&key, "b.ts", Anchor::Start));
    assert_eq!(path_of(&rig), "b.ts");
    assert_eq!(rig.session.history_len(), (1, 0));
    assert!(rig.session.can_back() && !rig.session.can_forward());

    // Back opens a.ts again where it was left: the OID seen and the byte.
    assert!(rig.session.back());
    rig.res.settle();
    assert_eq!(path_of(&rig), "a.ts");
    let back = rig.session.snapshot().unwrap();
    assert_eq!(back.oid, a.oid, "the content seen is the one found again");
    assert_eq!(rig.session.history_len(), (0, 1));
    let Anchor::Byte { byte, .. } = back.target.at else {
        panic!("a byte anchor: {:?}", back.target.at);
    };
    assert_eq!(byte, top.line_start);
    assert!(top.line_start > 0, "the position is not the start");

    // Forward goes to b.ts, and back again.
    assert!(rig.session.forward());
    assert_eq!(path_of(&rig), "b.ts");
    assert_eq!(rig.session.history_len(), (1, 0));
    assert!(!rig.session.forward(), "nothing ahead");
    assert_eq!(path_of(&rig), "b.ts");
    // Going back and forward emit no `ReaderEvent`: only opens do.
    assert_eq!(rig.host.events().len(), 2);
}

#[test]
fn session_history_holds_origin_oid_and_position_never_content() {
    let rig = Rig::new();
    let (tree, key) = rig.folder();
    tree.write("a.ts", &testing::ts(2 << 10, 0));
    tree.write("b.ts", &testing::ts(2 << 10, 1));
    let a = rig.show(file(&key, "a.ts", Anchor::Start));
    rig.show(file(&key, "b.ts", Anchor::Start));
    assert!(rig.session.back());
    let back = rig.session.snapshot().unwrap();
    assert_eq!(back.target.origin, TextOrigin::Worktree);
    assert_eq!(back.target.expect, a.oid);
    assert!(matches!(back.target.at, Anchor::Byte { line: 0, .. }));
}

#[test]
fn session_a_new_open_after_back_drops_what_was_ahead() {
    let rig = Rig::new();
    let (tree, key) = rig.folder();
    for name in ["a.ts", "b.ts", "c.ts"] {
        tree.write(name, b"let x = 1;\n");
    }
    rig.show(file(&key, "a.ts", Anchor::Start));
    rig.show(file(&key, "b.ts", Anchor::Start));
    assert!(rig.session.back());
    assert_eq!(rig.session.history_len(), (0, 1));
    rig.show(file(&key, "c.ts", Anchor::Start));
    assert_eq!(rig.session.history_len(), (1, 0));
    assert!(!rig.session.can_forward());
    assert!(rig.session.back());
    assert_eq!(path_of(&rig), "a.ts");
}

#[test]
fn session_history_keeps_at_most_a_hundred_positions_in_order() {
    let rig = Rig::new();
    let (tree, key) = rig.folder();
    let count = HISTORY_MAX + 5;
    for i in 0..count {
        tree.write(&format!("f{i}.ts"), b"let x = 1;\n");
    }
    for i in 0..count {
        rig.session
            .open(file(&key, &format!("f{i}.ts"), Anchor::Start));
        let request = rig.session.next_request().unwrap();
        rig.session.apply(request);
    }
    rig.res.settle();
    assert_eq!(path_of(&rig), format!("f{}.ts", count - 1));
    assert_eq!(rig.session.history_len(), (HISTORY_MAX, 0));
    // Back, back, back, forward.
    for expected in [count - 2, count - 3, count - 4] {
        assert!(rig.session.back());
        assert_eq!(path_of(&rig), format!("f{expected}.ts"));
    }
    assert!(rig.session.forward());
    assert_eq!(path_of(&rig), format!("f{}.ts", count - 3));
    // All the way back: the oldest kept position is `f4` (the current one
    // is `f104`, the hundred before it are kept), not `f0`.
    while rig.session.back() {}
    assert_eq!(path_of(&rig), "f4.ts");
    assert_eq!(rig.session.history_len().0, 0);
}

#[test]
fn session_cancel_leaves_the_document_and_the_history() {
    let rig = Rig::new();
    let (tree, key) = rig.folder();
    tree.write("a.ts", b"let a = 1;\n");
    tree.write("b.ts", b"let b = 1;\n");
    tree.write("c.ts", b"let c = 1;\n");
    rig.show(file(&key, "a.ts", Anchor::Start));
    rig.show(file(&key, "b.ts", Anchor::Start));
    let history = rig.session.history_len();
    let before = rig.session.snapshot().unwrap();

    let ticket = rig.session.open(file(&key, "c.ts", Anchor::Start));
    let request = rig.session.next_request().unwrap();
    rig.session.cancel(request);
    rig.res.settle();
    assert_eq!(rig.host.events().last(), Some(&cancelled(ticket)));
    assert_eq!(path_of(&rig), "b.ts");
    assert_eq!(rig.session.history_len(), history);
    let after = rig.session.snapshot().unwrap();
    assert_eq!(after.generation, before.generation);
    assert_eq!(after.oid, before.oid);
}

#[test]
fn session_a_request_for_a_closed_project_is_cancelled() {
    let rig = Rig::new();
    let (tree, key) = rig.folder();
    tree.write("a.ts", b"let a = 1;\n");
    rig.show(file(&key, "a.ts", Anchor::Start));
    rig.host.projects.remove(&key);
    let ticket = rig
        .session
        .open(file(&key, "a.ts", Anchor::Line { line: 3, column: 0 }));
    let request = rig.session.next_request().unwrap();
    assert_eq!(rig.session.apply(request), OpenOutcome::Cancelled);
    assert_eq!(rig.host.events().last(), Some(&cancelled(ticket)));
    assert_eq!(path_of(&rig), "a.ts", "the document stays");
    assert_eq!(rig.session.history_len(), (0, 0));
    // A worktree the project does not have is the same.
    let (git_key, wt) = tree.git();
    let mut target = file(&git_key, "a.ts", Anchor::Start);
    target.worktree = Some(wt);
    rig.host.projects.set(git_snapshot(
        &git_key,
        &lectern_services::project::WorktreeKey(tree.root.clone()),
        &tree.root,
        &[],
    ));
    rig.session.open(target);
    let request = rig.session.next_request().unwrap();
    assert_eq!(rig.session.apply(request), OpenOutcome::Cancelled);
}

#[test]
fn session_back_and_forward_with_nothing_there_do_nothing() {
    let rig = Rig::new();
    assert!(!rig.session.back());
    assert!(!rig.session.forward());
    assert!(!rig.session.can_back() && !rig.session.can_forward());
}

#[test]
fn session_back_to_a_closed_project_keeps_the_history() {
    let rig = Rig::new();
    let (tree, key) = rig.folder();
    tree.write("a.ts", b"let a = 1;\n");
    let (other_tree, other) = rig.folder();
    other_tree.write("b.ts", b"let b = 1;\n");
    rig.show(file(&key, "a.ts", Anchor::Start));
    rig.show(file(&other, "b.ts", Anchor::Start));
    rig.host.projects.remove(&key);
    assert!(!rig.session.back());
    assert_eq!(path_of(&rig), "b.ts");
    assert_eq!(rig.session.history_len(), (1, 0));
}

#[test]
fn session_scrolled_and_viewport_reach_the_document() {
    let rig = Rig::new();
    let (tree, key) = rig.folder();
    tree.write("a.ts", &testing::ts(2 << 10, 0));
    rig.show(file(&key, "a.ts", Anchor::Start));
    rig.session
        .set_viewport(lectern_syntax::Viewport { rows: 10, cols: 30 });
    rig.session.scrolled(RowPos::default());
    rig.res.settle();
    assert_eq!(rig.session.snapshot().unwrap().state, DocState::Ready);
}

#[test]
fn session_focus_is_set_once_the_oid_is_known() {
    let rig = Rig::new();
    let (tree, key) = rig.folder();
    tree.write("a.ts", &testing::ts(4 << 10, 0));
    rig.session.open(file(&key, "a.ts", Anchor::Start));
    let request = rig.session.next_request().unwrap();
    rig.session.apply(request);
    // The tasks have not run: no OID yet, no focus.
    assert!(rig.session.sync().unwrap().oid.is_none());
    assert!(rig.host.code_intel.calls().is_empty());

    rig.res.settle();
    let snapshot = rig.session.sync().unwrap();
    let oid = snapshot.oid.expect("the full read gave the OID");
    let calls = rig.host.code_intel.calls();
    assert_eq!(calls.len(), 1);
    let (project, worktree, focus) = &calls[0];
    assert_eq!((project, worktree), (&key, &None));
    let Focus::Current(Some(text)) = focus else {
        panic!("a focus: {focus:?}");
    };
    assert_eq!(
        (&*text.path, text.oid, &text.origin),
        ("a.ts", oid, &TextOrigin::Worktree)
    );
    // Asked again, nothing changed: no second call.
    rig.session.sync();
    rig.session.sync();
    assert_eq!(rig.host.code_intel.calls().len(), 1);
}

#[test]
fn session_a_git_file_the_index_does_not_name_waits_for_the_canonical_oid() {
    let rig = Rig::new();
    let tree = Tree::new();
    let (key, wt) = tree.git();
    // The index names nothing: the raw OID is not the canonical content's
    // as far as this session can tell.
    rig.host
        .projects
        .set(git_snapshot(&key, &wt, &tree.root, &[]));
    tree.write("a.ts", b"let a = 1;\n");
    let mut target = file(&key, "a.ts", Anchor::Start);
    target.worktree = Some(wt.clone());
    let snapshot = rig.show(target);
    let oid = snapshot.oid.expect("the full read gave the OID");
    assert_eq!(snapshot.canonical, Canonical::NotAsked);
    // `show` synced once: the canonical OID was asked for, in a task. There
    // is no repository on disk in this rig, so git has none to give.
    rig.res.settle();
    let snapshot = rig.session.sync().unwrap();
    assert_eq!(snapshot.canonical, Canonical::Unavailable);
    assert!(
        rig.host.code_intel.calls().is_empty(),
        "no focus on a text whose canonical OID is not known: {:?}",
        rig.host.code_intel.calls()
    );
    // Asked once: syncing again starts nothing.
    rig.session.sync();
    assert_eq!(rig.res.executor.run_until_idle(), 0);

    // Once the index names the content, the raw OID is the canonical one.
    rig.host
        .projects
        .set(git_snapshot(&key, &wt, &tree.root, &[("a.ts", oid)]));
    rig.session.sync();
    let focus = rig.host.code_intel.current();
    let Some(Focus::Current(Some(text))) = focus.last() else {
        panic!("a focus: {focus:?}");
    };
    assert_eq!((text.oid, &*text.path), (oid, "a.ts"));
}

#[test]
fn session_focus_is_cleared_when_the_document_closes() {
    let rig = Rig::new();
    let (tree, key) = rig.folder();
    tree.write("a.ts", b"let a = 1;\n");
    rig.show(file(&key, "a.ts", Anchor::Start));
    assert_eq!(rig.host.code_intel.calls().len(), 1);
    rig.session.close();
    let focus = rig.host.code_intel.current();
    assert_eq!(focus.len(), 2);
    assert_eq!(focus[1], Focus::Current(None));
    assert!(rig.session.snapshot().is_none());
    // Closed twice: once.
    rig.session.close();
    assert_eq!(rig.host.code_intel.calls().len(), 2);
    // The history survives the view closing.
    tree.write("b.ts", b"let b = 1;\n");
    rig.show(file(&key, "b.ts", Anchor::Start));
    assert_eq!(rig.session.history_len(), (0, 0));
}

#[test]
fn session_focus_follows_the_document_that_replaces_it() {
    let rig = Rig::new();
    let (tree, key) = rig.folder();
    tree.write("a.ts", b"let a = 1;\n");
    tree.write("b.ts", b"let b = 22;\n");
    rig.show(file(&key, "a.ts", Anchor::Start));
    rig.show(file(&key, "b.ts", Anchor::Start));
    let focus = rig.host.code_intel.current();
    // a is current, then nothing while b's OID is not known, then b.
    assert_eq!(focus.len(), 3, "{focus:?}");
    assert!(matches!(&focus[0], Focus::Current(Some(t)) if &*t.path == "a.ts"));
    assert_eq!(focus[1], Focus::Current(None));
    assert!(matches!(&focus[2], Focus::Current(Some(t)) if &*t.path == "b.ts"));
}

#[test]
fn session_an_object_is_current_with_its_expect() {
    let rig = Rig::new();
    let tree = Tree::new();
    let (key, wt) = tree.git();
    rig.host
        .projects
        .set(git_snapshot(&key, &wt, &tree.root, &[]));
    let bytes: Arc<[u8]> = Arc::from(&b"export const old = 1;\n"[..]);
    let oid = lectern_services::project::BlobId::Sha1([7; 20]);
    let _share = rig.session.share_object(oid, bytes.clone());
    let mut target = file(&key, "old.ts", Anchor::Start);
    target.worktree = Some(wt);
    target.origin = TextOrigin::Object;
    target.expect = Some(oid);
    let snapshot = rig.show(target);
    assert_eq!(snapshot.state, DocState::Ready);
    let focus = rig.host.code_intel.current();
    let Some(Focus::Current(Some(text))) = focus.last() else {
        panic!("a focus: {focus:?}");
    };
    assert_eq!((text.oid, &text.origin), (oid, &TextOrigin::Object));
}

#[test]
fn session_share_object_forwards_to_the_engine() {
    let rig = Rig::new();
    let tree = Tree::new();
    let (key, wt) = tree.git();
    rig.host
        .projects
        .set(git_snapshot(&key, &wt, &tree.root, &[]));
    // Over 1 MiB, so that the engine's text cache does not keep it once the
    // registration is dropped.
    let bytes: Arc<[u8]> = testing::ts(1200 << 10, 0).into();
    let oid = lectern_services::project::BlobId::Sha1([7; 20]);
    let mut target = file(&key, "old.ts", Anchor::Start);
    target.worktree = Some(wt);
    target.origin = TextOrigin::Object;
    target.expect = Some(oid);

    // Not registered: the object database (no repository here) has nothing.
    let missing = rig.show(target.clone());
    assert_ne!(missing.state, DocState::Ready);
    // Registered: the content is read from the registration.
    let share = rig.session.share_object(oid, bytes.clone());
    let present = rig.show(target.clone());
    assert_eq!(present.state, DocState::Ready);
    assert_eq!(present.size, bytes.len() as u64);
    // Dropped: the registration is gone again.
    drop(share);
    let gone = rig.show(target);
    assert_ne!(gone.state, DocState::Ready);
}

#[test]
fn session_prefetch_resolves_roots_and_drops_unknown_targets() {
    let rig = Rig::new();
    let (folder, folder_key) = rig.folder();
    folder.write("a.ts", &testing::ts(8 << 10, 0));
    let repo = Tree::new();
    let (git_key, wt) = repo.git();
    repo.write("b.ts", &testing::ts(8 << 10, 1));
    rig.host
        .projects
        .set(git_snapshot(&git_key, &wt, &repo.root, &[]));
    // A project that is not open, and a worktree the repository lacks:
    // both files exist, but no root is known for them.
    let stranger = Tree::new();
    stranger.write("c.ts", &testing::ts(8 << 10, 2));
    let stranger_key = stranger.folder();
    let other_wt =
        lectern_services::project::WorktreeKey(Arc::from(std::path::Path::new("/nowhere/.git")));
    repo.write("d.ts", &testing::ts(8 << 10, 3));

    let mut in_git = file(&git_key, "b.ts", Anchor::Start);
    in_git.worktree = Some(wt.clone());
    let mut unknown_worktree = file(&git_key, "d.ts", Anchor::Start);
    unknown_worktree.worktree = Some(other_wt);
    let done = Arc::new(Mutex::new(Vec::new()));
    let log = done.clone();
    rig.session.engine().set_trace(move |event| {
        if let lectern_syntax::Trace::PrefetchDone { path } = event {
            lock(&log).push(path.to_string());
        }
    });
    rig.session.prefetch(vec![
        file(&folder_key, "a.ts", Anchor::Start),
        file(&stranger_key, "c.ts", Anchor::Start),
        in_git,
        unknown_worktree,
    ]);
    rig.res.settle();
    let mut done = lock(&done).clone();
    done.sort();
    assert_eq!(
        done,
        ["a.ts", "b.ts"],
        "each from the root of its own project"
    );
}

#[test]
fn session_events_may_reenter_open() {
    // `Host::emit` runs after the locks are released: a listener of
    // `ReaderEvent` that opens another file does not deadlock.
    use std::sync::OnceLock;
    struct Reentrant {
        inner: super::rig::FakeHost,
        session: OnceLock<Arc<crate::Session>>,
        key: ProjectKey,
    }
    impl crate::Host for Reentrant {
        fn snapshot(
            &self,
            p: &ProjectKey,
        ) -> Option<Arc<lectern_services::project::ProjectSnapshot>> {
            self.inner.snapshot(p)
        }
        fn open_projects(&self) -> Vec<ProjectKey> {
            self.inner.open_projects()
        }
        fn set_focus(
            &self,
            p: &ProjectKey,
            w: Option<&lectern_services::project::WorktreeKey>,
            f: Focus,
        ) {
            self.inner.set_focus(p, w, f);
        }
        fn emit(&self, event: ReaderEvent) {
            self.inner.emit(event);
            if let Some(session) = self.session.get() {
                let _ = session.open(file(&self.key, "b.ts", Anchor::Start));
                let _ = session.pending();
                let _ = session.snapshot();
                session.sync();
            }
        }
    }
    let rig = Rig::new();
    let (tree, key) = rig.folder();
    tree.write("a.ts", b"let a = 1;\n");
    let host = Arc::new(Reentrant {
        inner: super::rig::FakeHost {
            projects: rig.host.projects.clone(),
            code_intel: rig.host.code_intel.clone(),
            events: Mutex::default(),
        },
        session: OnceLock::new(),
        key: key.clone(),
    });
    let engine = lectern_syntax::Engine::new(lectern_syntax::EngineDeps {
        executor: rig.res.executor.clone(),
        cache: rig.res.cache.clone(),
        watcher: rig.res.watcher.clone(),
        languages: lectern_syntax::Languages::core(),
    });
    engine.start(rig.kernel.handle.clone());
    let session = Arc::new(crate::Session::new(engine, host.clone()));
    host.session.set(session.clone()).ok();
    session.open(file(&key, "a.ts", Anchor::Start));
    let request = session.next_request().unwrap();
    assert_eq!(session.apply(request), OpenOutcome::Shown);
    assert_eq!(session.pending(), 1, "the listener's own request is queued");
}
