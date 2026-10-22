//! 「测试」过滤.

use std::time::Duration;

use log::Level;

use super::{Fixture, config, records, third_party, verbose_records, with_msg};
use crate::filter::snapshot_of;
use crate::{Ctx, Filter, FilterSource, PluginTag, Verbose, scope};

#[test]
fn filter_parse() {
    let inputs = [
        "",
        "info",
        "DEBUG",
        "cache=trace",
        "debug,cache=trace,gpui=info",
        "@review=debug",
        "warn, @code-intel=trace ,gix-odb=debug",
        "cache=trace,cache=warn",
        "verbose",
        "cache=loud",
        "=debug",
        "@=debug",
        "Cache Store=debug",
        "debug,,info",
        "trace,bad!,@x.y=info,off",
    ];
    let text: Vec<String> = inputs.iter().map(|s| snapshot_of(s)).collect();
    insta::assert_snapshot!(text.join("\n"));
}

fn probe() {
    crate::debug!("cache probe");
}

fn verbose(filter: &str) -> Option<Verbose> {
    Some(Verbose {
        filter: Filter::parse(filter).0,
        lasts: Duration::from_secs(3600),
    })
}

#[test]
fn filter_change_invalidates_callsite_cache() {
    let fx = Fixture::new();
    let logging = fx.start(None);
    // The global level lets debug through (for `other`), the target does not.
    logging.set_verbose(verbose("info,other=debug"));
    probe();
    logging.set_verbose(verbose("debug"));
    probe();
    logging.set_verbose(verbose("info,other=debug"));
    probe();
    assert!(logging.flush(Duration::from_secs(5)));
    assert_eq!(
        with_msg(&verbose_records(fx.path()), "cache probe").len(),
        1
    );
}

fn scoped_probe() {
    crate::debug!("plugin scoped");
}

#[test]
fn plugin_directive_matches_the_scope() {
    let fx = Fixture::new();
    let logging = fx.start(Some("@review=debug"));
    scoped_probe();
    {
        let _s = scope(Ctx {
            plugin: Some(PluginTag::new("review")),
            act: Some(4),
            ..Ctx::default()
        });
        scoped_probe();
        third_party(
            Level::Debug,
            "gpui::window",
            "src/window.rs",
            3,
            format_args!("third party in scope"),
        );
        let _other = scope(Ctx {
            plugin: Some(PluginTag::new("other")),
            ..Ctx::default()
        });
        scoped_probe();
    }
    assert!(logging.flush(Duration::from_secs(5)));
    let records = verbose_records(fx.path());
    let written = with_msg(&records, "plugin scoped");
    assert_eq!(written.len(), 1, "{records:?}");
    assert_eq!(
        (written[0]["plugin"].as_str(), written[0]["act"].as_u64()),
        (Some("review"), Some(4))
    );
    assert_eq!(with_msg(&records, "third party in scope").len(), 1);
}

#[test]
fn cli_beats_env_beats_setting() {
    let fx = Fixture::new();
    let logging = fx
        .install(Some("cache=trace"), Some("debug"))
        .start(config());
    assert!(logging.flush(Duration::from_secs(5)));
    let status = logging.status();
    assert_eq!(
        (status.source, status.filter.to_string()),
        (FilterSource::Cli, "cache=trace".to_owned())
    );
    assert_eq!(records(fx.path())[0]["filter"], "cache=trace");
    fx.restart(&logging);

    let logging = fx.install(None, Some("debug")).start(config());
    logging.set_verbose(verbose("warn"));
    assert!(logging.flush(Duration::from_secs(5)));
    let status = logging.status();
    assert_eq!(
        (status.source, status.filter.to_string()),
        (FilterSource::Env, "debug".to_owned())
    );
    assert!(
        fx.path().join("verbose.json").exists(),
        "the setting is saved"
    );
    fx.restart(&logging);

    let logging = fx.start(None);
    let status = logging.status();
    assert_eq!(
        (status.source, status.filter.to_string()),
        (FilterSource::Setting, "warn".to_owned())
    );
}
