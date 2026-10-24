//! The probe against GPUI's test platform: the recorder over real frames,
//! `capture`'s two frames, the main thread's handlers, and a whole session
//! on a real socket.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    AnyElement, Context, IntoElement, ParentElement, Render, Styled, TestAppContext,
    VisualTestContext, Window, actions, div, px,
};
use lectern_i18n::{Locale, Strings, keys};
use lectern_ui::semantic::{NodeId, Recorder, Role, Semantic, State, semantic};
use lectern_ui::{Appearance, Text, init_fixed};
use serde_json::json;

use crate::client::Client;
use crate::protocol::ErrorKind;
use crate::server::{Call, Config, Main, Outcome, Probe, SOCKET, dispatch_quit, handle};
use crate::tree::{CaptureError, capture, snapshot};

type SceneFn = Box<dyn Fn() -> AnyElement>;

struct Scene(SceneFn);

impl Render for Scene {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        (self.0)()
    }
}

fn verbatim(text: &str) -> Text {
    Text::verbatim(Arc::<str>::from(text))
}

/// Three nested levels: a panel with a list with two items, the first
/// selected, labelled by a Fluent key, and an attribute; plus a button
/// beside the list.
fn nested() -> AnyElement {
    let strings = Strings::of(Locale::En);
    let outer = NodeId::new("probe", "outer", None);
    let list = outer.child("list");
    semantic(
        Semantic::new(outer.clone(), Role::Panel),
        div()
            .w(px(300.0))
            .h(px(200.0))
            .child(semantic(
                Semantic::new(list.clone(), Role::List),
                div()
                    .w(px(100.0))
                    .h(px(60.0))
                    .child(semantic(
                        Semantic::new(list.item("a"), Role::ListItem)
                            .label(&verbatim("Alpha"))
                            .state(State {
                                selected: true,
                                ..State::default()
                            })
                            .attr("seen", "true"),
                        div().w(px(100.0)).h(px(20.0)),
                    ))
                    .child(semantic(
                        Semantic::new(list.item("b"), Role::ListItem).label(&verbatim("Beta")),
                        div().w(px(100.0)).h(px(20.0)),
                    )),
            ))
            .child(semantic(
                Semantic::new(outer.child("ok"), Role::Button)
                    .label(&Text::key(&strings, keys::common::OK)),
                div().w(px(40.0)).h(px(20.0)),
            )),
    )
    .into_any_element()
}

fn scene(
    cx: &mut TestAppContext,
    build: impl Fn() -> AnyElement + 'static,
) -> &mut VisualTestContext {
    cx.update(|cx| init_fixed(cx, Strings::of(Locale::En), Appearance::Light, false));
    let (_, cx) = cx.add_window_view(|_, _| Scene(Box::new(build)));
    cx.run_until_parked();
    cx
}

fn draw(cx: &mut VisualTestContext) {
    cx.update(|window, cx| window.draw(cx).clear(cx));
}

/// Delivers the next frame request: runs the callbacks scheduled with
/// `on_next_frame`. The first one of a capture marks the window dirty, and
/// GPUI's test context draws a dirty window when an update ends, which is
/// the frame the capture records.
fn next_frame(cx: &mut VisualTestContext) -> usize {
    cx.update(|window, cx| window.simulate_next_frame(cx))
}

#[gpui::test]
fn snapshot_of_nested_semantics(cx: &mut TestAppContext) {
    let cx = scene(cx, nested);
    let tree = cx.update(snapshot);
    assert!(!Recorder::is_armed());
    let ids: Vec<_> = tree
        .nodes
        .iter()
        .map(|n| (n.n, n.parent, n.id.as_str()))
        .collect();
    assert_eq!(ids[1], (1, Some(0), "probe.outer"));
    assert_eq!(ids[2], (2, Some(1), "probe.outer.list"));
    assert_eq!(ids[3], (3, Some(2), "probe.outer.list.item[a]"));
    assert!(tree.warnings.is_empty());
    insta::assert_snapshot!(tree.to_json());
}

#[gpui::test]
fn records_only_while_armed(cx: &mut TestAppContext) {
    let cx = scene(cx, nested);
    assert!(!Recorder::is_armed());
    draw(cx);
    assert!(
        Recorder::finish().is_empty(),
        "a frame drawn without begin records nothing"
    );
    // `snapshot` arms and disarms around its own frame.
    let tree = cx.update(snapshot);
    assert_eq!(tree.nodes.len(), 6);
    draw(cx);
    assert!(Recorder::finish().is_empty());
}

