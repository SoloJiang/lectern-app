//! What a benchmark gets: the mode, sampling and its scratch directory
//! (docs/design/bench.md「平台与噪声」).

use std::any::Any;
use std::cell::RefCell;
use std::collections::HashMap;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use crate::stats::{median, per_operation};
use crate::{BenchError, Mode, Samples};

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
    /// ...or once warm-up and sampling (setup included) have taken this
    /// long.
    pub max_wall: Duration,
    /// Stop once there are this many samples...
    pub enough_samples: usize,
    /// ...and the sampled operations (setup excluded) took this long.
    pub enough_time: Duration,
    /// Warm-up: at least this many calls...
    pub warmup_calls: usize,
    /// ...and at least this long...
    pub warmup_time: Duration,
    /// ...unless the calls so far (at least one) already took this long:
    /// a slow operation is warm after one call.
    pub warmup_enough: Duration,
    /// `time` and `compare` batch operations shorter than this so that one
    /// sample is not shorter.
    pub batch_time: Duration,
}

impl Default for SampleConfig {
    fn default() -> Self {
        SampleConfig {
            min_samples: MIN_SAMPLES,
            max_samples: 10_000,
            max_wall: Duration::from_secs(3),
            enough_samples: 30,
            enough_time: Duration::from_secs(1),
            warmup_calls: 3,
            warmup_time: Duration::from_millis(100),
            warmup_enough: Duration::from_millis(300),
            batch_time: Duration::from_millis(1),
        }
    }
}

/// The largest batch: operations shorter than 1 ns are still timed in
/// samples of at least 1 ms.
const MAX_BATCH: u32 = 1_000_000;

/// The fixtures the benchmarks of one target process share
/// ([`Ctx::shared`]), by key. Dropped when the target's run ends, which
/// removes their temporary directories.
#[derive(Clone, Default)]
pub(crate) struct Fixtures(Rc<RefCell<HashMap<String, Rc<dyn Any>>>>);

pub struct Ctx {
    mode: Mode,
    config: SampleConfig,
    scratch: PathBuf,
    fixtures: Fixtures,
    persistent: PathBuf,
    judge: bool,
}

impl Ctx {
    pub(crate) fn new(
        mode: Mode,
        config: SampleConfig,
        scratch: PathBuf,
        fixtures: Fixtures,
        persistent: PathBuf,
        judge: bool,
    ) -> Ctx {
        Ctx {
            mode,
            config,
            scratch,
            fixtures,
            persistent,
            judge,
        }
    }

    /// The run is on the judging machine (`LECTERN_BENCH_JUDGE=1`, ADR
    /// 0052): a process benchmark gives it to
    /// [`Scheduler::judging`](crate::process::Scheduler::judging), so a
    /// first frame without a `presentedTime` is an error there.
    pub fn judge(&self) -> bool {
        self.judge
    }

    /// A directory kept between benchmark runs and shared by the targets of
    /// one run (`<target dir>/lectern-bench/fixtures/`), for fixtures that
    /// take long to write and are only ever copied: each lives in a
    /// subdirectory keyed by everything its content depends on, and checks
    /// itself before it is used (bench design「夹具与 `test-support`」).
    /// Not created; one run at a time.
    pub fn persistent(&self) -> &Path {
        &self.persistent
    }

    /// A read-only fixture shared by every benchmark of this target process
    /// that asks for `key`: `build` runs the first time, later calls (other
    /// benchmarks, retries) get the same value. For large inputs that take
    /// seconds to generate, such as the 100 000-file worktree. The key names
    /// the generator and its size (the mode is the same for the whole
    /// process). A benchmark that modifies its input, on disk or in memory,
    /// builds its own instead. The fixture must not live under
    /// [`Ctx::scratch`], which is removed after each benchmark.
    pub fn shared<T: 'static>(
        &self,
        key: &str,
        build: impl FnOnce() -> Result<T, BenchError>,
    ) -> Result<Rc<T>, BenchError> {
        if let Some(found) = self.fixtures.0.borrow().get(key) {
            return Rc::clone(found)
                .downcast::<T>()
                .map_err(|_| BenchError::new(format!("共享夹具 {key} 的类型与先前不同")));
        }
        let built = Rc::new(build()?);
        self.fixtures
            .0
            .borrow_mut()
            .insert(key.to_owned(), Rc::clone(&built) as Rc<dyn Any>);
        Ok(built)
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
        let started = Instant::now();
        let k = self.batch(&mut || once(&mut f));
        let mut held = Vec::with_capacity(k as usize);
        let mut sampler = Sampler::starting(self.config, started);
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
        while !warm(&self.config, calls, started.elapsed()) {
            one();
            calls += 1;
        }
        let mut sampler = Sampler::starting(self.config, started);
        while !sampler.done() {
            let elapsed = one();
            sampler.push(elapsed, elapsed);
        }
        sampler.finish()
    }

    /// One timed call, without warm-up, in both modes: for record-only
    /// diagnostics whose value only has to show where the time goes (such
    /// as `git/filter_100k/walk`), not pass a budget. The benchmark warms
    /// the operation up itself when that matters.
    pub fn once<R>(&mut self, mut f: impl FnMut() -> R) -> Samples {
        Samples(vec![once(&mut f)])
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
        let started = Instant::now();
        let ks = self.batch(&mut || once(&mut subject));
        let kb = self.batch(&mut || once(&mut baseline));
        let mut held_s = Vec::with_capacity(ks as usize);
        let mut held_b = Vec::with_capacity(kb as usize);
        let mut subjects = Sampler::starting(self.config, started);
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
        while !warm(&self.config, times.len(), started.elapsed()) {
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

/// Whether warm-up is over after `calls` calls that took `elapsed`: at least
/// `warmup_calls` and `warmup_time`, or at least one call and
/// `warmup_enough`.
pub(crate) fn warm(config: &SampleConfig, calls: usize, elapsed: Duration) -> bool {
    (calls >= config.warmup_calls && elapsed >= config.warmup_time)
        || (calls >= 1 && elapsed >= config.warmup_enough)
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
/// 2. `max_samples`, or `max_wall` since warm-up started: stop;
/// 3. at least `enough_samples` taking at least `enough_time`: stop.
pub(crate) struct Sampler {
    config: SampleConfig,
    started: Instant,
    samples: Vec<Duration>,
    busy: Duration,
}

impl Sampler {
    /// Sampling whose warm-up began at `started`.
    pub(crate) fn starting(config: SampleConfig, started: Instant) -> Sampler {
        Sampler {
            config,
            started,
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
