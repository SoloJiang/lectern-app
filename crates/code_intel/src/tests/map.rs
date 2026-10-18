//! `map_*`: the blob → path map built from a file list with OIDs, lookups
//! by (OID, language), the overlay hiding index entries, and the hints.

use std::sync::Arc;

use lectern_services::project::{BlobId, FileKind, FileList};

use crate::NoGrammarPacks;
use crate::extract::LanguageTable;
use crate::map::{BaseMap, HintSlot, HintStamp, IndexedHints, LiveMap, Overlay, WorktreeState};

fn oid(prefix: u64, tail: u8) -> BlobId {
    let mut b = [0u8; 20];
    b[..8].copy_from_slice(&prefix.to_be_bytes());
    b[19] = tail;
    BlobId::Sha1(b)
}

fn list(entries: &[(&str, FileKind, BlobId)]) -> Arc<FileList> {
    let mut b = FileList::builder();
    for (p, k, o) in entries {
        b.push_with_oid(p, *k, *o);
    }
    Arc::new(b.build())
}

fn live(files: Arc<FileList>, overlay: Vec<(&str, WorktreeState)>) -> LiveMap {
    let table = LanguageTable::core();
    let base = BaseMap::build(files, &table, &NoGrammarPacks);
    let overlay = Overlay::build(
        overlay.into_iter().map(|(p, s)| (p.into(), s)).collect(),
        &base,
        &NoGrammarPacks,
    );
    LiveMap {
        hints: Arc::new(IndexedHints::new(base.files().len())),
        base: Arc::new(base),
        overlay: Arc::new(overlay),
    }
}

fn found(map: &LiveMap, oid: &BlobId, lang: &str) -> Vec<String> {
    map.paths(oid, lang)
        .into_iter()
        .map(|e| e.path.to_string())
        .collect()
}

#[test]
fn map_builds_from_the_file_list() {
    let files = list(&[
        ("a/lib.rs", FileKind::File, oid(1, 1)),
        ("a/run.py", FileKind::Executable, oid(2, 1)),
        ("a/link.ts", FileKind::Symlink, oid(3, 1)),
        ("a/sub", FileKind::Submodule, oid(4, 1)),
        ("docs/readme.md", FileKind::File, oid(5, 1)),
        ("web/app.tsx", FileKind::File, oid(6, 1)),
    ]);
    let map = live(files, Vec::new());
    // Regular and executable files with a supported language only.
    assert_eq!(map.base.len(), 3);
    assert_eq!(found(&map, &oid(1, 1), "rust"), ["a/lib.rs"]);
    assert_eq!(found(&map, &oid(2, 1), "python"), ["a/run.py"]);
    assert_eq!(found(&map, &oid(6, 1), "tsx"), ["web/app.tsx"]);
    for (o, lang) in [(oid(3, 1), "ts"), (oid(4, 1), "rust"), (oid(5, 1), "rust")] {
        assert!(found(&map, &o, lang).is_empty(), "{lang}");
    }
    assert!(map.effective("a/link.ts").is_none());
    assert_eq!(map.effective("a/lib.rs").map(|e| e.oid), Some(oid(1, 1)));
}

#[test]
fn map_oids_sharing_a_prefix_are_told_apart() {
    let files = list(&[
        ("a.ts", FileKind::File, oid(7, 1)),
        ("b.ts", FileKind::File, oid(7, 2)),
        ("c.ts", FileKind::File, oid(7, 1)),
    ]);
    let map = live(files, Vec::new());
    assert_eq!(found(&map, &oid(7, 1), "ts"), ["a.ts", "c.ts"]);
    assert_eq!(found(&map, &oid(7, 2), "ts"), ["b.ts"]);
    assert!(found(&map, &oid(7, 3), "ts").is_empty());
}

#[test]
fn map_overlay_hides_the_index_entry() {
    let files = list(&[
        ("edited.ts", FileKind::File, oid(1, 1)),
        ("deleted.ts", FileKind::File, oid(2, 1)),
        ("kept.ts", FileKind::File, oid(3, 1)),
        ("big.ts", FileKind::File, oid(4, 1)),
    ]);
    let map = live(
        files,
        vec![
            ("edited.ts", WorktreeState::Content(oid(9, 1))),
            ("deleted.ts", WorktreeState::Absent),
            ("big.ts", WorktreeState::Unindexable),
            ("new.ts", WorktreeState::Content(oid(8, 1))),
        ],
    );
    // The edited file's old OID and the deleted file are gone.
    assert!(found(&map, &oid(1, 1), "ts").is_empty());
    assert!(found(&map, &oid(2, 1), "ts").is_empty());
    assert!(found(&map, &oid(4, 1), "ts").is_empty());
    assert_eq!(found(&map, &oid(9, 1), "ts"), ["edited.ts"]);
    assert_eq!(found(&map, &oid(3, 1), "ts"), ["kept.ts"]);
    // Untracked files are found.
    assert_eq!(found(&map, &oid(8, 1), "ts"), ["new.ts"]);
    assert!(map.effective("deleted.ts").is_none());
    assert!(map.effective("big.ts").is_none());
    assert_eq!(
        map.effective("new.ts").map(|e| e.slot),
        Some(HintSlot::Extra("new.ts".into()))
    );
}

