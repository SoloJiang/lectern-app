//! The connection thread's half of the request path, without GPUI and
//! without a listening socket.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use image::RgbaImage;
use serde_json::{Value, json};

use crate::protocol::MAX_LINE;
use crate::server::{
    Call, Host, LineRead, Outcome, Session, dir_is_private, read_line, serve_connection,
};
use crate::tree::{Capture, Tree};

/// A main thread that records the calls it gets and answers with the
/// outcome it was given, once.
pub(super) struct FakeHost {
    calls: Mutex<Vec<String>>,
    outcome: Mutex<Option<Outcome>>,
    pub(super) quits: AtomicUsize,
}

impl FakeHost {
    pub(super) fn new() -> FakeHost {
        FakeHost {
            calls: Mutex::new(Vec::new()),
            outcome: Mutex::new(None),
            quits: AtomicUsize::new(0),
        }
    }

    pub(super) fn answering(outcome: Outcome) -> FakeHost {
        let host = FakeHost::new();
        *host.outcome.lock().unwrap() = Some(outcome);
        host
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

impl Host for FakeHost {
    fn submit(&self, call: Call) -> Option<Outcome> {
        self.calls.lock().unwrap().push(format!("{call:?}"));
        self.outcome.lock().unwrap().take()
    }

    fn dispatch_quit(&self) {
        self.quits.fetch_add(1, Ordering::SeqCst);
    }
}

fn session() -> Session {
    Session::new("/probe/dir".into())
}

/// The response to `line` as JSON, and whether `Quit` is to follow.
fn ask(session: &Session, host: &FakeHost, line: &str) -> (Value, bool) {
    let processed = session.process(host, 1, line.as_bytes());
    assert_eq!(processed.line.last(), Some(&b'\n'));
    (
        serde_json::from_slice(&processed.line).unwrap(),
        processed.quit,
    )
}

pub(super) fn request(id: u64, method: &str, params: Value) -> String {
    json!({ "v": 1, "id": id, "method": method, "params": params }).to_string()
}

fn error_kind(response: &Value) -> &str {
    assert_eq!(response["ok"], false, "{response}");
    response["error"]["kind"].as_str().unwrap()
}

fn capture_of(w: u32, h: u32) -> Capture {
    Capture {
        tree: Tree::from_records(super::tree::window(), Vec::new()),
        image: Some(RgbaImage::from_pixel(w, h, image::Rgba([1, 2, 3, 255]))),
    }
}

#[test]
fn hello_goes_to_the_main_thread_and_keeps_the_id() {
    let host = FakeHost::answering(Outcome::Value(json!({ "protocol": 1 })));
    let (response, quit) = ask(&session(), &host, &request(41, "hello", json!({})));
    assert!(!quit);
    assert_eq!(
        (response["ok"].clone(), response["id"].clone()),
        (json!(true), json!(41))
    );
    assert_eq!(response["result"]["protocol"], 1);
    assert_eq!(host.calls(), ["Hello"]);
}

#[test]
fn ui_tree_passes_the_window() {
    let tree = Tree::from_records(super::tree::window(), Vec::new());
    let host = FakeHost::answering(Outcome::Tree(tree));
    let (response, _) = ask(
        &session(),
        &host,
        &request(1, "ui_tree", json!({ "window": 5 })),
    );
    assert_eq!(response["result"]["schema"], "ui-tree/1");
    assert_eq!(response["result"]["nodes"][0]["role"], "window");
    assert_eq!(host.calls(), ["Tree { window: Some(5) }"]);

    // No `params` at all is the defaults.
    let host = FakeHost::answering(Outcome::Error(crate::protocol::ErrorKind::NoWindow));
    let (response, _) = ask(&session(), &host, r#"{"v":1,"id":2,"method":"ui_tree"}"#);
    assert_eq!(error_kind(&response), "no-window");
    assert_eq!(host.calls(), ["Tree { window: None }"]);
}

#[test]
fn an_unknown_method_never_reaches_the_main_thread() {
    let host = FakeHost::new();
    let (response, quit) = ask(&session(), &host, &request(3, "no_such_method", json!({})));
    assert_eq!(error_kind(&response), "unknown-method");
    assert_eq!(response["id"], 3);
    assert!(!quit);
    assert!(host.calls().is_empty());
}

#[test]
fn bad_requests_are_answered_with_bad_request() {
    let host = FakeHost::new();
    let s = session();
    let (response, _) = ask(&s, &host, "not json");
    assert_eq!(
        (error_kind(&response), response["id"].clone()),
        ("bad-request", json!(0))
    );
    let (response, _) = ask(&s, &host, r#"{"v":2,"id":9,"method":"hello"}"#);
    assert_eq!(
        (error_kind(&response), response["id"].clone()),
        ("bad-request", json!(9))
    );
    // A parameter of the wrong type, with its id.
    let (response, _) = ask(&s, &host, &request(10, "ui_tree", json!({ "window": "x" })));
    assert_eq!(
        (error_kind(&response), response["id"].clone()),
        ("bad-request", json!(10))
    );
    assert!(host.calls().is_empty());
}

#[test]
fn quit_answers_first_and_then_everything_is_quitting() {
    let host = FakeHost::new();
    let s = session();
    let (response, quit) = ask(&s, &host, &request(1, "quit", json!({})));
    assert_eq!(response["ok"], true);
    assert_eq!(response["result"], json!({}));
    assert!(
        quit,
        "the connection dispatches Quit after the line is written"
    );
    // The processing itself does not dispatch it.
    assert_eq!(host.quits.load(Ordering::SeqCst), 0);

    let (response, quit) = ask(&s, &host, &request(2, "hello", json!({})));
    assert_eq!(error_kind(&response), "quitting");
    assert!(!quit);
    let (response, _) = ask(&s, &host, &request(3, "no_such_method", json!({})));
    assert_eq!(error_kind(&response), "quitting");
    assert!(host.calls().is_empty());
}

#[test]
fn a_gone_main_thread_is_quitting() {
    let host = FakeHost::new();
    let (response, _) = ask(&session(), &host, &request(1, "hello", json!({})));
    assert_eq!(error_kind(&response), "quitting");
    assert_eq!(host.calls(), ["Hello"]);
}

#[test]
fn screenshot_checks_dir_and_name_before_asking_for_a_frame() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().to_str().unwrap().to_owned();
    let host = FakeHost::new();
    let s = session();
    for params in [
        json!({ "name": "../x" }),
        json!({ "name": "a/b" }),
        json!({ "name": ".." }),
        json!({ "name": "" }),
        json!({ "dir": "relative" }),
        json!({ "dir": format!("{dir}/missing") }),
        json!({ "dir": dir, "name": "a b" }),
    ] {
        let (response, _) = ask(&s, &host, &request(1, "screenshot", params.clone()));
        assert_eq!(error_kind(&response), "io", "{params}");
    }
    assert!(host.calls().is_empty());
    assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0);
}

#[test]
fn screenshot_annotates_and_writes_the_png() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().to_path_buf();
    let host = FakeHost::answering(Outcome::Shot {
        capture: capture_of(40, 30),
        dir: dir.clone(),
        name: "first".into(),
        annotate: true,
    });
    let params = json!({ "dir": dir, "name": "first" });
    let (response, _) = ask(&session(), &host, &request(1, "screenshot", params));
    assert_eq!(response["ok"], true, "{response}");
    let result = &response["result"];
    let path = dir.join("first.png");
    assert_eq!(result["path"], path.to_str().unwrap());
    assert_eq!(
        (result["width"].clone(), result["height"].clone()),
        (json!(40), json!(30))
    );
    assert_eq!(result["scale"], 2.0);
    assert_eq!(result["tree"]["schema"], "ui-tree/1");
    assert_eq!(
        host.calls(),
        [format!(
            "Screenshot {{ window: None, dir: {dir:?}, name: \"first\", annotate: true }}"
        )]
    );

    // The window root is annotated: its border is drawn over the image.
    let written = image::open(&path).unwrap().to_rgba8();
    assert_eq!(written.dimensions(), (40, 30));
    assert_ne!(*written.get_pixel(20, 0), image::Rgba([1, 2, 3, 255]));
}

#[test]
fn screenshot_without_annotation_writes_the_image_as_it_is() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().to_path_buf();
    let host = FakeHost::answering(Outcome::Shot {
        capture: capture_of(40, 30),
        dir: dir.clone(),
        name: "plain".into(),
        annotate: false,
    });
    let params = json!({ "dir": dir, "name": "plain", "annotate": false });
    let (response, _) = ask(&session(), &host, &request(1, "screenshot", params));
    assert_eq!(response["ok"], true, "{response}");
    let written = image::open(dir.join("plain.png")).unwrap().to_rgba8();
    assert!(written.pixels().all(|p| *p == image::Rgba([1, 2, 3, 255])));
    assert!(host.calls()[0].contains("annotate: false"));
}

