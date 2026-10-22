//! 「测试」导出.

use super::{Fixture, all_files};
use crate::{ExportError, ExportOptions, Logging};

fn run_id(logging: &Logging) -> String {
    format!("{:016x}", logging.state().run)
}

#[test]
fn export_copies_recent_runs_and_a_manifest() {
    let fx = Fixture::new();
    let mut ids = Vec::new();
    for run in 0..4 {
        let logging = fx.start(if run == 3 { Some("debug") } else { None });
        crate::info!("a run");
        crate::debug!("verbose detail");
        ids.push(run_id(&logging));
        if run < 3 {
            fx.restart(&logging);
        } else {
            let parent = tempfile::tempdir().expect("dir");
            let exported = logging
                .export(parent.path(), &ExportOptions::default())
                .expect("export");
            let files = all_files(&exported.dir);
            // Three runs' default files, the last run's verbose file, the manifest.
            assert_eq!(files.len(), 5, "{files:?}");
            assert_eq!(exported.files, 5);
            assert_eq!(
                exported.bytes,
                files.iter().map(|(_, t)| t.len() as u64).sum::<u64>()
            );
            assert!(exported.dir.join("verbose").is_dir());
            let manifest: serde_json::Value = serde_json::from_str(
                &std::fs::read_to_string(exported.dir.join("manifest.json")).expect("manifest"),
            )
            .expect("JSON");
            assert_eq!(manifest["format"], 1);
            assert_eq!(manifest["runs"], serde_json::json!(ids[1..]));
            assert_eq!(manifest["files"].as_array().map(Vec::len), Some(4));
            assert!(manifest["dropped"].is_u64() && manifest["suppressed"].is_u64());
            let copied: String = files.iter().map(|(_, t)| t.as_str()).collect();
            assert!(copied.contains("\"msg\":\"a run\""));
            assert!(copied.contains("\"msg\":\"verbose detail\""));

            let without = logging
                .export(
                    parent.path(),
                    &ExportOptions {
                        runs: 1,
                        include_verbose: false,
                        extra: Vec::new(),
                    },
                )
                .expect("export");
            assert_eq!(without.files, 2);
            assert_ne!(without.dir, exported.dir);
        }
    }
}

#[test]
fn export_without_a_directory() {
    let _fx = Fixture::new();
    let early = crate::sink::install_for_test(
        crate::EarlyConfig::default(),
        super::test_env(None),
        crate::sink::AllCaps::STANDARD,
    )
    .expect("install");
    let logging = early.start(super::config());
    let parent = tempfile::tempdir().expect("dir");
    assert_eq!(
        logging.export(parent.path(), &ExportOptions::default()),
        Err(ExportError::NoDirectory)
    );
}
