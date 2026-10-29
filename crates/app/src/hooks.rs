//! The automation build's extension point: a callback for each window once
//! `workspace` has taken it over (docs/design/app.md「自动化构建的装配」).
//! `app` does not know what the callback does.

use std::rc::Rc;

use gpui::{AnyWindowHandle, App, Global};

/// Main thread. Called once for each window `workspace` takes over, only after
/// the first frame: at the end of the first successful barrier release with a
/// window, and for a window a Dock click reopens while the workspace exists.
pub type OnWindow = Rc<dyn Fn(AnyWindowHandle, &mut App)>;

/// What [`crate::run_with`] is given. A GPUI global from `app.run` on: memory
/// only, nothing is called before the first frame.
#[derive(Default)]
pub struct Hooks {
    pub on_window: Option<OnWindow>,
}

impl Global for Hooks {}

/// Gives `window` to the callback, if there is one.
pub(crate) fn window_ready(window: AnyWindowHandle, cx: &mut App) {
    let Some(on_window) = cx.try_global::<Hooks>().and_then(|h| h.on_window.clone()) else {
        return;
    };
    on_window(window, cx);
}