#[gpui::test]
fn capture_uses_two_frames(cx: &mut TestAppContext) {
    let cx = scene(cx, nested);
    let got = Rc::new(RefCell::new(None));
    let _cancel = cx.update(|window, _| {
        let got = got.clone();
        capture(window, false, move |result| {
            *got.borrow_mut() = Some(result)
        })
    });
    assert!(
        !Recorder::is_armed(),
        "nothing is armed before the first frame"
    );

    // The first frame request arms the recorder, before the draw.
    assert_eq!(next_frame(cx), 1);
    assert!(Recorder::is_armed());
    assert!(got.borrow().is_none(), "one frame is not enough");

    // The second one, after the draw, builds the tree and disarms.
    assert_eq!(next_frame(cx), 1);
    assert!(!Recorder::is_armed());
    let capture = got
        .borrow_mut()
        .take()
        .expect("done was called")
        .ok()
        .unwrap();
    assert!(capture.image.is_none());
    assert_eq!(capture.tree.nodes.len(), 6);
    assert_eq!(capture.tree.nodes[3].id, "probe.outer.list.item[a]");
    // Nothing is left over.
    assert_eq!(next_frame(cx), 0);
}

#[gpui::test]
fn capture_without_a_renderer_reports_render(cx: &mut TestAppContext) {
    let cx = scene(cx, nested);
    let got = Rc::new(RefCell::new(None));
    let _cancel = cx.update(|window, _| {
        let got = got.clone();
        capture(window, true, move |result| *got.borrow_mut() = Some(result))
    });
    next_frame(cx);
    next_frame(cx);
    // The test platform has no renderer, so the image cannot be read back;
    // a real window can.
    let error = got.borrow_mut().take().expect("done was called").err();
    assert_eq!(error, Some(CaptureError::Render));
    assert!(!Recorder::is_armed());
}

#[gpui::test]
fn a_cancelled_capture_disarms_and_never_calls_done(cx: &mut TestAppContext) {
    let cx = scene(cx, nested);
    let called = Rc::new(Cell::new(false));
    let cancel = cx.update(|window, _| {
        let called = called.clone();
        capture(window, false, move |_| called.set(true))
    });
    next_frame(cx);
    assert!(Recorder::is_armed());
    cancel.cancel();
    assert!(!Recorder::is_armed(), "cancel disarms a recording capture");
    next_frame(cx);
    assert!(!called.get());

    // Cancelled before the first frame: the first frame does not arm.
    let cancel = cx.update(|window, _| capture(window, false, |_| {}));
    cancel.cancel();
    next_frame(cx);
    assert!(!Recorder::is_armed());
}

fn main_of(dir: &std::path::Path, timeout: Duration) -> Rc<Main> {
    Rc::new(Main {
        dir: dir.to_path_buf(),
        frame_timeout: timeout,
    })
}

/// Runs `handle(call)` as a foreground task and returns its slot.
fn run_handle(
    cx: &mut VisualTestContext,
    main: &Rc<Main>,
    call: Call,
) -> (Rc<RefCell<Option<Outcome>>>, gpui::Task<()>) {
    let slot = Rc::new(RefCell::new(None));
    let task = cx.update(|_, cx| {
        let (slot, main) = (slot.clone(), main.clone());
        cx.spawn(async move |cx| {
            let outcome = handle(cx, &main, call).await;
            *slot.borrow_mut() = Some(outcome);
        })
    });
    cx.run_until_parked();
    (slot, task)
}

#[gpui::test]
fn handle_ui_tree_waits_for_two_frames(cx: &mut TestAppContext) {
    let cx = scene(cx, nested);
    let tmp = tempfile::tempdir().unwrap();
    let main = main_of(tmp.path(), Duration::from_secs(5));
    let (slot, _task) = run_handle(cx, &main, Call::Tree { window: None });
    assert!(slot.borrow().is_none());
    next_frame(cx);
    cx.run_until_parked();
    assert!(slot.borrow().is_none(), "one frame is not enough");
    next_frame(cx);
    cx.run_until_parked();
    let Some(Outcome::Tree(tree)) = slot.borrow_mut().take() else {
        panic!("the outcome is a tree");
    };
    assert_eq!(tree.nodes.len(), 6);
}

