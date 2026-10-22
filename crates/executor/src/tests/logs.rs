//! Executor logging (`docs/design/executor.md`, 日志与诊断): the canary test,
//! the scope of panic records and the inherited task scope.

use std::sync::{Arc, mpsc};

use lectern_logging::testing::{self, Capture};
use lectern_logging::{Ctx, PluginTag};

use super::gates::sealed_gate;
use super::support::*;
use crate::{Config, Executor, Priority, Shutdown, TaskError};

const CANARY: &str = "executor-canary-3f6a91d2";

/// The trace-level capture, with the per-call-site limit out of the way: all
/// tests share one process under `cargo test`.
fn capture() -> &'static Capture {
    let capture = testing::capture();
    testing::set_callsite_limit(1_000_000);
    capture
}

/// The executor's records with message `msg`.
fn records(capture: &Capture, msg: &str) -> Vec<String> {
    let msg = format!("\"msg\":\"{msg}\"");
    capture
        .lines()
        .into_iter()
        .filter(|line| line.contains("\"tg\":\"executor\"") && line.contains(&msg))
        .collect()
}

/// Whether one of `lines` has every one of `parts`.
fn any_with(lines: &[String], parts: &[&str]) -> bool {
    lines
        .iter()
        .any(|line| parts.iter().all(|part| line.contains(part)))
}

/// A runtime string, so `panic!` formats it into a `String` payload.
fn canary(context: &str) -> String {
    format!("{context} {CANARY}")
}

/// Panics with its text when dropped.
struct PanicOnDropWith(String);

impl Drop for PanicOnDropWith {
    fn drop(&mut self) {
        panic!("{}", self.0);
    }
}

#[test]
fn canary_stays_out_of_executor_logs() {
    let capture = capture();
    let exec = Executor::manual();

    // A poll panics.
    let polled = exec.spawn(Priority::Background, async {
        panic!("{}", canary("poll"));
    });
    exec.run_until_idle();
    assert!(matches!(block_on(polled), Err(TaskError::Panicked(m)) if m.contains(CANARY)));

    // An unread result's drop panics.
    let result = exec.run(Priority::Background, || PanicOnDropWith(canary("result")));
    exec.run_until_idle();
    result.detach();
    exec.run_until_idle();

    // A cancelled future's drop panics.
    let probe = PanicOnDropWith(canary("future"));
    let pending = exec.spawn(Priority::Interactive, async move {
        let _probe = probe;
        std::future::pending::<()>().await;
    });
    exec.run_until_idle();
    drop(pending);
    exec.run_until_idle();

    // A dedicated closure panics.
    let dedicated = exec.run_dedicated("canary-dedicated", || {
        panic!("{}", canary("dedicated"));
    });
    assert!(block_on(dedicated).is_err());

    // A gated task panics; work refused by a sealed gate panics in its drop.
    let gated = TestPlugin::start();
    let e = exec.clone();
    assert!(gated.handle.own_with(move |gate| {
        e.spawn_gated(&gate, Priority::Interactive, async {
            panic!("{}", canary("gated"));
        })
    }));
    exec.run_until_idle();
    gated.wait_failed(CANARY);
    let sealed = TestPlugin::start();
    let gate = sealed_gate(&sealed);
    let probe = PanicOnDropWith(canary("rejected"));
    let refused = exec.spawn_gated(&gate, Priority::Interactive, async move {
        let _probe = probe;
    });
    assert!(refused.is_finished());
    sealed.wait_failed(CANARY);

    // A thread start hook panics.
    let hooked = Executor::new(Config {
        workers: 1,
        batch_workers: 1,
        on_thread_start: Some(Arc::new(|_| panic!("{}", canary("thread start")))),
    });
    assert_eq!(block_on(hooked.run(Priority::Background, || 1)), Ok(1));
    assert_eq!(hooked.shutdown(LIMIT), Shutdown::Clean);

    // A shutdown callback panics; an unregistered one panics in its drop.
    let _kept = exec.on_shutdown(Box::new(|| panic!("{}", canary("shutdown"))));
    let probe = PanicOnDropWith(canary("unregistered"));
    drop(exec.on_shutdown(Box::new(move || {
        let _probe = &probe;
    })));
    assert_eq!(exec.shutdown(LIMIT), Shutdown::Clean);

    let panicked = records(capture, "task panicked");
    for phase in ["poll", "result", "drop", "run"] {
        assert!(
            any_with(
                &panicked,
                &[&format!("\"phase\":\"{phase}\""), "\"lv\":\"error\""]
            ),
            "no `task panicked` record for {phase}: {panicked:#?}"
        );
    }
    let hooks = records(capture, "shutdown hook panicked");
    assert!(any_with(&hooks, &["\"phase\":\"run\""]), "{hooks:#?}");
    assert!(any_with(&hooks, &["\"phase\":\"drop\""]), "{hooks:#?}");
    let start = records(capture, "thread start hook panicked");
    assert!(any_with(&start, &["\"kind\":\"worker\""]), "{start:#?}");
    capture.assert_absent(CANARY);
}

