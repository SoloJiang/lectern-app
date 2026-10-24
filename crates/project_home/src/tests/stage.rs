//! The GPUI rig of the step-2 tests (docs/design/project_home.md「测试」):
//! `lectern_ui::init_fixed`, a test window that holds the two views the way
//! `workspace` would (the sidebar in its 248pt region with the `Sidebar`
//! context, the screen in the `Content` region with the `Content` context and
//! the 760pt reading column), the real plugins in a real kernel with the fake
//! services, and the semantic tree as compact text.
//!
//! `#[gpui::test]` draws frames only when a test asks; `frame` does.

use std::fmt::Write as _;
use std::sync::Arc;

use gpui::{
    AnyView, Context, Entity, FocusHandle, InteractiveElement, IntoElement, Modifiers,
    ParentElement, Render, Styled, TestAppContext, VisualTestContext, Window, div, px,
};
use lectern_i18n::{Locale, Strings, keys};
use lectern_services::project::{ProjectKey, Scan, WorktreeKey};
use lectern_services::review::OutsideCount;
use lectern_services::session::{Agent, SessionKey};
use lectern_services::usage::UsageState;
use lectern_services::workspace::{SidebarSection, Target, WorkspaceCommand};
use lectern_ui::contrib::{Screen, SidebarItem, SidebarPanel};
use lectern_ui::semantic::{Record, Recorder};
use lectern_ui::{Appearance, init_fixed};

use crate::screen::HomeScreen;
use crate::sidebar::HomeSidebar;
use crate::testing::{
    DAY, MINUTE, ProjectFx, Rig, branch, entry, overview, package, progress, project_usage, sess,
    usage_limits,
};

/// The sidebar and the screen side by side, as `workspace` lays them out.
pub(crate) struct Host {
    pub sidebar: Option<AnyView>,
    pub screen: Option<AnyView>,
    /// The workspace's root view takes the focus when a popped screen had it.
    pub focus: FocusHandle,
    /// A stand-in for an overlay: its own context and focus.
    pub overlay: FocusHandle,
}

impl Render for Host {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let mut row = div()
            .size_full()
            .flex()
            .flex_row()
            .key_context("Workspace")
            .track_focus(&self.focus)
            .child(
                div()
                    .key_context("Overlay")
                    .track_focus(&self.overlay)
                    .size(px(1.0)),
            );
        if let Some(sidebar) = &self.sidebar {
            row = row.child(
                div()
                    .key_context("Sidebar")
                    .w(px(248.0))
                    .h_full()
                    .child(sidebar.clone()),
            );
        }
        if let Some(screen) = &self.screen {
            row = row.child(
                div().key_context("Content").flex_1().h_full().child(
                    div()
                        .h_full()
                        .w_full()
                        .max_w(px(760.0))
                        .child(screen.clone()),
                ),
            );
        }
        row
    }
}

/// The kernel and the fakes, with GPUI set up (call first in a test).
pub(crate) fn rig(cx: &mut TestAppContext) -> Rig {
    // The kernel's threads wake the views' foreground tasks; the test
    // scheduler accepts a wake from another thread only with parking allowed.
    cx.executor().allow_parking();
    cx.update(|cx| init_fixed(cx, Strings::of(Locale::En), Appearance::Light, false));
    Rig::new()
}

/// The keys of the sessions of `standard`.
pub(crate) struct World {
    pub key: ProjectKey,
    pub main: WorktreeKey,
    /// Running, drifted, one critical: 2 of 5 seen.
    pub running: SessionKey,
    /// Codex: nothing seen yet.
    pub codex: SessionKey,
    /// In the other worktree; not in the overview.
    pub linked: SessionKey,
    /// Fully seen.
    pub done: SessionKey,
    /// A third-party agent without a title; no progress.
    pub other: SessionKey,
    /// Fully seen, then 2 files changed.
    pub changed: SessionKey,
}

