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
    Contribution, ContributionKind, ScreenLayout, SidebarSection, Target, Visibility,
    WorkspaceCommand, screens,
};
use lectern_syntax::NotEditable;
use lectern_ui::contrib::{KeyBindings, Screen, SidebarItem, SidebarPanel, ToolbarItems};
use lectern_ui::{Button, ButtonKind, ButtonSize, Icon, Text};

use crate::actions;
use crate::edit::reason_key;
use crate::files;
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

/// Where the Files entry stands among the project sidebar's `Views`
/// entries: after Worktrees (order 0), before Merge Requests.
const FILES_ENTRY_ORDER: u16 = 10;

/// The contributions of this crate: the reader screen and its key bindings
/// (the Files tree's keys too), the Files panel and its entry in the
/// project sidebar.
pub(crate) fn registrations(shared: &Arc<Shared>) -> Vec<Contribution> {
    let build_shared = shared.clone();
    let screen = Screen {
        build: Arc::new(move |_: &Target, window: &mut Window, cx: &mut App| {
            build(&build_shared, window, cx)
        }),
        toolbar: Arc::new(toolbar),
    };
    let files_shared = shared.clone();
    let panel = SidebarPanel {
        build: Arc::new(move |_: &(), window: &mut Window, cx: &mut App| {
            files::build(&files_shared, window, cx)
        }),
    };
    let entry = SidebarItem {
        label: keys::file_reader::FILES_TITLE,
        icon: Some(Icon::Files),
        on_select: WorkspaceCommand::ShowSidebarPanel(Some(Name::Borrowed(files::PANEL_ID))),
    };
    vec![
        Contribution {
            kind: ContributionKind::SidebarPanel,
            id: Name::Borrowed(files::PANEL_ID),
            order: 0,
            payload: panel.into_payload(),
        },
        Contribution {
            kind: ContributionKind::SidebarEntry {
                section: SidebarSection::Views,
            },
            id: Name::Borrowed(files::PANEL_ID),
            order: FILES_ENTRY_ORDER,
            payload: entry.into_payload(),
        },
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
            payload: KeyBindings(actions::key_bindings(shared.edit_reachable())).into_payload(),
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
    view.update(cx, |view, cx| view.pump(window, cx));
    view.into()
}

fn current(cx: &App) -> Option<Entity<ReadingView>> {
    cx.try_global::<Current>()?.0.upgrade()
}

/// The toolbar (`Screen::toolbar`): the file's name, its path, and the
/// history's back and forward buttons; Edit… where the edit mode is
/// reachable, and Done and Save while editing. `workspace` shows at most
/// three controls, so while editing the history has only the back button
/// (greyed): the forward one would push Save out.
fn toolbar(cx: &mut App) -> ToolbarItems {
    let strings = lectern_ui::strings(cx);
    let Some(view) = current(cx) else {
        return ToolbarItems {
            title: Text::verbatim(String::new()),
            subtitle: None,
            controls: Vec::new(),
        };
    };
    let (title, subtitle, forward, edit) = {
        let view = view.read(cx);
        let target = view
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.target.clone());
        (
            target.as_ref().map(title_of),
            target.as_ref().map(|t| subtitle_of(t, &strings)),
            view.can_forward(),
            EditFacts {
                reachable: view.shared.edit_reachable(),
                editing: view.editing(),
                reason: view.not_editable(),
                dirty: view.edit.as_ref().is_some_and(|edit| edit.dirty()),
                anyway: view.save_anyway(),
            },
        )
    };
    let weak = view.downgrade();
    let back_key = if edit.editing {
        keys::file_reader::BACK_EDITING_TOOLTIP
    } else {
        keys::file_reader::BACK_TOOLTIP
    };
    let back = {
        let weak = weak.clone();
        Button::icon_only(
            nodes::plain("toolbar").child("back"),
            Icon::Back,
            Text::key(&strings, back_key),
        )
        .tooltip(Text::key(&strings, back_key))
        .disabled(edit.editing)
        .on_click(move |window, cx| {
            let _ = weak.update(cx, |view, cx| view.go_back(window, cx));
        })
    };
    let mut controls = vec![back.into_any_element()];
    if edit.editing {
        let done = {
            let weak = weak.clone();
            Button::new(
                nodes::plain("toolbar").child("done"),
                Text::key(&strings, keys::file_reader::DONE),
            )
            .kind(ButtonKind::Secondary)
            .size(ButtonSize::Toolbar)
            .on_click(move |window, cx| {
                let _ = weak.update(cx, |view, cx| {
                    view.leave_edit(window, cx);
                });
            })
        };
        let anyway = edit.anyway;
        let save = Button::new(
            nodes::plain("toolbar").child("save"),
            Text::key(
                &strings,
                if anyway {
                    keys::file_reader::SAVE_ANYWAY
                } else {
                    keys::file_reader::SAVE
                },
            ),
        )
        .kind(ButtonKind::Primary)
        .size(ButtonSize::Toolbar)
        .disabled(!edit.dirty)
        .on_click(move |_, cx| {
            let _ = weak.update(cx, |view, cx| view.save(anyway, cx));
        });
        controls.push(done.into_any_element());
        controls.push(save.into_any_element());
    } else {
        let forward = {
            let weak = weak.clone();
            Button::icon_only(
                nodes::plain("toolbar").child("forward"),
                Icon::Forward,
                Text::key(&strings, keys::file_reader::FORWARD_TOOLTIP),
            )
            .tooltip(Text::key(&strings, keys::file_reader::FORWARD_TOOLTIP))
            .disabled(!forward)
            .on_click(move |window, cx| {
                let _ = weak.update(cx, |view, cx| view.go_forward(window, cx));
            })
        };
        controls.push(forward.into_any_element());
        if edit.reachable {
            // A grey button says why in its tooltip: the reason's text.
            let mut button = Button::new(
                nodes::plain("toolbar").child("edit"),
                Text::key(&strings, keys::file_reader::EDIT),
            )
            .kind(ButtonKind::Secondary)
            .size(ButtonSize::Toolbar)
            .disabled(edit.reason.is_some())
            .on_click(move |window, cx| {
                let _ = weak.update(cx, |view, cx| {
                    view.enter_edit(window, cx);
                });
            });
            let tooltip = match edit.reason {
                Some(reason) => {
                    let key = reason_key(reason);
                    button = button.attr("reason", key.name());
                    key
                }
                None => keys::file_reader::EDIT_TOOLTIP,
            };
            button = button.tooltip(Text::key(&strings, tooltip));
            controls.push(button.into_any_element());
        }
    }
    ToolbarItems {
        title: title.unwrap_or_else(|| Text::verbatim(String::new())),
        subtitle,
        controls,
    }
}

/// What the toolbar needs to know of the edit mode.
struct EditFacts {
    reachable: bool,
    editing: bool,
    /// Why the document cannot be edited now.
    reason: Option<NotEditable>,
    dirty: bool,
    anyway: bool,
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
