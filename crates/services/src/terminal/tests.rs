use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use lectern_kernel::{ActivationToken, Event, Service};

use super::*;

/// `Terminals` with no terminals, to check the trait is usable as a shared
/// trait object.
struct Empty;

impl Terminals for Empty {
    fn open(&self, _spec: OpenSpec) -> Result<TerminalId, LaunchError> {
        Err(LaunchError::TooMany)
    }
    fn list(&self) -> Arc<TerminalList> {
        Arc::new(TerminalList {
            generation: 0,
            terminals: Arc::from(Vec::new()),
        })
    }
    fn info(&self, _id: TerminalId) -> Option<TerminalInfo> {
        None
    }
    fn for_session(&self, _session: &SessionKey) -> Option<TerminalId> {
        None
    }
    fn frame(&self, _id: TerminalId) -> Option<Arc<Frame>> {
        None
    }
    fn watch(&self, _id: TerminalId) -> Option<FrameInterest> {
        None
    }
    fn input(&self, _id: TerminalId, _input: Input) {}
    fn copy_selection(
        &self,
        _id: TerminalId,
        _token: ActivationToken,
        reply: Box<dyn FnOnce(Option<String>) + Send>,
    ) {
        reply(None);
    }
    fn close(&self, _id: TerminalId) {}
}

fn round_trip<T>(value: &T) -> T
where
    T: serde::Serialize + serde::de::DeserializeOwned,
{
    let json = serde_json::to_string(value).expect("serializes");
    serde_json::from_str(&json).expect("deserializes")
}

#[test]
fn terminals_is_a_single_service() {
    assert_eq!(<dyn Terminals as Service>::ID, "terminal");
    const { assert!(!<dyn Terminals as Service>::MULTIPLE) };
    let terminals: Arc<dyn Terminals> = Arc::new(Empty);
    assert_eq!(terminals.list().terminals.len(), 0);
    let black = Rgb { r: 0, g: 0, b: 0 };
    let spec = OpenSpec {
        launch: Launch::Shell {
            cwd: Arc::from(std::path::Path::new("/r")),
        },
        size: None,
        theme: Theme {
            foreground: black,
            background: black,
            cursor: black,
            ansi: [black; 16],
            scheme: ColorScheme::Dark,
        },
    };
    assert_eq!(terminals.open(spec), Err(LaunchError::TooMany));
}

#[test]
fn terminal_events_round_trip() {
    fn event<E: Event + Clone>() {}
    event::<TerminalEvent>();

    let events = [
        TerminalEvent::Listed { generation: 3 },
        TerminalEvent::Frame {
            id: TerminalId(1),
            generation: u64::MAX,
        },
        TerminalEvent::Bell { id: TerminalId(2) },
    ];
    for event in &events {
        assert_eq!(&round_trip(event), event);
    }
}

#[test]
fn terminal_states_and_errors_round_trip() {
    let states = [
        RunState::Starting,
        RunState::Running { pid: 42 },
        RunState::Exited {
            code: 1,
            signaled: false,
        },
        RunState::Exited {
            code: 0,
            signaled: true,
        },
        RunState::Failed(FailReason::CwdMissing),
        RunState::Failed(FailReason::OutsideProject),
        RunState::Failed(FailReason::NotInstalled(AgentCommand::ClaudeCode)),
        RunState::Failed(FailReason::NotInstalled(AgentCommand::Codex)),
        RunState::Failed(FailReason::NoShell),
        RunState::Failed(FailReason::Spawn { errno: Some(2) }),
        RunState::Failed(FailReason::Spawn { errno: None }),
        RunState::Failed(FailReason::Internal),
    ];
    for state in &states {
        assert_eq!(&round_trip(state), state);
    }
    let errors = [
        LaunchError::AlreadyRunning(TerminalId(7)),
        LaunchError::UnknownSession,
        LaunchError::UntrustedSource,
        LaunchError::NotResumable,
        LaunchError::InvalidSessionId,
        LaunchError::OutsideProject,
        LaunchError::TooMany,
    ];
    for error in &errors {
        assert_eq!(&round_trip(error), error);
    }
    let size = GridSize {
        cols: 120,
        rows: 40,
        cell_width: 16,
        cell_height: 32,
    };
    assert_eq!(round_trip(&size), size);
}

#[test]
fn agent_command_program_names() {
    assert_eq!(AgentCommand::ClaudeCode.program(), "claude");
    assert_eq!(AgentCommand::Codex.program(), "codex");
}

#[test]
fn style_flags_are_distinct_bits() {
    let all = [
        StyleFlags::BOLD,
        StyleFlags::ITALIC,
        StyleFlags::FAINT,
        StyleFlags::STRIKETHROUGH,
        StyleFlags::OVERLINE,
        StyleFlags::INVISIBLE,
    ];
    let mut seen = 0u8;
    for bit in all {
        assert_eq!(bit.count_ones(), 1);
        assert_eq!(seen & bit, 0);
        seen |= bit;
    }
}

#[test]
fn frame_interest_releases_once() {
    let count = Arc::new(AtomicUsize::new(0));
    let c = count.clone();
    let interest = FrameInterest::new(move || {
        c.fetch_add(1, Ordering::SeqCst);
    });
    assert_eq!(count.load(Ordering::SeqCst), 0);
    assert_eq!(format!("{interest:?}"), "FrameInterest { .. }");
    // Dropped on another thread, as a view's state may be.
    std::thread::spawn(move || drop(interest)).join().unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 1);
}
