//! The quit order with fake steps. The checks are on the order of events
//! and on the timeouts passed, never on how long a step took.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lectern_executor::Shutdown;
use lectern_logging::Flushed;

use crate::quit::{QuitSteps, Timeouts, run};

#[derive(Debug, PartialEq, Eq)]
enum Event {
    Kernel,
    KernelDone,
    Watcher,
    Executor(Duration),
    Cache,
    Logging(Duration),
}

#[derive(Clone, Default)]
struct Fake {
    events: Arc<Mutex<Vec<Event>>>,
    /// The kernel never calls `done`.
    hang_kernel: bool,
    /// The kernel drops `done` without calling it.
    drop_done: bool,
    /// `watcher` never returns.
    hang_watcher: bool,
    busy_workers: Option<usize>,
}

impl Fake {
    fn events(&self) -> Vec<Event> {
        std::mem::take(&mut *self.events.lock().unwrap())
    }

    /// The events, once `event` has arrived. It is recorded on a step
    /// thread, so it may come after `run` returns when `run` stopped
    /// waiting for that step. The timeout only ends a hang.
    fn events_with(&self, event: Event) -> Vec<Event> {
        let start = Instant::now();
        while !self.events.lock().unwrap().contains(&event) {
            assert!(start.elapsed() < Duration::from_secs(10), "no {event:?}");
            std::thread::yield_now();
        }
        self.events()
    }

    fn run(&self, timeouts: &Timeouts) -> crate::quit::Outcome {
        run(Arc::new(self.clone()), timeouts)
    }
}

impl QuitSteps for Fake {
    fn kernel(&self, done: Box<dyn FnOnce() + Send>) {
        self.events.lock().unwrap().push(Event::Kernel);
        if self.hang_kernel {
            // Keep `done` alive so the channel stays open: a hung kernel.
            std::mem::forget(done);
        } else if self.drop_done {
            drop(done);
        } else {
            let events = self.events.clone();
            std::thread::spawn(move || {
                events.lock().unwrap().push(Event::KernelDone);
                done();
            });
        }
    }

    fn watcher(&self) {
        self.events.lock().unwrap().push(Event::Watcher);
        if self.hang_watcher {
            loop {
                std::thread::park();
            }
        }
    }

    fn executor(&self, timeout: Duration) -> Shutdown {
        self.events.lock().unwrap().push(Event::Executor(timeout));
        match self.busy_workers {
            Some(busy_workers) => Shutdown::TimedOut { busy_workers },
            None => Shutdown::Clean,
        }
    }

    fn cache(&self) {
        self.events.lock().unwrap().push(Event::Cache);
    }

    fn logging(&self, timeout: Duration) -> Flushed {
        self.events.lock().unwrap().push(Event::Logging(timeout));
        Flushed::Clean
    }
}

const LONG: Timeouts = Timeouts {
    services: Duration::from_secs(60),
    logging: Duration::from_millis(500),
};

#[test]
fn steps_run_in_order() {
    let fake = Fake::default();
    let outcome = fake.run(&LONG);
    assert!(!outcome.kernel_timed_out);
    assert!(!outcome.watcher_timed_out);
    assert!(matches!(outcome.executor, Shutdown::Clean));
    assert!(!outcome.cache_timed_out);
    assert!(matches!(outcome.logging, Flushed::Clean));
    let events = fake.events();
    assert_eq!(events.len(), 6, "{events:?}");
    assert_eq!(events[0], Event::Kernel);
    assert_eq!(events[1], Event::KernelDone);
    assert_eq!(events[2], Event::Watcher);
    assert!(matches!(events[3], Event::Executor(_)), "{events:?}");
    assert_eq!(events[4], Event::Cache);
    assert_eq!(events[5], Event::Logging(Duration::from_millis(500)));
}

