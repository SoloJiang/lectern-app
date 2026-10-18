//! The name table's fixture for `code_intel/names_100k` (time) and
//! `code_intel/names_100k_bytes` (memory): the synthetic extractions of a
//! 100 000-file repository stored in a temporary cache, and the map of its
//! file list.

use std::path::Path;
use std::sync::Arc;

use lectern_bench::{BenchError, Ctx};
use lectern_cache::{Budgets, Cache, Config, Written};
use lectern_code_intel::NoGrammarPacks;
use lectern_code_intel::extract::LanguageTable;
use lectern_code_intel::map::{BaseMap, IndexedHints, LiveMap, Overlay, Provenance};
use lectern_code_intel::store::{Store, Version};
use lectern_code_intel::testing::synthetic;

pub struct Names {
    /// Keeps the store's partition open.
    pub _cache: Cache,
    pub store: Store,
    pub map: LiveMap,
    pub table: LanguageTable,
}

pub fn names(ctx: &Ctx, root: &Path) -> Result<Names, BenchError> {
    let n = ctx.scale(100_000, 3_000);
    let cache = Cache::new(Config {
        root: root.to_path_buf(),
        budgets: Budgets::default(),
        ui_thread: None,
    });
    let store = Store::new(&cache.repo(Path::new("/work/names/.git")));
    let table = LanguageTable::core();
    let mut write = store.write();
    for i in 0..n {
        let path = synthetic::path(i);
        let Some(lang) = table.for_path(&path, &NoGrammarPacks) else {
            continue;
        };
        write.add(
            synthetic::oid(i),
            &Version::of(lang),
            &synthetic::extraction(i),
        );
        if write.len() == 64 {
            let full = std::mem::replace(&mut write, store.write());
            if !matches!(full.commit(), Written::Committed) {
                return Err(BenchError::new("a write was dropped"));
            }
        }
    }
    if !write.is_empty() && !matches!(write.commit(), Written::Committed) {
        return Err(BenchError::new("a write was dropped"));
    }
    let files = Arc::new(synthetic::file_list(n));
    let map = LiveMap {
        hints: Arc::new(IndexedHints::new(files.len())),
        base: Arc::new(BaseMap::build(files, &table, &NoGrammarPacks)),
        overlay: Arc::new(Overlay::empty()),
        provenance: Provenance::default(),
    };
    Ok(Names {
        _cache: cache,
        store,
        map,
        table,
    })
}
