//! Step 2 (docs/design/welcome.md「测试」): the view in a test window, under a
//! `Content` region like `workspace`'s, with the real factory from the
//! registered `Screen` payload, the real key bindings and a fake language
//! preference. The semantic tree comes from the `Recorder`; the snapshots
//! are that tree as text (no bounds, no call sites).

use std::fmt::Write as _;
use std::path::PathBuf;

use gpui::{
    AnyView, Bounds, Context, InteractiveElement, IntoElement, Modifiers, MouseButton,
    ParentElement, Pixels, Render, Styled, TestAppContext, VisualTestContext, Window, div, px,
    size,
};
use lectern_i18n::{Locale, Strings};
use lectern_platform::system::PrefError;
use lectern_services::project::OpenError;
use lectern_services::session::Agent;
use lectern_services::workspace::{Target, WorkspaceCommand};
use lectern_ui::motion::LOADING_SKELETON_AT;
use lectern_ui::semantic::{Record, Recorder, Role};
use lectern_ui::{Appearance, init_fixed};

use super::support::*;
use crate::actions::{OpenSelected, RemoveSelected, SelectLanguage, SelectNext, SelectPrevious};
use crate::{DiscoveredState, Notice, OpenSource, RecentState};

const LECTERN: &str = "/Users/example/work/lectern";
const NOTES: &str = "/Users/example/notes";
const BILLING: &str = "/Users/example/work/api/services/billing";
const FRESH: &str = "/Users/example/work/fresh";
const SCRATCH: &str = "/Users/example/scratch";

/// `workspace`'s root and `Content` region around the screen.
struct Host {
    content: AnyView,
}

impl Render for Host {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().key_context("Workspace").size_full().child(
            div()
                .id("workspace-content")
                .key_context("Content")
                .size_full()
                .child(self.content.clone()),
        )
    }
}

/// A window of the design's size with the screen built by the registered
/// factory and the screen's key bindings in the keymap. The plugin "welcome"
/// must be started.
fn open<'a>(cx: &'a mut TestAppContext, rig: &Rig) -> &'a mut VisualTestContext {
    open_sized(cx, rig, size(px(820.0), px(520.0)))
}

fn open_sized<'a>(
    cx: &'a mut TestAppContext,
    rig: &Rig,
    window_size: gpui::Size<Pixels>,
) -> &'a mut VisualTestContext {
    // The kernel and the model's wake-ups arrive from other threads.
    cx.executor().allow_parking();
    cx.update(|cx| init_fixed(cx, Strings::of(Locale::En), Appearance::Light, false));
    let bindings = cx.update(|cx| rig.contributions.key_bindings(cx));
    cx.update(|cx| cx.bind_keys(bindings));
    let screen = rig.contributions.screen();
    let (_, cx) = cx.add_window_view(move |window, cx| Host {
        content: (screen.build)(&Target::None, window, cx),
    });
    cx.simulate_resize(window_size);
    cx.run_until_parked();
    cx
}

/// One frame with the recorder armed for it.
fn frame(cx: &mut VisualTestContext) -> Vec<Record> {
    cx.run_until_parked();
    let id = cx.update(|window, _| window.window_handle().window_id());
    Recorder::begin(id);
    cx.update(|window, cx| window.draw(cx).clear(cx));
    Recorder::finish()
}

/// The records as text: the nesting, ID, role, label (message ID and text),
/// state and attributes.
fn render_tree(records: &[Record]) -> String {
    let mut out = String::new();
    for (index, record) in records.iter().enumerate() {
        let mut depth = 0;
        let mut parent = records[index].parent;
        while let Some(p) = parent {
            depth += 1;
            parent = records[p].parent;
        }
        let spec = &record.semantic;
        let _ = write!(
            out,
            "{}{} {:?}",
            "  ".repeat(depth),
            spec.id.as_str(),
            spec.role
        );
        if let Some(label) = &spec.label {
            match label.key {
                Some(key) => {
                    let _ = write!(out, " [{key}] {:?}", label.text.as_ref());
                }
                None => {
                    let _ = write!(out, " {:?}", label.text.as_ref());
                }
            }
        }
        if spec.state.selected {
            out.push_str(" selected");
        }
        if spec.state.disabled {
            out.push_str(" disabled");
        }
        for (key, value) in &spec.attrs {
            let _ = write!(out, " {key}={value}");
        }
        out.push('\n');
    }
    out
}

