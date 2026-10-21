//! The crate's tests, one file per prefix (docs/design/file_reader.md「测试」).

mod consistency;
mod deadline;
mod degrade;
mod health;
mod lang;
mod spans;
mod window;

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use crate::highlight::{Budget, ChainWindow, FullParse, Outcome, Source};
use crate::{Lang, Languages, TokenKind, WindowSpans};

/// A core language by name.
pub(crate) fn lang(name: &str) -> &'static Lang {
    static LANGS: OnceLock<Arc<Languages>> = OnceLock::new();
    LANGS
        .get_or_init(Languages::core)
        .get(name)
        .expect("core language")
}

/// A budget no test runs out of.
pub(crate) fn ample(cancel: &AtomicBool) -> Budget<'_> {
    Budget {
        budget: Duration::from_secs(600),
        cancel,
        yield_probe: None,
    }
}

/// The whole text's tokens through `FullParse` in one step.
pub(crate) fn full(lang: &Lang, text: &[u8]) -> WindowSpans {
    let cancel = AtomicBool::new(false);
    let src: Arc<dyn Source> = Arc::new(text.to_vec());
    match FullParse::new(lang, src).step(ample(&cancel)) {
        Outcome::Done(spans) => spans,
        other => panic!("{}: {other:?}", lang.name()),
    }
}

/// The chain over the whole text, each window in one step: the windows'
/// ranges and tokens.
pub(crate) fn chain(lang: &Lang, text: &[u8]) -> Vec<WindowSpans> {
    let cancel = AtomicBool::new(false);
    let src: Arc<dyn Source> = Arc::new(text.to_vec());
    let mut at = 0;
    let mut windows = Vec::new();
    while at < text.len() as u64 {
        let mut window = ChainWindow::new(lang, src.clone(), at);
        let (spans, next) = match window.step(ample(&cancel)) {
            Outcome::Done(done) => done,
            other => panic!("window at {at}: {other:?}"),
        };
        assert_eq!(spans.range(), at..next, "a window covers up to the next");
        assert!(next > at, "the chain moves on at {at}");
        at = next;
        windows.push(spans);
    }
    windows
}

/// The kind of every byte of `range` (absolute offsets), None where no
/// token covers it.
pub(crate) fn kinds(spans: &[&WindowSpans], range: std::ops::Range<u64>) -> Vec<Option<TokenKind>> {
    let mut out = vec![None; (range.end - range.start) as usize];
    for spans in spans {
        for (line_start, tokens) in spans.lines() {
            for token in tokens {
                for at in line_start + u64::from(token.start())..line_start + u64::from(token.end())
                {
                    if range.contains(&at) {
                        out[(at - range.start) as usize] = Some(token.kind());
                    }
                }
            }
        }
    }
    out
}

/// The non-whitespace bytes of `range` whose kinds differ, as
/// `(offset, got, expected)`; the research's comparison.
pub(crate) fn differences(
    text: &[u8],
    range: std::ops::Range<u64>,
    got: &[Option<TokenKind>],
    expected: &[Option<TokenKind>],
) -> Vec<(u64, Option<TokenKind>, Option<TokenKind>)> {
    (range.start..range.end)
        .zip(got.iter().zip(expected))
        .filter(|(at, (g, e))| !text[*at as usize].is_ascii_whitespace() && g != e)
        .map(|(at, (g, e))| (at, *g, *e))
        .collect()
}

/// A small deterministic generator (xorshift) for test offsets.
pub(crate) struct Rng(u64);

impl Rng {
    pub(crate) fn new(seed: u64) -> Rng {
        Rng(seed.max(1))
    }

    pub(crate) fn below(&mut self, n: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % n.max(1)
    }
}
