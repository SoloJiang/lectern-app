//! The probe session (docs/design/ui_probe.md「探针会话」「数据流与线程」):
//! a Unix domain socket in the probe directory, a listener thread, a short
//! connection thread per client, and a foreground task on the main thread
//! that answers the requests one at a time.
//!
//! The request path is split so the parts that need no GPUI can be tested
//! without a socket: `Session::process` takes a request line and a `Host`
//! (the queue to the main thread in the application, a fake in the tests)
//! and returns the response line.

use std::collections::HashMap;
use std::io::{self, BufRead, Read as _, Write};
use std::net::Shutdown;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use futures::StreamExt;
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use futures::channel::oneshot;
use gpui::{AnyWindowHandle, App, AsyncApp, FutureExt, Task};
use lectern_logging::{Ctx, Value, debug, info, scope, warn};
use serde::de::DeserializeOwned;
use serde_json::json;

use crate::annotate::{annotate, write_png};
use crate::protocol::{
    ErrorKind, MAX_LINE, PROTOCOL, Request, Response, ScreenshotParams, TREE_SCHEMA, TreeParams,
    check_dir, check_name, parse_request,
};
use crate::tree::{Capture, CaptureError, Tree, WindowInfo, capture};

/// The socket's file name inside the probe directory.
pub const SOCKET: &str = "probe.sock";

/// The name of the application's `Quit` action (`actions!(lectern, [Quit])`
/// in `app`): `quit` dispatches it by name, as ⌘Q does, because this crate
/// cannot depend on `app`.
pub const QUIT_ACTION: &str = "lectern::Quit";

/// The default wait for a frame.
pub const DEFAULT_FRAME_TIMEOUT: Duration = Duration::from_secs(5);

pub struct Config {
    /// The probe directory, which `app` reads from `LECTERN_UI_PROBE_DIR`;
    /// an absolute path.
    pub dir: PathBuf,
    /// How long the main thread waits for a frame. A window that is not in
    /// front is asked for frames at 30 fps, a covered one more slowly.
    pub frame_timeout: Duration,
}

impl Config {
    pub fn new(dir: PathBuf) -> Config {
        Config {
            dir,
            frame_timeout: DEFAULT_FRAME_TIMEOUT,
        }
    }
}

#[derive(Debug)]
pub enum StartError {
    /// The directory is missing, not absolute, not a directory, not owned by
    /// the current user, or accessible to others (its mode is not 0700).
    DirNotPrivate,
    /// `bind` failed; so did removing a `probe.sock` left from before.
    Bind(io::Error),
    Thread(io::Error),
}

impl StartError {
    /// The `reason` of `app`'s `probe start failed` record.
    pub fn reason(&self) -> &'static str {
        match self {
            StartError::DirNotPrivate => "dir-not-private",
            StartError::Bind(_) => "bind",
            StartError::Thread(_) => "thread",
        }
    }

    pub fn io(&self) -> Option<&io::Error> {
        match self {
            StartError::DirNotPrivate => None,
            StartError::Bind(e) | StartError::Thread(e) => Some(e),
        }
    }
}

/// A call the main thread answers.
#[derive(Debug)]
pub(crate) enum Call {
    Hello,
    Tree {
        window: Option<u64>,
    },
    Screenshot {
        window: Option<u64>,
        dir: PathBuf,
        name: String,
        annotate: bool,
    },
}

/// What the main thread hands back; the connection thread turns it into a
/// response, doing the work that needs no GPUI (annotating, encoding,
/// writing).
pub(crate) enum Outcome {
    Value(serde_json::Value),
    Tree(Tree),
    Shot {
        capture: Capture,
        dir: PathBuf,
        name: String,
        annotate: bool,
    },
    Error(ErrorKind),
}

/// The side of the request path that is not the connection thread.
pub(crate) trait Host: Send + Sync {
    /// Runs `call` on the main thread and waits for the outcome; None when
    /// the main thread is gone.
    fn submit(&self, call: Call) -> Option<Outcome>;
    /// Dispatches the application's `Quit` action on the main thread.
    fn dispatch_quit(&self);
}

