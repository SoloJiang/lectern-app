//! Plugin handles after deactivation, staged work in `started`, admissions.

use std::sync::Arc;

use super::support::*;
use crate::{PluginError, PluginSet, PluginState, TokenState};

#[test]
fn a_stale_handle_rejects_every_operation_even_after_reactivation() {
    let log = Log::default();
    let slot = HandleSlot::default();
    let kernel = start(PluginSet::new().with("p", handle_plugin(&slot, after_first_frame())));
    let old = slot.get();
    let l = log.clone();
    old.on(move |ping: &Ping, _| l.push(format!("old listener {}", ping.0)))
        .unwrap()
        .keep();
    let old_emitter = old.emitter();

    kernel.set_enabled("p", false);
    kernel.barrier();
    kernel.set_enabled("p", true);
    kernel.barrier();
    assert_eq!(state(&kernel, "p"), PluginState::Active);

    let main_thread = std::thread::current().id();
    struct ThreadGuard(std::thread::ThreadId, Log);
    impl Drop for ThreadGuard {
        fn drop(&mut self) {
            let same = std::thread::current().id() == self.0;
            self.1
                .push(format!("guard dropped on calling thread: {same}"));
        }
    }
    assert!(!old.own(ThreadGuard(main_thread, log.clone())));
    assert!(old.on(|_: &Ping, _| {}).is_none());
    assert!(old.busy().is_none());
    assert!(!old.is_active());
    assert!(!old.own_with(|_| ()));
    let l = log.clone();
    let new = slot.get();
    new.on(move |ping: &Ping, _| l.push(format!("new listener {}", ping.0)))
        .unwrap()
        .keep();
    old_emitter.emit(Ping(1));
    kernel.emitter().emit(Ping(2));
    assert_eq!(
        log.snapshot(),
        vec!["guard dropped on calling thread: true", "new listener 2"]
    );
    shutdown(&kernel);
}

#[test]
fn guards_owned_before_deactivation_drop_on_the_kernel_thread() {
    let log = Log::default();
    let slot = HandleSlot::default();
    let kernel = start(PluginSet::new().with("p", handle_plugin(&slot, after_first_frame())));
    struct Where(Log);
    impl Drop for Where {
        fn drop(&mut self) {
            self.0
                .push(std::thread::current().name().unwrap_or("?").to_owned());
        }
    }
    assert!(slot.get().own(Where(log.clone())));
    kernel.set_enabled("p", false);
    kernel.barrier();
    assert_eq!(log.snapshot(), vec!["kernel"]);
    shutdown(&kernel);
}

#[test]
fn work_from_started_is_held_until_live() {
    let log = Log::default();
    let (l1, l2) = (log.clone(), log.clone());
    let set = PluginSet::new()
        .with(
            "observer",
            TestPlugin::new(manifest(vec![], vec![], after_first_frame())).activate(move |ctx| {
                let log = l1.clone();
                ctx.on(move |pong: &Pong, _| log.push(format!("pong {}", pong.0)));
                Ok(Box::new(()))
            }),
        )
        .with(
            "p",
            TestPlugin::new(manifest(vec![], vec![], vec![crate::Activation::Manual])).activate(
                move |_| {
                    let log = l2.clone();
                    Ok(Box::new(OnStarted(Some(Box::new(move |handle| {
                        handle.emitter().emit(Pong(1));
                        let l = log.clone();
                        assert!(handle.own_with(move |_| l.push("start called")));
                        handle.emitter().emit(Pong(2));
                        assert!(handle.busy().is_some(), "busy() works in started");
                        log.push(format!("token in started: {:?}", handle.token().state()));
                    })))))
                },
            ),
        );
    let kernel = start(set);
    kernel.activate("p");
    kernel.barrier();
    assert_eq!(
        log.snapshot(),
        vec![
            "token in started: Pending",
            "pong 1",
            "start called",
            "pong 2"
        ]
    );
    shutdown(&kernel);
}

#[test]
fn held_work_is_dropped_when_started_fails() {
    for fail in ["panic", "fail"] {
        let log = Log::default();
        let (l1, l2) = (log.clone(), log.clone());
        let set = PluginSet::new()
            .with(
                "observer",
                TestPlugin::new(manifest(vec![], vec![], after_first_frame())).activate(
                    move |ctx| {
                        let log = l1.clone();
                        ctx.on(move |pong: &Pong, _| log.push(format!("pong {}", pong.0)));
                        Ok(Box::new(()))
                    },
                ),
            )
            .with(
                "p",
                TestPlugin::new(manifest(vec![], vec![], vec![crate::Activation::Manual]))
                    .activate(move |_| {
                        let log = l2.clone();
                        Ok(Box::new(OnStarted(Some(Box::new(move |handle| {
                            handle.emitter().emit(Pong(1));
                            let l = log.clone();
                            handle.own_with(move |_| l.push("start called"));
                            if fail == "panic" {
                                panic!("started");
                            }
                            handle.fail(PluginError::new("started failed"));
                        })))))
                    }),
            );
        let kernel = start(set);
        kernel.activate("p");
        kernel.barrier();
        assert!(log.snapshot().is_empty(), "{fail}: {:?}", log.snapshot());
        assert!(matches!(state(&kernel, "p"), PluginState::Failed(_)));
        shutdown(&kernel);
    }
}

