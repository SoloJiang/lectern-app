//! `code_intel/*` of the queries and the name table (docs/design/
//! code_intel.md「性能」, step 4): an indexer over a `FakeGit` repository
//! whose 100 000 files are virtual (index entries and stored synthetic
//! extractions, no worktree file) plus the real files a query reads.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use lectern_bench::{Bench, BenchError, Ctx, Measured, Mode, Samples, Value};
use lectern_cache::{Budgets, Cache, Config};
use lectern_code_intel::SystemClock;
use lectern_code_intel::names::build_table;
use lectern_code_intel::testing::{
    FakeGit, Rig, packages, synthetic, virtual_repo, virtual_repo_with,
};
use lectern_services::code_intel::{
    CodeIntelEvent, Completeness, Position, Query, QueryId, QueryKind, QueryState, TextOrigin,
    TextRef,
};

fn cache(root: &Path) -> Cache {
    Cache::new(Config {
        root: root.to_path_buf(),
        budgets: Budgets::default(),
        ui_thread: None,
    })
}

/// The position of the first `needle` in `text`.
fn position(text: &[u8], needle: &str) -> Result<Position, BenchError> {
    let at = text
        .windows(needle.len())
        .position(|w| w == needle.as_bytes())
        .ok_or_else(|| BenchError::new(format!("no {needle}")))?;
    let line = text[..at].iter().filter(|b| **b == b'\n').count() as u32;
    let start = text[..at]
        .iter()
        .rposition(|b| *b == b'\n')
        .map_or(0, |i| i + 1);
    Ok(Position {
        line,
        column: (at - start) as u32,
    })
}

fn text_ref(git: &FakeGit, path: &str, text: &[u8]) -> TextRef {
    TextRef {
        worktree: Some(git.worktree_key("main")),
        path: path.into(),
        oid: git.oid(text),
        origin: TextOrigin::Worktree,
    }
}

/// With `CODE_INTEL_BENCH_FULL` set, the queries run with the project's
/// namespace full (`Indexer::set_namespace_full`): the full-mode numbers of
/// the design's「实际验证」, recorded by hand, never gated.
fn maybe_full(rig: &Rig) {
    if std::env::var_os("CODE_INTEL_BENCH_FULL").is_some() {
        rig.indexer.set_namespace_full(&rig.git.key());
    }
}

/// Every `Results` event's (query, generation, time).
type Seen = Arc<Mutex<Vec<(QueryId, u32, Instant)>>>;

fn listen(rig: &Rig) -> Result<(Seen, lectern_kernel::Subscription), BenchError> {
    let seen: Seen = Arc::default();
    let s = seen.clone();
    let subscription = rig
        .host
        .handle
        .on::<CodeIntelEvent>(move |e, _| {
            if let CodeIntelEvent::Results { id, generation } = e {
                s.lock().unwrap().push((*id, *generation, Instant::now()));
            }
        })
        .ok_or_else(|| BenchError::new("cannot listen"))?;
    Ok((seen, subscription))
}

/// Runs one query to the end; its results.
fn run(
    rig: &Rig,
    query: &Query,
) -> Result<(QueryId, Arc<lectern_services::code_intel::QueryResults>), BenchError> {
    let id = rig.indexer.query(query.clone());
    rig.executor.run_until_idle();
    let r = rig
        .indexer
        .results(id)
        .ok_or_else(|| BenchError::new("the query is gone"))?;
    rig.indexer.close(id);
    Ok((id, r))
}

/// A jump answered from the index (「性能」: candidates ≤ 50 ms, this
/// crate's part ≤ 20 ms): 100 000 indexed files in sub-projects of 1000,
/// a 64 KiB current text calling a function some 130 of them define.
pub const GOTO_INDEXED: Bench = Bench {
    id: "code_intel/goto_indexed",
    run: goto_indexed,
};

