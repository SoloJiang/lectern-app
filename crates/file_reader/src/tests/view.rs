//! `view_*`: the reading view in a test window, over a session on a real
//! `Engine` (manual executor, temporary files) and the fake services: the
//! first rows, scrolling, the find bar, the history's buttons, Esc, the
//! states without content, the screen's registration and the strings.
//! Nothing here reads a file itself: the engine does, as in the process.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use gpui::{
    AnyWindowHandle, Context, Entity, InteractiveElement, IntoElement, ParentElement, Render,
    ScrollDelta, ScrollWheelEvent, Styled, TestAppContext, VisualTestContext, Window, WindowHandle,
    actions, div, point, px,
};
use lectern_i18n::{Locale, Strings, keys};
use lectern_kernel::testing::TestKernel;
use lectern_services::code_intel::CodeIntel;
use lectern_services::project::{ProjectKey, Projects};
use lectern_services::reader::{Anchor, Reader};
use lectern_services::workspace::{Contributions, Milestone, Target, WorkspaceCommand, screens};
use lectern_syntax::DocState;
use lectern_ui::semantic::{Record, Recorder};
use lectern_ui::{Appearance, init_fixed};

use super::rig::{
    FakeCodeIntel, FakeContributions, FakeProjects, Resources, Tree, file, folder_snapshot,
};
use crate::actions::key_bindings;
use crate::{FileReaderPlugin, ReadingView, Session};

actions!(file_reader_test, [ProbeEscape]);

/// The content region `workspace` gives the screen: the `Content` key
/// context the bindings name, and a stand-in for `workspace`'s Esc.
struct Host {
    child: Option<gpui::AnyView>,
    probe: Arc<AtomicUsize>,
}

impl Render for Host {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let probe = self.probe.clone();
        div()
            .id("content")
            .key_context("Content")
            .size_full()
            .on_action(cx.listener(move |_, _: &ProbeEscape, _, _| {
                probe.fetch_add(1, Ordering::SeqCst);
            }))
            .children(self.child.clone())
    }
}

/// The plugin in a kernel with the mocked services, a project folder with
/// files, and the reading view in a window.
struct Rig {
    res: Resources,
    kernel: TestKernel,
    session: Arc<Session>,
    code_intel: Arc<FakeCodeIntel>,
    contributions: Arc<FakeContributions>,
    tree: Tree,
    key: ProjectKey,
    window: WindowHandle<Host>,
    view: Entity<ReadingView>,
    probe: Arc<AtomicUsize>,
}

/// `lines` lines of `line <n>` and some words to find.
fn numbered(lines: usize) -> String {
    let mut text = String::new();
    for n in 0..lines {
        if n % 10 == 3 {
            text.push_str(&format!("line {n} needle\n"));
        } else {
            text.push_str(&format!("line {n}\n"));
        }
    }
    text
}

impl Rig {
    fn new(cx: &mut TestAppContext) -> Rig {
        // The kernel thread wakes the foreground task; the test scheduler
        // accepts a wake from another thread only with parking allowed.
        cx.executor().allow_parking();
        cx.update(|cx| init_fixed(cx, Strings::of(Locale::En), Appearance::Light, false));
        let res = Resources::new();
        let plugin = FileReaderPlugin::new(res.deps());
        let sessions = plugin.sessions();
        let projects = Arc::new(FakeProjects::default());
        let code_intel = Arc::new(FakeCodeIntel::default());
        let contributions = Arc::new(FakeContributions::default());
        let kernel = TestKernel::new(plugin)
            .mock::<dyn Projects>(projects.clone())
            .mock::<dyn CodeIntel>(code_intel.clone())
            .mock::<dyn Contributions>(contributions.clone());
        kernel.activate().expect("the plugin activates");
        kernel.inner().expect("the kernel").barrier();
        let session = sessions.lock().unwrap().upgrade().expect("a session");
        let tree = Tree::new();
        let key = tree.folder();
        projects.set(folder_snapshot(&key, &tree.root));
        cx.update(|cx| contributions.bind(cx));
        let probe = Arc::new(AtomicUsize::new(0));
        let host = Host {
            child: None,
            probe: probe.clone(),
        };
        let window = cx.add_window(|_, _| host);
        let screen = contributions.screen();
        let view = window
            .update(cx, |host, window, cx| {
                let view = (screen.build)(&Target::None, window, cx);
                host.child = Some(view.clone());
                cx.notify();
                view
            })
            .expect("the window is open");
        let view = view.downcast::<ReadingView>().expect("a reading view");
        Rig {
            res,
            kernel,
            session,
            code_intel,
            contributions,
            tree,
            key,
            window,
            view,
            probe,
        }
    }