/// A git project with a linked worktree, six sessions, three sub-projects and
/// an overview, opened and composed. The home plugin is activated, not the
/// usage plugin.
pub(crate) fn standard(rig: &Rig) -> World {
    let fx = ProjectFx::git("app")
        .linked("feature", branch("feature/search"))
        .packages(Scan::Ready(vec![
            package("api", "api", None),
            package("web", "web", None),
            package("web/ui", "ui", Some(1)),
        ]));
    let key = fx.key();
    let main = fx.wt("main");
    let feature = fx.wt("feature");
    let running = sess("fake", "s1", &key, Some(&main))
        .title("Fix the login redirect")
        .open_turn()
        .ago(2 * MINUTE);
    let codex = sess("fake", "s2", &key, Some(&main))
        .title("Add the search index")
        .agent(Agent::Codex)
        .ago(3 * 60 * MINUTE);
    let linked = sess("fake", "s3", &key, Some(&feature))
        .title("Try a new layout")
        .ago(DAY);
    let done = sess("fake", "s4", &key, Some(&main))
        .title("Rename the config keys")
        .ago(2 * DAY);
    let other = sess("fake", "s5", &key, Some(&main))
        .untitled()
        .agent(Agent::Other("aider".into()))
        .ago(5 * DAY);
    let changed = sess("fake", "s6", &key, Some(&main))
        .title("Tidy the error messages")
        .ago(3 * DAY);
    let keys = [
        running.key(),
        codex.key(),
        linked.key(),
        done.key(),
        other.key(),
        changed.key(),
    ];
    rig.projects.set(fx.snapshot());
    rig.sessions.set(
        [running, codex, linked, done, other, changed]
            .into_iter()
            .map(|s| s.build())
            .collect(),
    );
    let [k_running, k_codex, k_linked, k_done, k_other, k_changed] = keys;
    let mut ov = overview(
        &key,
        vec![
            entry(&k_running, progress(2, 5, 0), Some(true), 1),
            entry(&k_codex, progress(0, 3, 0), Some(false), 0),
            entry(&k_done, progress(4, 4, 0), Some(false), 0),
            entry(&k_other, None, None, 0),
            entry(&k_changed, progress(3, 3, 2), Some(false), 0),
        ],
        OutsideCount::Files(4),
    );
    ov.packages = Some(Arc::from(vec![(Arc::from("web"), 3u32)]));
    rig.review.set_overview(Some(ov));
    rig.activate_home();
    rig.opened(&key);
    rig.run();
    World {
        key,
        main,
        running: k_running,
        codex: k_codex,
        linked: k_linked,
        done: k_done,
        other: k_other,
        changed: k_changed,
    }
}

/// A usage plugin with a 7-day total and one limit at 61 %, and the entry
/// `usage_view` registers; composed again.
pub(crate) fn with_usage(rig: &Rig) {
    rig.usage
        .set_snapshot(Some((*project_usage(UsageState::Ready, 12_345)).clone()));
    rig.usage
        .set_limits((*usage_limits(vec![(Agent::ClaudeCode, 61.0)], Some(0))).clone());
    rig.activate_usage();
    rig.run();
    rig.contributions.add_entry(
        SidebarSection::Bottom,
        "usage",
        0,
        SidebarItem {
            label: keys::workspace::BACK,
            icon: None,
            on_select: WorkspaceCommand::Pop,
        },
    );
}

/// A window with the sidebar and the screen built by the factories the plugin
/// registered, the screen's key bindings bound, and the first frames drawn.
pub(crate) fn window<'a>(
    cx: &'a mut TestAppContext,
    rig: &Rig,
) -> (Entity<Host>, &'a mut VisualTestContext) {
    let screen: Screen = rig
        .contributions
        .payload("project-home")
        .expect("the screen is registered");
    let panel: SidebarPanel = rig
        .contributions
        .payload("project")
        .expect("the sidebar panel is registered");
    let (host, vcx) = cx.add_window_view(|window, cx| {
        let target = Target::None;
        let sidebar = (panel.build)(&(), window, cx);
        let screen_view = (screen.build)(&target, window, cx);
        Host {
            sidebar: Some(sidebar),
            screen: Some(screen_view),
            focus: cx.focus_handle(),
            overlay: cx.focus_handle(),
        }
    });
    vcx.update(|_, cx| cx.bind_keys(rig.contributions.key_bindings(cx)));
    frame(vcx);
    (host, vcx)
}