fn goto_indexed(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let n = ctx.scale(100_000, 3_000);
    let git = Arc::new(FakeGit::new());
    let dir = tempfile::tempdir_in(ctx.scratch())?;
    let cache = cache(dir.path());
    virtual_repo(&git, &cache, n, &["main"]);
    let path = "packages/pkg000/src/mod00/current.ts";
    let name = synthetic::shared_name();
    let mut text = synthetic::source("ts", 999_999, 64 << 10);
    text.extend_from_slice(
        format!("\nexport function jump(x: number) {{ return {name}(x); }}\n").as_bytes(),
    );
    git.add("main", &[(path, &text)]);
    git.commit("main");
    let rig = Rig::new(git, cache, Arc::new(SystemClock));
    rig.open_with(packages(&synthetic::package_dirs(n)));
    rig.settle();
    maybe_full(&rig);
    let query = Query {
        project: rig.git.key(),
        kind: QueryKind::Definition {
            at: text_ref(&rig.git, path, &text),
            position: position(&text, &format!("{name}(x)"))?,
        },
    };
    let (_, r) = run(&rig, &query)?;
    if r.candidates.is_empty() || r.state != QueryState::Done(Completeness::Complete) {
        return Err(BenchError::new(format!(
            "{} candidates, {:?}",
            r.candidates.len(),
            r.state
        )));
    }
    let time = ctx.time(|| run(&rig, &query));
    Ok(vec![Measured::new(
        "code_intel/goto_indexed",
        Value::Time(time),
    )])
}

/// The first jump of a fresh index (record-only, against the 50 ms
/// candidates budget): the cloudprint shape (728 files), opened and
/// settled, so the background task has compiled the languages' queries;
/// no jump before it in that index. Each sample is a new index with its
/// own language table, so nothing an earlier sample compiled is reused.
pub const GOTO_FIRST: Bench = Bench {
    id: "code_intel/goto_first",
    run: goto_first,
};

fn goto_first(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let n = 728;
    let mut samples = Vec::new();
    for _ in 0..slow_samples(ctx) {
        let git = Arc::new(FakeGit::new());
        let dir = tempfile::tempdir_in(ctx.scratch())?;
        let cache = cache(dir.path());
        virtual_repo(&git, &cache, n, &["main"]);
        let path = "packages/pkg000/src/mod00/current.ts";
        let name = synthetic::shared_name();
        let mut text = synthetic::source("ts", 999_999, 16 << 10);
        text.extend_from_slice(
            format!("\nexport function jump(x: number) {{ return {name}(x); }}\n").as_bytes(),
        );
        git.add("main", &[(path, &text)]);
        git.commit("main");
        let rig = Rig::new(git, cache, Arc::new(SystemClock));
        rig.open_with(packages(&synthetic::package_dirs(n)));
        rig.settle();
        let query = Query {
            project: rig.git.key(),
            kind: QueryKind::Definition {
                at: text_ref(&rig.git, path, &text),
                position: position(&text, &format!("{name}(x)"))?,
            },
        };
        let start = Instant::now();
        let (_, r) = run(&rig, &query)?;
        let took = start.elapsed();
        if r.candidates.is_empty() {
            return Err(BenchError::new(format!("no candidates: {:?}", r.state)));
        }
        samples.push(took);
    }
    Ok(vec![Measured::new(
        "code_intel/goto_first",
        Value::Time(Samples(samples)),
    )])
}

/// `Named` for a name some 50 000 files define (record-only): every unit
/// of a virtual repository also defines `id`. From the query to its first
/// (and only) batch: the lookup, ranking every candidate, the event.
pub const NAMED_50K: Bench = Bench {
    id: "code_intel/named_50k",
    run: named_50k,
};

