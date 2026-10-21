//! `syntax/*` timing (docs/design/file_reader.md「性能」): the first screen
//! (open, the one `read_ranges`, the plan and the window's parse) on files
//! and on object database content, tier A's sliced whole-file parse, tier
//! B's window chain, tier C's streaming scan and find, on the synthetic
//! shapes of `lectern_syntax::testing`.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use lectern_bench::{Bench, BenchError, Ctx, Measured, Samples, Value};
use lectern_services::project::BlobId;
use lectern_services::reader::Anchor;
use lectern_syntax::highlight::{Budget, ChainWindow, FullParse, Outcome, Source};
use lectern_syntax::limits::SLICE;
use lectern_syntax::{DocState, FindQuery, Highlighting, Lang, Languages, testing};

use crate::syntax_engine::{Rig, VIEW, wait};

const MIB: usize = 1 << 20;
/// Random offsets per file (「性能」: 40).
const OFFSETS: usize = 40;

fn lang<'a>(langs: &'a Languages, name: &str) -> Result<&'a Lang, BenchError> {
    let lang = langs
        .get(name)
        .ok_or_else(|| BenchError::new(format!("no language {name}")))?;
    // Compiling the query is not part of the first screen (「首帧之前」:
    // the first open of a language compiles it, outside the budget).
    if !lang.prepare() {
        return Err(BenchError::new(format!(
            "the {name} query does not compile"
        )));
    }
    Ok(lang)
}

/// A small deterministic generator (xorshift) for the offsets.
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % n.max(1)
    }
}

fn byte(at: u64) -> Anchor {
    Anchor::Byte {
        byte: at,
        line: 0,
        column: 0,
    }
}

fn write(path: &std::path::Path, bytes: &[u8]) -> Result<(), BenchError> {
    std::fs::write(path, bytes).map_err(|e| BenchError::new(format!("{}: {e}", path.display())))
}

/// One first screen of a file at `at`, through `Engine::first_screen`: what
/// the first task of `Engine::open` does, on this thread.
fn first_screen(rig: &Rig, path: &str, at: u64) -> Result<(), BenchError> {
    let s = rig
        .engine
        .first_screen(rig.target(path, byte(at)), rig.root.clone(), VIEW);
    if s.state != DocState::Ready {
        return Err(BenchError::new(format!("{path} at {at}: {:?}", s.state)));
    }
    Ok(())
}

/// `syntax/first_screen`: synthetic TypeScript, Rust and JSON of 1–64 MiB,
/// 40 random offsets each: open, the one `read_ranges`, the plan and the
/// window's parse and query with the 8 ms deadline; the median ≤ 8 ms.
/// `syntax/first_screen/minified`: 4 MiB of JavaScript in one line, record
/// only. The object database's first screens (`object_bench`) are measured
/// here too: their IDs are under this one.
pub const FIRST_SCREEN: Bench = Bench {
    id: "syntax/first_screen",
    run: first_screen_bench,
};

fn first_screen_bench(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let langs = Languages::core();
    let rig = Rig::new(ctx.scratch())?;
    let sizes = ctx.scale(vec![MIB, 4 * MIB, 16 * MIB, 64 * MIB], vec![2 * MIB]);
    let offsets = ctx.scale(OFFSETS, 4);
    let rounds = ctx.scale(3, 1);
    let mut samples = Vec::new();
    let mut rng = Rng(0x5eed);
    let mut sample = |path: &str, size: usize, samples: &mut Vec<Duration>| {
        let targets: Vec<u64> = (0..offsets).map(|_| rng.below(size as u64)).collect();
        // Warm the page cache, the root and the allocator, untimed.
        for &t in targets.iter().take(3) {
            first_screen(&rig, path, t)?;
        }
        for _ in 0..rounds {
            for &t in &targets {
                let start = Instant::now();
                first_screen(&rig, path, t)?;
                samples.push(start.elapsed());
            }
        }
        Ok::<(), BenchError>(())
    };
    for (name, ext, make) in [
        ("ts", "ts", testing::ts as fn(usize, usize) -> Vec<u8>),
        ("rust", "rs", testing::rust),
        ("json", "json", testing::json),
    ] {
        lang(&langs, name)?;
        for &size in &sizes {
            // One file on disk at a time.
            let path = format!("fixture.{ext}");
            let text = make(size, 0);
            write(&rig.root.join(&path), &text)?;
            sample(&path, text.len(), &mut samples)?;
            drop(text);
        }
    }
    let mut minified = Vec::new();
    lang(&langs, "js")?;
    let text = testing::minified_js(ctx.scale(4 * MIB, MIB), 0);
    write(&rig.root.join("app.min.js"), &text)?;
    sample("app.min.js", text.len(), &mut minified)?;
    drop(text);
    let mut out = vec![
        Measured::new("syntax/first_screen", Value::Time(Samples(samples))),
        Measured::new(
            "syntax/first_screen/minified",
            Value::Time(Samples(minified)),
        ),
    ];
    out.extend(object_bench(ctx, &rig)?);
    Ok(out)
}

