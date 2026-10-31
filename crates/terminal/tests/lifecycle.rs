//! The life of a terminal and its processes (docs/design/terminal.md「测试」
//! 生命周期): threads, replacement, close, exit, overflow, panic, quiescence
//! and concurrent starts. The programs are the fake program.

mod common;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{RecvTimeoutError, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::{Kit, UUID_A, claude_command, located, resume, running_pid, shell, size, wait_gone};
use lectern_kernel::PluginState;
use lectern_services::project::Projects;
use lectern_services::terminal::{
    AgentCommand, FailReason, Input, LaunchError, RunState, TerminalEvent, TerminalId,
};
use lectern_terminal::testing::Harness;

const WAIT: Duration = Duration::from_secs(20);

fn threads(harness: &Harness) -> usize {
    harness.threads.load(Ordering::SeqCst)
}

fn wait_threads(harness: &Harness, count: usize) {
    let reached = harness
        .wait_for(Duration::from_secs(10), || {
            (threads(harness) == count).then_some(())
        })
        .is_some();
    assert!(
        reached,
        "{} terminal threads, wanted {count}",
        threads(harness)
    );
}

/// Opens a shell terminal and waits until it runs; returns its ID and pid.
fn running_shell(kit: &Kit, harness: &Harness) -> (TerminalId, u32) {
    let id = harness.open(shell(&kit.project), size()).expect("opens");
    let info = harness.wait_state(id, |s| matches!(s, RunState::Running { .. }));
    (id, running_pid(&info))
}

fn resumable(harness: &Harness, kit: &Kit) -> lectern_services::session::SessionKey {
    let project = kit.project_key(harness);
    harness.claude.add(
        UUID_A,
        &kit.project,
        located(&project),
        Some(claude_command(UUID_A)),
    )
}

#[test]
fn no_threads_until_open() {
    let kit = Kit::new();
    let harness = kit.harness();
    assert_eq!(threads(&harness), 0);
    assert_eq!(kit.records(), 0);
    let (id, _) = running_shell(&kit, &harness);
    // The terminal thread and the wait thread.
    let two = harness
        .wait_for(WAIT, || (threads(&harness) == 2).then_some(()))
        .is_some();
    assert!(two, "{} threads", threads(&harness));
    harness.terminals.close(id);
    wait_threads(&harness, 0);
}

#[test]
fn an_exited_terminal_keeps_one_thread_and_a_failed_one_too() {
    let kit = Kit::new();
    kit.plan("exit:0");
    let harness = kit.harness();
    // The program may exit before the state `Running` is seen.
    let id = harness.open(shell(&kit.project), size()).unwrap();
    harness.wait_state(id, |s| matches!(s, RunState::Exited { .. }));
    wait_threads(&harness, 1);
    harness.terminals.close(id);
    wait_threads(&harness, 0);

    let failed = harness
        .open(shell(&kit.project.join("gone")), size())
        .unwrap();
    harness.wait_state(failed, |s| matches!(s, RunState::Failed(_)));
    wait_threads(&harness, 1);
}

#[test]
fn resume_refuses_while_running() {
    let kit = Kit::new();
    let harness = kit.harness();
    let key = resumable(&harness, &kit);
    let id = harness.open(resume(&key), size()).unwrap();
    // Still starting or already running: the same refusal.
    assert_eq!(
        harness.open(resume(&key), size()),
        Err(LaunchError::AlreadyRunning(id))
    );
    harness.wait_state(id, |s| matches!(s, RunState::Running { .. }));
    assert_eq!(
        harness.open(resume(&key), size()),
        Err(LaunchError::AlreadyRunning(id))
    );
    assert_eq!(harness.terminals.list().terminals.len(), 1);
    harness.wait_for(WAIT, || (kit.records() == 1).then_some(()));
    assert_eq!(kit.records(), 1, "one process");
}

#[test]
fn exited_terminal_is_replaced() {
    let kit = Kit::new();
    kit.plan("exit:0");
    let harness = kit.harness();
    let key = resumable(&harness, &kit);
    let old = harness.open(resume(&key), size()).unwrap();
    harness.wait_state(old, |s| matches!(s, RunState::Exited { .. }));
    assert_eq!(harness.terminals.for_session(&key), Some(old));

    kit.plan("say:READY,nl,hold");
    let new = harness.open(resume(&key), size()).expect("a new terminal");
    assert_ne!(new, old);
    assert!(harness.terminals.info(old).is_none());
    assert_eq!(harness.terminals.for_session(&key), Some(new));
    assert_eq!(harness.terminals.list().terminals.len(), 1);
    harness.wait_state(new, |s| matches!(s, RunState::Running { .. }));
}

#[test]
fn failed_terminal_is_replaced() {
    let kit = Kit::new();
    std::fs::remove_file(kit.bin.join("claude")).unwrap();
    let harness = kit.harness();
    let key = resumable(&harness, &kit);
    let old = harness.open(resume(&key), size()).unwrap();
    let info = harness.wait_state(old, |s| matches!(s, RunState::Failed(_)));
    assert_eq!(
        info.state,
        RunState::Failed(FailReason::NotInstalled(AgentCommand::ClaudeCode))
    );

    kit.install("claude");
    let new = harness.open(resume(&key), size()).expect("a new terminal");
    assert!(harness.terminals.info(old).is_none());
    harness.wait_state(new, |s| matches!(s, RunState::Running { .. }));
}

#[test]
fn close_hangs_up_the_process_and_ends_both_threads() {
    let kit = Kit::new();
    let harness = kit.harness();
    let (id, pid) = running_shell(&kit, &harness);
    kit.record(&harness, pid);
    harness.terminals.close(id);
    // Gone from the table before anything else happens.
    assert!(harness.terminals.info(id).is_none());
    assert!(harness.terminals.list().terminals.is_empty());
    assert!(harness.terminals.frame(id).is_none());
    assert!(wait_gone(&harness, pid, Duration::from_secs(5)));
    wait_threads(&harness, 0);
    // A closed ID is ignored.
    harness.terminals.close(id);
    harness.terminals.input(id, Input::SelectAll);
}

#[test]
fn close_kills_a_process_that_ignores_the_hang_up() {
    let kit = Kit::new();
    kit.plan("ignore-hup,say:READY,nl,hold");
    let harness = kit.harness();
    let (id, pid) = running_shell(&kit, &harness);
    let _watch = harness.terminals.watch(id).unwrap();
    harness.wait_frame(id, |text| text.contains("READY"));
    let closed = Instant::now();
    harness.terminals.close(id);
    assert!(
        wait_gone(&harness, pid, Duration::from_secs(8)),
        "the process survived the kill"
    );
    // SIGHUP was ignored: the kill came after the two seconds of grace.
    assert!(closed.elapsed() >= Duration::from_millis(1500));
    wait_threads(&harness, 0);
}

#[test]
fn project_closed_closes_its_terminals() {
    let kit = Kit::new();
    let other = kit.root.join("other");
    std::fs::create_dir(&other).unwrap();
    let harness = kit.harness();
    let second = harness.projects.add(&other);
    let first = kit.project_key(&harness);
    assert_ne!(first, second);
    let (a, pid_a) = running_shell(&kit, &harness);
    let b = harness.open(shell(&other), size()).unwrap();
    let pid_b = running_pid(&harness.wait_state(b, |s| matches!(s, RunState::Running { .. })));

    harness.close_project(&first);
    assert!(
        harness
            .wait_for(WAIT, || harness.terminals.info(a).is_none().then_some(()))
            .is_some()
    );
    assert!(wait_gone(&harness, pid_a, Duration::from_secs(5)));
    // The other project's terminal is untouched.
    assert!(harness.terminals.info(b).is_some());
    assert!(common::alive(pid_b));
    assert_eq!(
        Projects::open_projects(&*harness.projects),
        vec![second.clone()]
    );
}

#[test]
fn deactivate_closes_all() {
    let kit = Kit::new();
    let harness = kit.harness();
    let (_, a) = running_shell(&kit, &harness);
    let (_, b) = running_shell(&kit, &harness);
    harness.kernel.deactivate();
    assert!(wait_gone(&harness, a, Duration::from_secs(8)));
    assert!(wait_gone(&harness, b, Duration::from_secs(8)));
    assert!(harness.terminals.list().terminals.is_empty());
    wait_threads(&harness, 0);
}

#[test]
fn exit_codes_are_reported() {
    let kit = Kit::new();
    let harness = kit.harness();
    for (plan, expected) in [
        ("exit:0", (0, false)),
        ("exit:3", (3, false)),
        // 9 is SIGKILL.
        ("signal:9", (0, true)),
    ] {
        kit.plan(plan);
        let id = harness.open(shell(&kit.project), size()).unwrap();
        let info = harness.wait_state(id, |s| matches!(s, RunState::Exited { .. }));
        assert_eq!(
            info.state,
            RunState::Exited {
                code: expected.0,
                signaled: expected.1
            },
            "{plan}"
        );
        harness.terminals.close(id);
    }
}

#[test]
fn input_overflow_drops_whole_inputs_and_counts_them() {
    // The program never reads: the PTY's input queue fills, then the
    // 4 MiB write buffer, then inputs are dropped.
    let kit = Kit::new();
    kit.plan("say:READY,nl,hold");
    let harness = kit.harness();
    let (id, _) = running_shell(&kit, &harness);
    let chunk: Arc<str> = Arc::from("x".repeat(1 << 20));
    let started = Instant::now();
    for _ in 0..8 {
        harness.terminals.input(id, Input::Paste(chunk.clone()));
    }
    // `input` never waits for the program.
    assert!(started.elapsed() < Duration::from_secs(2));
    let info = harness
        .wait_for(WAIT, || {
            harness
                .terminals
                .info(id)
                .filter(|info| info.dropped_inputs > 0)
        })
        .expect("inputs were dropped");
    assert!(info.dropped_inputs >= 1, "{}", info.dropped_inputs);
    assert!(matches!(info.state, RunState::Running { .. }));
}

#[test]
fn panic_fails_only_that_terminal() {
    let kit = Kit::new();
    kit.plan("raw,say:READY,nl,echo");
    let target = Arc::new(AtomicU64::new(0));
    let hook = target.clone();
    let harness = Harness::start(kit.plugin().with_thread_hook(move |id| {
        if id.0 == hook.load(Ordering::SeqCst) {
            panic!("test hook");
        }
    }));
    harness.projects.add(&kit.project);
    let (bad, bad_pid) = running_shell(&kit, &harness);
    let (good, _) = running_shell(&kit, &harness);

    target.store(bad.0, Ordering::SeqCst);
    // Any command makes the thread run a round.
    harness.terminals.input(bad, Input::SelectAll);
    let info = harness.wait_state(bad, |s| matches!(s, RunState::Failed(_)));
    assert_eq!(info.state, RunState::Failed(FailReason::Internal));
    assert!(wait_gone(&harness, bad_pid, Duration::from_secs(8)));

    // The other terminal and the plugin go on.
    target.store(0, Ordering::SeqCst);
    assert!(matches!(
        harness.terminals.info(good).unwrap().state,
        RunState::Running { .. }
    ));
    let _watch = harness.terminals.watch(good).unwrap();
    harness.wait_frame(good, |text| text.contains("READY"));
    harness
        .terminals
        .input(good, Input::Text(Arc::from("still-here")));
    harness.wait_frame(good, |text| text.contains("still-here"));
    let status = harness
        .kernel
        .inner()
        .unwrap()
        .status()
        .into_iter()
        .find(|s| s.id == "plugin-under-test")
        .unwrap();
    assert_eq!(status.state, PluginState::Active);

    // The failed terminal still shows its last frame until it is closed.
    assert!(harness.terminals.frame(bad).is_some());
    harness.terminals.close(bad);
    harness.terminals.close(good);
    wait_threads(&harness, 0);
}

#[test]
fn quiescent_after_output_stops() {
    let kit = Kit::new();
    let output = kit.root.join("output.vt");
    std::fs::write(&output, "line\r\n".repeat(2000)).unwrap();
    kit.plan(&format!("out:{},say:DONE,nl,hold", output.display()));
    let harness = kit.harness();
    let (id, _) = running_shell(&kit, &harness);
    let _watch = harness.terminals.watch(id).unwrap();
    harness.wait_frame(id, |text| text.contains("DONE"));
    let kernel = harness.kernel.inner().unwrap();
    // The program is running but silent: the terminal holds no guard.
    let quiet = harness
        .wait_for(Duration::from_secs(10), || {
            kernel.is_quiescent().then_some(())
        })
        .is_some();
    assert!(quiet, "the kernel never became quiescent");
}

#[test]
fn concurrent_spawn() {
    // Eight terminals start at once while other threads allocate heavily
    // (fork then exec allocates between them: design 「依赖」 portable-pty).
    let kit = Kit::new();
    let harness = kit.harness();
    let stop = std::sync::atomic::AtomicBool::new(false);
    let ids = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| {
                let mut blocks: Vec<Vec<u8>> = Vec::new();
                while !stop.load(Ordering::Relaxed) {
                    blocks.push(vec![1u8; 48 * 1024]);
                    if blocks.len() > 64 {
                        blocks.clear();
                    }
                }
            });
        }
        let openers: Vec<_> = (0..8)
            .map(|_| {
                scope.spawn(|| {
                    let id = harness
                        .terminals
                        .open(lectern_services::terminal::OpenSpec {
                            launch: shell(&kit.project),
                            size: Some(size()),
                            theme: lectern_terminal::testing::theme(),
                        })
                        .expect("opens");
                    ids.lock().unwrap().push(id);
                })
            })
            .collect();
        for opener in openers {
            opener.join().unwrap();
        }
        // The allocating threads stop even when a wait below panics.
        struct Stop<'a>(&'a std::sync::atomic::AtomicBool);
        impl Drop for Stop<'_> {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Relaxed);
            }
        }
        let _stop = Stop(&stop);
        let ids = ids.lock().unwrap().clone();
        for id in ids {
            harness.wait_state(id, |s| matches!(s, RunState::Running { .. }));
        }
    });
    assert_eq!(harness.terminals.list().terminals.len(), 8);
}

