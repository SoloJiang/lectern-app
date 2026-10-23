//! Profiles: which plugins a run assembles and how its startup barrier is
//! released (docs/design/app.md「接口」「装配」, docs/design/kernel.md「启动」).

use std::path::PathBuf;
use std::sync::Arc;

use lectern_agents::{AgentRoots, AgentsDeps, SessionFilter};
use lectern_cache::Cache;
use lectern_executor::Executor;
use lectern_fs_watch::Watcher;
use lectern_kernel::PluginSet;
use lectern_logging::{Field, Value};
use lectern_project::{ProjectDeps, ProjectPlugin};
use lectern_review::ReviewDeps;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    /// What the release build runs.
    Desktop,
    /// `#[gpui::test]`: no real frames, so the barrier is released as soon as
    /// the window exists.
    Test,
}

/// The process-wide resources `release` creates and hands to the profile
/// (ADR 0025). Building it does no IO.
pub struct Resources {
    pub executor: Executor,
    pub cache: Cache,
    pub watcher: Watcher,
    /// `HOME`, absolute.
    pub home: PathBuf,
    /// `platform::dirs::app_cache()`.
    pub app_cache: PathBuf,
    /// `platform::dirs::app_data()`.
    pub app_data: PathBuf,
    /// `GIT_CEILING_DIRECTORIES`, read once (as `project` reads it).
    pub git_ceilings: Arc<[PathBuf]>,
}

impl Profile {
    #[cfg(test)]
    pub const ALL: [Profile; 2] = [Profile::Desktop, Profile::Test];

    /// Desktop: `project`, the two session sources and `review`
    /// (docs/design/app.md「插件与资源」). Test: none. With the number of
    /// plugins, for the log (`PluginSet` does not expose it). No IO.
    pub fn plugins(self, resources: &Resources) -> (PluginSet, usize) {
        match self {
            Profile::Desktop => desktop(resources),
            Profile::Test => (PluginSet::new(), 0),
        }
    }
}

fn desktop(r: &Resources) -> (PluginSet, usize) {
    let mut count = 1;
    let mut set = PluginSet::new().with(
        "project",
        ProjectPlugin::new(ProjectDeps {
            executor: r.executor.clone(),
            watcher: r.watcher.clone(),
            home: r.home.clone(),
        }),
    );
    for (id, plugin) in lectern_agents::plugins(AgentsDeps {
        executor: r.executor.clone(),
        cache: r.cache.clone(),
        watch: r.watcher.clone(),
        decoded_dir: r.app_cache.join("agents").join("decoded"),
        roots: AgentRoots::from_env(&r.home),
        filter: SessionFilter::new(),
    }) {
        set = set.with(id, plugin);
        count += 1;
    }
    let (id, review) = lectern_review::plugin(ReviewDeps {
        executor: r.executor.clone(),
        cache: r.cache.clone(),
        watcher: r.watcher.clone(),
        data_dir: r.app_data.join("review"),
        git_ceilings: r.git_ceilings.clone(),
    });
    (set.with(id, review), count + 1)
}

impl Field for Profile {
    fn value(&self) -> Value<'_> {
        Value::text(match self {
            Profile::Desktop => "desktop",
            Profile::Test => "test",
        })
    }
}
