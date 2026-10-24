//! Tests of the language preference. The write queue runs on every platform
//! with a fake writer; the preference reads and writes run on macOS only,
//! against a plist in a temporary directory: no preference of the user's is
//! written.

use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::{Done, PrefError, Queue, language_override, set_language_override, system_languages};

const WAIT: Duration = Duration::from_secs(10);

/// A `done` that reports its result on a channel, tagged with `id`.
fn report(id: &'static str, tx: &Sender<(&'static str, Result<(), PrefError>)>) -> Done {
    let tx = tx.clone();
    Box::new(move |result| {
        let _ = tx.send((id, result));
    })
}

type Written = Arc<Mutex<Vec<Option<String>>>>;

/// A queue whose writer records the tags it was given and, when `gate` is
/// set, waits for a message there before it returns.
fn queue(gate: Option<Receiver<()>>, fail_on: Option<&'static str>) -> (Arc<Queue>, Written) {
    let written = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&written);
    let gate = Mutex::new(gate);
    let queue = Queue::new(Box::new(move |tag| {
        log.lock().unwrap().push(tag.map(str::to_owned));
        if let Some(gate) = gate.lock().unwrap().as_ref() {
            gate.recv_timeout(WAIT).expect("the test releases the gate");
        }
        if tag == fail_on && tag.is_some() {
            return Err(PrefError::WriteFailed);
        }
        Ok(())
    }));
    (queue, written)
}

#[test]
fn queue_writes_in_the_order_of_the_calls() {
    let (queue, written) = queue(None, None);
    let (tx, rx) = mpsc::channel();
    queue.submit(Some("zh-Hans"), report("a", &tx));
    assert_eq!(rx.recv_timeout(WAIT).unwrap(), ("a", Ok(())));
    queue.submit(None, report("b", &tx));
    assert_eq!(rx.recv_timeout(WAIT).unwrap(), ("b", Ok(())));
    queue.submit(Some("en"), report("c", &tx));
    assert_eq!(rx.recv_timeout(WAIT).unwrap(), ("c", Ok(())));
    assert_eq!(
        *written.lock().unwrap(),
        [Some("zh-Hans".to_owned()), None, Some("en".to_owned())]
    );
}

#[test]
fn queue_drops_replaced_requests_and_writes_the_last_one_last() {
    let (gate_tx, gate_rx) = mpsc::channel();
    let (queue, written) = queue(Some(gate_rx), None);
    let (tx, rx) = mpsc::channel();
    // `a` is being written (the writer waits at the gate); `b` and `c` come
    // meanwhile: `b` is replaced by `c` before it starts.
    queue.submit(Some("en"), report("a", &tx));
    while written.lock().unwrap().is_empty() {
        std::thread::yield_now();
    }
    queue.submit(Some("zh-Hans"), report("b", &tx));
    queue.submit(None, report("c", &tx));
    gate_tx.send(()).unwrap();
    gate_tx.send(()).unwrap();

    let mut results = Vec::new();
    for _ in 0..3 {
        results.push(rx.recv_timeout(WAIT).unwrap());
    }
    // `a` first (it was running); then what was replaced; then the last.
    assert_eq!(
        results,
        [
            ("a", Ok(())),
            ("b", Err(PrefError::Superseded)),
            ("c", Ok(()))
        ]
    );
    assert_eq!(*written.lock().unwrap(), [Some("en".to_owned()), None]);
}

#[test]
fn queue_reports_a_failed_write_and_goes_on() {
    let (queue, written) = queue(None, Some("bad"));
    let (tx, rx) = mpsc::channel();
    queue.submit(Some("bad"), report("a", &tx));
    assert_eq!(
        rx.recv_timeout(WAIT).unwrap(),
        ("a", Err(PrefError::WriteFailed))
    );
    queue.submit(Some("en"), report("b", &tx));
    assert_eq!(rx.recv_timeout(WAIT).unwrap(), ("b", Ok(())));
    assert_eq!(written.lock().unwrap().len(), 2);
}

#[test]
fn queue_contains_a_panicking_done_and_a_panicking_writer() {
    let (tx, rx) = mpsc::channel();
    let queue = Queue::new(Box::new(|tag| {
        assert_ne!(tag, Some("boom"), "the writer panics");
        Ok(())
    }));
    queue.submit(Some("en"), Box::new(|_| panic!("done panics")));
    queue.submit(Some("boom"), report("a", &tx));
    // The writer's panic is a failed write.
    assert_eq!(
        rx.recv_timeout(WAIT).unwrap(),
        ("a", Err(PrefError::WriteFailed))
    );
    // The thread survived both: the next request is written.
    queue.submit(Some("zh-Hans"), report("b", &tx));
    assert_eq!(rx.recv_timeout(WAIT).unwrap(), ("b", Ok(())));
}

#[test]
fn queue_thread_ends_when_empty_and_starts_again() {
    let (queue, _) = queue(None, None);
    let (tx, rx) = mpsc::channel();
    for round in 0..3 {
        queue.submit(Some("en"), report("x", &tx));
        assert_eq!(rx.recv_timeout(WAIT).unwrap(), ("x", Ok(())), "{round}");
        // `running` is cleared under the lock, just after `done` ran.
        let deadline = std::time::Instant::now() + WAIT;
        while queue.is_running() {
            assert!(std::time::Instant::now() < deadline, "the thread stays");
            std::thread::yield_now();
        }
    }
}

