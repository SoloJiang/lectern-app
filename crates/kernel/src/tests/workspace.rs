//! `Workspace` activation and workspace facts.

use std::sync::Arc;

use super::support::*;
use crate::{Activation, PluginSet, PluginState, WorkspaceFacts};

struct Files(Vec<&'static str>);

impl WorkspaceFacts for Files {
    fn contains(&self, pattern: &str) -> bool {
        self.0.contains(&pattern)
    }
}

struct Panics;

impl WorkspaceFacts for Panics {
    fn contains(&self, _: &str) -> bool {
        panic!("contains")
    }
}

fn java(log: &Log) -> TestPlugin {
    chain_plugin(
        "java",
        log,
        vec![],
        vec![],
        vec![Activation::Workspace(vec!["pom.xml".into()])],
    )
}

#[test]
fn facts_activate_matching_plugins_and_are_cleared_with_their_submitter() {
    let log = Log::default();
    let project = HandleSlot::default();
    let kernel = start(
        PluginSet::new()
            .with("project", handle_plugin(&project, after_first_frame()))
            .with("java", java(&log)),
    );
    assert_eq!(state(&kernel, "java"), PluginState::Inactive);
    project
        .get()
        .control()
        .workspace_changed(Arc::new(Files(vec!["README.md"])));
    kernel.barrier();
    assert_eq!(state(&kernel, "java"), PluginState::Inactive);
    project
        .get()
        .control()
        .workspace_changed(Arc::new(Files(vec!["pom.xml"])));
    kernel.barrier();
    assert_eq!(state(&kernel, "java"), PluginState::Active);

    // Re-enabling with the project unchanged uses the saved facts.
    kernel.set_enabled("java", false);
    kernel.set_enabled("java", true);
    kernel.barrier();
    assert_eq!(state(&kernel, "java"), PluginState::Active);

    // Once the submitter is gone its facts are cleared.
    kernel.set_enabled("project", false);
    kernel.set_enabled("java", false);
    kernel.set_enabled("java", true);
    kernel.barrier();
    assert_eq!(state(&kernel, "java"), PluginState::Inactive);
    shutdown(&kernel);
}

#[test]
fn a_panicking_contains_fails_the_submitter() {
    let log = Log::default();
    let project = HandleSlot::default();
    let kernel = start(
        PluginSet::new()
            .with("project", handle_plugin(&project, after_first_frame()))
            .with("java", java(&log)),
    );
    project.get().control().workspace_changed(Arc::new(Panics));
    kernel.barrier();
    assert!(matches!(state(&kernel, "project"), PluginState::Failed(_)));
    assert_eq!(state(&kernel, "java"), PluginState::Inactive);
    shutdown(&kernel);
}
