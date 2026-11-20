//! The reader screen's registration (docs/design/file_reader.md「本 crate
//! 的公开项」「界面」): the factory `workspace` calls for each push of
//! `screens::READER`, the toolbar's title, subtitle and buttons, and the
//! layout. The state is the [`ReadingView`]'s; the toolbar reads the view
//! on top through a global, as `workspace` calls it every frame without a
//! window.

use std::sync::Arc;

use gpui::{AnyView, App, AppContext, Entity, Global, IntoElement, WeakEntity, Window};
use lectern_i18n::{Args, Strings, keys};
use lectern_kernel::Name;
use lectern_services::code_intel::TextOrigin;
use lectern_services::reader::ReadTarget;
use lectern_services::workspace::{
    Contribution, ContributionKind, ScreenLayout, Target, Visibility, screens,
};
use lectern_ui::contrib::{KeyBindings, Screen, ToolbarItems};
use lectern_ui::{Button, Icon, Text};

use crate::actions;
use crate::nodes;
use crate::shared::Shared;
use crate::text::{dependency_label, file_name, short_oid};
use crate::view::ReadingView;

/// The reader screen's layout: it takes the whole content area (the code is
/// not folded to the reading column), has no sidebar, no floating bar and
/// no inspector (the pages contributed so far are the review's), and no
/// back button of `workspace`'s: its own back and forward buttons are the
/// history's.
pub const LAYOUT: ScreenLayout = ScreenLayout {
    sidebar: Visibility::Hidden,
    back_button: false,
    reading_column: false,
    inspector: false,
    floating_bar: false,
};

/// The view on top, for the toolbar.
struct Current(WeakEntity<ReadingView>);

impl Global for Current {}

/// The two contributions of the reader: the screen and its key bindings.
pub(crate) fn registrations(shared: &Arc<Shared>) -> Vec<Contribution> {
    let build_shared = shared.clone();
    let screen = Screen {
        build: Arc::new(move |_: &Target, window: &mut Window, cx: &mut App| {
            build(&build_shared, window, cx)
        }),
        toolbar: Arc::new(toolbar),
    };
    vec![
        Contribution {
            kind: ContributionKind::Screen(LAYOUT),
            id: Name::Borrowed(screens::READER),
            order: 0,
            payload: screen.into_payload(),
        },
        Contribution {
            kind: ContributionKind::KeyBindings,
            id: Name::Borrowed(actions::KEY_BINDINGS_ID),
            order: 0,
            payload: KeyBindings(actions::key_bindings()).into_payload(),
        },
    ]
}

/// The factory of `Screen("reader")` (main thread). The target of the route
/// is not used: what the view shows is what the session was asked to open,
/// which it takes as soon as it exists.
fn build(shared: &Arc<Shared>, window: &mut Window, cx: &mut App) -> AnyView {
    let view = cx.new(|cx| ReadingView::new(shared.clone(), window, cx));
    cx.set_global(Current(view.downgrade()));
    let focus = view.read(cx).focus.clone();
    window.focus(&focus, cx);
    // A request is waiting (it is why the screen was pushed): take it now.
    view.update(cx, |view, cx| view.pump(cx));
    view.into()
}

fn current(cx: &App) -> Option<Entity<ReadingView>> {
    cx.try_global::<Current>()?.0.upgrade()
}

/// The toolbar (`Screen::toolbar`): the file's name, its path, and the
/// history's back and forward buttons.
fn toolbar(cx: &mut App) -> ToolbarItems {
    let strings = lectern_ui::strings(cx);
    let Some(view) = current(cx) else {
        return ToolbarItems {
            title: Text::verbatim(String::new()),
            subtitle: None,
            controls: Vec::new(),
        };
    };
    let (title, subtitle, forward) = {
        let view = view.read(cx);
        let target = view
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.target.clone());
        (
            target.as_ref().map(title_of),
            target.as_ref().map(|t| subtitle_of(t, &strings)),
            view.can_forward(),
        )
    };
    let weak = view.downgrade();
    let back = {
        let weak = weak.clone();
        Button::icon_only(
            nodes::plain("toolbar").child("back"),
            Icon::Back,
            Text::key(&strings, keys::file_reader::BACK_TOOLTIP),
        )
        .on_click(move |_, cx| {
            let _ = weak.update(cx, |view, cx| view.go_back(cx));
        })
    };
    let forward = Button::icon_only(
        nodes::plain("toolbar").child("forward"),
        Icon::Forward,
        Text::key(&strings, keys::file_reader::FORWARD_TOOLTIP),
    )
    .disabled(!forward)
    .on_click(move |_, cx| {
        let _ = weak.update(cx, |view, cx| view.go_forward(cx));
    });
    ToolbarItems {
        title: title.unwrap_or_else(|| Text::verbatim(String::new())),
        subtitle,
        controls: vec![back.into_any_element(), forward.into_any_element()],
    }
}

/// The file's name.
fn title_of(target: &ReadTarget) -> Text {
    Text::verbatim(file_name(&target.path).to_owned())
}

/// The path; a dependency's package and version in front of it, and an
/// object's source after it.
fn subtitle_of(target: &ReadTarget, strings: &Strings) -> Text {
    let mut line = String::new();
    if let TextOrigin::Dependency(_) = &target.origin
        && let Some(package) = dependency_label(&target.path)
    {
        line.push_str(&package);
        line.push_str(" \u{00B7} ");
    }
    line.push_str(&target.path);
    if target.origin == TextOrigin::Object
        && let Some(oid) = target.expect
    {
        let source = short_oid(oid);
        line.push_str(" \u{00B7} ");
        line.push_str(&strings.format(
            keys::file_reader::FROM_HISTORY,
            &Args::new().str("source", &source),
        ));
    }
    Text::verbatim(line)
}
