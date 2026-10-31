//! The GPUI rig of the step-2 tests (docs/design/worktrees.md「测试」):
//! `lectern_ui::init_fixed`, a test window that holds the screen and the
//! inspector's page the way `workspace` would (the screen in the `Content`
//! region with the `Content` context, the page in a 300pt region, the
//! toolbar's controls above), the real plugins in a real kernel with the fake
//! services, and the semantic tree as compact text.
//!
//! `#[gpui::test]` draws frames only when a test asks; `frame` does. No test
//! here opens a real window: the windows are GPUI's test windows.

use std::fmt::Write as _;
use std::sync::Arc;

use gpui::{
    AnyView, Context, Entity, FocusHandle, InteractiveElement, IntoElement, Modifiers,
    ParentElement, Render, Styled, TestAppContext, VisualTestContext, Window, div, px,
};
use lectern_i18n::{Locale, Strings};
use lectern_services::project::{ProjectKey, WorktreeKey};
use lectern_services::session::SessionKey;
use lectern_services::usage::UsageState;
use lectern_services::workspace::Target;
use lectern_ui::contrib::{InspectorTab, Screen, ToolbarItems};
use lectern_ui::semantic::{Record, Recorder};
use lectern_ui::{Appearance, init_fixed};

use crate::model::WorktreesModel;
use crate::screen::WorktreesScreen;
use crate::testing::{
    DAY, MINUTE, ProjectFx, Rig, branch, branch_usage, counted, overlap, overview_of, sess, tokens,
    worktree_query, worktree_snapshot,
};
use lectern_services::session::Agent;

type Toolbar = Arc<dyn Fn(&mut gpui::App) -> ToolbarItems + Send + Sync>;

/// The toolbar, the screen and the inspector's page side by side, as
/// `workspace` lays them out.
pub(crate) struct Host {
    pub toolbar: Option<Toolbar>,
    pub screen: Option<AnyView>,
    pub inspector: Option<AnyView>,
    /// The workspace's root view takes the focus when a popped screen had it.
    pub focus: FocusHandle,
}

impl Render for Host {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut column = div()
            .size_full()
            .flex()
            .flex_col()
            .key_context("Workspace")
            .track_focus(&self.focus);
        if let Some(toolbar) = &self.toolbar {
            let items = toolbar(cx);
            column = column.child(div().flex().flex_row().children(items.controls));
        }
        let mut row = div().flex_1().flex().flex_row();
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
        if let Some(inspector) = &self.inspector {
            row = row.child(
                div()
                    .key_context("Inspector")
                    .w(px(300.0))
                    .h_full()
                    .child(inspector.clone()),
            );
        }
        column.child(row)
    }
}

/// The kernel and the fakes, with GPUI set up (call first in a test).
pub(crate) fn rig(cx: &mut TestAppContext) -> Rig {
    rig_with(cx, false)
}

/// The same with the system's reduce-motion setting on or off.
pub(crate) fn rig_with(cx: &mut TestAppContext, reduced: bool) -> Rig {
    // The kernel's threads wake the views' foreground tasks; the test
    // scheduler accepts a wake from another thread only with parking allowed.
    cx.executor().allow_parking();
    cx.update(|cx| init_fixed(cx, Strings::of(Locale::En), Appearance::Light, reduced));
    Rig::new()
}

/// The same with the interface in another language.
pub(crate) fn rig_in(cx: &mut TestAppContext, locale: Locale) -> Rig {
    cx.executor().allow_parking();
    cx.update(|cx| init_fixed(cx, Strings::of(locale), Appearance::Light, false));
    Rig::new()
}

/// The keys of the standard fixture.
pub(crate) struct World {
    pub key: ProjectKey,
    pub main: WorktreeKey,
    pub feat: WorktreeKey,
    pub fix: WorktreeKey,
    pub old: WorktreeKey,
    /// Running in `main`.
    pub running: SessionKey,
    /// Codex, in `feat`.
    pub codex: SessionKey,
}

pub(crate) const BASE: &str = "refs/remotes/origin/main";

/// A git project with four worktrees (`main` is current and has a running
/// session; `feat` has two idle sessions and a branch usage; `fix` is locked
/// and detached; `old` is prunable), counts that have arrived and two
/// overlapping worktrees, the usage plugin on, opened and composed with the
/// screen shown.
pub(crate) fn standard(rig: &Rig) -> World {
    standard_with(rig, Some(BASE))
}

pub(crate) fn project_fx() -> ProjectFx {
    ProjectFx::git("app")
        .linked("feat", branch("feat/search"))
        .linked(
            "fix",
            lectern_services::project::Head::Detached(Arc::from("0123456789abcdef0123")),
        )
        .locked("fix")
        .prunable("old", branch("old/idea"))
}

