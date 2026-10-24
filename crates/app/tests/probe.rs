//! The probe profile end to end (docs/design/ui_probe.md「对其他 crate 的要求」
//! 第 3 项): launch the real `lectern` with a temporary `HOME` and
//! `LECTERN_UI_PROBE_DIR`, talk to it through `client::Client`, and quit it
//! through the probe. `CLAUDE_CONFIG_DIR` and `CODEX_HOME` are removed, so the
//! session sources look only under the temporary `HOME`. Needs a macOS
//! graphical session, like `tests/startup.rs`; the timeouts only end a hang.

#![cfg(feature = "ui-probe")]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Child, Command, ExitStatus};
use std::time::{Duration, Instant};

use lectern_ui_probe::client::{Client, Reply};
use serde_json::{Value, json};

const HANG: Duration = Duration::from_secs(30);
/// How long the probe may take to listen after the process starts.
const LISTEN: Duration = Duration::from_secs(10);
/// A screenshot name that must not reach the log.
const CANARY: &str = "canary-9c4e1f7a";

/// The process; killed if the test fails before it has quit.
struct Lectern(Child);

impl Lectern {
    fn wait_for_exit(&mut self) -> ExitStatus {
        let start = Instant::now();
        loop {
            if let Some(status) = self.0.try_wait().unwrap() {
                return status;
            }
            assert!(
                start.elapsed() < HANG,
                "lectern did not exit within {HANG:?}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Lectern {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn ok(reply: Reply) -> Value {
    assert!(reply.ok, "{:?}", reply.error);
    reply.result.expect("a result")
}

/// A client once the probe listens: the barrier is released after the first
/// frame, and the probe starts at its end.
fn connect(dir: &Path) -> Client {
    let start = Instant::now();
    loop {
        match Client::connect(dir, HANG) {
            Ok(client) => return client,
            Err(e) => {
                assert!(
                    start.elapsed() < LISTEN,
                    "the probe did not listen within {LISTEN:?}: {e}"
                );
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

/// Whether the PNG at `path` has more than one colour: a read-back that
/// really happened is not a single colour.
fn has_content(path: &Path) -> (bool, (u32, u32)) {
    let image = image::open(path).unwrap().to_rgba8();
    let first = image.pixels().next().copied();
    let varied = image.pixels().any(|pixel| Some(*pixel) != first);
    (varied, image.dimensions())
}

#[test]
fn a_session_answers_hello_ui_tree_screenshot_and_quit() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let probe = root.path().join("probe");
    let shots = root.path().join("shots");
    for dir in [&home, &probe, &shots] {
        fs::create_dir(dir).unwrap();
    }
    fs::set_permissions(&probe, fs::Permissions::from_mode(0o700)).unwrap();
    let mut lectern = Lectern(
        Command::new(env!("CARGO_BIN_EXE_lectern"))
            .env("HOME", &home)
            .env("LECTERN_UI_PROBE_DIR", &probe)
            .env_remove("LECTERN_LOG")
            .env_remove("CLAUDE_CONFIG_DIR")
            .env_remove("CODEX_HOME")
            .spawn()
            .unwrap(),
    );
    let mut client = connect(&probe);

    let hello = ok(client.call("hello", json!({})).unwrap());
    assert_eq!(hello["windows"].as_array().unwrap().len(), 1, "{hello}");

    // The skeleton, and whatever the plugins have put in the window by now:
    // the root is the window itself.
    let tree = ok(client.call("ui_tree", json!({})).unwrap());
    assert_eq!(tree["schema"], "ui-tree/1", "{tree}");
    assert_eq!(tree["nodes"][0]["role"], "window", "{tree}");

    // A read-back is not a single colour; the window may need a frame or two
    // to draw its content, so ask again until it does.
    let start = Instant::now();
    let shot = loop {
        let shot = ok(client
            .call(
                "screenshot",
                json!({ "dir": shots, "name": CANARY, "annotate": false }),
            )
            .unwrap());
        let path = shots.join(format!("{CANARY}.png"));
        assert_eq!(shot["path"], path.to_string_lossy().as_ref(), "{shot}");
        let (varied, _) = has_content(&path);
        if varied {
            break shot;
        }
        assert!(
            start.elapsed() < LISTEN,
            "the screenshot is a single colour: {shot}"
        );
        std::thread::sleep(Duration::from_millis(100));
    };
    let (_, (width, height)) = has_content(&shots.join(format!("{CANARY}.png")));
    let scale = shot["scale"].as_f64().unwrap();
    let size = &shot["tree"]["window"]["size"];
    assert_eq!(
        f64::from(width),
        (size["w"].as_f64().unwrap() * scale).round()
    );
    assert_eq!(
        f64::from(height),
        (size["h"].as_f64().unwrap() * scale).round()
    );

    ok(client.call("quit", json!({})).unwrap());
    assert!(lectern.wait_for_exit().success());

    // The probe's own records are there, and the name did not reach the log.
    let logs = home.join("Library/Logs/Lectern");
    let log: String = fs::read_dir(logs)
        .unwrap()
        .map(|entry| fs::read_to_string(entry.unwrap().path()).unwrap_or_default())
        .collect();
    assert!(log.contains("\"msg\":\"probe listening\""), "{log}");
    assert!(log.contains("\"msg\":\"probe stopped\""), "{log}");
    assert!(!log.contains(CANARY), "{log}");
}
