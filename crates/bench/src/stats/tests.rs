use std::time::Duration;

use super::*;
use crate::budgets::{BUDGETS, Check, Limit, Stat, lookup};
use crate::ctx::{SampleConfig, stop};

/// Milliseconds, rounded to the nanosecond.
fn ms(n: f64) -> Duration {
    Duration::from_nanos((n * 1e6).round() as u64)
}

fn samples(values_ms: &[f64]) -> Samples {
    Samples(values_ms.iter().map(|v| ms(*v)).collect())
}

fn passes(budget_id: &str, value: &Value) -> bool {
    let budget = lookup(BUDGETS, budget_id).unwrap();
    budget
        .checks
        .iter()
        .all(|c| evaluate(c, value).unwrap().pass)
}

#[test]
fn stats_counts_are_record_only() {
    let value = Value::Count(100_000);
    assert_eq!(describe(&value), "100000");
    assert!(!value.noisy());
    let check = Check {
        stat: Stat::Median,
        limit: Limit::BytesAtMost(1024),
    };
    assert!(
        evaluate(&check, &value).is_err(),
        "no check applies to a count"
    );
}

#[test]
fn stats_median_of_odd_and_even_counts() {
    assert_eq!(median(&[1.0, 2.0, 9.0]), 2.0);
    assert_eq!(median(&[1.0, 2.0, 4.0, 9.0]), 3.0);
    assert_eq!(median(&[5.0]), 5.0);
    assert!(median(&[]).is_nan());
}

#[test]
fn stats_p95_by_nearest_rank() {
    let sorted: Vec<f64> = (1..=20).map(f64::from).collect();
    // 95% of 20 is 19: the 19th sample.
    assert_eq!(p95(&sorted), 19.0);
    let sorted: Vec<f64> = (1..=100).map(f64::from).collect();
    assert_eq!(p95(&sorted), 95.0);
    assert_eq!(p95(&[7.0]), 7.0);
}

#[test]
fn stats_summary_has_max_and_median_absolute_deviation() {
    let s = summarize(&[3.0, 1.0, 2.0, 10.0, 2.0]);
    assert_eq!(s.median, 2.0);
    assert_eq!(s.max, 10.0);
    // Deviations 1, 1, 0, 8, 0 → sorted 0, 0, 1, 1, 8.
    assert_eq!(s.mad, 1.0);
    assert_eq!(s.samples, 5);
}

#[test]
fn stats_batch_divides_by_k() {
    assert_eq!(
        per_operation(Duration::from_millis(1), 1000),
        Duration::from_micros(1)
    );
    assert_eq!(
        per_operation(Duration::from_millis(3), 1),
        Duration::from_millis(3)
    );
    // k = 0 never happens; it is treated as 1 rather than dividing by zero.
    assert_eq!(
        per_operation(Duration::from_millis(3), 0),
        Duration::from_millis(3)
    );
}

#[test]
fn stats_cold_start_one_of_20_at_25_ms_passes() {
    let mut v = vec![15.0; 19];
    v.push(25.0);
    assert!(passes("app/cold_start", &Value::Time(samples(&v))));
}

#[test]
fn stats_cold_start_two_above_20_ms_fail() {
    let mut v = vec![15.0; 18];
    v.extend([21.0, 22.0]);
    assert!(!passes("app/cold_start", &Value::Time(samples(&v))));
}

#[test]
fn stats_cold_start_one_at_31_ms_fails() {
    let mut v = vec![15.0; 19];
    v.push(31.0);
    assert!(!passes("app/cold_start", &Value::Time(samples(&v))));
}

#[test]
fn stats_cold_start_exactly_30_ms_passes() {
    let mut v = vec![15.0; 19];
    v.push(30.0);
    assert!(passes("app/cold_start", &Value::Time(samples(&v))));
    // Exactly 20 ms is not above 20 ms.
    let v = vec![20.0; 20];
    assert!(passes("app/cold_start", &Value::Time(samples(&v))));
}

#[test]
fn stats_time_below_is_strict_and_at_most_is_not() {
    let below = Check {
        stat: Stat::Median,
        limit: Limit::TimeBelow(ms(1.0)),
    };
    let at_most = Check {
        stat: Stat::Median,
        limit: Limit::TimeAtMost(ms(1.0)),
    };
    let value = Value::Time(samples(&[1.0]));
    assert!(!evaluate(&below, &value).unwrap().pass);
    assert!(evaluate(&at_most, &value).unwrap().pass);
}

#[test]
fn stats_throughput_is_bytes_over_the_median_time() {
    // 200 MB in 1 s at the median: exactly the agents budget.
    let value = Value::Throughput {
        bytes: 200 << 20,
        time: samples(&[900.0, 1000.0, 1500.0]),
    };
    assert!(passes("agents/parse_throughput", &value));
    let slower = Value::Throughput {
        bytes: 200 << 20,
        time: samples(&[900.0, 1001.0, 1500.0]),
    };
    assert!(!passes("agents/parse_throughput", &slower));
    let outcome = evaluate(
        &lookup(BUDGETS, "agents/parse_throughput").unwrap().checks[0],
        &value,
    );
    assert_eq!(outcome.unwrap().measured, "200 MB/s (median)");
}

#[test]
fn stats_ratio_takes_both_medians_and_limits_the_subject() {
    // safe_stat: ratio ≤ 3 and the subject's median < 10 µs.
    let value = Value::Ratio {
        subject: samples(&[0.003, 0.003, 0.009]),
        baseline: samples(&[0.001, 0.001, 0.0001]),
    };
    assert!(passes("platform/safe_stat", &value));
    assert_eq!(ratio(&samples(&[3.0]), &samples(&[1.0]), Stat::Median), 3.0);
    let slow = Value::Ratio {
        subject: samples(&[0.011]),
        baseline: samples(&[0.01]),
    };
    assert!(!passes("platform/safe_stat", &slow));
    let ratio_over = Value::Ratio {
        subject: samples(&[0.0031]),
        baseline: samples(&[0.001]),
    };
    assert!(!passes("platform/safe_stat", &ratio_over));
}

