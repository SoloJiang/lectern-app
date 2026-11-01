//! The sentences of a row (docs/design/project_home.md「界面」): the first
//! and second line of a card, the sentences under the large title, the
//! limit's line, the third-party agent's name. They come from
//! `keys::project_home::*` and `Strings::relative_time`; the text is not in the
//! semantic tree, so it is checked here, in both languages.

use std::sync::Arc;

use lectern_i18n::{Locale, Strings};
use lectern_services::session::{Agent, SessionKey, SourceId, Timestamp};

use crate::nodes;
use crate::present::{
    agent_name, lede, limit as limit_text, meta, outside_names, relative, signals, title,
};
use crate::snapshot::{
    Card, MainModel, OutsideRow, SessionRow, SessionStatus, Signals, TightestLimit,
};
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

fn card(row: SessionRow, branch: Option<&str>, status: Option<SessionStatus>) -> Card {
    Card {
        session: row,
        branch: branch.map(Arc::from),
        status,
        progress: None,
        signals: Signals {
            drifted: false,
            critical: 0,
            changed_since_seen: 0,
        },
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
        // Running does not replace the time: the status word says it.
        (
            row(Agent::ClaudeCode, MINUTE, true, None),
            "1 min ago",
            "1 分钟前",
        ),
    ];
    for (row, expected_en, expected_zh) in cases {
        assert_eq!(relative(&en, &row, now), expected_en);
        assert_eq!(relative(&zh, &row, now), expected_zh);
    }
}

#[test]
fn the_first_line_of_a_card_joins_agent_branch_time_and_status() {
    let now = Timestamp(NOW);
    let en = Strings::of(Locale::En);
    let zh = Strings::of(Locale::ZhCn);
    let running = card(
        row(Agent::Codex, 34 * MINUTE, true, None),
        Some("feat-print-queue"),
        Some(SessionStatus::Running),
    );
    let line = meta(&en, &running, now, false);
    assert_eq!(line.text, "Codex · feat-print-queue · 34 min ago · running");
    let (range, status) = line.status.expect("the status word is marked");
    assert_eq!(&line.text[range], "running");
    assert_eq!(status, SessionStatus::Running);
    assert_eq!(
        meta(&zh, &running, now, false).text,
        "Codex · feat-print-queue · 34 分钟前 · 运行中"
    );

    // No branch (a folder project) and no status: only the two parts remain.
    let plain = card(
        row(Agent::ClaudeCode, 2 * 60 * MINUTE, false, None),
        None,
        None,
    );
    let line = meta(&en, &plain, now, false);
    assert_eq!(line.text, "Claude Code · 2 hours ago");
    assert!(line.status.is_none());

    // The statuses `agents` will give: the words exist in both languages.
    for (status, expected_en, expected_zh) in [
        (SessionStatus::Waiting, "waiting for you", "等你处理"),
        (SessionStatus::Ended, "ended", "已结束"),
    ] {
        let card = card(
            row(Agent::ClaudeCode, MINUTE, false, None),
            Some("main"),
            Some(status),
        );
        assert!(meta(&en, &card, now, false).text.ends_with(expected_en));
        assert!(meta(&zh, &card, now, false).text.ends_with(expected_zh));
    }

    // A session that waits for the user says so, whatever the snapshot says:
    // the snapshot does not know who waits.
    for status in [None, Some(SessionStatus::Running)] {
        let card = card(row(Agent::Codex, MINUTE, true, None), Some("main"), status);
        let line = meta(&en, &card, now, true);
        assert_eq!(line.text, "Codex · main · 1 min ago · waiting for you");
        let (range, status) = line.status.expect("the status word is marked");
        assert_eq!(&line.text[range], "waiting for you");
        assert_eq!(status, SessionStatus::Waiting);
        assert!(
            meta(&zh, &card, now, true).text.ends_with("等你处理"),
            "and in Chinese"
        );
    }

    // An agent without a dot is named as written.
    let other = card(
        row(Agent::Other("aider".into()), 5 * MINUTE, false, None),
        Some("main"),
        None,
    );
    assert_eq!(
        meta(&en, &other, now, false).text,
        "aider · main · 5 min ago"
    );
}