/// One request through the connection thread's half of the path.
pub(crate) struct Processed {
    /// The response line, with its newline.
    pub(crate) line: Vec<u8>,
    /// Write `line`, then dispatch `Quit`.
    pub(crate) quit: bool,
}

/// What the session shares between its threads.
pub(crate) struct Session {
    dir: PathBuf,
    pub(crate) stop: AtomicBool,
    /// A handle on every open connection, so `shutdown` can end the threads
    /// blocked reading them.
    streams: Mutex<HashMap<u64, UnixStream>>,
    quitting: AtomicBool,
    conns: AtomicU64,
    requests: AtomicU64,
    shots: AtomicU64,
}

/// The number of requests this process has answered; `Ctx::req`.
static REQ: AtomicU64 = AtomicU64::new(0);

struct Done {
    response: Response,
    method: &'static str,
    nodes: Option<usize>,
    shot: Option<ShotDone>,
    /// The I/O error behind an `io` response.
    io: Option<io::Error>,
    quit: bool,
}

struct ShotDone {
    path: PathBuf,
    width: u32,
    height: u32,
}

/// Why a request failed, with the I/O error behind it when there is one.
struct Failure {
    kind: ErrorKind,
    io: Option<io::Error>,
}

impl From<ErrorKind> for Failure {
    fn from(kind: ErrorKind) -> Failure {
        Failure { kind, io: None }
    }
}

/// The method names as `'static` log values: a request's own method string
/// is never logged.
fn method_name(method: &str) -> &'static str {
    match method {
        "hello" => "hello",
        "ui_tree" => "ui_tree",
        "screenshot" => "screenshot",
        "quit" => "quit",
        _ => "unknown",
    }
}

/// `params` as `T`; absent (`null`) parameters are the defaults.
fn params<T: DeserializeOwned + Default>(value: serde_json::Value) -> Result<T, ErrorKind> {
    if value.is_null() {
        return Ok(T::default());
    }
    serde_json::from_value(value).map_err(|_| ErrorKind::BadRequest)
}

impl Session {
    pub(crate) fn new(dir: PathBuf) -> Session {
        Session {
            dir,
            stop: AtomicBool::new(false),
            streams: Mutex::new(HashMap::new()),
            quitting: AtomicBool::new(false),
            conns: AtomicU64::new(0),
            requests: AtomicU64::new(0),
            shots: AtomicU64::new(0),
        }
    }

    /// Keeps a handle on connection `conn` for `close_connections`; a
    /// connection that arrives after the stop is closed at once.
    pub(crate) fn register(&self, conn: u64, stream: &UnixStream) {
        if self.stop.load(Ordering::Acquire) {
            let _ = stream.shutdown(Shutdown::Both);
        } else if let Ok(handle) = stream.try_clone() {
            self.streams
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(conn, handle);
        }
    }

