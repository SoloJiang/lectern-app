//! `TestKernel`.

use std::sync::Arc;

use super::support::*;
use crate::testing::TestKernel;
use crate::{PluginError, service};

#[test]
fn injects_mocks_activates_alone_and_records_events() {
    let log = Log::default();
    let l = log.clone();
    let plugin = TestPlugin::new(manifest(
        vec![],
        vec![service::<dyn Greeter>()],
        after_first_frame(),
    ))
    .activate(move |ctx| {
        let greeter = ctx.service::<dyn Greeter>();
        let l = l.clone();
        Ok(Box::new(OnStarted(Some(Box::new(move |handle| {
            l.push(greeter.get().unwrap().hello());
            handle.emitter().emit(Pong(42));
        })))))
    });
    let kernel = TestKernel::new(plugin).mock::<dyn Greeter>(Arc::new(Hello("mocked")));
    assert_eq!(kernel.activate(), Ok(()));
    assert_eq!(log.snapshot(), vec!["mocked"]);
    assert_eq!(kernel.emitted::<Pong>(), vec![Pong(42)]);
    kernel.deactivate();
}

#[test]
fn reports_activation_failures() {
    let plugin = TestPlugin::new(manifest(vec![], vec![], after_first_frame()))
        .activate(|_| Err(PluginError::new("broken")));
    let kernel = TestKernel::new(plugin);
    assert_eq!(kernel.activate(), Err(PluginError::new("broken")));
}

#[test]
fn a_missing_mock_leaves_the_plugin_waiting() {
    let plugin = TestPlugin::new(manifest(
        vec![],
        vec![service::<dyn Greeter>()],
        after_first_frame(),
    ));
    let kernel = TestKernel::new(plugin);
    assert_eq!(kernel.activate(), Err(PluginError::DependencyUnavailable));
}
