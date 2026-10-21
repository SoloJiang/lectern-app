//! `goto_large_*` (「大文件」「跳转里的大文件」, ADR 0034): a jump in a
//! current text of 1–16 MiB reads only the checkpoint's range around the
//! target and parses only the window around it; its identity and
//! checkpoints are recorded once (the warming, or the first jump); past the
//! 8 ms the name goes on unbound; the text's other definitions come from its
//! window units; an object's text reuses the kept window parse.

use std::ops::Range;
use std::sync::atomic::Ordering;
use std::time::{Duration, SystemTime};

use lectern_services::code_intel::{
    Binding, CandidateSource, CandidateTier, Completeness, Evidence, QueryError, QueryKind,
    QueryState, TextOrigin, TextRef,
};

use super::support::{Harness, pos_in};
use super::window::{big, opened};
use crate::query::large::{
    AHEAD, BACK, BACK_MORE, CHECKPOINT, READS, checkpoints, ident_at, safe_start,
};
use crate::window::ident;

/// A 1.2 MiB TypeScript file whose last line calls a function defined
/// near its start.
fn fixture() -> String {
    let mut text = big("ts", 1_200_000);
    text.push_str("export const last = build_10(1);\n");
    text
}

/// The ranges read from large texts on this thread since the last call.
fn reads() -> Vec<Range<u64>> {
    READS.with_borrow_mut(std::mem::take)
}

/// The sources parsed on this thread since the last call, by length.
fn parsed_lens() -> Vec<usize> {
    crate::site::PARSED
        .with_borrow_mut(std::mem::take)
        .iter()
        .map(|s| s.len())
        .collect()
}

/// At most what a jump reads around a checkpoint: the 64 KiB after it, the
/// window's margins before it and after the target.
const AROUND: u64 = CHECKPOINT + (BACK + BACK_MORE + AHEAD) as u64 + (4 << 10);
/// The longest window: from a safe start before the target to past it.
const LONGEST_WINDOW: usize = BACK + BACK_MORE + AHEAD;

fn jump(h: &Harness, at: TextRef, text: &str, needle: &str, offset: u32) -> QueryResultsRef {
    let mut position = pos_in(text.as_bytes(), needle, 0);
    position.column += offset;
    h.ask(QueryKind::Definition { at, position })
}

type QueryResultsRef = std::sync::Arc<lectern_services::code_intel::QueryResults>;

/// The parameter `value` of `build_<k>`: (line, column, byte).
fn parameter(text: &str, k: usize) -> (u32, u32, u64) {
    let needle = format!("build_{k}(value");
    let at = pos_in(text.as_bytes(), &needle, 0);
    let byte = text.find(&needle).unwrap() + needle.len() - "value".len();
    (at.line, at.column + needle.len() as u32 - 5, byte as u64)
}

fn assert_done(r: &lectern_services::code_intel::QueryResults) {
    assert!(matches!(r.state, QueryState::Done(_)), "{:?}", r.state);
}

/// The body's `value` of `build_<k>`, bound to the parameter.
fn assert_bound_to_parameter(r: &QueryResultsRef, text: &str, k: usize) {
    assert_done(r);
    let c = &r.candidates[0];
    assert_eq!(c.evidence, Evidence::Bound(Binding::Local));
    let (line, column, byte) = parameter(text, k);
    let l = &c.definition.location;
    assert_eq!((l.line, l.column, l.byte), (line, column, Some(byte)));
}