#[test]
fn screenshot_defaults_to_the_probe_dir_and_a_numbered_name() {
    let tmp = tempfile::tempdir().unwrap();
    let s = Session::new(tmp.path().to_path_buf());
    for want in ["shot-1", "shot-2"] {
        let host = FakeHost::new();
        let (response, _) = ask(&s, &host, &request(1, "screenshot", json!({})));
        // The fake gives no outcome, so the main thread is "gone"; what
        // matters is the call it was asked.
        assert_eq!(error_kind(&response), "quitting");
        let call = &host.calls()[0];
        assert!(call.contains(&format!("name: \"{want}\"")), "{call}");
        assert!(call.contains(&format!("dir: {:?}", tmp.path())), "{call}");
        assert!(call.contains("annotate: true"), "{call}");
    }
}

#[test]
fn screenshot_without_an_image_is_a_render_error() {
    let tmp = tempfile::tempdir().unwrap();
    let mut capture = capture_of(4, 4);
    capture.image = None;
    let host = FakeHost::answering(Outcome::Shot {
        capture,
        dir: tmp.path().to_path_buf(),
        name: "x".into(),
        annotate: true,
    });
    let (response, _) = ask(
        &session(),
        &host,
        &request(1, "screenshot", json!({ "dir": tmp.path() })),
    );
    assert_eq!(error_kind(&response), "render");
}