fn tree(cx: &mut VisualTestContext) -> String {
    render_tree(&frame(cx))
}

fn bounds(records: &[Record], id: &str) -> Bounds<Pixels> {
    records
        .iter()
        .find(|record| record.semantic.id.as_str() == id)
        .unwrap_or_else(|| panic!("no node {id}"))
        .bounds
}

fn has(records: &[Record], id: &str) -> bool {
    records
        .iter()
        .any(|record| record.semantic.id.as_str() == id)
}

fn click(cx: &mut VisualTestContext, id: &str) {
    let records = frame(cx);
    let bounds = bounds(&records, id);
    cx.simulate_click(bounds.center(), Modifiers::default());
    cx.run_until_parked();
}

/// A right click on the node, which opens the row's context menu.
fn right_click(cx: &mut VisualTestContext, id: &str) {
    let records = frame(cx);
    let at = bounds(&records, id).center();
    cx.simulate_mouse_down(at, MouseButton::Right, Modifiers::default());
    cx.simulate_mouse_up(at, MouseButton::Right, Modifiers::default());
    cx.run_until_parked();
}

/// What the menu item of the language pop-up button does: the action it
/// dispatches, built by name from its JSON like the probe does.
fn select_language(cx: &mut VisualTestContext, value: serde_json::Value) {
    let action = cx
        .update(|_, cx| cx.build_action("welcome::SelectLanguage", Some(value)))
        .expect("SelectLanguage is registered");
    cx.update(|window, cx| window.dispatch_action(action, cx));
    cx.run_until_parked();
}

/// The `value` attribute of the pop-up button.
fn language_value(cx: &mut VisualTestContext) -> String {
    frame(cx)
        .iter()
        .find(|record| record.semantic.id.as_str() == "welcome.language")
        .and_then(|record| {
            record
                .semantic
                .attrs
                .iter()
                .find(|(key, _)| *key == "value")
                .map(|(_, value)| value.to_string())
        })
        .expect("welcome.language has a value")
}

/// Moves the test clock and lets the timers that came due run.
fn advance(cx: &mut VisualTestContext, by: std::time::Duration) {
    // `advance_clock` polls the timers that came due itself, so it must not
    // run inside an `update`.
    let executor = cx.update(|_, cx| cx.background_executor().clone());
    executor.advance_clock(by);
    cx.run_until_parked();
}

/// The IDs of the rows that are selected.
fn selected(cx: &mut VisualTestContext) -> Vec<String> {
    frame(cx)
        .iter()
        .filter(|record| record.semantic.role == Role::ListItem && record.semantic.state.selected)
        .map(|record| record.semantic.id.as_str().to_owned())
        .collect()
}

fn recent_row(path: &str) -> String {
    format!("welcome.recent.item[{path}]")
}

fn discovered_row(path: &str) -> String {
    format!("welcome.discovered.item[{path}]")
}

fn lectern_key() -> lectern_services::project::ProjectKey {
    git_key("/Users/example/work/lectern/.git")
}

/// The file's three recent projects, one running session in the first, and
/// two folders found through sessions: one of both agents, one of Codex.
fn with_lists(rig: &mut Rig) {
    rig.seed(FIXTURE_V1);
    rig.start_screen();
    let fresh = git_key("/Users/example/work/fresh/.git");
    let scratch = folder_key(SCRATCH);
    rig.claude.set(vec![
        Sess::new("c1", LECTERN, resolved(&lectern_key(), ""))
            .running()
            .build(),
        Sess::new(
            "c2",
            format!("{FRESH}/crates/x"),
            resolved(&fresh, "crates/x"),
        )
        .at(NOW - 3 * 60 * MINUTE)
        .build(),
    ]);
    rig.codex.set(vec![
        Sess::new("x1", FRESH, resolved(&fresh, ""))
            .agent("session-codex", Agent::Codex)
            .at(NOW - 90 * MINUTE)
            .build(),
        Sess::new("x2", SCRATCH, resolved(&scratch, ""))
            .agent("session-codex", Agent::Codex)
            .at(NOW - 26 * 60 * MINUTE)
            .build(),
    ]);
    rig.start_discovery();
    rig.settle();
}

