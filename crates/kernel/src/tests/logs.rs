//! Kernel logging (`docs/design/kernel.md`, 日志与诊断): the canary test,
//! the scope of panic records, activation records and plugin tags.

use std::sync::{Arc, Mutex};

use lectern_logging::testing::{self, Capture};

use super::support::*;
use crate::{Activation, Name, PluginError, PluginSet, WorkspaceFacts, service};

const CANARY: &str = "kernel-canary-5d81e0c7";

/// The trace-level capture, with the per-call-site limit out of the way: all
/// tests share one process under `cargo test`.
fn capture() -> &'static Capture {
    let capture = testing::capture();
    testing::set_callsite_limit(1_000_000);
    capture
}

/// The kernel's records with message `msg`.
fn records(capture: &Capture, msg: &str) -> Vec<String> {
    let msg = format!("\"msg\":\"{msg}\"");
    capture
        .lines()
        .into_iter()
        .filter(|line| line.contains("\"tg\":\"kernel\"") && line.contains(&msg))
        .collect()
}

/// The scope keys as the encoder writes them, in front of `msg`.
fn scoped(plugin: &str, act: u64) -> String {
    format!("\"plugin\":\"{plugin}\",\"act\":{act},")
}

/// A runtime string, so `panic!` formats it into a `String` payload.
fn canary(context: &str) -> String {
    format!("{context} {CANARY}")
}

#[derive(Clone, Default)]
struct Id(Arc<Mutex<Option<u64>>>);

impl Id {
    fn set(&self, id: u64) {
        *self.0.lock().unwrap() = Some(id);
    }

    fn get(&self) -> u64 {
        self.0.lock().unwrap().expect("activation id recorded")
    }
}

/// Panics with `message` when dropped.
struct PanicOnDrop(String);

impl Drop for PanicOnDrop {
    fn drop(&mut self) {
        panic!("{}", self.0);
    }
}

/// Panics with a formatted canary message.
struct PanickyFacts;

impl WorkspaceFacts for PanickyFacts {
    fn contains(&self, _pattern: &str) -> bool {
        panic!("{}", canary("contains"));
    }
}

fn plain() -> TestPlugin {
    TestPlugin::new(manifest(vec![], vec![], after_first_frame()))
}

fn listening(phase: &'static str) -> TestPlugin {
    plain().activate(move |ctx| {
        ctx.on::<Ping>(move |_, _| panic!("{}", canary(phase)));
        Ok(Box::new(()))
    })
}

#[test]
fn canary_stays_out_of_kernel_logs() {
    let capture = capture();
    let set = PluginSet::new()
        .with(
            "canary-activate",
            plain().activate(|_| panic!("{}", canary("activate"))),
        )
        .with(
            "canary-error",
            plain().activate(|_| Err(PluginError::new(canary("error")))),
        )
        .with(
            "canary-started",
            plain().activate(|_| {
                Ok(Box::new(OnStarted(Some(Box::new(|_| {
                    panic!("{}", canary("started"))
                })))))
            }),
        )
        .with("canary-listener", listening("listener"))
        .with(
            "canary-guard",
            plain().activate(|ctx| {
                ctx.own(PanicOnDrop(canary("guard")));
                Ok(Box::new(()))
            }),
        )
        .with(
            "canary-project",
            plain().activate(|_| {
                Ok(Box::new(OnStarted(Some(Box::new(|handle| {
                    handle.control().workspace_changed(Arc::new(PanickyFacts));
                })))))
            }),
        )
        .with(
            "canary-workspace",
            TestPlugin::new(manifest(
                vec![],
                vec![],
                vec![Activation::Workspace(vec![Name::Owned(format!(
                    "**/{CANARY}"
                ))])],
            )),
        );
    let kernel = start(set);
    kernel.barrier();
    kernel.emitter().emit(Ping(1));
    kernel.activate(&canary("unknown"));
    kernel.set_enabled(&canary("unknown"), false);
    kernel.set_enabled("canary-guard", false);
    kernel.command(&canary("command"), |result| {
        assert!(matches!(result, Err(PluginError::UnknownCommand(_))));
    });
    kernel.barrier();
    kernel.barrier();
    shutdown(&kernel);

    let failed = records(capture, "plugin failed");
    for phase in ["activate", "started", "listener", "drop", "workspace"] {
        assert!(
            failed
                .iter()
                .any(|line| line.contains(&format!("\"phase\":\"{phase}\""))),
            "no `plugin failed` record for {phase}: {failed:#?}"
        );
    }
    assert!(!records(capture, "plugin activation failed").is_empty());
    assert!(!records(capture, "unknown plugin").is_empty());
    capture.assert_absent(CANARY);
}