    pub(crate) fn unregister(&self, conn: u64) {
        self.streams
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&conn);
    }

    /// Ends every open connection: the threads blocked reading them see the
    /// end of the stream.
    pub(crate) fn close_connections(&self) {
        let streams = std::mem::take(&mut *self.streams.lock().unwrap_or_else(|e| e.into_inner()));
        for stream in streams.values() {
            let _ = stream.shutdown(Shutdown::Both);
        }
    }

    /// Answers one request line. Runs in the request's correlation scope
    /// and blocks while the main thread works, so it is for a connection
    /// thread only.
    pub(crate) fn process(&self, host: &dyn Host, conn: u64, line: &[u8]) -> Processed {
        let started = Instant::now();
        self.requests.fetch_add(1, Ordering::Relaxed);
        let req = REQ.fetch_add(1, Ordering::Relaxed) + 1;
        let _scope = scope(Ctx {
            req: Some(req),
            ..Ctx::default()
        });
        let done = self.respond(host, line);
        let bytes = done.response.to_line();
        if let Some(error) = &done.response.error {
            warn!("probe request failed"; method = done.method, error = error.kind, io = done.io);
        } else if let Some(shot) = &done.shot {
            debug!("probe request"; conn = conn, method = done.method, took_us = started.elapsed(),
                nodes = done.nodes, bytes = bytes.len(), path = Value::path(&shot.path),
                width = shot.width, height = shot.height);
        } else {
            debug!("probe request"; conn = conn, method = done.method, took_us = started.elapsed(),
                nodes = done.nodes, bytes = bytes.len());
        }
        Processed {
            line: bytes,
            quit: done.quit,
        }
    }

    fn respond(&self, host: &dyn Host, line: &[u8]) -> Done {
        let request = match parse_request(line) {
            Ok(request) => request,
            Err(response) => {
                return Done {
                    response,
                    method: "unknown",
                    nodes: None,
                    shot: None,
                    io: None,
                    quit: false,
                };
            }
        };
        let method = method_name(&request.method);
        let id = request.id;
        let mut done = Done {
            response: Response::err(id, ErrorKind::Quitting),
            method,
            nodes: None,
            shot: None,
            io: None,
            quit: false,
        };
        if self.quitting.load(Ordering::Acquire) {
            return done;
        }
        match self.run(host, request, &mut done) {
            Ok(response) => done.response = response,
            Err(failure) => {
                done.response = Response::err(id, failure.kind);
                done.io = failure.io;
            }
        }
        done
    }

    fn run(&self, host: &dyn Host, request: Request, done: &mut Done) -> Result<Response, Failure> {
        let id = request.id;
        let call = match done.method {
            "hello" => Call::Hello,
            "ui_tree" => Call::Tree {
                window: params::<TreeParams>(request.params)?.window,
            },
            "screenshot" => self.screenshot_call(request.params)?,
            "quit" => {
                self.quitting.store(true, Ordering::Release);
                done.quit = true;
                return Ok(Response::ok(id, json!({})));
            }
            _ => return Err(ErrorKind::UnknownMethod.into()),
        };
        let outcome = host.submit(call).ok_or(ErrorKind::Quitting)?;
        match outcome {
            Outcome::Error(kind) => Err(kind.into()),
            Outcome::Value(value) => Ok(Response::ok(id, value)),
            Outcome::Tree(tree) => {
                done.nodes = Some(tree.nodes.len());
                let value = serde_json::to_value(&tree).map_err(|_| ErrorKind::Io)?;
                Ok(Response::ok(id, value))
            }
            Outcome::Shot {
                capture,
                dir,
                name,
                annotate,
            } => {
                done.nodes = Some(capture.tree.nodes.len());
                let (value, shot) = finish_shot(capture, &dir, &name, annotate)?;
                done.shot = Some(shot);
                Ok(Response::ok(id, value))
            }
        }
    }

    /// Checks the parameters of `screenshot` before any frame is drawn.
    fn screenshot_call(&self, value: serde_json::Value) -> Result<Call, ErrorKind> {
        let p = params::<ScreenshotParams>(value)?;
        let dir = match p.dir {
            Some(dir) => check_dir(&dir).ok_or(ErrorKind::Io)?,
            None => self.dir.clone(),
        };
        let name = match p.name {
            Some(name) if check_name(&name) => name,
            Some(_) => return Err(ErrorKind::Io),
            None => format!("shot-{}", self.shots.fetch_add(1, Ordering::Relaxed) + 1),
        };
        Ok(Call::Screenshot {
            window: p.window,
            dir,
            name,
            annotate: p.annotate.unwrap_or(true),
        })
    }
}

