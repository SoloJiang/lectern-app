//! `syntax/*` timing (docs/design/file_reader.md「性能」): the first screen's
//! plan and window parse, tier A's sliced whole-file parse and tier B's
//! window chain, on the synthetic shapes of `lectern_syntax::testing`. The
//! first read itself is `platform/safe_read_range` (and joins
//! `syntax/first_screen` in file_reader's step 3): here the bytes are
//! sliced from memory.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use lectern_bench::{Bench, BenchError, Ctx, Measured, Samples, Value};
use lectern_syntax::highlight::{self, Budget, ChainWindow, FullParse, Outcome, Source};
use lectern_syntax::limits::{FIRST_WINDOW_DEADLINE, SLICE};
use lectern_syntax::window::{self, Target};
use lectern_syntax::{Lang, Languages, testing};

const MIB: usize = 1 << 20;
/// Random offsets per file (「性能」: 40).
const OFFSETS: usize = 40;
/// Rows of the viewport: a window of about 60 lines.
const ROWS: u32 = 60;

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

/// One first screen at `target`: the plan from the first read's bytes and
/// the window parsed with the 8 ms deadline. A window that times out is
/// counted too (the screen shows plain text at the deadline).
fn first_screen(lang: &Lang, text: &Vec<u8>, target: u64) -> Result<(), BenchError> {
    let size = text.len() as u64;
    let ranges = window::first_read(size, target);
    let around = ranges.last().cloned().unwrap_or(0..0);
    let head = &text[..(size.min(8 << 10)) as usize];
    let plan = window::plan(
        lang.rules(),
        head,
        &text[around.start as usize..around.end as usize],
        around.start,
        size,
        Target::Byte(target),
        ROWS,
    );
    let Some(w) = plan.window.clone() else {
        return Err(BenchError::new(format!(
            "{}: no window at {target}",
            lang.name()
        )));
    };
    let cancel = AtomicBool::new(false);
    let budget = Budget {
        budget: FIRST_WINDOW_DEADLINE,
        cancel: &cancel,
        yield_probe: None,
    };
    match highlight::window(
        lang,
        text as &dyn Source,
        w,
        &plan.json_pad,
        plan.visible,
        budget,
    ) {
        Outcome::Done(_) | Outcome::TimedOut => Ok(()),
        other => Err(BenchError::new(format!(
            "{}: window at {target}: {other:?}",
            lang.name()
        ))),
    }
}

/// `syntax/first_screen`: synthetic TypeScript, Rust and JSON of 1–64 MiB,
/// 40 random offsets each, `window::plan` plus `highlight::window`; the
/// median ≤ 8 ms. `syntax/first_screen/minified`: 4 MiB of JavaScript in
/// one line, record only.
pub const FIRST_SCREEN: Bench = Bench {
    id: "syntax/first_screen",
    run: first_screen_bench,
};

fn first_screen_bench(ctx: &mut Ctx) -> Result<Vec<Measured>, BenchError> {
    let langs = Languages::core();
    let sizes = ctx.scale(vec![MIB, 4 * MIB, 16 * MIB, 64 * MIB], vec![2 * MIB]);
    let offsets = ctx.scale(OFFSETS, 4);
    let rounds = ctx.scale(3, 1);
    let mut samples = Vec::new();
    let mut rng = Rng(0x5eed);
    let mut sample = |lang: &Lang, text: &Vec<u8>, samples: &mut Vec<Duration>| {
        let targets: Vec<u64> = (0..offsets).map(|_| rng.below(text.len() as u64)).collect();
        // Warm the caches and the allocator, untimed.
        for &t in targets.iter().take(3) {
            first_screen(lang, text, t)?;
        }
        for _ in 0..rounds {
            for &t in &targets {
                let start = Instant::now();
                first_screen(lang, text, t)?;
                samples.push(start.elapsed());
            }
        }
        Ok::<(), BenchError>(())
    };
    for (name, make) in [
        ("ts", testing::ts as fn(usize, usize) -> Vec<u8>),
        ("rust", testing::rust),
        ("json", testing::json),
    ] {
        let lang = lang(&langs, name)?;
        for &size in &sizes {
            let text = make(size, 0);
            sample(lang, &text, &mut samples)?;
        }
    }
    let mut minified = Vec::new();
    let js = lang(&langs, "js")?;
    let text = testing::minified_js(ctx.scale(4 * MIB, MIB), 0);
    sample(js, &text, &mut minified)?;
    Ok(vec![
        Measured::new("syntax/first_screen", Value::Time(Samples(samples))),
        Measured::new(
            "syntax/first_screen/minified",
            Value::Time(Samples(minified)),
        ),
    ])
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
