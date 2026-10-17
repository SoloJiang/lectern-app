//! `PluginSet::prepare`, `PreparedSet` and `Kernel::start_prepared`
//! (`docs/design/kernel.md`, 插件集合的闭包与裁剪).

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use super::support::*;
use crate::{
    Activation, Active, Context, GraphError, Kernel, Manifest, Name, Plugin, PluginError,
    PluginSet, PluginState, PreparedSet, ServiceKey, service,
};

fn plugin(provides: Vec<ServiceKey>, requires: Vec<ServiceKey>) -> TestPlugin {
    TestPlugin::new(manifest(provides, requires, after_first_frame()))
}

/// A plugin providing marker services (and `Source` under its own name),
/// active after the first frame.
fn provider(
    name: &'static str,
    provides: Vec<ServiceKey>,
    requires: Vec<ServiceKey>,
) -> TestPlugin {
    let provided = provides.clone();
    TestPlugin::new(manifest(provides, requires, after_first_frame())).activate(move |ctx| {
        for key in &provided {
            if *key == service::<dyn Source>() {
                ctx.provide::<dyn Source>(Arc::new(Named(name.to_owned())))?;
            } else {
                provide_marker(ctx, *key)?;
            }
        }
        Ok(Box::new(()))
    })
}

fn names(ids: &[&str]) -> Vec<Name> {
    ids.iter().map(|id| Name::Owned((*id).to_owned())).collect()
}

/// `app` → `b` → `c`, plus an unrelated plugin.
fn chain() -> PluginSet {
    PluginSet::new()
        .with("unrelated", plugin(vec![], vec![]))
        .with("app", plugin(vec![], vec![service::<dyn SvcB>()]))
        .with(
            "b",
            plugin(vec![service::<dyn SvcB>()], vec![service::<dyn SvcC>()]),
        )
        .with("c", plugin(vec![service::<dyn SvcC>()], vec![]))
}

/// `consumer` needs the `MULTIPLE` service `source`, provided by `s1` and
/// `s2`; `s2` needs `c`. `greeter` and `other` are unrelated.
fn multiple() -> PluginSet {
    PluginSet::new()
        .with("consumer", plugin(vec![], vec![service::<dyn Source>()]))
        .with("greeter", plugin(vec![service::<dyn Greeter>()], vec![]))
        .with("s1", plugin(vec![service::<dyn Source>()], vec![]))
        .with(
            "s2",
            plugin(vec![service::<dyn Source>()], vec![service::<dyn SvcC>()]),
        )
        .with("c", plugin(vec![service::<dyn SvcC>()], vec![]))
        .with("other", plugin(vec![], vec![service::<dyn Greeter>()]))
}

#[test]
fn set_closure_follows_multi_level_dependencies() {
    let set = chain().prepare();
    assert_eq!(set.dependency_closure("app"), Ok(names(&["b", "c"])));
    assert_eq!(set.dependency_closure("b"), Ok(names(&["c"])));
    assert_eq!(set.dependency_closure("c"), Ok(vec![]));
    assert_eq!(set.dependency_closure("unrelated"), Ok(vec![]));
}

#[test]
fn set_closure_includes_every_provider_of_a_multiple_service() {
    let set = multiple().prepare();
    assert_eq!(
        set.dependency_closure("consumer"),
        Ok(names(&["s1", "s2", "c"]))
    );
    assert_eq!(set.dependency_closure("other"), Ok(names(&["greeter"])));
    // A provider of the `MULTIPLE` service is not pulled in by the others.
    assert_eq!(set.dependency_closure("s2"), Ok(names(&["c"])));
}

#[test]
fn set_closure_reports_a_missing_provider() {
    let set = PluginSet::new()
        .with("app", plugin(vec![], vec![service::<dyn SvcB>()]))
        .with(
            "b",
            plugin(
                vec![service::<dyn SvcB>()],
                vec![service::<dyn SvcC>(), service::<dyn SvcA>()],
            ),
        )
        .with("a", plugin(vec![service::<dyn SvcA>()], vec![]))
        .with("unrelated", plugin(vec![], vec![service::<dyn Greeter>()]))
        .prepare();
    assert_eq!(
        set.dependency_closure("app"),
        Err(vec![GraphError::MissingProvider {
            plugin: "b".into(),
            service: "svc-c",
        }])
    );
    // A broken plugin outside the closure does not matter.
    assert_eq!(set.dependency_closure("a"), Ok(vec![]));
    assert_eq!(
        set.dependency_closure("nobody"),
        Err(vec![GraphError::UnknownPlugin("nobody".into())])
    );
}

#[test]
fn set_closure_reports_a_provider_whose_manifest_panicked() {
    let set = PluginSet::new()
        .with("app", plugin(vec![], vec![service::<dyn SvcB>()]))
        .with("b", Panicking)
        .prepare();
    // The broken plugin provides nothing, so the requirement is unmet.
    assert_eq!(
        set.dependency_closure("app"),
        Err(vec![GraphError::MissingProvider {
            plugin: "app".into(),
            service: "svc-b",
        }])
    );
    assert_eq!(
        set.dependency_closure("b"),
        Err(vec![GraphError::ManifestPanicked {
            plugin: "b".into(),
            message: "no manifest".to_owned(),
        }])
    );
}

