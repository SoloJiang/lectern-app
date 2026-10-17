//! Dedicated tasks: one thread per long-blocking closure.

use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::{Arc, Mutex, Weak};
use std::task::{Context, Wake, Waker};
use std::thread::{self, Thread};

use lectern_kernel::{GatePoll, GateSlot, catch, contain};

use crate::output::Output;
use crate::sched::{Inner, Job, TaskId, run_hook};
use crate::task::Handle;
use crate::util::{lock, log};
use crate::{TaskError, ThreadKind};

type BoxFn<T> = Box<dyn FnOnce() -> T + Send>;

/// A finished task's result and the message of a panic still to report.
type Finished<T> = (Result<T, TaskError>, Option<String>);

pub(crate) struct DedicatedCell<T> {
    id: TaskId,
    cancelled: AtomicBool,
    work: Mutex<Option<BoxFn<T>>>,
    /// Only for `run_dedicated_gated`. Dropped after the task is done.
    slot: Mutex<Option<GateSlot>>,
    /// The thread running this task, once it has started; `cancel` unparks it.
    thread: Mutex<Option<Thread>>,
    output: Output<T>,
    exec: Weak<Inner>,
}

impl<T: Send + 'static> DedicatedCell<T> {
    pub(crate) fn new(exec: &Arc<Inner>, work: BoxFn<T>, slot: Option<GateSlot>) -> Arc<Self> {
        Arc::new(DedicatedCell {
            id: exec.next_id(),
            cancelled: AtomicBool::new(false),
            work: Mutex::new(Some(work)),
            slot: Mutex::new(slot),
            thread: Mutex::new(None),
            output: Output::new(),
            exec: Arc::downgrade(exec),
        })
    }

    pub(crate) fn id(&self) -> TaskId {
        self.id
    }

    /// The thread body: wait for the gate, then run the closure once.
    fn body(self: &Arc<Self>) -> Finished<T> {
        // Published under a lock before `cancelled` is read, so a concurrent
        // `cancel` either sees the thread and unparks it or is seen here.
        *lock(&self.thread) = Some(thread::current());
        let waker = Waker::from(Arc::new(Unpark(thread::current())));
        let mut cx = Context::from_waker(&waker);
        loop {
            let decision = match &*lock(&self.slot) {
                Some(slot) => slot.poll(&mut cx),
                None => GatePoll::Run,
            };
            if decision == GatePoll::Drop || self.cancelled.load(SeqCst) {
                return self.drop_unrun();
            }
            if decision == GatePoll::Run {
                break;
            }
            // Woken by the gate or by `cancel`; a spurious wakeup only loops
            // once more.
            thread::park();
        }
        let Some(work) = lock(&self.work).take() else {
            return (Err(TaskError::Cancelled), None);
        };
        match catch(work) {
            Ok(value) => (Ok(value), None),
            Err(payload) => {
                // The closure's captures were dropped while unwinding.
                let message = payload.dispose();
                (Err(TaskError::Panicked(message.clone())), Some(message))
            }
        }
    }

    /// Drops the closure without running it, under `contain`.
    fn drop_unrun(&self) -> Finished<T> {
        let work = lock(&self.work).take();
        let panic = contain(move || drop(work)).err();
        (Err(TaskError::Cancelled), panic)
    }

    /// Marks the task done (its result becomes visible), reports a panic to
    /// the gate, then drops the gate slot and leaves the registry.
    fn finish(&self, (result, panic): Finished<T>) {
        self.output.complete(result);
        if let Some(message) = panic {
            match &*lock(&self.slot) {
                Some(slot) => slot.panicked(message),
                None => log(format_args!("dedicated task panicked: {message}")),
            }
        }
        let slot = lock(&self.slot).take();
        drop(slot);
        if let Some(exec) = self.exec.upgrade() {
            exec.finished_dedicated(self.id);
        }
    }
}

impl<T: Send + 'static> Job for DedicatedCell<T> {
    fn run(self: Arc<Self>) {
        let hook = self.exec.upgrade().and_then(|exec| exec.hook.clone());
        run_hook(hook.as_ref(), ThreadKind::Dedicated);
        drop(hook);
        let finished = self.body();
        self.finish(finished);
    }

    fn cancel(self: Arc<Self>) {
        self.cancelled.store(true, SeqCst);
        if let Some(thread) = &*lock(&self.thread) {
            thread.unpark();
        }
    }

    fn discard(self: Arc<Self>) {
        let finished = self.drop_unrun();
        self.finish(finished);
    }
}

impl<T: Send + 'static> Handle<T> for DedicatedCell<T> {
    fn output(&self) -> &Output<T> {
        &self.output
    }

    fn cancel(self: Arc<Self>) {
        Job::cancel(self);
    }
}

/// The waker of a dedicated thread waiting for its gate.
struct Unpark(Thread);

impl Wake for Unpark {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.unpark();
    }
}