#[test]
fn screenshot_that_cannot_be_written_is_an_io_error() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("gone");
    std::fs::create_dir(&dir).unwrap();
    let host = FakeHost::answering(Outcome::Shot {
        capture: capture_of(4, 4),
        dir: dir.clone(),
        name: "x".into(),
        annotate: true,
    });
    let s = session();
    // The directory exists when the request is checked and is gone when the
    // frame arrives.
    let params = json!({ "dir": dir, "name": "x" });
    let line = request(1, "screenshot", params);
    struct Removing<'a>(&'a FakeHost, &'a Path);
    impl Host for Removing<'_> {
        fn submit(&self, call: Call) -> Option<Outcome> {
            std::fs::remove_dir(self.1).unwrap();
            self.0.submit(call)
        }
        fn dispatch_quit(&self) {}
    }
    let processed = s.process(&Removing(&host, &dir), 1, line.as_bytes());
    let response: Value = serde_json::from_slice(&processed.line).unwrap();
    assert_eq!(error_kind(&response), "io");
    assert!(!dir.exists());
}

/// A reader that hands out its chunks one `read` at a time, with a
/// `WouldBlock` between two of them where the chunk is `None`.
struct Chunks(std::collections::VecDeque<Option<Vec<u8>>>);

impl Read for Chunks {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        match self.0.pop_front() {
            None => Ok(0),
            Some(None) => Err(io::ErrorKind::WouldBlock.into()),
            Some(Some(mut chunk)) => {
                let n = chunk.len().min(out.len());
                out[..n].copy_from_slice(&chunk[..n]);
                if n < chunk.len() {
                    self.0.push_front(Some(chunk.split_off(n)));
                }
                Ok(n)
            }
        }
    }
}

fn reader(chunks: Vec<Option<&[u8]>>) -> BufReader<Chunks> {
    BufReader::with_capacity(
        4096,
        Chunks(chunks.into_iter().map(|c| c.map(<[u8]>::to_vec)).collect()),
    )
}

#[test]
fn read_line_returns_one_line_at_a_time() {
    let mut r = reader(vec![Some(b"one\ntwo\n")]);
    let mut buf = Vec::new();
    assert!(matches!(read_line(&mut r, &mut buf), LineRead::Line));
    assert_eq!(buf, b"one");
    buf.clear();
    assert!(matches!(read_line(&mut r, &mut buf), LineRead::Line));
    assert_eq!(buf, b"two");
    buf.clear();
    assert!(matches!(read_line(&mut r, &mut buf), LineRead::Eof));
}

#[test]
fn read_line_continues_a_partial_line_after_an_idle_wait() {
    let mut r = reader(vec![Some(b"par"), None, Some(b"tial\n")]);
    let mut buf = Vec::new();
    assert!(matches!(
        read_line(&mut r, &mut buf),
        LineRead::Line | LineRead::Idle
    ));
    // Whatever the first call returned, the data is kept and the line comes
    // out whole.
    while !matches!(read_line(&mut r, &mut buf), LineRead::Line) {}
    assert_eq!(buf, b"partial");
}

#[test]
fn read_line_idles_without_data() {
    let mut r = reader(vec![None]);
    let mut buf = Vec::new();
    assert!(matches!(read_line(&mut r, &mut buf), LineRead::Idle));
    assert!(buf.is_empty());
}

