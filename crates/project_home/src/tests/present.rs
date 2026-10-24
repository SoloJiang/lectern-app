//! The sentences of a row (docs/design/project_home.md「界面」): the second
//! line of a card, the time of a session, the third-party agent's name. They
//! come from `keys::project_home::*` and `Strings::relative_time`; the text
//! is not in the semantic tree, so it is checked here, in both languages.

use std::sync::Arc;

use lectern_i18n::{Locale, Strings};
use lectern_services::session::{Agent, SessionKey, SourceId, Timestamp};

use crate::nodes;
use crate::present::{activity, agent_name, signals, title};
use crate::snapshot::{SessionRow, Signals};
use crate::testing::{DAY, MINUTE, NOW};

fn row(agent: Agent, ago: i64, running: bool, title: Option<&str>) -> SessionRow {
    SessionRow {
        key: SessionKey {
            source: SourceId(Arc::from("fake")),
            id: Arc::from("s"),
        },
        agent,
        title: title.map(Arc::from),
        last_activity: Timestamp(NOW - ago),
        node: nodes::sidebar_session("s"),
        running,
        tail_pending: false,
    }
}

#[test]
fn the_time_of_a_session_counts_back_from_the_snapshot() {
    let now = Timestamp(NOW);
    let en = Strings::of(Locale::En);
    let zh = Strings::of(Locale::ZhCn);
    let cases = [
        (row(Agent::ClaudeCode, 0, false, None), "just now", "刚刚"),
        (
            row(Agent::ClaudeCode, 5 * MINUTE, false, None),
            "5 min ago",
            "5 分钟前",
        ),
        (
            row(Agent::Codex, 3 * 60 * MINUTE, false, None),
            "3 hours ago",
            "3 小时前",
        ),
        (
            row(Agent::Codex, 2 * DAY, false, None),
            "2 days ago",
            "2 天前",
        ),
        // A clock that is behind the file's time does not go negative.
        (row(Agent::Codex, -MINUTE, false, None), "just now", "刚刚"),
        // Running replaces the time.
        (
            row(Agent::ClaudeCode, MINUTE, true, None),
            "Running",
            "运行中",
        ),
        // An agent without a dot leads the line with its name, as written.
        (
            row(Agent::Other("aider".into()), 5 * MINUTE, false, None),
            "aider · 5 min ago",
            "aider · 5 分钟前",
        ),
        (
            row(Agent::Other("aider".into()), MINUTE, true, None),
            "aider · Running",
            "aider · 运行中",
        ),
    ];
    for (row, expected_en, expected_zh) in cases {
        assert_eq!(activity(&en, &row, now), expected_en);
        assert_eq!(activity(&zh, &row, now), expected_zh);
    }
}

#[test]
fn signals_join_the_ones_that_hold() {
    let en = Strings::of(Locale::En);
    let zh = Strings::of(Locale::ZhCn);
    let none = Signals {
        drifted: false,
        critical: 0,
        changed_since_seen: 0,
    };
    assert_eq!(signals(&en, &none), None, "the second line is left out");
    let all = Signals {
        drifted: true,
        critical: 2,
        changed_since_seen: 1,
    };
    assert_eq!(
        signals(&en, &all).as_deref(),
        Some("Drifted · 2 critical · 1 file changed since seen")
    );
    assert_eq!(
        signals(&zh, &all).as_deref(),
        Some("已漂移 · 2 处需注意 · 已看后又改了 1 个文件")
    );
    let some = Signals {
        changed_since_seen: 3,
        ..none
    };
    assert_eq!(
        signals(&en, &some).as_deref(),
        Some("3 files changed since seen")
    );
}

#[test]
fn titles_and_agents_are_written_as_given() {
    let en = Strings::of(Locale::En);
    let zh = Strings::of(Locale::ZhCn);
    let titled = row(Agent::Codex, 0, false, Some("Fix {the} bug"));
    assert_eq!(title(&en, &titled).as_str(), "Fix {the} bug");
    assert_eq!(title(&en, &titled).message_id(), None);
    let untitled = row(Agent::Codex, 0, false, None);
    assert_eq!(title(&en, &untitled).as_str(), "Untitled session");
    assert_eq!(title(&zh, &untitled).as_str(), "未命名会话");
    assert_eq!(
        title(&en, &untitled).message_id(),
        Some("project-home-untitled-session")
    );

    assert_eq!(agent_name(&en, &Agent::ClaudeCode), "Claude Code");
    assert_eq!(agent_name(&zh, &Agent::Codex), "Codex");
    assert_eq!(agent_name(&zh, &Agent::Other("aider".into())), "aider");
}