/// Annotates, encodes and writes the screenshot; the response's result.
fn finish_shot(
    capture: Capture,
    dir: &Path,
    name: &str,
    annotate_it: bool,
) -> Result<(serde_json::Value, ShotDone), Failure> {
    let Capture { tree, image } = capture;
    let mut image = image.ok_or(ErrorKind::Render)?;
    if annotate_it {
        annotate(&mut image, &tree);
    }
    let path = dir.join(format!("{name}.png"));
    write_png(&image, &path).map_err(|e| Failure {
        kind: ErrorKind::Io,
        io: Some(e),
    })?;
    let (width, height) = (image.width(), image.height());
    let value = json!({
        "path": path.to_string_lossy(),
        "width": width,
        "height": height,
        "scale": tree.window.scale,
        "tree": tree,
    });
    Ok((
        value,
        ShotDone {
            path,
            width,
            height,
        },
    ))
}

/// Reads of one request line.
pub(crate) enum LineRead {
    Line,
    Eof,
    TooLong,
    Idle,
    Failed,
}

/// Appends to `buf` until a newline; the line (without it) is complete on
/// `LineRead::Line`. After `LineRead::Idle` (an interrupted read) the partial
/// line stays in `buf`, so the next call continues it. A line longer than
/// `MAX_LINE` is `TooLong`.
pub(crate) fn read_line(reader: &mut impl BufRead, buf: &mut Vec<u8>) -> LineRead {
    let room = (MAX_LINE + 1).saturating_sub(buf.len()) as u64;
    match (&mut *reader).take(room).read_until(b'\n', buf) {
        Ok(0) if buf.is_empty() => LineRead::Eof,
        Ok(_) if buf.last() == Some(&b'\n') => {
            buf.pop();
            LineRead::Line
        }
        Ok(_) if buf.len() > MAX_LINE => LineRead::TooLong,
        // The client closed the connection in the middle of a line.
        Ok(_) => LineRead::Line,
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
            ) =>
        {
            LineRead::Idle
        }
        Err(e) if e.kind() == io::ErrorKind::Interrupted => LineRead::Idle,
        Err(_) => LineRead::Failed,
    }
}

/// One client: lines in, lines out, until it disconnects, the line is too
/// long or the session stops (`close_connections` ends the stream).
pub(crate) fn serve_connection(stream: UnixStream, session: &Session, host: &dyn Host, conn: u64) {
    info!("probe client connected"; conn = conn);
    if let Ok(read_half) = stream.try_clone() {
        let mut reader = io::BufReader::new(read_half);
        let mut writer = stream;
        let mut buf = Vec::new();
        loop {
            match read_line(&mut reader, &mut buf) {
                LineRead::Line => {
                    let processed = session.process(host, conn, &buf);
                    buf.clear();
                    if writer.write_all(&processed.line).is_err() {
                        break;
                    }
                    if processed.quit {
                        host.dispatch_quit();
                    }
                }
                LineRead::Idle => {
                    if session.stop.load(Ordering::Acquire) {
                        break;
                    }
                }
                LineRead::Eof | LineRead::TooLong | LineRead::Failed => break,
            }
        }
    }
    session.unregister(conn);
    info!("probe client disconnected"; conn = conn);
}

enum Job {
    Request {
        call: Call,
        reply: mpsc::Sender<Outcome>,
    },
    Quit,
}

/// The connection threads' side of the queue to the main thread.
struct QueueHost {
    queue: UnboundedSender<Job>,
}

impl Host for QueueHost {
    fn submit(&self, call: Call) -> Option<Outcome> {
        let (reply, outcome) = mpsc::channel();
        self.queue
            .unbounded_send(Job::Request { call, reply })
            .ok()?;
        outcome.recv().ok()
    }

    fn dispatch_quit(&self) {
        // A closed queue means the application is already gone.
        let _ = self.queue.unbounded_send(Job::Quit);
    }
}

/// A probe session. `app` creates it after the startup barrier is released
/// and drops it on the way out.
pub struct Probe {
    session: Arc<Session>,
    socket: PathBuf,
    listener: Mutex<Option<JoinHandle<()>>>,
    /// Answers the requests on the main thread; dropped with the probe.
    _task: Task<()>,
}

