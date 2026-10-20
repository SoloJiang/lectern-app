//! `code_intel/*` of the queries and the name table (docs/design/
//! code_intel.md「性能」, step 4): an indexer over a `FakeGit` repository
//! whose 100 000 files are virtual (index entries and stored synthetic
//! extractions, no worktree file) plus the real files a query reads. The
//! 100 000-file benchmarks start from a copy of the shared virtual
//! repository (`code_intel_virtual`); each has its own rig.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lectern_bench::{Bench, BenchError, Ctx, Measured, Mode, Samples, Value};
use lectern_cache::{Budgets, Cache, Config};
use lectern_code_intel::SystemClock;
use lectern_code_intel::names::build_table;
use lectern_code_intel::testing::{
    FakeGit, Rig, packages, synthetic, virtual_repo, virtual_repo_with,
};
use lectern_services::code_intel::{
    CodeIntelEvent, Completeness, Focus, Position, Query, QueryId, QueryKind, QueryState,
    TextOrigin, TextRef,
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

/// The jumps' current text: about 100 KB of TypeScript (the「性能」row's
/// size), ending in a call of `name`.
fn current_text(name: &str) -> Vec<u8> {
    let mut text = synthetic::source("ts", 999_999, 100 << 10);
    text.extend_from_slice(
        format!("\nexport function jump(x: number) {{ return {name}(x); }}\n").as_bytes(),
    );
    text
}

/// The path of the jumps' current text.
const CURRENT: &str = "packages/pkg000/src/mod00/current.ts";

/// How a jump fixture is set up.
#[derive(Clone, Copy, Default)]
struct Shape {
    /// The jump's name is the shared repository's popular one
    /// (`code_intel_virtual::POPULAR`).
    popular: bool,
    /// The project's namespace at its quota (`Indexer::set_namespace_full`).
    full: bool,
    /// The current text read as an object (a diff's old side).
    object: bool,
    /// The jump's name is the current text's own function, which the
    /// current text's locals bind (a `File` definition, step 1).
    same_file: bool,
}

/// A jump fixture: the rig, its query, and the directory its cache lives in.
struct Jump {
    rig: Rig,
    query: Query,
    at: TextRef,
    _dir: tempfile::TempDir,
}

/// A jump the index answers in a copy of the shared virtual repository
/// (`code_intel_virtual`: 100 000 files in sub-projects of 1000, the
/// 「性能」 jump rows): the 100 KB current text indexed and made the current
/// one, so its parse is kept (ADR 0037). The rig, its cache and its
/// in-memory state are this benchmark's own. Returns the number of files.
fn jump_fixture(ctx: &Ctx, shape: Shape) -> Result<(Jump, usize), BenchError> {
    let template = crate::code_intel_virtual::shared(ctx)?;
    let n = template.files();
    let dir = tempfile::tempdir_in(ctx.scratch())?;
    let (git, cache) = crate::code_intel_virtual::open(&template, dir.path())?;
    let name = if shape.popular {
        crate::code_intel_virtual::POPULAR.to_owned()
    } else if shape.same_file {
        "jumpTarget".to_owned()
    } else {
        synthetic::shared_name()
    };
    let mut text = current_text(&name);
    if shape.same_file {
        text.extend_from_slice(format!("function {name}(x: number) {{ return x; }}\n").as_bytes());
    }
    git.add("main", &[(CURRENT, &text)]);
    git.commit("main");
    let rig = Rig::new(git, cache, Arc::new(SystemClock));
    rig.open_with(packages(&synthetic::package_dirs(n)));
    rig.settle();
    if shape.full {
        rig.indexer.set_namespace_full(&rig.git.key());
    } else {
        maybe_full(&rig);
    }
    let mut at = text_ref(&rig.git, CURRENT, &text);
    if shape.object {
        at.origin = TextOrigin::Object;
    }
    rig.indexer.set_focus(
        &rig.git.key(),
        Some(&rig.git.worktree_key("main")),
        Focus::Current(Some(at.clone())),
    );
    rig.settle();
    if rig.indexer.kept_parses().0 != 1 {
        return Err(BenchError::new("the current text's parse is not kept"));
    }
    let query = Query {
        project: rig.git.key(),
        kind: QueryKind::Definition {
            at: at.clone(),
            position: position(&text, &format!("{name}(x)"))?,
        },
    };
    Ok((
        Jump {
            rig,
            query,
            at,
            _dir: dir,
        },
        n,
    ))
}

/// One `Results` event: its query, its time, and whether its results had
/// candidates, and the query's end.
type Batch = (QueryId, Instant, bool, bool);

/// Every `Results` event seen.
type Batches = Arc<Mutex<Vec<Batch>>>;

fn listen_batches(rig: &Rig) -> Result<(Batches, lectern_kernel::Subscription), BenchError> {
    let seen: Batches = Arc::default();
    let s = seen.clone();
    let indexer = rig.indexer.handle();
    let subscription = rig
        .host
        .handle
        .on::<CodeIntelEvent>(move |e, _| {
            if let CodeIntelEvent::Results { id, .. } = e
                && let Some(r) = indexer.results(*id)
            {
                let at = Instant::now();
                let done = matches!(r.state, QueryState::Done(_));
                s.lock()
                    .unwrap()
                    .push((*id, at, !r.candidates.is_empty(), done));
            }
        })
        .ok_or_else(|| BenchError::new("cannot listen"))?;
    Ok((seen, subscription))
}

/// Runs `query`: from the query to its first batch with candidates and
/// to its end, and its results.
fn first_and_done(
    rig: &Rig,
    seen: &Batches,
    query: &Query,
) -> Result<
    (
        Duration,
        Duration,
        Arc<lectern_services::code_intel::QueryResults>,
    ),
    BenchError,
> {
    seen.lock().unwrap().clear();
    let start = Instant::now();
    let (id, r) = run(rig, query)?;
    let events = seen.lock().unwrap().clone();
    let at = |want: &dyn Fn(&Batch) -> bool| {
        events
            .iter()
            .find(|e| e.0 == id && want(e))
            .map(|e| e.1 - start)
    };
    let first = at(&|e| e.2).ok_or_else(|| BenchError::new("no batch with candidates"))?;
    let done = at(&|e| e.3).ok_or_else(|| BenchError::new("no end"))?;
    Ok((first, done, r))
}

/// Samples of a jump: `before` runs untimed before each (forgetting a
/// cache). In measure mode a few warm-up runs, then 40.
fn sample_jumps(
    ctx: &Ctx,
    jump: &Jump,
    before: &dyn Fn(&Rig),
    want: &dyn Fn(&lectern_services::code_intel::QueryResults) -> Result<(), BenchError>,
) -> Result<(Samples, Samples), BenchError> {
    let (seen, _subscription) = listen_batches(&jump.rig)?;
    let (warmup, runs) = match ctx.mode() {
        Mode::Measure => (5, 40),
        Mode::Smoke => (0, 1),
    };
    let (mut firsts, mut dones) = (Vec::new(), Vec::new());
    for round in 0..warmup + runs {
        before(&jump.rig);
        let (first, done, r) = first_and_done(&jump.rig, &seen, &jump.query)?;
        want(&r)?;
        if round >= warmup {
            firsts.push(first);
            dones.push(done);
        }
    }
    Ok((Samples(firsts), Samples(dones)))
}

/// The results a jump of the index must give: candidates, and `Done`.
fn answered(r: &lectern_services::code_intel::QueryResults) -> Result<(), BenchError> {
    if r.candidates.is_empty() || !matches!(r.state, QueryState::Done(_)) {
        return Err(BenchError::new(format!(
            "{} candidates, {:?}",
            r.candidates.len(),
            r.state
        )));
    }
    Ok(())
}

/// [`answered`], and complete.
fn complete(r: &lectern_services::code_intel::QueryResults) -> Result<(), BenchError> {
    answered(r)?;
    if r.state != QueryState::Done(Completeness::Complete) && !full_mode() {
        return Err(BenchError::new(format!("not complete: {:?}", r.state)));
    }
    Ok(())
}

fn full_mode() -> bool {
    std::env::var_os("CODE_INTEL_BENCH_FULL").is_some()
}

fn nothing(_: &Rig) {}

/// A jump answered from the index (「性能」: candidates ≤ 50 ms, this
/// crate's part ≤ 20 ms to the first batch with candidates): 100 000
/// indexed files in sub-projects of 1000, a 100 KB current text whose
/// parse is kept (it is the current one), calling a function some 130
/// files define. The accounting's walk is cached (the same jump again);
/// to the end recorded.
pub const GOTO_INDEXED: Bench = Bench {
    id: "code_intel/goto_indexed",
    run: goto_indexed,
};

fn goto_indexed(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let (jump, _) = jump_fixture(ctx, Shape::default())?;
    let (first, done) = sample_jumps(ctx, &jump, &nothing, &complete)?;
    Ok(vec![
        Measured::new("code_intel/goto_indexed", Value::Time(first)),
        Measured::new("code_intel/goto_indexed/done", Value::Time(done)),
    ])
}

/// The same jump as the first one after the index changed: the
/// accounting's cached walk forgotten before each (untimed). The first
/// batch does not wait for the walk.
pub const GOTO_UNCACHED: Bench = Bench {
    id: "code_intel/goto_uncached",
    run: goto_uncached,
};

fn goto_uncached(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let (jump, _) = jump_fixture(ctx, Shape::default())?;
    let key = jump.rig.git.key();
    let forget = |rig: &Rig| rig.indexer.clear_walks(&key);
    let (first, done) = sample_jumps(ctx, &jump, &forget, &complete)?;
    Ok(vec![
        Measured::new("code_intel/goto_uncached", Value::Time(first)),
        Measured::new("code_intel/goto_uncached/done", Value::Time(done)),
    ])
}

/// A jump to a popular name: one file in 17 of 100 000 defines it, some
/// 3000 of them in the jump's language group (TypeScript, TSX, JavaScript:
/// about half the files), a Zipf-like name such as `get`. The first batch
/// reads one page and the nearest files' definitions; the rest follow.
pub const GOTO_POPULAR: Bench = Bench {
    id: "code_intel/goto_popular",
    run: goto_popular,
};

fn goto_popular(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let shape = Shape {
        popular: true,
        ..Shape::default()
    };
    let (jump, n) = jump_fixture(ctx, shape)?;
    let want = |r: &lectern_services::code_intel::QueryResults| {
        complete(r)?;
        if r.candidates.len() < n * 3 / 100 - n / 400 {
            return Err(BenchError::new(format!(
                "{} candidates",
                r.candidates.len()
            )));
        }
        Ok(())
    };
    let (first, done) = sample_jumps(ctx, &jump, &nothing, &want)?;
    Ok(vec![
        Measured::new("code_intel/goto_popular", Value::Time(first)),
        Measured::new("code_intel/goto_popular/done", Value::Time(done)),
    ])
}

/// The jump of [`GOTO_INDEXED`] with the namespace at its quota (100 000
/// files are some 270 MiB, over the 256 MiB quota: the usual state): hints
/// do not prove units, so the accounting checks the store, within the
/// query's 1 s time box. The first batch does not wait for it; the end
/// comes within the time box.
pub const GOTO_FULL: Bench = Bench {
    id: "code_intel/goto_full",
    run: goto_full,
};

fn goto_full(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let shape = Shape {
        full: true,
        ..Shape::default()
    };
    let (jump, _) = jump_fixture(ctx, shape)?;
    let (first, done) = sample_jumps(ctx, &jump, &nothing, &answered)?;
    Ok(vec![
        Measured::new("code_intel/goto_full", Value::Time(first)),
        Measured::new("code_intel/goto_full/done", Value::Time(done)),
    ])
}

/// The jump of [`GOTO_INDEXED`] with no kept parse (forgotten before each,
/// untimed): the first jump in a text before its parse is warmed, which
/// parses the text for its site. Recorded, not gated (ADR 0037).
pub const GOTO_COLD_PARSE: Bench = Bench {
    id: "code_intel/goto_cold_parse",
    run: goto_cold_parse,
};

fn goto_cold_parse(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let (jump, _) = jump_fixture(ctx, Shape::default())?;
    let forget = |rig: &Rig| rig.indexer.clear_kept_parses();
    let (first, _) = sample_jumps(ctx, &jump, &forget, &complete)?;
    Ok(vec![Measured::new(
        "code_intel/goto_cold_parse",
        Value::Time(first),
    )])
}

/// A jump to the current text's own function, a `File` definition its
/// locals bind (step 1 settles: no index lookup), with the parse kept.
pub const GOTO_SAME_FILE: Bench = Bench {
    id: "code_intel/goto_same_file",
    run: goto_same_file,
};

fn goto_same_file(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let shape = Shape {
        same_file: true,
        ..Shape::default()
    };
    let (jump, _) = jump_fixture(ctx, shape)?;
    let want = |r: &lectern_services::code_intel::QueryResults| {
        complete(r)?;
        if r.jump.is_none() {
            return Err(BenchError::new("no direct jump"));
        }
        Ok(())
    };
    let (first, _) = sample_jumps(ctx, &jump, &nothing, &want)?;
    Ok(vec![Measured::new(
        "code_intel/goto_same_file",
        Value::Time(first),
    )])
}

/// The jump of [`GOTO_INDEXED`] from a diff's old side: the current text
/// read by OID from the object database (`TextOrigin::Object`), its parse
/// kept as the worktree's would be.
pub const GOTO_OBJECT: Bench = Bench {
    id: "code_intel/goto_object",
    run: goto_object,
};

fn goto_object(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let shape = Shape {
        object: true,
        ..Shape::default()
    };
    let (jump, _) = jump_fixture(ctx, shape)?;
    if jump.at.origin != TextOrigin::Object {
        return Err(BenchError::new("not an object's text"));
    }
    let (first, _) = sample_jumps(ctx, &jump, &nothing, &answered)?;
    Ok(vec![Measured::new(
        "code_intel/goto_object",
        Value::Time(first),
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
    references(ctx, false)
}

/// [`REFERENCES_100K`] with the namespace at its quota (the usual state of
/// 100 000 files): the first batch within 200 ms all the same, the
/// accounting's store checks after it.
pub const REFERENCES_FULL: Bench = Bench {
    id: "code_intel/references_full",
    run: references_full,
};

fn references_full(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    references(ctx, true)
}

fn references(ctx: &mut Ctx, full: bool) -> Result<Vec<Measured>, BenchError> {
    let (id_all, id_first) = if full {
        (
            "code_intel/references_full/all",
            "code_intel/references_full/first",
        )
    } else {
        (
            "code_intel/references_100k/all",
            "code_intel/references_100k/first",
        )
    };
    let users = ctx.scale(300, 10);
    let template = crate::code_intel_virtual::shared(ctx)?;
    let n = template.files();
    let dir = tempfile::tempdir_in(ctx.scratch())?;
    let (git, cache) = crate::code_intel_virtual::open(&template, dir.path())?;
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
    if full {
        rig.indexer.set_namespace_full(&rig.git.key());
    } else {
        maybe_full(&rig);
    }
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
        Measured::new(id_all, Value::Time(Samples(all))),
        Measured::new(id_first, Value::Time(Samples(first))),
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
