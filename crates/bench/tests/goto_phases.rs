//! The per-phase breakdowns of `code_intel/goto_indexed`,
//! `code_intel/named_50k` and `code_intel/names_100k`
//! (docs/design/code_intel.md「实际验证」): the same
//! fixtures, each query with `lectern_code_intel::phases` recording, the
//! medians printed. Profiles, not checks: ignored by default; run them in
//! release:
//!
//! ```sh
//! cargo test --release --locked -p lectern-bench --all-features \
//!     --test goto_phases -- --ignored --nocapture
//! ```
//!
//! `GOTO_PHASES_FILES` sets the repository size (100 000 for the jump,
//! 60 000 for `Named`).

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lectern_bench::CountingAlloc;
use lectern_cache::{Budgets, Cache, Config};
use lectern_code_intel::SystemClock;
use lectern_code_intel::phases;
use lectern_code_intel::testing::{
    FakeGit, Rig, packages, synthetic, virtual_repo, virtual_repo_with,
};
use lectern_services::code_intel::{
    Completeness, Focus, Position, Query, QueryKind, QueryState, TextOrigin, TextRef,
};

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

const WARMUP: usize = 30;
const JUMPS: usize = 300;

fn cache(root: &Path) -> Cache {
    Cache::new(Config {
        root: root.to_path_buf(),
        budgets: Budgets::default(),
        ui_thread: None,
    })
}

fn position(text: &[u8], needle: &str) -> Position {
    let at = text
        .windows(needle.len())
        .position(|w| w == needle.as_bytes())
        .expect("the needle");
    let line = text[..at].iter().filter(|b| **b == b'\n').count() as u32;
    let start = text[..at]
        .iter()
        .rposition(|b| *b == b'\n')
        .map_or(0, |i| i + 1);
    Position {
        line,
        column: (at - start) as u32,
    }
}

fn median<T: Ord + Copy>(mut v: Vec<T>) -> T {
    v.sort_unstable();
    v[v.len() / 2]
}