#[test]
fn events_announce_the_list_and_frames() {
    let kit = Kit::new();
    let output = kit.root.join("output.vt");
    std::fs::write(&output, b"\x07\x1b]2;a-title\x07shown\r\n").unwrap();
    kit.plan(&format!("out:{},hold", output.display()));
    let harness = kit.harness();
    let id = harness.open(shell(&kit.project), size()).unwrap();
    let _watch = harness.terminals.watch(id).unwrap();
    let frame = harness.wait_frame(id, |text| text.contains("shown"));
    assert!(matches!(frame.state, RunState::Running { .. }));
    let info = harness
        .wait_for(WAIT, || {
            harness
                .terminals
                .info(id)
                .filter(|i| i.title.is_some() && i.bells == 1)
        })
        .expect("the title and the bell are in the info");
    assert_eq!(info.title.as_deref(), Some("a-title"));

    let events = harness.events();
    let listed: Vec<u64> = events
        .iter()
        .filter_map(|e| match e {
            TerminalEvent::Listed { generation } => Some(*generation),
            _ => None,
        })
        .collect();
    assert!(listed.len() >= 2, "{events:?}");
    assert!(listed.windows(2).all(|w| w[0] <= w[1]), "{listed:?}");
    assert!(
        events
            .iter()
            .any(|e| matches!(e, TerminalEvent::Frame { id: i, .. } if *i == id)),
        "{events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, TerminalEvent::Bell { id: i } if *i == id)),
        "{events:?}"
    );
    // No watcher, no frame events.
    drop(_watch);
    let before = harness
        .events()
        .iter()
        .filter(|e| matches!(e, TerminalEvent::Frame { .. }))
        .count();
    std::thread::sleep(Duration::from_millis(50));
    harness.terminals.input(id, Input::SelectAll);
    std::thread::sleep(Duration::from_millis(100));
    let after = harness
        .events()
        .iter()
        .filter(|e| matches!(e, TerminalEvent::Frame { .. }))
        .count();
    assert_eq!(before, after);
}

