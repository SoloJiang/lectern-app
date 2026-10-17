//! What a benchmark gets: the mode, sampling and its scratch directory
//! (docs/design/bench.md「平台与噪声」).

use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::stats::{median, per_operation};
use crate::{Mode, Samples};

/// Rule 1 of the stopping rules can never go below this many samples.
pub const MIN_SAMPLES: usize = 5;

/// Sampling in measure mode. The defaults are the rules of「平台与噪声」;
/// [`Ctx::config`] can raise them, not lower the 5-sample minimum.
#[derive(Clone, Copy, Debug)]
pub struct SampleConfig {
    /// Never stop with fewer samples.
    pub min_samples: usize,
    /// Stop at this many samples...
    pub max_samples: usize,
    /// ...or once sampling (setup included) has taken this long.
    pub max_wall: Duration,
    /// Stop once there are this many samples...
    pub enough_samples: usize,
    /// ...and the sampled operations (setup excluded) took this long.
    pub enough_time: Duration,
    /// Warm-up: at least this many calls...
    pub warmup_calls: usize,
    /// ...and at least this long.
    pub warmup_time: Duration,
    /// `time` and `compare` batch operations shorter than this so that one
    /// sample is not shorter.
    pub batch_time: Duration,
}

impl Default for SampleConfig {
    fn default() -> Self {
        SampleConfig {
            min_samples: MIN_SAMPLES,
            max_samples: 10_000,
            max_wall: Duration::from_secs(5),
            enough_samples: 30,
            enough_time: Duration::from_secs(1),
            warmup_calls: 3,
            warmup_time: Duration::from_millis(100),
            batch_time: Duration::from_millis(1),
        }
    }
}

/// The largest batch: operations shorter than 1 ns are still timed in
/// samples of at least 1 ms.
const MAX_BATCH: u32 = 1_000_000;

pub struct Ctx {
    mode: Mode,
    config: SampleConfig,
    scratch: PathBuf,
}

