//! The `#[gpui::test]`s of step 1: initialisation, the palette swap, the
//! locale switch, `ready`'s once-guard, `attach`, the `contrib` payloads and
//! the log canary.

use std::any::Any;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    Context, IntoElement, ParentElement, Render, Styled, TestAppContext, VisualTestContext, Window,
    WindowHandle, div, px,
};
use gpui_component::{Theme, ThemeMode};
use lectern_i18n::{Locale, Strings, keys};
use lectern_platform::appearance::Change;
use lectern_services::workspace::WorkspaceCommand;

use crate::contrib::{KeyBindings, Overlay, SidebarItem};
use crate::semantic::{NodeId, Recorder, Role, Semantic, semantic};
use crate::testing::{simulate_appearance_change, simulate_platform_change};
use crate::{
    Appearance, Button, GroupedList, Icon, Row, Text, accent_follows_system, attach, colors, init,
    init_fixed, is_ready, palette, ready, set_locale, strings,
};

/// A root view that counts its renders.
struct Counter(Rc<Cell<usize>>);

impl Render for Counter {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.0.set(self.0.get() + 1);
        div()
    }
}

fn fixed(cx: &mut TestAppContext, appearance: Appearance) {
    cx.update(|cx| init_fixed(cx, Strings::of(Locale::En), appearance, false));
}

fn counted_window(cx: &mut TestAppContext) -> (WindowHandle<Counter>, Rc<Cell<usize>>) {
    let renders = Rc::new(Cell::new(0));
    let window = cx.add_window({
        let renders = renders.clone();
        move |_, _| Counter(renders)
    });
    cx.run_until_parked();
    (window, renders)
}

#[gpui::test]
fn appearance_change_swaps_palette(cx: &mut TestAppContext) {
    fixed(cx, Appearance::Light);
    let (window, renders) = counted_window(cx);
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    let (light, dark) = cx.update(|window, cx| {
        assert_eq!(colors(window, cx), palette(Appearance::Light, cx));
        assert!(matches!(Theme::global(cx).mode, ThemeMode::Light));
        (
            *palette(Appearance::Light, cx),
            *palette(Appearance::Dark, cx),
        )
    });
    assert_ne!(light, dark);
    let before = renders.get();

    cx.update(|_, cx| simulate_appearance_change(Appearance::Dark, cx));
    cx.run_until_parked();

    cx.update(|window, cx| {
        assert_eq!(*colors(window, cx), dark);
        assert!(matches!(Theme::global(cx).mode, ThemeMode::Dark));
        // The projection followed: the theme's background is the dark palette's.
        assert_eq!(Theme::global(cx).colors.background, dark.window_background);
    });
    assert_eq!(renders.get(), before + 1, "the window is refreshed once");
}

#[gpui::test]
fn set_locale_refreshes_windows(cx: &mut TestAppContext) {
    fixed(cx, Appearance::Light);
    let (_first, first_renders) = counted_window(cx);
    let (_second, second_renders) = counted_window(cx);
    let before = (first_renders.get(), second_renders.get());
    cx.update(|cx| assert_eq!(strings(cx).locale(), Locale::En));

    cx.update(|cx| set_locale(Locale::ZhCn, cx));
    cx.run_until_parked();

    cx.update(|cx| {
        assert_eq!(strings(cx).locale(), Locale::ZhCn);
        assert_eq!(
            Text::key(&strings(cx), keys::common::OK).as_str(),
            Strings::of(Locale::ZhCn).get(keys::common::OK)
        );
    });
    assert_eq!(first_renders.get(), before.0 + 1);
    assert_eq!(second_renders.get(), before.1 + 1);

    // The same locale again is not a change: no refresh.
    cx.update(|cx| set_locale(Locale::ZhCn, cx));
    cx.run_until_parked();
    assert_eq!(first_renders.get(), before.0 + 1);
}

#[gpui::test]
fn ready_runs_component_init_once(cx: &mut TestAppContext) {
    cx.update(|cx| assert!(!is_ready(cx)));
    fixed(cx, Appearance::Dark);
    cx.update(|cx| {
        assert!(is_ready(cx));
        assert!(matches!(Theme::global(cx).mode, ThemeMode::Dark));
        // A mark that a second `gpui_component::init` would reset.
        Theme::update(cx, |theme| theme.radius = px(99.0));
        ready(cx);
        assert_eq!(Theme::global(cx).radius, px(99.0));
        assert!(matches!(Theme::global(cx).mode, ThemeMode::Dark));
    });
}

#[gpui::test]
fn attach_registers_once_per_window(cx: &mut TestAppContext) {
    fixed(cx, Appearance::Light);
    let (window, _) = counted_window(cx);
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    let (first, second) = cx.update(|window, cx| (attach(window, cx), attach(window, cx)));
    cx.update(|_, cx| assert_eq!(cx.global::<crate::state::UiState>().attached.len(), 1));
    drop(second);
    drop(first);
}