#[test]
fn copy_selection_replies_inside_the_callers_admission() {
    let kit = Kit::new();
    let output = kit.root.join("output.vt");
    std::fs::write(&output, b"copy this\r\n").unwrap();
    kit.plan(&format!("out:{},hold", output.display()));
    let harness = kit.harness();
    let (id, _) = running_shell(&kit, &harness);
    let _watch = harness.terminals.watch(id).unwrap();
    harness.wait_frame(id, |text| text.contains("copy this"));

    let ask = |harness: &Harness, id: TerminalId| {
        let (tx, rx) = channel();
        harness.terminals.copy_selection(
            id,
            harness.token.clone(),
            Box::new(move |text| {
                let _ = tx.send(text);
            }),
        );
        rx.recv_timeout(Duration::from_secs(5))
    };
    // Nothing is selected yet.
    assert_eq!(ask(&harness, id), Ok(None));
    harness.terminals.input(id, Input::SelectAll);
    assert_eq!(ask(&harness, id), Ok(Some("copy this".to_owned())));
    // A closed terminal answers with nothing.
    harness.terminals.close(id);
    assert_eq!(ask(&harness, id), Ok(None));
    // After the caller is deactivated the reply is dropped, not called.
    harness.kernel.deactivate();
    let (tx, rx) = channel::<Option<String>>();
    harness.terminals.copy_selection(
        id,
        harness.token.clone(),
        Box::new(move |text| {
            let _ = tx.send(text);
        }),
    );
    assert_eq!(
        rx.recv_timeout(Duration::from_millis(500)),
        Err(RecvTimeoutError::Disconnected)
    );
}

