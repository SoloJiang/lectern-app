//! `UsageLink` on its own (docs/design/worktrees.md「本 crate 的公开项」): the
//! usage plugin runs in a real kernel, the main plugin is not there, and the
//! test plays its part.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use lectern_services::usage::UsageQuery;

use crate::testing::{Rig, git_key, mr_ref, worktree_query, wt_key};

fn query(name: &str) -> UsageQuery {
    worktree_query(&git_key("app"), &wt_key("app", name))
}

fn wanted(names: &[&str]) -> Arc<[UsageQuery]> {
    names.iter().map(|name| query(name)).collect()
}

/// Counts the main plugin's callback.
fn counter(rig: &Rig) -> Arc<AtomicUsize> {
    let count = Arc::new(AtomicUsize::new(0));
    let calls = count.clone();
    rig.link.set_on_change(Arc::new(move || {
        calls.fetch_add(1, Ordering::SeqCst);
    }));
    count
}

fn calls(count: &AtomicUsize) -> usize {
    count.load(Ordering::SeqCst)
}

#[test]
fn reconcile_takes_the_wanted_and_drops_the_rest() {
    let rig = Rig::new();
    rig.activate_usage();
    rig.run();
    let changes = counter(&rig);

    rig.link.set_wanted(wanted(&["a", "b"]));
    assert_eq!(rig.usage.watched(), [query("a"), query("b")]);
    assert_eq!(rig.link.watching(), 2);
    assert_eq!(calls(&changes), 1);

    // `a` stays, `b` goes, `c` comes.
    rig.link.set_wanted(wanted(&["a", "c"]));
    assert_eq!(rig.usage.watched(), [query("a"), query("b"), query("c")]);
    assert_eq!(rig.usage.released(), 1);
    assert_eq!(rig.link.watching(), 2);
    assert_eq!(calls(&changes), 2);

    rig.link.set_wanted(Arc::from(Vec::new()));
    assert_eq!(rig.usage.released(), 3);
    assert_eq!(rig.link.watching(), 0);
    assert_eq!(calls(&changes), 3);
}

#[test]
fn the_same_set_reports_nothing() {
    let rig = Rig::new();
    rig.activate_usage();
    rig.run();
    let changes = counter(&rig);
    rig.link.set_wanted(wanted(&["a", "b"]));
    let generation = rig.link.generation();
    assert_eq!(calls(&changes), 1);

    rig.link.set_wanted(wanted(&["a", "b"]));
    rig.link.set_wanted(wanted(&["b", "a"]));
    rig.link.reconcile();
    assert_eq!(calls(&changes), 1, "no interest was taken or dropped");
    assert_eq!(rig.link.generation(), generation);
    assert_eq!(rig.usage.watched().len(), 2, "and none was taken twice");
}

#[test]
fn wanted_before_the_usage_plugin_waits_for_it() {
    let rig = Rig::new();
    let changes = counter(&rig);
    rig.link.set_wanted(wanted(&["a"]));
    assert!(rig.usage.watched().is_empty(), "no `dyn Usage` yet");
    assert_eq!(calls(&changes), 0, "nothing to report");

    rig.activate_usage();
    assert!(
        rig.usage.watched().is_empty(),
        "`started` only records: the token is still pending"
    );
    rig.run();
    assert_eq!(rig.usage.watched(), [query("a")]);
    assert_eq!(calls(&changes), 1, "the task reconciled once");
}

#[test]
fn clearing_the_usage_drops_the_interests_and_reports_once() {
    let rig = Rig::new();
    rig.activate_usage();
    rig.run();
    rig.link.set_wanted(wanted(&["a", "b"]));
    let changes = counter(&rig);

    rig.deactivate_usage();
    assert_eq!(rig.usage.released(), 2);
    assert_eq!(rig.link.watching(), 0);
    assert_eq!(calls(&changes), 1, "the main plugin composes without usage");
    assert!(rig.link.usage().is_none());

    // With nothing wanted, a usage plugin that vanishes still reports: the
    // snapshot had tokens (or a pending mark) and must lose them.
    let rig = Rig::new();
    rig.activate_usage();
    rig.run();
    let changes = counter(&rig);
    rig.link.set_wanted(Arc::from(Vec::new()));
    assert_eq!(calls(&changes), 0);
    rig.deactivate_usage();
    assert_eq!(calls(&changes), 1);
}

#[test]
fn changed_reports_only_the_wanted_queries() {
    let rig = Rig::new();
    rig.activate_usage();
    rig.run();
    let changes = counter(&rig);
    rig.link.set_wanted(wanted(&["a"]));
    let before = calls(&changes);
    let generation = rig.link.generation();

    rig.link.changed(&query("b"));
    rig.link.changed(&UsageQuery::Mr { mr: mr_ref(3) });
    assert_eq!(calls(&changes), before);
    assert_eq!(rig.link.generation(), generation);

    rig.link.changed(&query("a"));
    assert_eq!(calls(&changes), before + 1);
    assert_eq!(rig.link.generation(), generation + 1);
}

#[test]
fn the_usage_is_only_handed_out_to_a_live_token() {
    let rig = Rig::new();
    assert!(rig.link.usage().is_none(), "no plugin");
    rig.activate_usage();
    assert!(rig.link.usage().is_some(), "live once the barrier passed");
}
