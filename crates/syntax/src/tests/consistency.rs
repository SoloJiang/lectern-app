//! `consistency_*`: the first screen's window and the window chain against
//! one parse of the whole file, byte by byte (the research's method: only
//! non-whitespace bytes, each byte's innermost capture). On the synthetic
//! shapes both agree everywhere.

use std::sync::atomic::AtomicBool;

use crate::highlight::{Outcome, one_shot, window};
use crate::testing;
use crate::window::{Target, first_read, plan};
use crate::{Lang, WindowSpans};

use super::{Rng, ample, chain, differences, kinds, lang};

/// Just over tier A, so the first read is two ranges.
const SIZE: usize = (1 << 20) + (96 << 10);
const OFFSETS: usize = 40;
const ROWS: u32 = 60;

/// The first screen at `target`: the first read's bytes, the plan and the
/// window's tokens.
pub(crate) fn first_screen(
    lang: &Lang,
    text: &[u8],
    target: u64,
) -> (crate::window::Plan, Option<WindowSpans>) {
    let size = text.len() as u64;
    let ranges = first_read(size, target);
    let head = &text[..(size.min(8 << 10)) as usize];
    let around = ranges.last().unwrap().clone();
    let plan = plan(
        lang.rules(),
        head,
        &text[around.start as usize..around.end as usize],
        around.start,
        size,
        Target::Byte(target),
        ROWS,
    );
    let src = text.to_vec();
    let cancel = AtomicBool::new(false);
    let spans = plan.window.clone().map(|w| {
        match window(
            lang,
            &src,
            w,
            &plan.json_pad,
            plan.visible.clone(),
            ample(&cancel),
        ) {
            Outcome::Done(spans) => spans,
            other => panic!("{}: window at {target}: {other:?}", lang.name()),
        }
    });
    (plan, spans)
}

/// One line: the JSON shape with its line breaks and indentation dropped.
fn one_line(text: &[u8]) -> Vec<u8> {
    text.split(|&b| b == b'\n')
        .flat_map(|line| line.iter().skip_while(|b| **b == b' ').copied())
        .collect()
}

fn first_screens_agree(name: &str, text: &[u8]) {
    let lang = lang(name);
    let whole = one_shot(lang, text);
    let size = text.len() as u64;
    let expected = kinds(&[&whole], 0..size);
    let mut rng = Rng::new(size ^ 0x5eed);
    let mut targets: Vec<u64> = (0..OFFSETS).map(|_| rng.below(size)).collect();
    targets.extend([0, size / 2, size - 1, size]);
    for target in targets {
        let (plan, spans) = first_screen(lang, text, target);
        let spans = spans.unwrap_or_else(|| panic!("{name}: no window at {target}: {plan:?}"));
        assert!(!spans.exact());
        assert_eq!(spans.range(), plan.visible);
        let visible = plan.visible.clone();
        let got = kinds(&[&spans], visible.clone());
        let expected = &expected[visible.start as usize..visible.end as usize];
        let diff = differences(text, visible.clone(), &got, expected);
        assert!(
            diff.is_empty(),
            "{name} at {target} ({plan:?}): {} bytes differ, first {:?}",
            diff.len(),
            &diff[..diff.len().min(5)]
        );
    }
}

#[test]
fn consistency_first_screen_ts() {
    first_screens_agree("ts", &testing::ts(SIZE, 0));
}

#[test]
fn consistency_first_screen_rust() {
    first_screens_agree("rust", &testing::rust(SIZE, 0));
}

#[test]
fn consistency_first_screen_json() {
    first_screens_agree("json", &testing::json(SIZE, 0));
}

/// One line of JSON: windows start at members and are padded with the
/// brackets around them.
#[test]
fn consistency_first_screen_json_one_line() {
    first_screens_agree("json", &one_line(&testing::json(SIZE, 0)));
}

/// One line of minified JavaScript: windows start after `;` or `}`.
#[test]
fn consistency_first_screen_minified() {
    first_screens_agree("js", &testing::minified_js(SIZE, 0));
}

fn chain_agrees(name: &str, text: &[u8]) {
    let lang = lang(name);
    let whole = one_shot(lang, text);
    let size = text.len() as u64;
    let windows = chain(lang, text);
    assert!(windows.len() > 1, "{name}: one window");
    for pair in windows.windows(2) {
        assert_eq!(pair[0].range().end, pair[1].range().start);
    }
    assert_eq!(windows.last().unwrap().range().end, size);
    for w in &windows {
        assert!(w.exact());
        // About 64 KiB; never widened on these shapes.
        assert!(
            w.range().end - w.range().start <= 64 << 10,
            "{:?}",
            w.range()
        );
    }
    let refs: Vec<&WindowSpans> = windows.iter().collect();
    let got = kinds(&refs, 0..size);
    let expected = kinds(&[&whole], 0..size);
    let diff = differences(text, 0..size, &got, &expected);
    assert!(
        diff.is_empty(),
        "{name}: {} bytes differ, first {:?}",
        diff.len(),
        &diff[..diff.len().min(5)]
    );
}

#[test]
fn consistency_chain_ts() {
    chain_agrees("ts", &testing::ts(SIZE, 0));
}

#[test]
fn consistency_chain_rust() {
    chain_agrees("rust", &testing::rust(SIZE, 0));
}

#[test]
fn consistency_chain_json() {
    chain_agrees("json", &testing::json(SIZE, 0));
}

/// One line of JSON: each window is cut after its last comma and padded.
#[test]
fn consistency_chain_json_one_line() {
    chain_agrees("json", &one_line(&testing::json(SIZE, 0)));
}

/// One minified line: inside the `ERROR` node that holds the windows'
/// statements, a statement ending the window's last line is not a cut (an
/// automatic semicolon closes `return` before ` f(x)}` there).
#[test]
fn consistency_chain_minified() {
    chain_agrees("js", &testing::minified_js(SIZE, 0));
}