#[test]
fn concurrent_first_forks_from_several_plugins_start_every_program() {
    // Several terminals of several plugin instances in one process start at
    // the same moment, as the first forks of the process, while others exit
    // and are closed. Without one fork at a time, libSystem's fork handler in
    // the child could abort on a once-gate that another forking thread held:
    // the child died with SIGKILL before it ran one instruction of the program.
    let kits: Vec<Kit> = (0..8).map(|_| Kit::new()).collect();
    for (n, kit) in kits.iter().enumerate() {
        // Half of the plugins start programs that exit at once.
        kit.plan(if n % 2 == 0 {
            "say:READY,nl,hold"
        } else {
            "exit:0"
        });
    }
    let harnesses: Vec<Harness> = kits.iter().map(Kit::harness).collect();
    let start = std::sync::Barrier::new(8 * 6);
    let results = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for (n, (kit, harness)) in kits.iter().zip(&harnesses).enumerate() {
            for _ in 0..6 {
                let (start, results) = (&start, &results);
                scope.spawn(move || {
                    start.wait();
                    let id = harness.open(shell(&kit.project), size()).expect("opens");
                    let holds = n % 2 == 0;
                    let state = if holds {
                        harness.wait_state(id, |s| matches!(s, RunState::Running { .. }));
                        // Still running a moment later: not killed at birth.
                        std::thread::sleep(Duration::from_millis(150));
                        harness.terminals.info(id).unwrap().state
                    } else {
                        harness
                            .wait_state(id, |s| matches!(s, RunState::Exited { .. }))
                            .state
                    };
                    results.lock().unwrap().push((holds, state));
                    harness.terminals.close(id);
                });
            }
        }
    });
    let results = results.into_inner().unwrap();
    assert_eq!(results.len(), 48);
    for (holds, state) in results {
        if holds {
            assert!(matches!(state, RunState::Running { .. }), "{state:?}");
        } else {
            assert_eq!(
                state,
                RunState::Exited {
                    code: 0,
                    signaled: false
                }
            );
        }
    }
}