#[test]
fn a_hung_kernel_does_not_block_quit() {
    let fake = Fake {
        hang_kernel: true,
        ..Fake::default()
    };
    let timeouts = Timeouts {
        services: Duration::from_millis(20),
        logging: Duration::from_millis(500),
    };
    let outcome = fake.run(&timeouts);
    assert!(outcome.kernel_timed_out);
    assert!(!outcome.cache_timed_out);
    // The whole budget went to the kernel: the watcher is still asked to
    // stop (without a wait), the executor only cancels, the cache is not
    // closed.
    let mut events = fake.events_with(Event::Watcher);
    events.retain(|e| *e != Event::Watcher);
    assert_eq!(
        events,
        [
            Event::Kernel,
            Event::Executor(Duration::ZERO),
            Event::Logging(Duration::from_millis(500)),
        ]
    );
}

#[test]
fn a_hung_watcher_does_not_block_quit() {
    let capture = lectern_logging::testing::capture();
    capture.clear();
    let fake = Fake {
        hang_watcher: true,
        ..Fake::default()
    };
    let timeouts = Timeouts {
        services: Duration::from_millis(20),
        logging: Duration::from_millis(500),
    };
    let outcome = fake.run(&timeouts);
    assert!(!outcome.kernel_timed_out);
    assert!(outcome.watcher_timed_out);
    assert!(!outcome.cache_timed_out);
    // The watcher had the rest of the budget: the executor only cancels and
    // the cache is not closed.
    let mut events = fake.events_with(Event::Watcher);
    events.retain(|e| *e != Event::Watcher);
    assert_eq!(
        events,
        [
            Event::Kernel,
            Event::KernelDone,
            Event::Executor(Duration::ZERO),
            Event::Logging(Duration::from_millis(500)),
        ]
    );
    let lines = capture.lines();
    let watcher = lines
        .iter()
        .find(|l| l.contains("\"msg\":\"watcher shutdown timed out\""))
        .unwrap_or_else(|| panic!("{lines:#?}"));
    assert!(watcher.contains("\"timeout_us\":"), "{watcher}");
}

#[test]
fn a_dropped_done_is_not_a_timeout() {
    let fake = Fake {
        drop_done: true,
        ..Fake::default()
    };
    let outcome = fake.run(&LONG);
    assert!(!outcome.kernel_timed_out);
}

#[test]
fn executor_gets_the_remaining_time() {
    let fake = Fake::default();
    fake.run(&LONG);
    let executor = fake
        .events()
        .into_iter()
        .find_map(|e| match e {
            Event::Executor(timeout) => Some(timeout),
            _ => None,
        })
        .unwrap();
    assert!(
        executor > Duration::ZERO && executor <= LONG.services,
        "{executor:?}"
    );
}

#[test]
fn timeouts_are_logged() {
    let capture = lectern_logging::testing::capture();
    capture.clear();
    let fake = Fake {
        hang_kernel: true,
        busy_workers: Some(2),
        ..Fake::default()
    };
    let timeouts = Timeouts {
        services: Duration::from_millis(20),
        logging: Duration::from_millis(500),
    };
    let outcome = fake.run(&timeouts);
    assert!(matches!(
        outcome.executor,
        Shutdown::TimedOut { busy_workers: 2 }
    ));
    let lines = capture.lines();
    let kernel = lines
        .iter()
        .find(|l| l.contains("\"msg\":\"kernel shutdown timed out\""))
        .unwrap_or_else(|| panic!("{lines:#?}"));
    assert!(kernel.contains("\"waited_us\":"), "{kernel}");
    assert!(
        lines
            .iter()
            .any(|l| l.contains("\"msg\":\"executor shutdown timed out\"")
                && l.contains("\"busy_workers\":2")),
        "{lines:#?}"
    );
    // Written before logging shuts down.
    assert!(
        lines
            .iter()
            .any(|l| l.contains("\"msg\":\"quit finished\"")),
        "{lines:#?}"
    );
}
