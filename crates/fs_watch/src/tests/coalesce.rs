//! Coalescing (`coalesce_*`), docs/design/fs_watch.md「合并」.

use std::time::Duration;

use insta::assert_snapshot;

use super::support::*;
use crate::{Coalesce, Config, Failure, Flags, Health};

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

#[test]
fn coalesce_events_within_quiet_form_one_batch() {
    let fx = Fixture::new();
    let o = fx.opened();
    let base = o.worktree.revision().n;
    fx.emit_root(&[("a", F)]);
    fx.settle();
    fx.driver.advance(ms(30));
    fx.emit_root(&[("b", F)]);
    fx.settle();
    fx.driver.advance(ms(30));
    assert!(o.sink.is_empty(), "the second event extended the window");
    fx.driver.advance(ms(20));
    assert_snapshot!(worktree_batches(&o.sink.take(), base), @r#"rev=2 paths=["a", "b"] meta=[]"#);
}

#[test]
fn coalesce_continuous_events_deliver_at_max_delay() {
    let fx = Fixture::new();
    let o = fx.opened();
    let mut delivered_at = Vec::new();
    for step in 0..30u64 {
        let name = format!("f{step}");
        fx.emit_root(&[(name.as_str(), F)]);
        fx.settle();
        fx.driver.advance(ms(40));
        if !o.sink.take().is_empty() {
            delivered_at.push((step + 1) * 40);
        }
    }
    // Every event comes within `quiet`; batches go out every `max_delay`.
    assert_eq!(delivered_at, vec![520, 1040]);
}

#[test]
fn coalesce_two_handles_deliver_on_their_own() {
    let fx = Fixture::new();
    let o = fx.opened();
    let (fast, fast_sink) = fx.worktree_with(
        &o.repo,
        Coalesce {
            quiet: ms(10),
            max_delay: ms(100),
        },
    );
    fx.settle();
    fast_sink.take();
    fx.emit_root(&[("a", F)]);
    fx.settle();
    fx.driver.advance(ms(10));
    assert_eq!(fast_sink.take().len(), 1);
    assert!(o.sink.is_empty());
    fx.driver.advance(ms(40));
    assert_eq!(o.sink.take().len(), 1);
    drop(fast);
}

#[test]
fn coalesce_too_many_paths_becomes_a_rescan() {
    let fx = Fixture::with_config(Config {
        max_batch_paths: 10,
        ..Config::default()
    });
    let o = fx.opened();
    let base = o.worktree.revision().n;
    let names: Vec<String> = (0..11).map(|i| format!("f{i}")).collect();
    let events: Vec<(&str, Flags)> = names.iter().map(|name| (name.as_str(), F)).collect();
    fx.emit_root(&events);
    fx.settle();
    fx.wait();
    assert_snapshot!(worktree_batches(&o.sink.take(), base), @"rev=1 paths=[] meta=[] rescan");

    // Directory watches have the same limit.
    let watched = fx.base.join("sessions");
    std::fs::create_dir_all(&watched).unwrap();
    let (_dir, dir_sink) = fx.dir(&watched);
    fx.settle();
    dir_sink.take();
    fx.backend.emit(&watched, &events);
    fx.settle();
    fx.wait();
    assert_snapshot!(dir_batches(&dir_sink.take(), &fx.base), @"paths=[] rescan");
}

#[test]
fn coalesce_health_change_is_delivered_at_once() {
    let fx = Fixture::new();
    let o = fx.opened();
    let base = o.worktree.revision().n;
    fx.emit_root(&[("a", F)]);
    fx.settle();
    assert!(o.sink.is_empty());
    fx.emit_root(&[("", D | Flags::ROOT_CHANGED)]);
    fx.settle();
    // No time passed: the open batch, then the new health.
    let batches = o.sink.take();
    assert_eq!(batches[1].0.health, Some(Health::Failed(Failure::RootGone)));
    assert_snapshot!(worktree_batches(&batches, base), @r#"
    rev=2 paths=["a"] meta=[]
    rev=2 paths=[] meta=[] health=Failed(RootGone)
    "#);
}

#[test]
fn coalesce_no_timer_without_waiting_batches() {
    let fx = Fixture::new();
    let o = fx.opened();
    assert!(!fx.driver.shared().has_timer());
    fx.emit_root(&[("a", F)]);
    fx.settle();
    assert!(fx.driver.shared().has_timer());
    fx.wait();
    assert!(!fx.driver.shared().has_timer());
    // Events that are dropped open no batch and set no timer.
    fx.emit_common(&[("objects/ab/cd", F)]);
    fx.settle();
    assert!(!fx.driver.shared().has_timer());
    assert_eq!(o.sink.take().len(), 1);
}

#[test]
fn coalesce_repo_batches_use_their_own_window() {
    let fx = Fixture::new();
    let o = fx.opened();
    fx.emit_common(&[("refs/heads/a", F)]);
    fx.settle();
    fx.driver.advance(ms(20));
    assert_snapshot!(repo_batches(&o.repo_sink.take()), @r#"worktrees=[] ["refs"]"#);
    assert!(o.sink.is_empty(), "the worktree window is longer");
    fx.driver.advance(ms(30));
    assert_eq!(o.sink.take().len(), 1);
}
