//! `dedup_claude_*`, `dedup_codex_*`, `ccusage_*`, `reset_*`
//! (docs/design/usage.md「规范化与去重」). The record scripts in
//! `tests/fixtures/records/` are synthetic, shaped after agents design
//! 「外部格式」 and ADR 0014; no real session log is involved.

use std::path::Path;

use lectern_services::project::ProjectKey;
use lectern_services::session::{AdvisorUsage, Agent, Record, UsageKey};
use lectern_services::usage::{Tokens, Window};
use serde::Deserialize;

use super::support::*;
use crate::testing::FakeSessions;

const CLAUDE: &str = include_str!("../../tests/fixtures/records/claude.json");
const CODEX: &str = include_str!("../../tests/fixtures/records/codex.json");
const CCUSAGE: &str = include_str!("../../tests/fixtures/ccusage/claude.json");

/// Reads a fixture whose sessions run in `/work/app`, a repository opened
/// in Lectern.
fn read(fixture: &str) -> (Harness, ProjectKey) {
    let mut h = Harness::new();
    let repo = h.open_repo("/work/app");
    let fake = FakeSessions::from_json(fixture);
    h.read_every(&fake);
    (h, repo)
}

#[test]
fn dedup_claude_counts_each_response_once() {
    let (h, repo) = read(CLAUDE);
    // msg_1 three times (two content blocks, a repeated line) and again in
    // another session's file: once, at its largest total. msg_2: the
    // non-sidechain copy. msg_3: cache write from the 5m / 1h split.
    // msg_4: the subagent's own usage.
    assert_eq!(h.total(), tokens([18, 57, 150, 60]));
    insta::assert_snapshot!(render_project(&h.project(&repo, Window::D7)));
}

#[test]
fn dedup_claude_keeps_the_largest_total() {
    let h = Harness::new();
    let fake = FakeSessions::new("session-claude-code", Agent::ClaudeCode);
    fake.add("s", std::path::Path::new("/work/app"), OCT_20, None);
    let at = OCT_20 + 10 * HOUR;
    fake.push(
        "s",
        vec![
            context(0, at, "/work/app"),
            claude(100, at, None, "m", [1, 50, 0, 0]),
            claude(200, at, None, "m", [1, 10, 0, 0]),
        ],
    );
    h.read_all(&fake, "s");
    assert_eq!(h.total(), tokens([1, 50, 0, 0]));
    // A larger copy arriving later replaces it.
    fake.push("s", vec![claude(300, at, None, "m", [1, 70, 0, 0])]);
    h.read_all(&fake, "s");
    assert_eq!(h.total(), tokens([1, 70, 0, 0]));
}

#[test]
fn dedup_claude_sidechain_copy_yields_to_the_main_one() {
    let h = Harness::new();
    let fake = FakeSessions::new("session-claude-code", Agent::ClaudeCode);
    fake.add("s", std::path::Path::new("/work/app"), OCT_20, None);
    let at = OCT_20 + 10 * HOUR;
    let mut side = claude(100, at, None, "m", [1, 99, 0, 0]);
    if let Record::Usage(u) = &mut side.value {
        u.sidechain = true;
    }
    fake.push("s", vec![context(0, at, "/work/app"), side]);
    h.read_all(&fake, "s");
    // Alone, the sidechain copy counts.
    assert_eq!(h.total(), tokens([1, 99, 0, 0]));
    fake.push("s", vec![claude(200, at, None, "m", [1, 2, 0, 0])]);
    h.read_all(&fake, "s");
    assert_eq!(h.total(), tokens([1, 2, 0, 0]));
}

#[test]
fn dedup_claude_advisor_counts_under_its_own_model() {
    let mut h = Harness::new();
    let repo = h.open_repo("/work/app");
    let fake = FakeSessions::new("session-claude-code", Agent::ClaudeCode);
    fake.add("s", Path::new("/work/app"), OCT_20, None);
    let at = OCT_20 + 10 * HOUR;
    let mut r = claude(100, at, None, "m", [5, 5, 0, 0]);
    if let Record::Usage(u) = &mut r.value {
        u.advisor = Some(AdvisorUsage {
            model: "model-b".into(),
            tokens: anthropic([2, 1, 0, 0]),
        });
    }
    // The same line read twice (a repeated content block) still counts the
    // advisor once.
    let again = seq(r.clone(), 1);
    fake.push("s", vec![context(0, at, "/work/app"), r, again]);
    h.read_all(&fake, "s");
    assert_eq!(h.total(), tokens([7, 6, 0, 0]));
    insta::assert_snapshot!(render_project(&h.project(&repo, Window::D7)));
}

#[test]
fn dedup_codex_one_route_inherited_prefix_and_unknown_boundary() {
    let (h, _) = read(CODEX);
    // th-main: the first token_count, both responses (cached input over
    // input is clamped); the superseded token_count is not counted.
    // th-sub: its own token_count; the replayed parent response is
    // inherited. th-fork: its inheritance boundary is unknown, so its copy
    // of resp-1 is not marked; the key still counts once.
    assert_eq!(h.total(), tokens([57, 39, 140, 0]));
    let all = h.all(Window::D7);
    assert!(all.header.may_include_inherited);
    insta::assert_snapshot!(render_all(&all));
}

#[test]
fn dedup_codex_model_comes_from_the_context() {
    let (h, repo) = read(CODEX);
    let p = h.project(&repo, Window::D7);
    assert_eq!(p.by_model.len(), 1);
    assert_eq!(&*p.by_model[0].1, "model-c");
    insta::assert_snapshot!(render_project(&p));
}