#[test]
fn goto_large_window_only() {
    let text = fixture();
    // Opened: the warming recorded its identity and checkpoints.
    let h = opened("big.ts", text.as_bytes());
    reads();
    parsed_lens();
    let r = jump(&h, h.text("big.ts"), &text, "value) + 6000;", 0);
    assert_bound_to_parameter(&r, &text, 6000);
    // One range around the target's checkpoint, nothing streamed.
    let read = reads();
    assert_eq!(read.len(), 1, "{read:?}");
    assert!(read[0].end - read[0].start <= AROUND, "{read:?}");
    // Only the window, never the whole text.
    let lens = parsed_lens();
    assert_eq!(lens.len(), 1, "{lens:?}");
    assert!(lens[0] <= LONGEST_WINDOW, "{lens:?}");
    // Again in the same window: the kept window parse, nothing read.
    let r = jump(&h, h.text("big.ts"), &text, "value) + 6000;", 2);
    assert_bound_to_parameter(&r, &text, 6000);
    assert!(reads().is_empty());
    assert!(parsed_lens().is_empty());
}

#[test]
fn goto_large_finds_the_rest_in_window_units() {
    let text = fixture();
    let h = opened("big.ts", text.as_bytes());
    // `build_10` is a megabyte before the window: its window unit (step 3).
    let r = jump(&h, h.text("big.ts"), &text, "build_10(1)", 0);
    assert_done(&r);
    let def = pos_in(text.as_bytes(), "build_10(value", 0);
    let c = r
        .candidates
        .iter()
        .find(|c| c.definition.location.line == def.line)
        .expect("the window unit's definition");
    assert_eq!(c.tier, CandidateTier::CurrentFile);
    assert_eq!(c.source, CandidateSource::Index);
    assert_eq!(c.definition.location.column, def.column);
    let byte = c.definition.location.byte.expect("canonical") as usize;
    assert!(text.as_bytes()[byte..].starts_with(b"build_10(value"));
    // The only one, by name; the chain is complete, so the text counts as
    // searched.
    assert_eq!(r.candidates.len(), 1, "{:#?}", r.candidates);
    assert_eq!(r.preselect, Some(0));
    assert_eq!(r.state, QueryState::Done(Completeness::Complete));
}

#[test]
fn goto_large_timeout_falls_back_to_the_name() {
    let text = fixture();
    let h = opened("big.ts", text.as_bytes());
    h.indexer
        .inner()
        .window_parse_times_out
        .store(true, Ordering::Release);
    // In the middle of the name: the identifier around the target.
    let r = jump(&h, h.text("big.ts"), &text, "build_10(1)", 3);
    assert_done(&r);
    assert_eq!(&*r.symbol.as_ref().expect("a symbol").name, "build_10");
    assert!(
        r.candidates
            .iter()
            .all(|c| !matches!(c.evidence, Evidence::Bound(_)))
    );
    // Steps 2 and 3 went on with the name: the window unit's definition.
    let def = pos_in(text.as_bytes(), "build_10(value", 0);
    assert!(
        r.candidates
            .iter()
            .any(|c| c.definition.location.line == def.line
                && c.tier == CandidateTier::CurrentFile)
    );
    // A local name has no definition by name: nothing, but no failure.
    let r = jump(&h, h.text("big.ts"), &text, "value) + 6000;", 0);
    assert_done(&r);
    assert_eq!(&*r.symbol.as_ref().expect("a symbol").name, "value");
}

#[test]
fn goto_large_cold_then_warm() {
    let text = fixture();
    let h = Harness::new();
    h.git.add("main", &[("big.ts", text.as_bytes())]);
    h.git.commit("main");
    h.open();
    h.settle();
    reads();
    // Never current: no warming, the first jump streams the text.
    let r = jump(&h, h.text("big.ts"), &text, "value) + 6000;", 0);
    assert_bound_to_parameter(&r, &text, 6000);
    let read = reads();
    let (streamed, window) = read.split_at(read.len() - 1);
    let mut at = 0;
    for range in streamed {
        assert_eq!(range.start, at);
        assert!(range.end - range.start <= 1 << 20);
        at = range.end;
    }
    assert_eq!(at, text.len() as u64, "streamed whole, by 1 MiB ranges");
    assert!(window[0].end - window[0].start <= AROUND);
    // The next jump elsewhere reads only around its checkpoint.
    let r = jump(&h, h.text("big.ts"), &text, "value) + 100;", 0);
    assert_bound_to_parameter(&r, &text, 100);
    let read = reads();
    assert_eq!(read.len(), 1, "{read:?}");
    assert!(read[0].end - read[0].start <= AROUND);
    // No chain ran (never opened): the text's other definitions are not
    // known, and the answer says so.
    let r = jump(&h, h.text("big.ts"), &text, "build_10(1)", 0);
    assert!(r.candidates.is_empty(), "{:#?}", r.candidates);
    assert!(
        matches!(r.state, QueryState::Done(Completeness::Partial { .. })),
        "{:?}",
        r.state
    );
}

