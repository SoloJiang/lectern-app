//! Panic isolation and failure propagation.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::support::*;
use crate::{PluginError, PluginSet, PluginState, TokenState, contain, service};

/// A provider of `SvcA` whose handle is captured, and a `dependent` on it;
/// both log `started` and `drop` under their names.
fn provider_and_dependent(log: &Log, slot: &HandleSlot) -> PluginSet {
    let l = log.clone();
    let s = slot.clone();
    PluginSet::new()
        .with(
            "provider",
            TestPlugin::new(manifest(
                vec![service::<dyn SvcA>()],
                vec![],
                after_first_frame(),
            ))
            .activate(move |ctx| {
                ctx.provide::<dyn SvcA>(Arc::new(()))?;
                let s = s.clone();
                Ok(Box::new(
                    Probe::new("provider", &l).started(move |h| s.set(h)),
                ))
            }),
        )
        .with(
            "dependent",
            chain_plugin(
                "dependent",
                log,
                vec![],
                vec![service::<dyn SvcA>()],
                after_first_frame(),
            ),
        )
}

#[test]
fn token_fail_from_another_thread_deactivates_the_plugin_and_its_dependents() {
    let log = Log::default();
    let slot = HandleSlot::default();
    let kernel = start(provider_and_dependent(&log, &slot));
    log.take();
    let token = slot.get().token();
    // The caller holds an admission, as a foreign plugin running one of this
    // activation's closures does; `fail` must not wait for it.
    let admission = token.enter().expect("live");
    let t = token.clone();
    std::thread::spawn(move || t.fail(PluginError::Panicked("leave guard panicked".into())))
        .join()
        .unwrap();
    assert_eq!(token.state(), TokenState::Dead);
    assert!(token.enter().is_none());
    drop(admission);
    kernel.barrier();
    assert_eq!(log.snapshot(), vec!["drop dependent", "drop provider"]);
    assert_eq!(
        state(&kernel, "provider"),
        PluginState::Failed(PluginError::Panicked("leave guard panicked".into()))
    );
    assert!(matches!(
        state(&kernel, "dependent"),
        PluginState::Waiting { .. }
    ));
    assert!(kernel.is_quiescent(), "quiescent after the revocation");
    shutdown(&kernel);
}

#[test]
fn token_fail_twice_keeps_the_first_error() {
    let log = Log::default();
    let slot = HandleSlot::default();
    let kernel = start(provider_and_dependent(&log, &slot));
    log.take();
    let token = slot.get().token();
    token.fail(PluginError::new("first"));
    token.fail(PluginError::new("second"));
    kernel.barrier();
    assert_eq!(log.snapshot(), vec!["drop dependent", "drop provider"]);
    assert_eq!(
        state(&kernel, "provider"),
        PluginState::Failed(PluginError::new("first"))
    );
    assert!(kernel.is_quiescent());
    shutdown(&kernel);
}

#[test]
fn token_fail_while_pending_fails_the_activation() {
    let log = Log::default();
    let l = log.clone();
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
                Ok(Box::new(Probe::new("provider", &l).started(|h| {
                    let token = h.token();
                    assert_eq!(token.state(), TokenState::Pending);
                    std::thread::spawn(move || token.fail(PluginError::new("while pending")))
                        .join()
                        .unwrap();
                    assert_eq!(h.token().state(), TokenState::Dead);
                })))
            }),
        )
        .with(
            "dependent",
            chain_plugin(
                "dependent",
                &log,
                vec![],
                vec![service::<dyn SvcA>()],
                after_first_frame(),
            ),
        );
    let kernel = start(set);
    // The `Sweep` is queued by the kernel thread itself during startup, so a
    // `barrier` does not cover it (kernel.md, 实际验证, barrier 的已知限制).
    wait_until("the provider failed", || {
        state(&kernel, "provider") == PluginState::Failed(PluginError::new("while pending"))
    });
    assert_eq!(log.snapshot(), vec!["started provider", "drop provider"]);
    assert!(matches!(
        state(&kernel, "dependent"),
        PluginState::Waiting { .. }
    ));
    wait_until("quiescent", || kernel.is_quiescent());
    shutdown(&kernel);
}

