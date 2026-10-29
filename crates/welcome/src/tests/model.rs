//! The model and the two plugins on test kernels with a manual executor
//! (docs/design/welcome.md「测试」模型). The files are in a temporary
//! directory; the projects and the sessions are fakes.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use lectern_services::project::OpenError;
use lectern_services::workspace::{ContributionKind, screens};

use super::support::*;
use crate::plugin::LAYOUT;
use crate::recent::decode;
use crate::{DiscoveredState, Notice, OpenFailure, OpenSource, RecentState};

const LECTERN: &str = "/Users/example/work/lectern";
const NOTES: &str = "/Users/example/notes";
const BILLING: &str = "/Users/example/work/api/services/billing";

fn lectern_key() -> lectern_services::project::ProjectKey {
    git_key("/Users/example/work/lectern/.git")
}

fn app_snapshot() -> Arc<lectern_services::project::ProjectSnapshot> {
    snapshot(&git_key("/repos/app/.git"), "app", "/repos/app", "")
}

fn recent_of(rig: &Rig) -> Vec<crate::RecentEntry> {
    match &rig.model.snapshot().recent {
        RecentState::Ready(entries) => entries.to_vec(),
        RecentState::Loading => panic!("not loaded"),
    }
}

fn file_paths(rig: &Rig) -> Vec<PathBuf> {
    decode(&rig.file().expect("the file exists"))
        .unwrap()
        .into_iter()
        .map(|e| e.path)
        .collect()
}

#[test]
fn welcome_started_loads_the_file() {
    let rig = Rig::new();
    rig.seed(FIXTURE_V1);
    assert_eq!(rig.model.snapshot().recent, RecentState::Loading);
    rig.start_screen();
    assert_eq!(
        rig.model.snapshot().recent,
        RecentState::Loading,
        "activation reads nothing; the task has not run"
    );
    rig.settle();
    assert_eq!(rig.recent_paths(), paths(&[LECTERN, NOTES, BILLING]));
    assert_eq!(rig.writes(), 0, "loading does not write");
    assert_eq!(rig.rebuilds(), 0, "no discovery plugin, no rebuild");
    assert!(rig.projects.asked_to_open.lock().unwrap().is_empty());
}

#[test]
fn welcome_started_records_projects_opened_before_it() {
    let rig = Rig::new();
    rig.seed(FIXTURE_V1);
    // Opened by `restore` before this plugin's token went live: no event reached it.
    rig.projects.add(app_snapshot());
    rig.start_screen();
    rig.settle();
    assert_eq!(
        rig.recent_paths(),
        paths(&["/repos/app", LECTERN, NOTES, BILLING])
    );
    assert_eq!(recent_of(&rig)[0].last_opened, NOW);
    assert_eq!(rig.writes(), 1);
    assert_eq!(file_paths(&rig).len(), 4);
}

#[test]
fn welcome_activates_without_any_session_source() {
    let rig = Rig::new();
    rig.start_screen();
    assert_eq!(
        *rig.contributions.registered.lock().unwrap(),
        [
            (
                ContributionKind::Screen(LAYOUT),
                lectern_kernel::Name::Borrowed(screens::WELCOME)
            ),
            (
                ContributionKind::KeyBindings,
                lectern_kernel::Name::Borrowed(screens::WELCOME)
            )
        ],
        "the screen and its key bindings are registered"
    );
    assert_eq!(
        *rig.contributions.tokens.lock().unwrap(),
        [false, false],
        "activate has no token: the guard goes to the kernel for the commit"
    );
    rig.settle();
    let snapshot = rig.model.snapshot();
    assert_eq!(snapshot.discovered, DiscoveredState::Unavailable);
    assert_eq!(snapshot.recent, RecentState::Ready(Arc::from(Vec::new())));
    assert_eq!(rig.file(), None, "a first run writes nothing");
}