#[gpui::test]
fn first_render_shows_title_and_open_folder_before_any_snapshot(cx: &mut TestAppContext) {
    let mut rig = Rig::new();
    rig.seed(FIXTURE_V1);
    rig.start_screen();
    rig.start_discovery();
    assert_eq!(rig.model.snapshot().recent, RecentState::Loading);
    assert_eq!(rig.model.snapshot().discovered, DiscoveredState::Pending);
    let cx = open(cx, &rig);
    let text = tree(cx);
    assert!(!text.contains("welcome.recent.item"), "{text}");
    assert!(
        !text.contains(".empty"),
        "no empty state while loading: {text}"
    );
    insta::assert_snapshot!("welcome_tree_loading", text);
}

#[gpui::test]
fn welcome_tree_empty(cx: &mut TestAppContext) {
    let rig = Rig::new();
    rig.start_screen();
    rig.settle();
    let cx = open(cx, &rig);
    insta::assert_snapshot!("welcome_tree_empty", tree(cx));
}

#[gpui::test]
fn welcome_tree_with_lists(cx: &mut TestAppContext) {
    let mut rig = Rig::new();
    with_lists(&mut rig);
    let cx = open(cx, &rig);
    let first = tree(cx);
    insta::assert_snapshot!("welcome_tree_with_lists", first);
    // A selection shows as `selected` on its row only.
    cx.simulate_keystrokes("down down");
    cx.run_until_parked();
    assert_eq!(selected(cx), [recent_row(NOTES)]);
}

#[gpui::test]
fn welcome_tree_with_error(cx: &mut TestAppContext) {
    let mut rig = Rig::new();
    with_lists(&mut rig);
    let cx = open(cx, &rig);
    click(cx, &recent_row(NOTES));
    rig.emit_open_failed(OpenError::NotFound);
    rig.settle();
    let text = tree(cx);
    insta::assert_snapshot!("welcome_tree_with_error", text);
}

#[gpui::test]
fn skeleton_appears_after_100ms_and_rows_replace_it(cx: &mut TestAppContext) {
    let mut rig = Rig::new();
    rig.seed(FIXTURE_V1);
    rig.start_screen();
    rig.start_discovery();
    let cx = open(cx, &rig);
    // The skeletons declare no node: they show as the height they add above
    // the discovered list.
    let start = bounds(&frame(cx), "welcome.discovered").origin.y;
    advance(cx, LOADING_SKELETON_AT / 2);
    cx.run_until_parked();
    assert_eq!(
        bounds(&frame(cx), "welcome.discovered").origin.y,
        start,
        "nothing before the threshold"
    );
    advance(cx, LOADING_SKELETON_AT / 2);
    cx.run_until_parked();
    let with_skeleton = bounds(&frame(cx), "welcome.discovered").origin.y;
    assert!(with_skeleton > start, "{with_skeleton:?} > {start:?}");

    rig.settle();
    let records = frame(cx);
    assert!(has(&records, &recent_row(LECTERN)));
    assert!(has(&records, "welcome.discovered.empty"));
    assert!(
        bounds(&records, "welcome.discovered").origin.y > start,
        "the rows of the recent list are taller than its skeleton was"
    );
}

#[gpui::test]
fn unavailable_discovery_shows_the_empty_state_without_a_skeleton(cx: &mut TestAppContext) {
    let rig = Rig::new();
    rig.start_screen();
    rig.settle();
    assert_eq!(
        rig.model.snapshot().discovered,
        DiscoveredState::Unavailable
    );
    let cx = open(cx, &rig);
    let start = frame(cx);
    assert!(has(&start, "welcome.discovered.empty"));
    let at = bounds(&start, "welcome.discovered.empty").origin.y;
    advance(cx, LOADING_SKELETON_AT * 3);
    cx.run_until_parked();
    let later = frame(cx);
    assert_eq!(bounds(&later, "welcome.discovered.empty").origin.y, at);
}

