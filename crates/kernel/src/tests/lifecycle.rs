//! Automatic activation and deactivation, commit, revocation order, threads.

use std::sync::Arc;

use super::support::*;
use crate::{Activation, PluginError, PluginSet, PluginState, Staged, service};

fn abc(log: &Log) -> PluginSet {
    PluginSet::new()
        .with(
            "a",
            chain_plugin(
                "a",
                log,
                vec![],
                vec![service::<dyn SvcB>()],
                after_first_frame(),
            ),
        )
        .with(
            "b",
            chain_plugin(
                "b",
                log,
                vec![service::<dyn SvcB>()],
                vec![service::<dyn SvcC>()],
                after_first_frame(),
            ),
        )
        .with(
            "c",
            chain_plugin(
                "c",
                log,
                vec![service::<dyn SvcC>()],
                vec![],
                after_first_frame(),
            ),
        )
}

#[test]
fn activates_in_dependency_order_and_deactivates_dependents_first() {
    let log = Log::default();
    let kernel = start(abc(&log));
    log.push("-- disable c");
    kernel.set_enabled("c", false);
    kernel.barrier();
    let disabled = states(&kernel);
    log.push("-- enable c");
    kernel.set_enabled("c", true);
    kernel.barrier();
    let enabled = states(&kernel);
    insta::assert_snapshot!(log.text());
    insta::assert_debug_snapshot!((disabled, enabled));
    shutdown(&kernel);
}

#[test]
fn nothing_runs_before_start() {
    let log = Log::default();
    let set = abc(&log);
    assert!(log.snapshot().is_empty());
    let kernel = start(set);
    assert!(!log.snapshot().is_empty());
    shutdown(&kernel);
}

#[test]
fn activate_and_started_run_on_the_kernel_thread() {
    let log = Log::default();
    let threads = Log::default();
    let (t1, t2) = (threads.clone(), threads.clone());
    let set = PluginSet::new().with(
        "p",
        TestPlugin::new(manifest(vec![], vec![], after_first_frame())).activate(move |ctx| {
            t1.push(format!(
                "activate on {}",
                std::thread::current().name().unwrap_or("?")
            ));
            ctx.own(Guard("g".into(), log.clone()));
            let t2 = t2.clone();
            Ok(Box::new(OnStarted(Some(Box::new(move |_| {
                t2.push(format!(
                    "started on {}",
                    std::thread::current().name().unwrap_or("?")
                ));
            })))))
        }),
    );
    let kernel = Kernel::start(set);
    // Public methods return immediately; work happens on the kernel thread.
    kernel.barrier();
    assert_eq!(
        threads.snapshot(),
        vec!["activate on kernel", "started on kernel"]
    );
    shutdown(&kernel);
}

use crate::Kernel;

#[test]
fn a_failed_activate_publishes_nothing() {
    let log = Log::default();
    let seen = Log::default();
    let seen2 = seen.clone();
    let log2 = log.clone();
    let set = PluginSet::new()
        .with(
            "provider",
            TestPlugin::new(manifest(
                vec![service::<dyn Greeter>()],
                vec![],
                after_first_frame(),
            ))
            .activate(move |ctx| {
                ctx.provide::<dyn Greeter>(Arc::new(Hello("hi")))?;
                let log = log2.clone();
                ctx.on(move |ping: &Ping, _| log.push(format!("provider got {}", ping.0)));
                ctx.own(Guard("provider".into(), log2.clone()));
                Err(PluginError::new("cannot open database"))
            }),
        )
        .with(
            "dependent",
            TestPlugin::new(manifest(
                vec![],
                vec![service::<dyn Greeter>()],
                after_first_frame(),
            ))
            .activate(move |_| {
                seen2.push("dependent activated");
                Ok(Box::new(()))
            }),
        );
    let kernel = start(set);
    kernel.emitter().emit(Ping(1));
    assert_eq!(
        state(&kernel, "provider"),
        PluginState::Failed(PluginError::new("cannot open database"))
    );
    assert!(matches!(
        state(&kernel, "dependent"),
        PluginState::Waiting { .. }
    ));
    assert!(seen.snapshot().is_empty());
    assert_eq!(log.snapshot(), vec!["drop guard provider"]);
    shutdown(&kernel);
}

