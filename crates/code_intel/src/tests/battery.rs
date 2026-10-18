//! `battery_*`: on battery tiers 4 and 5 wait (`Deferred`), tiers 1–3 run;
//! on AC they go on; on battery a query (`demand`) lets them go on until the
//! power source changes.

use lectern_services::code_intel::TierState;

use super::support::{Harness, Options, packages, ts};

fn repo(h: &Harness) {
    h.git.add(
        "main",
        &[
            ("app/a.ts", &ts("a")),
            ("app/b.ts", &ts("b")),
            ("lib/c.ts", &ts("c")),
            ("d.ts", &ts("d")),
        ],
    );
    h.git.commit("main");
    h.git.add_worktree("feature");
    h.git.add("feature", &[("e.ts", &ts("e"))]);
}

fn coverage(h: &Harness) -> [TierState; 3] {
    let c = h.coverage();
    [c.subproject, c.worktree, c.other_worktrees]
}

#[test]
fn battery_defers_tiers_4_and_5() {
    let h = Harness::with(Options {
        battery: true,
        ..Options::default()
    });
    repo(&h);
    h.open_with("main", packages(&["app", "lib"]));
    // Without a focus no sub-project is current: tier 3 is empty and the
    // rest waits for AC.
    assert_eq!(h.extractions(), 0);
    assert_eq!(h.coverage().worktree, TierState::Deferred);
    h.current("app/a.ts");
    h.settle();
    assert_eq!(
        coverage(&h),
        [TierState::Done, TierState::Deferred, TierState::Deferred]
    );
    for path in ["app/a.ts", "app/b.ts"] {
        assert!(h.hinted(path), "{path}");
    }
    for path in ["lib/c.ts", "d.ts"] {
        assert!(!h.hinted(path), "{path} waits for AC");
    }
    assert!(!h.stored(h.git.oid(&ts("e")), "ts"));
    // On AC tiers 4 and 5 go on.
    h.power.set(false);
    h.settle();
    assert_eq!(coverage(&h), [TierState::Done; 3]);
    assert!(h.hinted("d.ts"));
    assert!(h.stored(h.git.oid(&ts("e")), "ts"));
}

#[test]
fn battery_demand_lets_tiers_4_and_5_go_on() {
    let h = Harness::with(Options {
        battery: true,
        ..Options::default()
    });
    repo(&h);
    h.open_with("main", packages(&["app", "lib"]));
    h.current("app/a.ts");
    h.settle();
    assert_eq!(coverage(&h)[1], TierState::Deferred);
    // A query on battery.
    h.indexer.demand(&h.key());
    h.settle();
    assert_eq!(coverage(&h), [TierState::Done; 3]);
    // A change of the power source ends the demand: new work waits again.
    h.power.set(false);
    h.power.set(true);
    h.git.write("main", "lib/new.ts", &ts("new"));
    h.emit(&[("lib/new.ts", super::support::CREATED)]);
    h.settle();
    assert_eq!(coverage(&h)[1], TierState::Deferred);
    assert!(!h.hinted("lib/new.ts"));
}

#[test]
fn battery_does_not_hold_back_tiers_1_and_2() {
    let h = Harness::with(Options {
        battery: true,
        ..Options::default()
    });
    repo(&h);
    h.open_with("main", packages(&["app", "lib"]));
    h.current("app/a.ts");
    h.set_focus(lectern_services::code_intel::Focus::Review(
        std::sync::Arc::from(vec!["d.ts".into()]),
    ));
    h.settle();
    assert!(h.hinted("d.ts"), "the review set is tier 2");
    assert_eq!(h.coverage().focus, TierState::Done);
}
