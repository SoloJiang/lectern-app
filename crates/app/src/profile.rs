//! Profiles: which plugins a run assembles and how its startup barrier is
//! released (docs/design/app.md「接口」「装配」, docs/design/kernel.md「启动」).

use std::path::PathBuf;
use std::sync::Arc;

use lectern_agents::{
    AgentRoots, AgentsDeps, ConversationsDeps, SessionFilter, conversations_plugin,
};
use lectern_cache::Cache;
use lectern_conversation::{
    LIVE_ID as CONVERSATION_LIVE_ID, PLUGIN_ID as CONVERSATION_ID,
    TERMINAL_ID as CONVERSATION_TERMINAL_ID,
};
use lectern_executor::Executor;
use lectern_fs_watch::Watcher;
use lectern_kernel::PluginSet;
use lectern_logging::{Field, Value};
use lectern_project::{ProjectDeps, ProjectPlugin};
use lectern_project_home::{
    PLUGIN_ID as PROJECT_HOME_ID, ProjectHomeDeps, ProjectHomePlugin, ProjectHomeUsagePlugin,
    ProjectHomeWaitingPlugin, USAGE_PLUGIN_ID as PROJECT_HOME_USAGE_ID, UsageSlot,
    WAITING_PLUGIN_ID as PROJECT_HOME_WAITING_ID, WaitingSlot,
};
use lectern_review::ReviewDeps;
use lectern_review_view::{PLUGIN_ID as REVIEW_VIEW_ID, USAGE_ID as REVIEW_VIEW_USAGE_ID};
use lectern_terminal::{PLUGIN_ID as TERMINAL_ID, TerminalDeps, TerminalPlugin};
use lectern_usage::{LocalZone, UsageDeps};
use lectern_welcome::WelcomeDeps;
use lectern_workspace::{WorkspaceHost, WorkspacePlugin};
use lectern_worktrees::{
    PLUGIN_ID as WORKTREES_ID, USAGE_PLUGIN_ID as WORKTREES_USAGE_ID, UsageLink, WorktreesDeps,
    WorktreesPlugin, WorktreesUsagePlugin,
};