#[test]
fn map_overlay_drops_entries_equal_to_the_index() {
    let files = list(&[("same.ts", FileKind::File, oid(1, 1))]);
    let map = live(
        files,
        vec![
            ("same.ts", WorktreeState::Content(oid(1, 1))),
            ("gone.ts", WorktreeState::Absent),
            ("notes.md", WorktreeState::Content(oid(2, 1))),
        ],
    );
    assert!(map.overlay.is_empty(), "{:?}", map.overlay.entries());
    assert_eq!(found(&map, &oid(1, 1), "ts"), ["same.ts"]);
}

#[test]
fn map_oid_language_match() {
    // The same bytes under `.java` and `.js`: one OID, two languages.
    let files = list(&[
        ("src/Same.java", FileKind::File, oid(1, 1)),
        ("web/same.js", FileKind::File, oid(1, 1)),
    ]);
    let map = live(files, Vec::new());
    assert_eq!(found(&map, &oid(1, 1), "java"), ["src/Same.java"]);
    assert_eq!(found(&map, &oid(1, 1), "js"), ["web/same.js"]);
    assert!(found(&map, &oid(1, 1), "ts").is_empty());
}

#[test]
fn map_hints_match_only_their_oid_and_stamp() {
    let files = list(&[
        ("a.ts", FileKind::File, oid(1, 1)),
        ("b.ts", FileKind::File, oid(2, 1)),
    ]);
    let map = live(files.clone(), Vec::new());
    let stamp = HintStamp {
        eviction: 3,
        grammar: 1,
    };
    let a = HintSlot::Base(0);
    assert!(!map.hints.matches(&a, &oid(1, 1), stamp));
    map.hints.set(&a, &oid(1, 1), stamp);
    assert!(map.hints.matches(&a, &oid(1, 1), stamp));
    // Another OID, eviction or language generation: no match. (A hint
    // stores the OID's first 8 bytes only, as the design sizes it.)
    assert!(!map.hints.matches(&a, &oid(2, 1), stamp));
    for other in [
        HintStamp {
            eviction: 4,
            ..stamp
        },
        HintStamp {
            grammar: 2,
            ..stamp
        },
    ] {
        assert!(!map.hints.matches(&a, &oid(1, 1), other));
    }
    // Carried to a new list by path: `a.ts` keeps its hint, a new file
    // before it does not shift it.
    let next = list(&[
        ("0.ts", FileKind::File, oid(5, 1)),
        ("a.ts", FileKind::File, oid(1, 1)),
        ("b.ts", FileKind::File, oid(2, 1)),
    ]);
    let carried = IndexedHints::carried(&map.hints, &files, &next);
    assert!(carried.matches(&HintSlot::Base(1), &oid(1, 1), stamp));
    assert!(!carried.matches(&HintSlot::Base(0), &oid(1, 1), stamp));
    assert!(!carried.matches(&HintSlot::Base(2), &oid(2, 1), stamp));
    // Untracked paths' hints live by path.
    let extra = HintSlot::Extra("new.ts".into());
    map.hints.set(&extra, &oid(8, 1), stamp);
    assert!(map.hints.matches(&extra, &oid(8, 1), stamp));
    assert!(!map.hints.matches(&extra, &oid(9, 1), stamp));
}

#[test]
fn map_100k_fits_the_budget() {
    // 「性能」: rebuilding the map of 100 000 files is ≤ 3 MB. The by_oid
    // table is 12 bytes per entry, the hints 16, the languages 1.
    let files = Arc::new(crate::testing::synthetic::file_list(100_000));
    let table = LanguageTable::core();
    let base = BaseMap::build(files.clone(), &table, &NoGrammarPacks);
    let hints = IndexedHints::new(files.len());
    let bytes = base.heap_bytes() + hints.heap_bytes();
    assert!(bytes <= 3 << 20, "{bytes} bytes");
    assert!(base.len() > 85_000, "{} supported files", base.len());
}
