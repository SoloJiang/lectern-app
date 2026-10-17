//! Shared helpers: a kernel with one plugin, `block_on`, drop probes.

use std::future::Future;
use std::pin::pin;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Mutex, mpsc};
use std::task::{Context, Poll, Wake, Waker};
use std::thread::{self, Thread};
use std::time::{Duration, Instant};

use lectern_kernel::{
    Activation, Active, Context as PluginContext, Kernel, Manifest, Plugin, PluginError,
    PluginHandle, PluginSet, PluginState,
};

use crate::{Config, Executor};

/// Upper bound for every wait; only reached when a test fails.
pub(crate) const LIMIT: Duration = Duration::from_secs(10);

pub(crate) fn executor(workers: usize) -> Executor {
    pools(workers, 1)
}

/// An executor with `workers` worker threads and `batch_workers` batch
/// threads.
pub(crate) fn pools(workers: usize, batch_workers: usize) -> Executor {
    Executor::new(Config {
        workers,
        batch_workers,
        on_thread_start: None,
    })
}

/// Polls `future` on this thread until it is ready, panicking after `LIMIT`.
pub(crate) fn block_on<F: Future>(future: F) -> F::Output {
    struct Unpark(Thread);
    impl Wake for Unpark {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = Waker::from(Arc::new(Unpark(thread::current())));
    let mut cx = Context::from_waker(&waker);
    let mut future = pin!(future);
    let deadline = Instant::now() + LIMIT;
    loop {
        if let Poll::Ready(value) = future.as_mut().poll(&mut cx) {
            return value;
        }
        let now = Instant::now();
        assert!(now < deadline, "block_on timed out");
        thread::park_timeout(deadline - now);
    }
}

/// Polls `condition` until it holds, panicking after `LIMIT`. Only for state
/// another thread reaches with no event to wait on (a plugin's status).
pub(crate) fn wait_until(what: &str, condition: impl Fn() -> bool) {
    let deadline = Instant::now() + LIMIT;
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(1));
    }
}

pub(crate) fn recv<T>(receiver: &mpsc::Receiver<T>, what: &str) -> T {
    receiver
        .recv_timeout(LIMIT)
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
}

/// An ordered log shared between a test and its tasks.
#[derive(Clone, Default)]
pub(crate) struct Log(Arc<Mutex<Vec<String>>>);

impl Log {
    pub(crate) fn push(&self, entry: impl Into<String>) {
        self.0.lock().unwrap().push(entry.into());
    }

    pub(crate) fn snapshot(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

/// Logs `dropped <name> on <thread>` when dropped.
pub(crate) struct DropProbe {
    pub(crate) name: &'static str,
    pub(crate) log: Log,
}

impl DropProbe {
    pub(crate) fn new(name: &'static str, log: &Log) -> Self {
        DropProbe {
            name,
            log: log.clone(),
        }
    }
}

impl Drop for DropProbe {
    fn drop(&mut self) {
        let thread = thread::current();
        let thread = thread.name().unwrap_or("unnamed");
        self.log.push(format!("dropped {} on {thread}", self.name));
    }
}

/// Panics when dropped.
pub(crate) struct PanicOnDrop;

impl Drop for PanicOnDrop {
    fn drop(&mut self) {
        panic!("drop panicked");
    }
}

/// A counter shared with a task.
#[derive(Clone, Default)]
pub(crate) struct Counter(Arc<AtomicUsize>);

impl Counter {
    pub(crate) fn bump(&self) -> usize {
        self.0.fetch_add(1, SeqCst) + 1
    }

    pub(crate) fn drop_one(&self) {
        self.0.fetch_sub(1, SeqCst);
    }

    pub(crate) fn get(&self) -> usize {
        self.0.load(SeqCst)
    }
}

/// A future that wakes itself and returns `Pending` `times` times, then is
/// ready.
pub(crate) struct Yield(pub(crate) usize);

impl Future for Yield {
    type Output = ();

    fn poll(mut self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.0 == 0 {
            return Poll::Ready(());
        }
        self.0 -= 1;
        cx.waker().wake_by_ref();
        Poll::Pending
    }
}

/// A started kernel with one plugin `p` whose `PluginHandle` is captured.
/// Shuts the kernel down when dropped.
pub(crate) struct TestPlugin {
    pub(crate) kernel: Arc<Kernel>,
    pub(crate) handle: PluginHandle,
}

impl TestPlugin {
    pub(crate) fn start() -> Self {
        let slot = Arc::new(Mutex::new(None));
        let kernel = Kernel::start(PluginSet::new().with("p", Capture(slot.clone())));
        kernel.barrier();
        let handle = slot.lock().unwrap().clone().expect("plugin started");
        TestPlugin { kernel, handle }
    }

    pub(crate) fn state(&self) -> PluginState {
        self.kernel
            .status()
            .into_iter()
            .find(|status| status.id == "p")
            .map(|status| status.state)
            .expect("plugin p")
    }

    /// Waits until the plugin has failed with a panic whose message contains
    /// `text`.
    pub(crate) fn wait_failed(&self, text: &str) {
        wait_until(
            "plugin failed",
            || matches!(self.state(), PluginState::Failed(PluginError::Panicked(message)) if message.contains(text)),
        );
    }
}

impl Drop for TestPlugin {
    fn drop(&mut self) {
        let (done, wait) = mpsc::channel();
        self.kernel.shutdown(move || {
            let _ = done.send(());
        });
        let _ = wait.recv_timeout(LIMIT);
    }
}

type HandleSlot = Arc<Mutex<Option<PluginHandle>>>;

struct Capture(HandleSlot);

impl Plugin for Capture {
    fn manifest(&self) -> Manifest {
        Manifest {
            provides: Vec::new(),
            requires: Vec::new(),
            activation: vec![Activation::AfterFirstFrame],
        }
    }

    fn activate(&self, _: &mut PluginContext<'_>) -> Result<Box<dyn Active>, PluginError> {
        Ok(Box::new(Started(self.0.clone())))
    }
}

struct Started(HandleSlot);

impl Active for Started {
    fn started(&mut self, handle: PluginHandle) {
        *self.0.lock().unwrap() = Some(handle);
    }
}
