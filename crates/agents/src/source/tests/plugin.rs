//! 插件（`plugin_*`）：两个插件各提供一个 `SessionSource`，首次发现后发出 `SessionsChanged`；
//! `TestKernel` 注入模拟的 `Projects`；停用后不再监听。

use std::sync::Arc;
use std::time::Duration;

use lectern_cache::Cache;
use lectern_executor::Executor;
use lectern_fs_watch::Config;
use lectern_fs_watch::testing::{FakeBackend, manual};
use lectern_kernel::testing::TestKernel;
use lectern_kernel::{Plugin, PluginState};
use lectern_services::project::Projects;
use lectern_services::session::{Agent, SessionsChanged};

use super::support::*;
use super::{copy_tree, fixtures};
use crate::{AgentsDeps, plugins};

#[test]
fn plugin_provides_two_sources_and_announces_the_first_discovery() {
    let mut h = Harness::new();
    copy_tree(
        &fixtures().join("claude-code/2.1.302/projects"),
        &h.claude_root(),
    );
    h.start();
    let mut ids: Vec<(String, Agent)> = {
        let sources = [claude_source(), codex_source()].map(|id| h.source(&id));
        sources
            .iter()
            .map(|s| (s.id().0.to_string(), s.agent()))
            .collect()
    };
    ids.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        ids,
        [
            ("session-claude-code".to_owned(), Agent::ClaudeCode),
            ("session-codex".to_owned(), Agent::Codex)
        ]
    );
    for source in [claude_source(), codex_source()] {
        let seen = h.take_seen(&source);
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].event.source, source);
        assert_eq!(seen[0].event.generation, seen[0].list.generation);
    }
    // Nothing left to do: the kernel is quiescent after the first discovery.
    assert!(h.quiescent());
}

#[test]
fn plugin_activates_in_a_test_kernel_with_mocked_projects() {
    let h = Harness::new();
    copy_tree(
        &fixtures().join("claude-code/2.1.302/projects"),
        &h.claude_root(),
    );
    let backend = FakeBackend::new();
    let executor = Executor::manual();
    let (watcher, driver) = manual(backend.clone(), executor.clone(), Config::default());
    let deps = AgentsDeps {
        executor: executor.clone(),
        cache: Cache::new(lectern_cache::Config {
            root: h.cache_dir(),
            budgets: lectern_cache::Budgets::default(),
            ui_thread: None,
        }),
        watch: watcher,
        decoded_dir: h.base.join("decoded"),
        roots: h.roots.clone(),
        filter: h.filter.clone(),
    };
    let [(_, claude), _] = plugins(deps);
    assert!(
        claude
            .manifest()
            .requires
            .contains(&lectern_kernel::service::<dyn Projects>())
    );
    let kernel =
        TestKernel::new(claude).mock::<dyn Projects>(h.projects.clone() as Arc<dyn Projects>);
    kernel.activate().unwrap();
    assert!(
        kernel.emitted::<SessionsChanged>().is_empty(),
        "activation submits work, it does none"
    );
    driver.run_until_idle();
    let events = kernel.emitted::<SessionsChanged>();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].source, claude_source());
    assert_eq!(events[0].changed.len(), 4);
    assert!(backend.is_armed(&h.claude_root()));
    kernel.deactivate();
    driver.run_until_idle();
    assert!(!backend.is_armed(&h.claude_root()), "the watch is dropped");
    drop(kernel);
    executor.shutdown(Duration::from_secs(10));
}

#[test]
fn plugin_deactivation_stops_watching_and_revokes_the_source() {
    let mut h = Harness::new();
    h.start();
    assert!(h.run().backend.is_armed(&h.codex_root()));
    let kernel = h.run().kernel.clone();
    kernel.set_enabled("session-codex", false);
    kernel.barrier();
    h.settle();
    assert!(!h.run().backend.is_armed(&h.codex_root()));
    assert!(h.run().backend.is_armed(&h.claude_root()));
    let state = kernel
        .status()
        .into_iter()
        .find(|s| s.id == "session-codex")
        .unwrap()
        .state;
    assert_eq!(state, PluginState::Disabled);
}