#[test]
fn a_missing_declared_service_fails_the_activation() {
    let set = PluginSet::new().with(
        "p",
        TestPlugin::new(manifest(
            vec![service::<dyn Greeter>()],
            vec![],
            after_first_frame(),
        )),
    );
    let kernel = start(set);
    assert_eq!(
        state(&kernel, "p"),
        PluginState::Failed(PluginError::MissingService("greeter"))
    );
    shutdown(&kernel);
}

#[test]
fn revocation_drops_listeners_guards_and_state_in_order() {
    let log = Log::default();
    let log2 = log.clone();
    let slot = HandleSlot::default();
    let slot2 = slot.clone();
    let set = PluginSet::new().with(
        "p",
        TestPlugin::new(manifest(
            vec![service::<dyn Greeter>()],
            vec![],
            after_first_frame(),
        ))
        .activate(move |ctx| {
            ctx.provide::<dyn Greeter>(Arc::new(Hello("hi")))?;
            ctx.own(Guard("1".into(), log2.clone()));
            ctx.own(Guard("2".into(), log2.clone()));
            let log = log2.clone();
            ctx.on(move |ping: &Ping, _| log.push(format!("got {}", ping.0)));
            let slot = slot2.clone();
            let log = log2.clone();
            Ok(Box::new(Probe::new("p", &log2).started(move |handle| {
                handle.own(Guard("3".into(), log.clone()));
                slot.set(handle);
            })))
        }),
    );
    let kernel = start(set);
    kernel.emitter().emit(Ping(1));
    kernel.set_enabled("p", false);
    kernel.barrier();
    kernel.emitter().emit(Ping(2));
    insta::assert_snapshot!(log.text());
    assert!(!slot.get().is_active());
    shutdown(&kernel);
}

#[test]
fn a_panicking_guard_drop_still_drops_the_rest_and_fails_the_plugin() {
    struct Bomb;
    impl Drop for Bomb {
        fn drop(&mut self) {
            panic!("guard drop");
        }
    }
    let log = Log::default();
    let log2 = log.clone();
    let set = PluginSet::new().with(
        "p",
        TestPlugin::new(manifest(vec![], vec![], after_first_frame())).activate(move |ctx| {
            ctx.own(Guard("first".into(), log2.clone()));
            ctx.own(Bomb);
            ctx.own(Guard("last".into(), log2.clone()));
            Ok(Box::new(Probe::new("p", &log2)))
        }),
    );
    let kernel = start(set);
    kernel.set_enabled("p", false);
    kernel.barrier();
    assert_eq!(
        log.snapshot(),
        vec!["started p", "drop guard last", "drop guard first", "drop p"]
    );
    kernel.set_enabled("p", true);
    kernel.barrier();
    // Re-enabling clears the failure and retries.
    assert_eq!(state(&kernel, "p"), PluginState::Active);
    shutdown(&kernel);
}