#[test]
fn panic_records_carry_plugin_and_act() {
    let capture = capture();
    let activate_id = Id::default();
    let started_id = Id::default();
    let listener_id = Id::default();
    let set = PluginSet::new()
        .with("scoped-activate", {
            let id = activate_id.clone();
            plain().activate(move |ctx| {
                id.set(ctx.token.id());
                panic!("activate");
            })
        })
        .with("scoped-started", {
            let id = started_id.clone();
            plain().activate(move |_| {
                let id = id.clone();
                Ok(Box::new(OnStarted(Some(Box::new(move |handle| {
                    id.set(handle.token().id());
                    panic!("{}", String::from("started"));
                })))))
            })
        })
        .with("scoped-listener", {
            let id = listener_id.clone();
            plain().activate(move |ctx| {
                id.set(ctx.token.id());
                ctx.own(PanicOnDrop(String::from("guard")));
                ctx.on::<Ping>(|_, _| panic!("listener"));
                Ok(Box::new(()))
            })
        });
    let kernel = start(set);
    kernel.barrier();
    kernel.emitter().emit(Ping(1));
    kernel.barrier();
    kernel.barrier();
    shutdown(&kernel);

    let failed = records(capture, "plugin failed");
    let find = |plugin: &str, act: u64, phase: &str| -> String {
        failed
            .iter()
            .find(|line| {
                line.contains(&scoped(plugin, act))
                    && line.contains(&format!("\"phase\":\"{phase}\""))
            })
            .unwrap_or_else(|| panic!("no {phase} record for {plugin} ({act}): {failed:#?}"))
            .clone()
    };
    // A `&'static str` payload is kept as text, a `String` only as its length.
    let activate = find("scoped-activate", activate_id.get(), "activate");
    assert!(activate.contains("\"message\":\"activate\""), "{activate}");
    let started = find("scoped-started", started_id.get(), "started");
    assert!(started.contains("\"message\":{\"len\":7}"), "{started}");
    let listener = find("scoped-listener", listener_id.get(), "listener");
    assert!(listener.contains("\"message\":\"listener\""), "{listener}");
    let drop = find("scoped-listener", listener_id.get(), "drop");
    assert!(drop.contains("\"message\":{\"len\":5}"), "{drop}");
    assert!(drop.contains("\"lv\":\"error\""), "{drop}");

    let deactivated = records(capture, "plugin deactivated after a failure");
    assert!(
        deactivated.iter().any(|line| {
            line.contains(&scoped("scoped-listener", listener_id.get()))
                && line.contains("\"error\":\"panicked\"")
        }),
        "{deactivated:#?}"
    );
    let aborted = records(capture, "plugin activation failed");
    assert!(
        aborted.iter().any(|line| {
            line.contains(&scoped("scoped-activate", activate_id.get()))
                && line.contains("\"error\":\"panicked\"")
        }),
        "{aborted:#?}"
    );
}

#[test]
fn token_fail_logs_one_deactivation_in_the_failed_activations_scope() {
    let capture = capture();
    let slot = HandleSlot::default();
    let kernel =
        start(PluginSet::new().with("token-fail", handle_plugin(&slot, after_first_frame())));
    let token = slot.get().token();
    let act = token.id();
    // No admission, another thread, called twice: one record, kind only.
    let t = token.clone();
    std::thread::spawn(move || {
        t.fail(PluginError::Panicked(canary("leave guard")));
        t.fail(PluginError::Panicked(canary("again")));
    })
    .join()
    .unwrap();
    kernel.barrier();
    shutdown(&kernel);

    let deactivated: Vec<String> = records(capture, "plugin deactivated after a failure")
        .into_iter()
        .filter(|line| line.contains(&scoped("token-fail", act)))
        .collect();
    assert_eq!(deactivated.len(), 1, "{deactivated:#?}");
    assert!(
        deactivated[0].contains("\"error\":\"panicked\""),
        "{}",
        deactivated[0]
    );
    assert!(
        !records(capture, "plugin failed")
            .iter()
            .any(|line| line.contains(&scoped("token-fail", act))),
        "`fail` itself logs no `plugin failed`"
    );
    capture.assert_absent(CANARY);
}

