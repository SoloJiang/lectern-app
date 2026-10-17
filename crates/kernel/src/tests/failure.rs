//! Panic isolation and failure propagation.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::support::*;
use crate::{PluginError, PluginSet, PluginState, contain, service};

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
