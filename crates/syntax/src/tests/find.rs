//! `find_*`: ASCII-only case folding, non-overlapping matches across block
//! boundaries, the 10,000 cap, and a new query cancelling the last one.

use std::sync::Arc;

use lectern_services::reader::Anchor;

use super::rig::Rig;
use crate::find::{BLOCK, find_in};
use crate::limits::FIND_MAX;
use crate::{FindQuery, Row};

fn ranges(text: &[u8], query: &str) -> Vec<std::ops::Range<u64>> {
    find_in(text, query, |_| false).unwrap().ranges.to_vec()
}

#[test]
fn find_folds_ascii_letters_only() {
    let text = "Hello HELLO hello hÉllo héllo Ünïcode ünïcode".as_bytes();
    assert_eq!(ranges(text, "hello").len(), 3);
    assert_eq!(ranges(text, "HeLLo").len(), 3);
    // Non-ASCII bytes match only themselves.
    assert_eq!(ranges(text, "héllo").len(), 1);
    assert_eq!(ranges(text, "ünïcode").len(), 1);
    assert_eq!(ranges(text, "Ünïcode").len(), 1);
    // No letters: exact bytes.
    assert_eq!(ranges(b"1+1 1+1", "1+1"), [0..3, 4..7]);
    let empty = find_in(text, "", |_| false).unwrap();
    assert!(empty.done && empty.ranges.is_empty());
}

#[test]
fn find_matches_do_not_overlap_and_cross_blocks() {
    assert_eq!(ranges(b"aaaa", "aa"), [0..2, 2..4]);
    let mut text = vec![b'.'; BLOCK * 2 + 10];
    let at = BLOCK - 2;
    text[at..at + 6].copy_from_slice(b"needle");
    text[BLOCK * 2..BLOCK * 2 + 6].copy_from_slice(b"NEEDLE");
    let found = ranges(&text, "needle");
    assert_eq!(
        found,
        [
            at as u64..at as u64 + 6,
            (BLOCK * 2) as u64..(BLOCK * 2 + 6) as u64
        ]
    );
}

#[test]
fn find_stops_at_ten_thousand() {
    let text = "x ".repeat(12_000);
    let results = find_in(text.as_bytes(), "X", |_| false).unwrap();
    assert_eq!(results.ranges.len(), FIND_MAX);
    assert!(results.capped && results.done);
    let under = "x ".repeat(9_999);
    let results = find_in(under.as_bytes(), "x", |_| false).unwrap();
    assert!(!results.capped && results.done);
    assert_eq!(results.ranges.len(), 9_999);
}

#[test]
fn find_in_a_document_replaces_the_last_query() {
    let rig = Rig::new();
    let mut text = String::new();
    for n in 0..5_000 {
        text.push_str(&format!("alpha {n} Beta\n"));
    }
    rig.write("a.txt", text.as_bytes());
    let (doc, _) = rig.open_settled(rig.target("a.txt", Anchor::Start));
    doc.find(FindQuery {
        text: Arc::from("alpha"),
    });
    doc.find(FindQuery {
        text: Arc::from("beta"),
    });
    rig.settle();
    let s = doc.snapshot();
    let found = s.find.clone().expect("results");
    assert!(found.done && !found.capped);
    assert_eq!(found.ranges.len(), 5_000);
    let first = &found.ranges[0];
    assert_eq!(
        &text.as_bytes()[first.start as usize..first.end as usize],
        b"Beta"
    );
    // The rows carry their matches.
    let rows = s.rows(s.anchor, 3, 120);
    for row in rows {
        let Row::Text { matches, .. } = row else {
            panic!("in memory");
        };
        assert_eq!(matches.len(), 1);
    }
    doc.clear_find();
    rig.settle();
    assert!(doc.snapshot().find.is_none());
}

#[test]
fn find_waits_for_the_full_read_and_searches_tier_c_files() {
    let rig = Rig::new();
    let line = b"0123456789 abcdefghij klmnopqrstuvwxyz;\n";
    let mut bytes = Vec::with_capacity(17 << 20);
    while bytes.len() < 17 << 20 {
        bytes.extend_from_slice(line);
    }
    bytes.extend_from_slice(b"needle at the end\n");
    rig.write("huge.txt", &bytes);
    let doc = rig.open(rig.target("huge.txt", Anchor::Start));
    doc.find(FindQuery {
        text: Arc::from("NEEDLE"),
    });
    rig.settle();
    let found = doc.snapshot().find.clone().expect("results");
    assert!(found.done);
    let at = (bytes.len() - 18) as u64;
    assert_eq!(found.ranges.len(), 1);
    assert_eq!(found.ranges[0], at..at + 6);
}
