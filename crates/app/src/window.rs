//! The skeleton window and the app menu (docs/design/app.md「界面」).

use gpui::{
    App, Bounds, Context, IntoElement, KeyBinding, Menu, MenuItem, Render, Subscription,
    TitlebarOptions, Window, WindowBackgroundAppearance, WindowBounds, WindowOptions, actions, div,
    prelude::*, px, size,
};
use lectern_i18n::{Strings, keys};

use crate::profile::Profile;
use crate::quit::Quit;
use crate::startup::{self, Startup};

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
            if cx.try_global::<Startup>().map(Startup::profile) == Some(Profile::Desktop) {
                startup::arm(window);
            }
        }
        div().size_full()
    }
}

/// Opens the skeleton window. Main thread.
pub fn open(cx: &mut App) {
    let bounds = Bounds::centered(None, size(px(WIDTH), px(HEIGHT)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions {
            title: None,
            appears_transparent: true,
            traffic_light_position: None,
        }),
        window_background: WindowBackgroundAppearance::Blurred,
        ..Default::default()
    };
    // Opening fails only without a display; there is nothing to fall back to.
    let _ = cx.open_window(options, |window, cx| {
        // One observer; `lectern_ui::init` ran before this window
        // (docs/design/ui.md「对其他 crate 的要求」).
        let appearance = lectern_ui::attach(window, cx);
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
    ]);
    cx.set_menus([Menu::new("Lectern").items([
        MenuItem::action(strings.get(keys::app::MENU_HIDE), Hide),
        MenuItem::action(strings.get(keys::app::MENU_HIDE_OTHERS), HideOthers),
        MenuItem::action(strings.get(keys::app::MENU_SHOW_ALL), ShowAll),
        MenuItem::separator(),
        MenuItem::action(strings.get(keys::app::MENU_QUIT), Quit),
    ])]);
}
