//! The judging of time-boxed rounds (`crate::rounds`, ADR 0043).

use std::time::Duration;

use crate::budgets::{BUDGETS, lookup};
use crate::rounds::{Cuts, Round, Verdict};
use crate::stats::evaluate;
use crate::{Samples, Value};

const BOX: Duration = Duration::from_millis(1900);

/// The round of the load run that reproduced the CI failure: the store's
/// checks outside the scope cut at the time box, everything stored.
fn cut() -> Round {
    Round {
        elapsed: Duration::from_millis(1917),
        time_box: BOX,
        partial: Some((0, 7430)),
        missing: Some(0),
        evictions: Some(0),
    }
}

#[test]
fn rounds_complete_rounds_keep_their_times() {
    let round = Round {
        elapsed: Duration::from_millis(1170),
        partial: None,
        ..cut()
    };
    assert_eq!(round.verdict(), Verdict::Complete);
    let mut cuts = Cuts::default();
    assert_eq!(cuts.judge(&round, "d"), Ok(None));
    assert_eq!(cuts, Cuts::default());
    assert_eq!(cuts.note(), None);
}

#[test]
fn rounds_a_time_boxed_round_is_charged_and_recorded() {
    let mut cuts = Cuts::default();
    assert_eq!(cut().verdict(), Verdict::Cut);
    let charge = cuts.judge(&cut(), "diagnosis").unwrap();
    assert_eq!(charge, Some(Duration::from_millis(1917 + 1900)));
    let fewer = Round {
        partial: Some((0, 12)),
        ..cut()
    };
    cuts.judge(&fewer, "later").unwrap();
    assert_eq!(cuts.rounds, 2);
    assert_eq!(cuts.max_outside, 7430, "the most one round left outside");
    let note = cuts.note().unwrap();
    assert!(note.contains("2 rounds cut"), "{note}");
    assert!(note.ends_with("12 outside; later"), "{note}");
}

#[test]
fn rounds_other_incomplete_rounds_are_errors() {
    let cases = [
        (
            "unsearched",
            Round {
                partial: Some((3, 7430)),
                ..cut()
            },
        ),
        (
            "before its",
            Round {
                elapsed: Duration::from_millis(1200),
                ..cut()
            },
        ),
        (
            "missing",
            Round {
                missing: Some(5),
                ..cut()
            },
        ),
        (
            "missing",
            Round {
                missing: None,
                ..cut()
            },
        ),
        (
            "evictions",
            Round {
                evictions: Some(83),
                ..cut()
            },
        ),
        (
            "evictions",
            Round {
                evictions: None,
                ..cut()
            },
        ),
        (
            "nothing left out",
            Round {
                partial: Some((0, 0)),
                ..cut()
            },
        ),
    ];
    for (why, round) in cases {
        let mut cuts = Cuts::default();
        let error = cuts.judge(&round, "d").unwrap_err();
        assert!(error.contains(why), "{why}: {error}");
        assert_eq!(cuts, Cuts::default(), "{why}: not counted as cut");
    }
}

/// The charge is over both budgets of `code_intel/references_full`: with
/// 11 samples (12 rounds, the first a warm-up), 5 cut rounds still pass the
/// median and 6 fail it, for `/all` and `/first` alike.
#[test]
fn rounds_the_median_fails_once_more_than_half_are_cut() {
    let charge = cut().charge();
    for (id, complete) in [
        (
            "code_intel/references_full/all",
            Duration::from_millis(1200),
        ),
        (
            "code_intel/references_full/first",
            Duration::from_millis(10),
        ),
    ] {
        let budget = lookup(BUDGETS, id).unwrap();
        let judged = |cut_rounds: usize| {
            let mut samples = vec![complete; 11 - cut_rounds];
            samples.extend(vec![charge; cut_rounds]);
            let value = Value::Time(Samples(samples));
            budget
                .checks
                .iter()
                .all(|check| evaluate(check, &value).unwrap().pass)
        };
        assert!(judged(0), "{id}");
        assert!(judged(5), "{id}: 5 of 11 cut");
        assert!(!judged(6), "{id}: 6 of 11 cut");
        assert!(!judged(11), "{id}");
    }
}

#[test]
fn rounds_cuts_give_their_record_results() {
    let mut cuts = Cuts::default();
    let none = cuts.measured("x/y");
    assert_eq!(none[0].id, "x/y/partial_rounds");
    assert!(matches!(none[0].value, Value::Count(0)));
    assert_eq!(none[1].id, "x/y/outside");
    assert!(none.iter().all(|m| m.note.is_none()));
    cuts.judge(&cut(), "d").unwrap();
    let one = cuts.measured("x/y");
    assert!(matches!(one[0].value, Value::Count(1)));
    assert!(matches!(one[1].value, Value::Count(7430)));
    assert!(one.iter().all(|m| m.note == cuts.note()));
}