#[test]
fn discovery_started_builds_the_list() {
    let mut rig = Rig::new();
    let fresh = git_key("/repos/fresh/.git");
    rig.claude.set(vec![
        Sess::new("1", "/repos/fresh/crates/x", resolved(&fresh, "crates/x")).build(),
    ]);
    rig.start_discovery();
    assert_eq!(rig.model.snapshot().discovered, DiscoveredState::Pending);
    rig.settle();
    assert_eq!(rig.discovered_paths(), paths(&["/repos/fresh"]));
    assert_eq!(rig.rebuilds(), 1);
}

#[test]
fn recent_loaded_after_discovery_triggers_a_rebuild() {
    let mut rig = Rig::new();
    rig.seed(FIXTURE_V1);
    let fresh = git_key("/repos/fresh/.git");
    rig.claude.set(vec![
        Sess::new("1", LECTERN, resolved(&lectern_key(), ""))
            .running()
            .build(),
        Sess::new("2", "/repos/fresh", resolved(&fresh, "")).build(),
    ]);
    // Discovery first: its first rebuild runs before the file is read.
    rig.start_discovery();
    rig.start_screen();
    rig.settle();
    assert_eq!(rig.rebuilds(), 2, "the load asked for another one");
    assert_eq!(
        rig.discovered_paths(),
        paths(&["/repos/fresh"]),
        "the recent project is not left in the discovered list"
    );
    let lectern = recent_of(&rig)[0].clone();
    assert_eq!((lectern.sessions, lectern.running), (1, 1));
}

#[test]
fn discovery_drop_sets_unavailable() {
    let mut rig = Rig::new();
    rig.seed(FIXTURE_V1);
    rig.claude.set(vec![
        Sess::new("1", LECTERN, resolved(&lectern_key(), "")).build(),
    ]);
    rig.start_screen();
    rig.start_discovery();
    rig.settle();
    assert!(matches!(
        rig.model.snapshot().discovered,
        DiscoveredState::Ready(_)
    ));
    assert_eq!(recent_of(&rig)[0].sessions, 1);

    let wakes = rig.wakes();
    rig.stop_discovery();
    let snapshot = rig.model.snapshot();
    assert_eq!(snapshot.discovered, DiscoveredState::Unavailable);
    assert_eq!(
        recent_of(&rig)[0].sessions,
        0,
        "no counts without discovery"
    );
    assert_eq!(rig.wakes(), wakes + 1, "the view is told");
    // The screen goes on.
    assert!(matches!(snapshot.recent, RecentState::Ready(_)));
}

#[test]
fn opened_records_a_recent_entry_with_its_key_and_writes_the_file() {
    let rig = Rig::new();
    rig.start_screen();
    rig.settle();
    let key = git_key("/repos/app/.git");
    rig.projects
        .add(snapshot(&key, "app", "/repos/app", "crates/review"));
    rig.model.begin_open(
        PathBuf::from("/repos/app/crates/review"),
        OpenSource::Recent,
    );
    rig.emit_opened(&key);
    rig.settle();

    let entries = recent_of(&rig);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].path, Path::new("/repos/app/crates/review"));
    assert_eq!(entries[0].key, key);
    assert_eq!(&*entries[0].name, "app");
    assert_eq!(rig.model.snapshot().opening, None, "Opened clears it");
    let written = decode(&rig.file().unwrap()).unwrap();
    assert_eq!(written, entries);
    assert!(rig.projects.asked_to_open.lock().unwrap().is_empty());
}

#[test]
fn opening_a_listed_project_moves_its_entry_to_the_front() {
    let rig = Rig::new();
    rig.seed(FIXTURE_V1);
    rig.start_screen();
    rig.settle();
    let key = folder_key(NOTES);
    rig.projects.add(snapshot(&key, "notes", NOTES, ""));
    rig.clock.store(NOW + MINUTE, Ordering::SeqCst);
    rig.emit_opened(&key);
    rig.settle();
    assert_eq!(rig.recent_paths(), paths(&[NOTES, LECTERN, BILLING]));
    assert_eq!(recent_of(&rig)[0].last_opened, NOW + MINUTE);
}