#[gpui::test]
fn click_and_enter_dispatch_open_project_once(cx: &mut TestAppContext) {
    let mut rig = Rig::new();
    with_lists(&mut rig);
    let cx = open(cx, &rig);
    click(cx, &recent_row(LECTERN));
    assert_eq!(
        *rig.contributions.dispatched.lock().unwrap(),
        [WorkspaceCommand::OpenProject(PathBuf::from(LECTERN))]
    );
    let opening = rig.model.snapshot().opening.clone().expect("opening");
    assert_eq!(opening.path, PathBuf::from(LECTERN));
    assert_eq!(opening.source, OpenSource::Recent);
    assert_eq!(selected(cx), [recent_row(LECTERN)], "the click selects");
    assert!(
        tree(cx).contains("opening=true"),
        "the root says an open is in flight"
    );

    // ↵ on the row being opened does nothing.
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(rig.contributions.dispatched.lock().unwrap().len(), 1);

    // Another row is not held back.
    cx.simulate_keystrokes("down enter");
    cx.run_until_parked();
    assert_eq!(
        rig.contributions.dispatched.lock().unwrap()[1],
        WorkspaceCommand::OpenProject(PathBuf::from(NOTES))
    );

    // A failure clears `opening`: the same row can be tried again.
    rig.emit_open_failed(OpenError::PermissionDenied);
    rig.settle();
    cx.run_until_parked();
    assert_eq!(rig.model.snapshot().opening, None);
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(rig.contributions.dispatched.lock().unwrap().len(), 3);
    assert!(rig.projects.asked_to_open.lock().unwrap().is_empty());
}

#[gpui::test]
fn a_click_on_a_row_that_is_gone_does_nothing(cx: &mut TestAppContext) {
    let mut rig = Rig::new();
    with_lists(&mut rig);
    let cx = open(cx, &rig);
    let records = frame(cx);
    let target = bounds(&records, &recent_row(NOTES)).center();
    // The entry goes away after the frame that drew it, before the click.
    rig.model.remove_recent(&PathBuf::from(NOTES));
    cx.simulate_click(target, Modifiers::default());
    cx.run_until_parked();
    assert!(
        rig.contributions.dispatched.lock().unwrap().is_empty(),
        "the removed entry is not opened, nor is its neighbour"
    );
    assert_eq!(rig.model.snapshot().opening, None);
}

#[gpui::test]
fn enter_opens_a_discovered_folder(cx: &mut TestAppContext) {
    let mut rig = Rig::new();
    with_lists(&mut rig);
    let cx = open(cx, &rig);
    // Three recent entries, then the first discovered folder (the newest).
    cx.simulate_keystrokes("down down down down enter");
    cx.run_until_parked();
    assert_eq!(
        *rig.contributions.dispatched.lock().unwrap(),
        [WorkspaceCommand::OpenProject(PathBuf::from(FRESH))]
    );
    assert_eq!(
        rig.model.snapshot().opening.as_ref().map(|o| o.source),
        Some(OpenSource::Discovered)
    );
}

#[gpui::test]
fn open_folder_dispatches_prompt(cx: &mut TestAppContext) {
    let mut rig = Rig::new();
    with_lists(&mut rig);
    rig.emit_open_failed(OpenError::Io("x".into()));
    rig.settle();
    assert!(rig.model.snapshot().notice.is_some());
    let cx = open(cx, &rig);
    click(cx, "welcome.open-folder");
    assert_eq!(
        *rig.contributions.dispatched.lock().unwrap(),
        [WorkspaceCommand::PromptOpenProject]
    );
    assert_eq!(rig.model.snapshot().notice, None, "the notice is cleared");
    assert_eq!(rig.model.snapshot().opening, None);
}

#[gpui::test]
fn the_clone_card_is_disabled_and_does_nothing(cx: &mut TestAppContext) {
    let mut rig = Rig::new();
    with_lists(&mut rig);
    let cx = open(cx, &rig);
    let records = frame(cx);
    // Under Open Folder, with the same width, and disabled.
    let open_folder = bounds(&records, "welcome.open-folder");
    let clone = bounds(&records, "welcome.clone");
    assert!(clone.top() >= open_folder.bottom(), "{clone:?}");
    assert!((clone.size.width - open_folder.size.width).abs() < px(0.5));
    let node = records
        .iter()
        .find(|record| record.semantic.id.as_str() == "welcome.clone")
        .unwrap();
    assert_eq!(node.semantic.role, Role::Button);
    assert!(node.semantic.state.disabled);
    assert_eq!(
        node.semantic.label.as_ref().and_then(|label| label.key),
        Some("welcome-clone")
    );
    assert!(
        !records
            .iter()
            .find(|record| record.semantic.id.as_str() == "welcome.open-folder")
            .unwrap()
            .semantic
            .state
            .disabled
    );

    // A click sends no command and clears no notice.
    click(cx, "welcome.clone");
    assert!(rig.contributions.dispatched.lock().unwrap().is_empty());
    assert_eq!(rig.model.snapshot().opening, None);

    // ↓ walks the rows only; ↵ on the last row opens it, never the card.
    cx.simulate_keystrokes("down down down down down down down");
    assert_eq!(selected(cx), [discovered_row(SCRATCH)]);
    assert!(!tree(cx).contains("welcome.clone selected"));
}

