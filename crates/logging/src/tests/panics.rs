//! 「测试」panic.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use super::{Fixture, records, with_msg};
use crate::hooks::imp as hooks;
use crate::{Ctx, PluginTag, scope};

#[test]
fn panic_hook_records_scope_and_location() {
    let fx = Fixture::new();
    let logging = fx.start(None);
    let line = {
        let _s = scope(Ctx {
            plugin: Some(PluginTag::new("review")),
            act: Some(3),
            task: Some(9),
            ..Ctx::default()
        });
        let line = line!() + 1;
        let result = catch_unwind(|| panic!("a static message"));
        assert!(result.is_err());
        line
    };
    assert!(logging.flush(Duration::from_secs(5)));
    let records = records(fx.path());
    let panics = with_msg(&records, "panic");
    assert_eq!(panics.len(), 1, "{records:?}");
    let p = panics[0];
    assert_eq!(p["lv"], "error");
    assert_eq!(
        (p["plugin"].as_str(), p["act"].as_u64(), p["task"].as_u64()),
        (Some("review"), Some(3), Some(9))
    );
    assert_eq!(p["at"], format!("{}:{line}", file!()));
    assert_eq!(p["message"], "a static message");
    assert!(p["th"].as_str().is_some_and(|t| !t.is_empty()));
}

#[test]
fn panic_hook_waits_at_most_100_ms() {
    let fx = Fixture::new();
    let logging = fx.start(None);
    hooks::stall_writer(true);
    let started = Instant::now();
    let result = catch_unwind(|| panic!("while the writer is stuck"));
    let waited = started.elapsed();
    assert!(result.is_err());
    assert!(waited >= Duration::from_millis(90), "{waited:?}");
    assert!(waited < Duration::from_secs(2), "{waited:?}");
    hooks::stall_writer(false);
    assert!(logging.flush(Duration::from_secs(5)));
    assert_eq!(with_msg(&records(fx.path()), "panic").len(), 1);
}

static PREVIOUS_CALLS: AtomicU32 = AtomicU32::new(0);

#[test]
fn panic_hook_chains_the_previous_hook() {
    let fx = Fixture::new();
    std::panic::set_hook(Box::new(|_| {
        PREVIOUS_CALLS.fetch_add(1, Ordering::SeqCst);
    }));
    let logging = fx.start(None);
    let before = PREVIOUS_CALLS.load(Ordering::SeqCst);
    assert!(catch_unwind(AssertUnwindSafe(|| panic!("chained"))).is_err());
    assert_eq!(PREVIOUS_CALLS.load(Ordering::SeqCst), before + 1);
    assert!(logging.flush(Duration::from_secs(5)));
    assert_eq!(with_msg(&records(fx.path()), "panic").len(), 1);
}