#[test]
fn opened_without_a_snapshot_records_nothing() {
    let rig = Rig::new();
    rig.start_screen();
    rig.settle();
    rig.model
        .begin_open(PathBuf::from("/repos/gone"), OpenSource::Discovered);
    // The project closed again before the listener looked.
    rig.emit_opened(&git_key("/repos/gone/.git"));
    rig.settle();
    assert!(recent_of(&rig).is_empty());
    assert_eq!(rig.file(), None);
    assert_eq!(rig.model.snapshot().opening, None);
}

#[test]
fn opened_before_the_file_is_loaded_does_not_overwrite_it() {
    let rig = Rig::new();
    rig.seed(FIXTURE_V1);
    rig.start_screen();
    let app = app_snapshot();
    rig.projects.remember(app.clone());
    // The load task is queued but has not run.
    rig.model.record_opened(&app.key);
    rig.model.remove_recent(Path::new(NOTES));
    assert_eq!(rig.model.snapshot().recent, RecentState::Loading);
    assert_eq!(
        rig.file().as_deref(),
        Some(FIXTURE_V1),
        "nothing is written yet"
    );
    rig.settle();
    assert_eq!(
        rig.recent_paths(),
        paths(&["/repos/app", LECTERN, BILLING]),
        "what was in the file is still there; the new entry first; the removed one gone"
    );
    assert_eq!(file_paths(&rig), paths(&["/repos/app", LECTERN, BILLING]));
}

#[test]
fn an_unreadable_file_gives_an_empty_list_that_the_next_open_replaces() {
    for bytes in [FIXTURE_GARBAGE, FIXTURE_BAD_VERSION] {
        let rig = Rig::new();
        rig.seed(bytes);
        rig.start_screen();
        rig.settle();
        assert!(recent_of(&rig).is_empty());
        assert_eq!(
            rig.file().as_deref(),
            Some(bytes),
            "reading does not touch it"
        );
        let app = app_snapshot();
        rig.projects.add(app.clone());
        rig.emit_opened(&app.key);
        rig.settle();
        assert_eq!(file_paths(&rig), paths(&["/repos/app"]));
    }
}

#[test]
fn a_failed_write_keeps_the_list_in_memory() {
    let rig = Rig::new();
    crate::recent::store::testing::block_dir(&rig.data_dir());
    rig.start_screen();
    rig.settle();
    let app = app_snapshot();
    rig.projects.add(app.clone());
    rig.emit_opened(&app.key);
    rig.settle();
    assert_eq!(rig.recent_paths(), paths(&["/repos/app"]));
    assert_eq!(rig.writes(), 1);
}

#[test]
fn writes_are_serialized_and_coalesced() {
    let rig = Rig::new();
    rig.seed(FIXTURE_V1);
    rig.start_screen();
    rig.settle();
    // Two changes before the write task runs: one task.
    rig.model.remove_recent(Path::new(LECTERN));
    rig.model.remove_recent(Path::new(NOTES));
    // A third arrives while that task runs.
    let model = rig.model.clone();
    *rig.model.stats.during_write.lock().unwrap() =
        Some(Box::new(move || model.remove_recent(Path::new(BILLING))));
    rig.settle();
    assert_eq!(rig.writes(), 2, "the running task goes round once more");
    assert_eq!(
        file_paths(&rig),
        Vec::<PathBuf>::new(),
        "the last write is the newest list"
    );
    assert!(!crate::recent::store::testing::temp_exists(&rig.data_dir()));
}

#[test]
fn sessions_changed_rebuilds_once_when_events_pile_up() {
    let mut rig = Rig::new();
    rig.start_discovery();
    rig.settle();
    assert_eq!(rig.rebuilds(), 1);
    // Three events before the task takes its input: one task, one run.
    for _ in 0..3 {
        rig.emit(rig.claude.changed());
    }
    rig.settle();
    assert_eq!(rig.rebuilds(), 2);
    // One while it runs: it goes round once more.
    let model = rig.model.clone();
    *rig.model.stats.during_rebuild.lock().unwrap() =
        Some(Box::new(move || model.request_rebuild()));
    rig.emit(rig.claude.changed());
    rig.settle();
    assert_eq!(rig.rebuilds(), 4);
}