    fn visual(&self, cx: &mut TestAppContext) -> VisualTestContext {
        VisualTestContext::from_window(AnyWindowHandle::from(self.window), cx)
    }

    /// Lets the kernel, the executor and the main-thread tasks finish, until
    /// nothing is left (each pass can wake the next).
    fn settle(&self, cx: &mut TestAppContext) {
        for _ in 0..4 {
            self.kernel.inner().expect("the kernel").barrier();
            self.res.settle();
            cx.run_until_parked();
        }
    }

    /// One frame, twice (the view reads what the element measured on its
    /// next render).
    fn draw(&self, cx: &mut TestAppContext) {
        let mut visual = self.visual(cx);
        for _ in 0..2 {
            visual.update(|window, cx| window.draw(cx).clear(cx));
            visual.run_until_parked();
        }
    }

    fn write(&self, rel: &str, text: &str) {
        self.tree.write(rel, text.as_bytes());
    }

    /// `Reader::open` of a file of the project, as the review screen does it.
    fn open(&self, cx: &mut TestAppContext, rel: &str, at: Anchor) {
        self.session.open(file(&self.key, rel, at));
        self.settle(cx);
        self.draw(cx);
        self.settle(cx);
        self.draw(cx);
    }

    fn read<R>(&self, cx: &mut TestAppContext, f: impl FnOnce(&ReadingView) -> R) -> R {
        self.view.read_with(cx, |view, _| f(view))
    }

    /// One frame with the recorder armed: the semantic records.
    fn record(&self, cx: &mut TestAppContext) -> Vec<Record> {
        self.draw(cx);
        let mut visual = self.visual(cx);
        let id = visual.update(|window, _| window.window_handle().window_id());
        Recorder::begin(id);
        visual.update(|window, cx| window.draw(cx).clear(cx));
        Recorder::finish()
    }

    fn scroll(&self, cx: &mut TestAppContext, rows: f32) {
        let mut visual = self.visual(cx);
        let line = self.read(cx, |view| view.metrics.expect("drawn").line_height);
        visual.simulate_event(ScrollWheelEvent {
            position: point(px(300.0), px(200.0)),
            delta: ScrollDelta::Pixels(point(px(0.0), -line * rows)),
            ..ScrollWheelEvent::default()
        });
        visual.run_until_parked();
    }

    fn type_in_find(&self, cx: &mut TestAppContext, text: &str) {
        let field = self.read(cx, |view| view.find.field.clone());
        let mut visual = self.visual(cx);
        visual.update(|window, cx| {
            field.update(cx, |field, cx| field.set_text(text, window, cx));
        });
        self.settle(cx);
        self.draw(cx);
    }
}

fn attr<'a>(record: &'a Record, key: &str) -> Option<&'a str> {
    record
        .semantic
        .attrs
        .iter()
        .find(|(name, _)| *name == key)
        .map(|(_, value)| value.as_ref())
}

fn find_node<'a>(records: &'a [Record], id: &str) -> Option<&'a Record> {
    records.iter().find(|r| r.semantic.id.as_str() == id)
}

