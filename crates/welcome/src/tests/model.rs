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
        [(
            ContributionKind::Screen(LAYOUT),
            lectern_kernel::Name::Borrowed(screens::WELCOME)
        )],
        "the screen is registered"
    );
    assert_eq!(
        *rig.contributions.tokens.lock().unwrap(),
        [false],
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
