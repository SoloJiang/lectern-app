//! `logs_hold_no_terminal_content` (docs/design/terminal.md「日志与诊断」):
//! no byte of what a terminal shows, takes or is started with reaches the
//! log, at trace level. The records that should be there are.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{FAKE, Kit, UUID_A, claude_command, located, resume, shell, size};
use lectern_logging::testing::capture;
use lectern_services::terminal::{Input, RunState};
use lectern_terminal::testing::Harness;

const CANARY: &str = "TERMINAL-CANARY-7f3a";

#[test]
fn logs_hold_no_terminal_content() {
    let log = capture();
    let kit = Kit::new();
    // The program's output has the canary as text, as the title and as the
    // directory of OSC 7; it echoes what it reads.
    let output = kit.root.join("output.vt");
    std::fs::write(
        &output,
        format!(
            "{CANARY} shown\r\n\x1b]0;{CANARY}-title\x07\x1b]7;file:///{CANARY}-dir\x07\x1b[21t"
        )
        .as_bytes(),
    )
    .unwrap();
    kit.plan(&format!("raw,out:{},say:READY,nl,echo", output.display()));
    // The environment has the canary in a value, a path in `PATH` and a
    // directory in the shell's path has it too.
    let mut env = kit.env();
    env.push((format!("SECRET_{CANARY}").into(), CANARY.into()));
    let odd_bin = kit.root.join(format!("{CANARY}-bin"));
    std::fs::create_dir(&odd_bin).unwrap();
    std::os::unix::fs::symlink(FAKE, odd_bin.join("claude")).unwrap();
    let mut path = odd_bin.clone().into_os_string();
    path.push(":");
    path.push(kit.bin.as_os_str());
    for (name, value) in &mut env {
        if name == "PATH" {
            *value = path.clone();
        }
    }
    let shell_path = kit.root.join(format!("{CANARY}-shell"));
    std::os::unix::fs::symlink(FAKE, &shell_path).unwrap();
    let harness = Harness::start(
        Harness::plugin()
            .with_login_env(env)
            .with_shell(&shell_path),
    );
    harness.projects.add(&kit.project);
    let project = kit.project_key(&harness);

    // A shell, a resumed agent, input, paste, a copy, a resize.
    let id = harness.open(shell(&kit.project), size()).unwrap();
    let _watch = harness.terminals.watch(id).unwrap();
    harness.wait_frame(id, |text| text.contains("READY"));
    harness
        .terminals
        .input(id, Input::Text(Arc::from(format!("typed-{CANARY}"))));
    harness
        .terminals
        .input(id, Input::Paste(Arc::from(format!("pasted-{CANARY}"))));
    harness.wait_frame(id, |text| text.contains(&format!("pasted-{CANARY}")));
    harness.terminals.input(id, Input::SelectAll);
    let (tx, rx) = std::sync::mpsc::channel();
    harness.terminals.copy_selection(
        id,
        harness.token.clone(),
        Box::new(move |text| {
            let _ = tx.send(text);
        }),
    );
    let copied = rx.recv_timeout(Duration::from_secs(5)).unwrap().unwrap();
    assert!(copied.contains(CANARY), "the canary is in the selection");
    harness.terminals.input(
        id,
        Input::Resize(lectern_services::terminal::GridSize {
            cols: 100,
            rows: 30,
            cell_width: 8,
            cell_height: 16,
        }),
    );
    let key = harness.claude.add(
        UUID_A,
        &kit.project,
        located(&project),
        Some(claude_command(UUID_A)),
    );
    let resumed = harness.open(resume(&key), size()).unwrap();
    harness.wait_state(resumed, |s| matches!(s, RunState::Running { .. }));
    // A launch that is refused, and one that fails.
    let _ = harness.open(resume(&harness.claude.key("not-a-uuid")), size());
    let failed = harness
        .open(shell(&kit.project.join("gone")), size())
        .unwrap();
    harness.wait_state(failed, |s| matches!(s, RunState::Failed(_)));
    harness.terminals.close(id);
    harness.terminals.close(resumed);
    harness.terminals.close(failed);
    harness.kernel.deactivate();

    // Each terminal thread writes its close line after `close` returns; the
    // three closed terminals (a shell, a resumed agent, a failed start) write
    // three, and the scan for the canary waits for all of them.
    let closed = || {
        log.lines()
            .iter()
            .filter(|l| l.contains("terminal closed"))
            .count()
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while closed() < 3 && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(closed() >= 3, "a close line for each closed terminal");
    let lines = log.lines();
    log.assert_absent(CANARY);
    // What is logged: the start, with the kind and the program name.
    let started: Vec<&String> = lines
        .iter()
        .filter(|line| line.contains("terminal started"))
        .collect();
    assert_eq!(started.len(), 2, "{started:?}");
    assert!(
        started.iter().any(|l| l.contains("\"program\":\"other\"")),
        "{started:?}"
    );
    assert!(
        started.iter().any(|l| l.contains("\"program\":\"claude\"")),
        "{started:?}"
    );
    assert!(lines.iter().any(|l| l.contains("terminal closed")));
    assert!(lines.iter().any(|l| l.contains("terminal start failed")));
    assert!(lines.iter().any(|l| l.contains("terminal launch refused")));
}