/// Whether `dir` is an absolute path to a directory that only the current
/// user can use.
pub(crate) fn dir_is_private(dir: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(dir) else {
        return false;
    };
    dir.is_absolute()
        && meta.is_dir()
        && meta.uid() == rustix::process::geteuid().as_raw()
        && meta.mode() & 0o777 == 0o700
}

impl Probe {
    /// On the main thread, after the first frame; does not block. Checks the
    /// directory, binds `dir/probe.sock` (mode 0600), starts the listener
    /// thread `lectern-ui-probe` and a foreground task for the requests. On
    /// failure `app` logs a warning and carries on.
    pub fn start(cx: &mut App, config: Config) -> Result<Probe, StartError> {
        if !dir_is_private(&config.dir) {
            return Err(StartError::DirNotPrivate);
        }
        let socket = config.dir.join(SOCKET);
        match std::fs::remove_file(&socket) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(StartError::Bind(e)),
        }
        let listener = UnixListener::bind(&socket).map_err(StartError::Bind)?;
        if let Err(e) = std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)) {
            remove_socket(&socket);
            return Err(StartError::Bind(e));
        }

        let session = Arc::new(Session::new(config.dir.clone()));
        let (queue, requests) = unbounded();
        let host = Arc::new(QueueHost { queue });
        let spawned = std::thread::Builder::new()
            .name("lectern-ui-probe".into())
            .spawn({
                let (session, socket, host) = (session.clone(), socket.clone(), host);
                move || listen(listener, &session, &socket, host)
            });
        let listener = match spawned {
            Ok(handle) => handle,
            Err(e) => {
                remove_socket(&socket);
                return Err(StartError::Thread(e));
            }
        };

        let main = Rc::new(Main {
            dir: config.dir.clone(),
            frame_timeout: config.frame_timeout,
        });
        let task = cx.spawn(async move |cx| serve(cx, requests, main).await);
        info!("probe listening"; dir = Value::path(&config.dir));
        Ok(Probe {
            session,
            socket,
            listener: Mutex::new(Some(listener)),
            _task: task,
        })
    }

    /// Any thread, does not block: sets the stop flag, ends the open
    /// connections and wakes the listener thread, which removes `probe.sock`
    /// and ends. A request in flight is answered into a closed connection.
    pub fn shutdown(&self) {
        if self.session.stop.swap(true, Ordering::AcqRel) {
            return;
        }
        self.session.close_connections();
        // `accept` has no other way to be woken: connect to it.
        let _ = UnixStream::connect(&self.socket);
        remove_socket(&self.socket);
        // The thread ends by itself; it is not joined.
        drop(
            self.listener
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take(),
        );
    }
}

