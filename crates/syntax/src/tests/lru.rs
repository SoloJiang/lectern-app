//! `lru_*`: the 8 MiB bound, the eviction order (background work first,
//! then the oldest), and documents keeping what they show.

use std::sync::Arc;

use lectern_services::project::BlobId;
use lectern_services::reader::Anchor;

use super::rig::Rig;
use crate::limits::LRU;
use crate::lru::{Class, Lru, LruKey, LruValue, OVERHEAD};
use crate::text::Text;
use crate::{Highlighting, Row, Tier, testing};

fn oid(n: u32) -> BlobId {
    let mut b = [0u8; 20];
    b[..4].copy_from_slice(&n.to_le_bytes());
    BlobId::Sha1(b)
}

fn text(len: usize) -> LruValue {
    LruValue::Text(Arc::new(Text::from_bytes(vec![b'a'; len])))
}

#[test]
fn lru_stays_within_its_limit_and_evicts_the_oldest() {
    let lru = Lru::new(1 << 20);
    for n in 0..40 {
        lru.insert(LruKey::Text(oid(n)), text(100 << 10), Class::Opened);
        assert!(lru.total() <= 1 << 20, "after {n}: {}", lru.total());
    }
    // About 10 fit; the newest are the ones kept.
    assert!(lru.len() >= 9 && lru.len() <= 10, "{}", lru.len());
    assert!(lru.contains(&LruKey::Text(oid(39))));
    assert!(!lru.contains(&LruKey::Text(oid(0))));
    // A use makes an entry the newest.
    let keep = oid(31);
    assert!(lru.text(keep, Class::Opened).is_some());
    for n in 40..48 {
        lru.insert(LruKey::Text(oid(n)), text(100 << 10), Class::Opened);
    }
    assert!(lru.contains(&LruKey::Text(keep)));
    assert!(!lru.contains(&LruKey::Text(oid(32))));
}

#[test]
fn lru_evicts_background_work_before_what_was_opened() {
    let lru = Lru::new(1 << 20);
    for n in 0..5 {
        lru.insert(LruKey::Text(oid(n)), text(100 << 10), Class::Opened);
    }
    for n in 100..105 {
        lru.insert(LruKey::Text(oid(n)), text(100 << 10), Class::Background);
    }
    // Newer than every opened entry, still the first to go.
    for n in 200..205 {
        lru.insert(LruKey::Text(oid(n)), text(100 << 10), Class::Opened);
    }
    for n in 0..5 {
        assert!(lru.contains(&LruKey::Text(oid(n))), "opened {n}");
    }
    for n in 100..105 {
        assert!(!lru.contains(&LruKey::Text(oid(n))), "background {n}");
    }
    // An opened use moves a background entry up.
    lru.insert(LruKey::Text(oid(300)), text(10), Class::Background);
    assert!(lru.text(oid(300), Class::Opened).is_some());
    lru.insert(LruKey::Text(oid(301)), text(200 << 10), Class::Opened);
    assert!(lru.contains(&LruKey::Text(oid(300))));
}

#[test]
fn lru_keeps_no_entry_over_its_limit_and_counts_replacements_once() {
    let lru = Lru::new(1 << 20);
    lru.insert(LruKey::Text(oid(1)), text(2 << 20), Class::Opened);
    assert_eq!(lru.len(), 0);
    lru.insert(LruKey::Text(oid(1)), text(1000), Class::Opened);
    let one = lru.total();
    lru.insert(LruKey::Text(oid(1)), text(1000), Class::Opened);
    assert_eq!(lru.total(), one);
    assert!(one > 1000 + OVERHEAD);
}

#[test]
fn lru_eviction_leaves_an_open_document_as_it_is() {
    let rig = Rig::new();
    rig.write("a.ts", &testing::ts(64 << 10, 0));
    let (doc, before) = rig.open_settled(rig.target("a.ts", Anchor::Start));
    assert_eq!(before.tier, Some(Tier::A));
    assert_eq!(before.highlight, Highlighting::On);
    let tokens = |s: &crate::DocSnapshot| {
        s.rows(s.anchor, 40, 100)
            .iter()
            .map(|r| match r {
                Row::Text { tokens, .. } => tokens.len(),
                Row::Pending { .. } => 0,
            })
            .sum::<usize>()
    };
    let shown = tokens(&before);
    assert!(shown > 0);
    // Fill the engine's LRU with other content.
    let lru = &rig.engine.inner().lru;
    for n in 0..20 {
        lru.insert(LruKey::Text(oid(n)), text(1 << 20), Class::Opened);
    }
    assert!(lru.total() <= LRU);
    let whole = LruKey::Spans {
        oid: before.oid.unwrap(),
        lang: "ts",
        grammar: super::lang("ts").grammar_version(),
        query: super::lang("ts").query_version(),
        start: crate::lru::WHOLE,
    };
    assert!(!lru.contains(&whole), "evicted");
    let after = doc.snapshot();
    assert_eq!(after.highlight, Highlighting::On);
    assert_eq!(tokens(&after), shown);
}

#[test]
fn lru_reopening_uses_the_whole_files_spans() {
    let rig = Rig::new();
    rig.write("a.rs", &testing::rust(32 << 10, 0));
    let (doc, first) = rig.open_settled(rig.target("a.rs", Anchor::Start));
    assert!(first.oid.is_some());
    drop(doc);
    rig.settle();
    // The first screen of the same file finds the whole file's spans by
    // its identity: exact from the start.
    let snapshot = rig.engine.first_screen(
        rig.target("a.rs", Anchor::Start),
        rig.root.clone(),
        super::rig::VIEW,
    );
    assert_eq!(snapshot.highlight, Highlighting::On);
    assert!(snapshot.spans[0].exact());
}