fn files(default: usize) -> usize {
    std::env::var("GOTO_PHASES_FILES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

#[test]
#[ignore = "a profile; see the module docs"]
fn goto_indexed_phases() {
    goto_phases(false, false, false);
}

/// The same jump as the first one after the index changed: the cached
/// walk of the accounting (「完整性的核算」) forgotten before each jump.
#[test]
#[ignore = "a profile; see the module docs"]
fn goto_indexed_uncached_phases() {
    goto_phases(false, true, false);
}

/// The same jump as the first one in the text: no kept parse (forgotten
/// before each jump), the text read and parsed for the site.
#[test]
#[ignore = "a profile; see the module docs"]
fn goto_indexed_cold_phases() {
    goto_phases(false, false, true);
}

/// The same jump once the namespace is full (`Indexer::set_namespace_full`):
/// hints no longer prove units, so the accounting checks every hinted file
/// against the store (「完整性的核算」); its cost is the `4 accounting`
/// phase and the `verified hints` count.
#[test]
#[ignore = "a profile; see the module docs"]
fn goto_indexed_full_phases() {
    goto_phases(true, false, false);
}

fn goto_phases(full: bool, uncached: bool, cold: bool) {
    let n = files(100_000);
    let git = Arc::new(FakeGit::new());
    let dir = tempfile::tempdir().expect("temp dir");
    let cache = cache(dir.path());
    virtual_repo(&git, &cache, n, &["main"]);
    let path = "packages/pkg000/src/mod00/current.ts";
    let name = synthetic::shared_name();
    // The 「性能」 row's 100 KB, as `code_intel/goto_indexed`.
    let mut text = synthetic::source("ts", 999_999, 100 << 10);
    text.extend_from_slice(
        format!("\nexport function jump(x: number) {{ return {name}(x); }}\n").as_bytes(),
    );
    git.add("main", &[(path, &text)]);
    git.commit("main");
    let rig = Rig::new(git, cache, Arc::new(SystemClock));
    rig.open_with(packages(&synthetic::package_dirs(n)));
    rig.settle();
    if full {
        rig.indexer.set_namespace_full(&rig.git.key());
    }
    let at = TextRef {
        worktree: Some(rig.git.worktree_key("main")),
        path: path.into(),
        oid: rig.git.oid(&text),
        origin: TextOrigin::Worktree,
    };
    // The current text: its parse is kept (ADR 0037), unless `cold`.
    rig.indexer.set_focus(
        &rig.git.key(),
        Some(&rig.git.worktree_key("main")),
        Focus::Current(Some(at.clone())),
    );
    rig.settle();
    let query = Query {
        project: rig.git.key(),
        kind: QueryKind::Definition {
            at,
            position: position(&text, &format!("{name}(x)")),
        },
    };
    let jump = || {
        if uncached {
            rig.indexer.clear_walks(&rig.git.key());
        }
        if cold {
            rig.indexer.clear_kept_parses();
        }
        let id = rig.indexer.query(query.clone());
        rig.executor.run_until_idle();
        let r = rig.indexer.results(id).expect("kept");
        rig.indexer.close(id);
        r
    };
    let r = jump();
    assert!(!r.candidates.is_empty());
    assert_eq!(r.state, QueryState::Done(Completeness::Complete));
    let title = match (full, uncached, cold) {
        (true, _, _) => "goto_indexed phases, namespace full",
        (false, true, _) => "goto_indexed phases, walk not cached",
        (false, false, true) => "goto_indexed phases, parse not kept",
        (false, false, false) => "goto_indexed phases",
    };
    profile(&format!("{title}, {n} files"), JUMPS, jump);
}

/// `code_intel/named_50k`'s breakdown: `Named` for `id`, which every unit
/// of a virtual repository defines.
#[test]
#[ignore = "a profile; see the module docs"]
fn named_50k_phases() {
    let n = files(60_000);
    let git = Arc::new(FakeGit::new());
    let dir = tempfile::tempdir().expect("temp dir");
    let cache = cache(dir.path());
    let units = virtual_repo_with(&git, &cache, n, &["main"], Some("id"));
    git.commit("main");
    let rig = Rig::new(git, cache, Arc::new(SystemClock));
    rig.open_with(packages(&synthetic::package_dirs(n)));
    rig.settle();
    let query = Query {
        project: rig.git.key(),
        kind: QueryKind::Named {
            name: "id".into(),
            near: Some(synthetic::path(0).into()),
        },
    };
    let named = || {
        let id = rig.indexer.query(query.clone());
        rig.executor.run_until_idle();
        let r = rig.indexer.results(id).expect("kept");
        rig.indexer.close(id);
        r
    };
    assert_eq!(named().candidates.len(), units);
    profile(&format!("named_50k phases, {units} definitions"), 20, named);
}

/// `code_intel/names_100k`'s breakdown: the name table of the benchmarks'
/// 100 000-file virtual repository.
#[test]
#[ignore = "a profile; see the module docs"]
fn names_100k_phases() {
    use lectern_code_intel::NoGrammarPacks;
    use lectern_code_intel::extract::LanguageTable;
    use lectern_code_intel::map::{BaseMap, IndexedHints, LiveMap, Overlay, Provenance};
    use lectern_code_intel::names::build_table;
    use lectern_code_intel::store::Store;
    use lectern_code_intel::testing::VirtualTemplate;
    let n = files(100_000);
    let template = VirtualTemplate::build(n, None, Budgets::default()).expect("template");
    let dir = tempfile::tempdir().expect("temp dir");
    let (git, cache) = template
        .open(&dir.path().join("cache"), Budgets::default())
        .expect("a copy");
    let store = Store::new(&cache.repo(git.common_dir()));
    let table = LanguageTable::core();
    let list = Arc::new(synthetic::file_list(n));
    let map = LiveMap {
        hints: Arc::new(IndexedHints::new(list.len())),
        base: Arc::new(BaseMap::build(list, &table, &NoGrammarPacks)),
        overlay: Arc::new(Overlay::empty()),
        provenance: Provenance::default(),
    };
    let names = build_table(&store, &map, &table, 1, &|| false).expect("a table");
    profile(
        &format!("names_100k phases, {} names", names.len()),
        10,
        || build_table(&store, &map, &table, 1, &|| false),
    );
}

/// Runs `op` a few times, then `runs` times with the phases recording;
/// prints the medians.
fn profile<R>(title: &str, runs: usize, op: impl Fn() -> R) {
    for _ in 0..WARMUP.min(runs) {
        op();
    }
    let mut totals = Vec::new();
    let mut allocs = Vec::new();
    let mut phases_by: Vec<(&'static str, Vec<Duration>)> = Vec::new();
    let mut counts_by: Vec<(&'static str, Vec<u64>)> = Vec::new();
    for _ in 0..runs {
        phases::take();
        phases::enable(true);
        let a = CountingAlloc::allocations();
        let start = Instant::now();
        op();
        let took = start.elapsed();
        let a = CountingAlloc::allocations() - a;
        phases::enable(false);
        let log = phases::take();
        totals.push(took);
        allocs.push(a);
        for (name, d) in log.phases {
            match phases_by.iter_mut().find(|(n, _)| *n == name) {
                Some((_, v)) => v.push(d),
                None => phases_by.push((name, vec![d])),
            }
        }
        for (name, c) in log.counts {
            match counts_by.iter_mut().find(|(n, _)| *n == name) {
                Some((_, v)) => v.push(c),
                None => counts_by.push((name, vec![c])),
            }
        }
    }
    println!("{title}, {runs} runs (medians; phases recorded with their own overhead)");
    println!(
        "{:<34} {:>10}",
        "query to results",
        format!("{:.2?}", median(totals))
    );
    for (name, v) in phases_by {
        let n = v.len();
        println!(
            "{name:<34} {:>10}  ({n} runs)",
            format!("{:.2?}", median(v))
        );
    }
    println!(
        "{:<34} {:>10}",
        "allocations (incl. reallocs)",
        median(allocs)
    );
    for (name, v) in counts_by {
        println!("{name:<34} {:>10}", median(v));
    }
}