#[gpui::test]
fn arrows_move_selection_across_both_lists(cx: &mut TestAppContext) {
    let mut rig = Rig::new();
    with_lists(&mut rig);
    let cx = open(cx, &rig);
    assert_eq!(selected(cx), Vec::<String>::new());
    let order = [
        recent_row(LECTERN),
        recent_row(NOTES),
        recent_row(BILLING),
        discovered_row(FRESH),
        discovered_row(SCRATCH),
        discovered_row(SCRATCH),
    ];
    for expected in &order {
        cx.simulate_keystrokes("down");
        assert_eq!(&selected(cx), std::slice::from_ref(expected));
    }
    for expected in order[..5].iter().rev().skip(1) {
        cx.simulate_keystrokes("up");
        assert_eq!(&selected(cx), std::slice::from_ref(expected));
    }
    cx.simulate_keystrokes("up up");
    assert_eq!(
        selected(cx),
        [recent_row(LECTERN)],
        "↑ stays on the first item"
    );
}

#[gpui::test]
fn up_without_a_selection_selects_the_last_item(cx: &mut TestAppContext) {
    let mut rig = Rig::new();
    with_lists(&mut rig);
    let cx = open(cx, &rig);
    cx.simulate_keystrokes("up");
    assert_eq!(selected(cx), [discovered_row(SCRATCH)]);
}

#[gpui::test]
fn backspace_removes_the_selected_recent_and_moves_selection(cx: &mut TestAppContext) {
    let mut rig = Rig::new();
    with_lists(&mut rig);
    let cx = open(cx, &rig);
    cx.simulate_keystrokes("down down");
    assert_eq!(selected(cx), [recent_row(NOTES)]);
    cx.simulate_keystrokes("backspace");
    // The next entry takes the position.
    assert_eq!(selected(cx), [recent_row(BILLING)]);
    assert_eq!(rig.recent_paths(), paths(&[LECTERN, BILLING]));
    rig.settle();
    assert_eq!(rig.recent_paths(), paths(&[LECTERN, BILLING]));
    let file = crate::recent::decode(&rig.file().expect("written")).unwrap();
    assert_eq!(file.len(), 2);

    // At the end of the list the previous entry takes over.
    cx.simulate_keystrokes("backspace");
    assert_eq!(selected(cx), [recent_row(LECTERN)]);
    cx.simulate_keystrokes("backspace");
    assert_eq!(selected(cx), Vec::<String>::new());
    assert_eq!(rig.recent_paths(), Vec::<PathBuf>::new());
    cx.run_until_parked();
    assert!(has(&frame(cx), "welcome.recent.empty"));

    // On a discovered folder ⌫ does nothing.
    cx.simulate_keystrokes("down");
    let before = rig.model.snapshot();
    assert_eq!(selected(cx), [discovered_row(FRESH)]);
    cx.simulate_keystrokes("backspace");
    assert_eq!(rig.model.snapshot().discovered, before.discovered);
    assert_eq!(selected(cx), [discovered_row(FRESH)]);
}

#[gpui::test]
fn the_context_menu_selects_its_row_and_does_not_open_it(cx: &mut TestAppContext) {
    let mut rig = Rig::new();
    with_lists(&mut rig);
    let cx = open(cx, &rig);
    // A selection elsewhere moves to the row that was right-clicked.
    cx.simulate_keystrokes("down");
    assert_eq!(selected(cx), [recent_row(LECTERN)]);
    right_click(cx, &recent_row(NOTES));
    assert_eq!(selected(cx), [recent_row(NOTES)]);
    assert!(
        rig.contributions.dispatched.lock().unwrap().is_empty(),
        "a right click is not a click on the row"
    );
    assert_eq!(rig.model.snapshot().opening, None);
    assert_eq!(rig.recent_paths(), paths(&[LECTERN, NOTES, BILLING]));

    // The menu's one item is `RemoveSelected`: it acts on the selected row.
    let item = RemoveSelected;
    cx.dispatch_action(item);
    cx.run_until_parked();
    assert_eq!(rig.recent_paths(), paths(&[LECTERN, BILLING]));
    assert!(rig.contributions.dispatched.lock().unwrap().is_empty());

    // A discovered row has no menu: the right click does not select it.
    right_click(cx, &discovered_row(FRESH));
    assert_eq!(selected(cx), [recent_row(BILLING)]);
}