#[gpui::test]
fn view_registers_the_screen_and_its_keys(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    assert_eq!(
        rig.contributions.kinds(),
        vec![
            "sidebar-panel:files".to_owned(),
            "sidebar-entry:files".to_owned(),
            "screen:reader".to_owned(),
            "key-bindings:file-reader".to_owned()
        ]
    );
    let keys: Vec<String> = rig.contributions.keys().into_iter().map(|k| k.0).collect();
    assert_eq!(
        keys,
        [
            "cmd-f",
            "cmd-g",
            "cmd-shift-g",
            "cmd-[",
            "cmd-]",
            "escape",
            "down",
            "up",
            "right",
            "left",
            "enter"
        ]
    );
    assert_eq!(key_bindings().len(), keys.len());
    // Esc is the only key that leaves a text box alone, and the reader's
    // only: the Files tree has none (`workspace`'s Esc closes the panel).
    let all = rig.contributions.keys();
    let escape = all.iter().find(|k| k.0 == "escape").unwrap();
    assert_eq!(escape.2.as_deref(), Some("(Content > reader) && !Input"));
    // The tree's keys work only inside the sidebar's region.
    assert!(
        all[6..]
            .iter()
            .all(|k| k.2.as_deref() == Some("Sidebar > files"))
    );
}

#[gpui::test]
fn view_a_request_without_a_view_asks_workspace_for_the_screen(cx: &mut TestAppContext) {
    // A second kernel and session with no view built: the plugin's own
    // wake-up is in the session.
    cx.executor().allow_parking();
    let res = Resources::new();
    let plugin = FileReaderPlugin::new(res.deps());
    let sessions = plugin.sessions();
    let projects = Arc::new(FakeProjects::default());
    let contributions = Arc::new(FakeContributions::default());
    let kernel = TestKernel::new(plugin)
        .mock::<dyn Projects>(projects.clone())
        .mock::<dyn CodeIntel>(Arc::new(FakeCodeIntel::default()))
        .mock::<dyn Contributions>(contributions.clone());
    kernel.activate().unwrap();
    kernel.inner().unwrap().barrier();
    let session = sessions.lock().unwrap().upgrade().unwrap();
    let tree = Tree::new();
    let key = tree.folder();
    projects.set(folder_snapshot(&key, &tree.root));
    session.open(file(&key, "a.rs", Anchor::Start));
    session.open(file(&key, "b.rs", Anchor::Start));
    let pushes = contributions
        .dispatched()
        .into_iter()
        .filter(|command| {
            matches!(command, WorkspaceCommand::Push(route) if route.screen == screens::READER)
        })
        .count();
    // One push per request, and the same route each time: `workspace`
    // ignores a push of the screen that is on top (nav.rs).
    assert_eq!(pushes, 2);
    assert_eq!(session.pending(), 2);
}

#[gpui::test]
fn view_open_lays_out_only_the_rows_in_view(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    rig.write("a.rs", &numbered(5_000));
    rig.open(cx, "a.rs", Anchor::Start);
    let (state, built, drawn, top, offset) = rig.read(cx, |view| {
        (
            view.snapshot.as_ref().map(|s| s.state.clone()),
            view.built_rows,
            view.drawn.len(),
            view.top,
            view.offset,
        )
    });
    assert_eq!(state, Some(DocState::Ready));
    assert!(built > 5 && built < 120, "{built} rows laid out of 5000");
    assert_eq!(built, drawn);
    assert_eq!(top.line_start, 0);
    assert_eq!(offset, px(0.0));
    // The semantic tree has the rows in view only, by their byte offsets,
    // and the first row's number.
    let records = rig.record(cx);
    let rows: Vec<&Record> = records
        .iter()
        .filter(|r| {
            r.semantic
                .id
                .as_str()
                .starts_with("file-reader.lines.item[")
        })
        .collect();
    assert_eq!(rows.len(), built);
    assert_eq!(rows[0].semantic.id.as_str(), "file-reader.lines.item[0.0]");
    assert_eq!(attr(rows[0], "line"), Some("1"));
    let document = records
        .iter()
        .find(|r| {
            r.semantic
                .id
                .as_str()
                .starts_with("file-reader.document.item[")
        })
        .expect("the document node");
    assert_eq!(attr(document, "state"), Some("ready"));
    assert_eq!(attr(document, "mode"), Some("readonly"));
    // The milestone is reported once the first screen has been drawn.
    let mut visual = rig.visual(cx);
    visual.update(|window, cx| window.simulate_next_frame(cx));
    visual.run_until_parked();
    assert_eq!(rig.contributions.milestones(), [Milestone::FileHighlighted]);
    rig.draw(cx);
    let mut visual = rig.visual(cx);
    visual.update(|window, cx| window.simulate_next_frame(cx));
    visual.run_until_parked();
    assert_eq!(rig.contributions.milestones(), [Milestone::FileHighlighted]);
}

