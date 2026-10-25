//! `wire_*`: every message survives encode and decode; bad lines are errors
//! (docs/design/bench.md「测试」).

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::*;

fn at(ns: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_nanos(ns)
}

fn reports() -> Vec<Report> {
    vec![
        Report::FirstFrame {
            pid: 812,
            presented: at(1_729_000_000_123_456_789),
        },
        Report::Busy { epoch: 3 },
        Report::Quiescent { epoch: 4 },
        Report::Ready,
        Report::Epoch(Some(7)),
        Report::Epoch(None),
        Report::Plugin(PluginReport {
            id: "project-home".into(),
            state: "Active".into(),
            activation: vec![ActivationKind::Workspace, ActivationKind::AfterFirstFrame],
            manifest: Some(Duration::from_nanos(1_200)),
            activate: Some(Duration::from_micros(40)),
            commit: Some(Duration::from_micros(3)),
            started: Some(Duration::from_micros(250)),
        }),
        Report::Plugin(PluginReport {
            id: "terminal".into(),
            state: "Inactive".into(),
            activation: vec![ActivationKind::Command, ActivationKind::Manual],
            manifest: Some(Duration::from_nanos(900)),
            activate: None,
            commit: None,
            started: None,
        }),
        Report::Plugin(PluginReport {
            id: "empty".into(),
            state: "Failed".into(),
            activation: Vec::new(),
            manifest: None,
            activate: None,
            commit: None,
            started: None,
        }),
        Report::PluginsEnd,
        Report::PluginFailed {
            id: "review".into(),
            message: "panicked: 100% broken\nsecond line".into(),
        },
        Report::Milestone {
            milestone: Milestone::ProjectHome,
            presented: at(5),
        },
        Report::Milestone {
            milestone: Milestone::FileHighlighted,
            presented: at(6),
        },
        Report::Milestone {
            milestone: Milestone::DiffShown,
            presented: at(7),
        },
        Report::Milestone {
            milestone: Milestone::WorktreeSwitched,
            presented: at(8),
        },
        Report::Frames(FrameStats {
            scenario: "scroll".into(),
            refresh: Duration::from_nanos(16_666_667),
            frames: vec![
                FrameTimes {
                    requested: at(100),
                    started: at(200),
                    completed: at(300),
                    presented: Some(at(400)),
                },
                FrameTimes {
                    requested: at(500),
                    started: at(600),
                    completed: at(700),
                    presented: None,
                },
            ],
            without_tick: 1,
        }),
        Report::Frames(FrameStats {
            scenario: "input".into(),
            refresh: Duration::from_nanos(8_333_333),
            frames: Vec::new(),
            without_tick: 0,
        }),
        Report::Child {
            pid: 900,
            kind: ChildKind::ProcessPlugin,
            id: "jdtls host".into(),
        },
        Report::Child {
            pid: 901,
            kind: ChildKind::LanguageServer,
            id: "rust-analyzer".into(),
        },
        Report::Child {
            pid: 902,
            kind: ChildKind::Other,
            id: String::new(),
        },
        Report::Error {
            message: "hook mismatch".into(),
        },
    ]
}

fn requests() -> Vec<Request> {
    // A path that is not UTF-8, with a space, a `%` and a newline.
    let odd = PathBuf::from(OsString::from_vec(b"/tmp/a b/%/\n/\xff\xfe.rs".to_vec()));
    vec![
        Request::Epoch,
        Request::Plugins,
        Request::Quit,
        Request::ActivateAll,
        Request::Open(OpenTarget::Project(PathBuf::from("/tmp/cloud print"))),
        Request::Open(OpenTarget::File(odd.clone())),
        Request::Open(OpenTarget::Diff {
            session: "abc 100%".into(),
        }),
        Request::Open(OpenTarget::Worktree(odd)),
        Request::Open(OpenTarget::Scroll),
        Request::Open(OpenTarget::Animate),
        Request::Open(OpenTarget::Input {
            text: "fn main() {\n\tprintln!(\"✓\");\n}".into(),
            interval: Duration::from_millis(8),
        }),
    ]
}

#[test]
fn wire_reports_round_trip() {
    for report in reports() {
        let line = report.encode();
        assert!(!line.contains('\n'), "{line}");
        assert_eq!(Report::decode(&line), Ok(report.clone()), "{line}");
    }
}

#[test]
fn wire_requests_round_trip() {
    for request in requests() {
        let line = request.encode();
        assert!(!line.contains('\n'), "{line}");
        assert!(line.is_ascii(), "{line}");
        assert_eq!(Request::decode(&line), Ok(request.clone()), "{line}");
    }
}

#[test]
fn wire_lines_are_a_name_and_escaped_pairs() {
    let line = Report::FirstFrame {
        pid: 812,
        presented: at(1_729_000_000_000_000_000),
    }
    .encode();
    assert_eq!(line, "first-frame pid=812 presented_unix_ns=1729000000000000000");
    let line = Report::Error {
        message: "a b%c\nd".into(),
    }
    .encode();
    assert_eq!(line, "error message=a%20b%25c%0Ad");
    assert_eq!(Request::Quit.encode(), "quit");
    assert_eq!(Report::Epoch(None).encode(), "epoch");
}

#[test]
fn wire_missing_fields_are_errors() {
    for line in [
        "first-frame pid=1",
        "first-frame presented_unix_ns=5",
        "busy",
        "plugin id=a state=Active",
        "plugin-failed id=a",
        "milestone kind=project_home",
        "frames scenario=s refresh_ns=1 without_tick=0",
        "child pid=1 kind=other",
        "error",
    ] {
        assert!(Report::decode(line).is_err(), "{line}");
    }
    for line in ["open", "open target=file", "open target=input text=a"] {
        assert!(Request::decode(line).is_err(), "{line}");
    }
}

#[test]
fn wire_unknown_names_keys_and_values_are_errors() {
    for line in [
        "",
        "hello",
        "busy epoch=1 extra=2",
        "busy epoch=1 epoch=2",
        "busy epoch",
        "busy epoch=x",
        "milestone kind=elsewhere presented_unix_ns=1",
        "child pid=1 kind=daemon id=a",
        "plugin id=a state=Active activation=sometimes",
        "frames scenario=s refresh_ns=1 without_tick=0 times=1,2,3",
        "frames scenario=s refresh_ns=1 without_tick=0 times=1,2,3,x",
    ] {
        assert!(Report::decode(line).is_err(), "{line}");
    }
    for line in ["open target=elsewhere", "quit now=1", "plugins-end"] {
        assert!(Request::decode(line).is_err(), "{line}");
    }
}

#[test]
fn wire_bad_percent_encoding_is_an_error() {
    for line in [
        "error message=%",
        "error message=%4",
        "error message=%zz",
        "error message=%+1",
        "error message=%FF",
    ] {
        assert!(Report::decode(line).is_err(), "{line}");
    }
    // A path may hold any byte.
    assert_eq!(
        Request::decode("open target=file path=%FF"),
        Ok(Request::Open(OpenTarget::File(PathBuf::from(
            OsString::from_vec(vec![0xFF])
        ))))
    );
}
