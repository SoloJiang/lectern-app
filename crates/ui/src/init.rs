//! Initialisation (docs/design/ui.md「初始化」): `init` before the first
//! frame, `ready` after it, `attach` per window, and the handlers that swap
//! the palette when the appearance, the accent colour or the reduce-motion
//! setting changes.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use gpui::{App, Subscription, Window};
use gpui_component::{Theme, ThemeColor, ThemeMode};
use lectern_i18n::Strings;
use lectern_logging::{debug, info, warn};
use lectern_platform::appearance::{self as platform, Change};

use crate::palette::{Appearance, Palette, SystemColor, on_accent};
use crate::state::{MONO_CACHE_SLOTS, UiState};

/// Before the first frame, once, before `app` opens a window. Memory only:
/// reads the reduce-motion setting, builds both palettes from the built-in
/// table, sets `Strings` as a global and registers `platform`'s change
/// notifications. Does not read the system colours (AppKit's first read of
/// its dynamic colours is about 2 ms: docs/design/platform.md「系统外观」);
/// `ready` reads them. Starts no thread, reads no file, does not run
/// `gpui_component::init`.
pub fn init(cx: &mut App, strings: Strings) {
    let start = Instant::now();
    let reduced_motion = platform::reduce_motion().unwrap_or(false);
    let async_cx = cx.to_async();
    let watch = platform::watch(Box::new(move |change| {
        // The notification arrives on the main thread without a GPUI
        // context, possibly while `App` is borrowed: queue the update to the
        // main thread's next run, as GPUI does for its own appearance callback.
        async_cx
            .spawn(async move |cx| cx.update(|cx| platform_changed(change, cx)))
            .detach();
    }));
    cx.set_global(UiState {
        palettes: Appearance::ALL.map(Palette::builtin),
        strings,
        reduced_motion,
        accent_follows_system: false,
        fixed_appearance: None,
        ready: false,
        mono_cache: [None; MONO_CACHE_SLOTS],
        mono_resolutions: 0,
        attached: Vec::new(),
        watch: Some(watch),
    });
    let appearance = Appearance::of(cx.window_appearance());
    info!("ui initialised"; appearance = appearance.as_str(), accent = "builtin", reduce_motion = reduced_motion, elapsed_us = elapsed_us(start));
}

/// After the first frame, once, from `startup::release`: runs
/// `gpui_component::init` (it parses its embedded theme JSON, so not before
/// the first frame), reads the system colours of both appearances, swaps
/// the palettes, and projects the current appearance's `Palette` onto
/// gpui-component's `Theme`. A second call does nothing.
pub fn ready(cx: &mut App) {
    if is_ready(cx) {
        return;
    }
    let start = Instant::now();
    gpui_component::init(cx);
    read_system_colors(cx);
    finish_ready(start, cx);
}

/// Whether `ready` has run. Views do not mount while it is false.
pub fn is_ready(cx: &App) -> bool {
    cx.try_global::<UiState>().is_some_and(|state| state.ready)
}

/// For tests, headless and the probe profile: no system reads; the built-in
/// table, the given appearance (used by `colors` in place of the windows'
/// own) and the given reduce-motion flag; runs `ready` as well.
pub fn init_fixed(cx: &mut App, strings: Strings, appearance: Appearance, reduced_motion: bool) {
    let start = Instant::now();
    cx.set_global(UiState {
        palettes: Appearance::ALL.map(Palette::builtin),
        strings,
        reduced_motion,
        accent_follows_system: false,
        fixed_appearance: Some(appearance),
        ready: false,
        mono_cache: [None; MONO_CACHE_SLOTS],
        mono_resolutions: 0,
        attached: Vec::new(),
        watch: None,
    });
    info!("ui initialised"; appearance = appearance.as_str(), accent = "builtin", reduce_motion = reduced_motion, elapsed_us = elapsed_us(start));
    let start = Instant::now();
    gpui_component::init(cx);
    finish_ready(start, cx);
}

fn finish_ready(start: Instant, cx: &mut App) {
    cx.global_mut::<UiState>().ready = true;
    sync_theme(current_appearance(cx), cx);
    let accent = if cx.global::<UiState>().accent_follows_system {
        "system"
    } else {
        "builtin"
    };
    info!("ui ready"; accent = accent, elapsed_us = elapsed_us(start));
}

/// Once per window after it opens (`app` for the skeleton window,
/// `workspace` for later ones): registers the appearance observer so that
/// `colors(window, cx)` follows this window's appearance. The window's root
/// view holds the `Subscription`; a second call for the same window
/// registers nothing and returns an empty subscription.
pub fn attach(window: &mut Window, cx: &mut App) -> Subscription {
    let handle = window.window_handle();
    let open = cx.windows();
    let state = cx.global_mut::<UiState>();
    state.attached.retain(|attached| open.contains(attached));
    if state.attached.contains(&handle) {
        return Subscription::new(|| {});
    }
    state.attached.push(handle);
    window.observe_window_appearance(|window, cx| {
        appearance_changed(Appearance::of(window.appearance()), cx);
    })
}

/// The window appearance changed (the observer of `attach`, or
/// `testing::simulate_appearance_change`): the other palette is served,
/// gpui-component follows when `ready` has run, every window is refreshed.
pub(crate) fn appearance_changed(appearance: Appearance, cx: &mut App) {
    let state = cx.global_mut::<UiState>();
    if state.fixed_appearance.is_some() {
        state.fixed_appearance = Some(appearance);
    }
    debug!("appearance changed"; appearance = appearance.as_str());
    if is_ready(cx) {
        sync_theme(appearance, cx);
    }
    cx.refresh_windows();
}