#[test]
fn the_sentences_under_the_large_title_leave_out_a_zero_count() {
    let en = Strings::of(Locale::En);
    let zh = Strings::of(Locale::ZhCn);
    let main = |running: u32, outside: OutsideRow| MainModel {
        running,
        overview_ready: true,
        needs_review: Arc::from(Vec::new()),
        outside,
        done: Arc::from(Vec::new()),
    };
    let files = |count: u32| OutsideRow::Files {
        count,
        names: Arc::from(Vec::new()),
    };
    assert_eq!(
        lede(&en, &main(2, files(3))).as_deref(),
        Some("2 agents are running. 3 files changed outside any session.")
    );
    assert_eq!(
        lede(&zh, &main(2, files(3))).as_deref(),
        Some("2 个 agent 正在运行。 3 个文件在会话之外被改动。")
    );
    assert_eq!(
        lede(&en, &main(1, files(1))).as_deref(),
        Some("1 agent is running. 1 file changed outside any session.")
    );
    assert_eq!(
        lede(&en, &main(0, files(3))).as_deref(),
        Some("3 files changed outside any session.")
    );
    assert_eq!(
        lede(&en, &main(2, files(0))).as_deref(),
        Some("2 agents are running.")
    );
    // Neither sentence: no line. A count that is not known is not said.
    assert_eq!(lede(&en, &main(0, files(0))), None);
    for outside in [
        OutsideRow::Pending,
        OutsideRow::Hidden,
        OutsideRow::Capped(5000),
    ] {
        assert_eq!(lede(&en, &main(0, outside.clone())), None);
        assert_eq!(
            lede(&en, &main(1, outside)).as_deref(),
            Some("1 agent is running.")
        );
    }
}

#[test]
fn the_outside_card_names_the_first_paths() {
    let names: Vec<Arc<str>> = ["AGENTS.md", "ui/Button.tsx", "pom.xml"]
        .into_iter()
        .map(Arc::from)
        .collect();
    assert_eq!(
        outside_names(3, &names).as_deref(),
        Some("AGENTS.md · ui/Button.tsx · pom.xml")
    );
    assert_eq!(
        outside_names(9, &names).as_deref(),
        Some("AGENTS.md · ui/Button.tsx · pom.xml…"),
        "more files than names given"
    );
    assert_eq!(outside_names(1, &names[..1]).as_deref(), Some("AGENTS.md"));
    assert_eq!(outside_names(4, &[]), None, "no paths, no line");
}

#[test]
fn the_limit_names_its_window() {
    let en = Strings::of(Locale::En);
    let zh = Strings::of(Locale::ZhCn);
    // The agent's name and the window are string arguments: isolated for
    // right-to-left text, which the figures here do not need.
    let limit = |strings: &Strings, limit: &TightestLimit| {
        limit_text(strings, limit).replace(['\u{2068}', '\u{2069}'], "")
    };
    let at = |agent: Agent, percent: u8, minutes: Option<u32>| TightestLimit {
        agent,
        used_percent: percent,
        window_minutes: minutes,
        resets_at: None,
    };
    assert_eq!(
        limit(&en, &at(Agent::Codex, 64, Some(300))),
        "Codex 5-hour limit · 64%"
    );
    assert_eq!(
        limit(&zh, &at(Agent::Codex, 64, Some(300))),
        "Codex 5 小时额度 · 64%"
    );
    assert_eq!(
        limit(&en, &at(Agent::ClaudeCode, 10, Some(10_080))),
        "Claude Code 7-day limit · 10%"
    );
    assert_eq!(
        limit(&zh, &at(Agent::ClaudeCode, 10, Some(10_080))),
        "Claude Code 7 天额度 · 10%"
    );
    // A window that is not a whole number of hours rounds up.
    assert_eq!(
        limit(&en, &at(Agent::Codex, 5, Some(90))),
        "Codex 2-hour limit · 5%"
    );
    assert_eq!(limit(&en, &at(Agent::Codex, 5, None)), "Codex limit · 5%");
    assert_eq!(
        limit(&en, &at(Agent::Codex, 5, Some(0))),
        "Codex limit · 5%"
    );
    assert_eq!(limit(&zh, &at(Agent::Codex, 5, None)), "Codex 额度 · 5%");
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