#[gpui::test]
fn view_scrolling_moves_the_position_and_the_session_remembers_it(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    rig.write("a.rs", &numbered(2_000));
    rig.write("b.rs", "first\nsecond\n");
    rig.open(cx, "a.rs", Anchor::Start);
    rig.scroll(cx, 25.5);
    rig.draw(cx);
    let (top, offset) = rig.read(cx, |view| (view.top, view.offset));
    assert!(top.line_start > 0, "{top:?}");
    assert!(offset > px(0.0) && offset < px(40.0));
    let records = rig.record(cx);
    let lines = find_node(&records, "file-reader.lines").expect("the list node");
    assert_eq!(
        attr(lines, "top"),
        Some(format!("{}.{}", top.line_start, top.sub_row).as_str())
    );
    // Another file, then back: the position left is the one scrolled to.
    rig.open(cx, "b.rs", Anchor::Start);
    assert_eq!(rig.read(cx, |view| view.top.line_start), 0);
    assert!(rig.session.can_back());
    rig.view.update(cx, |view, cx| view.go_back(cx));
    rig.settle(cx);
    rig.draw(cx);
    let back = rig.read(cx, |view| view.top);
    assert_eq!(back.line_start, top.line_start);
    // Back to the end of the history asks `workspace` to pop.
    rig.view.update(cx, |view, cx| view.go_back(cx));
    assert!(
        rig.contributions
            .dispatched()
            .contains(&WorkspaceCommand::Pop)
    );
}

#[gpui::test]
fn view_scrolling_stops_at_both_ends(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    rig.write("a.rs", &numbered(300));
    rig.open(cx, "a.rs", Anchor::Start);
    rig.scroll(cx, -50.0);
    assert_eq!(
        rig.read(cx, |view| (view.top.line_start, view.offset)),
        (0, px(0.0))
    );
    rig.scroll(cx, 100_000.0);
    rig.draw(cx);
    let (top, offset, built) = rig.read(cx, |view| (view.top, view.offset, view.built_rows));
    assert_eq!(offset, px(0.0), "no pixel offset at the end");
    // The last rows fill the viewport: there are as many as it holds, and
    // the first one is not the last line of the file.
    assert!(built >= 5, "{built}");
    let size = rig.read(cx, |view| view.snapshot.as_ref().map(|s| s.size));
    assert!(top.line_start > 0 && Some(top.line_start) < size);
    rig.scroll(cx, 100_000.0);
    assert_eq!(rig.read(cx, |view| view.top), top);
}

