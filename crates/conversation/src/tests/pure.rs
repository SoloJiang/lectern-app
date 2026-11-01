//! Pure functions of step 2 (docs/design/conversation.md「测试」): the answer
//! buttons (`options_*`), who drives a session (`drive_*`), the hint that a
//! session may run elsewhere (`elsewhere_*`) and the drafts (`memory_*`). No
//! window and no service.

use std::sync::Arc;

use lectern_i18n::{Locale, Strings};
use lectern_services::conversation::Link;
use lectern_services::conversation::{
    AcpProgram, OptionId, OptionKind, PermissionOption, Unavailable,
};
use lectern_services::session::Timestamp;
use lectern_services::terminal::{FailReason, RunState};

use crate::drive::{self, Block, ELSEWHERE_WINDOW_MS};
use crate::memory::{MAX_DRAFT, MAX_SESSIONS, Memory};
use crate::present;
use crate::testing::{session, summary};
use crate::view::Page;

fn option(id: &str, kind: OptionKind) -> PermissionOption {
    PermissionOption {
        id: OptionId(Arc::from(id)),
        kind,
    }
}

// ----------------------------------------------------------------- options

#[test]
fn options_are_ordered_by_kind() {
    let given = vec![
        option("d", OptionKind::RejectAlways),
        option("b", OptionKind::AllowAlways),
        option("c", OptionKind::RejectOnce),
        option("a", OptionKind::AllowOnce),
    ];
    let kinds: Vec<OptionKind> = present::option_buttons(&given)
        .into_iter()
        .map(|(kind, _)| kind)
        .collect();
    assert_eq!(
        kinds,
        [
            OptionKind::AllowOnce,
            OptionKind::AllowAlways,
            OptionKind::RejectOnce,
            OptionKind::RejectAlways,
        ]
    );
}

#[test]
fn options_draw_only_the_kinds_given_and_the_first_of_each() {
    let given = vec![
        option("second-reject", OptionKind::RejectOnce),
        option("only-allow", OptionKind::AllowOnce),
        option("first-reject", OptionKind::RejectOnce),
    ];
    // The provider keeps one per kind; a repeat that gets through is dropped
    // here, and the answer carries the first one's ID.
    let buttons = present::option_buttons(&given);
    assert_eq!(
        buttons,
        [
            (OptionKind::AllowOnce, OptionId(Arc::from("only-allow"))),
            (OptionKind::RejectOnce, OptionId(Arc::from("second-reject"))),
        ]
    );
    assert!(present::option_buttons(&[]).is_empty());
}

#[test]
fn options_text_comes_from_the_resource_files_not_from_the_option() {
    let strings = Strings::of(Locale::En);
    let text = |kind| present::option_text(&strings, kind);
    assert_eq!(text(OptionKind::AllowOnce).shared().as_ref(), "Allow");
    assert_eq!(
        text(OptionKind::AllowAlways).shared().as_ref(),
        "Always Allow"
    );
    assert_eq!(text(OptionKind::RejectOnce).shared().as_ref(), "Deny");
    assert_eq!(
        text(OptionKind::RejectAlways).shared().as_ref(),
        "Always Deny"
    );
    assert_eq!(
        text(OptionKind::AllowOnce).message_id(),
        Some("conversation-permission-allow-once")
    );
    let zh = Strings::of(Locale::ZhCn);
    assert_eq!(
        present::option_text(&zh, OptionKind::RejectOnce)
            .shared()
            .as_ref(),
        "拒绝"
    );
}

// ------------------------------------------------------------------- drive

#[test]
fn drive_only_a_starting_or_running_terminal_drives() {
    assert!(!drive::drives(None));
    assert!(drive::drives(Some(&RunState::Starting)));
    assert!(drive::drives(Some(&RunState::Running { pid: 7 })));
    assert!(!drive::drives(Some(&RunState::Exited {
        code: 0,
        signaled: false
    })));
    assert!(!drive::drives(Some(&RunState::Exited {
        code: 9,
        signaled: true
    })));
    assert!(!drive::drives(Some(&RunState::Failed(
        FailReason::CwdMissing
    ))));
}

#[test]
fn drive_the_reply_box_follows_the_provider_unless_a_terminal_drives() {
    // Nothing in the way.
    assert_eq!(drive::send_block(&Ok(()), false), None);
    // A reason from the provider.
    for why in [
        Unavailable::NotResumable,
        Unavailable::OutsideProject,
        Unavailable::NotInstalled(AcpProgram::Codex),
        Unavailable::Unsupported,
        Unavailable::TooMany,
        Unavailable::Busy,
    ] {
        assert_eq!(
            drive::send_block(&Err(why.clone()), false),
            Some(Block::Unavailable(why))
        );
    }
    // The terminal comes first, whatever the provider says.
    assert_eq!(drive::send_block(&Ok(()), true), Some(Block::InTerminal));
    assert_eq!(
        drive::send_block(&Err(Unavailable::NotResumable), true),
        Some(Block::InTerminal)
    );
}

#[test]
fn drive_a_finished_terminal_does_not_stop_the_box() {
    for state in [
        RunState::Exited {
            code: 1,
            signaled: false,
        },
        RunState::Failed(FailReason::NoShell),
    ] {
        let drives = drive::drives(Some(&state));
        assert_eq!(drive::send_block(&Ok(()), drives), None);
    }
}

// --------------------------------------------------------------- elsewhere