/// `init` and `ready` against the real system: the only test that reads
/// AppKit. On macOS both appearances resolve and the accent follows the
/// system; elsewhere the built-in table stays and the warn is logged.
#[gpui::test]
fn init_then_ready_reads_the_system(cx: &mut TestAppContext) {
    cx.update(|cx| {
        init(cx, Strings::of(Locale::En));
        assert!(!is_ready(cx));
        assert!(!accent_follows_system(cx));
        let builtin = *palette(Appearance::Light, cx);
        ready(cx);
        assert!(is_ready(cx));
        if cfg!(target_os = "macos") {
            assert!(accent_follows_system(cx));
        } else {
            assert_eq!(*palette(Appearance::Light, cx), builtin);
        }
        // The one that resolves through the window's appearance.
        let light = palette(Appearance::Light, cx).label;
        let dark = palette(Appearance::Dark, cx).label;
        assert!(
            light.l < 0.5 && dark.l > 0.5,
            "light {light:?}, dark {dark:?}"
        );
        simulate_platform_change(Change::SystemColors, cx);
        simulate_platform_change(Change::ReduceMotion, cx);
        assert_eq!(palette(Appearance::Light, cx).label, light);
    });
}

#[test]
fn contrib_payloads_round_trip_and_are_send() {
    fn assert_send<T: Send>() {}
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send::<crate::contrib::Screen>();
    assert_send::<crate::contrib::SidebarPanel>();
    assert_send_sync::<SidebarItem>();
    assert_send::<crate::contrib::InspectorTab>();
    assert_send::<crate::contrib::Panel>();
    assert_send::<Overlay>();
    assert_send::<KeyBindings>();

    let item = SidebarItem {
        label: keys::common::OK,
        icon: Some(Icon::Folder),
        on_select: WorkspaceCommand::PromptOpenProject,
    };
    let payload = item.into_payload();
    let item: Box<SidebarItem> = (payload.0 as Box<dyn Any>)
        .downcast()
        .expect("the payload is the item");
    assert_eq!(item.icon, Some(Icon::Folder));
    assert_eq!(item.on_select, WorkspaceCommand::PromptOpenProject);
    assert_eq!(item.label.name(), keys::common::OK.name());

    let bindings = KeyBindings(Vec::new()).into_payload();
    assert!(
        (bindings.0 as Box<dyn Any>)
            .downcast::<KeyBindings>()
            .is_ok()
    );
}

/// A root view that renders the step-3 controls with the canary as their
/// text and as a business key.
struct CanaryControls(&'static str);

impl Render for CanaryControls {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let canary = self.0;
        let text = || Text::verbatim(Arc::<str>::from(canary));
        let list = NodeId::new("canary", "list", None);
        div()
            .child(Button::new(list.child("button"), text()))
            // A node of a view's own: the canary as a business key, in the
            // label and in an attribute.
            .child(semantic(
                Semantic::new(list.item(canary), Role::Group)
                    .label(&text())
                    .attr("note", canary),
                div().size(px(4.0)),
            ))
            .child(
                GroupedList::new(list.clone()).header(text()).row(
                    Row::new(list.item(canary))
                        .selected(true)
                        .child(text().shared()),
                ),
            )
    }
}

/// The canary (docs/design/ui.md「日志与诊断」): text, keys, controls and
/// appearance changes leave only the event names in the log.
#[gpui::test]
fn logs_hold_no_text(cx: &mut TestAppContext) {
    let canary = "canary-5b1d9e7f";
    let capture = lectern_logging::testing::capture();
    capture.clear();
    fixed(cx, Appearance::Light);
    let controls = cx.add_window(|_, _| CanaryControls(canary));
    cx.run_until_parked();
    // One frame with the recorder armed: the canary reaches the records,
    // not the log.
    {
        let mut visual = VisualTestContext::from_window(controls.into(), cx);
        Recorder::begin(controls.window_id());
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let records = Recorder::finish();
        assert!(
            records
                .iter()
                .any(|record| record.semantic.id.as_str().contains(canary)),
            "the canary node was recorded"
        );
    }
    let (window, _) = counted_window(cx);
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    cx.update(|window, cx| {
        let verbatim = Text::verbatim(Arc::<str>::from(canary));
        assert_eq!(verbatim.as_str(), canary);
        let _ = Text::verbatim(canary.to_owned()).shared();
        let _ = Text::key(&strings(cx), keys::common::OK);
        let _ = colors(window, cx);
        let _ = crate::mono_metrics(window, cx);
        set_locale(Locale::ZhCn, cx);
        simulate_appearance_change(Appearance::Dark, cx);
    });
    cx.run_until_parked();

    capture.assert_absent(canary);
    let messages: Vec<String> = capture
        .lines()
        .into_iter()
        .filter(|line| line.contains("\"tg\":\"ui\""))
        .map(|line| {
            let start = line.find("\"msg\":\"").expect("every record has a msg") + 7;
            let end = line[start..].find('"').expect("the msg closes") + start;
            line[start..end].to_owned()
        })
        .collect();
    assert_eq!(
        messages,
        [
            "ui initialised",
            "ui ready",
            "locale changed",
            "appearance changed"
        ],
        "{:#?}",
        capture.lines()
    );
}
