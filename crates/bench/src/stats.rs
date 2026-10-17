//! Statistics over samples and the comparison with a budget's checks
//! (docs/design/bench.md「平台与噪声」).

use std::time::Duration;

use crate::budgets::{Check, Limit, Stat, fmt_bytes_f, fmt_number, fmt_seconds};
use crate::{Samples, Value};

/// Summary statistics of one set of samples, in the samples' unit
/// (seconds or bytes).
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct Summary {
    pub median: f64,
    pub p95: f64,
    pub max: f64,
    /// Median absolute deviation from the median.
    pub mad: f64,
    pub samples: usize,
}

/// The median: the middle sample, or the mean of the two middle ones.
pub(crate) fn median(sorted: &[f64]) -> f64 {
    let n = sorted.len();
    if n == 0 {
        return f64::NAN;
    }
    if n % 2 == 1 {
        sorted[n / 2]
    } else {
        (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
    }
}

/// The 95th percentile by nearest rank: the smallest sample with at least
/// 95% of the samples at or below it.
pub(crate) fn p95(sorted: &[f64]) -> f64 {
    let n = sorted.len();
    if n == 0 {
        return f64::NAN;
    }
    let rank = (n * 95).div_ceil(100).max(1);
    sorted[rank - 1]
}

pub(crate) fn summarize(values: &[f64]) -> Summary {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let m = median(&sorted);
    let mut deviations: Vec<f64> = sorted.iter().map(|x| (x - m).abs()).collect();
    deviations.sort_by(f64::total_cmp);
    Summary {
        median: m,
        p95: p95(&sorted),
        max: sorted.last().copied().unwrap_or(f64::NAN),
        mad: median(&deviations),
        samples: sorted.len(),
    }
}

impl Samples {
    pub(crate) fn seconds(&self) -> Vec<f64> {
        self.0.iter().map(Duration::as_secs_f64).collect()
    }

    pub(crate) fn summary(&self) -> Summary {
        summarize(&self.seconds())
    }

    fn count_above(&self, threshold: Duration) -> usize {
        self.0.iter().filter(|d| **d > threshold).count()
    }
}

/// A batch of `k` operations took `elapsed`: the per-operation sample.
pub(crate) fn per_operation(elapsed: Duration, k: u32) -> Duration {
    elapsed / k.max(1)
}

impl Summary {
    fn get(&self, stat: Stat) -> Option<f64> {
        match stat {
            Stat::Median => Some(self.median),
            Stat::P95 => Some(self.p95),
            Stat::Max => Some(self.max),
            Stat::CountAbove(_) => None,
        }
    }

    /// The median absolute deviation is more than 10% of the median.
    pub(crate) fn noisy(&self) -> bool {
        self.mad > self.median * 0.1
    }
}

/// The kinds of `Value`, for checking which checks apply to which value.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Kind {
    Time,
    Throughput,
    Ratio,
    Bytes,
    Frames,
}

impl Kind {
    #[cfg(test)]
    pub(crate) const ALL: [Kind; 5] = [
        Kind::Time,
        Kind::Throughput,
        Kind::Ratio,
        Kind::Bytes,
        Kind::Frames,
    ];
}

impl Value {
    pub(crate) fn kind(&self) -> Kind {
        match self {
            Value::Time(_) => Kind::Time,
            Value::Throughput { .. } => Kind::Throughput,
            Value::Ratio { .. } => Kind::Ratio,
            Value::Bytes(_) => Kind::Bytes,
            Value::Frames { .. } => Kind::Frames,
        }
    }

    pub(crate) fn noisy(&self) -> bool {
        match self {
            Value::Time(s) | Value::Throughput { time: s, .. } => s.summary().noisy(),
            Value::Ratio { subject, baseline } => {
                subject.summary().noisy() || baseline.summary().noisy()
            }
            Value::Bytes(b) => bytes_summary(b).noisy(),
            Value::Frames { .. } => false,
        }
    }
}

pub(crate) fn bytes_summary(bytes: &[u64]) -> Summary {
    summarize(&bytes.iter().map(|b| *b as f64).collect::<Vec<_>>())
}

/// Whether `check` can be applied to a value of kind `kind`.
pub(crate) fn applies(check: &Check, kind: Kind) -> bool {
    let plain = !matches!(check.stat, Stat::CountAbove(_));
    match check.limit {
        Limit::TimeBelow(_) | Limit::TimeAtMost(_) => {
            plain && matches!(kind, Kind::Time | Kind::Ratio | Kind::Frames)
        }
        Limit::ThroughputAtLeast(_) => plain && kind == Kind::Throughput,
        Limit::BytesBelow(_) | Limit::BytesAtMost(_) => plain && kind == Kind::Bytes,
        Limit::RatioAtMost(_) => plain && kind == Kind::Ratio,
        Limit::CountAtMost(_) => !plain && kind == Kind::Time,
        Limit::RefreshMultipleAtMost(_) => plain && kind == Kind::Frames,
    }
}

