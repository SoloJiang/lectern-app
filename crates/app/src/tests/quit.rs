//! The quit order with fake steps. The checks are on the order of events
//! and on the timeouts passed, never on how long a step took.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use lectern_executor::Shutdown;
use lectern_logging::Flushed;

use crate::quit::{QuitSteps, Timeouts, run};

#[derive(Debug, PartialEq, Eq)]
enum Event {
    Kernel,
    KernelDone,
    Executor(Duration),
    Logging(Duration),
}

#[derive(Clone, Default)]
struct Fake {
    events: Arc<Mutex<Vec<Event>>>,
    /// The kernel never calls `done`.
    hang_kernel: bool,
    /// The kernel drops `done` without calling it.
    drop_done: bool,
    busy_workers: Option<usize>,
}

impl Fake {
    fn events(&self) -> Vec<Event> {
        std::mem::take(&mut *self.events.lock().unwrap())
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

    fn executor(&self, timeout: Duration) -> Shutdown {
        self.events.lock().unwrap().push(Event::Executor(timeout));
        match self.busy_workers {
            Some(busy_workers) => Shutdown::TimedOut { busy_workers },
            None => Shutdown::Clean,
        }
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
    let outcome = run(&fake, &LONG);
    assert!(!outcome.kernel_timed_out);
    assert!(matches!(outcome.executor, Shutdown::Clean));
    assert!(matches!(outcome.logging, Flushed::Clean));
    let events = fake.events();
    assert_eq!(events.len(), 4, "{events:?}");
    assert_eq!(events[0], Event::Kernel);
    assert_eq!(events[1], Event::KernelDone);
    assert!(matches!(events[2], Event::Executor(_)), "{events:?}");
    assert_eq!(events[3], Event::Logging(Duration::from_millis(500)));
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
    let outcome = run(&fake, &timeouts);
    assert!(outcome.kernel_timed_out);
    // The whole budget went to the kernel: the executor only cancels.
    assert_eq!(
        fake.events(),
        [
            Event::Kernel,
            Event::Executor(Duration::ZERO),
            Event::Logging(Duration::from_millis(500)),
        ]
    );
}

#[test]
fn a_dropped_done_is_not_a_timeout() {
    let fake = Fake {
        drop_done: true,
        ..Fake::default()
    };
    let outcome = run(&fake, &LONG);
    assert!(!outcome.kernel_timed_out);
}

#[test]
fn executor_gets_the_remaining_time() {
    let fake = Fake::default();
    run(&fake, &LONG);
    let executor = fake
        .events()
        .into_iter()
        .find_map(|e| match e {
            Event::Executor(timeout) => Some(timeout),
            _ => None,
        })
        .unwrap();
    assert!(executor > Duration::ZERO && executor <= LONG.services, "{executor:?}");
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
    let outcome = run(&fake, &timeouts);
    assert!(matches!(outcome.executor, Shutdown::TimedOut { busy_workers: 2 }));
    let lines = capture.lines();
    let kernel = lines
        .iter()
        .find(|l| l.contains("\"msg\":\"kernel shutdown timed out\""))
        .unwrap_or_else(|| panic!("{lines:#?}"));
    assert!(kernel.contains("\"waited_us\":"), "{kernel}");
    assert!(
        lines.iter().any(|l| l.contains("\"msg\":\"executor shutdown timed out\"")
            && l.contains("\"busy_workers\":2")),
        "{lines:#?}"
    );
    // Written before logging shuts down.
    assert!(
        lines.iter().any(|l| l.contains("\"msg\":\"quit finished\"")),
        "{lines:#?}"
    );
}