#[test]
fn goto_large_racy_record_is_streamed_again() {
    let text = fixture();
    let h = Harness::new();
    h.git.add("main", &[("big.ts", text.as_bytes())]);
    h.git.commit("main");
    h.open();
    let meta = std::fs::metadata(h.git.root("main").join("big.ts")).unwrap();
    let ctime = {
        use std::os::unix::fs::MetadataExt;
        SystemTime::UNIX_EPOCH + Duration::new(meta.ctime() as u64, meta.ctime_nsec() as u32)
    };
    // Recorded within two seconds of the file's status change: the record
    // stands for nothing, and every jump streams it again.
    h.indexer
        .set_wall_clock(move || ctime + Duration::from_secs(1));
    h.settle();
    reads();
    let streams = |read: &[Range<u64>]| {
        read.iter()
            .filter(|r| r.start == 0 && r.end - r.start > AROUND)
            .count()
    };
    let r = jump(&h, h.text("big.ts"), &text, "value) + 6000;", 0);
    assert_bound_to_parameter(&r, &text, 6000);
    assert_eq!(streams(&reads()), 1);
    let r = jump(&h, h.text("big.ts"), &text, "value) + 100;", 0);
    assert_bound_to_parameter(&r, &text, 100);
    assert_eq!(streams(&reads()), 1, "a racy record is not trusted");
    // Later, streamed once more and then trusted.
    h.indexer
        .set_wall_clock(move || ctime + Duration::from_secs(3));
    let r = jump(&h, h.text("big.ts"), &text, "value) + 6000;", 0);
    assert_bound_to_parameter(&r, &text, 6000);
    assert_eq!(streams(&reads()), 1);
    let r = jump(&h, h.text("big.ts"), &text, "value) + 100;", 0);
    assert_bound_to_parameter(&r, &text, 100);
    assert_eq!(streams(&reads()), 0);
}

#[test]
fn goto_large_changed_file_is_stale() {
    let text = fixture();
    let h = opened("big.ts", text.as_bytes());
    let at = h.text("big.ts");
    // Changed after the snapshot, the same length: the identity differs,
    // the streamed OID is not the text's.
    let changed = text.replacen("build_6000", "build_600x", 1);
    assert_eq!(changed.len(), text.len());
    h.git.write("main", "big.ts", changed.as_bytes());
    let r = jump(&h, at, &text, "value) + 6000;", 0);
    assert_eq!(r.state, QueryState::Failed(QueryError::Stale));
}