#[gpui::test]
fn open_error_shows_a_notice_and_clears_on_the_next_open(cx: &mut TestAppContext) {
    let mut rig = Rig::new();
    with_lists(&mut rig);
    let cx = open(cx, &rig);
    click(cx, &recent_row(NOTES));
    rig.emit_open_failed(OpenError::NotFound);
    rig.settle();
    let records = frame(cx);
    let text = render_tree(&records);
    assert!(
        text.contains("welcome.notice Text [welcome-open-failed-not-found]"),
        "{text}"
    );
    assert!(
        text.contains(&format!(
            "{}.name Text \"notes\" kind=folder sessions=0 running=0 missing=true",
            recent_row(NOTES)
        )),
        "{text}"
    );
    assert!(!text.contains("opening=true"));

    // The next open clears the line.
    click(cx, &recent_row(LECTERN));
    assert_eq!(rig.model.snapshot().notice, None);
    assert!(!has(&frame(cx), "welcome.notice"));
    assert_eq!(
        rig.model.snapshot().opening.as_ref().map(|o| o.source),
        Some(OpenSource::Recent)
    );
}

#[gpui::test]
fn discovered_rows_update_on_wake(cx: &mut TestAppContext) {
    let mut rig = Rig::new();
    rig.start_screen();
    rig.start_discovery();
    rig.settle();
    let cx = open(cx, &rig);
    assert!(has(&frame(cx), "welcome.discovered.empty"));

    let fresh = git_key("/Users/example/work/fresh/.git");
    rig.claude
        .set(vec![Sess::new("c1", FRESH, resolved(&fresh, "")).build()]);
    rig.emit(rig.claude.changed());
    rig.settle();
    let records = frame(cx);
    assert!(has(&records, &discovered_row(FRESH)));
    assert!(!has(&records, "welcome.discovered.empty"));
}

#[gpui::test]
fn the_selection_stays_in_place_when_the_lists_change(cx: &mut TestAppContext) {
    let mut rig = Rig::new();
    with_lists(&mut rig);
    let cx = open(cx, &rig);
    cx.simulate_keystrokes("down down down down down");
    assert_eq!(selected(cx), [discovered_row(SCRATCH)]);
    // The newest folder is opened elsewhere: the list shrinks to one.
    rig.projects.add(snapshot(
        &git_key("/Users/example/work/fresh/.git"),
        "fresh",
        FRESH,
        "",
    ));
    rig.emit_opened(&git_key("/Users/example/work/fresh/.git"));
    rig.settle();
    cx.run_until_parked();
    // `fresh` became the first recent entry; the discovered list has the
    // one folder left, and the selection at its position (the last item).
    assert_eq!(selected(cx), [discovered_row(SCRATCH)]);
}

