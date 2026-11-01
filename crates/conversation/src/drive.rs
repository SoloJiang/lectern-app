//! Who drives a session, and what the reply box may do about it
//! (docs/design/conversation.md「数据结构」的「一处驱动」): pure functions of
//! the facts the page reads from the services. A session is driven from one
//! place in Lectern at a time (ADR 0050): through ACP, or by a terminal that
//! resumed it.

use std::time::{SystemTime, UNIX_EPOCH};

use lectern_services::conversation::{Link, Unavailable};
use lectern_services::session::{SessionSummary, Timestamp};
use lectern_services::terminal::RunState;

/// A session that was active in its log this recently may still run
/// somewhere else (2 minutes, in milliseconds).
pub(crate) const ELSEWHERE_WINDOW_MS: i64 = 2 * 60 * 1000;

/// Whether a terminal in this state drives its session: only a terminal that
/// starts or runs does; one that exited or failed, with its last picture
/// still on the page, does not.
pub(crate) fn drives(state: Option<&RunState>) -> bool {
    matches!(
        state,
        Some(RunState::Starting) | Some(RunState::Running { .. })
    )
}

/// Why the reply box cannot send.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum Block {
    /// A terminal drives the session: the box is replaced by a line and a
    /// button that shows the terminal.
    InTerminal,
    /// The provider says no (`ConversationSnapshot::can_prompt`).
    Unavailable(Unavailable),
}

/// What stops a send, if anything. The terminal comes first: while it drives
/// the session the provider's reason does not matter.
pub(crate) fn send_block(
    can_prompt: &Result<(), Unavailable>,
    terminal_drives: bool,
) -> Option<Block> {
    if terminal_drives {
        return Some(Block::InTerminal);
    }
    can_prompt.clone().err().map(Block::Unavailable)
}

/// Whether the session may still run somewhere else: its log has an open
/// turn, was written to within [`ELSEWHERE_WINDOW_MS`], and nothing in
/// Lectern drives the session. A hint only; it does not stop a send.
pub(crate) fn elsewhere(
    summary: &SessionSummary,
    link: &Link,
    terminal_drives: bool,
    now: Timestamp,
) -> bool {
    summary.open_turn
        && matches!(link, Link::None)
        && !terminal_drives
        && now.0.saturating_sub(summary.last_activity.0) <= ELSEWHERE_WINDOW_MS
}

/// The wall clock in the session list's unit (UTC milliseconds). Reading the
/// clock is not IO.
pub(crate) fn now() -> Timestamp {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_millis()).unwrap_or(i64::MAX)
        });
    Timestamp(millis)
}

/// Milliseconds from `then` to `now`, zero when `then` is later.
pub(crate) fn waited_ms(then: Timestamp, now: Timestamp) -> u64 {
    u64::try_from(now.0.saturating_sub(then.0)).unwrap_or(0)
}