#[test]
fn token_fail_after_dead_does_nothing() {
    let log = Log::default();
    let slot = HandleSlot::default();
    let kernel = start(provider_and_dependent(&log, &slot));
    let token = slot.get().token();
    kernel.set_enabled("provider", false);
    kernel.barrier();
    assert_eq!(token.state(), TokenState::Dead);
    assert_eq!(state(&kernel, "provider"), PluginState::Disabled);
    // A stale token of a deactivated activation: the plugin stays `Disabled`
    // and the next activation is not touched.
    token.fail(PluginError::new("too late"));
    kernel.barrier();
    assert_eq!(state(&kernel, "provider"), PluginState::Disabled);
    kernel.set_enabled("provider", true);
    kernel.barrier();
    assert_eq!(state(&kernel, "provider"), PluginState::Active);
    // The dependent's activation is a request the kernel thread queues for
    // itself after the barrier.
    wait_until("the dependent reactivated", || {
        state(&kernel, "dependent") == PluginState::Active
    });
    wait_until("quiescent", || kernel.is_quiescent());
    log.take();
    token.fail(PluginError::new("still too late"));
    kernel.barrier();
    assert_eq!(state(&kernel, "provider"), PluginState::Active);
    assert_eq!(slot.get().token().state(), TokenState::Live);
    assert!(log.snapshot().is_empty(), "{:?}", log.snapshot());
    assert!(kernel.is_quiescent());
    shutdown(&kernel);
    // After `shutdown` the kernel thread is gone: only the state is written.
    token.fail(PluginError::new("after shutdown"));
    let live = slot.get().token();
    live.fail(PluginError::new("after shutdown"));
    assert_eq!(live.state(), TokenState::Dead);
}

#[test]
fn a_panicking_activate_fails_only_that_plugin_and_its_dependents_stay_inactive() {
    let log = Log::default();
    let l = log.clone();
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
                ctx.own(Guard("partial".into(), l.clone()));
                panic!("activate");
            }),
        )
        .with(
            "dependent",
            chain_plugin(
                "dependent",
                &log,
                vec![],
                vec![service::<dyn SvcA>()],
                after_first_frame(),
            ),
        )
        .with(
            "other",
            chain_plugin("other", &log, vec![], vec![], after_first_frame()),
        );
    let kernel = start(set);
    insta::assert_debug_snapshot!(states(&kernel));
    insta::assert_snapshot!(log.text());
    shutdown(&kernel);
}

#[test]
fn started_panicking_deactivates_dependents_first() {
    let log = Log::default();
    let l = log.clone();
    let slot = HandleSlot::default();
    let s = slot.clone();
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
                let s = s.clone();
                Ok(Box::new(
                    Probe::new("provider", &l).started(move |h| s.set(h)),
                ))
            }),
        )
        .with(
            "dependent",
            chain_plugin(
                "dependent",
                &log,
                vec![],
                vec![service::<dyn SvcA>()],
                after_first_frame(),
            ),
        );
    let kernel = start(set);
    log.push("-- provider fails");
    slot.get().fail(PluginError::new("later failure"));
    kernel.barrier();
    insta::assert_snapshot!(log.text());
    shutdown(&kernel);
}

#[test]
fn fail_in_started_keeps_dependents_from_activating() {
    let log = Log::default();
    let l = log.clone();
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
                Ok(Box::new(
                    Probe::new("provider", &l).started(|h| h.fail(PluginError::new("no db"))),
                ))
            }),
        )
        .with(
            "dependent",
            chain_plugin(
                "dependent",
                &log,
                vec![],
                vec![service::<dyn SvcA>()],
                after_first_frame(),
            ),
        );
    let kernel = start(set);
    assert!(
        !log.snapshot()
            .iter()
            .any(|l| l.contains("activate dependent")),
        "{:?}",
        log.snapshot()
    );
    assert_eq!(
        state(&kernel, "provider"),
        PluginState::Failed(PluginError::new("no db"))
    );
    shutdown(&kernel);
}