/// One check applied to one value.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct Outcome {
    pub pass: bool,
    /// What was compared with the limit, in words: "median 0.95 ms".
    pub measured: String,
}

/// Applies `check` to `value`; `Err` when it does not apply to this kind.
pub(crate) fn evaluate(check: &Check, value: &Value) -> Result<Outcome, String> {
    if !applies(check, value.kind()) {
        return Err(format!(
            "预算「{}」不适用于 {:?} 类型的结果",
            check.describe(),
            value.kind()
        ));
    }
    let stat = check.stat;
    let name = stat.name();
    let outcome = match (check.limit, value) {
        (Limit::CountAtMost(limit), Value::Time(samples)) => {
            let Stat::CountAbove(threshold) = stat else {
                unreachable!("checked by applies")
            };
            let count = samples.count_above(threshold);
            Outcome {
                pass: count <= limit as usize,
                measured: format!("{name}: {count}"),
            }
        }
        (Limit::TimeBelow(d) | Limit::TimeAtMost(d), _) => {
            let summary = match value {
                Value::Time(s) => s.summary(),
                Value::Ratio { subject, .. } => subject.summary(),
                Value::Frames { work, .. } => work.summary(),
                _ => unreachable!("checked by applies"),
            };
            let x = summary.get(stat).expect("plain stat");
            let limit = d.as_secs_f64();
            let pass = if matches!(check.limit, Limit::TimeBelow(_)) {
                x < limit
            } else {
                x <= limit
            };
            Outcome {
                pass,
                measured: format!("{name} {}", fmt_seconds(x)),
            }
        }
        (Limit::ThroughputAtLeast(limit), Value::Throughput { bytes, time }) => {
            let t = time.summary().get(stat).expect("plain stat");
            let rate = *bytes as f64 / t;
            Outcome {
                pass: rate >= limit as f64,
                measured: format!("{}/s ({name})", fmt_bytes_f(rate)),
            }
        }
        (Limit::BytesBelow(limit) | Limit::BytesAtMost(limit), Value::Bytes(bytes)) => {
            let x = bytes_summary(bytes).get(stat).expect("plain stat");
            let pass = if matches!(check.limit, Limit::BytesBelow(_)) {
                x < limit as f64
            } else {
                x <= limit as f64
            };
            Outcome {
                pass,
                measured: format!("{name} {}", fmt_bytes_f(x)),
            }
        }
        (Limit::RatioAtMost(limit), Value::Ratio { subject, baseline }) => {
            let r = ratio(subject, baseline, stat);
            Outcome {
                pass: r <= limit,
                measured: format!("ratio {} ({name})", fmt_number(r)),
            }
        }
        (
            Limit::RefreshMultipleAtMost(limit),
            Value::Frames {
                intervals, refresh, ..
            },
        ) => {
            let x = intervals.summary().get(stat).expect("plain stat") / refresh.as_secs_f64();
            Outcome {
                pass: x <= limit,
                measured: format!("interval {} refresh periods ({name})", fmt_number(x)),
            }
        }
        _ => unreachable!("checked by applies"),
    };
    Ok(outcome)
}

/// Subject over baseline, both by the same statistic.
pub(crate) fn ratio(subject: &Samples, baseline: &Samples, stat: Stat) -> f64 {
    let s = subject.summary().get(stat).unwrap_or(f64::NAN);
    let b = baseline.summary().get(stat).unwrap_or(f64::NAN);
    s / b
}

/// The value in words when there is no check to describe it (record only).
pub(crate) fn describe(value: &Value) -> String {
    match value {
        Value::Time(s) => format!("median {}", fmt_seconds(s.summary().median)),
        Value::Throughput { bytes, time } => format!(
            "{}/s (median)",
            fmt_bytes_f(*bytes as f64 / time.summary().median)
        ),
        Value::Ratio { subject, baseline } => format!(
            "ratio {} (median)",
            fmt_number(ratio(subject, baseline, Stat::Median))
        ),
        Value::Bytes(b) => format!("median {}", fmt_bytes_f(bytes_summary(b).median)),
        Value::Frames {
            work,
            intervals,
            refresh,
        } => format!(
            "max work {}, max interval {} refresh periods",
            fmt_seconds(work.summary().max),
            fmt_number(intervals.summary().max / refresh.as_secs_f64())
        ),
    }
}

#[cfg(test)]
mod tests;
