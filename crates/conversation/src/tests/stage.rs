//! The GPUI rig of the view tests (docs/design/conversation.md「测试」):
//! `lectern_ui::init_fixed`, a test window that holds the page the way
//! `workspace` would (in a 300pt region), the real plugins in a real kernel
//! with the fake services, and the semantic tree as compact text.
//!
//! `#[gpui::test]` draws frames only when a test asks; `frame` does. No test
//! here opens a real window: the windows are GPUI's test windows.

use std::fmt::Write as _;

use gpui::{
    AnyView, AppContext, Context, Entity, InteractiveElement, IntoElement, Modifiers,
    ParentElement, Render, Styled, TestAppContext, VisualTestContext, Window, div, px,
};
use lectern_i18n::{Locale, Strings};
use lectern_services::session::SessionKey;
use lectern_ui::contrib::InspectorTab;
use lectern_ui::semantic::{Record, Recorder};
use lectern_ui::{Appearance, init_fixed};

use crate::plugin::TAB_ID;
use crate::testing::Rig;
use crate::view::{ConversationTab, MessagesModel, TerminalPage};

/// The inspector's region, as `workspace` lays it out.
pub(crate) struct Host {
    pub inspector: AnyView,
}

impl Render for Host {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .flex_row()
            .child(div().flex_1().h_full())
            .child(
                div()
                    .key_context("Inspector")
                    .w(px(300.0))
                    .h(px(560.0))
                    .child(self.inspector.clone()),
            )
    }
}

/// The kernel and the fakes, with GPUI set up (call first in a test).
pub(crate) fn rig(cx: &mut TestAppContext) -> Rig {
    rig_with(cx, false)
}

/// The same with the system's reduce-motion setting on or off.
pub(crate) fn rig_with(cx: &mut TestAppContext, reduced: bool) -> Rig {
    // The kernel's threads wake the page's foreground task; the test
    // scheduler accepts a wake from another thread only with parking allowed.
    cx.executor().allow_parking();
    cx.update(|cx| init_fixed(cx, Strings::of(Locale::En), Appearance::Light, reduced));
    Rig::new()
}

/// A window with the page built by the factory the plugin registered, the
/// bindings bound, and the first frames drawn. Both plugins are active and
/// the window shows the review of `session` (call `rig.activate_*` first for
/// a different set).
pub(crate) fn window<'a>(
    cx: &'a mut TestAppContext,
    rig: &Rig,
) -> (Entity<Host>, &'a mut VisualTestContext) {
    let tab: InspectorTab = rig
        .contributions
        .payload(TAB_ID)
        .expect("the page is registered");
    let (host, vcx) = cx.add_window_view(|window, cx| {
        let inspector = (tab.build)(&(), window, cx);
        Host { inspector }
    });
    vcx.update(|_, cx| cx.bind_keys(rig.contributions.key_bindings(cx)));
    frame(vcx);
    (host, vcx)
}

/// The three plugins on, the review of `key` on the screen, a window.
pub(crate) fn open<'a>(
    cx: &'a mut TestAppContext,
    rig: &Rig,
    key: &SessionKey,
) -> (Entity<Host>, &'a mut VisualTestContext) {
    rig.activate_main();
    rig.activate_live();
    rig.activate_terminal();
    rig.review(key);
    window(cx, rig)
}

/// Lets the page take what was sent to it, then draws two frames (the second
/// shows what the first one's callbacks changed).
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

/// Every node of the page.
pub(crate) fn all(id: &str) -> bool {
    id.starts_with("conversation.")
}

/// Whether the node `id` is on screen in a new frame.
pub(crate) fn on_screen(vcx: &mut VisualTestContext, id: &str) -> bool {
    record(vcx)
        .iter()
        .any(|record| record.semantic.id.as_str() == id && record.visible)
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

/// The page.
pub(crate) fn tab_of(host: &Entity<Host>, vcx: &mut VisualTestContext) -> Entity<ConversationTab> {
    vcx.update(|_, cx| host.read(cx).inspector.clone().downcast().unwrap())
}

/// The Messages page of the session on the screen, if there is one.
pub(crate) fn messages_of(
    host: &Entity<Host>,
    vcx: &mut VisualTestContext,
) -> Option<Entity<MessagesModel>> {
    let tab = tab_of(host, vcx);
    vcx.update(|_, cx| tab.read(cx).messages().cloned())
}

/// What the inspector shows while the page is hidden.
struct Blank;

impl Render for Blank {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

/// Hides the inspector's page: the page is dropped, as `workspace` drops it
/// when the inspector hides or shows another page.
pub(crate) fn hide_inspector(host: &Entity<Host>, vcx: &mut VisualTestContext) {
    let blank: AnyView = vcx.update(|_, cx| cx.new(|_| Blank).into());
    vcx.update(|_, cx| {
        host.update(cx, |host, cx| {
            host.inspector = blank;
            cx.notify();
        });
    });
    frame(vcx);
}

/// Shows the page again, built by the factory the plugin registered.
pub(crate) fn show_inspector(host: &Entity<Host>, rig: &Rig, vcx: &mut VisualTestContext) {
    let tab: InspectorTab = rig
        .contributions
        .payload(TAB_ID)
        .expect("the page is registered");
    vcx.update(|window, cx| {
        let built = (tab.build)(&(), window, cx);
        host.update(cx, |host, cx| {
            host.inspector = built;
            cx.notify();
        });
    });
    frame(vcx);
}

/// The Terminal page of the session on the screen, if the page was built.
pub(crate) fn terminal_of(
    host: &Entity<Host>,
    vcx: &mut VisualTestContext,
) -> Option<Entity<TerminalPage>> {
    let tab = tab_of(host, vcx);
    vcx.update(|_, cx| tab.read(cx).terminal().cloned())
}
