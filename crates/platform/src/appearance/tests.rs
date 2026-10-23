//! Tests of the system appearance. The pure parts run on every platform;
//! the reads and the watch run on macOS only, on a test thread, without a
//! window or an `NSApplication`: the colours resolve under the appearance
//! the block makes current for this thread, and a notification posted on
//! the test thread is delivered on it.

use super::{Appearance, Change, Srgba, lightness, reduce_motion, system_colors, watch};

#[test]
fn appearance_lightness_is_the_hsl_lightness() {
    let black = Srgba {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 0.85,
    };
    let white = Srgba {
        r: 1.0,
        g: 1.0,
        b: 1.0,
        a: 0.85,
    };
    let red = Srgba {
        r: 1.0,
        g: 0.0,
        b: 0.0,
        a: 1.0,
    };
    assert_eq!(lightness(black), 0.0);
    assert_eq!(lightness(white), 1.0);
    assert_eq!(lightness(red), 0.5);
}

#[test]
fn appearance_names_are_distinct_static_names() {
    assert_eq!(Appearance::Light.as_str(), "light");
    assert_eq!(Appearance::Dark.as_str(), "dark");
    assert_eq!(Change::SystemColors.as_str(), "system_colors");
    assert_eq!(Change::ReduceMotion.as_str(), "reduce_motion");
}

#[cfg(not(target_os = "macos"))]
#[test]
fn appearance_is_none_elsewhere() {
    assert_eq!(system_colors(Appearance::Light), None);
    assert_eq!(system_colors(Appearance::Dark), None);
    assert_eq!(reduce_motion(), None);
}

#[cfg(not(target_os = "macos"))]
#[test]
fn appearance_watch_is_inert_elsewhere() {
    use std::cell::Cell;
    use std::rc::Rc;

    let calls = Rc::new(Cell::new(0));
    let counted = Rc::clone(&calls);
    let watch = watch(Box::new(move |_| counted.set(counted.get() + 1)));
    assert!(!watch.is_live());
    assert_eq!(format!("{watch:?}"), "Watch { live: false }");
    drop(watch);
    assert_eq!(calls.get(), 0);
    // The callback was dropped with the watch.
    assert_eq!(Rc::strong_count(&calls), 1);
}

#[cfg(target_os = "macos")]
mod macos {
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;
    use std::sync::{Mutex, MutexGuard};
    use std::time::Instant;

    use lectern_logging::testing;

    use super::super::imp::post;
    use super::{Appearance, Change, Srgba, lightness, reduce_motion, system_colors, watch};

    fn in_unit(color: Srgba) -> bool {
        [color.r, color.g, color.b, color.a]
            .iter()
            .all(|c| (0.0..=1.0).contains(c))
    }

    /// Under `cargo test` the tests of this module share a process: the
    /// observers of one test would also see the posts of another, so the
    /// tests that post run one at a time. nextest gives each its own
    /// process.
    fn posting() -> MutexGuard<'static, ()> {
        static LOCK: Mutex<()> = Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A watch whose callback counts the deliveries of each change.
    fn counting_watch() -> (super::super::Watch, Rc<RefCell<Vec<Change>>>) {
        let seen = Rc::new(RefCell::new(Vec::new()));
        let record = Rc::clone(&seen);
        let watch = watch(Box::new(move |change| record.borrow_mut().push(change)));
        (watch, seen)
    }

    #[test]
    fn appearance_system_colors_resolve_for_both_appearances() {
        let started = Instant::now();
        let light = system_colors(Appearance::Light).expect("light");
        let dark = system_colors(Appearance::Dark).expect("dark");
        let elapsed = started.elapsed();
        eprintln!("two system_colors reads: {elapsed:?}");

        // The ui design's acceptance: the label is dark on light and light
        // on dark.
        assert!(lightness(light.label) < 0.5, "{:?}", light.label);
        assert!(lightness(dark.label) > 0.5, "{:?}", dark.label);
        assert!(
            lightness(light.window_background) > lightness(dark.window_background),
            "{:?} {:?}",
            light.window_background,
            dark.window_background
        );
        for colors in [&light, &dark] {
            for color in [
                colors.label,
                colors.secondary_label,
                colors.tertiary_label,
                colors.separator,
                colors.window_background,
                colors.control_background,
                colors.under_page_background,
                colors.accent,
                colors.selected_content_background,
                colors.unemphasized_selected_content_background,
            ]
            .into_iter()
            .chain(colors.system)
            {
                assert!(in_unit(color), "{color:?}");
            }
            // `controlAccentColor` exists since macOS 10.14.
            assert!(colors.accent_available);
            // The opaque surfaces and the system colours have full alpha;
            // the labels are translucent.
            assert_eq!(colors.window_background.a, 1.0);
            assert!(
                colors.system.iter().all(|c| c.a == 1.0),
                "{:?}",
                colors.system
            );
            assert!(colors.label.a > 0.0);
        }
        // The two appearances resolve to different tables, and a second
        // read of the same appearance to the same one.
        assert_ne!(light, dark);
        assert_eq!(system_colors(Appearance::Light), Some(light));
        assert_eq!(system_colors(Appearance::Dark), Some(dark));
        // Red is red in both.
        for colors in [&light, &dark] {
            let red = colors.system[0];
            assert!(red.r > red.g && red.r > red.b, "{red:?}");
        }
    }

