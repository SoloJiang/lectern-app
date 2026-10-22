//! `syntax/*` in bytes (docs/design/file_reader.md「性能」): the temporary
//! memory of one window of the chain, the highlight and text LRU, and an
//! edit session's tree. tree-sitter's allocations go through the counting
//! allocator (`memory.rs`'s `main`).

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use lectern_bench::{Bench, BenchError, CountingAlloc, Ctx, Measured, Value};
use lectern_services::reader::Anchor;
use lectern_syntax::highlight::{Budget, ChainWindow, Outcome, Source};
use lectern_syntax::{Editable, Highlighting, Languages, Tier, testing};

use crate::syntax_engine::{Rig, wait};

/// `syntax/lru_bytes`: the live bytes the engine releases when it is
/// dropped, after 24 tier A files of TypeScript and Rust (about 600 KiB
/// each, every one's text and whole-file spans offered to the LRU) were
/// opened and closed one after another: the LRU at its fullest, plus the
/// engine's few small tables; ≤ 8 MiB.
pub const LRU_BYTES: Bench = Bench {
    id: "syntax/lru_bytes",
    run: lru_bytes,
};

fn lru_bytes(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let langs = Languages::core();
    for name in ["ts", "rust"] {
        let lang = langs
            .get(name)
            .ok_or_else(|| BenchError::new(format!("no language {name}")))?;
        if !lang.prepare() {
            return Err(BenchError::new(format!(
                "the {name} query does not compile"
            )));
        }
    }
    let files = ctx.scale(24, 6);
    let mut samples = Vec::new();
    for round in 0..ctx.scale(3, 1) {
        let dir = ctx.scratch().join(format!("round{round}"));
        std::fs::create_dir_all(&dir).map_err(|e| BenchError::new(e.to_string()))?;
        for n in 0..files {
            let (name, bytes) = if n % 2 == 0 {
                (format!("f{n}.ts"), testing::ts(600 << 10, n * 1000))
            } else {
                (format!("f{n}.rs"), testing::rust(600 << 10, n * 1000))
            };
            std::fs::write(dir.join(name), bytes).map_err(|e| BenchError::new(e.to_string()))?;
        }
        let rig = Rig::new(&dir)?;
        let before = CountingAlloc::live();
        for n in 0..files {
            let name = if n % 2 == 0 {
                format!("f{n}.ts")
            } else {
                format!("f{n}.rs")
            };
            let (doc, notified) = rig.open(rig.target(&name, Anchor::Start));
            wait(&doc, &notified, |s| {
                s.tier == Some(Tier::A) && s.highlight == Highlighting::On && s.oid.is_some()
            })?;
            drop(doc);
            rig.idle()?;
        }
        let filled = CountingAlloc::live();
        if filled < before {
            return Err(BenchError::new("live bytes went down while filling"));
        }
        let counted = rig.engine.lru_bytes();
        if rig.engine.lru_len() < 2 {
            return Err(BenchError::new("the LRU kept almost nothing"));
        }
        let Rig { engine, .. } = rig;
        drop(engine);
        let released = filled.saturating_sub(CountingAlloc::live());
        if released < counted as usize / 2 {
            return Err(BenchError::new(format!(
                "dropping the engine released {released} bytes, the LRU counted {counted}"
            )));
        }
        samples.push(released as u64);
    }
    Ok(vec![Measured::new(
        "syntax/lru_bytes",
        Value::Bytes(samples),
    )])
}

/// `syntax/edit_tree_bytes`: the live bytes an edit session holds once its
/// first parse of a file at tier A's limit is done (1 MiB less 4 KiB of
/// TypeScript and of Rust, no line over 4 KiB), less its buffer's text and
/// spans: the tree and the parser; the largest ≤ 41 MiB (ADR 0034).
/// Dropping the session gives them back.
pub const EDIT_TREE_BYTES: Bench = Bench {
    id: "syntax/edit_tree_bytes",
    run: edit_tree_bytes,
};