/// The package version cut to "major.minor", for Welcome's subtitle
/// (docs/design/welcome.md「对其他 crate 的要求」E).
const APP_VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION_MAJOR"),
    ".",
    env!("CARGO_PKG_VERSION_MINOR")
);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    /// What the release build runs.
    Desktop,
    /// `#[gpui::test]`: no real frames, so the barrier is released as soon as
    /// the window exists.
    Test,
    /// What `desktop` runs, plus the UI probe after the barrier: chosen by
    /// `run` when `LECTERN_UI_PROBE_DIR` is set
    /// (docs/design/ui_probe.md「对其他 crate 的要求」).
    #[cfg(feature = "ui-probe")]
    Probe,
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
    pub const ALL: &[Profile] = &[
        Profile::Desktop,
        Profile::Test,
        #[cfg(feature = "ui-probe")]
        Profile::Probe,
    ];

    /// Whether the run has a real window with real frames: the barrier is
    /// armed on its first render and the window gets a frame autosave name.
    pub fn has_real_window(self) -> bool {
        match self {
            Profile::Desktop => true,
            Profile::Test => false,
            #[cfg(feature = "ui-probe")]
            Profile::Probe => true,
        }
    }

    /// Desktop: `project`, the two session sources, `review`, `usage`,
    /// `workspace`, `welcome`, `welcome-discovery`, `welcome-review`,
    /// `project-home`, `project-home-usage`, `project-home-waiting`,
    /// `review-view`, `review-view-usage`, `worktrees`, `worktrees-usage`,
    /// `terminal`, `conversations`, `conversation`, `conversation-live` and
    /// `conversation-terminal`
    /// (docs/design/app.md「插件与资源」).
    /// Test: none. With the number of plugins, for the log (`PluginSet` does
    /// not expose it). No IO.
    pub fn plugins(self, resources: &Resources) -> (PluginSet, usize) {
        match self {
            Profile::Desktop => desktop(resources),
            #[cfg(feature = "ui-probe")]
            Profile::Probe => desktop(resources),
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
        watcher: r.watcher.clone(),
        data_dir: r.app_data.join("usage"),
        zone: Arc::new(SystemZone),
    });
    // One cell for the two project-home plugins: `dyn Usage` may be absent,
    // so the interest in the project's usage is reconciled in it.
    let slot = UsageSlot::default();
    // And one for the sessions that wait for the user: `dyn Conversations` may
    // be absent too (docs/design/app.md「project_home 的等待标记」).
    let waiting = WaitingSlot::default();
    // The window skeleton. Its Welcome screen comes from `welcome`;
    // `project-home` registers no screen until its views are merged.
    let mut set = set
        .with(id, review)
        .with(usage_id, usage)
        .with("workspace", WorkspacePlugin::new(r.workspace.clone()));
    // `welcome`, `welcome-discovery` and `welcome-review` share one model;
    // `plugins` builds it empty and does no IO (docs/design/welcome.md「对其他
    // crate 的要求」C, H).
    for (id, plugin) in lectern_welcome::plugins(WelcomeDeps {
        executor: r.executor.clone(),
        data_dir: r.app_data.join("welcome"),
        version: Some(APP_VERSION.into()),
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
                waiting: waiting.clone(),
            }),
        )
        .with(
            PROJECT_HOME_USAGE_ID,
            ProjectHomeUsagePlugin::new(r.executor.clone(), slot),
        )
        .with(
            PROJECT_HOME_WAITING_ID,
            ProjectHomeWaitingPlugin::new(waiting),
        );
    // The review screen. Its second plugin, "review-view-handoff", requires
    // `dyn Reader` and `dyn CodeIntel`, which no assembled plugin provides
    // yet, so it is dropped: the screen works without it and its handoff
    // buttons stay disabled. The third, "review-view-usage", requires only
    // `dyn Usage`, which `usage` provides: the brief's usage rows and the
    // turns' tokens do not wait for the handoff
    // (docs/design/app.md「review_view 的装配」).
    let (review_view, _handoff, review_view_usage) =
        lectern_review_view::plugins(r.executor.clone());
    let set = set
        .with(REVIEW_VIEW_ID, review_view)
        .with(REVIEW_VIEW_USAGE_ID, review_view_usage);
    // The parallel overview. `dyn Usage` may be absent, so the interest in
    // the worktrees' usage is reconciled in one cell, as for `project-home`
    // (docs/design/app.md「worktrees 的装配」).
    let link = UsageLink::default();
    let set = set
        .with(
            WORKTREES_ID,
            WorktreesPlugin::new(WorktreesDeps {
                executor: r.executor.clone(),
                usage: link.clone(),
            }),
        )
        .with(
            WORKTREES_USAGE_ID,
            WorktreesUsagePlugin::new(r.executor.clone(), link),
        );
    // The terminals. `new` keeps the executor and nothing else: no thread,
    // no PTY, no login environment until a terminal opens, and it activates
    // after the first frame (docs/design/app.md「terminal 的装配」).
    let set = set.with(
        TERMINAL_ID,
        TerminalPlugin::new(TerminalDeps {
            executor: r.executor.clone(),
        }),
    );
    // The conversation. `conversations` (the ACP client, which provides
    // `dyn Conversations`) starts no process until a prompt is sent; the
    // three plugins of the inspector's Conversation page only register and
    // hold service references. All four activate after the first frame
    // (docs/design/app.md「conversation 的装配」).
    let (id, conversations) = conversations_plugin(ConversationsDeps {
        executor: r.executor.clone(),
    });
    let (page, live, terminal_page) = lectern_conversation::plugins();
    let set = set
        .with(id, conversations)
        .with(CONVERSATION_ID, page)
        .with(CONVERSATION_LIVE_ID, live)
        .with(CONVERSATION_TERMINAL_ID, terminal_page);
    (set, count + 15)
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
            #[cfg(feature = "ui-probe")]
            Profile::Probe => "probe",
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
