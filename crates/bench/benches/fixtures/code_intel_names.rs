//! The name table's fixture for `code_intel/names_100k` (time) and
//! `code_intel/names_100k_bytes` (memory): the stored units of the shared
//! 100 000-file virtual repository (`code_intel_virtual`), in a copy of its
//! own, and the map of its file list.

use std::path::Path;
use std::sync::Arc;

use lectern_bench::{BenchError, Ctx};
use lectern_cache::Cache;
use lectern_code_intel::NoGrammarPacks;
use lectern_code_intel::extract::LanguageTable;
use lectern_code_intel::map::{BaseMap, IndexedHints, LiveMap, Overlay, Provenance};
use lectern_code_intel::store::Store;
use lectern_code_intel::testing::{FakeGit, synthetic};

pub struct Names {
    /// Keep the copy's repository and partition open.
    pub _git: Arc<FakeGit>,
    pub _cache: Cache,
    pub store: Store,
    pub map: LiveMap,
    pub table: LanguageTable,
}

/// `root` is under `Ctx::scratch`.
pub fn names(ctx: &Ctx, root: &Path) -> Result<Names, BenchError> {
    let template = crate::code_intel_virtual::shared(ctx)?;
    let (git, cache) = crate::code_intel_virtual::open(&template, root)?;
    let store = Store::new(&cache.repo(git.common_dir()));
    let table = LanguageTable::core();
    let files = Arc::new(synthetic::file_list(template.files()));
    let map = LiveMap {
        hints: Arc::new(IndexedHints::new(files.len())),
        base: Arc::new(BaseMap::build(files, &table, &NoGrammarPacks)),
        overlay: Arc::new(Overlay::empty()),
        provenance: Provenance::default(),
    };
    Ok(Names {
        _git: git,
        _cache: cache,
        store,
        map,
        table,
    })
}
