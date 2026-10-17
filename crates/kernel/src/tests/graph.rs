//! `PluginSet::validate` and the startup graph check.

use super::support::*;
use crate::{Activation, GraphError, PluginSet, PluginState, Service, service};

trait Clash: Send + Sync {}
impl Service for dyn Clash {
    const ID: &'static str = "greeter";
    const MULTIPLE: bool = true;
}

fn plugin(provides: Vec<crate::ServiceKey>, requires: Vec<crate::ServiceKey>) -> TestPlugin {
    TestPlugin::new(manifest(provides, requires, after_first_frame()))
}

#[test]
fn reports_each_kind_of_graph_error() {
    let errors = PluginSet::new()
        .with("missing", plugin(vec![], vec![service::<dyn SvcA>()]))
        .with("dup", plugin(vec![], vec![]))
        .with("dup", plugin(vec![], vec![]))
        .with("greeter-1", plugin(vec![service::<dyn Greeter>()], vec![]))
        .with("greeter-2", plugin(vec![service::<dyn Greeter>()], vec![]))
        .with("clash", plugin(vec![], vec![service::<dyn Clash>()]))
        .with("idle", TestPlugin::new(manifest(vec![], vec![], vec![])))
        .with(
            "cycle-b",
            plugin(vec![service::<dyn SvcB>()], vec![service::<dyn SvcC>()]),
        )
        .with(
            "cycle-c",
            plugin(vec![service::<dyn SvcC>()], vec![service::<dyn SvcB>()]),
        )
        .with(
            "cmd-1",
            TestPlugin::new(manifest(
                vec![],
                vec![],
                vec![Activation::Command("go".into())],
            )),
        )
        .with(
            "cmd-2",
            TestPlugin::new(manifest(
                vec![],
                vec![],
                vec![Activation::Command("go".into())],
            )),
        )
        .validate()
        .unwrap_err();
    insta::assert_debug_snapshot!(errors);
}

#[test]
fn a_valid_graph_passes() {
    let log = Log::default();
    let set = PluginSet::new()
        .with(
            "a",
            chain_plugin(
                "a",
                &log,
                vec![],
                vec![service::<dyn SvcB>()],
                after_first_frame(),
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
            "c",
            chain_plugin(
                "c",
                &log,
                vec![service::<dyn SvcC>()],
                vec![],
                after_first_frame(),
            ),
        );
    assert_eq!(set.validate(), Ok(()));
}

#[test]
fn a_panicking_manifest_fails_only_that_plugin() {
    struct Broken;
    impl crate::Plugin for Broken {
        fn manifest(&self) -> crate::Manifest {
            panic!("no manifest")
        }
        fn activate(
            &self,
            _: &mut crate::Context<'_>,
        ) -> Result<Box<dyn crate::Active>, crate::PluginError> {
            unreachable!()
        }
    }
    let log = Log::default();
    let set = PluginSet::new().with("broken", Broken).with(
        "fine",
        chain_plugin("fine", &log, vec![], vec![], after_first_frame()),
    );
    let errors = set.validate().unwrap_err();
    assert!(
        matches!(&errors[0], GraphError::ManifestPanicked { plugin, .. } if plugin == "broken")
    );

    let kernel = start(set);
    assert!(matches!(state(&kernel, "broken"), PluginState::Failed(_)));
    assert_eq!(state(&kernel, "fine"), PluginState::Active);
    shutdown(&kernel);
}

#[test]
fn startup_fails_plugins_in_a_cycle_and_keeps_the_rest() {
    let log = Log::default();
    let set = PluginSet::new()
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
            "c",
            chain_plugin(
                "c",
                &log,
                vec![service::<dyn SvcC>()],
                vec![service::<dyn SvcB>()],
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
                after_first_frame(),
            ),
        );
    let kernel = start(set);
    assert!(matches!(state(&kernel, "b"), PluginState::Failed(_)));
    assert!(matches!(state(&kernel, "c"), PluginState::Failed(_)));
    assert_eq!(state(&kernel, "a"), PluginState::Active);
    shutdown(&kernel);
}