#[test]
fn pref_error_kinds_are_distinct_static_names() {
    assert_eq!(PrefError::Superseded.as_str(), "superseded");
    assert_eq!(PrefError::WriteFailed.as_str(), "write_failed");
    assert_ne!(
        PrefError::Superseded.to_string(),
        PrefError::WriteFailed.to_string()
    );
}

#[cfg(not(target_os = "macos"))]
#[test]
fn language_preference_does_nothing_elsewhere() {
    assert!(system_languages().is_empty());
    assert_eq!(language_override(), None);
    let (tx, rx) = mpsc::channel();
    // Runs on the calling thread, before the call returns.
    set_language_override(Some("zh-Hans"), report("a", &tx));
    assert_eq!(rx.try_recv().unwrap(), ("a", Ok(())));
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use crate::system::language::imp::{Domain, read_override, write_override};
    use crate::system::preferred_languages;

    fn plist_domain(dir: &tempfile::TempDir) -> String {
        // An absolute path is an application ID whose preferences are the
        // plist `<path>.plist`.
        dir.path().join("prefs").to_str().unwrap().to_owned()
    }

    #[test]
    fn system_languages_reads_the_global_domain() {
        let languages = system_languages();
        assert!(!languages.is_empty());
        assert!(languages.len() <= 16, "{languages:?}");
        for tag in &languages {
            assert!(
                crate::system::language_tag(tag.as_bytes()).is_some(),
                "{tag}"
            );
        }
        eprintln!("{languages:?}");
    }

    #[test]
    fn language_override_does_not_fall_back_to_the_global_domain() {
        // This process was started without an override, and the global
        // domain holds the system's languages: a lookup that fell back to it
        // would answer with the first of them.
        assert!(!system_languages().is_empty());
        assert_eq!(language_override(), None);
        // The effective list is still readable next to it.
        assert!(!preferred_languages().is_empty());
    }

    #[test]
    fn an_override_is_written_read_replaced_and_removed() {
        let dir = tempfile::tempdir().unwrap();
        let path = plist_domain(&dir);
        let domain = Domain::Path(&path);
        assert_eq!(read_override(domain), None);

        write_override(domain, Some("zh-Hans")).unwrap();
        assert_eq!(read_override(domain).as_deref(), Some("zh-Hans"));
        assert!(
            dir.path().join("prefs.plist").exists(),
            "the plist in the temporary directory holds it"
        );

        // A second write replaces the one-element list.
        write_override(domain, Some("en")).unwrap();
        assert_eq!(read_override(domain).as_deref(), Some("en"));

        write_override(domain, None).unwrap();
        assert_eq!(read_override(domain), None);
        // Removing what is not there is fine.
        write_override(domain, None).unwrap();
    }

    #[test]
    fn an_override_is_a_one_element_list_of_the_tag() {
        let dir = tempfile::tempdir().unwrap();
        let path = plist_domain(&dir);
        write_override(Domain::Path(&path), Some("zh-Hans-CN")).unwrap();
        let output = std::process::Command::new("/usr/bin/plutil")
            .args(["-convert", "json", "-o", "-"])
            .arg(dir.path().join("prefs.plist"))
            .output()
            .expect("plutil");
        assert!(output.status.success(), "{output:?}");
        assert_eq!(
            String::from_utf8(output.stdout).unwrap().trim(),
            r#"{"AppleLanguages":["zh-Hans-CN"]}"#
        );
    }

    #[test]
    fn a_tag_that_is_not_a_tag_is_not_written() {
        let dir = tempfile::tempdir().unwrap();
        let path = plist_domain(&dir);
        let domain = Domain::Path(&path);
        for bad in ["", "-en", "en_US", "zh Hans", "e\u{301}n", "en\n"] {
            assert_eq!(
                write_override(domain, Some(bad)),
                Err(PrefError::WriteFailed),
                "{bad:?}"
            );
        }
        assert_eq!(read_override(domain), None);
    }

    #[test]
    fn a_value_that_is_not_a_list_of_tags_is_no_override() {
        let dir = tempfile::tempdir().unwrap();
        let path = plist_domain(&dir);
        let plist = dir.path().join("prefs.plist");
        for body in [
            "<string>zh-Hans</string>",
            "<array/>",
            "<array><integer>1</integer><string>has space</string></array>",
        ] {
            std::fs::write(
                &plist,
                format!(
                    r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict><key>AppleLanguages</key>{body}</dict></plist>"#
                ),
            )
            .unwrap();
            assert_eq!(read_override(Domain::Path(&path)), None, "{body}");
        }
    }

    #[test]
    fn set_language_override_reports_a_bad_tag_and_logs_the_kind_only() {
        const CANARY: &str = "canary7q3v9t2m";
        let log = lectern_logging::testing::capture();
        log.clear();
        let (tx, rx) = mpsc::channel();
        // Rejected before any preference is touched.
        set_language_override(Some(&format!("{CANARY} x")), report("a", &tx));
        assert_eq!(
            rx.recv_timeout(WAIT).unwrap(),
            ("a", Err(PrefError::WriteFailed))
        );
        let lines: Vec<String> = log
            .lines()
            .into_iter()
            .filter(|l| l.contains("the language preference was not saved"))
            .collect();
        assert_eq!(lines.len(), 1, "{lines:#?}");
        assert!(lines[0].contains("\"lv\":\"warn\""), "{}", lines[0]);
        assert!(lines[0].contains("\"tg\":\"platform\""), "{}", lines[0]);
        assert!(
            lines[0].contains("\"kind\":\"write_failed\""),
            "{}",
            lines[0]
        );
        log.assert_absent(CANARY);
    }
}