#[gpui::test]
fn language_picker_switches_strings_and_calls_platform(cx: &mut TestAppContext) {
    let rig = Rig::new();
    *rig.prefs.system.lock().unwrap() = vec!["en-US".to_owned()];
    rig.start_screen();
    rig.settle();
    let cx = open(cx, &rig);
    let text = tree(cx);
    assert!(
        text.contains("welcome.language Button [welcome-language] \"Language\" value=system"),
        "{text}"
    );
    assert!(text.contains("welcome.title Text [welcome-title] \"Lectern\""));
    assert!(text.contains("welcome.open-folder Button [welcome-open-folder] \"Open Folder…\""));

    select_language(cx, serde_json::json!({"locale": "zh-CN"}));
    let text = tree(cx);
    assert!(
        text.contains("welcome.language Button [welcome-language] \"语言\" value=zh-CN"),
        "{text}"
    );
    assert!(text.contains("welcome.open-folder Button [welcome-open-folder] \"打开文件夹…\""));
    assert_eq!(
        *rig.prefs.writes.lock().unwrap(),
        [Some("zh-CN".to_owned())]
    );

    select_language(cx, serde_json::json!({"locale": "en"}));
    assert_eq!(
        *rig.prefs.writes.lock().unwrap(),
        [Some("zh-CN".to_owned()), Some("en".to_owned())]
    );
    assert_eq!(language_value(cx), "en");
    assert!(tree(cx).contains("\"Open Folder…\""));

    // Match System: the strings follow the system's languages, the
    // override is removed.
    *rig.prefs.system.lock().unwrap() = vec!["zh-Hans-CN".to_owned()];
    select_language(cx, serde_json::json!({"locale": null}));
    assert_eq!(rig.prefs.writes.lock().unwrap().last(), Some(&None));
    assert_eq!(language_value(cx), "system");
    assert!(tree(cx).contains("\"打开文件夹…\""));
    // Choosing what is chosen writes nothing.
    let writes = rig.prefs.writes.lock().unwrap().len();
    select_language(cx, serde_json::json!({"locale": null}));
    assert_eq!(rig.prefs.writes.lock().unwrap().len(), writes);
    assert_eq!(rig.model.snapshot().notice, None);
}

#[gpui::test]
fn the_picker_starts_on_the_saved_choice(cx: &mut TestAppContext) {
    let rig = Rig::new();
    *rig.prefs.saved.lock().unwrap() = Some("zh-CN".to_owned());
    rig.start_screen();
    rig.settle();
    let cx = open(cx, &rig);
    assert_eq!(language_value(cx), "zh-CN");
}

#[gpui::test]
fn a_failed_language_write_shows_the_notice_once(cx: &mut TestAppContext) {
    let rig = Rig::new();
    rig.start_screen();
    rig.settle();
    let cx = open(cx, &rig);
    *rig.prefs.outcome.lock().unwrap() = Some(Err(PrefError::Superseded));
    select_language(cx, serde_json::json!({"locale": "en"}));
    assert_eq!(
        rig.model.snapshot().notice,
        None,
        "superseded is no failure"
    );
    *rig.prefs.outcome.lock().unwrap() = Some(Err(PrefError::WriteFailed));
    select_language(cx, serde_json::json!({"locale": "zh-CN"}));
    assert_eq!(rig.model.snapshot().notice, Some(Notice::LanguageNotSaved));
    let text = tree(cx);
    assert!(
        text.contains("welcome.notice Text [welcome-language-not-saved]"),
        "{text}"
    );
    // The next open clears it.
    *rig.prefs.outcome.lock().unwrap() = None;
    click(cx, "welcome.open-folder");
    assert_eq!(rig.model.snapshot().notice, None);
}

#[gpui::test]
fn actions_are_triggerable_by_name(cx: &mut TestAppContext) {
    let mut rig = Rig::new();
    with_lists(&mut rig);
    let cx = open(cx, &rig);
    let press = |name: &str, cx: &mut VisualTestContext| {
        let action = cx
            .update(|_, cx| cx.build_action(name, None))
            .unwrap_or_else(|_| panic!("{name} is registered"));
        cx.update(|window, cx| window.dispatch_action(action, cx));
        cx.run_until_parked();
    };
    press("welcome::SelectNext", cx);
    press("welcome::SelectNext", cx);
    assert_eq!(selected(cx), [recent_row(NOTES)]);
    press("welcome::SelectPrevious", cx);
    assert_eq!(selected(cx), [recent_row(LECTERN)]);
    press("welcome::OpenSelected", cx);
    assert_eq!(
        *rig.contributions.dispatched.lock().unwrap(),
        [WorkspaceCommand::OpenProject(PathBuf::from(LECTERN))]
    );
    press("welcome::RemoveSelected", cx);
    assert_eq!(rig.recent_paths(), paths(&[NOTES, BILLING]));

    // The typed actions reach the same handlers.
    cx.dispatch_action(SelectNext);
    cx.dispatch_action(SelectPrevious);
    cx.dispatch_action(OpenSelected);
    cx.dispatch_action(RemoveSelected);
    cx.run_until_parked();

    // `SelectLanguage` carries data: the probe builds it from JSON.
    let build = |value: serde_json::Value, cx: &mut VisualTestContext| {
        cx.update(|_, cx| cx.build_action("welcome::SelectLanguage", Some(value)))
    };
    let zh = build(serde_json::json!({"locale": "zh-CN"}), cx).expect("built from JSON");
    assert!(zh.partial_eq(&SelectLanguage {
        locale: Some(Locale::ZhCn)
    }));
    cx.update(|window, cx| window.dispatch_action(zh, cx));
    cx.run_until_parked();
    assert!(tree(cx).contains("打开文件夹…"));
    assert_eq!(
        *rig.prefs.writes.lock().unwrap(),
        [Some("zh-CN".to_owned())]
    );
    for value in [serde_json::json!({"locale": null}), serde_json::json!({})] {
        let system = build(value, cx).expect("follows the system");
        assert!(system.partial_eq(&SelectLanguage { locale: None }));
    }
    assert!(build(serde_json::json!({"locale": "fr"}), cx).is_err());
    assert!(build(serde_json::json!({"locale": 3}), cx).is_err());
}