#[gpui::test]
fn view_find_opens_steps_and_closes_with_escape(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    rig.write("a.rs", &numbered(500));
    rig.open(cx, "a.rs", Anchor::Start);
    let mut visual = rig.visual(cx);
    // Esc with no bar: the key goes on to `workspace`.
    cx.update(|cx| {
        cx.bind_keys([gpui::KeyBinding::new(
            "escape",
            ProbeEscape,
            Some("Content"),
        )]);
    });
    visual.simulate_keystrokes("escape");
    assert_eq!(rig.probe.load(Ordering::SeqCst), 1);

    visual.simulate_keystrokes("cmd-f");
    assert!(rig.read(cx, |view| view.find.open));
    rig.draw(cx);
    rig.type_in_find(cx, "needle");
    rig.settle(cx);
    rig.draw(cx);
    // 500 lines, every tenth has the word from the 4th on: 50 matches, the
    // first one taken.
    let (total, current) = rig.read(cx, |view| {
        (view.results().map(|r| r.ranges.len()), view.find.current)
    });
    assert_eq!(total, Some(50));
    assert_eq!(current, Some(0));
    let records = rig.record(cx);
    let count = find_node(&records, "file-reader.find.count").expect("the count node");
    assert_eq!(
        count.semantic.label.as_ref().unwrap().text.as_ref(),
        "1 of 50"
    );
    assert_eq!(
        count.semantic.label.as_ref().unwrap().key,
        Some("file-reader-find-count")
    );

    let mut visual = rig.visual(cx);
    visual.simulate_keystrokes("cmd-g");
    visual.simulate_keystrokes("cmd-g");
    assert_eq!(rig.read(cx, |view| view.find.current), Some(2));
    visual.simulate_keystrokes("cmd-shift-g");
    assert_eq!(rig.read(cx, |view| view.find.current), Some(1));
    visual.simulate_keystrokes("cmd-shift-g");
    visual.simulate_keystrokes("cmd-shift-g");
    assert_eq!(rig.read(cx, |view| view.find.current), Some(49), "wraps");
    // The current match is in view.
    rig.draw(cx);
    let top = rig.read(cx, |view| view.top.line_start);
    assert!(top > 0);
    let found_at = rig.read(cx, |view| {
        let results = view.results().unwrap();
        results.ranges[49].start
    });
    assert!(found_at >= top);

    // Esc closes the bar, and the next Esc is not the view's.
    visual.simulate_keystrokes("escape");
    assert!(!rig.read(cx, |view| view.find.open));
    assert_eq!(rig.probe.load(Ordering::SeqCst), 1, "the bar took it");
    rig.settle(cx);
    assert!(
        rig.read(cx, |view| view.results().is_none()),
        "the matches went"
    );
    visual.simulate_keystrokes("escape");
    assert_eq!(rig.probe.load(Ordering::SeqCst), 2, "now it went on");
}

#[gpui::test]
fn view_find_reports_a_capped_count_and_nothing_found(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    rig.write("a.rs", &numbered(50));
    rig.open(cx, "a.rs", Anchor::Start);
    let mut visual = rig.visual(cx);
    visual.simulate_keystrokes("cmd-f");
    rig.type_in_find(cx, "absent");
    let records = rig.record(cx);
    let count = find_node(&records, "file-reader.find.count").unwrap();
    assert_eq!(
        count.semantic.label.as_ref().unwrap().text.as_ref(),
        "0 of 0"
    );
    // The field empty: no count, no matches.
    rig.type_in_find(cx, "");
    let records = rig.record(cx);
    let count = find_node(&records, "file-reader.find.count").unwrap();
    assert!(count.semantic.label.is_none());
}