#[test]
fn project_events_other_than_changed_rebuild() {
    let mut rig = Rig::new();
    rig.start_discovery();
    rig.settle();
    let key = git_key("/repos/a/.git");
    rig.emit(lectern_services::project::ProjectEvent::Closed {
        project: key.clone(),
    });
    rig.settle();
    assert_eq!(rig.rebuilds(), 2);
    rig.emit(lectern_services::project::ProjectEvent::Changed {
        project: key,
        generation: 2,
        parts: Default::default(),
    });
    rig.settle();
    assert_eq!(rig.rebuilds(), 2, "Changed does not matter to the list");
}

#[test]
fn an_open_project_leaves_the_discovered_list() {
    let mut rig = Rig::new();
    let fresh = git_key("/repos/fresh/.git");
    rig.claude.set(vec![
        Sess::new("1", "/repos/fresh", resolved(&fresh, "")).build(),
    ]);
    rig.start_screen();
    rig.start_discovery();
    rig.settle();
    assert_eq!(rig.discovered_paths(), paths(&["/repos/fresh"]));
    // It opens: the project service lists it, and the session is located.
    rig.projects
        .add(snapshot(&fresh, "fresh", "/repos/fresh", ""));
    rig.claude.set(vec![
        Sess::new("1", "/repos/fresh", located(&fresh, "")).build(),
    ]);
    rig.emit_opened(&fresh);
    rig.settle();
    assert!(rig.discovered_paths().is_empty());
    assert_eq!(rig.recent_paths(), paths(&["/repos/fresh"]));
    assert_eq!(
        recent_of(&rig)[0].sessions,
        1,
        "its session counts on the recent row"
    );
}

#[test]
fn remove_recent_updates_the_snapshot_before_writing() {
    let rig = Rig::new();
    rig.seed(FIXTURE_V1);
    rig.start_screen();
    rig.settle();
    let generation = rig.model.snapshot().generation;
    rig.model.remove_recent(Path::new(NOTES));
    let after = rig.model.snapshot();
    assert_eq!(after.generation, generation + 1);
    assert_eq!(rig.recent_paths(), paths(&[LECTERN, BILLING]));
    assert_eq!(rig.writes(), 0, "the file waits for the task");
    assert_eq!(rig.file().as_deref(), Some(FIXTURE_V1));
    rig.settle();
    assert_eq!(file_paths(&rig), paths(&[LECTERN, BILLING]));
}

#[test]
fn remove_recent_of_an_unknown_path_is_ignored() {
    let rig = Rig::new();
    rig.seed(FIXTURE_V1);
    rig.start_screen();
    rig.settle();
    let before = rig.model.snapshot();
    rig.model.remove_recent(Path::new("/not/in/the/list"));
    rig.settle();
    assert_eq!(rig.model.snapshot().generation, before.generation);
    assert_eq!(rig.writes(), 0);
}

#[test]
fn rebuild_merges_counts_without_restoring_a_removed_entry() {
    let mut rig = Rig::new();
    rig.seed(FIXTURE_V1);
    rig.claude.set(vec![
        Sess::new("1", LECTERN, resolved(&lectern_key(), "")).build(),
    ]);
    rig.start_screen();
    rig.start_discovery();
    rig.settle();
    assert_eq!(recent_of(&rig)[0].sessions, 1);

    // A rebuild starts with the old list, then the user removes an entry.
    rig.claude.set(vec![
        Sess::new("1", LECTERN, resolved(&lectern_key(), "")).build(),
        Sess::new("2", LECTERN, resolved(&lectern_key(), "")).build(),
    ]);
    let model = rig.model.clone();
    *rig.model.stats.during_rebuild.lock().unwrap() =
        Some(Box::new(move || model.remove_recent(Path::new(LECTERN))));
    rig.emit(rig.claude.changed());
    rig.settle();
    assert_eq!(
        rig.recent_paths(),
        paths(&[NOTES, BILLING]),
        "the removed entry does not come back with the counts"
    );
    // With the entry gone, its key is not a recent key any more: the folder shows.
    assert_eq!(rig.discovered_paths(), paths(&[LECTERN]));
    assert_eq!(file_paths(&rig), paths(&[NOTES, BILLING]));
}