#[test]
fn the_bindings_are_the_designs_and_below_content() {
    let rig = Rig::new();
    rig.start_screen();
    let bindings = rig.contributions.bindings();
    assert_eq!(
        bindings,
        [
            (
                "down",
                "welcome::SelectNext".to_owned(),
                Some("Content > welcome")
            ),
            (
                "up",
                "welcome::SelectPrevious".to_owned(),
                Some("Content > welcome")
            ),
            (
                "enter",
                "welcome::OpenSelected".to_owned(),
                Some("Content > welcome")
            ),
            (
                "backspace",
                "welcome::RemoveSelected".to_owned(),
                Some("Content > welcome")
            ),
        ]
    );
}

#[gpui::test]
fn the_columns_follow_the_canvas(cx: &mut TestAppContext) {
    let mut rig = Rig::new();
    with_lists(&mut rig);
    let cx = open(cx, &rig);
    let records = frame(cx);
    let left_width = 820.0 * 360.0 / 680.0;
    let card = bounds(&records, "welcome.open-folder");
    // The card is as wide as it may be (300 pt) and centred in the left
    // column; the title is centred too.
    assert!((card.size.width.as_f32() - 300.0).abs() < 0.5, "{card:?}");
    assert!(
        (card.center().x.as_f32() - left_width / 2.0).abs() < 0.5,
        "{card:?}"
    );
    let title = bounds(&records, "welcome.title");
    assert!(
        (title.center().x.as_f32() - left_width / 2.0).abs() < 1.0,
        "{title:?}"
    );
    // The language choice is at the bottom of the left column.
    let language = bounds(&records, "welcome.language");
    assert!(language.right().as_f32() < left_width, "{language:?}");
    assert!(language.bottom().as_f32() > 520.0 - 40.0, "{language:?}");
    // The rows are in the right column and as wide as it, less its padding.
    let row = bounds(&records, &recent_row(LECTERN));
    assert!(row.left().as_f32() >= left_width, "{row:?}");
    assert!(
        (row.right().as_f32() - (820.0 - 10.0)).abs() < 0.5,
        "{row:?}"
    );
    // The sidebar rows are flat: no gap but the 4 pt between them.
    let next = bounds(&records, &recent_row(NOTES));
    assert!(((next.top() - row.bottom()).as_f32() - 4.0).abs() < 0.5);
}

#[gpui::test]
fn the_selected_row_is_scrolled_into_view(cx: &mut TestAppContext) {
    let mut rig = Rig::new();
    with_lists(&mut rig);
    let cx = open_sized(cx, &rig, size(px(820.0), px(260.0)));
    let last = discovered_row(SCRATCH);
    let records = frame(cx);
    assert!(
        bounds(&records, &last).bottom() > px(260.0),
        "the last row does not fit"
    );
    cx.simulate_keystrokes("up");
    // One frame moves the offset, the next draws at it.
    frame(cx);
    let records = frame(cx);
    let row = bounds(&records, &last);
    assert!(row.bottom() <= px(260.0), "{row:?}");
    assert!(row.top() >= px(0.0), "{row:?}");
    // And back to the top.
    cx.simulate_keystrokes("up up up up up");
    frame(cx);
    let records = frame(cx);
    let first = bounds(&records, &recent_row(LECTERN));
    assert!(first.top() >= px(0.0), "{first:?}");
    assert!(first.bottom() <= px(260.0), "{first:?}");
}