#[gpui::test]
fn view_history_buttons_follow_the_history(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    rig.write("a.rs", &numbered(100));
    rig.write("b.rs", &numbered(100));
    rig.open(cx, "a.rs", Anchor::Start);
    let toolbar = |cx: &mut TestAppContext| {
        let screen = rig.contributions.screen();
        cx.update(|cx| (screen.toolbar)(cx))
    };
    // Back is always there (at the start of the history it leaves the
    // reader); forward only with a position ahead.
    let items = toolbar(cx);
    assert_eq!(items.title.as_str(), "a.rs");
    assert_eq!(items.subtitle.as_ref().unwrap().as_str(), "a.rs");
    assert_eq!(items.controls.len(), 2);
    assert!(!rig.read(cx, |view| view.can_forward()));
    rig.open(cx, "b.rs", Anchor::Start);
    assert_eq!(toolbar(cx).title.as_str(), "b.rs");
    rig.view.update(cx, |view, cx| view.go_back(cx));
    rig.settle(cx);
    assert!(rig.read(cx, |view| view.can_forward()));
    assert_eq!(toolbar(cx).title.as_str(), "a.rs");
    // ⌘] goes forward, ⌘[ back, through the keys.
    let mut visual = rig.visual(cx);
    visual.simulate_keystrokes("cmd-]");
    rig.settle(cx);
    assert_eq!(toolbar(cx).title.as_str(), "b.rs");
    assert!(!rig.read(cx, |view| view.can_forward()));
    visual.simulate_keystrokes("cmd-[");
    rig.settle(cx);
    assert_eq!(toolbar(cx).title.as_str(), "a.rs");
    // The buttons' probe nodes: back is always enabled, forward is not
    // while nothing is ahead.
    let enabled = |cx: &mut TestAppContext| -> (bool, bool) {
        let screen = rig.contributions.screen();
        let window = cx.add_window(|_, _| Bar { screen });
        let mut visual = VisualTestContext::from_window(AnyWindowHandle::from(window), cx);
        for _ in 0..2 {
            visual.update(|window, cx| window.draw(cx).clear(cx));
        }
        let id = visual.update(|window, _| window.window_handle().window_id());
        Recorder::begin(id);
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let records = Recorder::finish();
        let disabled = |name: &str| {
            find_node(&records, name)
                .unwrap_or_else(|| panic!("{name} is a node"))
                .semantic
                .state
                .disabled
        };
        (
            !disabled("file-reader.toolbar.back"),
            !disabled("file-reader.toolbar.forward"),
        )
    };
    // Here, at a.rs, b.rs is ahead.
    assert_eq!(enabled(cx), (true, true));
    rig.view.update(cx, |view, cx| view.go_forward(cx));
    rig.settle(cx);
    assert_eq!(enabled(cx), (true, false));
}

/// The toolbar's controls, drawn the way `workspace` draws them.
struct Bar {
    screen: lectern_ui::contrib::Screen,
}

impl Render for Bar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let items = (self.screen.toolbar)(cx);
        div().flex().children(items.controls)
    }
}

#[gpui::test]
fn view_states_without_content_show_one_line(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    rig.open(cx, "gone.rs", Anchor::Start);
    let strings = Strings::of(Locale::En);
    let records = rig.record(cx);
    let message = find_node(&records, "file-reader.message").expect("a message");
    assert_eq!(
        message.semantic.label.as_ref().unwrap().text.as_ref(),
        strings.get(keys::file_reader::MISSING)
    );
    let document = records
        .iter()
        .find(|r| {
            r.semantic
                .id
                .as_str()
                .starts_with("file-reader.document.item[")
        })
        .unwrap();
    assert_eq!(attr(document, "state"), Some("missing"));

    // A directory is not a regular file.
    rig.write("dir/inside.rs", "fn inside() {}\n");
    rig.open(cx, "dir", Anchor::Start);
    let records = rig.record(cx);
    let message = find_node(&records, "file-reader.message").expect("a message");
    assert_eq!(
        message.semantic.label.as_ref().unwrap().text.as_ref(),
        strings.get(keys::file_reader::UNSUPPORTED)
    );
}

#[gpui::test]
fn view_the_strings_exist_in_both_languages(cx: &mut TestAppContext) {
    let _ = cx;
    for locale in [Locale::En, Locale::ZhCn] {
        let strings = Strings::of(locale);
        for key in [
            keys::file_reader::BACK_TOOLTIP,
            keys::file_reader::FORWARD_TOOLTIP,
            keys::file_reader::FIND_PLACEHOLDER,
            keys::file_reader::MISSING,
            keys::file_reader::UNREADABLE,
            keys::file_reader::UNSUPPORTED,
            keys::file_reader::LOADING,
        ] {
            assert!(!strings.get(key).is_empty(), "{}", key.name());
        }
    }
    let zh = Strings::of(Locale::ZhCn);
    assert_ne!(
        zh.get(keys::file_reader::MISSING),
        Strings::of(Locale::En).get(keys::file_reader::MISSING)
    );
}