#[test]
fn dedup_codex_inherited_only_key_is_not_counted() {
    let h = Harness::new();
    let fake = FakeSessions::new("session-codex", Agent::Codex);
    fake.add("child", Path::new("/work/app"), OCT_20, Some("parent"));
    let at = OCT_20 + 10 * HOUR;
    let r = super::support::rec(
        100,
        Record::Usage(usage(
            at,
            None,
            "model-c",
            UsageKey::CodexResponse {
                thread_id: "parent".into(),
                response_id: "r".into(),
            },
            openai(10, 0, 1),
        )),
    );
    fake.push("child", vec![context(0, at, "/work/app"), inherited(r)]);
    h.read_all(&fake, "child");
    assert_eq!(h.total(), Tokens::default());
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CcusageDay {
    date: String,
    input_tokens: u64,
    output_tokens: u64,
    cache_creation_tokens: u64,
    cache_read_tokens: u64,
    total_tokens: u64,
}

#[derive(Deserialize)]
struct Ccusage {
    daily: Vec<CcusageDay>,
}

#[test]
fn ccusage_daily_totals_match() {
    let (h, _) = read(CLAUDE);
    let expected: Ccusage = serde_json::from_str(CCUSAGE).unwrap();
    let days: Vec<_> = h
        .days(7)
        .into_iter()
        .filter(|(_, t)| t.total() > 0)
        .collect();
    assert_eq!(days.len(), expected.daily.len());
    for ((d, t), e) in days.iter().zip(&expected.daily) {
        assert_eq!(date(*d), e.date);
        assert_eq!(t.input, e.input_tokens);
        assert_eq!(t.output, e.output_tokens);
        assert_eq!(t.cache_write, e.cache_creation_tokens);
        assert_eq!(t.cache_read, e.cache_read_tokens);
        assert_eq!(t.total(), e.total_tokens);
    }
}

fn three(at: i64) -> Vec<lectern_services::session::Positioned<Record>> {
    vec![
        context(0, at, "/work/app"),
        claude(100, at, None, "a", [1, 0, 0, 0]),
        claude(200, at, None, "b", [10, 0, 0, 0]),
        claude(300, at, None, "c", [100, 0, 0, 0]),
    ]
}

#[test]
fn reset_old_generation_counts_until_the_replay_ends() {
    let h = Harness::new();
    let fake = FakeSessions::new("session-claude-code", Agent::ClaudeCode);
    fake.add("s", std::path::Path::new("/work/app"), OCT_20, None);
    let at = OCT_20 + 10 * HOUR;
    fake.push("s", three(at));
    h.read_all(&fake, "s");
    assert_eq!(h.total().input, 111);

    // The adapter was upgraded: the file is parsed again, now without "c"
    // and with a new "d".
    fake.replace(
        "s",
        vec![
            context(0, at, "/work/app"),
            claude(100, at, None, "a", [1, 0, 0, 0]),
            claude(200, at, None, "b", [10, 0, 0, 0]),
            claude(300, at, None, "d", [1000, 0, 0, 0]),
        ],
    );
    let first = h.read_once(&fake, "s", 150);
    assert!(first.replay_started && !first.at_end);
    // Mid-replay the old generation still counts; keys already replayed
    // count once.
    assert_eq!(h.total().input, 111);
    let mut last = first;
    while !last.at_end {
        last = h.read_once(&fake, "s", 150);
    }
    assert!(last.replay_done);
    assert_eq!(h.total().input, 1011);
}

#[test]
fn reset_same_key_in_another_session_still_counts() {
    let h = Harness::new();
    let fake = FakeSessions::new("session-claude-code", Agent::ClaudeCode);
    let at = OCT_20 + 10 * HOUR;
    for id in ["a", "b"] {
        fake.add(id, std::path::Path::new("/work/app"), OCT_20, None);
        fake.push(id, three(at));
        h.read_all(&fake, id);
    }
    assert_eq!(h.total().input, 111);
    // Session "a" (chosen for every key) is replayed without "c": "b"'s copy
    // takes over and the total stays.
    fake.replace(
        "a",
        vec![
            context(0, at, "/work/app"),
            claude(100, at, None, "a", [1, 0, 0, 0]),
            claude(200, at, None, "b", [10, 0, 0, 0]),
        ],
    );
    h.read_all(&fake, "a");
    assert_eq!(h.total().input, 111);
}

#[test]
fn reset_during_a_replay_starts_it_again() {
    let h = Harness::new();
    let fake = FakeSessions::new("session-claude-code", Agent::ClaudeCode);
    fake.add("s", std::path::Path::new("/work/app"), OCT_20, None);
    let at = OCT_20 + 10 * HOUR;
    fake.push("s", three(at));
    h.read_all(&fake, "s");
    let replay = |extra: &str| {
        vec![
            context(0, at, "/work/app"),
            claude(100, at, None, extra, [7, 0, 0, 0]),
            claude(200, at, None, "b", [10, 0, 0, 0]),
            claude(300, at, None, "c", [100, 0, 0, 0]),
        ]
    };
    fake.replace("s", replay("x"));
    let first = h.read_once(&fake, "s", 150);
    assert!(first.replay_started && !first.at_end);
    // Another upgrade before the replay ends: "x" is dropped with the
    // unfinished generation.
    fake.replace("s", replay("y"));
    h.read_all(&fake, "s");
    // a (old, gone), x (abandoned) are not counted; y, b, c are.
    assert_eq!(h.total().input, 117);
}