#[test]
fn goto_large_object_reuses_the_kept_window() {
    let text = fixture();
    let h = Harness::new();
    h.git.add("main", &[("big.ts", text.as_bytes())]);
    h.git.commit("main");
    h.open();
    h.settle();
    let at = TextRef {
        worktree: Some(h.wt()),
        path: "big.ts".into(),
        oid: h.git.oid(text.as_bytes()),
        origin: TextOrigin::Object,
    };
    let blobs = h.git.count("blob");
    parsed_lens();
    let r = jump(&h, at.clone(), &text, "value) + 6000;", 0);
    assert_bound_to_parameter(&r, &text, 6000);
    assert_eq!(
        r.candidates[0].definition.location.origin,
        TextOrigin::Object
    );
    assert_eq!(h.git.count("blob"), blobs + 1, "taken whole once");
    let lens = parsed_lens();
    assert!(lens.len() == 1 && lens[0] <= LONGEST_WINDOW, "{lens:?}");
    // In the kept window: nothing taken, nothing parsed.
    let r = jump(&h, at.clone(), &text, "value) + 6000;", 1);
    assert_bound_to_parameter(&r, &text, 6000);
    assert_eq!(h.git.count("blob"), blobs + 1);
    assert!(parsed_lens().is_empty());
    // Elsewhere: taken whole again, its checkpoints kept.
    let r = jump(&h, at, &text, "value) + 100;", 0);
    assert_bound_to_parameter(&r, &text, 100);
    assert_eq!(h.git.count("blob"), blobs + 2);
}

#[test]
fn goto_large_crlf_text_has_no_byte_offsets() {
    let text = fixture();
    let crlf = text.replace('\n', "\r\n");
    let h = Harness::new();
    h.git.add("main", &[("big.ts", text.as_bytes())]);
    h.git.commit("main");
    h.git.eol("big.ts");
    h.git.write("main", "big.ts", crlf.as_bytes());
    // A checkout: git's stat cache takes the CRLF file as unchanged.
    h.git.assume_unchanged(true);
    h.open();
    h.settle();
    let calls = h.git.count("canonical_oid");
    let at = h.text("big.ts");
    assert_eq!(at.oid, h.git.oid(text.as_bytes()));
    let r = jump(&h, at, &crlf, "value) + 6000;", 0);
    assert_done(&r);
    // Converted once (the whole file read once for that).
    assert_eq!(h.git.count("canonical_oid"), calls + 1);
    let c = &r.candidates[0];
    assert_eq!(c.evidence, Evidence::Bound(Binding::Local));
    let (line, column, _) = parameter(&text, 6000);
    let l = &c.definition.location;
    assert_eq!((l.line, l.column, l.byte), (line, column, None));
}

#[test]
fn goto_large_checkpoints_and_windows() {
    let text = fixture();
    let bytes = text.as_bytes();
    // The line at every 64 KiB, whatever the pieces streamed.
    let lines = checkpoints(bytes, 1 << 20);
    assert_eq!(lines, checkpoints(bytes, 1000));
    assert_eq!(lines.len(), bytes.len().div_ceil(CHECKPOINT as usize));
    for (k, line) in lines.iter().enumerate() {
        let at = k * CHECKPOINT as usize;
        assert_eq!(
            *line as usize,
            bytes[..at].iter().filter(|b| **b == b'\n').count()
        );
    }
    // A window starts at a statement's line, 8–12 KiB before the target.
    for target in [20_000, 300_001, 1_000_000] {
        let start = safe_start(bytes, target);
        assert!(start + BACK <= target && start + BACK + BACK_MORE >= target);
        assert_eq!(bytes[start - 1], b'\n');
        assert!(!bytes[start].is_ascii_whitespace());
    }
    assert_eq!(safe_start(bytes, 100), 0);
    // A minified line: just after a `;`, else never inside a name.
    let line = "call(a_long_name_here), ".repeat(2_000);
    let line = line.as_bytes();
    let start = safe_start(line, 20_000);
    assert!((20_000 - BACK..20_000 - BACK + 24).contains(&start));
    assert!(!(ident(line[start - 1]) && ident(line[start])));
    let semi = format!("{};{}", "x".repeat(9_000), "y".repeat(20_000));
    assert_eq!(safe_start(semi.as_bytes(), 20_000), 9_001);
    // The identifier at or just before a position.
    let s = b"let foo_bar = baz;";
    assert_eq!(ident_at(s, 6), Some(4..11));
    assert_eq!(ident_at(s, 11), Some(4..11));
    assert_eq!(ident_at(s, 12), None);
    assert_eq!(ident_at(b"x = 12;", 4), None, "a number is no name");
}