/// The point over `col` of display row `row` of the code area.
fn over(view: &ReadingView, row: usize, col: usize) -> gpui::Point<gpui::Pixels> {
    let metrics = view.metrics.expect("drawn");
    let area = view.area.get();
    let gutter = metrics.cell_width * view.gutter_cells() as f32;
    point(
        area.origin.x + gutter + metrics.cell_width * (col as f32 + 0.5),
        area.origin.y + metrics.line_height * (row as f32 + 0.5),
    )
}

#[gpui::test]
fn view_hover_prefetches_the_definition_after_the_delay(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    rig.write("a.rs", "fn main() {\n    let value = other(1);\n}\n");
    rig.open(cx, "a.rs", Anchor::Start);
    let hover = |cx: &mut TestAppContext, row: usize, col: usize| {
        rig.view.update(cx, |view, cx| {
            let at = over(view, row, col);
            view.pointer_moved(at, cx);
        });
    };
    let wait = |cx: &mut TestAppContext, by: std::time::Duration| {
        cx.executor().advance_clock(by);
        cx.run_until_parked();
    };
    // On `value` (line 1, column 8): nothing before the delay, one query
    // after it.
    hover(cx, 1, 9);
    wait(cx, crate::view::HOVER_DELAY / 2);
    assert!(rig.code_intel.prefetched().is_empty());
    wait(cx, crate::view::HOVER_DELAY);
    let queries = rig.code_intel.prefetched();
    assert_eq!(queries.len(), 1, "{queries:?}");
    let lectern_services::code_intel::QueryKind::Definition { at, position } = &queries[0].kind
    else {
        panic!("a definition query: {queries:?}");
    };
    assert_eq!(&*at.path, "a.rs");
    assert_eq!((position.line, position.column), (1, 9));
    assert_eq!(queries[0].project, rig.key);

    // Moving on before the delay: nothing. A space: nothing. The same
    // place again does not start another.
    hover(cx, 1, 12);
    wait(cx, crate::view::HOVER_DELAY / 2);
    hover(cx, 2, 0);
    wait(cx, crate::view::HOVER_DELAY * 2);
    hover(cx, 1, 13);
    wait(cx, crate::view::HOVER_DELAY * 2);
    assert_eq!(rig.code_intel.prefetched().len(), 1);
    // `other` starts at column 16: a second query.
    hover(cx, 1, 17);
    wait(cx, crate::view::HOVER_DELAY * 2);
    assert_eq!(rig.code_intel.prefetched().len(), 2);
    // The pointer leaves the code area, or the view scrolls: no query.
    hover(cx, 0, 4);
    rig.view.update(cx, |view, _| view.pointer_left());
    wait(cx, crate::view::HOVER_DELAY * 2);
    assert_eq!(rig.code_intel.prefetched().len(), 2);
}

#[gpui::test]
fn view_ending_the_view_closes_the_document_and_brings_the_push_back(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    rig.write("a.rs", "let a = 1;\n");
    rig.open(cx, "a.rs", Anchor::Start);
    assert!(rig.session.snapshot().is_some());
    assert!(!rig.code_intel.current().is_empty());
    // `workspace` pops the screen: the host lets go of the view (the rig's
    // own handle too).
    let Rig {
        res,
        kernel,
        session,
        code_intel,
        contributions,
        key,
        window,
        view,
        ..
    } = rig;
    drop(view);
    window
        .update(cx, |host, _, cx| {
            host.child = None;
            cx.notify();
        })
        .unwrap();
    cx.update(|cx| cx.refresh_windows());
    // The global of the toolbar held only a weak handle; the entity goes.
    cx.run_until_parked();
    assert!(session.snapshot().is_none(), "the document went");
    assert_eq!(
        code_intel.current().last(),
        Some(&lectern_services::code_intel::Focus::Current(None))
    );
    // The next request asks for the screen again.
    let before = contributions.dispatched().len();
    session.open(file(&key, "a.rs", Anchor::Start));
    let pushed = contributions.dispatched().split_off(before);
    assert!(
        matches!(pushed.as_slice(), [WorkspaceCommand::Push(route)] if route.screen == screens::READER),
        "{pushed:?}"
    );
    let _ = (res, kernel);
}