#[test]
fn the_first_held_event_failing_the_plugin_drops_the_rest() {
    let log = Log::default();
    let (l1, l2) = (log.clone(), log.clone());
    let slot = HandleSlot::default();
    let s = slot.clone();
    let set = PluginSet::new()
        .with(
            "observer",
            TestPlugin::new(manifest(vec![], vec![], after_first_frame())).activate(move |ctx| {
                let log = l1.clone();
                let s = s.clone();
                ctx.on(move |pong: &Pong, _| {
                    log.push(format!("pong {}", pong.0));
                    s.get().fail(PluginError::new("stop"));
                });
                Ok(Box::new(()))
            }),
        )
        .with(
            "p",
            TestPlugin::new(manifest(vec![], vec![], vec![crate::Activation::Manual])).activate(
                move |_| {
                    let log = l2.clone();
                    let slot = slot.clone();
                    Ok(Box::new(OnStarted(Some(Box::new(move |handle| {
                        slot.set(handle.clone());
                        handle.emitter().emit(Pong(1));
                        handle.emitter().emit(Pong(2));
                        let l = log.clone();
                        handle.own_with(move |_| l.push("start called"));
                    })))))
                },
            ),
        );
    let kernel = start(set);
    kernel.activate("p");
    kernel.barrier();
    assert_eq!(log.snapshot(), vec!["pong 1"]);
    shutdown(&kernel);
}

#[test]
fn fail_from_another_thread_marks_the_plugin_and_its_dependents_dead_before_returning() {
    let provider = HandleSlot::default();
    let user = HandleSlot::default();
    let (p, u) = (provider.clone(), user.clone());
    let set = PluginSet::new()
        .with(
            "provider",
            TestPlugin::new(manifest(
                vec![crate::service::<dyn SvcA>()],
                vec![],
                after_first_frame(),
            ))
            .activate(move |ctx| {
                ctx.provide::<dyn SvcA>(Arc::new(()))?;
                let p = p.clone();
                Ok(Box::new(OnStarted(Some(Box::new(move |h| p.set(h))))))
            }),
        )
        .with(
            "user",
            TestPlugin::new(manifest(
                vec![],
                vec![crate::service::<dyn SvcA>()],
                after_first_frame(),
            ))
            .activate(move |_| {
                let u = u.clone();
                Ok(Box::new(OnStarted(Some(Box::new(move |h| u.set(h))))))
            }),
        );
    let kernel = start(set);
    let handle = provider.get();
    std::thread::spawn(move || handle.fail(PluginError::new("background init failed")))
        .join()
        .unwrap();
    assert_eq!(provider.get().token().state(), TokenState::Dead);
    assert_eq!(user.get().token().state(), TokenState::Dead);
    kernel.barrier();
    assert_eq!(
        state(&kernel, "provider"),
        PluginState::Failed(PluginError::new("background init failed"))
    );
    shutdown(&kernel);
}

#[test]
fn enter_returns_none_once_dead() {
    let slot = HandleSlot::default();
    let kernel = start(PluginSet::new().with("p", handle_plugin(&slot, after_first_frame())));
    let token = slot.get().token();
    assert!(token.enter().is_some());
    slot.get().fail(PluginError::new("x"));
    assert!(token.enter().is_none());
    assert!(token.pending().is_some(), "pending() works on dead tokens");
    shutdown(&kernel);
}

#[test]
fn admission_own_registers_even_when_the_token_dies_meanwhile() {
    let log = Log::default();
    let slot = HandleSlot::default();
    let kernel = start(PluginSet::new().with("p", handle_plugin(&slot, after_first_frame())));
    let handle = slot.get();
    let token = handle.token();
    let admission = token.enter().unwrap();
    handle.fail(PluginError::new("concurrent failure"));
    assert!(admission.own(Guard("unregister".into(), log.clone())));
    drop(admission);
    kernel.barrier();
    assert_eq!(log.snapshot(), vec!["drop guard unregister"]);
    shutdown(&kernel);
}