fn blob_id(oid: &lectern_git::Oid) -> Result<BlobId, BenchError> {
    BlobId::from_bytes(oid.as_bytes()).ok_or_else(|| BenchError::new("an OID of 20 or 32 bytes"))
}

/// The first screen of an object target in the middle of `len` bytes.
fn object_screen(
    rig: &Rig,
    common_dir: &std::path::Path,
    oid: BlobId,
    len: usize,
) -> Result<Duration, BenchError> {
    let start = Instant::now();
    let s = rig.engine.first_screen(
        rig.object(common_dir, oid, byte(len as u64 / 2)),
        rig.root.clone(),
        VIEW,
    );
    let elapsed = start.elapsed();
    if s.state != DocState::Ready || s.highlight == Highlighting::Plain {
        return Err(BenchError::new(format!(
            "object: {:?}, {:?}",
            s.state, s.highlight
        )));
    }
    Ok(elapsed)
}

/// `syntax/first_screen/object_shared`: a 4 MiB TypeScript blob the review
/// registered (`share_object`), the first screen at its middle: no object
/// read; the median ≤ 8 ms. `syntax/first_screen/object_1mib`: a 1 MiB blob
/// nobody holds, taken out of the object database on the first screen's
/// path; the median ≤ 8 ms. `syntax/first_screen/object_cold`: a 4 MiB blob
/// nobody holds (ADR 0042's exception), record only.
/// Measured as part of [`FIRST_SCREEN`], under whose ID they are.
fn object_bench(ctx: &mut Ctx, rig: &Rig) -> Result<Vec<Measured>, BenchError> {
    let langs = Languages::core();
    lang(&langs, "ts")?;
    let repo = lectern_git::testing::TestRepo::new();
    let common = repo.common_dir().to_path_buf();
    let rounds = ctx.scale(40, 4);
    let measure = |len: usize, share: bool| -> Result<Vec<Duration>, BenchError> {
        let bytes = testing::ts(len, 0);
        let len = bytes.len();
        let oid = blob_id(&repo.blob(&bytes))?;
        let shared: Arc<[u8]> = bytes.into();
        let _share = share.then(|| rig.engine.share_object(oid, shared.clone()));
        // Discovering the repository and the page cache, untimed.
        object_screen(rig, &common, oid, len)?;
        (0..rounds)
            .map(|_| object_screen(rig, &common, oid, len))
            .collect()
    };
    let shared = measure(ctx.scale(4 * MIB, MIB), true)?;
    let inline = measure(MIB - (4 << 10), false)?;
    let cold = measure(ctx.scale(4 * MIB, MIB), false)?;
    Ok(vec![
        Measured::new(
            "syntax/first_screen/object_shared",
            Value::Time(Samples(shared)),
        ),
        Measured::new(
            "syntax/first_screen/object_1mib",
            Value::Time(Samples(inline)),
        ),
        Measured::new(
            "syntax/first_screen/object_cold",
            Value::Time(Samples(cold)),
        ),
    ])
}

/// `syntax/scan_64mib`: tier C's streaming scan of 64 MiB of TypeScript,
/// from `Engine::open` to the content's OID and exact line count (1 MiB
/// reads, the blob hash and the line index in one pass). Record only.
pub const SCAN_64MIB: Bench = Bench {
    id: "syntax/scan_64mib",
    run: scan_64mib,
};

fn scan_64mib(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let langs = Languages::core();
    lang(&langs, "ts")?;
    let rig = Rig::new(ctx.scratch())?;
    let text = testing::ts(ctx.scale(64 * MIB, 20 * MIB), 0);
    write(&rig.root.join("huge.ts"), &text)?;
    drop(text);
    let mut samples = Vec::new();
    for _ in 0..ctx.scale(5, 1) {
        let start = Instant::now();
        let (doc, notified) = rig.open(rig.target("huge.ts", Anchor::Start));
        wait(&doc, &notified, |s| s.oid.is_some())?;
        samples.push(start.elapsed());
        drop(doc);
        rig.idle()?;
    }
    Ok(vec![Measured::new(
        "syntax/scan_64mib",
        Value::Time(Samples(samples)),
    )])
}