#[test]
fn stats_bytes_use_mb_of_2_pow_20() {
    let under = Value::Bytes(vec![60 * (1 << 20) - 1, 61 << 20, 10]);
    assert!(passes("app/idle_empty", &under));
    let at = Value::Bytes(vec![60 << 20]);
    assert!(!passes("app/idle_empty", &at));
}

#[test]
fn stats_noisy_when_the_deviation_is_above_10_percent() {
    assert!(!Value::Time(samples(&[1.0, 1.05, 0.95, 1.0, 1.0])).noisy());
    assert!(Value::Time(samples(&[1.0, 1.5, 0.5, 1.2, 0.8])).noisy());
}

#[test]
fn stats_checks_that_do_not_apply_are_errors() {
    let bytes = Check {
        stat: Stat::Median,
        limit: Limit::BytesBelow(1),
    };
    assert!(evaluate(&bytes, &Value::Time(samples(&[1.0]))).is_err());
    let ratio = Check {
        stat: Stat::Median,
        limit: Limit::RatioAtMost(1.0),
    };
    assert!(evaluate(&ratio, &Value::Bytes(vec![1])).is_err());
}

fn frames(work_ms: &[f64], intervals_ms: &[f64], hz: f64) -> Value {
    Value::Frames {
        work: samples(work_ms),
        intervals: samples(intervals_ms),
        refresh: Duration::from_secs_f64(1.0 / hz),
    }
}

#[test]
fn stats_frames_at_60_hz_with_5_ms_of_work_pass() {
    let value = frames(&[5.0; 30], &[16.7; 29], 60.0);
    assert!(passes("app/scroll_frames", &value));
}

#[test]
fn stats_frames_with_one_9_ms_frame_are_over() {
    let mut work = vec![5.0; 30];
    work[10] = 9.0;
    assert!(!passes(
        "app/scroll_frames",
        &frames(&work, &[16.7; 29], 60.0)
    ));
}

#[test]
fn stats_frames_blocked_for_two_periods_are_over() {
    // The main thread blocks 20 ms after `requested`: this frame's work is
    // still short, but `started` misses a display link callback.
    let mut intervals = vec![16.7; 29];
    intervals[10] = 33.3;
    assert!(!passes(
        "app/input_frames",
        &frames(&[5.0; 30], &intervals, 60.0)
    ));
}

#[test]
fn stats_frames_at_120_hz_match_max_interval_of_8_3_ms() {
    // At 120 Hz both conditions together say "every interval ≤ 8.3 ms
    // (1 period) unless a frame is dropped": 1.5 periods is 12.5 ms, and a
    // dropped frame makes it 16.7 ms.
    assert!(passes(
        "app/animation_frames",
        &frames(&[5.0; 30], &[8.33; 29], 120.0)
    ));
    let mut dropped = vec![8.33; 29];
    dropped[3] = 16.7;
    assert!(!passes(
        "app/animation_frames",
        &frames(&[5.0; 30], &dropped, 120.0)
    ));
    assert!(!passes(
        "app/animation_frames",
        &frames(&[8.4; 30], &[8.33; 29], 120.0)
    ));
}

#[test]
fn stats_warm_up_ends_after_three_calls_or_one_slow_call() {
    use crate::ctx::warm;
    let c = SampleConfig::default();
    let ms = Duration::from_millis;
    // At least 3 calls and 100 ms.
    assert!(!warm(&c, 2, ms(250)));
    assert!(!warm(&c, 3, ms(99)));
    assert!(warm(&c, 3, ms(100)));
    assert!(warm(&c, 40, ms(100)));
    // A call that took 300 ms is warm-up enough.
    assert!(!warm(&c, 0, ms(500)));
    assert!(warm(&c, 1, ms(300)));
    assert!(warm(&c, 2, ms(400)));
}

/// A 1 s operation: one warm-up call, then the 5-sample minimum, 6 s in
/// all, where the 5 s cap counted from the end of warm-up took 8 s.
#[test]
fn stats_slow_operation_takes_the_minimum() {
    use crate::ctx::warm;
    let c = SampleConfig::default();
    let s = Duration::from_secs;
    let mut calls = 0;
    while !warm(&c, calls, s(calls as u64)) {
        calls += 1;
    }
    assert_eq!(calls, 1);
    let mut samples = 0;
    while !stop(&c, samples, s((calls + samples) as u64), s(samples as u64)) {
        samples += 1;
    }
    assert_eq!(samples, 5);
}

#[test]
fn stats_stop_rules_in_order() {
    let c = SampleConfig::default();
    let s = Duration::from_secs;
    // 1. fewer than 5 samples: never stop, even past 3 s.
    assert!(!stop(&c, 4, s(60), s(60)));
    // 2. 10 000 samples or 3 s of wall time since warm-up began.
    assert!(stop(&c, 10_000, s(0), s(0)));
    assert!(stop(&c, 5, s(3), s(0)));
    assert!(!stop(&c, 5, Duration::from_millis(2999), s(0)));
    // 3. 30 samples taking 1 s.
    assert!(stop(&c, 30, s(2), s(1)));
    assert!(!stop(&c, 30, s(2), Duration::from_millis(999)));
    assert!(!stop(&c, 29, s(2), s(2)));
    // A config cannot lower the 5-sample minimum.
    let low = SampleConfig {
        min_samples: 1,
        ..c
    };
    assert!(!stop(&low, 4, s(60), s(60)));
}
