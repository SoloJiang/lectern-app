//! Service handles, multi-provider sets and admissions.

use std::sync::{Arc, Barrier, Mutex};
use std::thread;
use std::time::Duration;

use super::support::*;
use crate::{
    Activation, Active, PluginError, PluginSet, PluginState, ServiceAdded, ServiceRef,
    ServiceRemoved, ServiceSet, service,
};

fn source_provider(name: &'static str, activation: Vec<Activation>) -> TestPlugin {
    TestPlugin::new(manifest(vec![service::<dyn Source>()], vec![], activation)).activate(
        move |ctx| {
            ctx.provide::<dyn Source>(Arc::new(Named(name.to_owned())))?;
            Ok(Box::new(()))
        },
    )
}

type SetSlot = Arc<Mutex<Option<ServiceSet<dyn Source>>>>;

fn source_user(slot: &SetSlot, log: &Log) -> TestPlugin {
    let slot = slot.clone();
    let log = log.clone();
    TestPlugin::new(manifest(
        vec![],
        vec![service::<dyn Source>()],
        after_first_frame(),
    ))
    .activate(move |ctx| {
        *slot.lock().unwrap() = Some(ctx.service_set::<dyn Source>());
        log.push("user activated");
        let l1 = log.clone();
        ctx.on(move |added: &ServiceAdded<dyn Source>, _| {
            l1.push(format!("added {}", added.provider))
        });
        let l2 = log.clone();
        ctx.on(move |removed: &ServiceRemoved<dyn Source>, _| {
            l2.push(format!("removed {}", removed.provider))
        });
        Ok(Box::new(()))
    })
}

fn names(set: &ServiceSet<dyn Source>) -> Vec<String> {
    let mut names = Vec::new();
    set.for_each(|source| names.push(source.name()));
    assert_eq!(
        names,
        set.current().iter().map(|s| s.name()).collect::<Vec<_>>()
    );
    names
}

#[test]
fn multi_provider_changes_update_the_set_without_reactivating_dependents() {
    let log = Log::default();
    let slot: SetSlot = Arc::default();
    let set = PluginSet::new()
        .with("one", source_provider("one", after_first_frame()))
        .with("two", source_provider("two", vec![Activation::Manual]))
        .with("user", source_user(&slot, &log));
    let kernel = start(set);
    let sources = slot.lock().unwrap().clone().unwrap();
    assert_eq!(names(&sources), vec!["one"]);
    kernel.activate("two");
    kernel.barrier();
    assert_eq!(names(&sources), vec!["one", "two"]);
    kernel.set_enabled("one", false);
    kernel.barrier();
    assert_eq!(names(&sources), vec!["two"]);
    assert_eq!(state(&kernel, "user"), PluginState::Active);
    kernel.set_enabled("two", false);
    kernel.barrier();
    assert!(matches!(
        state(&kernel, "user"),
        PluginState::Waiting { .. }
    ));
    insta::assert_snapshot!(log.text());
    shutdown(&kernel);
}

#[test]
fn a_new_provider_is_invisible_until_live_and_after_a_failed_started() {
    let log = Log::default();
    let slot: SetSlot = Arc::default();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen2 = seen.clone();
    let slot2 = slot.clone();
    let set = PluginSet::new()
        .with("one", source_provider("one", after_first_frame()))
        .with("user", source_user(&slot, &log))
        .with(
            "late",
            TestPlugin::new(manifest(
                vec![service::<dyn Source>()],
                vec![],
                vec![Activation::Manual],
            ))
            .activate(move |ctx| {
                ctx.provide::<dyn Source>(Arc::new(Named("late".into())))?;
                let (slot, seen) = (slot2.clone(), seen2.clone());
                Ok(Box::new(OnStarted(Some(Box::new(move |_| {
                    let set = slot.lock().unwrap().clone().unwrap();
                    seen.lock().unwrap().push(names(&set));
                    panic!("started fails");
                })))))
            }),
        );
    let kernel = start(set);
    kernel.activate("late");
    kernel.barrier();
    assert_eq!(*seen.lock().unwrap(), vec![vec!["one".to_owned()]]);
    let sources = slot.lock().unwrap().clone().unwrap();
    assert_eq!(names(&sources), vec!["one"]);
    assert!(
        !log.snapshot().iter().any(|l| l.contains("late")),
        "{:?}",
        log.snapshot()
    );
    shutdown(&kernel);
}