#[test]
fn staged_guards_commit_only_when_activation_commits() {
    struct Commit(&'static str, Log);
    impl Staged for Commit {
        fn commit(&mut self, token: crate::ActivationToken) {
            self.1
                .push(format!("commit {} {:?}", self.0, token.state()));
        }
    }
    let log = Log::default();
    let (l1, l2) = (log.clone(), log.clone());
    let set = PluginSet::new()
        .with(
            "ok",
            TestPlugin::new(manifest(vec![], vec![], after_first_frame())).activate(move |ctx| {
                ctx.own_staged(Commit("ok", l1.clone()));
                Ok(Box::new(()))
            }),
        )
        .with(
            "bad",
            TestPlugin::new(manifest(vec![], vec![], after_first_frame())).activate(move |ctx| {
                ctx.own_staged(Commit("bad", l2.clone()));
                Err(PluginError::new("no"))
            }),
        );
    let kernel = start(set);
    assert_eq!(log.snapshot(), vec!["commit ok Pending"]);
    shutdown(&kernel);
}

#[test]
fn a_panicking_commit_fails_the_plugin_and_revocation_completes() {
    struct Panics;
    impl Staged for Panics {
        fn commit(&mut self, _: crate::ActivationToken) {
            panic!("commit");
        }
    }
    let log = Log::default();
    let log2 = log.clone();
    let set = PluginSet::new().with(
        "p",
        TestPlugin::new(manifest(vec![], vec![], after_first_frame())).activate(move |ctx| {
            ctx.own(Guard("g".into(), log2.clone()));
            ctx.own_staged(Panics);
            Ok(Box::new(Probe::new("p", &log2)))
        }),
    );
    let kernel = start(set);
    assert!(matches!(
        state(&kernel, "p"),
        PluginState::Failed(PluginError::Panicked(_))
    ));
    assert_eq!(log.snapshot(), vec!["drop guard g", "drop p"]);
    shutdown(&kernel);
}

#[test]
fn started_receives_the_active_returned_by_activate() {
    let log = Log::default();
    let log2 = log.clone();
    let set = PluginSet::new().with(
        "p",
        TestPlugin::new(manifest(vec![], vec![], after_first_frame()))
            .activate(move |_| Ok(Box::new(Probe::new("the-one", &log2)))),
    );
    let kernel = start(set);
    kernel.set_enabled("p", false);
    kernel.barrier();
    assert_eq!(log.snapshot(), vec!["started the-one", "drop the-one"]);
    shutdown(&kernel);
}

#[test]
fn a_waiting_plugin_lists_missing_services() {
    let log = Log::default();
    let set = PluginSet::new()
        .with(
            "needs",
            chain_plugin(
                "needs",
                &log,
                vec![],
                vec![service::<dyn SvcA>()],
                after_first_frame(),
            ),
        )
        .with(
            "a",
            chain_plugin(
                "a",
                &log,
                vec![service::<dyn SvcA>()],
                vec![],
                vec![Activation::Manual],
            ),
        );
    let kernel = start(set);
    assert_eq!(
        state(&kernel, "needs"),
        PluginState::Waiting {
            missing: vec![service::<dyn SvcA>()]
        }
    );
    kernel.activate("a");
    kernel.barrier();
    assert_eq!(state(&kernel, "needs"), PluginState::Active);
    shutdown(&kernel);
}

#[test]
fn manual_plugins_activate_only_on_request() {
    let log = Log::default();
    let set = PluginSet::new().with(
        "m",
        chain_plugin("m", &log, vec![], vec![], vec![Activation::Manual]),
    );
    let kernel = start(set);
    assert_eq!(state(&kernel, "m"), PluginState::Inactive);
    kernel.activate("m");
    kernel.barrier();
    assert_eq!(state(&kernel, "m"), PluginState::Active);
    shutdown(&kernel);
}

#[test]
fn deactivation_releases_cached_services() {
    let greeter: Arc<dyn Greeter> = Arc::new(Hello("hi"));
    let provided = greeter.clone();
    let set = PluginSet::new()
        .with(
            "provider",
            TestPlugin::new(manifest(
                vec![service::<dyn Greeter>()],
                vec![],
                after_first_frame(),
            ))
            .activate(move |ctx| {
                ctx.provide::<dyn Greeter>(provided.clone())?;
                Ok(Box::new(()))
            }),
        )
        .with(
            "user",
            TestPlugin::new(manifest(
                vec![],
                vec![service::<dyn Greeter>()],
                after_first_frame(),
            ))
            .activate(|ctx| {
                struct Cached(
                    #[allow(dead_code, reason = "held only to keep the reference")]
                    crate::ServiceRef<dyn Greeter>,
                );
                impl crate::Active for Cached {}
                Ok(Box::new(Cached(ctx.service::<dyn Greeter>())))
            }),
        );
    let baseline = Arc::strong_count(&greeter);
    let kernel = start(set);
    assert!(Arc::strong_count(&greeter) > baseline);
    kernel.set_enabled("provider", false);
    kernel.barrier();
    assert_eq!(
        state(&kernel, "user"),
        PluginState::Waiting {
            missing: vec![service::<dyn Greeter>()]
        }
    );
    wait_until("references released", || {
        Arc::strong_count(&greeter) == baseline
    });
    shutdown(&kernel);
}
