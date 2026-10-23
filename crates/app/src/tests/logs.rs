//! The canary test (docs/design/app.md「日志与诊断」): command-line values
//! never reach the log.

use std::ffi::OsString;
use std::sync::Arc;
use std::time::Duration;

use lectern_executor::Shutdown;
use lectern_logging::Flushed;

use crate::args;
use crate::quit::{QuitSteps, Timeouts, run};

struct Noop;

impl QuitSteps for Noop {
    fn kernel(&self, done: Box<dyn FnOnce() + Send>) {
        done();
    }

    fn watcher(&self) {}

    fn executor(&self, _: Duration) -> Shutdown {
        Shutdown::Clean
    }

    fn cache(&self) {}

    fn logging(&self, _: Duration) -> Flushed {
        Flushed::Clean
    }
}

#[test]
fn canary_stays_out_of_app_logs() {
    let canary = "canary-7f3e9a1c";
    let capture = lectern_logging::testing::capture();
    capture.clear();
    let parsed = args::parse(
        [
            OsString::from("lectern"),
            OsString::from(format!("/Users/dev/{canary}")),
            OsString::from("--log"),
            OsString::from(format!("{canary}=trace")),
        ]
        .into_iter(),
    );
    args::log_unknown(&parsed);
    run(Arc::new(Noop), &Timeouts::RELEASE);
    let lines = capture.lines();
    assert!(
        lines
            .iter()
            .any(|l| l.contains("\"msg\":\"unrecognized argument\"") && l.contains("\"index\":1")),
        "{lines:#?}"
    );
    capture.assert_absent(canary);
}
