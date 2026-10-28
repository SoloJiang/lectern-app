//! Tests of the frame autosave name. The name check and the order of the
//! checks run on every platform; the paths that reach the thread check run
//! on macOS only. No test has a real window: Rust's test harness never runs
//! a test on the main thread, where AppKit windows live (the design's
//! 「测试」 says what is left to a real run).

use raw_window_handle::{HandleError, HasWindowHandle, WindowHandle};

use super::{
    MAX_NAME_LEN, WindowError, clamp_to_visible, set_frame_autosave_name,
    set_frame_autosave_name_or_center, valid_name,
};

/// A window whose handle must not be asked for.
struct Untouchable;

impl HasWindowHandle for Untouchable {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        panic!("the window handle was asked for");
    }
}

/// GPUI's test platform window: no handle.
struct NoHandle;

impl HasWindowHandle for NoHandle {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        Err(HandleError::NotSupported)
    }
}

#[test]
fn frame_autosave_name_accepts_plain_names() {
    for name in ["welcome", "project", "a", "Main-Window_2", "0"] {
        assert!(valid_name(name), "{name:?}");
    }
    let longest = "n".repeat(MAX_NAME_LEN);
    assert!(valid_name(&longest));
}

#[test]
fn frame_autosave_name_rejects_anything_else() {
    for name in [
        "", " ", "a b", "a\0", "a/b", "a.b", "a:b", "é", "名", "a\n", "welcome ",
    ] {
        assert!(!valid_name(name), "{name:?}");
    }
    let overlong = "n".repeat(MAX_NAME_LEN + 1);
    assert!(!valid_name(&overlong));
}

#[test]
fn set_frame_autosave_name_checks_the_name_before_the_window() {
    assert_eq!(
        set_frame_autosave_name(&Untouchable, ""),
        Err(WindowError::InvalidName)
    );
    assert_eq!(
        set_frame_autosave_name(&Untouchable, "a b"),
        Err(WindowError::InvalidName)
    );
}

#[test]
fn or_center_checks_the_name_before_the_window() {
    assert_eq!(
        set_frame_autosave_name_or_center(&Untouchable, "", (1180.0, 760.0)),
        Err(WindowError::InvalidName)
    );
    assert_eq!(
        set_frame_autosave_name_or_center(&Untouchable, "a b", (1180.0, 760.0)),
        Err(WindowError::InvalidName)
    );
}

#[test]
fn clamp_to_visible_keeps_a_size_that_fits() {
    assert_eq!(
        clamp_to_visible((1180.0, 760.0), Some((1512.0, 920.0))),
        (1180.0, 760.0)
    );
    assert_eq!(
        clamp_to_visible((1180.0, 760.0), Some((1180.0, 760.0))),
        (1180.0, 760.0)
    );
    // No screen: nothing to cut to.
    assert_eq!(clamp_to_visible((1180.0, 760.0), None), (1180.0, 760.0));
}

#[test]
fn clamp_to_visible_shrinks_each_side_that_does_not() {
    assert_eq!(
        clamp_to_visible((1180.0, 760.0), Some((1024.0, 900.0))),
        (1024.0, 760.0)
    );
    assert_eq!(
        clamp_to_visible((1180.0, 760.0), Some((1440.0, 700.0))),
        (1180.0, 700.0)
    );
    assert_eq!(
        clamp_to_visible((1180.0, 760.0), Some((800.0, 600.0))),
        (800.0, 600.0)
    );
}

#[test]
fn window_error_kinds_are_distinct_static_names() {
    let kinds = [
        WindowError::InvalidName,
        WindowError::NoWindow,
        WindowError::NotMainThread,
        WindowError::NameInUse,
    ];
    for (i, a) in kinds.iter().enumerate() {
        assert!(!a.as_str().is_empty());
        assert!(
            a.as_str()
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b == b'_')
        );
        for b in &kinds[i + 1..] {
            assert_ne!(a.as_str(), b.as_str());
        }
    }
}

#[cfg(not(target_os = "macos"))]
#[test]
fn set_frame_autosave_name_does_nothing_elsewhere() {
    assert_eq!(set_frame_autosave_name(&Untouchable, "welcome"), Ok(()));
    assert_eq!(set_frame_autosave_name(&NoHandle, "project"), Ok(()));
}

#[cfg(not(target_os = "macos"))]
#[test]
fn or_center_does_nothing_elsewhere() {
    use super::FrameSource;
    assert_eq!(
        set_frame_autosave_name_or_center(&Untouchable, "project", (1180.0, 760.0)),
        Ok(FrameSource::Restored)
    );
}

#[cfg(target_os = "macos")]
mod macos {
    use std::ptr::NonNull;

    use lectern_logging::testing;
    use raw_window_handle::{
        AppKitWindowHandle, HandleError, HasWindowHandle, RawWindowHandle, WindowHandle,
        XlibWindowHandle,
    };

    use super::{
        NoHandle, WindowError, set_frame_autosave_name, set_frame_autosave_name_or_center,
    };