/// `syntax/find_16mib`: find in 16 MiB of TypeScript held as a `Text`
/// (ASCII letters folded, 1 MiB blocks), from `find` to the last results.
/// Record only.
pub const FIND_16MIB: Bench = Bench {
    id: "syntax/find_16mib",
    run: find_16mib,
};

fn find_16mib(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let rig = Rig::new(ctx.scratch())?;
    let text = testing::ts(ctx.scale(16 * MIB - (64 << 10), 2 * MIB), 0);
    write(&rig.root.join("big.ts"), &text)?;
    drop(text);
    let (doc, notified) = rig.open(rig.target("big.ts", Anchor::Start));
    wait(&doc, &notified, |s| s.oid.is_some())?;
    rig.idle()?;
    let mut samples = Vec::new();
    for round in 0..ctx.scale(10, 2) {
        // A new query each time: `Service1` and a digit.
        let query = FindQuery {
            text: Arc::from(format!("service1{}", round % 10)),
        };
        let start = Instant::now();
        doc.find(query);
        let s = wait(&doc, &notified, |s| s.find.as_ref().is_some_and(|f| f.done))?;
        samples.push(start.elapsed());
        if s.find.as_ref().is_some_and(|f| f.ranges.is_empty()) {
            return Err(BenchError::new("find found nothing"));
        }
    }
    Ok(vec![Measured::new(
        "syntax/find_16mib",
        Value::Time(Samples(samples)),
    )])
}

/// `syntax/full_parse_1mib`: tier A's whole-file parse of 1 MiB of
/// TypeScript in 8 ms slices, from the first step to the tokens. Record
/// only.
pub const FULL_PARSE_1MIB: Bench = Bench {
    id: "syntax/full_parse_1mib",
    run: full_parse_1mib,
};

fn full_parse_1mib(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let langs = Languages::core();
    let ts = lang(&langs, "ts")?;
    let src: Arc<dyn Source> = Arc::new(testing::ts(ctx.scale(MIB - (4 << 10), 64 << 10), 0));
    let cancel = AtomicBool::new(false);
    let mut failed = None;
    let samples = ctx.time_with_setup(
        || FullParse::new(ts, src.clone()),
        |mut full| loop {
            let budget = Budget {
                budget: SLICE,
                cancel: &cancel,
                yield_probe: None,
            };
            match full.step(budget) {
                Outcome::Done(spans) => return Some(spans),
                Outcome::TimedOut => {}
                other => {
                    failed = Some(format!("{other:?}"));
                    return None;
                }
            }
        },
    );
    if let Some(failed) = failed {
        return Err(BenchError::new(format!("full parse: {failed}")));
    }
    Ok(vec![Measured::new(
        "syntax/full_parse_1mib",
        Value::Time(samples),
    )])
}

/// `syntax/chain_16mib`: tier B's whole window chain over 16 MiB of
/// TypeScript, each window in steps of 50 ms (the batch task's limit).
/// Record only.
pub const CHAIN_16MIB: Bench = Bench {
    id: "syntax/chain_16mib",
    run: chain_16mib,
};

fn chain_16mib(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let langs = Languages::core();
    let ts = lang(&langs, "ts")?;
    let text = testing::ts(ctx.scale(16 * MIB - (64 << 10), 2 * MIB), 0);
    let len = text.len() as u64;
    let src: Arc<dyn Source> = Arc::new(text);
    let cancel = AtomicBool::new(false);
    let mut samples = Vec::new();
    for _ in 0..ctx.scale(3, 1) {
        let start = Instant::now();
        let mut at = 0;
        while at < len {
            let mut window = ChainWindow::new(ts, src.clone(), at);
            at = loop {
                let budget = Budget {
                    budget: Duration::from_millis(50),
                    cancel: &cancel,
                    yield_probe: None,
                };
                match window.step(budget) {
                    Outcome::Done((_, next)) => break next,
                    Outcome::TimedOut => {}
                    other => return Err(BenchError::new(format!("window at {at}: {other:?}"))),
                }
            };
        }
        samples.push(start.elapsed());
    }
    Ok(vec![Measured::new(
        "syntax/chain_16mib",
        Value::Time(Samples(samples)),
    )])
}