#[gpui::test]
fn handle_screenshot_carries_the_parameters_through(cx: &mut TestAppContext) {
    let cx = scene(cx, nested);
    let tmp = tempfile::tempdir().unwrap();
    let main = main_of(tmp.path(), Duration::from_secs(5));
    let call = Call::Screenshot {
        window: None,
        dir: tmp.path().to_path_buf(),
        name: "n".into(),
        annotate: false,
    };
    let (slot, _task) = run_handle(cx, &main, call);
    next_frame(cx);
    next_frame(cx);
    cx.run_until_parked();
    // The test platform cannot render to an image.
    assert!(matches!(
        slot.borrow_mut().take(),
        Some(Outcome::Error(ErrorKind::Render))
    ));
}

#[gpui::test]
fn handle_times_out_and_disarms_a_recording_that_never_finished(cx: &mut TestAppContext) {
    let cx = scene(cx, nested);
    let tmp = tempfile::tempdir().unwrap();
    let main = main_of(tmp.path(), Duration::from_millis(50));
    let (slot, _task) = run_handle(cx, &main, Call::Tree { window: None });
    next_frame(cx);
    assert!(Recorder::is_armed(), "the first frame armed the recorder");
    cx.executor().advance_clock(Duration::from_millis(100));
    cx.run_until_parked();
    assert!(matches!(
        slot.borrow_mut().take(),
        Some(Outcome::Error(ErrorKind::Timeout))
    ));
    assert!(!Recorder::is_armed(), "the timeout disarmed it");
    // A late second frame finds nothing to do.
    next_frame(cx);
    assert!(!Recorder::is_armed());
}

#[gpui::test]
fn handle_times_out_before_the_first_frame(cx: &mut TestAppContext) {
    let cx = scene(cx, nested);
    let tmp = tempfile::tempdir().unwrap();
    let main = main_of(tmp.path(), Duration::from_millis(50));
    let (slot, _task) = run_handle(cx, &main, Call::Tree { window: None });
    cx.executor().advance_clock(Duration::from_millis(100));
    cx.run_until_parked();
    assert!(matches!(
        slot.borrow_mut().take(),
        Some(Outcome::Error(ErrorKind::Timeout))
    ));
    // The first frame that comes later does not arm the recorder.
    next_frame(cx);
    assert!(!Recorder::is_armed());
}

#[gpui::test]
fn handle_names_a_window_that_does_not_exist(cx: &mut TestAppContext) {
    let cx = scene(cx, nested);
    let tmp = tempfile::tempdir().unwrap();
    let main = main_of(tmp.path(), Duration::from_secs(5));
    let (slot, _task) = run_handle(
        cx,
        &main,
        Call::Tree {
            window: Some(12345),
        },
    );
    assert!(matches!(
        slot.borrow_mut().take(),
        Some(Outcome::Error(ErrorKind::NoWindow))
    ));
}

#[gpui::test]
fn hello_lists_the_windows(cx: &mut TestAppContext) {
    let cx = scene(cx, nested);
    let tmp = tempfile::tempdir().unwrap();
    let main = main_of(tmp.path(), Duration::from_secs(5));
    let (slot, _task) = run_handle(cx, &main, Call::Hello);
    let Some(Outcome::Value(value)) = slot.borrow_mut().take() else {
        panic!("the outcome is a value");
    };
    assert_eq!(value["protocol"], 1);
    assert_eq!(value["schemas"], json!(["ui-tree/1"]));
    assert_eq!(value["dir"], tmp.path().to_str().unwrap());
    assert_eq!(value["pid"], std::process::id());
    let windows = value["windows"].as_array().unwrap();
    assert_eq!(windows.len(), 1);
    let window = &windows[0];
    assert!(window["id"].as_u64().is_some());
    assert!(window["size"]["w"].as_f64().unwrap() > 0.0);
    assert!(window["scale"].as_f64().unwrap() >= 1.0);
}

actions!(lectern, [Quit]);

#[gpui::test]
fn quit_dispatches_the_applications_quit_action_by_name(cx: &mut TestAppContext) {
    let quits = Rc::new(Cell::new(0));
    cx.update(|cx| {
        let quits = quits.clone();
        cx.on_action(move |_: &Quit, _| quits.set(quits.get() + 1));
    });
    let cx = scene(cx, nested);
    cx.update(|_, cx| dispatch_quit(cx));
    assert_eq!(quits.get(), 1);
}

