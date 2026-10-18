//! `warm_*`: reopening a project whose units are all in the cache extracts
//! nothing (existence checks only); with some units gone, only those.

use lectern_services::code_intel::TierState;
use lectern_services::project::BlobId;

use crate::store::Version;
use crate::testing::synthetic;

use super::support::{Harness, Options, all, tiers};

/// `n` synthetic source files committed in the main worktree; returns
/// their (path, OID, language).
pub(crate) fn fill(h: &Harness, n: usize) -> Vec<(String, BlobId, &'static str)> {
    let mut out = Vec::new();
    for i in 0..n {
        let ext = ["ts", "js", "rs", "py", "go", "java", "tsx"][i % 7];
        let path = format!("src/m{}/file{i}.{ext}", i % 5);
        let source = synthetic::source(ext, i, 600);
        h.git.add("main", &[(&path, &source)]);
        let lang = match ext {
            "rs" => "rust",
            "py" => "python",
            other => other,
        };
        out.push((path, h.git.oid(&source), lang));
    }
    h.git.commit("main");
    out
}

#[test]
fn warm_reopen_extracts_nothing() {
    let h = Harness::new();
    let files = fill(&h, 40);
    h.open();
    assert_eq!(tiers(&h.coverage()), all(TierState::Done));
    assert_eq!(h.extractions(), 40);
    for (path, oid, lang) in &files {
        assert!(h.stored(*oid, lang), "{path}");
        assert!(h.hinted(path), "{path}");
    }
    let h = h.reopen(Options::default());
    h.open();
    assert_eq!(tiers(&h.coverage()), all(TierState::Done));
    assert_eq!(h.extractions(), 0, "a warm start extracts nothing");
    // 40 files of 7 languages: one existence check per language.
    assert_eq!(h.stats().exists_checks, 7);
    for (path, _, _) in &files {
        assert!(h.hinted(path), "{path}");
    }
}

#[test]
fn warm_extracts_only_the_missing_units() {
    let h = Harness::new();
    let files = fill(&h, 30);
    h.open();
    assert_eq!(h.extractions(), 30);
    let store = h.indexer.store(&h.key()).expect("store");
    let table = crate::extract::LanguageTable::core();
    let gone: Vec<_> = files.iter().step_by(4).cloned().collect();
    let mut write = store.write();
    for (_, oid, lang) in &gone {
        write.remove(*oid, &Version::of(table.get(lang).expect("lang")));
    }
    assert!(matches!(write.commit(), lectern_cache::Written::Committed));
    let h = h.reopen(Options::default());
    h.open();
    let mut extracted = h.extracted();
    extracted.sort();
    let mut want: Vec<BlobId> = gone.iter().map(|(_, o, _)| *o).collect();
    want.sort();
    assert_eq!(extracted, want);
    assert_eq!(tiers(&h.coverage()), all(TierState::Done));
}
