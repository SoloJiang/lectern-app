use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;

use crate::args::{Args, parse};

fn run(args: &[&str]) -> Args {
    parse(
        std::iter::once("lectern")
            .chain(args.iter().copied())
            .map(OsString::from),
    )
}

#[test]
fn log_value_forms() {
    assert_eq!(run(&["--log", "debug"]).log.as_deref(), Some("debug"));
    assert_eq!(run(&["--log=cache=trace"]).log.as_deref(), Some("cache=trace"));
    // The last one wins.
    assert_eq!(
        run(&["--log", "debug", "--log=warn"]).log.as_deref(),
        Some("warn")
    );
    assert_eq!(run(&[]).log, None);
}

#[test]
fn log_without_a_value_is_unknown() {
    let args = run(&["--log"]);
    assert_eq!(args.log, None);
    assert_eq!(args.unknown, [1]);
}

#[test]
fn non_utf8_log_is_ignored() {
    let args = parse(
        [
            OsString::from("lectern"),
            OsString::from("--log"),
            OsString::from_vec(b"de\xffbug".to_vec()),
        ]
        .into_iter(),
    );
    assert_eq!(args.log, None);
    assert_eq!(args.unknown, [1]);
}

#[test]
fn unknown_arguments_keep_only_indexes() {
    let args = parse(
        [
            OsString::from("lectern"),
            OsString::from("/Users/dev/project"),
            OsString::from("--log=info"),
            OsString::from_vec(b"\xff".to_vec()),
            OsString::from("-x"),
        ]
        .into_iter(),
    );
    assert_eq!(args.unknown, [1, 3, 4]);
    assert_eq!(args.log.as_deref(), Some("info"));
}

#[cfg(debug_assertions)]
#[test]
fn quit_after_start_only_in_debug_builds() {
    use crate::args::QuitPath;
    assert_eq!(
        run(&["--quit-after-start"]).quit_after_start,
        Some(QuitPath::Action)
    );
    assert_eq!(
        run(&["--quit-after-start=system"]).quit_after_start,
        Some(QuitPath::System)
    );
    assert_eq!(run(&["--quit-after-start=other"]).unknown, [1]);
}

#[cfg(not(debug_assertions))]
#[test]
fn quit_after_start_only_in_debug_builds() {
    assert_eq!(run(&["--quit-after-start"]).unknown, [1]);
}