#[test]
fn a_panicking_active_drop_still_completes_revocation() {
    struct Explodes;
    impl crate::Active for Explodes {}
    impl Drop for Explodes {
        fn drop(&mut self) {
            panic!("active drop");
        }
    }
    let log = Log::default();
    let l = log.clone();
    let set = PluginSet::new().with(
        "p",
        TestPlugin::new(manifest(vec![], vec![], after_first_frame())).activate(move |ctx| {
            ctx.own(Guard("g".into(), l.clone()));
            Ok(Box::new(Explodes))
        }),
    );
    let kernel = start(set);
    kernel.set_enabled("p", false);
    kernel.barrier();
    assert_eq!(log.snapshot(), vec!["drop guard g"]);
    assert_eq!(state(&kernel, "p"), PluginState::Disabled);
    shutdown(&kernel);
}

#[test]
fn panics_with_payloads_whose_drop_panics_do_not_unwind_the_kernel_thread() {
    struct Nasty;
    impl Drop for Nasty {
        fn drop(&mut self) {
            panic!("payload drop");
        }
    }
    let log = Log::default();
    let set = PluginSet::new()
        .with(
            "bad-activate",
            TestPlugin::new(manifest(vec![], vec![], after_first_frame()))
                .activate(|_| std::panic::panic_any(Nasty)),
        )
        .with(
            "bad-listener",
            TestPlugin::new(manifest(vec![], vec![], after_first_frame())).activate(|ctx| {
                ctx.on(|_: &Ping, _| std::panic::panic_any(Nasty));
                Ok(Box::new(()))
            }),
        )
        .with(
            "fine",
            chain_plugin("fine", &log, vec![], vec![], after_first_frame()),
        );
    let kernel = start(set);
    kernel.emitter().emit(Ping(1));
    kernel.barrier();
    assert!(matches!(
        state(&kernel, "bad-activate"),
        PluginState::Failed(_)
    ));
    assert!(matches!(
        state(&kernel, "bad-listener"),
        PluginState::Failed(_)
    ));
    assert_eq!(state(&kernel, "fine"), PluginState::Active);
    shutdown(&kernel);
}

#[test]
fn a_blocking_payload_drop_does_not_hold_up_later_panics() {
    struct Blocks(Arc<Mutex<()>>);
    impl Drop for Blocks {
        fn drop(&mut self) {
            let _held = self.0.lock().unwrap();
        }
    }
    let lock = Arc::new(Mutex::new(()));
    let held = lock.lock().unwrap();
    let begin = Instant::now();
    let first = contain(|| std::panic::panic_any(Blocks(lock.clone())));
    assert_eq!(first.unwrap_err(), "panic with a non-string payload");
    let second = contain(|| panic!("plain"));
    assert_eq!(second.unwrap_err(), "plain");
    let third = contain(|| std::panic::panic_any(Blocks(lock.clone())));
    assert!(third.is_err());
    assert!(begin.elapsed() < Duration::from_secs(1));
    drop(held);
}

#[test]
fn rejected_closures_with_panicking_drops_do_not_unwind_the_caller() {
    struct Bomb;
    impl Drop for Bomb {
        fn drop(&mut self) {
            panic!("captured value drop");
        }
    }
    let slot = HandleSlot::default();
    let kernel = start(PluginSet::new().with("p", handle_plugin(&slot, after_first_frame())));
    let handle = slot.get();
    kernel.set_enabled("p", false);
    kernel.barrier();
    let bomb = Bomb;
    assert!(!handle.own_with(move |_| drop(bomb)));
    let bomb = Bomb;
    assert!(
        handle
            .on(move |_: &Ping, _| {
                let _ = &bomb;
            })
            .is_none()
    );
    assert!(!handle.own(Bomb));
    shutdown(&kernel);
}
