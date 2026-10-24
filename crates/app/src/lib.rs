//! Lectern app entry: startup, plugin assembly per profile, window snapshot
//! restore. See docs/design/app.md.
//!
//! A library behind the `lectern` binary so that the log target is `app`
//! (the crate name minus `lectern_`), not the binary's name.

mod args;
#[cfg(debug_assertions)]
mod debug_quit;
mod profile;
mod quit;
mod startup;
mod window;
mod window_state;

#[cfg(test)]
mod tests;

use lectern_i18n::{Locale, Strings};
use lectern_logging::EarlyConfig;

use crate::profile::Profile;
use crate::startup::Startup;

/// The whole program; `main` only calls it.
pub fn run() {
    // Before the first frame: memory only, no thread, no file of ours
    // (docs/design/app.md「启动顺序」, ADR 0017).
    let args = args::parse(std::env::args_os());
    let Ok(early) = lectern_logging::install_early(EarlyConfig {
        dir: lectern_platform::dirs::logs(),
        cli_filter: args.log.clone(),
    }) else {
        // Only one install per process, and this is the first.
        return;
    };
    args::log_unknown(&args);
    let languages = lectern_platform::system::preferred_languages();
    let strings = Strings::of(Locale::negotiate(languages.iter().map(String::as_str)));

    // The only place GPUI takes an asset source; without it every `Icon`
    // draws blank (docs/design/ui.md「对其他 crate 的要求」).
    let app = gpui_platform::application().with_assets(lectern_ui::IconAssets);
    // Closing the last window does not quit; the Dock icon reopens it
    // (docs/design/app.md「界面」; decided by the product owner).
    app.on_reopen(|cx| {
        if cx.windows().is_empty() {
            window::reopen(cx);
        }
    });
    app.run(move |cx| {
        cx.set_global(Startup::new(early, Profile::Desktop));
        quit::init(cx);
        #[cfg(debug_assertions)]
        debug_quit::init(cx, args.quit_after_start);
        // Memory only: the built-in palettes, the reduce-motion flag and
        // `platform`'s change notifications. The system colours are read
        // behind the barrier (docs/design/ui.md「初始化」).
        lectern_ui::init(cx, strings);
        window::set_menus(cx, strings);
        window::open(cx);
        cx.activate(true);
    });
}
