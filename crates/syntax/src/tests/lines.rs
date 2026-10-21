//! `lines_*`: `LineCheckpoints`' encoding, written only for content of at
//! least 1 MiB and read back by the raw bytes' OID, not used when the
//! length differs.

use lectern_cache::Entry;
use lectern_git::{HashKind, Oid};
use lectern_services::reader::Anchor;

use super::rig::{Rig, VIEW};
use crate::git_access::blob_id;
use crate::limits::CHECKPOINT_STRIDE;
use crate::text::{IndexBuilder, LineCheckpoints, Text};
use crate::{LineCount, testing};

fn encoded(c: &LineCheckpoints) -> Vec<u8> {
    let mut out = Vec::new();
    c.encode(&mut out);
    out
}

#[test]
fn lines_encoding_round_trips_and_rejects_bad_values() {
    let bytes = testing::ts(300 << 10, 0);
    let mut builder = IndexBuilder::new();
    builder.feed(&bytes);
    let checkpoints = builder.finish().checkpoints();
    assert_eq!(
        checkpoints.lines.len() as u64,
        bytes.len() as u64 / CHECKPOINT_STRIDE + 1
    );
    let out = encoded(&checkpoints);
    assert_eq!(LineCheckpoints::decode(&out), Some(checkpoints.clone()));
    // Truncated, a count that does not match the length, a bad flag,
    // lines going down.
    assert_eq!(LineCheckpoints::decode(&out[..out.len() - 4]), None);
    let mut flag = out.clone();
    flag[20] = 2;
    assert_eq!(LineCheckpoints::decode(&flag), None);
    let mut down = checkpoints.clone();
    down.lines[2] = 0;
    down.lines[1] = 5;
    assert_eq!(LineCheckpoints::decode(&encoded(&down)), None);
    assert_eq!(LineCheckpoints::decode(&[]), None);
    // The checkpoints agree with the text: lines[k] line breaks before
    // byte k × 64 KiB.
    for (k, &line) in checkpoints.lines.iter().enumerate() {
        let at = (k as u64 * CHECKPOINT_STRIDE) as usize;
        assert_eq!(
            memchr::memchr_iter(b'\n', &bytes[..at]).count() as u32,
            line
        );
    }
    let text = Text::from_bytes(bytes.clone());
    for line in [0u32, 1, 500, 3000, checkpoints.line_count as u32 - 1] {
        let (byte, at_line) = checkpoints.before_line(line);
        let start = text.index().line_start(&text, u64::from(line));
        assert!(
            byte <= start && start - byte <= CHECKPOINT_STRIDE,
            "line {line}"
        );
        assert!(at_line <= line);
    }
}

fn stored(rig: &Rig, bytes: &[u8]) -> Option<LineCheckpoints> {
    let oid = blob_id(&Oid::for_blob(HashKind::Sha1, bytes));
    rig.engine.inner().checkpoints(&rig.project(), &oid)
}

#[test]
fn lines_are_written_by_oid_only_from_one_mebibyte() {
    let rig = Rig::new();
    let small = testing::ts(900 << 10, 0);
    let large = testing::ts((1 << 20) + 50_000, 0);
    rig.write("small.ts", &small);
    rig.write("large.ts", &large);
    let (_a, s) = rig.open_settled(rig.target("small.ts", Anchor::Start));
    let (_b, l) = rig.open_settled(rig.target("large.ts", Anchor::Start));
    assert!(s.oid.is_some() && l.oid.is_some());
    assert_eq!(stored(&rig, &small), None);
    let checkpoints = stored(&rig, &large).expect("written");
    assert_eq!(checkpoints.len, large.len() as u64);
    assert_eq!(LineCount::Exact(checkpoints.line_count), l.lines);
}

#[test]
fn lines_put_the_first_screen_at_a_line_on_the_next_open() {
    let rig = Rig::new();
    let bytes = testing::rust(3 << 20, 0);
    rig.write("big.rs", &bytes);
    let text = Text::from_bytes(bytes.clone());
    let line = 50_000u32;
    let start = text.index().line_start(&text, u64::from(line));
    let at = Anchor::Line { line, column: 0 };
    // No checkpoints yet: the first screen opens at the file's start.
    let first = rig
        .engine
        .first_screen(rig.target("big.rs", at), rig.root.clone(), VIEW);
    assert_eq!(first.anchor.line_start, 0);
    // The full read moves there, and stores the checkpoints.
    let (doc, full) = rig.open_settled(rig.target("big.rs", at));
    assert_eq!(full.anchor.line_start, start);
    drop(doc);
    rig.settle();
    // The next first screen finds them by the OID its identity maps to.
    let again = rig
        .engine
        .first_screen(rig.target("big.rs", at), rig.root.clone(), VIEW);
    assert_eq!(again.anchor.line_start, start);
    // And by `expect`.
    let oid = full.oid.unwrap();
    let mut target = rig.target("big.rs", at);
    target.expect = Some(oid);
    let by_expect = rig.engine.first_screen(target, rig.root.clone(), VIEW);
    assert_eq!(by_expect.anchor.line_start, start);
}

#[test]
fn lines_with_another_length_are_not_used() {
    let rig = Rig::new();
    let bytes = testing::rust(2 << 20, 0);
    rig.write("big.rs", &bytes);
    let oid = blob_id(&Oid::for_blob(HashKind::Sha1, &bytes));
    // Checkpoints under that OID claiming another length.
    let mut builder = IndexBuilder::new();
    builder.feed(&bytes[..bytes.len() - 1000]);
    let wrong = builder.finish().checkpoints();
    rig.engine
        .inner()
        .store_checkpoints(&rig.project(), &oid, &wrong);
    let mut target = rig.target(
        "big.rs",
        Anchor::Line {
            line: 20_000,
            column: 0,
        },
    );
    target.expect = Some(oid);
    let first = rig.engine.first_screen(target, rig.root.clone(), VIEW);
    assert_eq!(first.anchor.line_start, 0, "not used");
}