fn named_50k(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let n = ctx.scale(60_000, 600);
    let git = Arc::new(FakeGit::new());
    let dir = tempfile::tempdir_in(ctx.scratch())?;
    let cache = cache(dir.path());
    let units = virtual_repo_with(&git, &cache, n, &["main"], Some("id"));
    git.commit("main");
    let rig = Rig::new(git, cache, Arc::new(SystemClock));
    rig.open_with(packages(&synthetic::package_dirs(n)));
    rig.settle();
    maybe_full(&rig);
    let (seen, _subscription) = listen(&rig)?;
    let query = Query {
        project: rig.git.key(),
        kind: QueryKind::Named {
            name: "id".into(),
            near: Some(synthetic::path(0).into()),
        },
    };
    let mut samples = Vec::new();
    for round in 0..slow_samples(ctx) {
        seen.lock().unwrap().clear();
        let start = Instant::now();
        let (id, r) = run(&rig, &query)?;
        let first = seen
            .lock()
            .unwrap()
            .iter()
            .find(|(i, _, _)| *i == id)
            .map(|(_, _, t)| *t)
            .ok_or_else(|| BenchError::new("no batch"))?;
        if r.candidates.len() < units {
            return Err(BenchError::new(format!(
                "{} candidates of {units} units",
                r.candidates.len()
            )));
        }
        if round > 0 || ctx.mode() == Mode::Smoke {
            samples.push(first - start);
        }
    }
    Ok(vec![Measured::new(
        "code_intel/named_50k",
        Value::Time(Samples(samples)),
    )])
}

/// Samples of an operation taking about a second: one warm-up, then 5.
fn slow_samples(ctx: &Ctx) -> usize {
    match ctx.mode() {
        Mode::Measure => 6,
        Mode::Smoke => 1,
    }
}

/// A jump nothing indexed answers: the search of a sub-project of 6000
/// unindexed files, each a hit, stops one second after the query
/// (`≤ 1.02 s`, cancellation within 20 ms). From the query to the event of
/// its last batch.
pub const SEARCH_STOP: Bench = Bench {
    id: "code_intel/search_stop",
    run: search_stop,
};

fn search_stop(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let n = ctx.scale(6_000, 40);
    let git = Arc::new(FakeGit::new());
    for i in 0..n {
        let mut text = synthetic::source("ts", i, 8 << 10);
        text.extend_from_slice(
            format!("\nexport function wanted() {{ return {i}; }}\n").as_bytes(),
        );
        git.add("main", &[(&format!("app/m{}/f{i}.ts", i % 40), &text)]);
    }
    let main = b"export function run() { return wanted(); }\n".to_vec();
    git.add("main", &[("app/main.ts", &main)]);
    git.commit("main");
    let dir = tempfile::tempdir_in(ctx.scratch())?;
    let rig = Rig::new(git, cache(dir.path()), Arc::new(SystemClock));
    // Nothing gets indexed in the background.
    rig.host.kernel().pause();
    rig.open_with(packages(&["app".to_owned()]));
    rig.settle();
    maybe_full(&rig);
    let (seen, _subscription) = listen(&rig)?;
    let query = Query {
        project: rig.git.key(),
        kind: QueryKind::Definition {
            at: text_ref(&rig.git, "app/main.ts", &main),
            position: position(&main, "wanted")?,
        },
    };
    let mut samples = Vec::new();
    for round in 0..slow_samples(ctx) {
        seen.lock().unwrap().clear();
        let start = Instant::now();
        let (id, r) = run(&rig, &query)?;
        let last = seen
            .lock()
            .unwrap()
            .iter()
            .filter(|(i, g, _)| *i == id && *g == r.generation)
            .map(|(_, _, t)| *t)
            .next()
            .ok_or_else(|| BenchError::new("no last batch"))?;
        let partial = matches!(r.state, QueryState::Done(Completeness::Partial { .. }));
        if ctx.mode() == Mode::Measure && !partial {
            return Err(BenchError::new(format!("searched through: {:?}", r.state)));
        }
        if round > 0 || ctx.mode() == Mode::Smoke {
            samples.push(last - start);
        }
    }
    Ok(vec![Measured::new(
        "code_intel/search_stop",
        Value::Time(Samples(samples)),
    )])
}

/// References of a function 300 of 100 000 indexed files use (「性能」:
/// first batch ≤ 200 ms, all ≤ 2 s): `ci_ref` gives the 300, each read and
/// parsed. Two IDs: to the event of the first batch with references, and
/// to the last.
pub const REFERENCES_100K: Bench = Bench {
    id: "code_intel/references_100k",
    run: references_100k,
};