pub(crate) fn standard_with(rig: &Rig, base: Option<&str>) -> World {
    let fx = project_fx();
    let key = fx.key();
    let (main, feat, fix, old) = (fx.wt("main"), fx.wt("feat"), fx.wt("fix"), fx.wt("old"));
    let running = sess("fake", "s1", &key, Some(&main))
        .title("Fix the login redirect")
        .open_turn()
        .ago(2 * MINUTE);
    let codex = sess("fake", "s2", &key, Some(&feat))
        .title("Add the search index")
        .agent(Agent::Codex)
        .ago(180 * MINUTE);
    let untitled = sess("fake", "s3", &key, Some(&feat)).untitled().ago(DAY);
    let keys = [running.key(), codex.key(), untitled.key()];
    rig.projects.set(fx.snapshot());
    rig.sessions.set(
        [running, codex, untitled]
            .into_iter()
            .map(|s| s.build())
            .collect(),
    );
    let paths: Vec<String> = (0..10).map(|i| format!("src/shared/file{i}.rs")).collect();
    let path_refs: Vec<&str> = paths.iter().map(String::as_str).collect();
    rig.review.set_overview(Some(overview_of(
        &key,
        base,
        vec![
            counted(&main, 2, 3, 5, vec![overlap(&feat, 12, &path_refs)]),
            counted(&feat, 1, 4, 5, vec![overlap(&main, 12, &path_refs)]),
            counted(&fix, 0, 0, 0, vec![]),
        ],
    )));
    // `main` is the default branch (not counted); `feat` has a branch usage.
    rig.usage.set_snapshot(
        worktree_query(&key, &main),
        worktree_snapshot(branch_usage(UsageState::Ready, None, vec![])),
    );
    rig.usage.set_snapshot(
        worktree_query(&key, &feat),
        worktree_snapshot(branch_usage(
            UsageState::Ready,
            Some(tokens(9_000, 3_345)),
            vec![],
        )),
    );
    rig.usage.set_snapshot(
        worktree_query(&key, &fix),
        worktree_snapshot(branch_usage(UsageState::Ready, None, vec![])),
    );
    rig.activate_main();
    rig.activate_usage();
    rig.opened(&key);
    rig.show();
    rig.run();
    // The wanted queries were taken after the first composition; the second
    // one reads what the usage snapshots say.
    rig.run();
    let [k_running, k_codex, _] = keys;
    World {
        key,
        main,
        feat,
        fix,
        old,
        running: k_running,
        codex: k_codex,
    }
}

/// A window with the screen and the page built by the factories the plugin
/// registered, the screen's key bindings bound, and the first frames drawn.
pub(crate) fn window<'a>(
    cx: &'a mut TestAppContext,
    rig: &Rig,
) -> (Entity<Host>, &'a mut VisualTestContext) {
    let screen: Screen = rig
        .contributions
        .payload("worktrees")
        .expect("the screen is registered");
    let tab: InspectorTab = rig
        .contributions
        .payload("worktree")
        .expect("the page is registered");
    let toolbar = screen.toolbar.clone();
    let (host, vcx) = cx.add_window_view(|window, cx| {
        let target = Target::None;
        let screen_view = (screen.build)(&target, window, cx);
        // The page is built after the screen, as `workspace` does: the
        // screen's model is the one it reads.
        let page = (tab.build)(&(), window, cx);
        Host {
            toolbar: Some(toolbar),
            screen: Some(screen_view),
            inspector: Some(page),
            focus: cx.focus_handle(),
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
        if let Some(expanded) = state.expanded {
            let _ = write!(out, " expanded={expanded}");
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

/// The inspector's nodes.
pub(crate) fn detail_part(id: &str) -> bool {
    id.starts_with("worktrees.detail")
}

/// Everything but the inspector's nodes and the toolbar's.
pub(crate) fn main_part(id: &str) -> bool {
    !detail_part(id) && !id.starts_with("worktrees.toolbar")
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

/// The row's node ID for the worktree `key`.
pub(crate) fn row_id(key: &WorktreeKey) -> String {
    crate::nodes::row(&key.0).as_str().to_owned()
}

/// The screen and its model.
pub(crate) fn model_of(host: &Entity<Host>, vcx: &mut VisualTestContext) -> Entity<WorktreesModel> {
    vcx.update(|_, cx| {
        let screen: Entity<WorktreesScreen> = host
            .read(cx)
            .screen
            .clone()
            .expect("a screen")
            .downcast()
            .unwrap();
        screen.read(cx).model().clone()
    })
}

/// The selected worktree.
pub(crate) fn selected(
    model: &Entity<WorktreesModel>,
    vcx: &mut VisualTestContext,
) -> Option<WorktreeKey> {
    vcx.update(|_, cx| model.read(cx).selected.clone())
}