#[test]
fn service_ref_returns_none_after_the_provider_fails() {
    let refs: Arc<Mutex<Option<ServiceRef<dyn Greeter>>>> = Arc::default();
    let refs2 = refs.clone();
    let provider_slot = HandleSlot::default();
    let p2 = provider_slot.clone();
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
                let p = p2.clone();
                Ok(Box::new(OnStarted(Some(Box::new(move |h| p.set(h))))))
            }),
        )
        .with(
            "user",
            TestPlugin::new(manifest(
                vec![],
                vec![service::<dyn Greeter>()],
                after_first_frame(),
            ))
            .activate(move |ctx| {
                *refs2.lock().unwrap() = Some(ctx.service::<dyn Greeter>());
                Ok(Box::new(()))
            }),
        );
    let kernel = start(set);
    let greeter = refs.lock().unwrap().clone().unwrap();
    assert_eq!(greeter.get().unwrap().hello(), "hi");
    provider_slot.get().fail(PluginError::new("gone"));
    assert!(greeter.get().is_none());
    kernel.barrier();
    shutdown(&kernel);
}

#[test]
fn revocation_waits_for_an_admitted_call() {
    let log = Log::default();
    let refs: Arc<Mutex<Option<ServiceRef<dyn Greeter>>>> = Arc::default();
    let refs2 = refs.clone();
    let l = log.clone();
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
                ctx.own(Guard("provider".into(), l.clone()));
                Ok(Box::new(()))
            }),
        )
        .with(
            "user",
            TestPlugin::new(manifest(
                vec![],
                vec![service::<dyn Greeter>()],
                vec![Activation::Manual],
            ))
            .activate(move |ctx| {
                *refs2.lock().unwrap() = Some(ctx.service::<dyn Greeter>());
                Ok(Box::new(()))
            }),
        );
    let kernel = start(set);
    kernel.activate("user");
    kernel.barrier();
    let greeter = refs.lock().unwrap().clone().unwrap();
    let call = greeter.get().unwrap();
    kernel.set_enabled("provider", false);
    thread::sleep(Duration::from_millis(50));
    assert!(log.snapshot().is_empty());
    assert!(kernel.quiescent_epoch().is_none());
    log.push("call returns");
    drop(call);
    kernel.barrier();
    assert_eq!(log.snapshot(), vec!["call returns", "drop guard provider"]);
    shutdown(&kernel);
}

#[test]
fn a_listener_can_call_a_service_and_fail_without_deadlock() {
    let log = Log::default();
    let l = log.clone();
    let set = PluginSet::new()
        .with("one", source_provider("one", after_first_frame()))
        .with(
            "user",
            TestPlugin::new(manifest(
                vec![],
                vec![service::<dyn Source>()],
                after_first_frame(),
            ))
            .activate(move |ctx| {
                let sources = ctx.service_set::<dyn Source>();
                let log = l.clone();
                ctx.on(move |_: &Ping, handle| {
                    sources.for_each(|s| log.push(s.name()));
                    handle.fail(PluginError::new("fail inside listener"));
                });
                Ok(Box::new(()))
            }),
        );
    let kernel = start(set);
    kernel.emitter().emit(Ping(1));
    kernel.barrier();
    assert_eq!(log.snapshot(), vec!["one"]);
    assert!(matches!(state(&kernel, "user"), PluginState::Failed(_)));
    shutdown(&kernel);
}

#[test]
fn losing_the_last_two_providers_concurrently_always_kills_the_dependent() {
    for _ in 0..300 {
        let a = HandleSlot::default();
        let b = HandleSlot::default();
        let user = HandleSlot::default();
        let provider = |slot: &HandleSlot, name: &'static str| {
            let slot = slot.clone();
            TestPlugin::new(manifest(
                vec![service::<dyn Source>()],
                vec![],
                after_first_frame(),
            ))
            .activate(move |ctx| {
                ctx.provide::<dyn Source>(Arc::new(Named(name.into())))?;
                let slot = slot.clone();
                Ok(Box::new(OnStarted(Some(Box::new(move |h| slot.set(h))))) as Box<dyn Active>)
            })
        };
        let u = user.clone();
        let set = PluginSet::new()
            .with("a", provider(&a, "a"))
            .with("b", provider(&b, "b"))
            .with(
                "user",
                TestPlugin::new(manifest(
                    vec![],
                    vec![service::<dyn Source>()],
                    after_first_frame(),
                ))
                .activate(move |_| {
                    let u = u.clone();
                    Ok(Box::new(OnStarted(Some(Box::new(move |h| u.set(h))))))
                }),
            );
        let kernel = start(set);
        let (ha, hb) = (a.get(), b.get());
        let go = Arc::new(Barrier::new(2));
        let go2 = go.clone();
        let t = thread::spawn(move || {
            go2.wait();
            ha.fail(PluginError::new("a"));
        });
        go.wait();
        hb.fail(PluginError::new("b"));
        t.join().unwrap();
        assert_eq!(user.get().token().state(), crate::TokenState::Dead);
        shutdown(&kernel);
    }
}
