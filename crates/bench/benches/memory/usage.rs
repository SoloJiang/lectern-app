//! `usage/resident_bytes` (docs/design/usage.md「性能」): what the `usage`
//! plugin holds at rest, in live bytes (`CountingAlloc`; SQLite's own
//! memory, the page cache included, goes through its C allocator and is not
//! counted). The `heavy_days` fixture (30 days, 1500 sessions) is opened by
//! the plugin under a `TestKernel` whose session sources list every one of
//! those sessions with their records, and the projects service the 20
//! repositories, open. The run goes on until every session has been read
//! once from its saved cursor (nothing new: the backfill finds each log
//! unchanged at its end) and the kernel is quiescent. A kernel with a
//! plugin that only requires the same services, run the same way, is the
//! baseline. The sources, the projects and the database copy are made
//! before the count; the first sample is a warm-up and is not reported.

use std::sync::Arc;
use std::time::{Duration, Instant};

use lectern_bench::{Bench, BenchError, CountingAlloc, Ctx, Measured, Value};
use lectern_executor::Executor;
use lectern_kernel::testing::TestKernel;
use lectern_kernel::{Activation, Active, Context, Manifest, Plugin, PluginError, service};
use lectern_services::project::Projects;
use lectern_services::session::{Agent, SessionSource};
use lectern_usage::testing::{
    FakeProjects, FakeSessions, HEAVY_NOW, HEAVY_TURNS, ManualTime, TableZone, heavy_days,
    heavy_roots, heavy_sessions, synthetic_turns,
};
use lectern_usage::{UsageDeps, plugin};

const SAMPLES: usize = 3;

/// Quiet seconds the backfill waits for between batches.
const PACE: Duration = Duration::from_secs(10);

pub const RESIDENT: Bench = Bench {
    id: "usage/resident_bytes",
    run: resident,
};

/// Requires what `usage` requires and does nothing: the baseline.
struct Baseline;

impl Plugin for Baseline {
    fn manifest(&self) -> Manifest {
        Manifest {
            provides: Vec::new(),
            requires: vec![service::<dyn Projects>(), service::<dyn SessionSource>()],
            activation: vec![Activation::AfterFirstFrame],
        }
    }

    fn activate(&self, _: &mut Context<'_>) -> Result<Box<dyn Active>, PluginError> {
        Ok(Box::new(()))
    }
}

struct Fixture {
    db: tempfile::TempDir,
    claude: Arc<FakeSessions>,
    codex: Arc<FakeSessions>,
    projects: Arc<FakeProjects>,
    sessions: usize,
}

fn fixture(days: u32) -> Result<Fixture, BenchError> {
    let db = tempfile::tempdir()?;
    heavy_days(db.path(), days)?;
    let claude = Arc::new(FakeSessions::new("session-claude-code", Agent::ClaudeCode));
    let codex = Arc::new(FakeSessions::new("session-codex", Agent::Codex));
    let mut sessions = 0;
    for day in 0..days {
        for h in heavy_sessions(days, day) {
            let fake = if h.agent == Agent::Codex {
                &codex
            } else {
                &claude
            };
            fake.add(&h.id, &h.cwd, h.start_ms, None);
            let records =
                synthetic_turns(&h.agent, &h.id, &h.cwd, h.start_ms, 0..HEAVY_TURNS, h.model);
            fake.push(&h.id, records);
            sessions += 1;
        }
    }
    let projects = Arc::new(FakeProjects::new());
    for root in heavy_roots() {
        let key = projects.repo(&root, &root.join(".git"));
        projects.set_open(&key, true);
    }
    Ok(Fixture {
        db,
        claude,
        codex,
        projects,
        sessions,
    })
}

fn resident(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let f = fixture(ctx.scale(30, 2))?;
    let mut samples = Vec::new();
    for round in 0..=ctx.scale(SAMPLES, 1) {
        let data = ctx.scratch().join(format!("data-{round}"));
        std::fs::create_dir_all(&data)?;
        std::fs::copy(f.db.path().join("usage.db"), data.join("usage.db"))?;
        let baseline = at_rest(&f, None)?;
        let with = at_rest(&f, Some(data))?;
        if round > 0 {
            samples.push(with.saturating_sub(baseline));
        }
    }
    Ok(vec![
        Measured::new("usage/resident_bytes", Value::Bytes(samples)).with_note(Some(format!(
            "{} sessions read once, 20 repositories open",
            f.sessions
        ))),
    ])
}

/// Live bytes a kernel holds at rest, with the `usage` plugin on `data`
/// (or the baseline plugin), after every session has been read once.
fn at_rest(f: &Fixture, data: Option<std::path::PathBuf>) -> Result<u64, BenchError> {
    let reads = || f.claude.total_reads() + f.codex.total_reads();
    let before_reads = reads();
    let before = CountingAlloc::live();
    let executor = Executor::manual();
    let time = ManualTime::new(HEAVY_NOW);
    let kernel = match &data {
        Some(dir) => {
            let (_, usage) = plugin(UsageDeps {
                executor: executor.clone(),
                data_dir: dir.clone(),
                zone: Arc::new(TableZone::fixed(0)),
            });
            TestKernel::new(usage.with_time(&time))
        }
        None => TestKernel::new(Baseline),
    };
    let kernel = kernel
        .mock::<dyn Projects>(f.projects.clone())
        .mock::<dyn SessionSource>(f.claude.clone())
        .mock::<dyn SessionSource>(f.codex.clone());
    kernel
        .activate()
        .map_err(|e| BenchError::debug("activate", e))?;
    let inner = kernel.inner().ok_or_else(|| BenchError::new("no kernel"))?;
    let deadline = Instant::now() + Duration::from_secs(600);
    if data.is_some() {
        // One read per session, one batch per quiet stretch.
        while reads() - before_reads < f.sessions {
            if Instant::now() > deadline {
                return Err(BenchError::new(format!(
                    "{} of {} sessions read",
                    reads() - before_reads,
                    f.sessions
                )));
            }
            executor.run_until_idle();
            if inner.is_quiescent() {
                time.advance(PACE);
            } else {
                std::thread::yield_now();
            }
        }
        // The archive check and the reclaim check after the backfill.
        for _ in 0..4 {
            executor.run_until_idle();
            time.advance(PACE);
        }
    }
    while executor.run_until_idle() > 0 || !inner.is_quiescent() {
        if Instant::now() > deadline {
            return Err(BenchError::new("the kernel did not come to rest"));
        }
        std::thread::yield_now();
    }
    let held = CountingAlloc::live().saturating_sub(before) as u64;
    drop(inner);
    drop(kernel);
    executor.run_until_idle();
    Ok(held)
}
