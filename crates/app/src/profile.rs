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
use lectern_project_home::{
    PLUGIN_ID as PROJECT_HOME_ID, ProjectHomeDeps, ProjectHomePlugin, ProjectHomeUsagePlugin,
    USAGE_PLUGIN_ID as PROJECT_HOME_USAGE_ID, UsageSlot,
};
use lectern_review::ReviewDeps;
use lectern_usage::{LocalZone, UsageDeps};
use lectern_welcome::WelcomeDeps;
use lectern_workspace::{WorkspaceHost, WorkspacePlugin};

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
    /// What `workspace::install` returned, for the "workspace" plugin.
    pub workspace: WorkspaceHost,
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

    /// Desktop: `project`, the two session sources, `review`, `usage`,
    /// `workspace`, `welcome`, `welcome-discovery`, `project-home` and
    /// `project-home-usage`
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
    let (usage_id, usage) = lectern_usage::plugin(UsageDeps {
        executor: r.executor.clone(),
        data_dir: r.app_data.join("usage"),
        zone: Arc::new(SystemZone),
    });
    // One cell for the two project-home plugins: `dyn Usage` may be absent,
    // so the interest in the project's usage is reconciled in it.
    let slot = UsageSlot::default();
    // The window skeleton. Its Welcome screen comes from `welcome`;
    // `project-home` registers no screen until its views are merged.
    let mut set = set
        .with(id, review)
        .with(usage_id, usage)
        .with("workspace", WorkspacePlugin::new(r.workspace.clone()));
    // `welcome` and `welcome-discovery` share one model; `plugins` builds it
    // empty and does no IO (docs/design/welcome.md「对其他 crate 的要求」C).
    for (id, plugin) in lectern_welcome::plugins(WelcomeDeps {
        executor: r.executor.clone(),
        data_dir: r.app_data.join("welcome"),
    }) {
        set = set.with(id, plugin);
        count += 1;
    }
    let set = set
        .with(
            PROJECT_HOME_ID,
            ProjectHomePlugin::new(ProjectHomeDeps {
                executor: r.executor.clone(),
                usage: slot.clone(),
            }),
        )
        .with(
            PROJECT_HOME_USAGE_ID,
            ProjectHomeUsagePlugin::new(r.executor.clone(), slot),
        );
    (set, count + 5)
}

/// `usage`'s local time zone: `platform::time::utc_offset`
/// (docs/design/usage.md「对其他 crate 的要求」B, C).
struct SystemZone;

impl LocalZone for SystemZone {
    fn offset_at(&self, unix_ms: i64) -> i32 {
        offset_at(unix_ms, lectern_platform::time::utc_offset)
    }
}

/// `unix_ms` in seconds, rounded down (a truncating division would put the
/// last second before a switch of the offset after the switch), and the
/// offset `utc_offset` gives for it. None is not expected for a millisecond
/// timestamp (docs/design/platform.md「本地时区的偏移」「范围」); it counts as
/// UTC.
fn offset_at(unix_ms: i64, utc_offset: impl FnOnce(i64) -> Option<i32>) -> i32 {
    utc_offset(unix_ms.div_euclid(1000)).unwrap_or(0)
}

impl Field for Profile {
    fn value(&self) -> Value<'_> {
        Value::text(match self {
            Profile::Desktop => "desktop",
            Profile::Test => "test",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::offset_at;

    #[test]
    fn zone_rounds_milliseconds_down_to_seconds() {
        let seen = |ms| offset_at(ms, |secs| i32::try_from(secs).ok());
        assert_eq!(seen(1_999), 1);
        assert_eq!(seen(2_000), 2);
        assert_eq!(seen(-1), -1);
        assert_eq!(seen(-1_000), -1);
        assert_eq!(seen(-1_001), -2);
    }

    #[test]
    fn zone_without_an_offset_is_utc() {
        assert_eq!(offset_at(0, |_| None), 0);
    }
}