impl Drop for Probe {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Removes the socket file; a file that is already gone is the goal.
fn remove_socket(socket: &Path) {
    let _ = std::fs::remove_file(socket);
}

/// The listener thread: accepts connections until the stop flag is set.
fn listen(listener: UnixListener, session: &Arc<Session>, socket: &Path, host: Arc<QueueHost>) {
    for stream in listener.incoming() {
        if session.stop.load(Ordering::Acquire) {
            break;
        }
        let stream = match stream {
            Ok(stream) => stream,
            Err(_) => {
                // Out of file descriptors or similar: do not spin.
                std::thread::sleep(Duration::from_millis(50));
                continue;
            }
        };
        let conn = session.conns.fetch_add(1, Ordering::Relaxed) + 1;
        session.register(conn, &stream);
        let spawned = std::thread::Builder::new()
            .name("lectern-ui-probe-conn".into())
            .spawn({
                let (session, host) = (session.clone(), host.clone());
                move || serve_connection(stream, &session, host.as_ref(), conn)
            });
        if let Err(e) = spawned {
            session.unregister(conn);
            warn!("probe client rejected"; conn = conn, io = Value::io(&e));
        }
    }
    drop(listener);
    remove_socket(socket);
    info!("probe stopped"; conns = session.conns.load(Ordering::Relaxed),
        requests = session.requests.load(Ordering::Relaxed));
}

/// What the foreground task knows.
pub(crate) struct Main {
    pub(crate) dir: PathBuf,
    pub(crate) frame_timeout: Duration,
}

/// The foreground task: the requests one at a time, in the order they came.
async fn serve(cx: &mut AsyncApp, mut requests: UnboundedReceiver<Job>, main: Rc<Main>) {
    while let Some(job) = requests.next().await {
        match job {
            Job::Request { call, reply } => {
                let outcome = handle(cx, &main, call).await;
                // The connection may have gone; the result is dropped then.
                let _ = reply.send(outcome);
            }
            Job::Quit => cx.update(dispatch_quit),
        }
    }
}

/// Dispatches the `Quit` action by name, as the menu and ⌘Q do.
pub(crate) fn dispatch_quit(cx: &mut App) {
    match cx.build_action(QUIT_ACTION, None) {
        Ok(action) => cx.dispatch_action(&*action),
        Err(_) => warn!("probe quit failed"),
    }
}

/// Answers one call on the main thread.
pub(crate) async fn handle(cx: &mut AsyncApp, main: &Main, call: Call) -> Outcome {
    match call {
        Call::Hello => Outcome::Value(cx.update(|cx| hello(cx, &main.dir))),
        Call::Tree { window } => match frame(cx, main, window, false).await {
            Ok(capture) => Outcome::Tree(capture.tree),
            Err(kind) => Outcome::Error(kind),
        },
        Call::Screenshot {
            window,
            dir,
            name,
            annotate,
        } => match frame(cx, main, window, true).await {
            Ok(capture) => Outcome::Shot {
                capture,
                dir,
                name,
                annotate,
            },
            Err(kind) => Outcome::Error(kind),
        },
    }
}

/// The result of `hello`.
pub(crate) fn hello(cx: &mut App, dir: &Path) -> serde_json::Value {
    let windows: Vec<WindowInfo> = cx
        .windows()
        .into_iter()
        .filter_map(|handle| {
            handle
                .update(cx, |_, window, _| WindowInfo::of(window))
                .ok()
        })
        .collect();
    json!({
        "protocol": PROTOCOL,
        "pid": std::process::id(),
        "schemas": [TREE_SCHEMA],
        "dir": dir.to_string_lossy(),
        "windows": windows,
    })
}

/// The window a request names: the one with that ID; without one, the
/// active window, else the first.
fn pick_window(cx: &App, id: Option<u64>) -> Option<AnyWindowHandle> {
    let windows = cx.windows();
    match id {
        Some(id) => windows
            .into_iter()
            .find(|handle| handle.window_id().as_u64() == id),
        None => cx
            .active_window()
            .filter(|active| windows.contains(active))
            .or_else(|| windows.first().copied()),
    }
}

/// One captured frame of the window, or why there is none. After
/// `frame_timeout` the capture is cancelled, so a frame that comes late
/// finds the recorder disarmed.
async fn frame(
    cx: &mut AsyncApp,
    main: &Main,
    window: Option<u64>,
    with_image: bool,
) -> Result<Capture, ErrorKind> {
    let (done, frame) = oneshot::channel();
    let cancel = cx
        .update(|cx| {
            let handle = pick_window(cx, window)?;
            handle
                .update(cx, |_, window, _| {
                    capture(window, with_image, move |result| {
                        // The wait may have timed out already.
                        let _ = done.send(result);
                    })
                })
                .ok()
        })
        .ok_or(ErrorKind::NoWindow)?;
    match frame
        .with_timeout(main.frame_timeout, cx.background_executor())
        .await
    {
        Ok(Ok(Ok(capture))) => Ok(capture),
        Ok(Ok(Err(CaptureError::Render))) => Err(ErrorKind::Render),
        // The capture was dropped without finishing: the window closed.
        Ok(Err(_)) => Err(ErrorKind::NoWindow),
        Err(_) => {
            cancel.cancel();
            Err(ErrorKind::Timeout)
        }
    }
}
