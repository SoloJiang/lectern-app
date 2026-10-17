//! A minimal executor that follows the executor's contract with the kernel:
//! `wake` only enqueues, a running task is never re-entered, a finished task
//! is never polled again, and a panicking task is finished, its future dropped
//! and its payload released before `GateSlot::panicked` is called.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Mutex, mpsc};
use std::task::{Context, Poll, Wake, Waker};
use std::thread;

use crate::{GatePoll, GateSlot, SubmitGate, catch, contain};

const IDLE: u8 = 0;
const QUEUED: u8 = 1;
const RUNNING: u8 = 2;
const REPOLL: u8 = 3;
const DONE: u8 = 4;

type BoxFuture = Pin<Box<dyn Future<Output = ()> + Send>>;

pub(crate) struct Task {
    future: Mutex<Option<BoxFuture>>,
    slot: Mutex<Option<GateSlot>>,
    state: AtomicU8,
    cancelled: AtomicBool,
    queue: mpsc::Sender<Arc<Task>>,
    pub(crate) polls: AtomicUsize,
    pub(crate) entered: AtomicUsize,
}

impl Wake for Task {
    fn wake(self: Arc<Self>) {
        schedule(self);
    }
}

fn schedule(task: Arc<Task>) {
    loop {
        let state = task.state.load(SeqCst);
        let next = match state {
            IDLE => QUEUED,
            RUNNING => REPOLL,
            _ => return,
        };
        if task
            .state
            .compare_exchange(state, next, SeqCst, SeqCst)
            .is_ok()
        {
            if next == QUEUED {
                let queue = task.queue.clone();
                let _ = queue.send(task);
            }
            return;
        }
    }
}

fn run(task: Arc<Task>) {
    if task
        .state
        .compare_exchange(QUEUED, RUNNING, SeqCst, SeqCst)
        .is_err()
    {
        return;
    }
    let entered = task.entered.fetch_add(1, SeqCst);
    assert_eq!(entered, 0, "task re-entered");
    let finished = poll_once(&task);
    task.entered.fetch_sub(1, SeqCst);
    if finished {
        task.state.store(DONE, SeqCst);
        let slot = task.slot.lock().unwrap().take();
        drop(slot);
        return;
    }
    if task
        .state
        .compare_exchange(RUNNING, IDLE, SeqCst, SeqCst)
        .is_err()
    {
        // Woken while running: poll again.
        task.state.store(QUEUED, SeqCst);
        let queue = task.queue.clone();
        let _ = queue.send(task);
    }
}

/// Returns true when the task is finished.
fn poll_once(task: &Arc<Task>) -> bool {
    let waker = Waker::from(task.clone());
    let mut cx = Context::from_waker(&waker);
    let decision = {
        let slot = task.slot.lock().unwrap();
        match slot.as_ref() {
            Some(slot) => slot.poll(&mut cx),
            None => GatePoll::Drop,
        }
    };
    let decision = if task.cancelled.load(SeqCst) {
        GatePoll::Drop
    } else {
        decision
    };
    match decision {
        GatePoll::Wait => false,
        GatePoll::Drop => {
            drop_future(task);
            true
        }
        GatePoll::Run => {
            let Some(mut future) = task.future.lock().unwrap().take() else {
                return true;
            };
            task.polls.fetch_add(1, SeqCst);
            match catch(|| future.as_mut().poll(&mut cx)) {
                Ok(Poll::Pending) => {
                    *task.future.lock().unwrap() = Some(future);
                    false
                }
                Ok(Poll::Ready(())) => {
                    if let Err(message) = contain(move || drop(future)) {
                        report(task, message);
                    }
                    true
                }
                Err(payload) => {
                    task.state.store(DONE, SeqCst);
                    if let Err(message) = contain(move || drop(future)) {
                        report(task, message);
                    }
                    let message = payload.dispose();
                    report(task, message);
                    true
                }
            }
        }
    }
}

fn drop_future(task: &Arc<Task>) {
    let future = task.future.lock().unwrap().take();
    if let Err(message) = contain(move || drop(future)) {
        report(task, message);
    }
}

fn report(task: &Arc<Task>, message: String) {
    if let Some(slot) = task.slot.lock().unwrap().as_ref() {
        slot.panicked(message);
    }
}

/// Cancels the task when dropped (the handle kept by the kernel).
pub(crate) struct TaskHandle {
    task: Option<Arc<Task>>,
}

impl TaskHandle {
    pub(crate) fn task(&self) -> Option<Arc<Task>> {
        self.task.clone()
    }
}

impl Drop for TaskHandle {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.cancelled.store(true, SeqCst);
            schedule(task);
        }
    }
}

#[derive(Clone)]
pub(crate) struct Executor {
    queue: mpsc::Sender<Arc<Task>>,
}

impl Executor {
    pub(crate) fn new() -> Self {
        let (queue, receiver) = mpsc::channel::<Arc<Task>>();
        thread::Builder::new()
            .name("test-executor".to_owned())
            .spawn(move || {
                while let Ok(task) = receiver.recv() {
                    run(task);
                }
            })
            .unwrap();
        Executor { queue }
    }

    pub(crate) fn spawn(
        &self,
        gate: &SubmitGate,
        future: impl Future<Output = ()> + Send + 'static,
    ) -> TaskHandle {
        let Some(slot) = gate.slot() else {
            if let Err(message) = contain(move || drop(future)) {
                gate.panicked(message);
            }
            return TaskHandle { task: None };
        };
        let task = Arc::new(Task {
            future: Mutex::new(Some(Box::pin(future))),
            slot: Mutex::new(Some(slot)),
            state: AtomicU8::new(QUEUED),
            cancelled: AtomicBool::new(false),
            queue: self.queue.clone(),
            polls: AtomicUsize::new(0),
            entered: AtomicUsize::new(0),
        });
        let _ = self.queue.send(task.clone());
        TaskHandle { task: Some(task) }
    }
}

/// A future that is pending until `release` is called.
#[derive(Clone, Default)]
pub(crate) struct Latch {
    inner: Arc<Mutex<(bool, Option<Waker>)>>,
}

impl Latch {
    pub(crate) fn wait(&self) -> LatchWait {
        LatchWait(self.inner.clone())
    }

    pub(crate) fn release(&self) {
        let waker = {
            let mut inner = self.inner.lock().unwrap();
            inner.0 = true;
            inner.1.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

pub(crate) struct LatchWait(Arc<Mutex<(bool, Option<Waker>)>>);

impl Future for LatchWait {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let mut inner = self.0.lock().unwrap();
        if inner.0 {
            Poll::Ready(())
        } else {
            inner.1 = Some(cx.waker().clone());
            Poll::Pending
        }
    }
}