#[test]
fn begin_open_then_error_marks_the_entry_missing_and_clears_opening() {
    let rig = Rig::new();
    rig.seed(FIXTURE_V1);
    rig.start_screen();
    rig.settle();
    rig.model
        .begin_open(PathBuf::from(NOTES), OpenSource::Recent);
    assert_eq!(
        rig.model.snapshot().opening.as_ref().map(|o| o.source),
        Some(OpenSource::Recent)
    );
    rig.emit_open_failed(OpenError::NotFound);
    let snapshot = rig.model.snapshot();
    assert_eq!(snapshot.opening, None);
    assert_eq!(
        snapshot.notice,
        Some(Notice::OpenFailed(OpenFailure::NotFound))
    );
    let entries = recent_of(&rig);
    assert_eq!(
        entries.iter().map(|e| e.missing).collect::<Vec<_>>(),
        [false, true, false],
        "only the entry that was opened"
    );
    assert_eq!(rig.writes(), 0, "missing is not saved");

    // The next try clears the notice.
    rig.model
        .begin_open(PathBuf::from(NOTES), OpenSource::Recent);
    assert_eq!(rig.model.snapshot().notice, None);
    // A permission error does not mean the folder is gone.
    rig.emit_open_failed(OpenError::PermissionDenied);
    assert_eq!(
        rig.model.snapshot().notice,
        Some(Notice::OpenFailed(OpenFailure::PermissionDenied))
    );
    assert!(recent_of(&rig)[1].missing, "still marked from before");
    // It opens after all: the mark goes.
    let key = folder_key(NOTES);
    rig.projects.add(snapshot_of(&key, "notes", NOTES));
    rig.emit_opened(&key);
    rig.settle();
    assert!(recent_of(&rig).iter().all(|e| !e.missing));
}

fn snapshot_of(
    key: &lectern_services::project::ProjectKey,
    name: &str,
    root: &str,
) -> Arc<lectern_services::project::ProjectSnapshot> {
    snapshot(key, name, root, "")
}

#[test]
fn error_without_opening_only_shows_the_notice() {
    let rig = Rig::new();
    rig.seed(FIXTURE_V1);
    rig.start_screen();
    rig.settle();
    rig.emit_open_failed(OpenError::RepoUnreadable(Arc::from("details")));
    assert_eq!(
        rig.model.snapshot().notice,
        Some(Notice::OpenFailed(OpenFailure::RepoUnreadable))
    );
    assert!(recent_of(&rig).iter().all(|e| !e.missing));
    rig.model.clear_notice();
    assert_eq!(rig.model.snapshot().notice, None);
}

#[test]
fn language_not_saved_sets_the_notice() {
    let rig = Rig::new();
    rig.model.language_not_saved();
    assert_eq!(rig.model.snapshot().notice, Some(Notice::LanguageNotSaved));
}

#[test]
fn wake_is_called_once_per_snapshot() {
    let rig = Rig::new();
    rig.seed(FIXTURE_V1);
    let woken = Arc::new(AtomicUsize::new(0));
    let counter = woken.clone();
    let guard = rig.model.subscribe(Box::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    }));
    let start = rig.model.snapshot().generation;
    rig.start_screen();
    rig.settle(); // loaded
    rig.model
        .begin_open(PathBuf::from(NOTES), OpenSource::Recent);
    rig.emit_open_failed(OpenError::Io(Arc::from("x")));
    rig.model.clear_notice();
    rig.model.remove_recent(Path::new(NOTES));
    rig.settle();
    let generations = rig.model.snapshot().generation - start;
    assert_eq!(generations, 5);
    assert_eq!(woken.load(Ordering::SeqCst) as u64, generations);

    // Nothing changes, nothing is sent.
    rig.model.clear_notice();
    rig.model.remove_recent(Path::new(NOTES));
    assert_eq!(woken.load(Ordering::SeqCst) as u64, generations);

    // After the guard goes, the callback is not called.
    drop(guard);
    rig.model.language_not_saved();
    assert_eq!(woken.load(Ordering::SeqCst) as u64, generations);
}