/// Lets the views take what was sent to them, then draws two frames (the
/// second shows what the first one's callbacks changed).
pub(crate) fn frame(vcx: &mut VisualTestContext) {
    vcx.run_until_parked();
    vcx.update(|window, cx| window.draw(cx).clear(cx));
    vcx.update(|window, cx| window.draw(cx).clear(cx));
}

/// Draws a frame with the recorder armed and returns the records.
pub(crate) fn record(vcx: &mut VisualTestContext) -> Vec<Record> {
    let id = vcx.update(|window, _| window.window_handle().window_id());
    Recorder::begin(id);
    vcx.update(|window, cx| window.draw(cx).clear(cx));
    Recorder::finish()
}

/// The semantic tree of one frame as indented text: `id Role`, the label (its
/// message key, or `"text"` when verbatim), the state flags that are set, the
/// attributes, and `hidden` for a node outside the content mask. No bounds
/// and no call sites, which a layout change would move.
pub(crate) fn tree_of(records: &[Record], keep: impl Fn(&str) -> bool) -> String {
    let mut out = String::new();
    for record in records {
        let semantic = &record.semantic;
        if !keep(semantic.id.as_str()) {
            continue;
        }
        let mut depth = 0;
        let mut parent = record.parent;
        while let Some(index) = parent {
            // Only the nodes that are kept count as ancestors in the text.
            if keep(records[index].semantic.id.as_str()) {
                depth += 1;
            }
            parent = records[index].parent;
        }
        let _ = write!(
            out,
            "{}{} {:?}",
            "  ".repeat(depth),
            semantic.id.as_str(),
            semantic.role
        );
        if let Some(label) = &semantic.label {
            match label.key {
                Some(key) => {
                    let _ = write!(out, " label=[{key}]");
                }
                None => {
                    let _ = write!(out, " label={:?}", label.text.as_ref());
                }
            }
        }
        let state = &semantic.state;
        for (set, name) in [
            (state.selected, "selected"),
            (state.focused, "focused"),
            (state.disabled, "disabled"),
        ] {
            if set {
                let _ = write!(out, " {name}");
            }
        }
        for (key, value) in &semantic.attrs {
            let _ = write!(out, " {key}={value}");
        }
        if !record.visible {
            out.push_str(" hidden");
        }
        out.push('\n');
    }
    out
}

/// The tree of a new frame.
pub(crate) fn tree(vcx: &mut VisualTestContext, keep: impl Fn(&str) -> bool) -> String {
    let records = record(vcx);
    tree_of(&records, keep)
}

pub(crate) fn sidebar_part(id: &str) -> bool {
    id.starts_with("project-home.sidebar")
}

pub(crate) fn main_part(id: &str) -> bool {
    !sidebar_part(id)
}

/// Clicks the centre of the node `id` of a new frame.
pub(crate) fn click(vcx: &mut VisualTestContext, id: &str) {
    let records = record(vcx);
    let found = records
        .iter()
        .find(|record| record.semantic.id.as_str() == id && record.visible)
        .unwrap_or_else(|| panic!("node {id} is not on screen"));
    let centre = found.bounds.center();
    vcx.simulate_click(centre, Modifiers::none());
    vcx.run_until_parked();
}

/// The screen and the sidebar views of a window.
pub(crate) fn views(
    host: &Entity<Host>,
    vcx: &mut VisualTestContext,
) -> (Entity<HomeSidebar>, Entity<HomeScreen>) {
    vcx.update(|_, cx| {
        let host = host.read(cx);
        let sidebar = host.sidebar.clone().expect("a sidebar").downcast().unwrap();
        let screen = host.screen.clone().expect("a screen").downcast().unwrap();
        (sidebar, screen)
    })
}