fn edit_tree_bytes(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let len = ctx.scale((1 << 20) - (4 << 10), 128 << 10);
    let dir = ctx.scratch().join("edit");
    std::fs::create_dir_all(&dir).map_err(|e| BenchError::new(e.to_string()))?;
    for (name, bytes) in [
        ("a.ts", testing::ts(len, 0)),
        ("a.rs", testing::rust(len, 0)),
    ] {
        std::fs::write(dir.join(name), bytes).map_err(|e| BenchError::new(e.to_string()))?;
    }
    let rig = Rig::new(&dir)?;
    let mut samples = Vec::new();
    for _ in 0..ctx.scale(3, 1) {
        for name in ["a.ts", "a.rs"] {
            let (doc, notified) = rig.open(rig.target(name, Anchor::Start));
            wait(&doc, &notified, |s| {
                s.editable == Editable::Yes && s.highlight == Highlighting::On
            })?;
            rig.idle()?;
            let before = CountingAlloc::live();
            let session = doc
                .edit()
                .map_err(|why| BenchError::new(format!("{name}: not editable: {why:?}")))?;
            let deadline = Instant::now() + Duration::from_secs(120);
            while !session.idle() {
                if Instant::now() > deadline {
                    return Err(BenchError::new(format!(
                        "{name}: the first parse did not end"
                    )));
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            rig.idle()?;
            // Less the buffer's text and its spans: the tree and the parser.
            let others = session.initial_text().len() + session.spans().bytes();
            let held = CountingAlloc::live()
                .saturating_sub(before)
                .saturating_sub(others);
            drop(session);
            rig.idle()?;
            let left = CountingAlloc::live().saturating_sub(before);
            if left > held / 4 {
                return Err(BenchError::new(format!(
                    "{name}: the session held {held} bytes, {left} were left after it ended"
                )));
            }
            samples.push(held as u64);
            drop(doc);
            rig.idle()?;
        }
    }
    Ok(vec![Measured::new(
        "syntax/edit_tree_bytes",
        Value::Bytes(samples),
    )])
}

/// `syntax/window_peak`: the peak of live bytes while one 64 KiB window of
/// the chain is parsed and highlighted (its tree, the query's captures and
/// the tokens), on TypeScript, Rust, JSON and minified JavaScript, a few
/// windows each; the largest peak ≤ 8 MiB. The source is allocated before
/// the count starts.
pub const WINDOW_PEAK: Bench = Bench {
    id: "syntax/window_peak",
    run: window_peak,
};

fn window_peak(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let langs = Languages::core();
    let len = ctx.scale(2 << 20, 256 << 10);
    let starts = ctx.scale(vec![0, 300_000, 900_000, 1_500_000], vec![0, 100_000]);
    let cancel = AtomicBool::new(false);
    let mut samples = Vec::new();
    for (name, make) in [
        ("ts", testing::ts as fn(usize, usize) -> Vec<u8>),
        ("rust", testing::rust),
        ("json", testing::json),
        ("js", testing::minified_js),
    ] {
        let lang = langs
            .get(name)
            .ok_or_else(|| BenchError::new(format!("no language {name}")))?;
        if !lang.prepare() {
            return Err(BenchError::new(format!(
                "the {name} query does not compile"
            )));
        }
        let src: Arc<dyn Source> = Arc::new(make(len, 0));
        for &start in &starts {
            let before = CountingAlloc::live();
            CountingAlloc::reset_peak();
            let mut window = ChainWindow::new(lang, src.clone(), start);
            let budget = Budget {
                budget: Duration::from_secs(60),
                cancel: &cancel,
                yield_probe: None,
            };
            let spans = match window.step(budget) {
                Outcome::Done((spans, _)) => spans,
                other => {
                    return Err(BenchError::new(format!(
                        "{name} window at {start}: {other:?}"
                    )));
                }
            };
            let peak = CountingAlloc::peak().saturating_sub(before);
            drop((spans, window));
            samples.push(peak as u64);
        }
    }
    Ok(vec![Measured::new(
        "syntax/window_peak",
        Value::Bytes(samples),
    )])
}