impl Ctx {
    pub(crate) fn new(mode: Mode, config: SampleConfig, scratch: PathBuf) -> Ctx {
        Ctx {
            mode,
            config,
            scratch,
        }
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// The input size for this mode: `measure` in measure mode, `smoke`
    /// in smoke mode.
    pub fn scale<T>(&self, measure: T, smoke: T) -> T {
        match self.mode {
            Mode::Measure => measure,
            Mode::Smoke => smoke,
        }
    }

    /// An empty directory of this benchmark's own, under
    /// `CARGO_TARGET_TMPDIR/lectern-bench/<id>/`; removed after the
    /// benchmark, and recreated for a retry.
    pub fn scratch(&self) -> &Path {
        &self.scratch
    }

    /// Overrides the sampling of this benchmark (fewer samples for a slow
    /// operation, a longer run for a noisy one). The minimum stays at least
    /// 5 samples.
    pub fn config(&mut self, config: SampleConfig) {
        self.config = SampleConfig {
            min_samples: config.min_samples.max(MIN_SAMPLES),
            ..config
        };
    }

    /// Warms up, then samples `f`; each call runs the operation under test
    /// once and returns its result. Operations shorter than 1 ms are timed
    /// in batches. Results are passed to `black_box` and kept until the
    /// batch's clock stops, so dropping them is not timed.
    pub fn time<R>(&mut self, mut f: impl FnMut() -> R) -> Samples {
        if self.mode == Mode::Smoke {
            return Samples(vec![once(&mut f)]);
        }
        let k = self.batch(&mut || once(&mut f));
        let mut held = Vec::with_capacity(k as usize);
        let mut sampler = Sampler::new(self.config);
        while !sampler.done() {
            let elapsed = batch(&mut f, k, &mut held);
            sampler.push(per_operation(elapsed, k), elapsed);
        }
        sampler.finish()
    }

    /// Calls `setup` before each call of `f`, untimed (append 4 KB, change a
    /// file); `setup`'s result goes through `black_box` to `f`. Never
    /// batched: each sample is one `setup` and one `f`, so a stateful
    /// operation always meets freshly prepared input.
    pub fn time_with_setup<S, R>(
        &mut self,
        mut setup: impl FnMut() -> S,
        mut f: impl FnMut(S) -> R,
    ) -> Samples {
        let mut one = || {
            let input = black_box(setup());
            let start = Instant::now();
            let result = black_box(f(input));
            let elapsed = start.elapsed();
            drop(result);
            elapsed
        };
        if self.mode == Mode::Smoke {
            return Samples(vec![one()]);
        }
        let started = Instant::now();
        let mut calls = 0;
        while calls < self.config.warmup_calls || started.elapsed() < self.config.warmup_time {
            one();
            calls += 1;
        }
        let mut sampler = Sampler::new(self.config);
        while !sampler.done() {
            let elapsed = one();
            sampler.push(elapsed, elapsed);
        }
        sampler.finish()
    }

    /// Samples the subject and the baseline alternately, cancelling out
    /// frequency and cache drift during sampling.
    pub fn compare<R1, R2>(
        &mut self,
        mut subject: impl FnMut() -> R1,
        mut baseline: impl FnMut() -> R2,
    ) -> (Samples, Samples) {
        if self.mode == Mode::Smoke {
            return (
                Samples(vec![once(&mut subject)]),
                Samples(vec![once(&mut baseline)]),
            );
        }
        let ks = self.batch(&mut || once(&mut subject));
        let kb = self.batch(&mut || once(&mut baseline));
        let mut held_s = Vec::with_capacity(ks as usize);
        let mut held_b = Vec::with_capacity(kb as usize);
        let mut subjects = Sampler::new(self.config);
        let mut baselines = Vec::new();
        while !subjects.done() {
            let s = batch(&mut subject, ks, &mut held_s);
            let b = batch(&mut baseline, kb, &mut held_b);
            subjects.push(per_operation(s, ks), s + b);
            baselines.push(per_operation(b, kb));
        }
        (subjects.finish(), Samples(baselines))
    }

    /// Warms `call` up and returns the batch size: how many operations make
    /// a sample of at least `batch_time`.
    fn batch(&self, call: &mut dyn FnMut() -> Duration) -> u32 {
        let started = Instant::now();
        let mut times = Vec::new();
        while times.len() < self.config.warmup_calls || started.elapsed() < self.config.warmup_time
        {
            times.push(call().as_secs_f64());
        }
        times.sort_by(f64::total_cmp);
        let one = median(&times).max(1e-9);
        let target = self.config.batch_time.as_secs_f64();
        if one >= target {
            1
        } else {
            ((target / one).ceil() as u32).clamp(1, MAX_BATCH)
        }
    }
}

/// Runs `f` once and times it; the result is dropped after the clock stops.
fn once<R>(f: &mut impl FnMut() -> R) -> Duration {
    let start = Instant::now();
    let result = black_box(f());
    let elapsed = start.elapsed();
    drop(result);
    elapsed
}

/// Runs `f` `k` times into `held` (allocated before), then drops the
/// results after the clock stops.
fn batch<R>(f: &mut impl FnMut() -> R, k: u32, held: &mut Vec<R>) -> Duration {
    let start = Instant::now();
    for _ in 0..k {
        held.push(black_box(f()));
    }
    let elapsed = start.elapsed();
    held.clear();
    elapsed
}

/// The stopping rules, in order (the earlier wins):
/// 1. fewer than `min_samples`: go on;
/// 2. `max_samples`, or `max_wall` since sampling started: stop;
/// 3. at least `enough_samples` taking at least `enough_time`: stop.
pub(crate) struct Sampler {
    config: SampleConfig,
    started: Instant,
    samples: Vec<Duration>,
    busy: Duration,
}

impl Sampler {
    pub(crate) fn new(config: SampleConfig) -> Sampler {
        Sampler {
            config,
            started: Instant::now(),
            samples: Vec::new(),
            busy: Duration::ZERO,
        }
    }

    pub(crate) fn done(&self) -> bool {
        stop(
            &self.config,
            self.samples.len(),
            self.started.elapsed(),
            self.busy,
        )
    }

    /// One sample, and how long taking it kept the operation busy.
    pub(crate) fn push(&mut self, sample: Duration, busy: Duration) {
        self.samples.push(sample);
        self.busy += busy;
    }

    pub(crate) fn finish(self) -> Samples {
        Samples(self.samples)
    }
}

pub(crate) fn stop(config: &SampleConfig, samples: usize, wall: Duration, busy: Duration) -> bool {
    if samples < config.min_samples.max(MIN_SAMPLES) {
        return false;
    }
    if samples >= config.max_samples || wall >= config.max_wall {
        return true;
    }
    samples >= config.enough_samples && busy >= config.enough_time
}
