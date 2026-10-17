//! `executor/*` (docs/design/executor.md「性能」): record only.

use std::future::Future;
use std::hint::black_box;
use std::pin::{Pin, pin};
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};
use std::thread::{self, Thread};
use std::time::Duration;

use lectern_bench::{Bench, BenchError, Ctx, Measured, Value};
use lectern_executor::{Config, Executor, Priority, Task};

struct Unpark(Thread);

impl Wake for Unpark {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.unpark();
    }
}

/// Waits for `future` on this thread.
fn block_on<F: Future>(future: F) -> F::Output {
    let waker = Waker::from(Arc::new(Unpark(thread::current())));
    let mut cx = Context::from_waker(&waker);
    let mut future = pin!(future);
    loop {
        if let Poll::Ready(output) = future.as_mut().poll(&mut cx) {
            return output;
        }
        thread::park();
    }
}

/// Waits for every task; returns how many finished with a value.
fn join<T>(tasks: Vec<Task<T>>) -> usize {
    tasks
        .into_iter()
        .map(block_on)
        .filter(Result::is_ok)
        .count()
}

fn shutdown(exec: &Executor) -> Result<(), BenchError> {
    match exec.shutdown(Duration::from_secs(10)) {
        lectern_executor::Shutdown::Clean => Ok(()),
        other => Err(BenchError::debug("shutdown", other)),
    }
}

/// Submit 10 000 empty tasks and wait until all of them have finished.
pub const SPAWN_10K: Bench = Bench {
    id: "executor/spawn_10k",
    run: spawn_10k,
};

fn spawn_10k(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let n = ctx.scale(10_000, 100);
    let exec = Executor::new(Config::default());
    let spawn = |exec: &Executor| -> usize {
        let tasks: Vec<Task<()>> = (0..black_box(n))
            .map(|_| exec.run(Priority::Background, || ()))
            .collect();
        join(tasks)
    };
    if spawn(&exec) != n {
        return Err(BenchError::new("not every task finished"));
    }
    let time = ctx.time(|| spawn(&exec));
    shutdown(&exec)?;
    Ok(vec![Measured::new("executor/spawn_10k", Value::Time(time))])
}

/// Wakes itself this many more times before it finishes.
struct Yield(u32);

impl Future for Yield {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.0 == 0 {
            return Poll::Ready(());
        }
        self.0 -= 1;
        cx.waker().wake_by_ref();
        Poll::Pending
    }
}

/// 1 000 tasks, each woken 100 times before it finishes.
pub const WAKE_1000X100: Bench = Bench {
    id: "executor/wake_1000x100",
    run: wake_1000x100,
};

fn wake_1000x100(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let (tasks, wakes) = ctx.scale((1_000, 100), (10, 10));
    let exec = Executor::new(Config::default());
    let wake = |exec: &Executor| -> usize {
        let handles: Vec<Task<()>> = (0..black_box(tasks))
            .map(|_| exec.spawn(Priority::Background, Yield(wakes)))
            .collect();
        join(handles)
    };
    if wake(&exec) != tasks {
        return Err(BenchError::new("not every task finished"));
    }
    let time = ctx.time(|| wake(&exec));
    shutdown(&exec)?;
    Ok(vec![Measured::new(
        "executor/wake_1000x100",
        Value::Time(time),
    )])
}