    /// A handle of another windowing system.
    struct Foreign;

    impl HasWindowHandle for Foreign {
        fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
            let raw = RawWindowHandle::Xlib(XlibWindowHandle::new(1));
            // SAFETY: the function under test never uses an Xlib handle.
            Ok(unsafe { WindowHandle::borrow_raw(raw) })
        }
    }

    /// An AppKit handle whose view pointer must never be read: the thread
    /// check comes first, and tests do not run on the main thread.
    struct Dangling;

    impl HasWindowHandle for Dangling {
        fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
            let raw = RawWindowHandle::AppKit(AppKitWindowHandle::new(NonNull::dangling()));
            // SAFETY: off the main thread the function returns before it
            // reads the view, which this test asserts it is.
            Ok(unsafe { WindowHandle::borrow_raw(raw) })
        }
    }

    /// Rust's test harness runs every test on a spawned thread.
    fn off_the_main_thread() -> bool {
        // SAFETY: no arguments; reads a flag of the calling thread.
        unsafe { libc::pthread_main_np() == 0 }
    }

    #[test]
    fn set_frame_autosave_name_without_a_handle_is_no_window() {
        assert_eq!(
            set_frame_autosave_name(&NoHandle, "welcome"),
            Err(WindowError::NoWindow)
        );
    }

    #[test]
    fn set_frame_autosave_name_with_a_foreign_handle_is_no_window() {
        assert_eq!(
            set_frame_autosave_name(&Foreign, "welcome"),
            Err(WindowError::NoWindow)
        );
    }

    #[test]
    fn set_frame_autosave_name_off_the_main_thread_is_an_error_and_logs_the_kind_only() {
        const CANARY: &str = "canary5k2w8p1r6z";
        assert!(off_the_main_thread(), "this test needs a test thread");
        let log = testing::capture();
        log.clear();

        assert_eq!(
            set_frame_autosave_name(&Dangling, CANARY),
            Err(WindowError::NotMainThread)
        );

        let lines = log.lines();
        assert_eq!(lines.len(), 1, "{lines:#?}");
        let line = &lines[0];
        assert!(
            line.contains(
                "\"msg\":\"the window frame autosave name was not set; the frame is not restored or saved\""
            ),
            "{line}"
        );
        assert!(line.contains("\"lv\":\"warn\""), "{line}");
        assert!(line.contains("\"tg\":\"platform\""), "{line}");
        assert!(line.contains("\"kind\":\"not_main_thread\""), "{line}");
        log.assert_absent(CANARY);
    }

    #[test]
    fn or_center_without_a_handle_is_no_window() {
        assert_eq!(
            set_frame_autosave_name_or_center(&NoHandle, "project", (1180.0, 760.0)),
            Err(WindowError::NoWindow)
        );
        assert_eq!(
            set_frame_autosave_name_or_center(&Foreign, "project", (1180.0, 760.0)),
            Err(WindowError::NoWindow)
        );
    }

    #[test]
    fn or_center_off_the_main_thread_is_an_error_and_logs_the_kind_only() {
        const CANARY: &str = "canary9t3m7q2x4v";
        assert!(off_the_main_thread(), "this test needs a test thread");
        let log = testing::capture();
        log.clear();

        assert_eq!(
            set_frame_autosave_name_or_center(&Dangling, CANARY, (1180.0, 760.0)),
            Err(WindowError::NotMainThread)
        );

        let lines: Vec<String> = log
            .lines()
            .into_iter()
            .filter(|line| line.contains("\"msg\":\"the window frame autosave name was not set"))
            .collect();
        assert_eq!(lines.len(), 1, "{lines:#?}");
        let line = &lines[0];
        assert!(line.contains("\"lv\":\"warn\""), "{line}");
        assert!(line.contains("\"tg\":\"platform\""), "{line}");
        assert!(line.contains("\"kind\":\"not_main_thread\""), "{line}");
        log.assert_absent(CANARY);
    }

    #[test]
    fn every_failure_is_logged_with_its_kind() {
        let log = testing::capture();
        log.clear();
        assert_eq!(
            set_frame_autosave_name(&NoHandle, "no-handle"),
            Err(WindowError::NoWindow)
        );
        assert_eq!(
            set_frame_autosave_name(&NoHandle, "bad name"),
            Err(WindowError::InvalidName)
        );
        // Under `cargo test` other tests of this process may log meanwhile;
        // nextest gives this test its own process.
        let lines: Vec<String> = log
            .lines()
            .into_iter()
            .filter(|line| line.contains("\"msg\":\"the window frame autosave name was not set"))
            .collect();
        assert_eq!(lines.len(), 2, "{lines:#?}");
        assert!(lines[0].contains("\"kind\":\"no_window\""), "{}", lines[0]);
        assert!(
            lines[1].contains("\"kind\":\"invalid_name\""),
            "{}",
            lines[1]
        );
        log.assert_absent("no-handle");
        log.assert_absent("bad name");
    }
}