#[gpui::test]
fn start_refuses_a_directory_that_is_not_private(cx: &mut TestAppContext) {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::set_permissions(
        tmp.path(),
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    let result = cx.update(|cx| Probe::start(cx, Config::new(tmp.path().to_path_buf())));
    assert_eq!(result.err().map(|e| e.reason()), Some("dir-not-private"));
    assert!(!tmp.path().join(SOCKET).exists());
}

/// A whole session over a real socket: a client thread asks, the test pumps
/// the frames and the foreground task.
#[gpui::test]
fn a_session_answers_hello_ui_tree_screenshot_and_quit(cx: &mut TestAppContext) {
    // The connection threads wake the foreground task from their own
    // threads, which the test scheduler must be told is expected.
    cx.executor().allow_parking();
    let quits = Rc::new(Cell::new(0));
    cx.update(|cx| {
        let quits = quits.clone();
        cx.on_action(move |_: &Quit, _| quits.set(quits.get() + 1));
    });
    let cx = scene(cx, nested);
    let tmp = tempfile::tempdir().unwrap();
    std::fs::set_permissions(
        tmp.path(),
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .unwrap();
    let probe = cx
        .update(|_, cx| Probe::start(cx, Config::new(tmp.path().to_path_buf())))
        .expect("the probe starts");
    let socket = tmp.path().join(SOCKET);
    assert_eq!(
        std::os::unix::fs::MetadataExt::mode(&std::fs::metadata(&socket).unwrap()) & 0o777,
        0o600
    );

    let dir = tmp.path().to_path_buf();
    let client = std::thread::spawn(move || {
        let mut client = Client::connect(&dir, Duration::from_secs(30)).unwrap();
        let hello = client.call("hello", json!({})).unwrap();
        let tree = client.call("ui_tree", json!({})).unwrap();
        let bad_name = client
            .call("screenshot", json!({ "name": "../x" }))
            .unwrap();
        let shot = client.call("screenshot", json!({})).unwrap();
        let unknown = client.call("nope", json!({})).unwrap();
        let quit = client.call("quit", json!({})).unwrap();
        let after = client.call("hello", json!({})).unwrap();
        (hello, tree, bad_name, shot, unknown, quit, after)
    });

    // Pump: let the foreground task run, and give every capture its frames.
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while !client.is_finished() {
        assert!(
            std::time::Instant::now() < deadline,
            "the session did not finish"
        );
        cx.run_until_parked();
        next_frame(cx);
        std::thread::sleep(Duration::from_millis(2));
    }
    let (hello, tree, bad_name, shot, unknown, quit, after) = client.join().unwrap();
    cx.run_until_parked();

    assert!(hello.ok);
    let result = hello.result.unwrap();
    assert_eq!(result["windows"].as_array().unwrap().len(), 1);
    assert!(tree.ok);
    let result = tree.result.unwrap();
    assert_eq!(result["schema"], "ui-tree/1");
    assert_eq!(result["nodes"].as_array().unwrap().len(), 6);
    assert_eq!(result["nodes"][3]["id"], "probe.outer.list.item[a]");
    assert_eq!(result["nodes"][3]["label"]["text"], "Alpha");
    assert_eq!(result["nodes"][5]["label"]["key"], "common-ok");
    let kind = |reply: &crate::client::Reply| reply.error.as_ref().map(|e| e.kind.clone());
    assert_eq!(kind(&bad_name).as_deref(), Some("io"));
    // The test platform cannot render an image.
    assert_eq!(kind(&shot).as_deref(), Some("render"));
    assert_eq!(kind(&unknown).as_deref(), Some("unknown-method"));
    assert!(quit.ok);
    assert_eq!(kind(&after).as_deref(), Some("quitting"));
    assert_eq!(quits.get(), 1, "quit dispatched the Quit action once");
    assert!(!Recorder::is_armed());

    // Shutting down removes the socket and stops the threads.
    drop(probe);
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while socket.exists() {
        assert!(std::time::Instant::now() < deadline, "the socket stays");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(Client::connect(tmp.path(), Duration::from_secs(1)).is_err());
}

/// `app`'s quit thread calls `Probe::shutdown` while the main thread holds
/// the `Probe` (docs/design/ui_probe.md「对其他 crate 的要求」第 3 项).
#[test]
fn probe_can_be_shut_down_from_another_thread() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Probe>();
}