fn references_100k(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let n = ctx.scale(100_000, 3_000);
    let users = ctx.scale(300, 10);
    let git = Arc::new(FakeGit::new());
    let dir = tempfile::tempdir_in(ctx.scratch())?;
    let cache = cache(dir.path());
    virtual_repo(&git, &cache, n, &["main"]);
    let def_path = "packages/pkg000/src/mod00/orders.ts";
    let def = b"export function findOrder(id: number) {\n  return id;\n}\n".to_vec();
    git.add("main", &[(def_path, &def)]);
    for i in 0..users {
        let mut text = synthetic::source("ts", 500_000 + i, 8 << 10);
        text.extend_from_slice(b"\nexport function lookup(id: number) { return findOrder(id); }\n");
        let package = i % synthetic::package_dirs(n).len();
        git.add(
            "main",
            &[(
                &format!("packages/pkg{package:03}/src/users/u{i}.ts"),
                &text,
            )],
        );
    }
    git.commit("main");
    let rig = Rig::new(git, cache, Arc::new(SystemClock));
    rig.open_with(packages(&synthetic::package_dirs(n)));
    rig.settle();
    maybe_full(&rig);
    let (_, r) = run(
        &rig,
        &Query {
            project: rig.git.key(),
            kind: QueryKind::Definition {
                at: text_ref(&rig.git, def_path, &def),
                position: position(&def, "findOrder")?,
            },
        },
    )?;
    let of = r
        .symbol
        .as_ref()
        .and_then(|s| s.defines.clone())
        .ok_or_else(|| BenchError::new("no definition"))?;
    let query = Query {
        project: rig.git.key(),
        kind: QueryKind::References { of },
    };
    let (seen, _subscription) = listen(&rig)?;
    let (mut first, mut all) = (Vec::new(), Vec::new());
    let rounds = match ctx.mode() {
        Mode::Measure => 12,
        Mode::Smoke => 1,
    };
    for round in 0..rounds {
        seen.lock().unwrap().clear();
        let start = Instant::now();
        let (id, r) = run(&rig, &query)?;
        if r.same_name.len() < users {
            return Err(BenchError::new(format!(
                "{} references for {users} users",
                r.same_name.len()
            )));
        }
        let events: Vec<(u32, Instant)> = seen
            .lock()
            .unwrap()
            .iter()
            .filter(|(i, _, _)| *i == id)
            .map(|(_, g, t)| (*g, *t))
            .collect();
        // Generation 1 carries the symbol only; 2 the first references.
        let first_batch = events
            .iter()
            .find(|(g, _)| *g == 2)
            .ok_or_else(|| BenchError::new("no batch"))?
            .1;
        let last = events
            .iter()
            .find(|(g, _)| *g == r.generation)
            .ok_or_else(|| BenchError::new("no last batch"))?
            .1;
        if round > 0 || ctx.mode() == Mode::Smoke {
            first.push(first_batch - start);
            all.push(last - start);
        }
    }
    Ok(vec![
        Measured::new("code_intel/references_100k/all", Value::Time(Samples(all))),
        Measured::new(
            "code_intel/references_100k/first",
            Value::Time(Samples(first)),
        ),
    ])
}

/// Building the ⌘K name table of 100 000 files (≤ 1 s): the current
/// versions' units filtered by the map, then their definition names.
pub const NAMES_100K: Bench = Bench {
    id: "code_intel/names_100k",
    run: names_100k,
};

fn names_100k(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let dir = tempfile::tempdir_in(ctx.scratch())?;
    let f = crate::code_intel_names::names(ctx, dir.path())?;
    let built = build_table(&f.store, &f.map, &f.table, 1, &|| false)
        .ok_or_else(|| BenchError::new("no table"))?;
    eprintln!("code_intel/names_100k: {} names", built.len());
    drop(built);
    let time = ctx.time(|| build_table(&f.store, &f.map, &f.table, 1, &|| false));
    Ok(vec![Measured::new(
        "code_intel/names_100k",
        Value::Time(time),
    )])
}
