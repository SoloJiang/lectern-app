//! Event dispatch semantics.

use std::sync::{Arc, Barrier};
use std::thread;

use super::support::*;
use crate::{PluginSet, PluginState, Subscription, service};

#[test]
fn listeners_run_in_subscription_order_and_nested_events_are_queued() {
    let log = Log::default();
    let (l1, l2) = (log.clone(), log.clone());
    let set = PluginSet::new()
        .with(
            "first",
            TestPlugin::new(manifest(vec![], vec![], after_first_frame())).activate(move |ctx| {
                let log = l1.clone();
                ctx.on(move |ping: &Ping, handle| {
                    log.push(format!("first ping {}", ping.0));
                    if ping.0 == 1 {
                        handle.emitter().emit(Pong(1));
                        log.push("first emitted pong");
                    }
                });
                let log = l1.clone();
                ctx.on(move |pong: &Pong, _| log.push(format!("first pong {}", pong.0)));
                Ok(Box::new(()))
            }),
        )
        .with(
            "second",
            TestPlugin::new(manifest(vec![], vec![], after_first_frame())).activate(move |ctx| {
                let log = l2.clone();
                ctx.on(move |ping: &Ping, _| log.push(format!("second ping {}", ping.0)));
                Ok(Box::new(()))
            }),
        );
    let kernel = start(set);
    kernel.emitter().emit(Ping(1));
    insta::assert_snapshot!(log.text());
    shutdown(&kernel);
}

#[test]
fn dropping_a_subscription_stops_delivery_from_the_next_emit() {
    let log = Log::default();
    let slot = HandleSlot::default();
    let kernel = start(PluginSet::new().with("p", handle_plugin(&slot, after_first_frame())));
    let handle = slot.get();
    let log2 = log.clone();
    let subscription: Subscription = handle
        .on(move |ping: &Ping, _| log2.push(format!("got {}", ping.0)))
        .unwrap();
    kernel.emitter().emit(Ping(1));
    drop(subscription);
    kernel.emitter().emit(Ping(2));
    let log3 = log.clone();
    handle
        .on(move |ping: &Ping, _| log3.push(format!("kept {}", ping.0)))
        .unwrap()
        .keep();
    kernel.emitter().emit(Ping(3));
    assert_eq!(log.snapshot(), vec!["got 1", "kept 3"]);
    shutdown(&kernel);
}

#[test]
fn a_panicking_listener_fails_its_plugin_and_skips_its_later_listeners_and_dependents() {
    let log = Log::default();
    let (l1, l2) = (log.clone(), log.clone());
    let set = PluginSet::new()
        .with(
            "provider",
            TestPlugin::new(manifest(
                vec![service::<dyn SvcA>()],
                vec![],
                after_first_frame(),
            ))
            .activate(move |ctx| {
                ctx.provide::<dyn SvcA>(Arc::new(()))?;
                ctx.on(|_: &Ping, _| panic!("listener"));
                let log = l1.clone();
                ctx.on(move |_: &Ping, _| log.push("provider second listener"));
                Ok(Box::new(()))
            }),
        )
        .with(
            "dependent",
            TestPlugin::new(manifest(
                vec![],
                vec![service::<dyn SvcA>()],
                after_first_frame(),
            ))
            .activate(move |ctx| {
                let log = l2.clone();
                ctx.on(move |_: &Ping, _| log.push("dependent listener"));
                Ok(Box::new(()))
            }),
        );
    let kernel = start(set);
    kernel.emitter().emit(Ping(1));
    kernel.barrier();
    assert!(log.snapshot().is_empty(), "{:?}", log.snapshot());
    assert!(matches!(state(&kernel, "provider"), PluginState::Failed(_)));
    assert!(matches!(
        state(&kernel, "dependent"),
        PluginState::Waiting { .. }
    ));
    shutdown(&kernel);
}

#[test]
fn queued_events_of_a_failed_emitter_are_dropped() {
    let log = Log::default();
    let l1 = log.clone();
    let set = PluginSet::new().with(
        "p",
        TestPlugin::new(manifest(vec![], vec![], after_first_frame())).activate(move |ctx| {
            ctx.on(|_: &Ping, handle| {
                handle.emitter().emit(Pong(1));
                panic!("after emitting");
            });
            let log = l1.clone();
            ctx.on(move |pong: &Pong, _| log.push(format!("pong {}", pong.0)));
            Ok(Box::new(()))
        }),
    );
    let kernel = start(set);
    kernel.emitter().emit(Ping(1));
    assert!(log.snapshot().is_empty());
    shutdown(&kernel);
}