/// The rig of the review tests: the file with the three recent projects, the
/// three plugins, two sessions of "lectern" in the list (`s1` and `s2`,
/// both placed in the project, `s3` is not in it).
fn review_rig() -> Rig {
    let mut rig = Rig::new();
    rig.seed(FIXTURE_V1);
    rig.claude.set(vec![
        Sess::new("s1", LECTERN, located(&lectern_key(), ""))
            .at(NOW - 3 * MINUTE)
            .build(),
        Sess::new("s2", LECTERN, resolved(&lectern_key(), ""))
            .at(NOW - 2 * MINUTE)
            .build(),
    ]);
    rig.start_screen();
    rig.start_discovery();
    rig.start_review();
    rig.settle();
    rig
}

fn marked(rig: &Rig) -> Vec<Option<crate::ReviewMark>> {
    recent_of(rig).into_iter().map(|e| e.review).collect()
}

fn file_marks(rig: &Rig) -> Vec<Option<crate::ReviewMark>> {
    decode(&rig.file().expect("the file exists"))
        .unwrap()
        .into_iter()
        .map(|e| e.review)
        .collect()
}

#[test]
fn overview_event_marks_the_entries_of_that_key_and_writes_once() {
    let rig = review_rig();
    assert_eq!(rig.writes(), 0);
    assert_eq!(
        recent_of(&rig)
            .iter()
            .map(|e| e.to_review)
            .collect::<Vec<_>>(),
        [None, None, None],
        "no mark yet: the rows show the session count"
    );

    // s1 needs review, s2 is done, s3 needs review and is not in the list.
    let key = lectern_key();
    rig.review
        .set(&key, &[("s1", true), ("s2", false), ("s3", true)]);
    rig.emit(rig.review.event(&key));
    rig.settle();
    let mark = |ids: &[&str], at: i64| crate::ReviewMark {
        sessions: ids.iter().map(|id| session_key(id)).collect(),
        at,
    };
    assert_eq!(
        marked(&rig),
        [Some(mark(&["s1", "s3"], NOW)), None, None],
        "only the entry with the project's key"
    );
    assert_eq!(rig.writes(), 1);
    assert_eq!(file_marks(&rig), marked(&rig));
    assert_eq!(
        recent_of(&rig)[0].to_review,
        Some(1),
        "s1 is listed, s3 is gone, s2 is not marked and has no later activity"
    );
    assert_eq!(recent_of(&rig)[1].to_review, None);

    // The same set again, later: no write; the time moves on in memory.
    rig.clock
        .store(NOW + 5 * MINUTE, std::sync::atomic::Ordering::SeqCst);
    rig.emit(rig.review.event(&key));
    rig.settle();
    assert_eq!(rig.writes(), 1, "the set did not change");
    assert_eq!(marked(&rig)[0], Some(mark(&["s1", "s3"], NOW + 5 * MINUTE)));
    assert_eq!(file_marks(&rig)[0], Some(mark(&["s1", "s3"], NOW)));

    // Another set is written, and the number follows: s1 and s2 are listed.
    rig.review.set(&key, &[("s2", true), ("s1", true)]);
    rig.emit(rig.review.event(&key));
    rig.settle();
    assert_eq!(rig.writes(), 2);
    assert_eq!(
        file_marks(&rig)[0],
        Some(mark(&["s1", "s2"], NOW + 5 * MINUTE)),
        "the order of the overview is not part of the mark"
    );
    assert_eq!(recent_of(&rig)[0].to_review, Some(2));
    assert!(rig.review.watched.lock().unwrap().is_empty());
}