#[test]
fn read_line_takes_a_line_of_exactly_the_limit_and_cuts_a_longer_one() {
    let mut exact = vec![b'a'; MAX_LINE];
    exact.push(b'\n');
    let mut r = reader(vec![Some(&exact)]);
    let mut buf = Vec::new();
    assert!(matches!(read_line(&mut r, &mut buf), LineRead::Line));
    assert_eq!(buf.len(), MAX_LINE);

    let longer = vec![b'a'; MAX_LINE + 10];
    let mut r = reader(vec![Some(&longer)]);
    let mut buf = Vec::new();
    let mut result = read_line(&mut r, &mut buf);
    while matches!(result, LineRead::Idle) {
        result = read_line(&mut r, &mut buf);
    }
    assert!(matches!(result, LineRead::TooLong));
}

#[test]
fn read_line_gives_a_last_line_without_a_newline() {
    let mut r = reader(vec![Some(b"tail")]);
    let mut buf = Vec::new();
    assert!(matches!(read_line(&mut r, &mut buf), LineRead::Line));
    assert_eq!(buf, b"tail");
}

#[test]
fn the_probe_directory_must_be_private() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(dir_is_private(dir));
    for mode in [0o755, 0o750, 0o770, 0o701, 0o500, 0o777] {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(mode)).unwrap();
        assert!(!dir_is_private(dir), "{mode:o}");
    }
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(!dir_is_private(&dir.join("missing")));
    assert!(!dir_is_private(Path::new("relative")));
    let file = dir.join("f");
    std::fs::write(&file, b"").unwrap();
    assert!(!dir_is_private(&file));
}

/// A connection over a socket pair, served by a thread.
fn serve(
    host: std::sync::Arc<FakeHost>,
    session: std::sync::Arc<Session>,
) -> (UnixStream, std::thread::JoinHandle<()>) {
    let (client, server) = UnixStream::pair().unwrap();
    let handle = std::thread::spawn(move || serve_connection(server, &session, host.as_ref(), 1));
    (client, handle)
}

#[test]
fn a_connection_answers_requests_in_order_and_dispatches_quit_after_the_line() {
    let host = std::sync::Arc::new(FakeHost::new());
    let s = std::sync::Arc::new(session());
    let (mut client, thread) = serve(host.clone(), s);
    client
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut reader = BufReader::new(client.try_clone().unwrap());

    // Two requests in one write, answered one line each, in order.
    let batch = format!(
        "{}\n{}\n",
        request(1, "no_such_method", json!({})),
        request(2, "quit", json!({}))
    );
    client.write_all(batch.as_bytes()).unwrap();
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let first: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(
        (first["id"].clone(), error_kind(&first)),
        (json!(1), "unknown-method")
    );
    line.clear();
    reader.read_line(&mut line).unwrap();
    let second: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(
        (second["id"].clone(), second["ok"].clone()),
        (json!(2), json!(true))
    );

    drop(reader);
    drop(client);
    thread.join().unwrap();
    assert_eq!(host.quits.load(Ordering::SeqCst), 1);
}

#[test]
fn a_connection_ends_when_a_line_is_too_long() {
    let host = std::sync::Arc::new(FakeHost::new());
    let (mut client, thread) = serve(host.clone(), std::sync::Arc::new(session()));
    client
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let junk = vec![b'x'; MAX_LINE + 2];
    // The server closes while we write; the write may fail half way.
    let _ = client.write_all(&junk);
    let mut rest = Vec::new();
    let _ = client.read_to_end(&mut rest);
    assert!(rest.is_empty(), "no response to an oversized line");
    thread.join().unwrap();
    assert!(host.calls().is_empty());
}

#[test]
fn closing_the_connections_ends_the_threads_blocked_reading_them() {
    let host = std::sync::Arc::new(FakeHost::new());
    let s = std::sync::Arc::new(session());
    let (client, server) = UnixStream::pair().unwrap();
    s.register(1, &server);
    let thread = {
        let (s, host) = (s.clone(), host.clone());
        std::thread::spawn(move || serve_connection(server, &s, host.as_ref(), 1))
    };
    // The client stays connected and says nothing; the thread is blocked.
    s.close_connections();
    thread.join().unwrap();
    drop(client);

    // A connection that arrives after the stop is closed at once.
    s.stop.store(true, Ordering::Release);
    let (mut late, server) = UnixStream::pair().unwrap();
    s.register(2, &server);
    let mut rest = Vec::new();
    late.read_to_end(&mut rest).unwrap();
    assert!(rest.is_empty());
}