#[test]
fn nested_event_source_is_the_emitter_not_the_listener() {
    // A's listener emits through B's emitter. If B fails before the queued
    // event runs it is dropped; if only A fails it is delivered.
    for fail_b in [true, false] {
        let log = Log::default();
        let a_slot = HandleSlot::default();
        let b_slot = HandleSlot::default();
        let l = log.clone();
        let set = PluginSet::new()
            .with("a", handle_plugin(&a_slot, after_first_frame()))
            .with("b", handle_plugin(&b_slot, after_first_frame()))
            .with(
                "observer",
                TestPlugin::new(manifest(vec![], vec![], after_first_frame())).activate(
                    move |ctx| {
                        let log = l.clone();
                        ctx.on(move |pong: &Pong, _| log.push(format!("pong {}", pong.0)));
                        Ok(Box::new(()))
                    },
                ),
            );
        let kernel = start(set);
        let a = a_slot.get();
        let b = b_slot.get();
        let b_emitter = b.emitter();
        let b_for_listener = b.clone();
        let a_for_listener = a.clone();
        a.on(move |_: &Ping, _| {
            b_emitter.emit(Pong(7));
            if fail_b {
                b_for_listener.fail(crate::PluginError::new("b failed"));
            } else {
                a_for_listener.fail(crate::PluginError::new("a failed"));
            }
        })
        .unwrap()
        .keep();
        kernel.emitter().emit(Ping(1));
        let expected: Vec<String> = if fail_b {
            vec![]
        } else {
            vec!["pong 7".into()]
        };
        assert_eq!(log.snapshot(), expected, "fail_b = {fail_b}");
        shutdown(&kernel);
    }
}

#[test]
fn a_listener_can_deactivate_its_own_plugin_without_deadlock() {
    let slot = HandleSlot::default();
    let kernel = start(PluginSet::new().with("p", handle_plugin(&slot, after_first_frame())));
    let handle = slot.get();
    handle
        .on(|_: &Ping, handle| handle.fail(crate::PluginError::new("stop")))
        .unwrap()
        .keep();
    kernel.emitter().emit(Ping(1));
    kernel.barrier();
    assert!(matches!(state(&kernel, "p"), PluginState::Failed(_)));
    shutdown(&kernel);
}

#[test]
fn concurrent_emits_and_lifecycle_requests_do_not_deadlock() {
    let log = Log::default();
    let kernel = start(
        PluginSet::new()
            .with(
                "a",
                chain_plugin(
                    "a",
                    &log,
                    vec![service::<dyn SvcA>()],
                    vec![],
                    after_first_frame(),
                ),
            )
            .with(
                "b",
                TestPlugin::new(manifest(
                    vec![],
                    vec![service::<dyn SvcA>()],
                    after_first_frame(),
                ))
                .activate(|ctx| {
                    ctx.on(|ping: &Ping, handle| {
                        if ping.0.is_multiple_of(50) {
                            handle.emitter().emit(Pong(ping.0));
                        }
                    });
                    Ok(Box::new(()))
                }),
            ),
    );
    let barrier = Arc::new(Barrier::new(5));
    let mut threads = Vec::new();
    for t in 0..4 {
        let emitter = kernel.emitter();
        let barrier = barrier.clone();
        threads.push(thread::spawn(move || {
            barrier.wait();
            for i in 0..2000 {
                emitter.emit(Ping(t * 10_000 + i));
            }
        }));
    }
    barrier.wait();
    for i in 0..200 {
        kernel.set_enabled("a", i % 2 == 1);
    }
    for thread in threads {
        thread.join().unwrap();
    }
    kernel.barrier();
    assert_eq!(state(&kernel, "a"), PluginState::Active);
    shutdown(&kernel);
}

#[test]
fn deactivation_waits_for_a_running_listener() {
    let log = Log::default();
    let l = log.clone();
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let (e, r) = (entered.clone(), release.clone());
    let set = PluginSet::new().with(
        "p",
        TestPlugin::new(manifest(vec![], vec![], after_first_frame())).activate(move |ctx| {
            let (e, r, log) = (e.clone(), r.clone(), l.clone());
            ctx.on(move |_: &Ping, _| {
                e.wait();
                r.wait();
                log.push("listener done");
            });
            ctx.own(Guard("g".into(), l.clone()));
            Ok(Box::new(()))
        }),
    );
    let kernel = start(set);
    let emitter = kernel.emitter();
    let worker = thread::spawn(move || emitter.emit(Ping(1)));
    entered.wait();
    kernel.set_enabled("p", false);
    thread::sleep(std::time::Duration::from_millis(50));
    assert!(
        log.snapshot().is_empty(),
        "guard dropped while listener ran"
    );
    release.wait();
    worker.join().unwrap();
    kernel.barrier();
    assert_eq!(log.snapshot(), vec!["listener done", "drop guard g"]);
    shutdown(&kernel);
}