/// A `platform` notification, queued to the main thread by `init`'s callback.
pub(crate) fn platform_changed(change: Change, cx: &mut App) {
    match change {
        Change::SystemColors => {
            read_system_colors(cx);
            debug!("accent changed"; accent = accent_name(cx));
            if is_ready(cx) {
                sync_theme(current_appearance(cx), cx);
            }
            cx.refresh_windows();
        }
        Change::ReduceMotion => {
            let reduced_motion = platform::reduce_motion().unwrap_or(false);
            cx.global_mut::<UiState>().reduced_motion = reduced_motion;
            debug!("reduce motion changed"; reduce_motion = reduced_motion);
        }
    }
}

static WARNED_COLORS: AtomicBool = AtomicBool::new(false);

/// Reads both appearances' system colours through `platform` and replaces
/// the palettes. When a read fails the built-in palette stays and a warn is
/// logged once per run.
fn read_system_colors(cx: &mut App) {
    let state = cx.global_mut::<UiState>();
    let mut accent_follows_system = true;
    for appearance in Appearance::ALL {
        match platform::system_colors(appearance.to_platform()) {
            Some(colors) => {
                state.palettes[appearance.index()] = Palette::from_system(appearance, &colors);
                accent_follows_system &= colors.accent_available;
            }
            None => {
                state.palettes[appearance.index()] = Palette::builtin(appearance);
                accent_follows_system = false;
                if !WARNED_COLORS.swap(true, Ordering::Relaxed) {
                    warn!("system colours are unavailable; the built-in table is used"; appearance = appearance.as_str());
                }
            }
        }
    }
    state.accent_follows_system = accent_follows_system;
}

fn accent_name(cx: &App) -> &'static str {
    if cx.global::<UiState>().accent_follows_system {
        "system"
    } else {
        "builtin"
    }
}

/// The appearance gpui-component's theme follows: the fixed one, else the
/// first window's, else the application's.
fn current_appearance(cx: &mut App) -> Appearance {
    if let Some(appearance) = cx.global::<UiState>().fixed_appearance {
        return appearance;
    }
    let window = cx
        .windows()
        .into_iter()
        .next()
        .and_then(|handle| handle.update(cx, |_, window, _| window.appearance()).ok());
    Appearance::of(window.unwrap_or_else(|| cx.window_appearance()))
}

/// Sets gpui-component's light / dark mode and projects the `Palette` onto
/// its `Theme`: `Theme::change` loads the mode's registered theme, so the
/// projection is a second edit, after it. Only after `ready`: `Theme`
/// exists only once `gpui_component::init` has run.
fn sync_theme(appearance: Appearance, cx: &mut App) {
    debug_assert!(is_ready(cx), "sync_theme before ready");
    let mode = match appearance {
        Appearance::Light => ThemeMode::Light,
        Appearance::Dark => ThemeMode::Dark,
    };
    Theme::change(mode, None, cx);
    let palette = *cx.global::<UiState>().palette(appearance);
    Theme::update(cx, |theme| project(&palette, &mut theme.colors));
}

/// The fields of gpui-component's `ThemeColor` with a direct source in the
/// `Palette`. The others keep the registered theme's values until a wrapped
/// control (step 3) needs them.
fn project(palette: &Palette, colors: &mut ThemeColor) {
    colors.background = palette.window_background;
    colors.foreground = palette.label;
    colors.border = palette.separator;
    colors.muted = palette.under_page_background;
    colors.muted_foreground = palette.secondary_label;
    colors.primary = palette.accent;
    colors.primary_foreground = on_accent();
    colors.secondary = palette.control_background;
    colors.secondary_foreground = palette.label;
    colors.accent = palette.unemphasized_selected_content_background;
    colors.accent_foreground = palette.label;
    colors.list = palette.control_background;
    colors.list_active = palette.selected_content_background;
    colors.list_active_border = palette.selected_content_background;
    colors.selection = palette.unemphasized_selected_content_background;
    colors.sidebar = palette.window_background;
    colors.sidebar_foreground = palette.label;
    colors.sidebar_border = palette.separator;
    colors.sidebar_accent = palette.unemphasized_selected_content_background;
    colors.sidebar_accent_foreground = palette.label;
    colors.sidebar_primary = palette.accent;
    colors.sidebar_primary_foreground = on_accent();
    colors.popover = palette.control_background;
    colors.popover_foreground = palette.label;
    colors.input = palette.separator;
    colors.ring = palette.accent;
    colors.caret = palette.label;
    colors.link = palette.system(SystemColor::Blue);
    colors.title_bar = palette.window_background;
    colors.title_bar_border = palette.separator;
    colors.skeleton = palette.tertiary_label;
    colors.scrollbar_thumb = palette.tertiary_label;
    colors.table = palette.control_background;
    colors.table_active = palette.selected_content_background;
    colors.danger = palette.system(SystemColor::Red);
    colors.success = palette.system(SystemColor::Green);
    colors.warning = palette.system(SystemColor::Orange);
    colors.info = palette.system(SystemColor::Blue);
}

fn elapsed_us(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX)
}
