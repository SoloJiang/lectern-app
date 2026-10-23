use std::sync::Arc;

use lectern_cache::Cache;
use lectern_executor::Executor;
use lectern_fs_watch::Watcher;
use lectern_fs_watch::testing::FakeBackend;

use crate::profile::{Profile, Resources};

/// The per-profile graph test kernel.md asks `app` for: a wrong graph
/// fails CI rather than a release. Building the plugins does no IO: the
/// directories they are given stay empty.
#[test]
fn every_profile_graph_validates() {
    let dir = tempfile::tempdir().unwrap();
    let executor = Executor::new(lectern_executor::Config::default());
    let resources = Resources {
        executor: executor.clone(),
        cache: Cache::new(lectern_cache::Config {
            root: dir.path().join("cache"),
            budgets: lectern_cache::Budgets::default(),
            ui_thread: None,
        }),
        watcher: Watcher::new(
            FakeBackend::new(),
            executor.clone(),
            lectern_fs_watch::Config::default(),
        ),
        home: dir.path().join("home"),
        app_cache: dir.path().join("app_cache"),
        app_data: dir.path().join("app_data"),
        git_ceilings: Arc::from([]),
    };
    for profile in Profile::ALL {
        let (set, count) = profile.plugins(&resources);
        if let Err(errors) = set.validate() {
            panic!("{profile:?}: {errors:?}");
        }
        let expected = match profile {
            Profile::Desktop => 4,
            Profile::Test => 0,
        };
        assert_eq!(count, expected, "{profile:?}");
    }
    let written: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
    assert!(written.is_empty(), "{written:?}");
}
