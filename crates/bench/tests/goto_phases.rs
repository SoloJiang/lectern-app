//! The per-phase breakdown of `code_intel/goto_indexed`
//! (docs/design/code_intel.md「实际验证」): the same fixture, each jump with
//! `lectern_code_intel::phases` recording, the medians printed. A profile,
//! not a check: ignored by default; run it in release:
//!
//! ```sh
//! cargo test --release --locked -p lectern-bench --all-features \
//!     --test goto_phases -- --ignored --nocapture
//! ```
//!
//! `GOTO_PHASES_FILES` sets the repository size (100 000).

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lectern_bench::CountingAlloc;
use lectern_cache::{Budgets, Cache, Config};
use lectern_code_intel::SystemClock;
use lectern_code_intel::phases;
use lectern_code_intel::testing::{FakeGit, Rig, packages, synthetic, virtual_repo};
use lectern_services::code_intel::{
    Completeness, Position, Query, QueryKind, QueryState, TextOrigin, TextRef,
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

#[test]
#[ignore = "a profile; see the module docs"]
fn goto_indexed_phases() {
    let n: usize = std::env::var("GOTO_PHASES_FILES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(100_000);
    let git = Arc::new(FakeGit::new());
    let dir = tempfile::tempdir().expect("temp dir");
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
    let query = Query {
        project: rig.git.key(),
        kind: QueryKind::Definition {
            at: TextRef {
                worktree: Some(rig.git.worktree_key("main")),
                path: path.into(),
                oid: rig.git.oid(&text),
                origin: TextOrigin::Worktree,
            },
            position: position(&text, &format!("{name}(x)")),
        },
    };
    let jump = || {
        let id = rig.indexer.query(query.clone());
        rig.executor.run_until_idle();
        let r = rig.indexer.results(id).expect("kept");
        rig.indexer.close(id);
        r
    };
    let r = jump();
    assert!(!r.candidates.is_empty());
    assert_eq!(r.state, QueryState::Done(Completeness::Complete));
    for _ in 0..WARMUP {
        jump();
    }
    let mut totals = Vec::new();
    let mut allocs = Vec::new();
    let mut phases_by: Vec<(&'static str, Vec<Duration>)> = Vec::new();
    let mut counts_by: Vec<(&'static str, Vec<u64>)> = Vec::new();
    for _ in 0..JUMPS {
        phases::take();
        phases::enable(true);
        let a = CountingAlloc::allocations();
        let start = Instant::now();
        jump();
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
    println!(
        "goto_indexed phases, {n} files, {JUMPS} jumps (medians; phases recorded with their own overhead)"
    );
    println!(
        "{:<34} {:>10}",
        "jump, query to results",
        format!("{:.2?}", median(totals))
    );
    for (name, v) in phases_by {
        let runs = v.len();
        println!(
            "{name:<34} {:>10}  ({runs} jumps)",
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
