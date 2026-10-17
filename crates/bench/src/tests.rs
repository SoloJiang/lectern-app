//! Tests of the runner, the reports and the merge with fake benchmarks and
//! fake budget tables (docs/design/bench.md「测试」).

mod merge;
mod report;
mod runner;

use std::path::Path;
use std::time::Duration;

use crate::budgets::{Budget, Check, Limit, Stat, State, Target, Tier};
use crate::ctx::SampleConfig;
use crate::report::Host;
use crate::runner::Options;

/// A fake source; the runner never reads it.
const SOURCE: &str = "docs/design/bench.md#测试";

const fn budget(
    id: &'static str,
    checks: &'static [Check],
    state: State,
    target: Target,
) -> Budget {
    Budget {
        id,
        checks,
        tier: Tier::Share,
        source: SOURCE,
        state,
        target,
    }
}

const UNDER_1_MS: &[Check] = &[Check {
    stat: Stat::Median,
    limit: Limit::TimeBelow(Duration::from_millis(1)),
}];

/// Fast sampling, so measure-mode tests take milliseconds.
const FAST: SampleConfig = SampleConfig {
    min_samples: 5,
    max_samples: 200,
    max_wall: Duration::from_secs(1),
    enough_samples: 5,
    enough_time: Duration::from_millis(1),
    warmup_calls: 3,
    warmup_time: Duration::from_millis(1),
    batch_time: Duration::from_micros(100),
};

fn host() -> Host {
    Host {
        model: None,
        chip: None,
        os_version: None,
        cpus: 8,
    }
}

/// Options on the gating platform (Apple silicon macOS).
fn options<'a>(budgets: &'a [Budget], target: Target, args: &[&str], dir: &Path) -> Options<'a> {
    Options {
        target,
        budgets,
        args: args.iter().map(|a| (*a).to_owned()).collect(),
        apple_silicon: true,
        macos: true,
        scratch_root: dir.join("scratch"),
        report_dir: dir.join("reports"),
        config: FAST,
        host: host(),
        commit: None,
    }
}