#[test]
fn set_closure_reports_several_providers_of_a_single_provider_service() {
    let set = PluginSet::new()
        .with("app", plugin(vec![], vec![service::<dyn Greeter>()]))
        .with("g1", plugin(vec![service::<dyn Greeter>()], vec![]))
        .with("g2", plugin(vec![service::<dyn Greeter>()], vec![]))
        .prepare();
    assert_eq!(
        set.dependency_closure("app"),
        Err(vec![GraphError::MultipleProviders {
            service: "greeter",
            plugins: names(&["g1", "g2"]),
        }])
    );
}

/// Keeps `id`'s closure, and `id` too when `with_self`.
fn pruned(set: PreparedSet, id: &str, with_self: bool) -> PreparedSet {
    let closure = set.dependency_closure(id).expect("closure");
    set.retain(|plugin| (with_self && plugin == id) || closure.iter().any(|c| c == plugin))
}

#[test]
fn set_closure_validates_after_retain() {
    for build in [chain as fn() -> PluginSet, multiple] {
        assert_eq!(build().validate(), Ok(()));
        let ids: Vec<Name> = build()
            .prepare()
            .manifests()
            .map(|(id, _)| id.clone())
            .collect();
        for id in &ids {
            for with_self in [false, true] {
                let kept = pruned(build().prepare(), id, with_self);
                assert_eq!(kept.validate(), Ok(()), "{id}, with itself: {with_self}");
                let mut expected = build().prepare().dependency_closure(id).unwrap();
                if with_self {
                    expected.push(id.clone());
                }
                let mut got: Vec<Name> = kept.manifests().map(|(id, _)| id.clone()).collect();
                got.sort();
                expected.sort();
                assert_eq!(got, expected);
            }
        }
    }
    // Without the closure the dependent's requirement is unmet.
    let broken = chain().prepare().retain(|id| id == "app");
    assert_eq!(
        broken.validate(),
        Err(vec![GraphError::MissingProvider {
            plugin: "app".into(),
            service: "svc-b",
        }])
    );
}

#[test]
fn set_closure_with_a_provider_that_depends_back_on_the_target() {
    // `consumer` needs `source` from `s1` or `s2`; `s2` needs `consumer`'s
    // service. The full set is valid (`s1`, `consumer`, `s2`).
    let build = || {
        PluginSet::new()
            .with(
                "consumer",
                plugin(vec![service::<dyn SvcA>()], vec![service::<dyn Source>()]),
            )
            .with("s1", plugin(vec![service::<dyn Source>()], vec![]))
            .with(
                "s2",
                plugin(vec![service::<dyn Source>()], vec![service::<dyn SvcA>()]),
            )
    };
    assert_eq!(build().validate(), Ok(()));
    let set = build().prepare();
    assert_eq!(set.dependency_closure("consumer"), Ok(names(&["s1", "s2"])));
    assert_eq!(
        pruned(build().prepare(), "consumer", true).validate(),
        Ok(())
    );
    // The known limitation: the closure alone lacks `consumer`'s service.
    assert_eq!(
        pruned(build().prepare(), "consumer", false).validate(),
        Err(vec![GraphError::MissingProvider {
            plugin: "s2".into(),
            service: "svc-a",
        }])
    );
}

/// Counts `manifest()` calls and takes a little time in each.
struct Counted {
    inner: TestPlugin,
    calls: Arc<AtomicUsize>,
}

impl Plugin for Counted {
    fn manifest(&self) -> Manifest {
        self.calls.fetch_add(1, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(2));
        self.inner.manifest()
    }

    fn activate(&self, ctx: &mut Context<'_>) -> Result<Box<dyn Active>, PluginError> {
        Plugin::activate(&self.inner, ctx)
    }
}

