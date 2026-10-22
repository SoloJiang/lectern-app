//! 「测试」详细日志.

use std::time::Duration;

use super::{Fixture, base_time, records, verbose_records, with_msg};
use crate::{Filter, FilterSource, Verbose};

const HOUR: Duration = Duration::from_secs(3600);

fn debug_for(lasts: Duration) -> Option<Verbose> {
    Some(Verbose {
        filter: Filter::parse("debug").0,
        lasts,
    })
}

fn verbose_probe() {
    crate::debug!("verbose probe");
}

fn plugin_probe() {
    crate::debug!("plugin probe");
}

#[test]
fn registered_plugin_directive_survives_verbose_json() {
    // What `register` accepts, a directive can name and verbose.json keeps.
    for bad in ["Foo", "a.b", "", "with space"] {
        assert_eq!(crate::PluginTag::register(bad), None, "{bad:?}");
    }
    let tag = crate::PluginTag::register("proc-plugin_2").expect("a valid ID");
    let fx = Fixture::new();
    let logging = fx.start(None);
    logging.set_verbose(Some(Verbose {
        filter: Filter::parse("@proc-plugin_2=debug").0,
        lasts: HOUR,
    }));
    assert!(logging.flush(Duration::from_secs(5)));
    fx.restart(&logging);

    let logging = fx.start(None);
    let status = logging.status();
    assert_eq!(status.source, FilterSource::Setting);
    assert_eq!(status.filter.to_string(), "@proc-plugin_2=debug");
    {
        let _s = crate::scope(crate::Ctx {
            plugin: Some(tag),
            ..crate::Ctx::default()
        });
        plugin_probe();
    }
    plugin_probe();
    assert!(logging.flush(Duration::from_secs(5)));
    let written = with_msg(&verbose_records(fx.path()), "plugin probe")
        .into_iter()
        .map(|r| r["plugin"].clone())
        .collect::<Vec<_>>();
    assert_eq!(written, [serde_json::json!("proc-plugin_2")]);
}

#[test]
fn setting_survives_restart_until_it_expires() {
    let fx = Fixture::new();
    let logging = fx.start(None);
    logging.set_verbose(debug_for(24 * HOUR));
    assert_eq!(logging.status().source, FilterSource::Setting);
    verbose_probe();
    assert!(logging.flush(Duration::from_secs(5)));
    let saved = std::fs::read_to_string(fx.path().join("verbose.json")).expect("verbose.json");
    assert_eq!(
        saved,
        "{\"filter\":\"debug\",\"until\":\"2026-10-23T03:27:01Z\"}\n"
    );
    assert_eq!(
        with_msg(&verbose_records(fx.path()), "verbose probe").len(),
        1
    );
    fx.restart(&logging);

    let logging = fx.start(None);
    let status = logging.status();
    assert_eq!(status.source, FilterSource::Setting);
    assert_eq!(status.filter.to_string(), "debug");
    assert_eq!(status.verbose_until, Some(base_time() + 24 * HOUR));
    fx.restart(&logging);

    fx.clock.advance(25 * HOUR);
    let logging = fx.start(None);
    let status = logging.status();
    assert_eq!(
        (status.source, status.verbose_until),
        (FilterSource::Default, None)
    );
    assert!(
        !fx.path().join("verbose.json").exists(),
        "an expired setting is removed"
    );
}

#[test]
fn expiry_restores_the_previous_filter() {
    let fx = Fixture::new();
    let logging = fx.start(None);
    logging.set_verbose(debug_for(HOUR));
    assert!(logging.flush(Duration::from_secs(5)));
    assert!(fx.path().join("verbose.json").exists());
    fx.clock.advance(2 * HOUR);
    assert!(logging.flush(Duration::from_secs(5)));
    let status = logging.status();
    assert_eq!(
        (status.source, status.filter, status.verbose_until),
        (FilterSource::Default, Filter::default(), None)
    );
    assert!(!fx.path().join("verbose.json").exists());
    let records = records(fx.path());
    assert_eq!(
        with_msg(&records, "verbose logging on").len(),
        1,
        "{records:#?}"
    );
    assert_eq!(with_msg(&records, "verbose logging expired").len(), 1);
    verbose_probe();
    assert!(logging.flush(Duration::from_secs(5)));
    assert!(with_msg(&verbose_records(fx.path()), "verbose probe").is_empty());
}

#[test]
fn turning_verbose_off_restores_the_default() {
    let fx = Fixture::new();
    let logging = fx.start(None);
    logging.set_verbose(debug_for(HOUR));
    logging.set_verbose(None);
    assert!(logging.flush(Duration::from_secs(5)));
    assert_eq!(logging.status().source, FilterSource::Default);
    assert!(!fx.path().join("verbose.json").exists());
    assert_eq!(
        with_msg(&records(fx.path()), "verbose logging off").len(),
        1
    );
}
