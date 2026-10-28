//! The skeleton window and the app menu (docs/design/app.md「界面」).

use gpui::{
    AnyWindowHandle, App, Bounds, Context, IntoElement, KeyBinding, Menu, MenuItem, Render,
    Subscription, TitlebarOptions, Window, WindowBackgroundAppearance, WindowBounds, WindowOptions,
    actions, div, point, prelude::*, px, size,
};
use lectern_i18n::{Strings, keys};
use lectern_review_view::actions as review;

use crate::profile::Profile;
use crate::quit::Quit;
use crate::startup::{self, Started, Startup};
use crate::window_state::{WELCOME, autosave_name};

actions!(lectern, [Hide, HideOthers, ShowAll]);

/// The window when no project is open is the Welcome width
/// (docs/design/interaction.md「窗口结构」).
const WIDTH: f32 = 820.0;
const HEIGHT: f32 = 520.0;

/// The neutral skeleton of ADR 0017: draws nothing over the system's
/// window material. Welcome and Overview replace it.
pub(crate) struct Skeleton {
    armed: bool,
    /// `lectern_ui::attach`'s observer: `colors(window, cx)` follows this
    /// window's appearance for as long as the root view lives.
    _appearance: Subscription,
    /// How many frames this view has rendered; the skeleton window tests
    /// count refreshes with it.
    #[cfg(test)]
    pub(crate) renders: usize,
}

impl Render for Skeleton {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(test)]
        {
            self.renders += 1;
        }
        if !self.armed {
            self.armed = true;
            if cx
                .try_global::<Startup>()
                .is_some_and(|s| s.profile().has_real_window())
            {
                startup::arm(window);
            }
        }
        div().size_full()
    }
}

/// Opens the skeleton window with the Welcome frame. Main thread.
pub fn open(cx: &mut App) {
    open_named(cx, WELCOME);
}

/// Opens the skeleton window and, in the desktop profile, gives it the
/// frame autosave name `frame`: AppKit applies the frame saved under it
/// before the first frame is drawn (docs/design/workspace.md「对其他 crate
/// 的要求」D). A Dock reopen with a project open passes `project`. Main
/// thread. None when the window could not be opened.
pub fn open_named(cx: &mut App, frame: &'static str) -> Option<AnyWindowHandle> {
    let desktop = cx
        .try_global::<Startup>()
        .is_some_and(|s| s.profile().has_real_window());
    // The measurement build's probe: no saved frame, so that every start draws
    // the same pixels and the user's window position stays alone
    // (docs/design/app.md「数据目录」).
    let bench = cx.try_global::<Startup>().is_some_and(|s| s.bench_probe());
    let bounds = Bounds::centered(None, size(px(WIDTH), px(HEIGHT)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions {
            title: None,
            appears_transparent: true,
            // Centred in the 52pt toolbar: (52 - 14) / 2 for 14pt buttons
            // (docs/design/workspace.md「对其他 crate 的要求」G).
            traffic_light_position: Some(point(px(20.0), px(19.0))),
        }),
        window_background: WindowBackgroundAppearance::Blurred,
        ..Default::default()
    };
    // Opening fails only without a display; there is nothing to fall back to.
    let opened = cx.open_window(options, |window, cx| {
        // One observer; `lectern_ui::init` ran before this window
        // (docs/design/ui.md「对其他 crate 的要求」).
        let appearance = lectern_ui::attach(window, cx);
        if let Some(name) = autosave_name(desktop, bench, frame) {
            // The one preference read before the first frame: AppKit's
            // in-memory user defaults, no file of ours (ADR 0017). `platform`
            // logs a failure; the window keeps the frame it was opened with.
            let _ = lectern_platform::window::set_frame_autosave_name(&*window, name);
        }
        cx.new(|_| Skeleton {
            armed: false,
            _appearance: appearance,
            #[cfg(test)]
            renders: 0,
        })
    });
    // The test profile has no real frames: release once the window exists.
    if cx.try_global::<Startup>().map(Startup::profile) == Some(Profile::Test) {
        startup::release(cx);
    }
    opened.ok().map(Into::into)
}

/// A Dock click with no window open: a new skeleton window, named for the
/// state the workspace is in, and the workspace moves into it, keeping its
/// registry and navigation state (docs/design/workspace.md「本 crate 的公开
/// 项」`attach`). Before the barrier the new window only waits for it, as the
/// first one does. Main thread.
pub fn reopen(cx: &mut App) {
    let shell = cx.try_global::<Started>().and_then(|s| s.shell.clone());
    let frame = shell
        .as_ref()
        .map_or(WELCOME, |shell| shell.window_state.frame_name());
    let Some(window) = open_named(cx, frame) else {
        return;
    };
    if let Some(shell) = shell {
        shell.window_state.window_changed(window, frame);
        shell.host.attach(window, cx);
    }
}

/// The app menu and its key bindings, in the language chosen at startup.
pub fn set_menus(cx: &mut App, strings: Strings) {
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    cx.bind_keys([
        KeyBinding::new("cmd-h", Hide, None),
        KeyBinding::new("alt-cmd-h", HideOthers, None),
        KeyBinding::new("cmd-q", Quit, None),
        // The review screen's handler is the only one, so the key does
        // nothing elsewhere (docs/design/review_view.md「对其他 crate 的要求」G).
        KeyBinding::new("cmd-r", review::Refresh, None),
    ]);
    cx.set_menus([
        Menu::new("Lectern").items([
            MenuItem::action(strings.get(keys::app::MENU_HIDE), Hide),
            MenuItem::action(strings.get(keys::app::MENU_HIDE_OTHERS), HideOthers),
            MenuItem::action(strings.get(keys::app::MENU_SHOW_ALL), ShowAll),
            MenuItem::separator(),
            MenuItem::action(strings.get(keys::app::MENU_QUIT), Quit),
        ]),
        // The review screen's two commands that have no control on screen; macOS
        // greys an item out while no view on the focus path handles its action.
        Menu::new(strings.get(keys::app::MENU_VIEW)).items([
            MenuItem::action(
                strings.get(keys::app::MENU_TOGGLE_DIFF_MODE),
                review::ToggleDiffMode,
            ),
            MenuItem::action(strings.get(keys::app::MENU_REFRESH), review::Refresh),
        ]),
    ]);
}