#[test]
fn app_command_callbacks_run_outside_the_failed_plugins_scope() {
    // `started` panics: the activation is revoked and the waiting `done` gets
    // the error from the revocation, which runs in the plugin's scope.
    let set = PluginSet::new().with(
        "scope-command",
        TestPlugin::new(manifest(
            vec![],
            vec![],
            vec![Activation::Command(Name::Borrowed("scope.run"))],
        ))
        .activate(|_| {
            Ok(Box::new(OnStarted(Some(Box::new(|_| {
                panic!("started");
            })))))
        }),
    );
    let kernel = start(set);
    let seen: Arc<Mutex<Option<(bool, lectern_logging::Ctx)>>> = Arc::default();
    let slot = seen.clone();
    kernel.command("scope.run", move |result| {
        *slot.lock().unwrap() = Some((result.is_err(), lectern_logging::current()));
    });
    wait_until("the command callback", || seen.lock().unwrap().is_some());
    let (failed, ctx) = seen.lock().unwrap().take().unwrap();
    assert!(failed, "the command reports the failure");
    assert_eq!(
        ctx,
        lectern_logging::Ctx::default(),
        "app's callback has no scope"
    );
    shutdown(&kernel);
}

#[test]
fn activation_logs_took_us_and_ids_increase() {
    let capture = capture();
    let first = Id::default();
    let second = Id::default();
    let set = PluginSet::new()
        .with("took-first", {
            let id = first.clone();
            TestPlugin::new(manifest(
                vec![service::<dyn SvcA>()],
                vec![],
                after_first_frame(),
            ))
            .activate(move |ctx| {
                id.set(ctx.token.id());
                ctx.provide::<dyn SvcA>(Arc::new(()))?;
                Ok(Box::new(()))
            })
        })
        .with("took-second", {
            let id = second.clone();
            TestPlugin::new(manifest(
                vec![],
                vec![service::<dyn SvcA>()],
                after_first_frame(),
            ))
            .activate(move |ctx| {
                id.set(ctx.token.id());
                Ok(Box::new(()))
            })
        });
    let kernel = start(set);
    kernel.barrier();
    let (a, b) = (first.get(), second.get());
    assert!(a >= 1 && b > a, "{a} then {b}");

    let activated = records(capture, "plugin activated");
    for (plugin, act) in [("took-first", a), ("took-second", b)] {
        let line = activated
            .iter()
            .find(|line| line.contains(&scoped(plugin, act)))
            .unwrap_or_else(|| panic!("no record for {plugin}: {activated:#?}"));
        for field in ["took_us", "activate_us", "commit_us", "started_us"] {
            assert!(line.contains(&format!("\"{field}\":")), "{line}");
        }
        assert!(line.contains("\"lv\":\"info\""), "{line}");
    }

    // Disabling deactivates both; re-enabling activates again with new IDs.
    kernel.set_enabled("took-first", false);
    kernel.barrier();
    kernel.set_enabled("took-first", true);
    kernel.barrier();
    wait_until("took-second reactivated", || second.get() > b);
    assert!(first.get() > b && second.get() > first.get());
    let deactivated = records(capture, "plugin deactivated");
    assert!(
        deactivated
            .iter()
            .any(|line| line.contains(&scoped("took-second", b))),
        "{deactivated:#?}"
    );
    shutdown(&kernel);
}

#[test]
fn process_plugin_names_are_registered() {
    let capture = capture();
    let registered = Id::default();
    let unregistered = Id::default();
    let panicking = |id: &Id| {
        let id = id.clone();
        plain().activate(move |ctx| {
            id.set(ctx.token.id());
            panic!("process plugin");
        })
    };
    let set = PluginSet::new()
        .with(String::from("proc-plugin"), panicking(&registered))
        .with(String::from("Not.A.Tag"), panicking(&unregistered));
    let kernel = start(set);
    kernel.barrier();
    shutdown(&kernel);

    let failed = records(capture, "plugin failed");
    assert!(
        failed
            .iter()
            .any(|line| line.contains(&scoped("proc-plugin", registered.get()))),
        "{failed:#?}"
    );
    let act = format!("\"act\":{},", unregistered.get());
    let line = failed
        .iter()
        .find(|line| line.contains(&act))
        .unwrap_or_else(|| panic!("no record for the unregistered ID: {failed:#?}"));
    assert!(!line.contains("\"plugin\":"), "{line}");
}