#[test]
fn a_session_active_after_the_mark_counts_until_the_next_overview() {
    let rig = review_rig();
    let key = lectern_key();
    rig.review.set(&key, &[("s1", true)]);
    rig.emit(rig.review.event(&key));
    rig.settle();
    assert_eq!(recent_of(&rig)[0].to_review, Some(1));

    // The agent works on in s2 after the overview was taken.
    rig.claude.set(vec![
        Sess::new("s1", LECTERN, located(&key, ""))
            .at(NOW - 3 * MINUTE)
            .build(),
        Sess::new("s2", LECTERN, resolved(&key, ""))
            .at(NOW + MINUTE)
            .build(),
    ]);
    rig.emit(rig.claude.changed());
    rig.settle();
    assert_eq!(recent_of(&rig)[0].to_review, Some(2), "s1 and the later s2");
}

#[test]
fn overview_gone_leaves_the_mark() {
    let rig = review_rig();
    let key = lectern_key();
    rig.review.set(&key, &[("s1", true)]);
    rig.emit(rig.review.event(&key));
    rig.settle();
    let before = marked(&rig);
    let writes = rig.writes();

    rig.review.clear(&key);
    rig.emit(rig.review.event(&key));
    rig.settle();
    assert_eq!(marked(&rig), before);
    assert_eq!(rig.writes(), writes);
    assert_eq!(recent_of(&rig)[0].to_review, Some(1));
}

#[test]
fn an_overview_of_another_project_marks_nothing() {
    let rig = review_rig();
    let other = git_key("/repos/other/.git");
    rig.review.set(&other, &[("s1", true)]);
    rig.emit(rig.review.event(&other));
    rig.settle();
    assert_eq!(marked(&rig), [None, None, None]);
    assert_eq!(rig.writes(), 0);
}

#[test]
fn opening_a_project_again_keeps_its_mark() {
    let rig = review_rig();
    let key = lectern_key();
    rig.review.set(&key, &[("s1", true)]);
    rig.emit(rig.review.event(&key));
    rig.settle();
    let before = marked(&rig)[0].clone();
    assert!(before.is_some());

    rig.projects.add(snapshot(&key, "lectern", LECTERN, ""));
    rig.emit_opened(&key);
    rig.settle();
    assert_eq!(rig.recent_paths()[0], Path::new(LECTERN));
    assert_eq!(marked(&rig)[0], before);
    assert_eq!(recent_of(&rig)[0].to_review, Some(1));
}

#[test]
fn the_numbers_go_when_discovery_stops_and_the_marks_stay() {
    let mut rig = review_rig();
    let key = lectern_key();
    rig.review.set(&key, &[("s1", true)]);
    rig.emit(rig.review.event(&key));
    rig.settle();
    assert_eq!(recent_of(&rig)[0].to_review, Some(1));

    rig.stop_discovery();
    assert_eq!(recent_of(&rig)[0].to_review, None);
    assert!(recent_of(&rig)[0].review.is_some());

    // A later overview still marks, with no number to show.
    rig.review.set(&key, &[("s1", true), ("s2", true)]);
    rig.emit(rig.review.event(&key));
    rig.settle();
    assert_eq!(
        recent_of(&rig)[0].review.as_ref().unwrap().sessions.len(),
        2
    );
    assert_eq!(recent_of(&rig)[0].to_review, None);
}

#[test]
fn welcome_activates_without_review() {
    let mut rig = Rig::new();
    rig.seed(FIXTURE_V1);
    rig.claude.set(vec![
        Sess::new("s1", LECTERN, located(&lectern_key(), "")).build(),
    ]);
    rig.start_screen();
    rig.start_discovery();
    rig.settle();
    let first = &recent_of(&rig)[0];
    assert_eq!(
        (first.sessions, first.to_review, first.review.clone()),
        (1, None, None)
    );
    assert!(matches!(
        rig.model.snapshot().discovered,
        DiscoveredState::Ready(_)
    ));
    // An overview event with no review plugin listening changes nothing.
    rig.emit(rig.review.event(&lectern_key()));
    rig.settle();
    assert_eq!(recent_of(&rig)[0].review, None);
}