#[test]
fn panic_records_carry_plugin_act_and_task() {
    let capture = capture();
    let plugin = TestPlugin::start();
    let act = plugin.handle.token().id();
    let exec = Executor::manual();
    let (seen, scopes) = mpsc::channel();
    let e = exec.clone();
    assert!(plugin.handle.own_with(move |gate| {
        let inner = e.clone();
        let first = e.spawn_gated(&gate, Priority::Interactive, async move {
            seen.send(("gated", lectern_logging::current())).unwrap();
            // Spawned from inside the gated task: no gate, the same plugin
            // and activation, a task number of its own.
            let seen = seen.clone();
            inner
                .spawn(Priority::Background, async move {
                    seen.send(("nested", lectern_logging::current())).unwrap();
                    panic!("nested logs");
                })
                .detach();
            panic!("gated logs");
        });
        // Dropped unpolled once the activation is dead; the drop panics.
        let probe = PanicOnDrop;
        let second = e.spawn_gated(&gate, Priority::Interactive, async move {
            let _probe = probe;
        });
        (first, second)
    }));
    exec.run_until_idle();
    let (_, gated) = recv(&scopes, "the gated task's scope");
    let (_, nested) = recv(&scopes, "the nested task's scope");
    plugin.kernel.barrier();
    assert!(matches!(
        plugin.state(),
        lectern_kernel::PluginState::Failed(_)
    ));

    let p = Some(PluginTag::new("p"));
    assert_eq!((gated.plugin, gated.act), (p, Some(act)));
    assert_eq!((nested.plugin, nested.act), (p, Some(act)));
    let (gated_task, nested_task) = (gated.task.unwrap(), nested.task.unwrap());
    assert!(gated_task >= 1 && nested_task > gated_task);
    // The scope ends with the step: this thread ran both tasks.
    assert_eq!(lectern_logging::current(), Ctx::default());

    let scoped = |task: u64| format!("\"plugin\":\"p\",\"act\":{act},\"task\":{task},\"msg\"");
    let panicked = records(capture, "task panicked");
    let poll = [
        scoped(gated_task),
        "\"phase\":\"poll\"".into(),
        "\"message\":\"gated logs\"".into(),
    ];
    assert!(
        any_with(&panicked, &poll.each_ref().map(String::as_str)),
        "{panicked:#?}"
    );
    let nested_poll = [scoped(nested_task), "\"message\":\"nested logs\"".into()];
    assert!(
        any_with(&panicked, &nested_poll.each_ref().map(String::as_str)),
        "{panicked:#?}"
    );
    // The second gated task: its own number, after the first.
    let second = panicked.iter().find(|line| {
        line.contains(&format!("\"plugin\":\"p\",\"act\":{act},\"task\":"))
            && line.contains("\"phase\":\"drop\"")
            && line.contains("\"message\":\"drop panicked\"")
    });
    assert!(second.is_some(), "{panicked:#?}");

    // Work refused by a sealed gate: the gate's plugin and act, no task.
    let other = TestPlugin::start();
    let other_act = other.handle.token().id();
    let gate = sealed_gate(&other);
    let probe = PanicOnDrop;
    let refused = exec.spawn_gated(&gate, Priority::Interactive, async move {
        let _probe = probe;
    });
    assert!(refused.is_finished());
    let panicked = records(capture, "task panicked");
    let rejected = format!("\"plugin\":\"p\",\"act\":{other_act},\"msg\"");
    assert!(
        any_with(&panicked, &[&rejected, "\"phase\":\"drop\""]),
        "{panicked:#?}"
    );
}

#[test]
fn a_task_spawned_inside_a_scope_inherits_it() {
    let capture = capture();
    let exec = Executor::manual();
    let (seen, scopes) = mpsc::channel();
    let outer = Ctx {
        session: Some([7; 16]),
        review: Some(0x5eed),
        ..Ctx::default()
    };
    let (task, dedicated) = {
        let _scope = lectern_logging::scope(outer);
        let s = seen.clone();
        let task = exec.spawn(Priority::Background, async move {
            s.send(lectern_logging::current()).unwrap();
            panic!("inherited");
        });
        let dedicated = exec.run_dedicated("scoped-dedicated", lectern_logging::current);
        (task, dedicated)
    };
    // The submitter's scope does not reach the thread that runs the task.
    exec.run_until_idle();
    assert_eq!(lectern_logging::current(), Ctx::default());
    assert!(block_on(task).is_err());

    let ctx = recv(&scopes, "the task's scope");
    assert_eq!((ctx.session, ctx.review), (outer.session, outer.review));
    assert_eq!((ctx.plugin, ctx.act), (None, None));
    let number = ctx.task.expect("a task number");
    let on_thread = block_on(dedicated).unwrap();
    assert_eq!(
        (on_thread.session, on_thread.review),
        (outer.session, outer.review)
    );
    assert!(on_thread.task.is_some_and(|n| n > number));

    let panicked = records(capture, "task panicked");
    let scoped = format!(
        "\"task\":{number},\"session\":\"07070707-0707-0707-0707-070707070707\",\"review\":"
    );
    assert!(
        any_with(&panicked, &[&scoped, "\"message\":\"inherited\""]),
        "{panicked:#?}"
    );
}