#[test]
fn elsewhere_needs_an_open_turn_recent_activity_no_link_and_no_terminal() {
    let now = Timestamp(10_000_000);
    let mut open = summary(&session(1));
    open.open_turn = true;
    open.last_activity = Timestamp(now.0 - 30_000);

    assert!(drive::elsewhere(&open, &Link::None, false, now));
    // Each of the four conditions alone turns it off.
    let mut closed = open.clone();
    closed.open_turn = false;
    assert!(!drive::elsewhere(&closed, &Link::None, false, now));
    for link in [Link::Connecting, Link::Idle, Link::Working, Link::Stopping] {
        assert!(!drive::elsewhere(&open, &link, false, now));
    }
    assert!(!drive::elsewhere(&open, &Link::None, true, now));
    let mut quiet = open.clone();
    quiet.last_activity = Timestamp(now.0 - ELSEWHERE_WINDOW_MS - 1);
    assert!(!drive::elsewhere(&quiet, &Link::None, false, now));
}

#[test]
fn elsewhere_the_two_minute_boundary_is_inclusive() {
    let now = Timestamp(10_000_000);
    let mut open = summary(&session(1));
    open.open_turn = true;
    open.last_activity = Timestamp(now.0 - ELSEWHERE_WINDOW_MS);
    assert!(drive::elsewhere(&open, &Link::None, false, now));
    open.last_activity = Timestamp(now.0 - ELSEWHERE_WINDOW_MS - 1);
    assert!(!drive::elsewhere(&open, &Link::None, false, now));
    // A log written "in the future" (clock skew) is recent.
    open.last_activity = Timestamp(now.0 + 5_000);
    assert!(drive::elsewhere(&open, &Link::None, false, now));
}

#[test]
fn elsewhere_a_failed_link_is_not_none() {
    let now = Timestamp(10_000_000);
    let mut open = summary(&session(1));
    open.open_turn = true;
    open.last_activity = Timestamp(now.0 - 1_000);
    let failed = Link::Failed(lectern_services::conversation::LinkError::Protocol);
    assert!(!drive::elsewhere(&open, &failed, false, now));
}

#[test]
fn drive_waited_milliseconds_never_go_negative() {
    assert_eq!(drive::waited_ms(Timestamp(1_000), Timestamp(4_500)), 3_500);
    assert_eq!(drive::waited_ms(Timestamp(9_000), Timestamp(4_500)), 0);
}

// ------------------------------------------------------------------ memory

#[test]
fn memory_keeps_and_gives_back_a_draft() {
    let mut memory = Memory::new();
    let (one, two) = (session(1), session(2));
    assert_eq!(memory.draft(&one), None);
    memory.set_draft(&one, "first");
    memory.set_draft(&two, "second");
    assert_eq!(memory.draft(&one).as_deref(), Some("first"));
    assert_eq!(memory.draft(&two).as_deref(), Some("second"));
    // The latest text replaces the earlier one; an empty box forgets it.
    memory.set_draft(&one, "changed");
    assert_eq!(memory.draft(&one).as_deref(), Some("changed"));
    memory.set_draft(&one, "");
    assert_eq!(memory.draft(&one), None);
    assert_eq!(memory.len(), 1, "a session with nothing left is not kept");
}

#[test]
fn memory_keeps_the_page_apart_from_the_draft() {
    let mut memory = Memory::new();
    let one = session(1);
    assert_eq!(memory.page(&one), None);
    memory.set_page(&one, Page::Terminal);
    memory.set_draft(&one, "text");
    assert_eq!(memory.page(&one), Some(Page::Terminal));
    memory.set_draft(&one, "");
    assert_eq!(memory.page(&one), Some(Page::Terminal));
    assert_eq!(memory.draft(&one), None);
}

#[test]
fn memory_forgets_the_least_recently_used_of_sixteen() {
    let mut memory = Memory::new();
    for n in 1..=MAX_SESSIONS as u32 {
        memory.set_draft(&session(n), &format!("draft {n}"));
    }
    assert_eq!(memory.len(), MAX_SESSIONS);
    // Reading the first one makes it the most recent; the second is now the
    // oldest and goes when a seventeenth comes.
    assert_eq!(memory.draft(&session(1)).as_deref(), Some("draft 1"));
    memory.set_draft(&session(99), "newcomer");
    assert_eq!(memory.len(), MAX_SESSIONS);
    assert_eq!(memory.draft(&session(2)), None);
    assert_eq!(memory.draft(&session(1)).as_deref(), Some("draft 1"));
    assert_eq!(memory.draft(&session(99)).as_deref(), Some("newcomer"));
    assert_eq!(memory.draft(&session(3)).as_deref(), Some("draft 3"));
}

#[test]
fn memory_does_not_keep_a_draft_over_256_kib() {
    let mut memory = Memory::new();
    let one = session(1);
    let limit = "x".repeat(MAX_DRAFT);
    memory.set_draft(&one, &limit);
    assert_eq!(memory.draft(&one).map(|draft| draft.len()), Some(MAX_DRAFT));
    // One byte more: not kept, and the older text is not what the box held.
    memory.set_draft(&one, &"x".repeat(MAX_DRAFT + 1));
    assert_eq!(memory.draft(&one), None);
    // Nothing was there to replace: nothing is made.
    let mut empty = Memory::new();
    empty.set_draft(&one, &"x".repeat(MAX_DRAFT + 1));
    assert_eq!(empty.len(), 0);
}
