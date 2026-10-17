//! Activation by command.

use std::sync::mpsc;
use std::time::Duration;

use super::support::*;
use crate::{
    Activation, ActivationToken, PluginError, PluginSet, PluginState, TokenState, service,
};

fn command(name: &str) -> Vec<Activation> {
    vec![Activation::Command(name.to_owned().into())]
}

fn run_command(kernel: &crate::Kernel, name: &str) -> Result<ActivationToken, PluginError> {
    let (done, wait) = mpsc::channel();
    kernel.command(name, move |result| {
        let _ = done.send(result);
    });
    wait.recv_timeout(Duration::from_secs(5))
        .expect("done called")
}

#[test]
fn done_runs_once_after_the_activation_is_live() {
    let log = Log::default();
    let kernel = start(PluginSet::new().with(
        "term",
        chain_plugin("term", &log, vec![], vec![], command("terminal.open")),
    ));
    assert_eq!(kernel.lazy_commands(), vec!["terminal.open"]);
    assert_eq!(state(&kernel, "term"), PluginState::Inactive);
    let token = run_command(&kernel, "terminal.open").unwrap();
    assert_eq!(token.state(), TokenState::Live);
    // Already active: immediate, same activation, no second activate.
    let again = run_command(&kernel, "terminal.open").unwrap();
    assert!(again.same(&token));
    assert_eq!(log.snapshot(), vec!["activate term", "started term"]);
    // Disabled and re-enabled: the command activates it again.
    kernel.set_enabled("term", false);
    kernel.set_enabled("term", true);
    kernel.barrier();
    assert_eq!(state(&kernel, "term"), PluginState::Inactive);
    assert!(run_command(&kernel, "terminal.open").is_ok());
    shutdown(&kernel);
}

#[test]
fn failures_reach_done() {
    let kernel = start(
        PluginSet::new()
            .with(
                "panics-in-started",
                TestPlugin::new(manifest(vec![], vec![], command("a")))
                    .activate(|_| Ok(Box::new(OnStarted(Some(Box::new(|_| panic!("started"))))))),
            )
            .with(
                "errors",
                TestPlugin::new(manifest(vec![], vec![], command("b")))
                    .activate(|_| Err(PluginError::new("nope"))),
            ),
    );
    assert!(matches!(
        run_command(&kernel, "a"),
        Err(PluginError::Panicked(_))
    ));
    assert_eq!(
        run_command(&kernel, "b").err(),
        Some(PluginError::new("nope"))
    );
    assert!(matches!(
        run_command(&kernel, "missing"),
        Err(PluginError::UnknownCommand(_))
    ));
    shutdown(&kernel);
}

#[test]
fn unavailable_dependencies_are_reported() {
    let log = Log::default();
    let kernel = start(
        PluginSet::new()
            // c needs a workspace match that never comes; b needs c.
            .with(
                "c",
                chain_plugin(
                    "c",
                    &log,
                    vec![service::<dyn SvcC>()],
                    vec![],
                    vec![Activation::Workspace(vec!["*.never".into()])],
                ),
            )
            .with(
                "b",
                chain_plugin(
                    "b",
                    &log,
                    vec![service::<dyn SvcB>()],
                    vec![service::<dyn SvcC>()],
                    after_first_frame(),
                ),
            )
            .with(
                "deep",
                chain_plugin(
                    "deep",
                    &log,
                    vec![],
                    vec![service::<dyn SvcB>()],
                    command("deep"),
                ),
            )
            .with(
                "direct",
                chain_plugin(
                    "direct",
                    &log,
                    vec![],
                    vec![service::<dyn SvcC>()],
                    command("direct"),
                ),
            ),
    );
    assert_eq!(
        run_command(&kernel, "deep").err(),
        Some(PluginError::DependencyUnavailable)
    );
    assert_eq!(
        run_command(&kernel, "direct").err(),
        Some(PluginError::DependencyUnavailable)
    );
    shutdown(&kernel);
}

#[test]
fn disabled_plugins_and_unrequested_dependencies_are_reported() {
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
                    vec![Activation::Manual],
                ),
            )
            .with(
                "cmd",
                chain_plugin(
                    "cmd",
                    &log,
                    vec![],
                    vec![service::<dyn SvcA>()],
                    command("go"),
                ),
            ),
    );
    // a is manual and not requested: a command does not satisfy it.
    assert_eq!(
        run_command(&kernel, "go").err(),
        Some(PluginError::DependencyUnavailable)
    );
    kernel.activate("a");
    kernel.set_enabled("cmd", false);
    kernel.barrier();
    assert_eq!(
        run_command(&kernel, "go").err(),
        Some(PluginError::Disabled)
    );
    kernel.set_enabled("cmd", true);
    assert!(run_command(&kernel, "go").is_ok());
    shutdown(&kernel);
}

#[test]
fn a_panicking_done_does_not_stop_the_kernel_thread() {
    let log = Log::default();
    let kernel =
        start(PluginSet::new().with("p", chain_plugin("p", &log, vec![], vec![], command("go"))));
    kernel.command("go", |_| panic!("done"));
    kernel.barrier();
    assert!(run_command(&kernel, "go").is_ok());
    shutdown(&kernel);
}

#[test]
fn a_dead_requester_never_sees_done() {
    let log = Log::default();
    let slot = HandleSlot::default();
    let kernel = start(
        PluginSet::new()
            .with("workspace", handle_plugin(&slot, after_first_frame()))
            .with(
                "slow",
                chain_plugin("slow", &log, vec![], vec![], command("go")),
            ),
    );
    let control = slot.get().control();
    let calls = Log::default();
    let c = calls.clone();
    slot.get().fail(PluginError::new("workspace died"));
    control.command("go", move |_| c.push("called"));
    kernel.barrier();
    assert!(calls.snapshot().is_empty());
    assert_eq!(state(&kernel, "slow"), PluginState::Inactive);
    shutdown(&kernel);
}

#[test]
fn a_control_handle_is_ignored_after_its_activation_ends() {
    let log = Log::default();
    let slot = HandleSlot::default();
    let kernel = start(
        PluginSet::new()
            .with("driver", handle_plugin(&slot, after_first_frame()))
            .with(
                "m",
                chain_plugin("m", &log, vec![], vec![], vec![Activation::Manual]),
            ),
    );
    let control = slot.get().control();
    kernel.set_enabled("driver", false);
    kernel.barrier();
    control.activate("m");
    kernel.barrier();
    assert_eq!(state(&kernel, "m"), PluginState::Inactive);
    shutdown(&kernel);
}