#[derive(Default)]
struct Calls(HashMap<&'static str, Arc<AtomicUsize>>);

impl Calls {
    fn wrap(&mut self, id: &'static str, inner: TestPlugin) -> Counted {
        let calls = Arc::new(AtomicUsize::new(0));
        self.0.insert(id, calls.clone());
        Counted { inner, calls }
    }

    fn get(&self) -> Vec<(&'static str, usize)> {
        let mut calls: Vec<_> = self
            .0
            .iter()
            .map(|(id, n)| (*id, n.load(Ordering::SeqCst)))
            .collect();
        calls.sort();
        calls
    }
}

#[test]
fn set_prepare_calls_each_manifest_once_and_reports_its_time() {
    let mut calls = Calls::default();
    let set = PluginSet::new()
        .with("unrelated", calls.wrap("unrelated", plugin(vec![], vec![])))
        .with(
            "app",
            calls.wrap(
                "app",
                provider("app", vec![], vec![service::<dyn Source>()]),
            ),
        )
        .with(
            "s1",
            calls.wrap(
                "s1",
                provider(
                    "s1",
                    vec![service::<dyn Source>()],
                    vec![service::<dyn SvcC>()],
                ),
            ),
        )
        .with(
            "s2",
            calls.wrap("s2", provider("s2", vec![service::<dyn Source>()], vec![])),
        )
        .with(
            "c",
            calls.wrap("c", provider("c", vec![service::<dyn SvcC>()], vec![])),
        );

    let prepared = set.prepare();
    let closure = prepared.dependency_closure("app").unwrap();
    assert_eq!(closure, names(&["s1", "s2", "c"]));
    let prepared = prepared
        .retain(|id| id == "app" || closure.iter().any(|c| c == id))
        .with("extra", calls.wrap("extra", plugin(vec![], vec![])));
    assert_eq!(prepared.validate(), Ok(()));
    assert_eq!(prepared.manifests().count(), 5);
    let recorded: Vec<(Name, Duration)> = prepared
        .plugins
        .iter()
        .map(|p| (p.id.clone(), p.time))
        .collect();

    let kernel = Kernel::start_prepared(prepared);
    kernel.barrier();
    assert_eq!(
        calls.get(),
        vec![
            ("app", 1),
            ("c", 1),
            ("extra", 1),
            ("s1", 1),
            ("s2", 1),
            ("unrelated", 1),
        ]
    );
    let status = kernel.status();
    assert_eq!(
        status.iter().map(|s| s.id.clone()).collect::<Vec<_>>(),
        names(&["app", "s1", "s2", "c", "extra"])
    );
    for s in &status {
        assert_eq!(s.state, PluginState::Active, "{}", s.id);
        let (_, time) = recorded.iter().find(|(id, _)| *id == s.id).unwrap();
        assert_eq!(s.manifest_time, Some(*time), "{}", s.id);
        assert!(*time >= Duration::from_millis(2), "{}", s.id);
    }
    shutdown(&kernel);
    // Shutting down does not read a manifest either.
    assert!(calls.get().iter().all(|(_, n)| *n == 1));
}

/// A plugin whose `manifest()` panics.
struct Panicking;

impl Plugin for Panicking {
    fn manifest(&self) -> Manifest {
        panic!("no manifest")
    }

    fn activate(&self, _: &mut Context<'_>) -> Result<Box<dyn Active>, PluginError> {
        unreachable!("never activated")
    }
}

#[test]
fn set_prepare_a_panicking_manifest_fails_the_plugin_as_start_does() {
    let build = || {
        PluginSet::new()
            .with("broken", Panicking)
            .with("fine", plugin(vec![], vec![]))
    };

    let started = start(build());
    let expected = states(&started);
    shutdown(&started);

    let prepared = build().prepare().with("late", Panicking);
    assert_eq!(
        prepared
            .manifests()
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>(),
        names(&["fine"])
    );
    assert_eq!(
        prepared.validate(),
        Err(vec![
            GraphError::ManifestPanicked {
                plugin: "broken".into(),
                message: "no manifest".to_owned(),
            },
            GraphError::ManifestPanicked {
                plugin: "late".into(),
                message: "no manifest".to_owned(),
            },
        ])
    );
    let kernel = Kernel::start_prepared(prepared);
    kernel.barrier();
    let mut got = states(&kernel);
    assert_eq!(got.pop(), Some(("late".into(), expected[0].1.clone())));
    assert_eq!(got, expected);
    assert_eq!(
        expected[0].1,
        PluginState::Failed(PluginError::Panicked(
            "manifest panicked: no manifest".to_owned()
        ))
    );
    assert_eq!(expected[1].1, PluginState::Active);
    assert!(kernel.status().iter().all(|s| s.manifest_time.is_some()));
    shutdown(&kernel);
}

#[test]
fn set_prepare_keeps_activation_conditions_for_the_caller() {
    let set = PluginSet::new()
        .with(
            "cmd",
            TestPlugin::new(manifest(
                vec![],
                vec![],
                vec![Activation::Command("go".into())],
            )),
        )
        .with(
            "manual",
            TestPlugin::new(manifest(vec![], vec![], vec![Activation::Manual])),
        )
        .prepare();
    let activations: Vec<(Name, Vec<Activation>)> = set
        .manifests()
        .map(|(id, m)| (id.clone(), m.activation.clone()))
        .collect();
    assert_eq!(
        activations,
        vec![
            ("cmd".into(), vec![Activation::Command("go".into())]),
            ("manual".into(), vec![Activation::Manual]),
        ]
    );
    // The prepared kernel serves the conditions like `Kernel::start`.
    let kernel = Kernel::start_prepared(set);
    assert_eq!(kernel.lazy_commands(), vec![Name::from("go")]);
    kernel.activate("manual");
    kernel.barrier();
    assert_eq!(state(&kernel, "manual"), PluginState::Active);
    assert_eq!(state(&kernel, "cmd"), PluginState::Inactive);
    shutdown(&kernel);
}