    #[test]
    fn appearance_reduce_motion_is_some() {
        let value = reduce_motion().expect("accessibilityDisplayShouldReduceMotion");
        eprintln!("reduce motion: {value}");
    }

    #[test]
    fn appearance_watch_delivers_a_change() {
        let _serial = posting();
        let (watch, seen) = counting_watch();
        assert!(watch.is_live());
        assert_eq!(format!("{watch:?}"), "Watch { live: true }");
        assert!(seen.borrow().is_empty());

        post(Change::SystemColors);
        assert_eq!(*seen.borrow(), [Change::SystemColors]);
        post(Change::ReduceMotion);
        assert_eq!(*seen.borrow(), [Change::SystemColors, Change::ReduceMotion]);
        post(Change::SystemColors);
        assert_eq!(seen.borrow().len(), 3);
        drop(watch);
    }

    #[test]
    fn appearance_watch_stops_after_drop() {
        let _serial = posting();
        let (watch, seen) = counting_watch();
        post(Change::SystemColors);
        assert_eq!(seen.borrow().len(), 1);
        drop(watch);
        post(Change::SystemColors);
        post(Change::ReduceMotion);
        assert_eq!(seen.borrow().len(), 1);
        // The callback (and its clone of `seen`) was dropped with the watch.
        assert_eq!(Rc::strong_count(&seen), 1);
    }

    #[test]
    fn appearance_watch_is_not_reentered() {
        let _serial = posting();
        let calls = Rc::new(Cell::new(0));
        let counted = Rc::clone(&calls);
        let watch = watch(Box::new(move |_| {
            counted.set(counted.get() + 1);
            // A post from inside the callback: skipped, not delivered
            // again to this callback.
            post(Change::SystemColors);
        }));
        post(Change::SystemColors);
        assert_eq!(calls.get(), 1);
        post(Change::SystemColors);
        assert_eq!(calls.get(), 2);
        drop(watch);
    }

    #[test]
    fn appearance_watch_dropped_inside_its_callback_is_fine() {
        let _serial = posting();
        let slot: Rc<RefCell<Option<super::super::Watch>>> = Rc::new(RefCell::new(None));
        let calls = Rc::new(Cell::new(0));
        let counted = Rc::clone(&calls);
        let dropper = Rc::clone(&slot);
        let watch = watch(Box::new(move |_| {
            counted.set(counted.get() + 1);
            // Drops the watch while its callback runs.
            dropper.borrow_mut().take();
        }));
        *slot.borrow_mut() = Some(watch);
        post(Change::SystemColors);
        assert_eq!(calls.get(), 1);
        assert!(slot.borrow().is_none());
        post(Change::SystemColors);
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn appearance_watch_contains_a_panicking_callback_and_logs_no_payload() {
        const CANARY: &str = "canary7m3q9x2v5k";
        let _serial = posting();
        let log = testing::capture();
        log.clear();
        let calls = Rc::new(Cell::new(0));
        let counted = Rc::clone(&calls);
        let watch = watch(Box::new(move |_| {
            counted.set(counted.get() + 1);
            panic!("{CANARY}");
        }));

        post(Change::SystemColors);
        assert_eq!(calls.get(), 1);
        // The callback is gone: no further delivery, no second record.
        post(Change::SystemColors);
        post(Change::ReduceMotion);
        assert_eq!(calls.get(), 1);

        let lines: Vec<String> = log
            .lines()
            .into_iter()
            .filter(|line| line.contains("\"msg\":\"the appearance change callback panicked"))
            .collect();
        assert_eq!(lines.len(), 1, "{lines:#?}");
        let line = &lines[0];
        assert!(line.contains("\"lv\":\"error\""), "{line}");
        assert!(line.contains("\"tg\":\"platform\""), "{line}");
        assert!(line.contains("\"change\":\"system_colors\""), "{line}");
        log.assert_absent(CANARY);
        drop(watch);
    }
}
